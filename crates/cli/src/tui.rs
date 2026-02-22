use std::io;
use std::time::Duration;

use chrono::Utc;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use rust_decimal::Decimal;

use eutrader_core::dashboard::SharedDashboard;
use eutrader_core::Side;

/// Run the TUI dashboard until 'q' is pressed or the token signals shutdown.
pub async fn run_dashboard(
    dashboard: SharedDashboard,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> io::Result<()> {
    // Setup terminal
    terminal::enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    loop {
        // Check for shutdown signal
        if *shutdown.borrow() {
            break;
        }

        // Draw
        terminal.draw(|frame| draw(frame, &dashboard))?;

        // Handle input (non-blocking, 250ms timeout)
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press && key.code == KeyCode::Char('q') {
                    break;
                }
            }
        }
    }

    // Restore terminal
    terminal::disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    Ok(())
}

fn draw(frame: &mut Frame, dashboard: &SharedDashboard) {
    let state = match dashboard.read() {
        Ok(s) => s.clone(),
        Err(_) => return,
    };

    let area = frame.area();

    // Layout: header, markets table, fills log, footer
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Header
            Constraint::Min(8),    // Markets table
            Constraint::Length(12), // Recent fills
            Constraint::Length(3),  // Footer / totals
        ])
        .split(area);

    // --- Header ---
    let uptime = Utc::now() - state.uptime_start;
    let uptime_str = format!(
        "{}h {}m {}s",
        uptime.num_hours(),
        uptime.num_minutes() % 60,
        uptime.num_seconds() % 60
    );
    let header = Paragraph::new(format!(
        " EUTRADER  |  Mode: {}  |  Markets: {}  |  Uptime: {}",
        state.mode,
        state.markets.len(),
        uptime_str,
    ))
    .style(Style::default().fg(Color::Cyan).bold())
    .block(Block::default().borders(Borders::BOTTOM));
    frame.render_widget(header, chunks[0]);

    // --- Markets Table ---
    let header_cells = [
        "Market", "Mid", "Bid", "Ask", "Spread", "Inventory", "Real PnL", "Unrl PnL", "Fills",
    ]
    .into_iter()
    .map(|h| Cell::from(h).style(Style::default().fg(Color::Yellow).bold()));
    let header_row = Row::new(header_cells).height(1);

    let mut rows: Vec<Row> = state
        .markets
        .values()
        .map(|m| {
            let pnl_color = if m.realized_pnl >= Decimal::ZERO {
                Color::Green
            } else {
                Color::Red
            };
            let inv_color = if m.inventory == Decimal::ZERO {
                Color::White
            } else if m.inventory > Decimal::ZERO {
                Color::Cyan
            } else {
                Color::Magenta
            };

            Row::new(vec![
                Cell::from(truncate(&m.name, 30)),
                Cell::from(format!("{:.4}", m.midpoint)),
                Cell::from(format!("{:.2}", m.our_bid)).style(Style::default().fg(Color::Green)),
                Cell::from(format!("{:.2}", m.our_ask)).style(Style::default().fg(Color::Red)),
                Cell::from(format!("{:.2}", m.spread)),
                Cell::from(format!("{:.1}", m.inventory)).style(Style::default().fg(inv_color)),
                Cell::from(format!("${:.2}", m.realized_pnl))
                    .style(Style::default().fg(pnl_color)),
                Cell::from(format!("${:.2}", m.unrealized_pnl)),
                Cell::from(format!("{}", m.fill_count)),
            ])
        })
        .collect();

    // Sort by name for stable display
    rows.sort_by(|a, b| {
        let a_name = a.clone();
        let b_name = b.clone();
        format!("{:?}", a_name).cmp(&format!("{:?}", b_name))
    });

    let widths = [
        Constraint::Min(30),
        Constraint::Length(8),
        Constraint::Length(7),
        Constraint::Length(7),
        Constraint::Length(7),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(6),
    ];

    let table = Table::new(rows, widths)
        .header(header_row)
        .block(
            Block::default()
                .title(" Markets ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .row_highlight_style(Style::default().bg(Color::DarkGray));
    frame.render_widget(table, chunks[1]);

    // --- Recent Fills ---
    let fill_header = Row::new(
        ["Time", "Market", "Side", "Price", "Size", "PnL After"]
            .into_iter()
            .map(|h| Cell::from(h).style(Style::default().fg(Color::Yellow).bold())),
    );

    let fill_rows: Vec<Row> = state
        .recent_fills
        .iter()
        .rev()
        .take(9)
        .map(|f| {
            let side_color = match f.side {
                Side::Buy => Color::Green,
                Side::Sell => Color::Red,
            };
            Row::new(vec![
                Cell::from(f.timestamp.format("%H:%M:%S").to_string()),
                Cell::from(truncate(&f.market_name, 25)),
                Cell::from(format!("{}", f.side)).style(Style::default().fg(side_color)),
                Cell::from(format!("{:.4}", f.price)),
                Cell::from(format!("{:.1}", f.size)),
                Cell::from(format!("${:.2}", f.pnl_after)),
            ])
        })
        .collect();

    let fill_widths = [
        Constraint::Length(10),
        Constraint::Min(25),
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Length(6),
        Constraint::Length(10),
    ];

    let fills_table = Table::new(fill_rows, fill_widths)
        .header(fill_header)
        .block(
            Block::default()
                .title(" Recent Fills ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
    frame.render_widget(fills_table, chunks[2]);

    // --- Footer ---
    let total_pnl = state.total_realized_pnl;
    let pnl_color = if total_pnl >= Decimal::ZERO {
        Color::Green
    } else {
        Color::Red
    };

    let footer = Paragraph::new(format!(
        " Total PnL: ${:.4}  |  Total Fills: {}  |  Press 'q' to quit",
        total_pnl, state.total_fills,
    ))
    .style(Style::default().fg(pnl_color).bold())
    .block(Block::default().borders(Borders::TOP));
    frame.render_widget(footer, chunks[3]);
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}

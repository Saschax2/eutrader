mod tui;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use tracing::info;
use tracing_subscriber::EnvFilter;

use eutrader_core::dashboard::new_shared_dashboard;
use eutrader_core::{Config, Mode};
use eutrader_engine::{OrderManager, PaperExecutor};
use eutrader_feed::{FeedManager, GammaClient};
use eutrader_strategy::{Quoter, RiskManager};

/// eutrader — Polymarket market-making engine
#[derive(Parser)]
#[command(name = "eutrader", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the market-making engine with TUI dashboard.
    Run {
        /// Path to the TOML configuration file.
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,

        /// Override the execution mode from the config file.
        #[arg(short, long)]
        mode: Option<ModeArg>,

        /// Disable TUI and use plain log output instead.
        #[arg(long)]
        no_tui: bool,
    },
    /// Discover available Polymarket markets sorted by volume.
    Discover {
        /// Minimum 24h volume in USD to show.
        #[arg(long, default_value = "10000")]
        min_volume: f64,

        /// Maximum number of markets to display.
        #[arg(long, default_value = "20")]
        limit: usize,
    },
}

/// CLI-level mode argument, mapped to `eutrader_core::Mode`.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum ModeArg {
    Paper,
    Live,
}

impl From<ModeArg> for Mode {
    fn from(arg: ModeArg) -> Self {
        match arg {
            ModeArg::Paper => Mode::Paper,
            ModeArg::Live => Mode::Live,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            config: path,
            mode,
            no_tui,
        } => run(path, mode, no_tui).await,
        Commands::Discover { min_volume, limit } => {
            init_tracing();
            discover(min_volume, limit).await
        }
    }
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}

async fn discover(min_volume: f64, limit: usize) -> Result<()> {
    info!("discovering active Polymarket markets (min volume: ${min_volume})...");

    let client = GammaClient::new();
    let mut markets = client
        .fetch_markets()
        .await
        .context("failed to fetch markets from Gamma API")?;

    markets.retain(|m| {
        m.active && !m.closed && m.volume_num >= min_volume && m.yes_token_id().is_some()
    });
    markets.sort_by(|a, b| {
        b.volume_num
            .partial_cmp(&a.volume_num)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    markets.truncate(limit);

    if markets.is_empty() {
        info!("no markets found matching criteria");
        return Ok(());
    }

    println!(
        "\n{:<60} {:>12} {}",
        "Market", "Volume ($)", "YES Token ID"
    );
    println!("{}", "-".repeat(120));
    for m in &markets {
        let token_id = m.yes_token_id().unwrap_or("N/A");
        println!(
            "{:<60} {:>12.0} {}",
            if m.question.len() > 57 {
                format!("{}...", &m.question[..57])
            } else {
                m.question.clone()
            },
            m.volume_num,
            token_id,
        );
    }
    println!(
        "\nFound {} markets. Copy a token_id into config.toml or use [auto_discover].\n",
        markets.len()
    );

    Ok(())
}

async fn run(config_path: PathBuf, mode_override: Option<ModeArg>, no_tui: bool) -> Result<()> {
    // --- Load configuration ---
    let mut config = Config::load(&config_path)
        .with_context(|| format!("failed to load config from {}", config_path.display()))?;

    if let Some(m) = mode_override {
        config.mode = m.into();
    }

    // Auto-discover markets if configured and no manual markets specified
    if config.markets.is_empty() {
        if let Some(ref discover_config) = config.auto_discover {
            // Need tracing for discovery phase
            if no_tui {
                init_tracing();
            }
            eprintln!("Auto-discovering markets...");
            let gamma = GammaClient::new();
            let discovered = gamma
                .discover_markets(discover_config)
                .await
                .context("auto-discovery failed")?;
            if discovered.is_empty() {
                anyhow::bail!("auto-discovery found no markets matching criteria");
            }
            config.markets = discovered;
        }
    }

    let mode = config.mode;
    let token_ids: Vec<String> = config.markets.iter().map(|m| m.token_id.clone()).collect();
    let mode_str = format!("{:?}", mode);

    if no_tui {
        // Plain log mode (original behavior)
        if !tracing::dispatcher::has_been_set() {
            init_tracing();
        }

        info!("========================================");
        info!("  eutrader — Polymarket Market Maker");
        info!("========================================");
        info!("mode: {:?} | markets: {}", mode, config.markets.len());
        for market in &config.markets {
            info!(
                "  [{}] spread={}bps size={} max_inv={}",
                market.name, market.spread_bps, market.size, market.max_inventory
            );
        }

        match mode {
            Mode::Paper => {
                let executor = PaperExecutor::new();
                let dashboard = new_shared_dashboard(&mode_str);
                let mut manager = OrderManager::new(executor, Quoter::new(), RiskManager::new(), config)
                    .with_dashboard(dashboard);

                let snapshots = FeedManager::new(token_ids)
                    .stream()
                    .await
                    .context("failed to start feed")?;

                manager.run_paper(snapshots).await;
            }
            Mode::Live => {
                anyhow::bail!("live mode is not yet implemented");
            }
        }
    } else {
        // TUI dashboard mode
        // Set tracing to write to a file instead of stdout (TUI owns stdout)
        let log_file = std::fs::File::create("eutrader.log")
            .context("failed to create log file")?;
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug")),
            )
            .with_writer(log_file)
            .with_ansi(false)
            .init();

        match mode {
            Mode::Paper => {
                let executor = PaperExecutor::new();
                let dashboard = new_shared_dashboard(&mode_str);
                let dash_clone = dashboard.clone();
                let mut manager =
                    OrderManager::new(executor, Quoter::new(), RiskManager::new(), config)
                        .with_dashboard(dashboard);

                let snapshots = FeedManager::new(token_ids)
                    .stream()
                    .await
                    .context("failed to start feed")?;

                // Shutdown signal: engine tells TUI to quit
                let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

                // Spawn engine in background task
                let engine_handle = tokio::spawn(async move {
                    manager.run_paper(snapshots).await;
                    let _ = shutdown_tx.send(true);
                });

                // Run TUI on the main thread (must own terminal)
                tui::run_dashboard(dash_clone, shutdown_rx)
                    .await
                    .context("TUI error")?;

                // If TUI exited (user pressed 'q'), abort the engine
                engine_handle.abort();
            }
            Mode::Live => {
                anyhow::bail!("live mode is not yet implemented");
            }
        }
    }

    eprintln!("eutrader shut down cleanly");
    Ok(())
}

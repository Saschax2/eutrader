use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::{Fill, Side};

/// Per-market state displayed on the dashboard.
#[derive(Debug, Clone)]
pub struct MarketRow {
    pub name: String,
    pub token_id: String,
    pub midpoint: Decimal,
    pub our_bid: Decimal,
    pub our_ask: Decimal,
    pub spread: Decimal,
    pub inventory: Decimal,
    pub realized_pnl: Decimal,
    pub unrealized_pnl: Decimal,
    pub fill_count: u64,
    pub last_update: DateTime<Utc>,
}

/// A recent fill for the activity log.
#[derive(Debug, Clone)]
pub struct FillRow {
    pub timestamp: DateTime<Utc>,
    pub market_name: String,
    pub side: Side,
    pub price: Decimal,
    pub size: Decimal,
    pub pnl_after: Decimal,
}

/// Shared dashboard state, updated by the engine and read by the TUI.
#[derive(Debug, Clone)]
pub struct DashboardState {
    pub mode: String,
    pub uptime_start: DateTime<Utc>,
    pub markets: HashMap<String, MarketRow>,
    pub recent_fills: Vec<FillRow>,
    pub total_realized_pnl: Decimal,
    pub total_fills: u64,
}

impl DashboardState {
    pub fn new(mode: &str) -> Self {
        Self {
            mode: mode.to_string(),
            uptime_start: Utc::now(),
            markets: HashMap::new(),
            recent_fills: Vec::new(),
            total_realized_pnl: Decimal::ZERO,
            total_fills: 0,
        }
    }

    pub fn update_market(&mut self, row: MarketRow) {
        self.markets.insert(row.token_id.clone(), row);
    }

    pub fn add_fill(&mut self, fill: FillRow) {
        self.total_fills += 1;
        self.total_realized_pnl = fill.pnl_after;
        self.recent_fills.push(fill);
        // Keep only the last 50 fills
        if self.recent_fills.len() > 50 {
            self.recent_fills.remove(0);
        }
    }

    /// Recalculate totals from market rows.
    pub fn refresh_totals(&mut self) {
        self.total_realized_pnl = self.markets.values().map(|m| m.realized_pnl).sum();
        self.total_fills = self.markets.values().map(|m| m.fill_count).sum();
    }
}

/// Thread-safe handle to dashboard state.
pub type SharedDashboard = Arc<RwLock<DashboardState>>;

pub fn new_shared_dashboard(mode: &str) -> SharedDashboard {
    Arc::new(RwLock::new(DashboardState::new(mode)))
}

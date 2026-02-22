use std::collections::HashMap;

use futures::StreamExt;
use rust_decimal::Decimal;
use tracing::{debug, error, info, warn};

use eutrader_core::{
    Config, Fill, InventoryPosition, MarketConfig, MarketSnapshot, OpenOrder, Quote, Side,
};
use eutrader_strategy::{Quoter, RiskManager};

use crate::executor::Executor;
use crate::paper::PaperExecutor;

/// The main market-making loop. Receives market snapshots, computes target
/// quotes via the `Quoter`, checks risk limits, and reconciles open orders
/// through the `Executor`.
pub struct OrderManager<E: Executor> {
    executor: E,
    _quoter: Quoter,
    _risk_manager: RiskManager,
    positions: HashMap<String, InventoryPosition>,
    config: Config,
    /// Lookup from token_id to its per-market config.
    market_configs: HashMap<String, MarketConfig>,
}

impl<E: Executor> OrderManager<E> {
    /// Build a new `OrderManager`.
    pub fn new(
        executor: E,
        quoter: Quoter,
        risk_manager: RiskManager,
        config: Config,
    ) -> Self {
        let market_configs: HashMap<String, MarketConfig> = config
            .markets
            .iter()
            .map(|m| (m.token_id.clone(), m.clone()))
            .collect();

        Self {
            executor,
            _quoter: quoter,
            _risk_manager: risk_manager,
            positions: HashMap::new(),
            config,
            market_configs,
        }
    }

    /// Run the main event loop, consuming a stream of `MarketSnapshot`s.
    ///
    /// For each snapshot the manager:
    /// 1. (Paper mode) checks for simulated fills
    /// 2. Retrieves/creates the inventory position for the token
    /// 3. Computes a target quote via the Quoter
    /// 4. Runs risk checks
    /// 5. Reconciles open orders (cancel stale, place new)
    /// 6. Logs current state
    ///
    /// The loop runs until the stream ends or Ctrl+C is received.
    pub async fn run(
        &mut self,
        mut snapshots: impl futures::Stream<Item = MarketSnapshot> + Unpin,
    ) {
        info!("order manager started — waiting for market data");

        let shutdown = tokio::signal::ctrl_c();
        tokio::pin!(shutdown);

        loop {
            tokio::select! {
                maybe_snap = snapshots.next() => {
                    match maybe_snap {
                        Some(snapshot) => {
                            if let Err(e) = self.handle_snapshot(&snapshot).await {
                                error!(
                                    token = %snapshot.token_id,
                                    error = %e,
                                    "error handling snapshot"
                                );
                            }
                        }
                        None => {
                            info!("snapshot stream ended — shutting down");
                            break;
                        }
                    }
                }
                _ = &mut shutdown => {
                    info!("ctrl+c received — shutting down gracefully");
                    break;
                }
            }
        }

        // Graceful shutdown: cancel all outstanding orders
        self.shutdown().await;
    }

    /// Process a single market snapshot.
    async fn handle_snapshot(
        &mut self,
        snapshot: &MarketSnapshot,
    ) -> eutrader_core::Result<()> {
        let token_id = &snapshot.token_id;

        let market_cfg = match self.market_configs.get(token_id) {
            Some(cfg) => <MarketConfig as Clone>::clone(cfg),
            None => {
                debug!(token = %token_id, "ignoring snapshot for unconfigured token");
                return Ok(());
            }
        };

        // Ensure we have a position tracker for this token
        self.positions
            .entry(token_id.clone())
            .or_insert_with(|| InventoryPosition::new(token_id.clone()));

        // --- Step 1: Compute target quote ---
        // Borrow position temporarily for quote computation
        let target_quote = {
            let position = &self.positions[token_id];
            Quoter::quote(snapshot, position, &market_cfg)
        };
        let target_quote = match target_quote {
            Some(q) => q,
            None => {
                debug!(token = %token_id, "quoter returned None — spread too tight, pulling quotes");
                self.executor.cancel_all().await?;
                return Ok(());
            }
        };

        // --- Step 2: Risk checks ---
        {
            let position = &self.positions[token_id];
            if let Err(e) = RiskManager::check_order(
                position,
                &target_quote,
                &self.config.risk,
            ) {
                warn!(
                    token = %token_id,
                    reason = %e,
                    "risk check failed — pulling quotes"
                );
                self.executor.cancel_all().await?;
                return Ok(());
            }
        }

        // --- Step 3: Reconcile orders ---
        self.reconcile_orders(token_id, &target_quote).await?;

        // --- Step 4: Log state ---
        let position = &self.positions[token_id];
        let unrealized = position.unrealized_pnl(snapshot.midpoint);
        info!(
            token = %token_id,
            mid = %snapshot.midpoint,
            our_bid = %target_quote.bid_price,
            our_ask = %target_quote.ask_price,
            spread = %target_quote.spread(),
            inventory = %position.net_position,
            realized_pnl = %position.realized_pnl,
            unrealized_pnl = %unrealized,
            fills = position.fill_count,
            "quote cycle"
        );

        Ok(())
    }

    /// Cancel stale orders and place new ones to match the target quote.
    async fn reconcile_orders(
        &self,
        token_id: &str,
        target: &Quote,
    ) -> eutrader_core::Result<()> {
        let current_orders = self.executor.open_orders().await?;

        // Filter to orders for this token
        let my_orders: Vec<&OpenOrder> = current_orders
            .iter()
            .filter(|o| o.token_id == token_id)
            .collect();

        // Check if current orders already match target
        let has_matching_bid = my_orders.iter().any(|o| {
            o.side == Side::Buy
                && o.price == target.bid_price
                && o.size == target.size
        });
        let has_matching_ask = my_orders.iter().any(|o| {
            o.side == Side::Sell
                && o.price == target.ask_price
                && o.size == target.size
        });

        if has_matching_bid && has_matching_ask && my_orders.len() == 2 {
            debug!(token = %token_id, "orders already match target — no action");
            return Ok(());
        }

        // Cancel all stale orders for this token
        for order in &my_orders {
            self.executor.cancel_order(&order.id).await?;
        }

        // Place new bid
        if target.bid_price > Decimal::ZERO && target.size > Decimal::ZERO {
            self.executor
                .place_order(token_id, Side::Buy, target.bid_price, target.size)
                .await?;
        }

        // Place new ask
        if target.ask_price > Decimal::ZERO && target.size > Decimal::ZERO {
            self.executor
                .place_order(token_id, Side::Sell, target.ask_price, target.size)
                .await?;
        }

        Ok(())
    }

    /// Apply simulated fills from the paper executor to inventory positions.
    pub fn apply_fills(&mut self, fills: &[Fill]) {
        for fill in fills {
            let position = self
                .positions
                .entry(fill.token_id.clone())
                .or_insert_with(|| InventoryPosition::new(fill.token_id.clone()));
            position.apply_fill(fill);
        }
    }

    /// Cancel all orders and print final PnL summary.
    async fn shutdown(&mut self) {
        info!("cancelling all open orders...");
        if let Err(e) = self.executor.cancel_all().await {
            error!(error = %e, "failed to cancel orders during shutdown");
        }

        self.print_pnl_summary();
    }

    /// Print a summary of realised PnL across all positions.
    pub fn print_pnl_summary(&self) {
        info!("=== Final PnL Summary ===");
        let mut total_realized = Decimal::ZERO;
        let mut total_fills: u64 = 0;

        for (token_id, pos) in &self.positions {
            info!(
                token = %token_id,
                net_position = %pos.net_position,
                avg_entry = %pos.avg_entry,
                realized_pnl = %pos.realized_pnl,
                fills = pos.fill_count,
            );
            total_realized += pos.realized_pnl;
            total_fills += pos.fill_count;
        }

        info!(
            total_realized_pnl = %total_realized,
            total_fills = total_fills,
            "session complete"
        );
    }

    /// Return a reference to all tracked positions.
    pub fn positions(&self) -> &HashMap<String, InventoryPosition> {
        &self.positions
    }
}

/// Specialised `OrderManager` that also handles paper fills on each tick.
impl OrderManager<PaperExecutor> {
    /// Run the main loop with paper fill detection.
    ///
    /// Before computing quotes on each snapshot, this checks whether any
    /// virtual orders have been filled by the market moving through them.
    pub async fn run_paper(
        &mut self,
        mut snapshots: impl futures::Stream<Item = MarketSnapshot> + Unpin,
    ) {
        info!("order manager started in PAPER mode — waiting for market data");

        let shutdown = tokio::signal::ctrl_c();
        tokio::pin!(shutdown);

        loop {
            tokio::select! {
                maybe_snap = snapshots.next() => {
                    match maybe_snap {
                        Some(snapshot) => {
                            // Check for paper fills before processing the snapshot
                            let fills = self.executor.check_fills(&snapshot).await;
                            if !fills.is_empty() {
                                self.apply_fills(&fills);
                            }

                            if let Err(e) = self.handle_snapshot(&snapshot).await {
                                error!(
                                    token = %snapshot.token_id,
                                    error = %e,
                                    "error handling snapshot"
                                );
                            }
                        }
                        None => {
                            info!("snapshot stream ended — shutting down");
                            break;
                        }
                    }
                }
                _ = &mut shutdown => {
                    info!("ctrl+c received — shutting down gracefully");
                    break;
                }
            }
        }

        self.shutdown().await;
    }
}

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use tokio::sync::Mutex;
use tracing::{debug, info};

use eutrader_core::{Fill, MarketSnapshot, OpenOrder, OrderId, Result, Side};

use crate::executor::Executor;

/// Internal mutable state for the paper executor.
struct PaperState {
    /// Virtual open orders keyed by OrderId.
    orders: HashMap<OrderId, OpenOrder>,
    /// Complete log of simulated fills.
    fills: Vec<Fill>,
    /// Monotonic counter for generating order IDs.
    next_id: u64,
}

impl PaperState {
    fn new() -> Self {
        Self {
            orders: HashMap::new(),
            fills: Vec::new(),
            next_id: 1,
        }
    }

    fn next_order_id(&mut self) -> OrderId {
        let id = OrderId(format!("paper-{}", self.next_id));
        self.next_id += 1;
        id
    }
}

/// Simulates order execution against live market data without placing
/// real orders on Polymarket. Useful for back-testing and paper trading.
pub struct PaperExecutor {
    state: Arc<Mutex<PaperState>>,
}

impl PaperExecutor {
    /// Create a new paper executor with empty state.
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(PaperState::new())),
        }
    }

    /// Check whether any virtual open orders would have been filled
    /// by the current market prices in the snapshot.
    ///
    /// - Buy orders fill when `best_ask <= our bid price`
    /// - Sell orders fill when `best_bid >= our ask price`
    ///
    /// Filled orders are removed from the internal map and returned
    /// as `Fill` structs.
    pub async fn check_fills(&self, snapshot: &MarketSnapshot) -> Vec<Fill> {
        let mut state = self.state.lock().await;
        let mut filled_ids = Vec::new();
        let mut fills = Vec::new();

        for (id, order) in state.orders.iter() {
            if order.token_id != snapshot.token_id {
                continue;
            }

            let should_fill = match order.side {
                // Our bid gets lifted: market ask <= our bid price
                Side::Buy => snapshot.best_ask <= order.price,
                // Our ask gets hit: market bid >= our ask price
                Side::Sell => snapshot.best_bid >= order.price,
            };

            if should_fill {
                let fill = Fill {
                    token_id: order.token_id.clone(),
                    side: order.side,
                    price: order.price,
                    size: order.size,
                    timestamp: Utc::now(),
                    is_simulated: true,
                };

                info!(
                    side = %fill.side,
                    price = %fill.price,
                    size = %fill.size,
                    token = %fill.token_id,
                    "paper fill"
                );

                fills.push(fill);
                filled_ids.push(id.clone());
            }
        }

        // Remove filled orders from the book
        for id in &filled_ids {
            state.orders.remove(id);
        }

        // Record fills in the trade log
        for fill in &fills {
            state.fills.push(fill.clone());
            Self::write_fill_log(fill);
        }

        fills
    }

    /// Append a single fill record to `paper_trades.jsonl` for post-session analysis.
    fn write_fill_log(fill: &Fill) {
        let line = match serde_json::to_string(fill) {
            Ok(json) => json,
            Err(e) => {
                tracing::warn!(error = %e, "failed to serialize fill for log");
                return;
            }
        };

        let result = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("paper_trades.jsonl")
            .and_then(|mut f| writeln!(f, "{}", line));

        if let Err(e) = result {
            tracing::warn!(error = %e, "failed to write paper trade log");
        }
    }

    /// Return a copy of all recorded fills.
    pub async fn fill_log(&self) -> Vec<Fill> {
        let state = self.state.lock().await;
        state.fills.clone()
    }

    /// Return the total number of simulated fills so far.
    pub async fn fill_count(&self) -> usize {
        let state = self.state.lock().await;
        state.fills.len()
    }
}

impl Default for PaperExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Executor for PaperExecutor {
    async fn place_order(
        &self,
        token_id: &str,
        side: Side,
        price: Decimal,
        size: Decimal,
    ) -> Result<OrderId> {
        let mut state = self.state.lock().await;
        let id = state.next_order_id();

        let order = OpenOrder {
            id: id.clone(),
            token_id: token_id.to_string(),
            side,
            price,
            size,
        };

        debug!(
            order_id = %id,
            side = %side,
            price = %price,
            size = %size,
            token = token_id,
            "paper order placed"
        );

        state.orders.insert(id.clone(), order);
        Ok(id)
    }

    async fn cancel_order(&self, id: &OrderId) -> Result<()> {
        let mut state = self.state.lock().await;
        if state.orders.remove(id).is_some() {
            debug!(order_id = %id, "paper order cancelled");
        } else {
            debug!(order_id = %id, "cancel: order not found (already filled or cancelled)");
        }
        Ok(())
    }

    async fn cancel_all(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        let count = state.orders.len();
        state.orders.clear();
        info!(count, "cancelled all paper orders");
        Ok(())
    }

    async fn open_orders(&self) -> Result<Vec<OpenOrder>> {
        let state = self.state.lock().await;
        Ok(state.orders.values().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn snapshot(token_id: &str, best_bid: Decimal, best_ask: Decimal) -> MarketSnapshot {
        let mid = (best_bid + best_ask) / dec!(2);
        MarketSnapshot {
            token_id: token_id.to_string(),
            best_bid,
            best_ask,
            midpoint: mid,
            spread: best_ask - best_bid,
            timestamp: Utc::now(),
        }
    }

    #[tokio::test]
    async fn place_and_cancel_order() {
        let exec = PaperExecutor::new();
        let id = exec
            .place_order("tok1", Side::Buy, dec!(0.50), dec!(10))
            .await
            .unwrap();

        let orders = exec.open_orders().await.unwrap();
        assert_eq!(orders.len(), 1);

        exec.cancel_order(&id).await.unwrap();
        let orders = exec.open_orders().await.unwrap();
        assert!(orders.is_empty());
    }

    #[tokio::test]
    async fn cancel_all_clears_orders() {
        let exec = PaperExecutor::new();
        exec.place_order("tok1", Side::Buy, dec!(0.50), dec!(10))
            .await
            .unwrap();
        exec.place_order("tok1", Side::Sell, dec!(0.55), dec!(10))
            .await
            .unwrap();

        exec.cancel_all().await.unwrap();
        let orders = exec.open_orders().await.unwrap();
        assert!(orders.is_empty());
    }

    #[tokio::test]
    async fn buy_order_fills_when_ask_crosses() {
        let exec = PaperExecutor::new();
        exec.place_order("tok1", Side::Buy, dec!(0.50), dec!(10))
            .await
            .unwrap();

        // Market ask drops to our bid price => fill
        let snap = snapshot("tok1", dec!(0.49), dec!(0.50));
        let fills = exec.check_fills(&snap).await;
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].side, Side::Buy);
        assert_eq!(fills[0].price, dec!(0.50));

        // Order should be removed after fill
        let orders = exec.open_orders().await.unwrap();
        assert!(orders.is_empty());
    }

    #[tokio::test]
    async fn sell_order_fills_when_bid_crosses() {
        let exec = PaperExecutor::new();
        exec.place_order("tok1", Side::Sell, dec!(0.55), dec!(10))
            .await
            .unwrap();

        // Market bid rises to our ask price => fill
        let snap = snapshot("tok1", dec!(0.55), dec!(0.60));
        let fills = exec.check_fills(&snap).await;
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].side, Side::Sell);
        assert_eq!(fills[0].price, dec!(0.55));
    }

    #[tokio::test]
    async fn no_fill_when_market_does_not_cross() {
        let exec = PaperExecutor::new();
        exec.place_order("tok1", Side::Buy, dec!(0.50), dec!(10))
            .await
            .unwrap();

        // Market ask is above our bid => no fill
        let snap = snapshot("tok1", dec!(0.49), dec!(0.52));
        let fills = exec.check_fills(&snap).await;
        assert!(fills.is_empty());

        let orders = exec.open_orders().await.unwrap();
        assert_eq!(orders.len(), 1);
    }

    #[tokio::test]
    async fn ignores_orders_for_different_tokens() {
        let exec = PaperExecutor::new();
        exec.place_order("tok1", Side::Buy, dec!(0.50), dec!(10))
            .await
            .unwrap();

        // Snapshot is for a different token
        let snap = snapshot("tok2", dec!(0.45), dec!(0.50));
        let fills = exec.check_fills(&snap).await;
        assert!(fills.is_empty());
    }
}

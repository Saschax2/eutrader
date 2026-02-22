use async_trait::async_trait;
use eutrader_core::{OpenOrder, OrderId, Result, Side};
use rust_decimal::Decimal;

/// Trait for order execution backends.
///
/// Implementations include `PaperExecutor` (simulated) and future
/// live executors that hit the Polymarket CLOB API.
#[async_trait]
pub trait Executor: Send + Sync {
    /// Place a limit order on the given token/side.
    async fn place_order(
        &self,
        token_id: &str,
        side: Side,
        price: Decimal,
        size: Decimal,
    ) -> Result<OrderId>;

    /// Cancel a single open order by its ID.
    async fn cancel_order(&self, id: &OrderId) -> Result<()>;

    /// Cancel every open order managed by this executor.
    async fn cancel_all(&self) -> Result<()>;

    /// Return all currently open orders.
    async fn open_orders(&self) -> Result<Vec<OpenOrder>>;
}

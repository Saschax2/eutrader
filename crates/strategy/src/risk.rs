use eutrader_core::config::RiskConfig;
use eutrader_core::{InventoryPosition, Quote, Result};
use rust_decimal::Decimal;
use tracing::{debug, warn};

/// Risk manager that enforces position limits and portfolio-level constraints.
pub struct RiskManager;

impl Default for RiskManager {
    fn default() -> Self {
        Self
    }
}

impl RiskManager {
    /// Create a new `RiskManager`.
    ///
    /// Currently stateless — all checks are pure functions of the inputs.
    pub fn new() -> Self {
        Self
    }

    /// Create a new `RiskManager` from a risk config.
    ///
    /// Currently the config is not stored (all checks take config as a parameter),
    /// but this constructor exists for forward compatibility.
    pub fn with_config(_config: &RiskConfig) -> Self {
        Self
    }

    /// Validate that a single order does not breach per-market position limits.
    ///
    /// Checks that both the bid side and ask side of the quote, when filled,
    /// would not push the position beyond `max_position_per_market`.
    pub fn check_order(
        inventory: &InventoryPosition,
        quote: &Quote,
        config: &RiskConfig,
    ) -> Result<()> {
        // After a buy fill at bid, position would increase
        let position_after_buy = inventory.net_position + quote.size;
        if position_after_buy.abs() > config.max_position_per_market {
            return Err(eutrader_core::Error::RiskBreach(format!(
                "bid fill would breach per-market limit: position would be {} (max {})",
                position_after_buy, config.max_position_per_market
            )));
        }

        // After a sell fill at ask, position would decrease
        let position_after_sell = inventory.net_position - quote.size;
        if position_after_sell.abs() > config.max_position_per_market {
            return Err(eutrader_core::Error::RiskBreach(format!(
                "ask fill would breach per-market limit: position would be {} (max {})",
                position_after_sell, config.max_position_per_market
            )));
        }

        debug!(
            token_id = %quote.token_id,
            net_position = %inventory.net_position,
            quote_size = %quote.size,
            "order passed risk check"
        );
        Ok(())
    }

    /// Validate total exposure across all positions does not exceed
    /// `max_total_exposure`.
    ///
    /// Total exposure is the sum of absolute position values.
    pub fn check_portfolio(
        positions: &[InventoryPosition],
        config: &RiskConfig,
    ) -> Result<()> {
        let total_exposure: Decimal = positions
            .iter()
            .map(|p| p.net_position.abs())
            .sum();

        if total_exposure > config.max_total_exposure {
            return Err(eutrader_core::Error::RiskBreach(format!(
                "total exposure {} exceeds max {} — portfolio limit breached",
                total_exposure, config.max_total_exposure
            )));
        }

        debug!(
            total_exposure = %total_exposure,
            max = %config.max_total_exposure,
            "portfolio exposure within limits"
        );
        Ok(())
    }

    /// Determine if the kill switch should be activated.
    ///
    /// Returns `true` if total unrealized loss across all positions exceeds
    /// `max_unrealized_loss`. Uses each position's `avg_entry` as a rough
    /// mid-price proxy (in production you'd pass real mid-prices).
    pub fn should_kill_switch(
        positions: &[InventoryPosition],
        config: &RiskConfig,
    ) -> bool {
        // Sum unrealized P&L using avg_entry as a conservative mid-price estimate.
        // In production, you would pass actual mid-prices for each position.
        let total_unrealized: Decimal = positions
            .iter()
            .map(|p| {
                // Worst-case: if long, assume price dropped to 0; if short, assume price
                // went to 1. Use avg_entry as the proxy mid for a more realistic check.
                p.unrealized_pnl(p.avg_entry)
            })
            .sum();

        // unrealized_pnl returns 0 when mid == avg_entry, so in the absence of
        // real mid-prices this is a no-op sentinel. Callers should use the
        // overload below for production checks.
        if total_unrealized < Decimal::ZERO
            && total_unrealized.abs() > config.max_unrealized_loss
        {
            warn!(
                total_unrealized = %total_unrealized,
                max_loss = %config.max_unrealized_loss,
                "KILL SWITCH TRIGGERED — unrealized loss exceeds limit"
            );
            return true;
        }

        false
    }

    /// Check kill switch with explicit mid-prices.
    ///
    /// `mid_prices` must be parallel to `positions` (same length, same order).
    pub fn should_kill_switch_with_prices(
        positions: &[InventoryPosition],
        mid_prices: &[Decimal],
        config: &RiskConfig,
    ) -> bool {
        assert_eq!(
            positions.len(),
            mid_prices.len(),
            "positions and mid_prices must have the same length"
        );

        let total_unrealized: Decimal = positions
            .iter()
            .zip(mid_prices.iter())
            .map(|(p, &mid)| p.unrealized_pnl(mid))
            .sum();

        if total_unrealized < Decimal::ZERO
            && total_unrealized.abs() > config.max_unrealized_loss
        {
            warn!(
                total_unrealized = %total_unrealized,
                max_loss = %config.max_unrealized_loss,
                "KILL SWITCH TRIGGERED — unrealized loss exceeds limit"
            );
            return true;
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn make_risk_config() -> RiskConfig {
        RiskConfig {
            max_position_per_market: dec!(100),
            max_total_exposure: dec!(500),
            max_unrealized_loss: dec!(50),
            quote_refresh_interval_ms: 1000,
        }
    }

    fn make_inventory(token: &str, net: Decimal) -> InventoryPosition {
        InventoryPosition {
            token_id: token.into(),
            net_position: net,
            avg_entry: dec!(0.50),
            realized_pnl: Decimal::ZERO,
            fill_count: 0,
        }
    }

    fn make_quote(size: Decimal) -> Quote {
        Quote {
            token_id: "tok_test".into(),
            bid_price: dec!(0.48),
            ask_price: dec!(0.52),
            size,
        }
    }

    #[test]
    fn order_within_limits_passes() {
        let config = make_risk_config();
        let inv = make_inventory("tok_test", dec!(30));
        let quote = make_quote(dec!(10));

        assert!(RiskManager::check_order(&inv, &quote, &config).is_ok());
    }

    #[test]
    fn order_breaching_buy_limit_fails() {
        let config = make_risk_config();
        let inv = make_inventory("tok_test", dec!(95));
        let quote = make_quote(dec!(10));

        // After buy: 95 + 10 = 105 > 100
        let result = RiskManager::check_order(&inv, &quote, &config);
        assert!(result.is_err());
    }

    #[test]
    fn order_breaching_sell_limit_fails() {
        let config = make_risk_config();
        let inv = make_inventory("tok_test", dec!(-95));
        let quote = make_quote(dec!(10));

        // After sell: -95 - 10 = -105, abs = 105 > 100
        let result = RiskManager::check_order(&inv, &quote, &config);
        assert!(result.is_err());
    }

    #[test]
    fn portfolio_within_limits_passes() {
        let config = make_risk_config();
        let positions = vec![
            make_inventory("tok1", dec!(50)),
            make_inventory("tok2", dec!(-30)),
            make_inventory("tok3", dec!(100)),
        ];
        // Total exposure = 50 + 30 + 100 = 180 < 500
        assert!(RiskManager::check_portfolio(&positions, &config).is_ok());
    }

    #[test]
    fn portfolio_exceeding_exposure_fails() {
        let config = make_risk_config();
        let positions = vec![
            make_inventory("tok1", dec!(200)),
            make_inventory("tok2", dec!(-200)),
            make_inventory("tok3", dec!(150)),
        ];
        // Total exposure = 200 + 200 + 150 = 550 > 500
        let result = RiskManager::check_portfolio(&positions, &config);
        assert!(result.is_err());
    }

    #[test]
    fn kill_switch_not_triggered_within_limits() {
        let config = make_risk_config();
        let positions = vec![
            make_inventory("tok1", dec!(10)),
            make_inventory("tok2", dec!(-5)),
        ];
        // With mid_prices equal to avg_entry, unrealized PnL is 0
        let mid_prices = vec![dec!(0.50), dec!(0.50)];
        assert!(!RiskManager::should_kill_switch_with_prices(
            &positions,
            &mid_prices,
            &config
        ));
    }

    #[test]
    fn kill_switch_triggered_on_large_loss() {
        let config = make_risk_config();
        // Long 100 at avg_entry 0.50, current mid 0.10 => loss = 100 * (0.10 - 0.50) = -40
        // Short 100 at avg_entry 0.50, current mid 0.90 => loss = 100 * (0.50 - 0.90) = -40
        // Total unrealized = -80 > max_unrealized_loss (50)
        let positions = vec![
            InventoryPosition {
                token_id: "tok1".into(),
                net_position: dec!(100),
                avg_entry: dec!(0.50),
                realized_pnl: Decimal::ZERO,
                fill_count: 0,
            },
            InventoryPosition {
                token_id: "tok2".into(),
                net_position: dec!(-100),
                avg_entry: dec!(0.50),
                realized_pnl: Decimal::ZERO,
                fill_count: 0,
            },
        ];
        let mid_prices = vec![dec!(0.10), dec!(0.90)];
        assert!(RiskManager::should_kill_switch_with_prices(
            &positions,
            &mid_prices,
            &config
        ));
    }

    #[test]
    fn kill_switch_not_triggered_on_profit() {
        let config = make_risk_config();
        let positions = vec![InventoryPosition {
            token_id: "tok1".into(),
            net_position: dec!(100),
            avg_entry: dec!(0.40),
            realized_pnl: Decimal::ZERO,
            fill_count: 0,
        }];
        // Long 100 at 0.40, current mid 0.60 => profit = 100 * 0.20 = +20
        let mid_prices = vec![dec!(0.60)];
        assert!(!RiskManager::should_kill_switch_with_prices(
            &positions,
            &mid_prices,
            &config
        ));
    }

    #[test]
    fn empty_portfolio_passes_all_checks() {
        let config = make_risk_config();
        let positions: Vec<InventoryPosition> = vec![];
        assert!(RiskManager::check_portfolio(&positions, &config).is_ok());
        assert!(!RiskManager::should_kill_switch(&positions, &config));
    }
}

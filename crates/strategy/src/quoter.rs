use eutrader_core::{InventoryPosition, MarketSnapshot, Quote};
use eutrader_core::config::MarketConfig;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::debug;

/// The core market-making quoting engine.
///
/// Given a market snapshot, current inventory, and config, produces a two-sided
/// quote with inventory-aware skew.
pub struct Quoter;

impl Default for Quoter {
    fn default() -> Self {
        Self
    }
}

impl Quoter {
    /// Create a new `Quoter`.
    pub fn new() -> Self {
        Self
    }

    /// Compute a two-sided quote for a market.
    ///
    /// Returns `None` if the resulting bid >= ask (spread too tight after
    /// clamping/skew) or if the snapshot is invalid.
    pub fn quote(
        snapshot: &MarketSnapshot,
        inventory: &InventoryPosition,
        config: &MarketConfig,
    ) -> Option<Quote> {
        let mid = snapshot.midpoint;

        // --- Half spread ---
        let half_spread =
            Decimal::from(config.spread_bps) / dec!(10000) / dec!(2);

        // --- Base quotes ---
        let mut bid = mid - half_spread;
        let mut ask = mid + half_spread;

        // --- Inventory skew ---
        // Positive net_position (long) => skew pushes both quotes down so we
        // become more eager to sell and less eager to buy.
        let skew = inventory.net_position * config.skew_factor;
        bid -= skew;
        ask -= skew;

        // --- Round to tick size 0.01 ---
        // Floor for bid (conservative buy), ceil for ask (conservative sell).
        bid = floor_to_tick(bid, dec!(0.01));
        ask = ceil_to_tick(ask, dec!(0.01));

        // --- Clamp to [0.01, 0.99] ---
        bid = bid.max(dec!(0.01)).min(dec!(0.99));
        ask = ask.max(dec!(0.01)).min(dec!(0.99));

        // --- Check spread validity ---
        if bid >= ask {
            debug!(
                token_id = %snapshot.token_id,
                %bid, %ask,
                "spread too tight after skew/clamp — no quote"
            );
            return None;
        }

        // --- Size reduction near max inventory ---
        let mut size = config.size;
        if config.max_inventory > Decimal::ZERO {
            let utilization = inventory.net_position.abs() / config.max_inventory;
            if utilization > dec!(0.8) {
                // Linear reduction: at 80% usage keep full size, at 100% reduce to 20%
                let reduction = dec!(1) - (utilization - dec!(0.8)) / dec!(0.2) * dec!(0.8);
                size = (size * reduction.max(dec!(0.2))).max(dec!(1));
            }
        }

        Some(Quote {
            token_id: snapshot.token_id.clone(),
            bid_price: bid,
            ask_price: ask,
            size,
        })
    }
}

/// Floor a value to the nearest tick (round down).
fn floor_to_tick(value: Decimal, tick: Decimal) -> Decimal {
    (value / tick).floor() * tick
}

/// Ceil a value to the nearest tick (round up).
fn ceil_to_tick(value: Decimal, tick: Decimal) -> Decimal {
    (value / tick).ceil() * tick
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rust_decimal_macros::dec;

    fn make_snapshot(mid: Decimal) -> MarketSnapshot {
        MarketSnapshot {
            token_id: "tok_test".into(),
            best_bid: mid - dec!(0.01),
            best_ask: mid + dec!(0.01),
            midpoint: mid,
            spread: dec!(0.02),
            timestamp: Utc::now(),
        }
    }

    fn make_config(spread_bps: u32) -> MarketConfig {
        MarketConfig {
            name: "Test".into(),
            token_id: "tok_test".into(),
            spread_bps,
            size: dec!(10),
            max_inventory: dec!(50),
            skew_factor: dec!(0.001),
        }
    }

    fn make_inventory(net_position: Decimal) -> InventoryPosition {
        InventoryPosition {
            token_id: "tok_test".into(),
            net_position,
            avg_entry: dec!(0.50),
            realized_pnl: Decimal::ZERO,
            fill_count: 0,
        }
    }

    #[test]
    fn zero_inventory_produces_symmetric_quotes() {
        let snap = make_snapshot(dec!(0.50));
        let inv = make_inventory(Decimal::ZERO);
        let config = make_config(300); // 3% spread => 1.5% each side

        let quote = Quoter::quote(&snap, &inv, &config).unwrap();

        // half_spread = 300 / 10000 / 2 = 0.015
        // bid = 0.50 - 0.015 = 0.485 -> floor(0.01) = 0.48
        // ask = 0.50 + 0.015 = 0.515 -> ceil(0.01) = 0.52
        assert_eq!(quote.bid_price, dec!(0.48));
        assert_eq!(quote.ask_price, dec!(0.52));
        assert_eq!(quote.size, dec!(10));
    }

    #[test]
    fn long_inventory_skews_quotes_down() {
        let snap = make_snapshot(dec!(0.50));
        let inv = make_inventory(dec!(20)); // long 20
        let config = make_config(300);

        let quote = Quoter::quote(&snap, &inv, &config).unwrap();

        // skew = 20 * 0.001 = 0.02
        // bid = 0.50 - 0.015 - 0.02 = 0.465 -> floor = 0.46
        // ask = 0.50 + 0.015 - 0.02 = 0.495 -> ceil  = 0.50
        assert_eq!(quote.bid_price, dec!(0.46));
        assert_eq!(quote.ask_price, dec!(0.50));
    }

    #[test]
    fn short_inventory_skews_quotes_up() {
        let snap = make_snapshot(dec!(0.50));
        let inv = make_inventory(dec!(-20)); // short 20
        let config = make_config(300);

        let quote = Quoter::quote(&snap, &inv, &config).unwrap();

        // skew = -20 * 0.001 = -0.02
        // bid = 0.50 - 0.015 - (-0.02) = 0.505 -> floor = 0.50
        // ask = 0.50 + 0.015 - (-0.02) = 0.535 -> ceil  = 0.54
        assert_eq!(quote.bid_price, dec!(0.50));
        assert_eq!(quote.ask_price, dec!(0.54));
    }

    #[test]
    fn prices_clamped_to_valid_range() {
        // Very high midpoint — ask should be clamped to 0.99
        let snap = make_snapshot(dec!(0.98));
        let inv = make_inventory(Decimal::ZERO);
        let config = make_config(300);

        let quote = Quoter::quote(&snap, &inv, &config).unwrap();
        assert!(quote.ask_price <= dec!(0.99));
        assert!(quote.bid_price >= dec!(0.01));

        // Very low midpoint — bid should be clamped to 0.01
        let snap_low = make_snapshot(dec!(0.02));
        let quote_low = Quoter::quote(&snap_low, &inv, &config).unwrap();
        assert!(quote_low.bid_price >= dec!(0.01));
        assert!(quote_low.ask_price <= dec!(0.99));
    }

    #[test]
    fn bid_gte_ask_returns_none() {
        // Extreme skew that would push bid above ask after clamping
        let snap = make_snapshot(dec!(0.98));
        let inv = make_inventory(dec!(-500)); // massive short
        let config = MarketConfig {
            name: "Test".into(),
            token_id: "tok_test".into(),
            spread_bps: 100, // tight 1% spread
            size: dec!(10),
            max_inventory: dec!(50),
            skew_factor: dec!(0.01), // aggressive skew
        };

        // skew = -500 * 0.01 = -5.0 (massive upward push)
        // bid = 0.98 - 0.005 + 5.0 = 5.975 -> clamped to 0.99
        // ask = 0.98 + 0.005 + 5.0 = 5.985 -> clamped to 0.99
        // bid >= ask => None
        let quote = Quoter::quote(&snap, &inv, &config);
        assert!(quote.is_none());
    }

    #[test]
    fn size_reduced_near_max_inventory() {
        let snap = make_snapshot(dec!(0.50));
        let inv = make_inventory(dec!(45)); // 90% of max_inventory=50
        let config = make_config(300);

        let quote = Quoter::quote(&snap, &inv, &config).unwrap();

        // utilization = 45/50 = 0.9 > 0.8
        // reduction = 1 - (0.9 - 0.8)/0.2 * 0.8 = 1 - 0.5*0.8 = 1 - 0.4 = 0.6
        // size = 10 * 0.6 = 6
        assert_eq!(quote.size, dec!(6));
    }

    #[test]
    fn size_at_max_inventory_is_minimum() {
        let snap = make_snapshot(dec!(0.50));
        let inv = make_inventory(dec!(50)); // 100% of max_inventory
        let config = make_config(300);

        let quote = Quoter::quote(&snap, &inv, &config).unwrap();

        // utilization = 50/50 = 1.0
        // reduction = 1 - (1.0 - 0.8)/0.2 * 0.8 = 1 - 1.0*0.8 = 0.2
        // size = 10 * 0.2 = 2, but min is 1
        assert_eq!(quote.size, dec!(2));
    }
}

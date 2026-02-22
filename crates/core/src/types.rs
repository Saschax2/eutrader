use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Unique order identifier
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OrderId(pub String);

impl fmt::Display for OrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Buy or Sell
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Side::Buy => write!(f, "BUY"),
            Side::Sell => write!(f, "SELL"),
        }
    }
}

/// A two-sided quote to post on the book
#[derive(Debug, Clone)]
pub struct Quote {
    pub token_id: String,
    pub bid_price: Decimal,
    pub ask_price: Decimal,
    pub size: Decimal,
}

impl Quote {
    pub fn spread(&self) -> Decimal {
        self.ask_price - self.bid_price
    }
}

/// A simulated or real fill
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub token_id: String,
    pub side: Side,
    pub price: Decimal,
    pub size: Decimal,
    pub timestamp: DateTime<Utc>,
    pub is_simulated: bool,
}

/// Current inventory for a single market
#[derive(Debug, Clone, Default)]
pub struct InventoryPosition {
    pub token_id: String,
    /// Positive = long, negative = short
    pub net_position: Decimal,
    pub avg_entry: Decimal,
    pub realized_pnl: Decimal,
    pub fill_count: u64,
}

impl InventoryPosition {
    pub fn new(token_id: String) -> Self {
        Self {
            token_id,
            ..Default::default()
        }
    }

    /// Apply a fill to this position
    pub fn apply_fill(&mut self, fill: &Fill) {
        let signed_size = match fill.side {
            Side::Buy => fill.size,
            Side::Sell => -fill.size,
        };

        let old_position = self.net_position;
        self.net_position += signed_size;

        // Update average entry for increasing positions
        if (old_position >= Decimal::ZERO && signed_size > Decimal::ZERO)
            || (old_position <= Decimal::ZERO && signed_size < Decimal::ZERO)
        {
            // Increasing position — update avg entry
            let old_cost = old_position.abs() * self.avg_entry;
            let new_cost = signed_size.abs() * fill.price;
            let total_size = old_position.abs() + signed_size.abs();
            if total_size > Decimal::ZERO {
                self.avg_entry = (old_cost + new_cost) / total_size;
            }
        } else {
            // Reducing or flipping — realize PnL on the closed portion
            let closed_size = signed_size.abs().min(old_position.abs());
            let pnl_per_unit = match fill.side {
                Side::Sell => fill.price - self.avg_entry,
                Side::Buy => self.avg_entry - fill.price,
            };
            self.realized_pnl += closed_size * pnl_per_unit;

            // If we flipped sides, reset avg entry to fill price
            if (self.net_position > Decimal::ZERO && old_position < Decimal::ZERO)
                || (self.net_position < Decimal::ZERO && old_position > Decimal::ZERO)
            {
                self.avg_entry = fill.price;
            }
        }

        self.fill_count += 1;
    }

    pub fn unrealized_pnl(&self, mid_price: Decimal) -> Decimal {
        if self.net_position > Decimal::ZERO {
            self.net_position * (mid_price - self.avg_entry)
        } else if self.net_position < Decimal::ZERO {
            self.net_position.abs() * (self.avg_entry - mid_price)
        } else {
            Decimal::ZERO
        }
    }
}

/// Snapshot of a market's orderbook state
#[derive(Debug, Clone)]
pub struct MarketSnapshot {
    pub token_id: String,
    pub best_bid: Decimal,
    pub best_ask: Decimal,
    pub midpoint: Decimal,
    pub spread: Decimal,
    pub timestamp: DateTime<Utc>,
}

/// An open order on the book
#[derive(Debug, Clone)]
pub struct OpenOrder {
    pub id: OrderId,
    pub token_id: String,
    pub side: Side,
    pub price: Decimal,
    pub size: Decimal,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn inventory_tracks_buys_and_sells() {
        let mut inv = InventoryPosition::new("test".into());

        // Buy 10 at 0.50
        inv.apply_fill(&Fill {
            token_id: "test".into(),
            side: Side::Buy,
            price: dec!(0.50),
            size: dec!(10),
            timestamp: Utc::now(),
            is_simulated: true,
        });
        assert_eq!(inv.net_position, dec!(10));
        assert_eq!(inv.avg_entry, dec!(0.50));

        // Sell 10 at 0.55 — realize profit
        inv.apply_fill(&Fill {
            token_id: "test".into(),
            side: Side::Sell,
            price: dec!(0.55),
            size: dec!(10),
            timestamp: Utc::now(),
            is_simulated: true,
        });
        assert_eq!(inv.net_position, dec!(0));
        assert_eq!(inv.realized_pnl, dec!(0.50)); // 10 * 0.05
    }

    #[test]
    fn quote_spread_calculation() {
        let q = Quote {
            token_id: "test".into(),
            bid_price: dec!(0.48),
            ask_price: dec!(0.52),
            size: dec!(10),
        };
        assert_eq!(q.spread(), dec!(0.04));
    }
}

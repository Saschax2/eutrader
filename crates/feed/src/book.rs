use chrono::Utc;
use eutrader_core::{MarketSnapshot, Result};
use reqwest::Client;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tracing::instrument;

const CLOB_BASE_URL: &str = "https://clob.polymarket.com";

/// A single price level (bid or ask) from the CLOB orderbook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceLevel {
    pub price: String,
    pub size: String,
}

/// The raw orderbook response from the CLOB REST API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookResponse {
    pub market: String,
    pub asset_id: String,
    pub bids: Vec<PriceLevel>,
    pub asks: Vec<PriceLevel>,
}

/// Client for the Polymarket CLOB REST API.
pub struct BookClient {
    client: Client,
}

impl BookClient {
    /// Create a new `BookClient` with a default reqwest client.
    pub fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }

    /// Fetch the full orderbook for a given token.
    #[instrument(skip(self), name = "book_get_orderbook")]
    pub async fn get_orderbook(&self, token_id: &str) -> Result<OrderBookResponse> {
        let url = format!("{CLOB_BASE_URL}/book?token_id={token_id}");
        let book: OrderBookResponse = self
            .client
            .get(&url)
            .send()
            .await?
            .error_for_status()
            .map_err(|e| eutrader_core::Error::Feed(format!("CLOB book HTTP error: {e}")))?
            .json()
            .await?;

        tracing::debug!(
            token_id,
            bids = book.bids.len(),
            asks = book.asks.len(),
            "fetched orderbook"
        );
        Ok(book)
    }

    /// Fetch the midpoint price for a given token.
    #[instrument(skip(self), name = "book_get_midpoint")]
    pub async fn get_midpoint(&self, token_id: &str) -> Result<Decimal> {
        let url = format!("{CLOB_BASE_URL}/midpoint?token_id={token_id}");
        let resp: serde_json::Value = self
            .client
            .get(&url)
            .send()
            .await?
            .error_for_status()
            .map_err(|e| eutrader_core::Error::Feed(format!("CLOB midpoint HTTP error: {e}")))?
            .json()
            .await?;

        let mid_str = resp["mid"]
            .as_str()
            .ok_or_else(|| eutrader_core::Error::Feed("missing 'mid' field in response".into()))?;

        Decimal::from_str(mid_str)
            .map_err(|e| eutrader_core::Error::Feed(format!("invalid midpoint decimal: {e}")))
    }
}

impl Default for BookClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert a raw `OrderBookResponse` into a core `MarketSnapshot`.
///
/// Returns `None` if bids or asks are empty (cannot compute meaningful snapshot).
pub fn to_snapshot(token_id: &str, book: &OrderBookResponse) -> Option<MarketSnapshot> {
    let best_bid = book
        .bids
        .iter()
        .filter_map(|l| Decimal::from_str(&l.price).ok())
        .max()?;

    let best_ask = book
        .asks
        .iter()
        .filter_map(|l| Decimal::from_str(&l.price).ok())
        .min()?;

    if best_bid >= best_ask {
        tracing::warn!(token_id, %best_bid, %best_ask, "crossed book — skipping snapshot");
        return None;
    }

    let midpoint = (best_bid + best_ask) / Decimal::from(2);
    let spread = best_ask - best_bid;

    Some(MarketSnapshot {
        token_id: token_id.to_string(),
        best_bid,
        best_ask,
        midpoint,
        spread,
        timestamp: Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_book(bids: &[(&str, &str)], asks: &[(&str, &str)]) -> OrderBookResponse {
        OrderBookResponse {
            market: "test_market".into(),
            asset_id: "test_asset".into(),
            bids: bids
                .iter()
                .map(|(p, s)| PriceLevel {
                    price: p.to_string(),
                    size: s.to_string(),
                })
                .collect(),
            asks: asks
                .iter()
                .map(|(p, s)| PriceLevel {
                    price: p.to_string(),
                    size: s.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn snapshot_from_valid_book() {
        let book = make_book(&[("0.48", "100"), ("0.47", "50")], &[("0.52", "80"), ("0.53", "60")]);
        let snap = to_snapshot("tok1", &book).unwrap();

        assert_eq!(snap.best_bid, Decimal::from_str("0.48").unwrap());
        assert_eq!(snap.best_ask, Decimal::from_str("0.52").unwrap());
        assert_eq!(snap.midpoint, Decimal::from_str("0.50").unwrap());
        assert_eq!(snap.spread, Decimal::from_str("0.04").unwrap());
        assert_eq!(snap.token_id, "tok1");
    }

    #[test]
    fn snapshot_none_for_empty_bids() {
        let book = make_book(&[], &[("0.52", "80")]);
        assert!(to_snapshot("tok1", &book).is_none());
    }

    #[test]
    fn snapshot_none_for_empty_asks() {
        let book = make_book(&[("0.48", "100")], &[]);
        assert!(to_snapshot("tok1", &book).is_none());
    }

    #[test]
    fn snapshot_none_for_crossed_book() {
        let book = make_book(&[("0.55", "100")], &[("0.50", "80")]);
        assert!(to_snapshot("tok1", &book).is_none());
    }
}

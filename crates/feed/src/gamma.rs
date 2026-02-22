use eutrader_core::config::{AutoDiscoverConfig, MarketConfig};
use eutrader_core::Result;
use reqwest::Client;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::{info, instrument};

const GAMMA_API_URL: &str =
    "https://gamma-api.polymarket.com/markets?closed=false&enableOrderBook=true&limit=100";

/// A token within a Gamma market (Yes / No outcome).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub token_id: String,
    pub outcome: String,
    pub price: Decimal,
}

/// A market returned by the Gamma API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GammaMarket {
    pub condition_id: String,
    pub question: String,
    /// Legacy nested token objects (may not always be present).
    #[serde(default)]
    pub tokens: Vec<Token>,
    /// CLOB token IDs: [Yes token ID, No token ID].
    /// The Gamma API returns this as a JSON string (stringified array), not a native array.
    #[serde(default, deserialize_with = "deserialize_clob_token_ids")]
    pub clob_token_ids: Vec<String>,
    pub active: bool,
    pub closed: bool,
    #[serde(default)]
    pub volume_num: f64,
}

impl GammaMarket {
    /// Get the YES token ID, preferring clobTokenIds over tokens[].
    pub fn yes_token_id(&self) -> Option<&str> {
        self.clob_token_ids
            .first()
            .map(|s| s.as_str())
            .or_else(|| self.tokens.first().map(|t| t.token_id.as_str()))
    }

    /// Get the NO token ID.
    pub fn no_token_id(&self) -> Option<&str> {
        self.clob_token_ids
            .get(1)
            .map(|s| s.as_str())
            .or_else(|| self.tokens.get(1).map(|t| t.token_id.as_str()))
    }
}

/// Client for the Polymarket Gamma API.
pub struct GammaClient {
    client: Client,
}

impl GammaClient {
    /// Create a new `GammaClient` with a default reqwest client.
    pub fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }

    /// Fetch active, order-book-enabled markets from the Gamma API.
    #[instrument(skip(self), name = "gamma_fetch_markets")]
    pub async fn fetch_markets(&self) -> Result<Vec<GammaMarket>> {
        let markets: Vec<GammaMarket> = self
            .client
            .get(GAMMA_API_URL)
            .send()
            .await?
            .error_for_status()
            .map_err(|e| eutrader_core::Error::Feed(format!("Gamma API HTTP error: {e}")))?
            .json()
            .await?;

        tracing::info!(count = markets.len(), "fetched markets from Gamma API");
        Ok(markets)
    }

    /// Auto-discover markets based on volume and config criteria.
    ///
    /// Fetches active markets from the Gamma API, filters by minimum volume,
    /// sorts by volume descending, and returns MarketConfig entries ready to trade.
    #[instrument(skip(self, config), name = "gamma_discover_markets")]
    pub async fn discover_markets(&self, config: &AutoDiscoverConfig) -> Result<Vec<MarketConfig>> {
        let markets = self.fetch_markets().await?;

        let mut candidates: Vec<&GammaMarket> = markets
            .iter()
            .filter(|m| m.active && !m.closed && m.volume_num >= config.min_volume)
            .filter(|m| m.yes_token_id().is_some()) // Must have at least a YES token
            .collect();

        // Sort by volume descending — highest volume = tightest spreads = best for MM
        candidates.sort_by(|a, b| b.volume_num.partial_cmp(&a.volume_num).unwrap_or(std::cmp::Ordering::Equal));
        candidates.truncate(config.max_markets);

        let market_configs: Vec<MarketConfig> = candidates
            .iter()
            .filter_map(|m| {
                let token_id = m.yes_token_id()?;
                info!(
                    question = %m.question,
                    token_id = %token_id,
                    volume = m.volume_num,
                    "auto-discovered market"
                );
                Some(MarketConfig {
                    name: truncate_question(&m.question, 50),
                    token_id: token_id.to_string(),
                    spread_bps: config.spread_bps,
                    size: config.size,
                    max_inventory: config.max_inventory,
                    skew_factor: config.skew_factor,
                })
            })
            .collect();

        info!(count = market_configs.len(), "auto-discovery complete");
        Ok(market_configs)
    }
}

/// Deserialize clobTokenIds which can be either a JSON array or a stringified JSON array.
fn deserialize_clob_token_ids<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        Vec(Vec<String>),
        String(String),
    }

    match StringOrVec::deserialize(deserializer)? {
        StringOrVec::Vec(v) => Ok(v),
        StringOrVec::String(s) => {
            // Try parsing the string as a JSON array
            serde_json::from_str(&s).map_err(de::Error::custom)
        }
    }
}

fn truncate_question(q: &str, max: usize) -> String {
    if q.len() <= max {
        q.to_string()
    } else {
        format!("{}...", &q[..max - 3])
    }
}

impl Default for GammaClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_gamma_market_with_clob_token_ids() {
        let json = r#"{
            "conditionId": "0xabc",
            "question": "Will it rain?",
            "tokens": [],
            "clobTokenIds": ["tok_yes_123", "tok_no_456"],
            "active": true,
            "closed": false,
            "volumeNum": 12345.67
        }"#;

        let market: GammaMarket = serde_json::from_str(json).unwrap();
        assert_eq!(market.condition_id, "0xabc");
        assert_eq!(market.yes_token_id(), Some("tok_yes_123"));
        assert_eq!(market.no_token_id(), Some("tok_no_456"));
        assert!(market.active);
        assert!(!market.closed);
    }

    #[test]
    fn deserializes_gamma_market_with_legacy_tokens() {
        let json = r#"{
            "conditionId": "0xdef",
            "question": "Will BTC hit 100k?",
            "tokens": [
                { "token_id": "tok_yes", "outcome": "Yes", "price": 0.55 },
                { "token_id": "tok_no", "outcome": "No", "price": 0.45 }
            ],
            "active": true,
            "closed": false,
            "volumeNum": 99999.0
        }"#;

        let market: GammaMarket = serde_json::from_str(json).unwrap();
        assert_eq!(market.yes_token_id(), Some("tok_yes"));
        assert_eq!(market.no_token_id(), Some("tok_no"));
    }
}

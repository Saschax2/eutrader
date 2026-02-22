use rust_decimal::Decimal;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub mode: Mode,
    pub risk: RiskConfig,
    #[serde(default)]
    pub auto_discover: Option<AutoDiscoverConfig>,
    #[serde(default)]
    pub markets: Vec<MarketConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AutoDiscoverConfig {
    /// Minimum 24h volume (USD) to consider a market
    #[serde(default = "default_min_volume")]
    pub min_volume: f64,
    /// Maximum number of markets to trade simultaneously
    #[serde(default = "default_max_markets")]
    pub max_markets: usize,
    /// Default spread in bps for auto-discovered markets
    #[serde(default = "default_spread_bps")]
    pub spread_bps: u32,
    /// Default quote size for auto-discovered markets
    pub size: Decimal,
    /// Default max inventory for auto-discovered markets
    pub max_inventory: Decimal,
    /// Default skew factor for auto-discovered markets
    #[serde(default = "default_skew_factor")]
    pub skew_factor: Decimal,
}

fn default_min_volume() -> f64 {
    10_000.0
}
fn default_max_markets() -> usize {
    5
}
fn default_spread_bps() -> u32 {
    400
}
fn default_skew_factor() -> Decimal {
    rust_decimal_macros::dec!(0.001)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Paper,
    Live,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RiskConfig {
    pub max_position_per_market: Decimal,
    pub max_total_exposure: Decimal,
    pub max_unrealized_loss: Decimal,
    pub quote_refresh_interval_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MarketConfig {
    pub name: String,
    pub token_id: String,
    /// Spread in basis points (e.g. 300 = 3%)
    pub spread_bps: u32,
    /// Number of shares to quote per side
    pub size: Decimal,
    /// Max net position before reducing quotes
    pub max_inventory: Decimal,
    /// How aggressively to skew quotes based on inventory
    pub skew_factor: Decimal,
}

impl Config {
    pub fn load(path: &Path) -> crate::Result<Self> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| crate::Error::Config(format!("Failed to read {}: {e}", path.display())))?;
        let config: Config = toml::from_str(&contents)
            .map_err(|e| crate::Error::Config(format!("Failed to parse config: {e}")))?;

        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> crate::Result<()> {
        if self.markets.is_empty() && self.auto_discover.is_none() {
            return Err(crate::Error::Config(
                "No markets configured and auto_discover not enabled. \
                 Add [[markets]] entries or [auto_discover] to config."
                    .into(),
            ));
        }
        for m in &self.markets {
            if m.spread_bps == 0 {
                return Err(crate::Error::Config(format!(
                    "Market '{}' has zero spread",
                    m.name
                )));
            }
            if m.size <= Decimal::ZERO {
                return Err(crate::Error::Config(format!(
                    "Market '{}' has non-positive size",
                    m.name
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_config() {
        let toml = r#"
            mode = "paper"

            [risk]
            max_position_per_market = 100.0
            max_total_exposure = 500.0
            max_unrealized_loss = 50.0
            quote_refresh_interval_ms = 1000

            [[markets]]
            name = "Test"
            token_id = "abc123"
            spread_bps = 300
            size = 10.0
            max_inventory = 50.0
            skew_factor = 0.001
        "#;

        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.mode, Mode::Paper);
        assert_eq!(config.markets.len(), 1);
        assert_eq!(config.markets[0].spread_bps, 300);
    }

    #[test]
    fn rejects_empty_markets() {
        let toml = r#"
            mode = "paper"

            [risk]
            max_position_per_market = 100.0
            max_total_exposure = 500.0
            max_unrealized_loss = 50.0
            quote_refresh_interval_ms = 1000
        "#;

        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.validate().is_err());
    }
}

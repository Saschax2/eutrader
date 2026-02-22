pub mod config;
pub mod dashboard;
pub mod error;
pub mod types;

pub use config::{AutoDiscoverConfig, Config, MarketConfig, Mode, RiskConfig};
pub use error::Error;
pub use types::*;

pub type Result<T> = std::result::Result<T, Error>;

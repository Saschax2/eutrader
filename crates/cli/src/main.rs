use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use tracing::info;
use tracing_subscriber::EnvFilter;

use eutrader_core::{Config, Mode};
use eutrader_engine::{OrderManager, PaperExecutor};
use eutrader_feed::{FeedManager, GammaClient};
use eutrader_strategy::{Quoter, RiskManager};

/// eutrader — Polymarket market-making engine
#[derive(Parser)]
#[command(name = "eutrader", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the market-making engine.
    Run {
        /// Path to the TOML configuration file.
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,

        /// Override the execution mode from the config file.
        #[arg(short, long)]
        mode: Option<ModeArg>,
    },
    /// Discover available Polymarket markets sorted by volume.
    Discover {
        /// Minimum 24h volume in USD to show.
        #[arg(long, default_value = "10000")]
        min_volume: f64,

        /// Maximum number of markets to display.
        #[arg(long, default_value = "20")]
        limit: usize,
    },
}

/// CLI-level mode argument, mapped to `eutrader_core::Mode`.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum ModeArg {
    Paper,
    Live,
}

impl From<ModeArg> for Mode {
    fn from(arg: ModeArg) -> Self {
        match arg {
            ModeArg::Paper => Mode::Paper,
            ModeArg::Live => Mode::Live,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialise tracing with RUST_LOG env filter (default: info)
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Run { config: path, mode } => run(path, mode).await,
        Commands::Discover { min_volume, limit } => discover(min_volume, limit).await,
    }
}

async fn discover(min_volume: f64, limit: usize) -> Result<()> {
    info!("discovering active Polymarket markets (min volume: ${min_volume})...");

    let client = GammaClient::new();
    let mut markets = client.fetch_markets().await
        .context("failed to fetch markets from Gamma API")?;

    // Filter and sort
    markets.retain(|m| m.active && !m.closed && m.volume_num >= min_volume && m.yes_token_id().is_some());
    markets.sort_by(|a, b| b.volume_num.partial_cmp(&a.volume_num).unwrap_or(std::cmp::Ordering::Equal));
    markets.truncate(limit);

    if markets.is_empty() {
        info!("no markets found matching criteria");
        return Ok(());
    }

    println!("\n{:<60} {:>12} {}", "Market", "Volume ($)", "YES Token ID");
    println!("{}", "-".repeat(120));
    for m in &markets {
        let token_id = m.yes_token_id().unwrap_or("N/A");
        println!(
            "{:<60} {:>12.0} {}",
            if m.question.len() > 57 { format!("{}...", &m.question[..57]) } else { m.question.clone() },
            m.volume_num,
            token_id,
        );
    }
    println!("\nFound {} markets. Copy a token_id into config.toml or use [auto_discover].\n", markets.len());

    Ok(())
}

async fn run(config_path: PathBuf, mode_override: Option<ModeArg>) -> Result<()> {
    // --- Load configuration ---
    let mut config = Config::load(&config_path)
        .with_context(|| format!("failed to load config from {}", config_path.display()))?;

    // Apply CLI mode override if provided
    if let Some(m) = mode_override {
        config.mode = m.into();
    }

    // Auto-discover markets if configured and no manual markets specified
    if config.markets.is_empty() {
        if let Some(ref discover_config) = config.auto_discover {
            info!("no manual markets configured — running auto-discovery...");
            let gamma = GammaClient::new();
            let discovered = gamma.discover_markets(discover_config).await
                .context("auto-discovery failed")?;
            if discovered.is_empty() {
                anyhow::bail!("auto-discovery found no markets matching criteria");
            }
            config.markets = discovered;
        }
    }

    let mode = config.mode;
    let num_markets = config.markets.len();
    let token_ids: Vec<String> = config.markets.iter().map(|m| m.token_id.clone()).collect();

    // --- Startup banner ---
    info!("========================================");
    info!("  eutrader — Polymarket Market Maker");
    info!("========================================");
    info!("mode:           {:?}", mode);
    info!("markets:        {}", num_markets);
    info!(
        "max pos/market: {}",
        config.risk.max_position_per_market
    );
    info!(
        "max exposure:   {}",
        config.risk.max_total_exposure
    );
    info!(
        "max unreal loss:{}",
        config.risk.max_unrealized_loss
    );
    info!("refresh:        {} ms", config.risk.quote_refresh_interval_ms);
    info!("----------------------------------------");

    for market in &config.markets {
        info!(
            "  [{}] spread={}bps size={} max_inv={}",
            market.name, market.spread_bps, market.size, market.max_inventory
        );
    }
    info!("========================================");

    // --- Build components ---
    let quoter = Quoter::new();
    let risk_manager = RiskManager::new();
    let feed = FeedManager::new(token_ids);

    match mode {
        Mode::Paper => {
            let executor = PaperExecutor::new();
            let mut manager = OrderManager::new(executor, quoter, risk_manager, config);

            info!("starting paper trading loop — press Ctrl+C to stop");
            let snapshots = feed.stream().await
                .context("failed to start market data feed")?;

            manager.run_paper(snapshots).await;
        }
        Mode::Live => {
            // Live execution is not yet implemented — fail gracefully
            anyhow::bail!(
                "live mode is not yet implemented; use --mode paper for now"
            );
        }
    }

    info!("eutrader shut down cleanly");
    Ok(())
}

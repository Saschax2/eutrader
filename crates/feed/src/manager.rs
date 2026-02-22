use eutrader_core::MarketSnapshot;
use futures::stream::{self, Stream};
use std::pin::Pin;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::book::{self, BookClient};

/// Default polling interval in milliseconds.
const DEFAULT_INTERVAL_MS: u64 = 1000;

/// Manages periodic polling of orderbooks and produces a stream of `MarketSnapshot`s.
pub struct FeedManager {
    token_ids: Vec<String>,
    interval: Duration,
}

impl FeedManager {
    /// Create a new `FeedManager` with the default polling interval (1000 ms).
    ///
    /// * `token_ids` -- the CLOB token IDs to poll.
    pub fn new(token_ids: Vec<String>) -> Self {
        Self {
            token_ids,
            interval: Duration::from_millis(DEFAULT_INTERVAL_MS),
        }
    }

    /// Create a new `FeedManager` with a custom polling interval.
    ///
    /// * `token_ids` -- the CLOB token IDs to poll.
    /// * `interval_ms` -- polling interval in milliseconds.
    pub fn with_interval(token_ids: Vec<String>, interval_ms: u64) -> Self {
        Self {
            token_ids,
            interval: Duration::from_millis(interval_ms),
        }
    }

    /// Start polling and return a `Stream` of `MarketSnapshot`s.
    ///
    /// Internally spawns a tokio task that polls each token's orderbook on a
    /// fixed interval and pushes snapshots through a broadcast channel. The
    /// returned stream will receive all snapshots.
    pub async fn stream(
        self,
    ) -> eutrader_core::Result<Pin<Box<dyn Stream<Item = MarketSnapshot> + Send>>> {
        let (tx, rx) = broadcast::channel::<MarketSnapshot>(256);
        let token_ids = self.token_ids.clone();
        let interval = self.interval;

        tokio::spawn(async move {
            let client = BookClient::new();
            let mut ticker = tokio::time::interval(interval);

            info!(
                tokens = token_ids.len(),
                interval_ms = interval.as_millis() as u64,
                "feed manager started"
            );

            loop {
                ticker.tick().await;

                for token_id in &token_ids {
                    match client.get_orderbook(token_id).await {
                        Ok(book_resp) => {
                            if let Some(snapshot) = book::to_snapshot(token_id, &book_resp) {
                                if tx.send(snapshot).is_err() {
                                    // All receivers dropped -- stop the loop
                                    info!("all feed receivers dropped, stopping feed manager");
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            warn!(token_id, error = %e, "failed to fetch orderbook");
                        }
                    }
                }
            }
        });

        // Convert the broadcast receiver into a Stream
        let stream = stream::unfold(rx, |mut rx| async move {
            loop {
                match rx.recv().await {
                    Ok(snapshot) => return Some((snapshot, rx)),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(skipped = n, "feed consumer lagged, skipping messages");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        });

        Ok(Box::pin(stream))
    }

    /// Start polling and return a `Stream` of `MarketSnapshot`s (infallible variant).
    ///
    /// Same as `stream()` but does not return a `Result` — use when you don't need
    /// startup validation.
    pub fn run(self) -> Pin<Box<dyn Stream<Item = MarketSnapshot> + Send>> {
        let (tx, rx) = broadcast::channel::<MarketSnapshot>(256);
        let token_ids = self.token_ids.clone();
        let interval = self.interval;

        tokio::spawn(async move {
            let client = BookClient::new();
            let mut ticker = tokio::time::interval(interval);

            info!(
                tokens = token_ids.len(),
                interval_ms = interval.as_millis() as u64,
                "feed manager started"
            );

            loop {
                ticker.tick().await;

                for token_id in &token_ids {
                    match client.get_orderbook(token_id).await {
                        Ok(book_resp) => {
                            if let Some(snapshot) = book::to_snapshot(token_id, &book_resp) {
                                if tx.send(snapshot).is_err() {
                                    info!("all feed receivers dropped, stopping feed manager");
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            warn!(token_id, error = %e, "failed to fetch orderbook");
                        }
                    }
                }
            }
        });

        let stream = stream::unfold(rx, |mut rx| async move {
            loop {
                match rx.recv().await {
                    Ok(snapshot) => return Some((snapshot, rx)),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(skipped = n, "feed consumer lagged, skipping messages");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        });

        Box::pin(stream)
    }
}

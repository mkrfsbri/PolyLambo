use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde::Deserialize;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio_retry::Retry;
use tokio_retry::strategy::ExponentialBackoff;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::state::{AppState, atomic_to_f64, f64_to_atomic, trend};

const WS_URL: &str = "wss://stream.binance.com:9443/ws/btcusdt@aggTrade";

#[derive(Debug, Deserialize)]
struct AggTrade {
    /// price as string
    #[serde(rename = "p")]
    price: String,
    /// trade time unix ms
    #[serde(rename = "T")]
    trade_time: u64,
}

/// Entry point — retries with exponential backoff (base 1s, max 30s, 10 attempts).
/// Spawned as a long-running tokio task.
pub async fn run_btc_feed(state: Arc<AppState>) -> Result<()> {
    let mut attempt = 0u32;

    Retry::spawn(
        ExponentialBackoff::from_millis(1000)
            .max_delay(Duration::from_secs(30))
            .take(10),
        || {
            attempt += 1;
            let state = state.clone();
            let n = attempt;
            async move { connect_and_stream(state, n).await }
        },
    )
    .await
}

async fn connect_and_stream(state: Arc<AppState>, attempt: u32) -> Result<()> {
    let (mut ws, _) = connect_async(WS_URL)
        .await
        .context("binance ws connect")?;

    let current_price = state.btc.price();
    if attempt == 1 {
        tracing::info!("[BINANCE] Connected | Price: ${current_price:.2}");
    } else {
        tracing::info!(
            "[BINANCE] Connected | Price: ${current_price:.2} | Reconnect attempt #{attempt}"
        );
    }

    while let Some(msg) = ws.next().await {
        match msg.context("binance ws read")? {
            Message::Text(text) => {
                let trade: AggTrade = match serde_json::from_str(&text) {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                update_feed(&state, &trade);
            }
            Message::Close(_) => {
                tracing::warn!("[BINANCE] Server closed connection");
                break;
            }
            // tokio-tungstenite auto-responds to Ping; ignore other frame types
            _ => {}
        }
    }

    anyhow::bail!("binance ws stream ended")
}

#[inline]
fn update_feed(state: &AppState, trade: &AggTrade) {
    let price: f64 = match trade.price.parse() {
        Ok(p) if p > 0.0 => p,
        _ => return,
    };

    let prev_raw = state.btc.price_raw.load(Ordering::Acquire);
    let prev_price = atomic_to_f64(prev_raw);

    let new_trend = if prev_price > 0.0 {
        let ratio = price / prev_price;
        if ratio > 1.000_01 {
            trend::BULL
        } else if ratio < 0.999_99 {
            trend::BEAR
        } else {
            trend::NEUTRAL
        }
    } else {
        trend::NEUTRAL
    };

    state.btc.price_prev.store(prev_raw, Ordering::Release);
    state.btc.price_raw.store(f64_to_atomic(price), Ordering::Release);
    state.btc.last_update_ms.store(trade.trade_time, Ordering::Release);
    state.btc.trend.store(new_trend, Ordering::Release);

    tracing::trace!(
        "[BINANCE] Price: ${price:.2} | Trend: {}",
        match new_trend {
            trend::BULL => "BULL",
            trend::BEAR => "BEAR",
            _ => "NEUTRAL",
        }
    );
}

/// Read current BTC price from shared state (lock-free Acquire).
pub fn get_btc_price(state: &AppState) -> f64 {
    atomic_to_f64(state.btc.price_raw.load(Ordering::Acquire))
}

// ── tests ──��─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::trend;
    use std::sync::atomic::Ordering;

    fn make_trade(price: &str, ts: u64) -> AggTrade {
        AggTrade { price: price.to_string(), trade_time: ts }
    }

    #[test]
    fn test_update_feed_sets_price() {
        let state = AppState::new();
        let trade = make_trade("65000.50", 1_700_000_000_000);
        update_feed(&state, &trade);
        let stored = get_btc_price(&state);
        assert!((stored - 65_000.50).abs() < 0.01);
    }

    #[test]
    fn test_update_feed_bull_trend() {
        let state = AppState::new();
        // prime with a base price
        update_feed(&state, &make_trade("65000.00", 1));
        // price up > 0.001%
        update_feed(&state, &make_trade("65010.00", 2));
        assert_eq!(state.btc.trend.load(Ordering::Acquire), trend::BULL);
    }

    #[test]
    fn test_update_feed_bear_trend() {
        let state = AppState::new();
        update_feed(&state, &make_trade("65000.00", 1));
        update_feed(&state, &make_trade("64990.00", 2));
        assert_eq!(state.btc.trend.load(Ordering::Acquire), trend::BEAR);
    }

    #[test]
    fn test_update_feed_neutral_on_tiny_move() {
        let state = AppState::new();
        update_feed(&state, &make_trade("65000.00", 1));
        // < 0.001% move → neutral
        update_feed(&state, &make_trade("65000.05", 2));
        assert_eq!(state.btc.trend.load(Ordering::Acquire), trend::NEUTRAL);
    }

    #[test]
    fn test_update_feed_ignores_invalid_price() {
        let state = AppState::new();
        update_feed(&state, &make_trade("not_a_number", 1));
        assert_eq!(state.btc.price_raw.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_update_feed_updates_timestamp() {
        let state = AppState::new();
        update_feed(&state, &make_trade("65000.00", 1_700_000_000_123));
        assert_eq!(
            state.btc.last_update_ms.load(Ordering::Acquire),
            1_700_000_000_123
        );
    }

    #[test]
    fn test_prev_price_tracked() {
        let state = AppState::new();
        update_feed(&state, &make_trade("65000.00", 1));
        update_feed(&state, &make_trade("65500.00", 2));
        let prev = state.btc.prev_price();
        assert!((prev - 65_000.0).abs() < 0.01);
    }

    /// Live smoke-test: connect to Binance WS, wait up to 5s for a price tick.
    /// Skipped in CI (requires network). Run manually: cargo test binance_live -- --ignored
    #[tokio::test]
    #[ignore]
    async fn binance_live_price_updates() {
        let state = AppState::new();
        let state2 = state.clone();
        let feed = tokio::spawn(async move {
            let _ = run_btc_feed(state2).await;
        });
        tokio::time::sleep(Duration::from_secs(5)).await;
        let price = get_btc_price(&state);
        feed.abort();
        assert!(price > 1000.0, "expected live BTC price > $1000, got {price}");
    }
}

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use anyhow::{bail, Result};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::state::{AppState, atomic_to_f64, f64_to_atomic};

const CLOB_WS: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
const RTDS_WS: &str = "wss://ws-live-data.polymarket.com";

// ── local order book ──────────────────────────────────────────────────────────

/// Per-token local book.  BTreeMap keyed by price fixed-point so we get
/// best-bid (max) and best-ask (min) in O(log n) without sorting.
struct TokenBook {
    bids: BTreeMap<u64, u64>,
    asks: BTreeMap<u64, u64>,
}

impl TokenBook {
    fn new() -> Self {
        Self { bids: BTreeMap::new(), asks: BTreeMap::new() }
    }

    fn load_snapshot(&mut self, bids_val: &serde_json::Value, asks_val: &serde_json::Value) {
        self.bids.clear();
        self.asks.clear();
        Self::ingest_levels(&mut self.bids, bids_val);
        Self::ingest_levels(&mut self.asks, asks_val);
    }

    fn ingest_levels(map: &mut BTreeMap<u64, u64>, arr: &serde_json::Value) {
        let Some(levels) = arr.as_array() else { return };
        for lvl in levels {
            let Some(ps) = lvl["price"].as_str() else { continue };
            let Some(ss) = lvl["size"].as_str()  else { continue };
            let Ok(p)    = ps.parse::<f64>()     else { continue };
            let Ok(s)    = ss.parse::<f64>()     else { continue };
            let pk = f64_to_atomic(p);
            if s == 0.0 { map.remove(&pk); } else { map.insert(pk, f64_to_atomic(s)); }
        }
    }

    fn apply_change(&mut self, ch: &serde_json::Value) {
        let Some(ps)   = ch["price"].as_str() else { return };
        let Some(side) = ch["side"].as_str()  else { return };
        let Some(ss)   = ch["size"].as_str()  else { return };
        let Ok(p) = ps.parse::<f64>() else { return };
        let Ok(s) = ss.parse::<f64>() else { return };
        let pk = f64_to_atomic(p);
        let map = if side == "BUY" { &mut self.bids } else { &mut self.asks };
        if s == 0.0 { map.remove(&pk); } else { map.insert(pk, f64_to_atomic(s)); }
    }

    /// Best bid = highest bid price.
    fn best_bid(&self) -> Option<f64> {
        self.bids.keys().next_back().map(|&k| k as f64 / 1_000_000.0)
    }

    /// Best ask = lowest ask price.
    fn best_ask(&self) -> Option<f64> {
        self.asks.keys().next().map(|&k| k as f64 / 1_000_000.0)
    }
}

// ── CLOB WebSocket ────────────────────────────────────────────────────────────

/// Long-running task: subscribes to both UP and DOWN token books.
/// Reconnects automatically when the 5-minute window rotates.
pub async fn run_clob_ws(state: Arc<AppState>) {
    loop {
        let up_id   = state.up_token_id.read().await.clone();
        let down_id = state.down_token_id.read().await.clone();

        if up_id.is_empty() || down_id.is_empty() {
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }

        if let Err(e) = clob_session(&state, &up_id, &down_id).await {
            tracing::warn!("[CLOB_WS] session: {e:#}");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn clob_session(state: &Arc<AppState>, up_id: &str, down_id: &str) -> Result<()> {
    let (mut ws, _) = connect_async(CLOB_WS).await?;
    tracing::info!(
        "[CLOB_WS] connected up={}… dn={}…",
        &up_id[..up_id.len().min(8)],
        &down_id[..down_id.len().min(8)]
    );

    let sub = serde_json::json!({
        "auth": {},
        "type": "market",
        "markets": [up_id, down_id]
    });
    ws.send(Message::Text(sub.to_string())).await?;

    let mut up_book = TokenBook::new();
    let mut dn_book = TokenBook::new();

    while let Some(raw) = ws.next().await {
        // Exit and let the caller reconnect when the market window rotates.
        {
            let cur_up = state.up_token_id.read().await;
            let cur_dn = state.down_token_id.read().await;
            if *cur_up != up_id || *cur_dn != down_id {
                tracing::info!("[CLOB_WS] market rotated → reconnecting");
                return Ok(());
            }
        }

        match raw? {
            Message::Text(text) => {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
                on_clob_msg(state, &v, up_id, down_id, &mut up_book, &mut dn_book);
            }
            Message::Close(_) => break,
            Message::Ping(d)  => { let _ = ws.send(Message::Pong(d)).await; }
            _ => {}
        }
    }
    bail!("CLOB WS closed")
}

fn on_clob_msg(
    state: &AppState,
    v: &serde_json::Value,
    up_id: &str,
    down_id: &str,
    up_book: &mut TokenBook,
    dn_book: &mut TokenBook,
) {
    let event = v["event_type"].as_str().unwrap_or("");
    let asset = v["asset_id"].as_str().unwrap_or("");

    let is_up = asset == up_id;
    if !is_up && asset != down_id {
        return;
    }

    let book = if is_up { up_book } else { dn_book };

    match event {
        "book" => book.load_snapshot(&v["bids"], &v["asks"]),
        "price_change" => {
            if let Some(changes) = v["changes"].as_array() {
                for ch in changes {
                    book.apply_change(ch);
                }
            }
        }
        _ => return,
    }

    let bid = book.best_bid();
    let ask = book.best_ask();

    if is_up {
        if let Some(b) = bid { state.eth_up_bid.store(f64_to_atomic(b), Ordering::Release); }
        if let Some(a) = ask { state.eth_up_ask.store(f64_to_atomic(a), Ordering::Release); }
        if let (Some(b), Some(a)) = (bid, ask) {
            let mid = (b + a) / 2.0;
            let prev = state.eth_up_price.load(Ordering::Acquire);
            state.eth_up_prev.store(prev, Ordering::Release);
            state.eth_up_price.store(f64_to_atomic(mid), Ordering::Release);
        }
        tracing::trace!("[CLOB_WS] UP bid={bid:?} ask={ask:?}");
    } else {
        if let Some(b) = bid { state.eth_down_bid.store(f64_to_atomic(b), Ordering::Release); }
        if let Some(a) = ask { state.eth_down_ask.store(f64_to_atomic(a), Ordering::Release); }
        if let (Some(b), Some(a)) = (bid, ask) {
            let mid = (b + a) / 2.0;
            let prev = state.eth_down_price.load(Ordering::Acquire);
            state.eth_down_prev.store(prev, Ordering::Release);
            state.eth_down_price.store(f64_to_atomic(mid), Ordering::Release);
        }
        tracing::trace!("[CLOB_WS] DN bid={bid:?} ask={ask:?}");
    }
}

// ── RTDS WebSocket ────────────────────────────────────────────────────────────

/// Subscribes to Polymarket RTDS for crypto price updates.
/// When an ETH/USD price (>500) is received within the first 30s of a 5-min
/// window it is stored as eth_open_price ("price to beat").
pub async fn run_rtds_ws(state: Arc<AppState>) {
    loop {
        if let Err(e) = rtds_session(&state).await {
            tracing::warn!("[RTDS_WS] {e:#}");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn rtds_session(state: &Arc<AppState>) -> Result<()> {
    let (mut ws, _) = connect_async(RTDS_WS).await?;
    tracing::info!("[RTDS_WS] connected");

    // Subscribe to Chainlink ETH/USD — Polymarket's oracle for ETH resolution
    let sub = serde_json::json!({
        "action": "subscribe",
        "subscriptions": [{
            "topic": "crypto_prices_chainlink",
            "type": "*",
            "filters": "{\"symbol\":\"eth/usd\"}"
        }]
    });
    ws.send(Message::Text(sub.to_string())).await?;

    let mut ping_tick = tokio::time::interval(Duration::from_secs(5));
    ping_tick.tick().await; // consume immediate first tick

    loop {
        tokio::select! {
            _ = ping_tick.tick() => {
                ws.send(Message::Text("PING".to_string())).await?;
            }
            raw = ws.next() => {
                let Some(msg) = raw else { break };
                match msg? {
                    Message::Text(text) => {
                        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
                        on_rtds_msg(state, &v);
                    }
                    Message::Close(_) => break,
                    Message::Ping(d)  => { let _ = ws.send(Message::Pong(d)).await; }
                    _ => {}
                }
            }
        }
    }
    bail!("RTDS WS closed")
}

/// Processes a single RTDS message.
/// Expects Chainlink ETH/USD messages: {topic, payload: {symbol, value, timestamp}}.
fn on_rtds_msg(state: &AppState, v: &serde_json::Value) {
    let topic = v["topic"].as_str().unwrap_or("");
    if topic != "crypto_prices_chainlink" { return }

    let Some(p) = v["payload"]["value"].as_f64() else { return };
    if !(500.0..=10_000.0).contains(&p) { return }

    // Chainlink ETH/USD = Polymarket's live oracle price (shown as "POLY ETH" in TUI).
    // Note: this is the CURRENT price, not the window-open price-to-beat.
    // eth_open_price is set exclusively by run_open_price_boundary via the equity API.
    let prev = state.eth_poly_spot.load(Ordering::Acquire);
    state.eth_poly_spot_prev.store(prev, Ordering::Release);
    state.eth_poly_spot.store(f64_to_atomic(p), Ordering::Release);
    tracing::trace!("[RTDS_WS] Chainlink ETH: ${p:.2}");
}

// ── Boundary timer ────────────────────────────────────────────────────────────

/// Fires at each 300-second UTC boundary and fetches the authoritative
/// price-to-beat from `GET /api/equity/price-to-beat/{slug}`.
///
/// Seeds eth_open_price immediately with Chainlink (or Binance) so the TUI
/// has a value from t=0, then retries the equity API up to 6 times (every 3 s)
/// until Polymarket's backend has the exact window-open price available.
/// discover_and_update also tries the API on slug change, so any success from
/// either source wins.
pub async fn run_open_price_boundary(state: Arc<AppState>) {
    let client = reqwest::Client::new();
    loop {
        let ts = chrono::Utc::now().timestamp() as u64;
        let next_boundary = ((ts / 300) + 1) * 300;
        tokio::time::sleep(Duration::from_secs(next_boundary - ts)).await;

        let slug = crate::gamma::compute_current_slug();

        // Immediate seed: Chainlink live price (or Binance fallback).
        // This gives the TUI a value instantly; the API loop below overwrites it.
        let poly_price = atomic_to_f64(state.eth_poly_spot.load(Ordering::Acquire));
        let seed = if poly_price > 0.0 {
            poly_price
        } else {
            atomic_to_f64(state.eth_spot_raw.load(Ordering::Acquire))
        };
        if seed > 0.0 {
            state.eth_open_price.store(f64_to_atomic(seed), Ordering::Release);
            tracing::info!("[BOUNDARY] price-to-beat seed: ${seed:.2} (Chainlink/Binance)");
        }

        // Retry equity API until Polymarket has the exact window-open price ready.
        for attempt in 1u32..=6 {
            tokio::time::sleep(Duration::from_secs(3)).await;
            match crate::gamma::fetch_price_to_beat(&slug, &client).await {
                Ok(p) => {
                    state.eth_open_price.store(f64_to_atomic(p), Ordering::Release);
                    tracing::info!(
                        "[BOUNDARY] price-to-beat: ${p:.2} (equity API, attempt {attempt})"
                    );
                    break;
                }
                Err(e) => {
                    tracing::debug!("[BOUNDARY] price-to-beat attempt {attempt}/6: {e:#}");
                }
            }
        }
    }
}

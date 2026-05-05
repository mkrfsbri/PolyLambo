use anyhow::{bail, Context, Result};
use chrono::Utc;
use reqwest::Client;
use serde::{Deserialize, Deserializer};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::state::{AppState, f64_to_atomic};

const GAMMA_BASE: &str = "https://gamma-api.polymarket.com";
const POLY_BASE:  &str = "https://polymarket.com";
const POLL_INTERVAL_SECS: u64 = 10;
const WINDOW_SECS: u64 = 300;

// ── slug algorithm ────────────────────────────────────────────────────────────

/// Compute the slug for the currently active 5-minute window.
///
/// Polymarket slugs are named by the window OPEN time (floor of 300s boundary),
/// e.g. "eth-updown-5m-1746000000" opens at T=1746000000 and resolves at T+300.
pub fn compute_current_slug() -> String {
    let ts = Utc::now().timestamp() as u64;
    slug_for_ts(ts)
}

/// Pure function — compute slug for an arbitrary unix timestamp.
/// Separated for deterministic unit-testing.
pub fn slug_for_ts(ts: u64) -> String {
    let open = (ts / WINDOW_SECS) * WINDOW_SECS;
    format!("eth-updown-5m-{open}")
}

/// Seconds remaining until the next 300s boundary from an arbitrary timestamp.
pub fn expiry_secs_for_ts(ts: u64) -> u64 {
    let next = ((ts / WINDOW_SECS) + 1) * WINDOW_SECS;
    next - ts
}

// ── API types ─────────────────────────────────────────────────────────────────

/// Deserialize a JSON field that may be either a native array `["a","b"]`
/// or a JSON-encoded string `"[\"a\",\"b\"]"` — both forms appear in Gamma API responses.
fn deserialize_string_array<'de, D>(de: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error as _;
    let v = serde_json::Value::deserialize(de)?;
    match v {
        serde_json::Value::Array(arr) => arr
            .into_iter()
            .map(|x| match x {
                serde_json::Value::String(s) => Ok(s),
                other => Ok(other.to_string()),
            })
            .collect(),
        serde_json::Value::String(s) => serde_json::from_str::<Vec<String>>(&s)
            .map_err(|e| D::Error::custom(format!("string array parse: {e}"))),
        _ => Ok(vec![]),
    }
}

#[derive(Debug, Deserialize)]
struct GammaEvent {
    #[allow(dead_code)]
    slug: String,
    markets: Vec<GammaMarket>,
    /// Event-level description — may contain the opening ETH reference price.
    #[serde(default)]
    description: String,
}

#[derive(Debug, Deserialize)]
struct GammaMarket {
    #[serde(rename = "clobTokenIds", deserialize_with = "deserialize_string_array")]
    clob_token_ids: Vec<String>,
    question: String,
    /// Up price at index 0, Down price at index 1 (0.0–1.0)
    #[serde(rename = "outcomePrices", default, deserialize_with = "deserialize_string_array")]
    outcome_prices: Vec<String>,
    #[allow(dead_code)]
    #[serde(rename = "endDate")]
    end_date: String,
    /// Market-level description — may also contain the opening reference price.
    #[serde(default)]
    description: String,
}

// ── reference price parser ────────────────────────────────────────────────────

/// Extract the first dollar amount from a text string.
/// Matches patterns like "$3,245.12", "$3245.12", "$3,245".
/// Returns None if no parseable price is found or it is <= 0.
fn parse_first_dollar_amount(text: &str) -> Option<f64> {
    let start = text.find('$')?;
    let rest = &text[start + 1..];
    let price_str: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '.')
        .filter(|c| *c != ',')
        .collect();
    price_str.parse::<f64>().ok().filter(|&p| p > 0.0)
}

// ── HTTP fetch ────────────────────────────────────────────────────────────────

pub struct MarketTokens {
    pub up_token_id: String,
    pub down_token_id: String,
    /// Current Up outcome price (0.0–1.0). None if not available.
    pub up_price: Option<f64>,
    /// Current Down outcome price (0.0–1.0). None if not available.
    pub down_price: Option<f64>,
    /// Opening ETH/USD reference price from Polymarket ("price to beat"). None if not in description.
    pub reference_price: Option<f64>,
    /// Human-readable question text, e.g. "Ethereum Up or Down - May 2, 9:05AM ET"
    pub question: String,
}

/// Fetch token IDs (and current prices) for the given slug.
///
/// The event has one market: "Ethereum Up or Down - ...". Its `clobTokenIds[0]`
/// is the Up outcome token, `clobTokenIds[1]` is the Down outcome token.
pub async fn fetch_market_tokens(slug: &str, client: &Client) -> Result<MarketTokens> {
    let url = format!("{GAMMA_BASE}/events?slug={slug}");
    let events: Vec<GammaEvent> = client
        .get(&url)
        .send()
        .await
        .context("gamma fetch events")?
        .json()
        .await
        .context("gamma parse events")?;

    if events.is_empty() {
        bail!("gamma: slug not found: {slug}");
    }

    // Single market per event — question always contains "Up or Down"
    let market = events[0]
        .markets
        .iter()
        .find(|m| m.question.contains("Up"))
        .context("gamma: no market with 'Up' in question")?;

    if market.clob_token_ids.len() < 2 {
        bail!("gamma: expected 2 token ids, got {}", market.clob_token_ids.len());
    }

    let up_price = market.outcome_prices.first().and_then(|s| s.parse().ok());
    let down_price = market.outcome_prices.get(1).and_then(|s| s.parse().ok());

    // Try market description first, then event description for opening reference price
    let reference_price = parse_first_dollar_amount(&market.description)
        .or_else(|| parse_first_dollar_amount(&events[0].description));

    Ok(MarketTokens {
        up_token_id: market.clob_token_ids[0].clone(),
        down_token_id: market.clob_token_ids[1].clone(),
        up_price,
        down_price,
        reference_price,
        question: market.question.clone(),
    })
}

/// Fetch the authoritative "price to beat" from Polymarket's equity API.
///
/// This is the ETH/USD Chainlink price that was captured at the exact window-open
/// moment. It is what Polymarket uses for final resolution — not a live price.
/// Returns an error when the slug is not yet active or the API is unreachable.
pub async fn fetch_price_to_beat(slug: &str, client: &Client) -> Result<f64> {
    let url = format!("{POLY_BASE}/api/equity/price-to-beat/{slug}");
    let v: serde_json::Value = client
        .get(&url)
        .send()
        .await
        .context("price-to-beat HTTP")?
        .json()
        .await
        .context("price-to-beat JSON")?;

    // Handle several plausible response shapes
    let price = v["price"].as_f64()
        .or_else(|| v["priceToBeat"].as_f64())
        .or_else(|| v["value"].as_f64())
        .or_else(|| v["data"].as_f64())
        .or_else(|| v.as_f64())
        .filter(|&p| (500.0..=100_000.0).contains(&p));

    price.ok_or_else(|| anyhow::anyhow!("price-to-beat: unexpected response body: {v}"))
}

// ── CLOB orderbook refresh ────────────────────────────────────────────────────

const CLOB_BASE: &str = "https://clob.polymarket.com";

#[derive(Deserialize)]
struct ClobBookLevel {
    price: String,
    #[allow(dead_code)]
    size: String,
}

#[derive(Deserialize)]
struct ClobBook {
    bids: Vec<ClobBookLevel>,
    asks: Vec<ClobBookLevel>,
}

/// Fetch best bid and best ask from the CLOB /book endpoint.
/// Bids are sorted descending (bids[0] = highest), asks ascending (asks[0] = lowest).
async fn fetch_clob_book_top(token_id: &str, client: &Client) -> (Option<f64>, Option<f64>) {
    let url = format!("{CLOB_BASE}/book?token_id={token_id}");
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(_) => return (None, None),
    };
    let book: ClobBook = match resp.json().await {
        Ok(b) => b,
        Err(_) => return (None, None),
    };
    // Compute best bid (max) and best ask (min) without relying on sort order.
    let best_bid = book.bids.iter()
        .filter_map(|l| l.price.parse::<f64>().ok())
        .reduce(f64::max);
    let best_ask = book.asks.iter()
        .filter_map(|l| l.price.parse::<f64>().ok())
        .reduce(f64::min);
    (best_bid, best_ask)
}

/// Fetch UP/DOWN orderbook top-of-book from the CLOB REST API and write to state.
/// Stores best bid, best ask, and mid price. Call every 1–2 s.
pub async fn refresh_prices_from_clob(state: &AppState, client: &Client) {
    let up_id   = state.up_token_id.read().await.clone();
    let down_id = state.down_token_id.read().await.clone();
    if up_id.is_empty() || down_id.is_empty() {
        return;
    }

    let (up_bid, up_ask) = fetch_clob_book_top(&up_id, client).await;
    if up_bid.is_some() || up_ask.is_some() {
        let mid = match (up_bid, up_ask) {
            (Some(b), Some(a)) => (b + a) / 2.0,
            (Some(b), None)    => b,
            (None,    Some(a)) => a,
            _                  => unreachable!(),
        };
        let prev = state.eth_up_price.load(Ordering::Acquire);
        state.eth_up_prev.store(prev, Ordering::Release);
        state.eth_up_price.store(f64_to_atomic(mid), Ordering::Release);
        if let Some(b) = up_bid  { state.eth_up_bid.store(f64_to_atomic(b), Ordering::Release); }
        if let Some(a) = up_ask  { state.eth_up_ask.store(f64_to_atomic(a), Ordering::Release); }
        tracing::trace!("[GAMMA] CLOB UP  bid={up_bid:?} ask={up_ask:?} mid={mid:.4}");
    }

    let (dn_bid, dn_ask) = fetch_clob_book_top(&down_id, client).await;
    if dn_bid.is_some() || dn_ask.is_some() {
        let mid = match (dn_bid, dn_ask) {
            (Some(b), Some(a)) => (b + a) / 2.0,
            (Some(b), None)    => b,
            (None,    Some(a)) => a,
            _                  => unreachable!(),
        };
        let prev = state.eth_down_price.load(Ordering::Acquire);
        state.eth_down_prev.store(prev, Ordering::Release);
        state.eth_down_price.store(f64_to_atomic(mid), Ordering::Release);
        if let Some(b) = dn_bid  { state.eth_down_bid.store(f64_to_atomic(b), Ordering::Release); }
        if let Some(a) = dn_ask  { state.eth_down_ask.store(f64_to_atomic(a), Ordering::Release); }
        tracing::trace!("[GAMMA] CLOB DOWN bid={dn_bid:?} ask={dn_ask:?} mid={mid:.4}");
    }
}

// ── discovery loop ────────────────────────────────────────────────────────────

/// Apply UP/DOWN token prices to shared state.
fn apply_poly_prices(state: &AppState, tokens: &MarketTokens) {
    if let Some(p) = tokens.up_price {
        let prev = state.eth_up_price.load(Ordering::Acquire);
        state.eth_up_prev.store(prev, Ordering::Release);
        state.eth_up_price.store(f64_to_atomic(p), Ordering::Release);
    }
    if let Some(p) = tokens.down_price {
        let prev = state.eth_down_price.load(Ordering::Acquire);
        state.eth_down_prev.store(prev, Ordering::Release);
        state.eth_down_price.store(f64_to_atomic(p), Ordering::Release);
    }
}

/// Try to fetch and store eth_open_price (price-to-beat) from the equity API.
/// Primary source: `GET /api/equity/price-to-beat/{slug}`.
/// Fallback: description-parsed reference_price from the Gamma market object.
/// Returns true when a price was successfully stored.
async fn try_set_open_price(
    state: &AppState,
    slug: &str,
    client: &Client,
    fallback: Option<f64>,
) -> bool {
    match fetch_price_to_beat(slug, client).await {
        Ok(p) => {
            state.eth_open_price.store(f64_to_atomic(p), Ordering::Release);
            tracing::info!("[GAMMA] price-to-beat: ${p:.2} (equity API)");
            return true;
        }
        Err(e) => tracing::debug!("[GAMMA] price-to-beat API: {e:#}"),
    }
    // Fallback: description-parsed price from Gamma market
    if let Some(p) = fallback.filter(|&p| p > 0.0) {
        state.eth_open_price.store(f64_to_atomic(p), Ordering::Release);
        tracing::info!("[GAMMA] price-to-beat: ${p:.2} (description fallback)");
        return true;
    }
    false
}

/// Long-running task: polls every 10s.
///
/// - Updates slug + token IDs + question only when the window changes.
/// - On window change, fetches price-to-beat from `/api/equity/price-to-beat/{slug}`.
/// - Always refreshes UP/DOWN token prices so the TUI stays live.
/// - When the window changes but the new slug isn't live yet, refreshes prices
///   from the old slug so POLY prices don't go stale during the transition.
pub async fn discover_and_update(state: Arc<AppState>, client: Client) {
    loop {
        let slug = compute_current_slug();
        let ts = Utc::now().timestamp() as u64;
        let current = state.current_slug.read().await.clone();
        let slug_changed = slug != current;

        if slug_changed {
            // New window: try the upcoming slug, fall back to old slug for prices.
            match fetch_market_tokens(&slug, &client).await {
                Ok(tokens) => {
                    tracing::info!("[GAMMA] New window: {slug} | q: {}", tokens.question);
                    *state.current_slug.write().await = slug.clone();
                    *state.up_token_id.write().await = tokens.up_token_id.clone();
                    *state.down_token_id.write().await = tokens.down_token_id.clone();
                    *state.current_question.write().await = tokens.question.clone();

                    // Authoritative price-to-beat for this window.
                    // The boundary timer may have already seeded a Chainlink/Binance
                    // approximation; overwrite it with the exact API value.
                    try_set_open_price(&state, &slug, &client, tokens.reference_price).await;

                    apply_poly_prices(&state, &tokens);
                }
                Err(e) => {
                    tracing::warn!("[GAMMA] New slug {slug} not yet available: {e:#}");
                    // Keep POLY prices fresh from the still-active old market.
                    if !current.is_empty() {
                        match fetch_market_tokens(&current, &client).await {
                            Ok(tokens) => apply_poly_prices(&state, &tokens),
                            Err(e2) => tracing::warn!("[GAMMA] Old slug price refresh: {e2:#}"),
                        }
                    }
                }
            }
        } else if !current.is_empty() {
            match fetch_market_tokens(&current, &client).await {
                Ok(tokens) => {
                    // Retry price-to-beat if the boundary timer hasn't set it yet
                    // (API may not respond immediately at t=0 of a new window).
                    if state.eth_open_price.load(Ordering::Acquire) == 0 {
                        try_set_open_price(&state, &current, &client, tokens.reference_price).await;
                    }
                    apply_poly_prices(&state, &tokens);
                }
                Err(e) => tracing::warn!("[GAMMA] Price refresh failed: {e:#}"),
            }
        }

        // Always refresh expiry countdown.
        state.time_to_expiry_secs.store(expiry_secs_for_ts(ts), Ordering::Release);

        tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── slug generation ───────────────────────────────────────────────────────

    #[test]
    fn test_slug_generation_mid_window() {
        // 1746000150 is 150s into the window that OPENS at 1746000000
        let slug = slug_for_ts(1_746_000_150);
        assert_eq!(slug, "eth-updown-5m-1746000000");
    }

    #[test]
    fn test_slug_generation_near_boundary() {
        // 1746000299 is 1s before the next boundary, still in the 1746000000 window
        let slug = slug_for_ts(1_746_000_299);
        assert_eq!(slug, "eth-updown-5m-1746000000");
    }

    #[test]
    fn test_slug_boundary_edge() {
        // Exactly on a 300s boundary → the NEW window has just opened at that timestamp
        let slug = slug_for_ts(1_746_000_000);
        assert_eq!(slug, "eth-updown-5m-1746000000");
    }

    #[test]
    fn test_slug_boundary_plus_one() {
        // 1 second past a boundary → still in the window that opened at 1746000300
        let slug = slug_for_ts(1_746_000_301);
        assert_eq!(slug, "eth-updown-5m-1746000300");
    }

    #[test]
    fn test_slug_format() {
        let slug = slug_for_ts(1_000_000_000);
        assert!(slug.starts_with("eth-updown-5m-"));
        let suffix: u64 = slug.trim_start_matches("eth-updown-5m-").parse().unwrap();
        assert_eq!(suffix % 300, 0, "slug timestamp must be multiple of 300");
    }

    // ── expiry countdown ──────────────────────────────────────────────────────

    #[test]
    fn test_expiry_mid_window() {
        // 150s into window → 150s remaining
        assert_eq!(expiry_secs_for_ts(1_746_000_150), 150);
    }

    #[test]
    fn test_expiry_on_boundary() {
        // Exactly on boundary → full 300s to next
        assert_eq!(expiry_secs_for_ts(1_746_000_000), 300);
    }

    #[test]
    fn test_expiry_one_before_boundary() {
        assert_eq!(expiry_secs_for_ts(1_746_000_299), 1);
    }

    // ── token parsing (unit, no HTTP) ─────────────────────────────────────────

    #[test]
    fn test_parse_gamma_event_json() {
        // Matches real Gamma API response structure observed 2026-05-02
        let json = r#"[{
            "slug": "eth-updown-5m-1777727100",
            "markets": [
                {
                    "question": "Ethereum Up or Down - May 2, 9:05AM-9:10AM ET",
                    "clobTokenIds": ["94554453955679131155753198461295833893575139847238407557374141327659939814789",
                                     "66111132003643556944495002313248397782437884834651728460739474369989675475918"],
                    "outcomePrices": ["0.505", "0.495"],
                    "endDate": "2026-05-02T13:10:00Z"
                }
            ]
        }]"#;

        let events: Vec<GammaEvent> = serde_json::from_str(json).unwrap();
        assert_eq!(events.len(), 1);
        let market = events[0].markets.iter().find(|m| m.question.contains("Up")).unwrap();
        assert_eq!(market.clob_token_ids.len(), 2);
        let up_price: f64 = market.outcome_prices[0].parse().unwrap();
        let down_price: f64 = market.outcome_prices[1].parse().unwrap();
        assert!((up_price - 0.505).abs() < 1e-6);
        assert!((down_price - 0.495).abs() < 1e-6);
    }

    /// The live Gamma API sometimes returns clobTokenIds and outcomePrices as
    /// JSON-encoded strings rather than native arrays. Both forms must parse.
    #[test]
    fn test_parse_gamma_event_string_encoded_fields() {
        let json = r#"[{
            "slug": "eth-updown-5m-1777738500",
            "markets": [
                {
                    "question": "Ethereum Up or Down - May 3",
                    "clobTokenIds": "[\"66509974476329633817894519361708972512933473416410477343917590711622208409549\", \"98831085497971659267355113836123523466884781766668270733188350551666247502647\"]",
                    "outcomePrices": "[\"0.505\", \"0.495\"]",
                    "endDate": "2026-05-03T13:10:00Z"
                }
            ]
        }]"#;

        let events: Vec<GammaEvent> = serde_json::from_str(json).unwrap();
        let market = events[0].markets.iter().find(|m| m.question.contains("Up")).unwrap();
        assert_eq!(market.clob_token_ids.len(), 2);
        assert!(market.clob_token_ids[0].starts_with("665"));
        let up_price: f64 = market.outcome_prices[0].parse().unwrap();
        assert!((up_price - 0.505).abs() < 1e-6);
    }

    // ── reference price parser ────────────────────────────────────────────────

    #[test]
    fn test_parse_dollar_amount_with_commas() {
        assert!((parse_first_dollar_amount("Opening price: $3,245.12 ETH").unwrap() - 3245.12).abs() < 1e-6);
    }

    #[test]
    fn test_parse_dollar_amount_no_commas() {
        assert!((parse_first_dollar_amount("ref $1800.50").unwrap() - 1800.50).abs() < 1e-6);
    }

    #[test]
    fn test_parse_dollar_amount_none_when_absent() {
        assert!(parse_first_dollar_amount("no price here").is_none());
    }

    #[test]
    fn test_parse_dollar_amount_from_event_description() {
        let json = r#"[{
            "slug": "eth-updown-5m-1777727100",
            "description": "Will ETH close higher? Opening price: $3,245.12",
            "markets": [{
                "question": "Ethereum Up or Down - May 2, 9:05AM-9:10AM ET",
                "clobTokenIds": ["aaa", "bbb"],
                "outcomePrices": ["0.505", "0.495"],
                "endDate": "2026-05-02T13:10:00Z"
            }]
        }]"#;
        let events: Vec<GammaEvent> = serde_json::from_str(json).unwrap();
        let ref_price = parse_first_dollar_amount(&events[0].description);
        assert!((ref_price.unwrap() - 3245.12).abs() < 1e-6);
    }

    #[test]
    fn test_parse_gamma_event_missing_up_market() {
        let json = r#"[{
            "slug": "eth-updown-5m-1746000300",
            "markets": [
                {
                    "question": "Some other market",
                    "clobTokenIds": ["0xA", "0xB"],
                    "endDate": "2025-04-30T00:05:00Z"
                }
            ]
        }]"#;
        let events: Vec<GammaEvent> = serde_json::from_str(json).unwrap();
        let up_market = events[0].markets.iter().find(|m| m.question.contains("Up"));
        assert!(up_market.is_none());
    }
}

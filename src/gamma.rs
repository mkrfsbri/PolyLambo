use anyhow::{bail, Context, Result};
use chrono::Utc;
use reqwest::Client;
use serde::{Deserialize, Deserializer};
use std::sync::Arc;
use std::time::Duration;

use crate::state::AppState;

const GAMMA_BASE: &str = "https://gamma-api.polymarket.com";
const POLL_INTERVAL_SECS: u64 = 10;
const WINDOW_SECS: u64 = 300;

// ── slug algorithm ────────────────────────────────────────────────────────────

/// Compute the next 5-minute window slug.
///
/// Always rounds to the STRICTLY NEXT multiple of 300s — even when `now` is
/// exactly on a boundary (market has just expired → predict the next one).
pub fn compute_next_slug() -> String {
    let ts = Utc::now().timestamp() as u64;
    slug_for_ts(ts)
}

/// Pure function — compute slug for an arbitrary unix timestamp.
/// Separated for deterministic unit-testing.
pub fn slug_for_ts(ts: u64) -> String {
    let next = ((ts / WINDOW_SECS) + 1) * WINDOW_SECS;
    format!("eth-updown-5m-{next}")
}

/// Seconds remaining until the next 300s boundary from an arbitrary timestamp.
pub fn expiry_secs_for_ts(ts: u64) -> u64 {
    let next = ((ts / WINDOW_SECS) + 1) * WINDOW_SECS;
    next - ts
}

// ── API types ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GammaEvent {
    #[allow(dead_code)]
    slug: String,
    markets: Vec<GammaMarket>,
}

/// The Gamma API returns outcomePrices either as a native JSON array
/// `["0.505","0.495"]` or as a JSON-encoded string `"[\"0.505\",\"0.495\"]"`.
/// This deserializer handles both forms.
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
}

// ── HTTP fetch ────────────────────────────────────────────────────────────────

pub struct MarketTokens {
    pub up_token_id: String,
    pub down_token_id: String,
    /// Current Up outcome price (0.0–1.0). None if not available.
    pub up_price: Option<f64>,
    /// Current Down outcome price (0.0–1.0). None if not available.
    pub down_price: Option<f64>,
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

    Ok(MarketTokens {
        up_token_id: market.clob_token_ids[0].clone(),
        down_token_id: market.clob_token_ids[1].clone(),
        up_price,
        down_price,
    })
}

// ── discovery loop ────────────────────────────────────────────────────────────

/// Long-running task: polls every 10s, updates AppState when the slug changes.
pub async fn discover_and_update(state: Arc<AppState>, client: Client) {
    loop {
        let slug = compute_next_slug();
        let ts = Utc::now().timestamp() as u64;

        // Only re-fetch when the slug has changed
        let current = state.current_slug.read().await.clone();
        if slug != current {
            match fetch_market_tokens(&slug, &client).await {
                Ok(tokens) => {
                    tracing::info!(
                        "[GAMMA] Slug: {slug} | UP: {} | DOWN: {}",
                        tokens.up_token_id,
                        tokens.down_token_id,
                    );
                    *state.current_slug.write().await = slug;
                    *state.up_token_id.write().await = tokens.up_token_id;
                    *state.down_token_id.write().await = tokens.down_token_id;
                    if let Some(p) = tokens.up_price {
                        state.eth_up_price.store(
                            crate::state::f64_to_atomic(p),
                            std::sync::atomic::Ordering::Release,
                        );
                    }
                    if let Some(p) = tokens.down_price {
                        state.eth_down_price.store(
                            crate::state::f64_to_atomic(p),
                            std::sync::atomic::Ordering::Release,
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!("[GAMMA] Slug {slug} not yet available: {e:#}");
                }
            }
        }

        // Always refresh expiry countdown
        let expiry = expiry_secs_for_ts(ts);
        state
            .time_to_expiry_secs
            .store(expiry, std::sync::atomic::Ordering::Release);

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
        // 1746000150 is 150s into a window (1746000000..1746000300)
        let slug = slug_for_ts(1_746_000_150);
        assert_eq!(slug, "eth-updown-5m-1746000300");
    }

    #[test]
    fn test_slug_generation_near_boundary() {
        // 1746000299 is 1s before boundary
        let slug = slug_for_ts(1_746_000_299);
        assert_eq!(slug, "eth-updown-5m-1746000300");
    }

    #[test]
    fn test_slug_boundary_edge() {
        // Exactly on a 300s boundary → must predict the NEXT window, not current
        let slug = slug_for_ts(1_746_000_000);
        assert_eq!(slug, "eth-updown-5m-1746000300");
    }

    #[test]
    fn test_slug_boundary_plus_one() {
        // 1 second past a boundary → next boundary is 299s away
        let slug = slug_for_ts(1_746_000_301);
        assert_eq!(slug, "eth-updown-5m-1746000600");
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

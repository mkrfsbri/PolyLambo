use anyhow::Result;
use dotenvy::dotenv;
use std::env;

pub struct Config {
    pub private_key: String,
    pub clob_api_key: String,
    pub clob_secret: String,
    pub clob_passphrase: String,
    pub polymarket_proxy_address: String,
    pub log_level: String,
    pub reversal_threshold_pct: f64,
    pub momentum_window_secs: u64,
    pub order_ttl_secs: u64,
    pub kelly_fraction: f64,
    pub dry_run: bool,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let _ = dotenv();
        Ok(Config {
            private_key: env::var("PRIVATE_KEY").unwrap_or_default(),
            clob_api_key: env::var("CLOB_API_KEY").unwrap_or_default(),
            clob_secret: env::var("CLOB_SECRET").unwrap_or_default(),
            clob_passphrase: env::var("CLOB_PASSPHRASE").unwrap_or_default(),
            polymarket_proxy_address: env::var("POLYMARKET_PROXY_ADDRESS").unwrap_or_default(),
            log_level: env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string()),
            reversal_threshold_pct: env::var("REVERSAL_THRESHOLD_PCT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.08),
            momentum_window_secs: env::var("MOMENTUM_WINDOW_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(15),
            order_ttl_secs: env::var("ORDER_TTL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3),
            kelly_fraction: env::var("KELLY_FRACTION")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.5),
            dry_run: env::var("DRY_RUN")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
        })
    }
}

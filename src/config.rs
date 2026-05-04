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
    pub alpha: f64,
    pub beta: f64,
    pub v_scale: f64,
    pub ptb_scale: f64,
    pub score_threshold: f64,
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
            alpha: env::var("ALPHA")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(0.6),
            beta: env::var("BETA")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(0.4),
            v_scale: env::var("V_SCALE")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(10.0),
            ptb_scale: env::var("PTB_SCALE")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(2.0),
            score_threshold: env::var("SCORE_THRESHOLD")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(0.15),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize env-mutation tests to avoid race conditions
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_signal_param_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("ALPHA");
        std::env::remove_var("BETA");
        std::env::remove_var("V_SCALE");
        std::env::remove_var("PTB_SCALE");
        std::env::remove_var("SCORE_THRESHOLD");
        let cfg = Config::from_env().unwrap();
        assert!((cfg.alpha - 0.6).abs() < 1e-9);
        assert!((cfg.beta - 0.4).abs() < 1e-9);
        assert!((cfg.v_scale - 10.0).abs() < 1e-9);
        assert!((cfg.ptb_scale - 2.0).abs() < 1e-9);
        assert!((cfg.score_threshold - 0.15).abs() < 1e-9);
    }

    #[test]
    fn test_signal_params_from_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("ALPHA", "0.7");
        std::env::set_var("SCORE_THRESHOLD", "0.20");
        let cfg = Config::from_env().unwrap();
        assert!((cfg.alpha - 0.7).abs() < 1e-9);
        assert!((cfg.score_threshold - 0.20).abs() < 1e-9);
        std::env::remove_var("ALPHA");
        std::env::remove_var("SCORE_THRESHOLD");
    }
}

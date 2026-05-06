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
    pub signal_confirm_ticks: u8,
    pub take_profit_pct: f64,
    pub stop_loss_pct: f64,
    pub db_path: String,
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
            signal_confirm_ticks: env::var("SIGNAL_CONFIRM_TICKS")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(3),
            take_profit_pct: env::var("TAKE_PROFIT_PCT")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(8.0),
            stop_loss_pct: env::var("STOP_LOSS_PCT")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(5.0),
            db_path: env::var("DB_PATH")
                .unwrap_or_else(|_| "./trades.db".to_string()),
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

    #[test]
    fn test_new_param_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("SIGNAL_CONFIRM_TICKS");
        std::env::remove_var("TAKE_PROFIT_PCT");
        std::env::remove_var("STOP_LOSS_PCT");
        std::env::remove_var("DB_PATH");
        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.signal_confirm_ticks, 3);
        assert!((cfg.take_profit_pct - 8.0).abs() < 1e-9);
        assert!((cfg.stop_loss_pct - 5.0).abs() < 1e-9);
        assert_eq!(cfg.db_path, "./trades.db");
    }

    #[test]
    fn test_new_params_from_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("SIGNAL_CONFIRM_TICKS", "5");
        std::env::set_var("TAKE_PROFIT_PCT", "12.0");
        std::env::set_var("STOP_LOSS_PCT", "3.5");
        std::env::set_var("DB_PATH", "/tmp/test.db");
        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.signal_confirm_ticks, 5);
        assert!((cfg.take_profit_pct - 12.0).abs() < 1e-9);
        assert!((cfg.stop_loss_pct - 3.5).abs() < 1e-9);
        assert_eq!(cfg.db_path, "/tmp/test.db");
        std::env::remove_var("SIGNAL_CONFIRM_TICKS");
        std::env::remove_var("TAKE_PROFIT_PCT");
        std::env::remove_var("STOP_LOSS_PCT");
        std::env::remove_var("DB_PATH");
    }
}

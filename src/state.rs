use dashmap::DashMap;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::RwLock;

// ── fixed-point helpers ──────────────────────────────────────────────────────

pub fn f64_to_atomic(v: f64) -> u64 {
    (v * 1_000_000.0) as u64
}

pub fn atomic_to_f64(v: u64) -> f64 {
    v as f64 / 1_000_000.0
}

// ── BTC price feed ───────────────────────────────────────────────────────────

/// Trend encoding for AtomicU8: 0=neutral, 1=bull, 2=bear
pub mod trend {
    pub const NEUTRAL: u8 = 0;
    pub const BULL: u8 = 1;
    pub const BEAR: u8 = 2;
}

pub struct BtcFeed {
    /// current price * 1_000_000
    pub price_raw: AtomicU64,
    /// previous price * 1_000_000 (for diff %)
    pub price_prev: AtomicU64,
    /// unix ms of last update
    pub last_update_ms: AtomicU64,
    /// trend::NEUTRAL / BULL / BEAR
    pub trend: AtomicU8,
}

impl Default for BtcFeed {
    fn default() -> Self {
        Self::new()
    }
}

impl BtcFeed {
    pub fn new() -> Self {
        BtcFeed {
            price_raw: AtomicU64::new(0),
            price_prev: AtomicU64::new(0),
            last_update_ms: AtomicU64::new(0),
            trend: AtomicU8::new(trend::NEUTRAL),
        }
    }

    pub fn price(&self) -> f64 {
        atomic_to_f64(self.price_raw.load(Ordering::Acquire))
    }

    pub fn prev_price(&self) -> f64 {
        atomic_to_f64(self.price_prev.load(Ordering::Acquire))
    }
}

// ── order types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum OrderSide {
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OrderStatus {
    Pending,
    Filled,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct ActiveOrder {
    pub order_id: String,
    pub side: OrderSide,
    pub price: f64,
    pub quantity: f64,
    pub placed_at: Instant,
    pub status: OrderStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TradeStatus {
    Filled,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct TradeRecord {
    pub closed_at:   chrono::DateTime<chrono::Utc>,
    pub side:        OrderSide,
    pub entry_price: f64,
    pub qty:         f64,
    pub status:      TradeStatus,
}

// ── bot status constants ─────────────────────────────────────────────────────

pub mod bot_status {
    pub const HUNTING: u8 = 0;
    pub const POSITION: u8 = 1;
    pub const REVERSAL: u8 = 2;
    pub const EMERGENCY: u8 = 3;
}

// ── shared application state ─────────────────────────────────────────────────

pub struct AppState {
    pub btc: Arc<BtcFeed>,
    pub orders: Arc<DashMap<String, ActiveOrder>>,

    /// shares * 1000 fixed-point (signed: positive = long)
    pub inventory_up: AtomicI64,
    pub inventory_down: AtomicI64,

    /// USDC cents (signed)
    pub pnl_usdc: AtomicI64,
    /// USDC * 1_000_000 fixed-point
    pub balance_usdc: AtomicU64,

    /// current Polymarket slug, e.g. "eth-updown-5m-1746000300"
    pub current_slug: RwLock<String>,
    /// human-readable market question, e.g. "Ethereum Up or Down - May 2, 9:05AM ET"
    pub current_question: RwLock<String>,
    /// seconds until market window expires
    pub time_to_expiry_secs: AtomicU64,

    /// bot_status::{HUNTING, POSITION, REVERSAL, EMERGENCY}
    pub bot_status: AtomicU8,

    pub up_token_id: RwLock<String>,
    pub down_token_id: RwLock<String>,

    /// ETH Up outcome price * 1_000_000 (Polymarket token mid price 0–1)
    pub eth_up_price: AtomicU64,
    /// previous ETH Up price — used for ↑/↓ indicator in TUI
    pub eth_up_prev: AtomicU64,
    /// ETH Up best bid from CLOB orderbook * 1_000_000
    pub eth_up_bid: AtomicU64,
    /// ETH Up best ask from CLOB orderbook * 1_000_000
    pub eth_up_ask: AtomicU64,
    /// ETH Down outcome price * 1_000_000 (Polymarket token mid price 0–1)
    pub eth_down_price: AtomicU64,
    /// previous ETH Down price — used for ↑/↓ indicator in TUI
    pub eth_down_prev: AtomicU64,
    /// ETH Down best bid from CLOB orderbook * 1_000_000
    pub eth_down_bid: AtomicU64,
    /// ETH Down best ask from CLOB orderbook * 1_000_000
    pub eth_down_ask: AtomicU64,

    /// ETH/USD spot price from Binance * 1_000_000
    pub eth_spot_raw: AtomicU64,
    /// previous ETH/USD spot price * 1_000_000
    pub eth_spot_prev: AtomicU64,
    /// ETH/USD price at window open ("price to beat") * 1_000_000
    pub eth_open_price: AtomicU64,
    /// Polymarket live ETH/USD price from RTDS feed * 1_000_000
    pub eth_poly_spot: AtomicU64,
    /// previous Polymarket ETH/USD price * 1_000_000
    pub eth_poly_spot_prev: AtomicU64,

    /// last measured CLOB API round-trip ms
    pub api_latency_ms: AtomicU64,
    /// last measured Binance ws_recv→price_update μs
    pub ws_latency_us: AtomicU64,

    /// reversal deviation * 1_000_000 (0 = Normal, >0 = Warning/Emergency)
    pub reversal_deviation: AtomicU64,
    /// 1 = momentum currently decaying, 0 = OK
    pub momentum_decaying: AtomicU8,
    /// Composite signal score * 1_000_000 (signed; positive = Up signal, negative = Down)
    pub signal_score: AtomicI64,
    /// Completed trade records, newest first, capped at 50
    pub trade_history: Mutex<VecDeque<TradeRecord>>,
}

impl AppState {
    pub fn new() -> Arc<Self> {
        Arc::new(AppState {
            btc: Arc::new(BtcFeed::new()),
            orders: Arc::new(DashMap::new()),
            inventory_up: AtomicI64::new(0),
            inventory_down: AtomicI64::new(0),
            pnl_usdc: AtomicI64::new(0),
            balance_usdc: AtomicU64::new(0),
            current_slug: RwLock::new(String::new()),
            current_question: RwLock::new(String::new()),
            time_to_expiry_secs: AtomicU64::new(0),
            bot_status: AtomicU8::new(bot_status::HUNTING),
            up_token_id: RwLock::new(String::new()),
            down_token_id: RwLock::new(String::new()),
            eth_up_price: AtomicU64::new(0),
            eth_up_prev: AtomicU64::new(0),
            eth_up_bid: AtomicU64::new(0),
            eth_up_ask: AtomicU64::new(0),
            eth_down_price: AtomicU64::new(0),
            eth_down_prev: AtomicU64::new(0),
            eth_down_bid: AtomicU64::new(0),
            eth_down_ask: AtomicU64::new(0),
            eth_spot_raw: AtomicU64::new(0),
            eth_spot_prev: AtomicU64::new(0),
            eth_open_price: AtomicU64::new(0),
            eth_poly_spot: AtomicU64::new(0),
            eth_poly_spot_prev: AtomicU64::new(0),
            api_latency_ms: AtomicU64::new(0),
            ws_latency_us: AtomicU64::new(0),
            reversal_deviation: AtomicU64::new(0),
            momentum_decaying: AtomicU8::new(0),
            signal_score: AtomicI64::new(0),
            trade_history: Mutex::new(VecDeque::with_capacity(50)),
        })
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn test_fixed_point_roundtrip() {
        let cases = [0.0_f64, 1.0, 0.62, 65432.123456, 0.000001];
        for &v in &cases {
            let encoded = f64_to_atomic(v);
            let decoded = atomic_to_f64(encoded);
            assert!(
                (decoded - v).abs() < 1e-5,
                "roundtrip failed for {v}: got {decoded}"
            );
        }
    }

    #[test]
    fn test_btc_feed_price_store_load() {
        let feed = BtcFeed::new();
        let price = 65_000.50_f64;
        feed.price_raw.store(f64_to_atomic(price), Ordering::Release);
        let loaded = feed.price();
        assert!((loaded - price).abs() < 0.01, "price mismatch: {loaded}");
    }

    #[test]
    fn test_btc_feed_trend() {
        let feed = BtcFeed::new();
        assert_eq!(feed.trend.load(Ordering::Relaxed), trend::NEUTRAL);
        feed.trend.store(trend::BULL, Ordering::Release);
        assert_eq!(feed.trend.load(Ordering::Acquire), trend::BULL);
        feed.trend.store(trend::BEAR, Ordering::Release);
        assert_eq!(feed.trend.load(Ordering::Acquire), trend::BEAR);
    }

    #[test]
    fn test_appstate_new_defaults() {
        let state = AppState::new();
        assert_eq!(state.bot_status.load(Ordering::Relaxed), bot_status::HUNTING);
        assert_eq!(state.inventory_up.load(Ordering::Relaxed), 0);
        assert_eq!(state.pnl_usdc.load(Ordering::Relaxed), 0);
        assert_eq!(state.btc.price(), 0.0);
    }

    #[test]
    fn test_active_order_clone() {
        let order = ActiveOrder {
            order_id: "abc123".to_string(),
            side: OrderSide::Up,
            price: 0.62,
            quantity: 15.0,
            placed_at: Instant::now(),
            status: OrderStatus::Pending,
        };
        let cloned = order.clone();
        assert_eq!(cloned.order_id, "abc123");
        assert_eq!(cloned.side, OrderSide::Up);
        assert_eq!(cloned.status, OrderStatus::Pending);
    }

    #[test]
    fn test_trade_history_cap_at_50() {
        let state = AppState::new();
        for _ in 0..55 {
            let mut h = state.trade_history.lock().unwrap();
            h.push_front(TradeRecord {
                closed_at: chrono::Utc::now(),
                side: OrderSide::Up,
                entry_price: 0.52,
                qty: 10.0,
                status: TradeStatus::Filled,
            });
            h.truncate(50);
        }
        let h = state.trade_history.lock().unwrap();
        assert_eq!(h.len(), 50);
    }

    #[test]
    fn test_trade_history_newest_first() {
        let state = AppState::new();
        {
            let mut h = state.trade_history.lock().unwrap();
            h.push_front(TradeRecord {
                closed_at: chrono::Utc::now(),
                side: OrderSide::Up,
                entry_price: 0.50,
                qty: 5.0,
                status: TradeStatus::Cancelled,
            });
            h.push_front(TradeRecord {
                closed_at: chrono::Utc::now(),
                side: OrderSide::Down,
                entry_price: 0.48,
                qty: 8.0,
                status: TradeStatus::Filled,
            });
        }
        let h = state.trade_history.lock().unwrap();
        assert!((h[0].entry_price - 0.48).abs() < 1e-9, "newest first");
        assert!((h[1].entry_price - 0.50).abs() < 1e-9, "oldest second");
    }

    #[test]
    fn test_signal_score_atomic_roundtrip() {
        let state = AppState::new();
        let score: f64 = -0.372_111;
        state.signal_score.store((score * 1_000_000.0) as i64, Ordering::Release);
        let recovered = state.signal_score.load(Ordering::Acquire) as f64 / 1_000_000.0;
        assert!((recovered - score).abs() < 1e-5);
    }

    #[test]
    fn test_orders_dashmap() {
        let state = AppState::new();
        let order = ActiveOrder {
            order_id: "order-1".to_string(),
            side: OrderSide::Down,
            price: 0.38,
            quantity: 10.0,
            placed_at: Instant::now(),
            status: OrderStatus::Pending,
        };
        state.orders.insert(order.order_id.clone(), order);
        assert_eq!(state.orders.len(), 1);
        assert!(state.orders.contains_key("order-1"));
        state.orders.remove("order-1");
        assert!(state.orders.is_empty());
    }
}

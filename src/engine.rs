use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use crate::binance::get_btc_price;
use crate::config::Config;
use crate::state::{AppState, OrderSide, atomic_to_f64, bot_status, trend};

const LOOP_INTERVAL_MS: u64 = 250;
const EXPIRY_GUARD_SECS: u64 = 90;

// ── Direction ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Direction {
    Up,
    Down,
}

impl From<Direction> for OrderSide {
    fn from(d: Direction) -> OrderSide {
        match d {
            Direction::Up => OrderSide::Up,
            Direction::Down => OrderSide::Down,
        }
    }
}

// ── MomentumWindow ────────────────────────────────────────────────────────────

/// Rolling price window keyed on Binance trade timestamps (unix ms).
pub struct MomentumWindow {
    /// (unix_ms, price) — oldest first
    prices: VecDeque<(u64, f64)>,
    max_window_ms: u64,
}

impl MomentumWindow {
    pub fn new(max_window_secs: u64) -> Self {
        MomentumWindow {
            prices: VecDeque::with_capacity(200),
            max_window_ms: max_window_secs * 1000,
        }
    }

    pub fn push(&mut self, ts_ms: u64, price: f64) {
        self.prices.push_back((ts_ms, price));
        // drop entries outside the rolling window
        while let Some(&(t, _)) = self.prices.front() {
            if ts_ms.saturating_sub(t) > self.max_window_ms {
                self.prices.pop_front();
            } else {
                break;
            }
        }
    }

    /// Price change per second over the full window (signed).
    pub fn velocity(&self) -> f64 {
        if self.prices.len() < 2 {
            return 0.0;
        }
        let &(t0, p0) = self.prices.front().unwrap();
        let &(t1, p1) = self.prices.back().unwrap();
        let elapsed = (t1 as f64 - t0 as f64) / 1000.0;
        if elapsed < 0.001 {
            return 0.0;
        }
        (p1 - p0) / elapsed
    }

    /// True when the last 3 consecutive per-tick velocities are monotonically
    /// decreasing in absolute magnitude — momentum is losing steam.
    pub fn is_decaying(&self) -> bool {
        let n = self.prices.len();
        if n < 4 {
            return false;
        }
        // collect the 4 most-recent (oldest → newest)
        let tail: Vec<(u64, f64)> = self.prices.iter().rev().take(4).cloned().collect();
        // tail[0]=newest … tail[3]=4th-from-newest
        let vel = |a: &(u64, f64), b: &(u64, f64)| -> f64 {
            // a is newer, b is older
            let dt = (a.0 as f64 - b.0 as f64) / 1000.0;
            if dt < 0.001 {
                return 0.0;
            }
            ((a.1 - b.1) / dt).abs()
        };
        let v1 = vel(&tail[2], &tail[3]); // oldest pair
        let v2 = vel(&tail[1], &tail[2]);
        let v3 = vel(&tail[0], &tail[1]); // newest pair
        v3 < v2 && v2 < v1
    }
}

// ── EntryContext ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct EntryContext {
    pub entry_price: f64,
    pub entry_time: Instant,
    pub direction: Direction,
}

// ── ReversalStatus ────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
pub enum ReversalStatus {
    Normal,
    /// Deviation % that triggered the warning
    Warning(f64),
    EmergencyFlip,
}

// ── TradingEngine ─────────────────────────────────────────────────────────────

pub struct TradingEngine {
    pub state: Arc<AppState>,
    pub config: Arc<Config>,
    pub momentum: MomentumWindow,
    pub entry: Option<EntryContext>,
}

impl TradingEngine {
    pub fn new(state: Arc<AppState>, config: Arc<Config>) -> Self {
        let window = config.momentum_window_secs;
        TradingEngine {
            state,
            config,
            momentum: MomentumWindow::new(window),
            entry: None,
        }
    }

    /// Check whether current BTC price has moved adversely against our position.
    pub fn check_reversal(&self, current_btc: f64) -> ReversalStatus {
        let entry = match &self.entry {
            Some(e) => e,
            None => return ReversalStatus::Normal,
        };

        let deviation =
            (current_btc - entry.entry_price).abs() / entry.entry_price * 100.0;

        // Direction mismatch: BTC moved opposite to what we bet
        let btc_moved_up = current_btc > entry.entry_price;
        let mismatch = match entry.direction {
            Direction::Up => !btc_moved_up,
            Direction::Down => btc_moved_up,
        };

        let thresh = self.config.reversal_threshold_pct;
        if deviation >= thresh && mismatch {
            ReversalStatus::EmergencyFlip
        } else if deviation >= thresh * 0.6 {
            ReversalStatus::Warning(deviation)
        } else {
            ReversalStatus::Normal
        }
    }

    /// Push latest BTC price into momentum window and return a trading signal.
    pub fn compute_signal(&mut self, ts_ms: u64, btc_price: f64) -> Option<Direction> {
        self.momentum.push(ts_ms, btc_price);

        let expiry = self.state.time_to_expiry_secs.load(Ordering::Acquire);
        if expiry < EXPIRY_GUARD_SECS && self.momentum.is_decaying() {
            tracing::debug!("[ENGINE] Expiry guard: momentum decaying with {expiry}s left");
            return None;
        }

        match self.state.btc.trend.load(Ordering::Acquire) {
            t if t == trend::BULL => Some(Direction::Up),
            t if t == trend::BEAR => Some(Direction::Down),
            _ => None,
        }
    }

    /// Half-Kelly position size in USDC.
    /// `edge` = estimated win probability minus 0.5 (e.g. 0.05 = 55% win rate).
    pub fn half_kelly_size(&self, edge: f64, balance: f64) -> f64 {
        let fraction = (2.0 * edge) * self.config.kelly_fraction;
        fraction.max(0.0).min(0.1) * balance
    }
}

// ── engine loop ───────────────────────────────────────────────────────────────

pub async fn run_engine_loop(
    engine: Arc<Mutex<TradingEngine>>,
    clob: Arc<crate::clob::ClobClient>,
) {
    let mut ticker = tokio::time::interval(Duration::from_millis(LOOP_INTERVAL_MS));
    loop {
        ticker.tick().await;

        // ── read feed ──────────────────────────────────────────────────────
        let (btc_price, ts_ms, expiry) = {
            let eng = engine.lock().await;
            let price = get_btc_price(&eng.state);
            let ts    = eng.state.btc.last_update_ms.load(Ordering::Acquire);
            let exp   = eng.state.time_to_expiry_secs.load(Ordering::Acquire);
            (price, ts, exp)
        };

        if btc_price == 0.0 || ts_ms == 0 {
            continue; // BTC feed not yet connected
        }

        // ── reversal check + state broadcast ──────────────────────────────
        let reversal = { engine.lock().await.check_reversal(btc_price) };

        match &reversal {
            ReversalStatus::EmergencyFlip if expiry > EXPIRY_GUARD_SECS => {
                let state = engine.lock().await.state.clone();
                state.bot_status.store(bot_status::EMERGENCY, Ordering::Release);
                state.reversal_deviation.store(0, Ordering::Release);
                let _ = clob.cancel_all().await;
                tracing::warn!("[ENGINE] EMERGENCY FLIP — all orders cancelled");
                let mut eng = engine.lock().await;
                eng.entry = None;
                eng.state.bot_status.store(bot_status::HUNTING, Ordering::Release);
            }
            ReversalStatus::Warning(pct) => {
                tracing::warn!("[ENGINE] Reversal WARNING {pct:.4}% — tightening stop");
                let state = engine.lock().await.state.clone();
                state.bot_status.store(bot_status::REVERSAL, Ordering::Release);
                state.reversal_deviation.store(
                    crate::state::f64_to_atomic(*pct),
                    Ordering::Release,
                );
            }
            _ => {
                let state = engine.lock().await.state.clone();
                state.reversal_deviation.store(0, Ordering::Release);
                if state.bot_status.load(Ordering::Acquire) == bot_status::REVERSAL {
                    state.bot_status.store(bot_status::HUNTING, Ordering::Release);
                }
            }
        }

        // ── skip if already in a position or emergency ─────────────────────
        let status = engine.lock().await.state.bot_status.load(Ordering::Acquire);
        if matches!(status, s if s == bot_status::EMERGENCY || s == bot_status::POSITION) {
            continue;
        }

        // ── compute signal ─────────────────────────────────────────────────
        let direction = {
            let mut eng = engine.lock().await;
            match eng.compute_signal(ts_ms, btc_price) {
                Some(d) => d,
                None => continue,
            }
        };

        // ── size + token id ────────────────────────────────────────────────
        let (size, dry_run, ttl_secs, state_arc) = {
            let eng = engine.lock().await;
            let balance = atomic_to_f64(
                eng.state.balance_usdc.load(Ordering::Acquire) as u64
            );
            // Halt if balance fell below $1
            if balance < 1.0 {
                tracing::error!("[ENGINE] Balance ${balance:.2} < $1 — EMERGENCY halt");
                eng.state.bot_status.store(bot_status::EMERGENCY, Ordering::Release);
                let _ = clob.cancel_all().await;
                break;
            }
            let edge = 0.05_f64; // derive from spread in Phase 9+ refinement
            let sz   = eng.half_kelly_size(edge, balance);
            (sz, eng.config.dry_run, eng.config.order_ttl_secs, eng.state.clone())
        };

        if size < 1.0 {
            continue;
        }

        if dry_run {
            tracing::info!(
                "[ENGINE] DRY_RUN signal={:?} size=${size:.2} expiry={expiry}s",
                direction
            );
            // Update momentum_decaying for TUI even in dry_run
            let decaying = {
                let mut eng = engine.lock().await;
                eng.momentum.is_decaying()
            };
            state_arc.momentum_decaying.store(decaying as u8, Ordering::Release);
            continue;
        }

        // ── resolve token + price ──────────────────────────────────────────
        let (token_id, price) = {
            match direction {
                Direction::Up => (
                    state_arc.up_token_id.read().await.clone(),
                    atomic_to_f64(state_arc.eth_up_price.load(Ordering::Acquire)),
                ),
                Direction::Down => (
                    state_arc.down_token_id.read().await.clone(),
                    atomic_to_f64(state_arc.eth_down_price.load(Ordering::Acquire)),
                ),
            }
        };

        if token_id.is_empty() || price == 0.0 {
            tracing::debug!("[ENGINE] Skipping — market not yet discovered");
            continue;
        }

        // ── place order ────────────────────────────────────────────────────
        let t_signal = Instant::now();
        let order_side = OrderSide::from(direction.clone());
        let order_id = match clob.place_limit_order(&token_id, &order_side, price, size).await {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("[ENGINE] Order placement failed: {e:#}");
                continue;
            }
        };
        let lat_us = t_signal.elapsed().as_micros();
        state_arc.api_latency_ms.store((lat_us / 1000) as u64, Ordering::Release);
        if lat_us > 500 {
            tracing::warn!("[ENGINE] signal→order {}μs exceeds 500μs target", lat_us);
        }

        state_arc.bot_status.store(bot_status::POSITION, Ordering::Release);
        tracing::info!(
            "[ENGINE] POSITION {:?} | ${size:.2} | price={price:.4} | ttl={ttl_secs}s | lat={}μs",
            direction, lat_us
        );

        // ── watchdog ───────────────────────────────────────────────────────
        let initial_trend   = state_arc.btc.trend.load(Ordering::Acquire);
        let state_for_watch = state_arc.clone();
        let clob_w          = clob.clone();
        tokio::spawn(async move {
            order_watchdog(order_id, ttl_secs, initial_trend, state_for_watch, clob_w).await;
        });

        // Update momentum_decaying for TUI
        let decaying = { engine.lock().await.momentum.is_decaying() };
        state_arc.momentum_decaying.store(decaying as u8, Ordering::Release);
    }
}

/// Cancel an order after TTL expires OR BTC trend flips — whichever comes first.
async fn order_watchdog(
    order_id: String,
    ttl_secs: u64,
    initial_trend: u8,
    state: Arc<AppState>,
    clob: Arc<crate::clob::ClobClient>,
) {
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(ttl_secs)) => {
            tracing::info!("[ENGINE] TTL expired — cancelling {order_id}");
            let _ = clob.cancel_order(&order_id).await;
            state.orders.remove(&order_id);
        }
        _ = wait_trend_change(&state, initial_trend) => {
            tracing::info!("[ENGINE] Trend flipped — cancelling {order_id}");
            let _ = clob.cancel_order(&order_id).await;
            state.orders.remove(&order_id);
        }
    }
}

async fn wait_trend_change(state: &AppState, initial_trend: u8) {
    loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if state.btc.trend.load(Ordering::Acquire) != initial_trend {
            break;
        }
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::state::AppState;

    fn make_engine() -> TradingEngine {
        let state = AppState::new();
        let config = Arc::new(Config::from_env().unwrap());
        TradingEngine::new(state, config)
    }

    // ── MomentumWindow ────────────────────────────────────────────────────────

    #[test]
    fn test_velocity_rising() {
        let mut w = MomentumWindow::new(15);
        w.push(0, 100.0);
        w.push(1000, 101.0); // +1/s
        assert!((w.velocity() - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_velocity_falling() {
        let mut w = MomentumWindow::new(15);
        w.push(0, 100.0);
        w.push(1000, 99.0); // -1/s
        assert!((w.velocity() + 1.0).abs() < 0.001);
    }

    #[test]
    fn test_velocity_empty() {
        let w = MomentumWindow::new(15);
        assert_eq!(w.velocity(), 0.0);
    }

    #[test]
    fn test_momentum_decay_true() {
        let mut w = MomentumWindow::new(15);
        // Decreasing increments: 2, 1.5, 1.0, 0.5
        w.push(0,    100.0);
        w.push(1000, 102.0); // Δ2
        w.push(2000, 103.5); // Δ1.5
        w.push(3000, 104.5); // Δ1.0
        w.push(4000, 105.0); // Δ0.5
        assert!(w.is_decaying(), "should be decaying");
    }

    #[test]
    fn test_momentum_decay_false_accelerating() {
        let mut w = MomentumWindow::new(15);
        // Increasing increments: 0.5, 1.0, 1.5, 2.0
        w.push(0,    100.0);
        w.push(1000, 100.5);
        w.push(2000, 101.5);
        w.push(3000, 103.0);
        w.push(4000, 105.0);
        assert!(!w.is_decaying(), "should NOT be decaying");
    }

    #[test]
    fn test_momentum_decay_needs_4_points() {
        let mut w = MomentumWindow::new(15);
        w.push(0, 100.0);
        w.push(1000, 101.0);
        w.push(2000, 101.5);
        assert!(!w.is_decaying()); // only 3 points, insufficient
    }

    #[test]
    fn test_window_prunes_old_entries() {
        let mut w = MomentumWindow::new(1); // 1-second window
        w.push(0,     100.0);
        w.push(500,   100.5);
        w.push(2000,  101.0); // this should evict entries older than 1000ms from now
        assert!(w.prices.len() <= 2, "old entries should be pruned");
    }

    // ── check_reversal ────────────────────────────────────────────────────────

    fn engine_with_entry(entry_price: f64, direction: Direction) -> TradingEngine {
        let mut eng = make_engine();
        eng.entry = Some(EntryContext {
            entry_price,
            entry_time: Instant::now(),
            direction,
        });
        eng
    }

    #[test]
    fn test_reversal_normal_no_entry() {
        let eng = make_engine();
        assert_eq!(eng.check_reversal(65_000.0), ReversalStatus::Normal);
    }

    #[test]
    fn test_reversal_normal_same_direction() {
        // Entry Up @ 65000, price moves +0.04% upward → still going our way
        let eng = engine_with_entry(65_000.0, Direction::Up);
        let current = 65_000.0 * 1.0004; // +0.04%
        assert_eq!(eng.check_reversal(current), ReversalStatus::Normal);
    }

    #[test]
    fn test_reversal_warning_opposite_direction() {
        // Entry Up @ 65000, price drops 0.05% → mismatch, deviation 0.05 >= 0.6*0.08=0.048
        let eng = engine_with_entry(65_000.0, Direction::Up);
        let current = 65_000.0 * (1.0 - 0.0005); // -0.05%
        match eng.check_reversal(current) {
            ReversalStatus::Warning(d) => assert!(d > 0.0, "deviation should be positive"),
            other => panic!("expected Warning, got {other:?}"),
        }
    }

    #[test]
    fn test_reversal_flip_opposite_direction() {
        // Entry Up @ 65000, price drops 0.08% → EmergencyFlip
        let eng = engine_with_entry(65_000.0, Direction::Up);
        let current = 65_000.0 * (1.0 - 0.0008); // -0.08%
        assert_eq!(eng.check_reversal(current), ReversalStatus::EmergencyFlip);
    }

    #[test]
    fn test_reversal_no_flip_same_direction() {
        // Entry Up @ 65000, price rises 0.08% → same direction, only Warning at most
        let eng = engine_with_entry(65_000.0, Direction::Up);
        let current = 65_000.0 * 1.0008; // +0.08%
        // deviation >= threshold but NO mismatch → should be Warning (deviation >= 0.6*thresh)
        match eng.check_reversal(current) {
            ReversalStatus::EmergencyFlip => panic!("same-direction move should not flip"),
            _ => {}
        }
    }

    // ── half_kelly_size ───────────────────────────────────────────────────────

    #[test]
    fn test_kelly_sizing() {
        // edge=0.05, balance=$100, kelly_fraction=0.5 (default)
        // fraction = 2*0.05*0.5 = 0.05 → size = $5
        let eng = make_engine();
        let size = eng.half_kelly_size(0.05, 100.0);
        assert!((size - 5.0).abs() < 0.001, "expected $5, got {size}");
    }

    #[test]
    fn test_kelly_capped_at_10pct() {
        // edge=0.3, kelly_fraction=0.5 → raw fraction = 0.3, capped to 0.1
        let eng = make_engine();
        let size = eng.half_kelly_size(0.3, 100.0);
        assert!((size - 10.0).abs() < 0.001, "should be capped at $10");
    }

    #[test]
    fn test_kelly_negative_edge() {
        let eng = make_engine();
        let size = eng.half_kelly_size(-0.1, 100.0);
        assert_eq!(size, 0.0, "negative edge → zero size");
    }

    // ── compute_signal ────────────────────────────────────────────────────────

    #[test]
    fn test_signal_bull_trend() {
        let mut eng = make_engine();
        eng.state.btc.trend.store(trend::BULL, Ordering::Release);
        eng.state.time_to_expiry_secs.store(120, Ordering::Release);
        let sig = eng.compute_signal(1000, 65_000.0);
        assert_eq!(sig, Some(Direction::Up));
    }

    #[test]
    fn test_signal_bear_trend() {
        let mut eng = make_engine();
        eng.state.btc.trend.store(trend::BEAR, Ordering::Release);
        eng.state.time_to_expiry_secs.store(120, Ordering::Release);
        let sig = eng.compute_signal(1000, 65_000.0);
        assert_eq!(sig, Some(Direction::Down));
    }

    #[test]
    fn test_signal_none_neutral_trend() {
        let mut eng = make_engine();
        eng.state.btc.trend.store(trend::NEUTRAL, Ordering::Release);
        eng.state.time_to_expiry_secs.store(120, Ordering::Release);
        let sig = eng.compute_signal(1000, 65_000.0);
        assert_eq!(sig, None);
    }

    #[test]
    fn test_signal_suppressed_near_expiry_decaying() {
        let mut eng = make_engine();
        eng.state.btc.trend.store(trend::BULL, Ordering::Release);
        eng.state.time_to_expiry_secs.store(60, Ordering::Release); // < 90s guard

        // Feed decaying momentum
        eng.compute_signal(0,    100.0);
        eng.compute_signal(1000, 102.0);
        eng.compute_signal(2000, 103.5);
        eng.compute_signal(3000, 104.5);
        let sig = eng.compute_signal(4000, 105.0); // now decaying
        assert_eq!(sig, None, "signal should be suppressed near expiry with decaying momentum");
    }
}

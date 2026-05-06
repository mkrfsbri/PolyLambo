use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use chrono::Utc;

use crate::binance::get_btc_price;
use crate::config::Config;
use crate::state::{AppState, OrderSide, atomic_to_f64, bot_status};

const LOOP_INTERVAL_MS: u64 = 250;
const EXPIRY_GUARD_SECS: u64 = 90;
/// Hard floor: never open a new position inside the final 30 seconds.
const MIN_ENTRY_EXPIRY_SECS: u64 = 30;
/// Minimum token price to enter: below 5 ¢ the outcome is near-certain; skip.
const MIN_TOKEN_PRICE: f64 = 0.05;

// ── Direction ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
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
    pub signal_streak: (Option<Direction>, u8),
}

impl TradingEngine {
    pub fn new(state: Arc<AppState>, config: Arc<Config>) -> Self {
        let window = config.momentum_window_secs;
        TradingEngine {
            state,
            config,
            momentum: MomentumWindow::new(window),
            entry: None,
            signal_streak: (None, 0),
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

    /// Compute composite signal score from BTC momentum and PTB displacement.
    /// Returns Some((direction, score)) when |score| > threshold, else None.
    pub fn compute_signal(
        &mut self,
        ts_ms: u64,
        btc_price: f64,
        eth_live: f64,
        ptb: f64,
    ) -> Option<(Direction, f64)> {
        self.momentum.push(ts_ms, btc_price);

        let expiry = self.state.time_to_expiry_secs.load(Ordering::Acquire);
        if expiry < EXPIRY_GUARD_SECS && self.momentum.is_decaying() {
            tracing::debug!("[ENGINE] Expiry guard: momentum decaying with {expiry}s left");
            return None;
        }

        let velocity  = self.momentum.velocity();
        let btc_norm  = (velocity / self.config.v_scale).clamp(-1.0, 1.0);

        let ptb_norm = if ptb > 0.0 && eth_live > 0.0 {
            let ptb_pct = (eth_live - ptb) / ptb * 100.0;
            (ptb_pct / self.config.ptb_scale).clamp(-1.0, 1.0)
        } else {
            0.0
        };

        let score = self.config.alpha * btc_norm + self.config.beta * ptb_norm;
        tracing::debug!(
            "[ENGINE] vel={velocity:.2} btc_norm={btc_norm:.3} ptb_norm={ptb_norm:.3} score={score:.3}"
        );

        if score > self.config.score_threshold {
            Some((Direction::Up, score))
        } else if score < -self.config.score_threshold {
            Some((Direction::Down, score))
        } else {
            None
        }
    }

    /// Half-Kelly position size in USDC.
    /// `edge` = estimated win probability minus 0.5 (e.g. 0.05 = 55% win rate).
    pub fn half_kelly_size(&self, edge: f64, balance: f64) -> f64 {
        let fraction = (2.0 * edge) * self.config.kelly_fraction;
        fraction.clamp(0.0, 0.1) * balance
    }
}

/// Returns (new_streak_state, should_enter).
/// Fires when the same direction holds for `confirm_ticks` consecutive ticks.
/// Resets to (None, 0) after firing or on no-signal / direction flip.
pub fn update_streak(
    streak: (Option<Direction>, u8),
    signal: Option<Direction>,
    confirm_ticks: u8,
) -> ((Option<Direction>, u8), bool) {
    match signal {
        None => ((None, 0), false),
        Some(dir) => {
            let count = if streak.0 == Some(dir) { streak.1 + 1 } else { 1 };
            if count >= confirm_ticks {
                ((None, 0), true)
            } else {
                ((Some(dir), count), false)
            }
        }
    }
}

// ── engine loop ───────────────────────────────────────────────────────────────

pub async fn run_engine_loop(
    engine: Arc<Mutex<TradingEngine>>,
    clob: Arc<crate::clob::ClobClient>,
    db: Arc<crate::db::Db>,
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

        // Read composite signal inputs from state
        let (eth_live, ptb) = {
            let eng = engine.lock().await;
            let poly = atomic_to_f64(eng.state.eth_poly_spot.load(Ordering::Acquire));
            let eth  = if poly > 0.0 {
                poly
            } else {
                atomic_to_f64(eng.state.eth_spot_raw.load(Ordering::Acquire))
            };
            let p = atomic_to_f64(eng.state.eth_open_price.load(Ordering::Acquire));
            (eth, p)
        };

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
        if status == bot_status::EMERGENCY || status == bot_status::POSITION {
            continue;
        }

        // ── compute signal ─────────────────────────────────────────────────
        let signal_result = {
            let mut eng = engine.lock().await;
            eng.compute_signal(ts_ms, btc_price, eth_live, ptb)
        };

        // Update streak and gate entry on N consecutive matching ticks.
        let entry_gate = {
            let mut eng = engine.lock().await;
            match signal_result {
                Some((dir, sc)) => {
                    eng.state.signal_score.store((sc * 1_000_000.0) as i64, Ordering::Release);
                    let (new_streak, fire) = update_streak(
                        eng.signal_streak, Some(dir), eng.config.signal_confirm_ticks,
                    );
                    eng.signal_streak = new_streak;
                    if fire { Some((dir, sc)) } else { None }
                }
                None => {
                    eng.state.signal_score.store(0, Ordering::Release);
                    eng.signal_streak = (None, 0);
                    None
                }
            }
        };

        let (direction, score) = match entry_gate {
            Some(pair) => pair,
            None => continue,
        };

        // ── size + token id ────────────────────────────────────────────────
        let (size, dry_run, ttl_secs, state_arc, token_price_snap) = {
            let eng = engine.lock().await;
            let balance = atomic_to_f64(eng.state.balance_usdc.load(Ordering::Acquire));
            if balance < 1.0 {
                tracing::error!("[ENGINE] Balance ${balance:.2} < $1 — EMERGENCY halt");
                eng.state.bot_status.store(bot_status::EMERGENCY, Ordering::Release);
                let _ = clob.cancel_all().await;
                break;
            }
            let token_price = match direction {
                Direction::Up =>
                    atomic_to_f64(eng.state.eth_up_price.load(Ordering::Acquire)),
                Direction::Down =>
                    atomic_to_f64(eng.state.eth_down_price.load(Ordering::Acquire)),
            };
            const STALE_THRESHOLD: f64 = 0.65;
            let edge = if token_price >= MIN_TOKEN_PRICE && token_price < STALE_THRESHOLD {
                let score_edge = (score.abs() - eng.config.score_threshold).max(0.0);
                let token_mispricing = if token_price < 0.5 { 0.5 - token_price } else { 0.0 };
                score_edge + token_mispricing
            } else {
                0.0
            };
            tracing::debug!(
                "[ENGINE] token_price={token_price:.4} score={score:.3} edge={edge:.4}"
            );
            let sz = eng.half_kelly_size(edge, balance);
            (sz, eng.config.dry_run, eng.config.order_ttl_secs, eng.state.clone(), token_price)
        };

        if size < 1.0 {
            continue;
        }

        // Hard expiry guard: refuse new entries in the final 30 seconds regardless of signal.
        if expiry < MIN_ENTRY_EXPIRY_SECS {
            tracing::debug!(
                "[ENGINE] Expiry guard: {expiry}s remaining — new entries blocked"
            );
            continue;
        }

        if dry_run {
            let fake_id = format!(
                "dry-{:016x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0),
            );
            state_arc.orders.insert(fake_id.clone(), crate::state::ActiveOrder {
                order_id:  fake_id.clone(),
                side:      match direction { Direction::Up => crate::state::OrderSide::Up,
                                             Direction::Down => crate::state::OrderSide::Down },
                price:     token_price_snap,
                size_usdc: size,
                placed_at: Instant::now(),
                status:    crate::state::OrderStatus::Filled,
            });
            state_arc.bot_status.store(bot_status::POSITION, Ordering::Release);

            // Deduct entry cost from simulated balance.
            let bal = atomic_to_f64(state_arc.balance_usdc.load(Ordering::Acquire));
            state_arc.balance_usdc.store(
                crate::state::f64_to_atomic((bal - size).max(0.0)),
                Ordering::Release,
            );

            let qty_shares_entry = if token_price_snap > 0.0 { size / token_price_snap } else { 0.0 };
            let ptb_at_fill = atomic_to_f64(state_arc.eth_open_price.load(Ordering::Acquire));
            let initial_slug = state_arc.current_slug.read().await.clone();

            let btc_vel_snap = engine.lock().await.momentum.velocity();
            let ptb_pct_snap = if ptb > 0.0 && eth_live > 0.0 {
                (eth_live - ptb) / ptb * 100.0
            } else { 0.0 };
            let eth_price_snap = if eth_live > 0.0 { eth_live }
                else { atomic_to_f64(state_arc.eth_spot_raw.load(Ordering::Acquire)) };
            let slug_snap = initial_slug.clone();
            let confirm_ticks_snap = engine.lock().await.config.signal_confirm_ticks;

            // Persist trade open to DB
            let trade_id = match db.insert_trade(&crate::db::TradeEntry {
                opened_at:     chrono::Utc::now().to_rfc3339(),
                slug:          slug_snap,
                side:          match direction { Direction::Up => "Up".to_string(), Direction::Down => "Down".to_string() },
                entry_price:   token_price_snap,
                size_usdc:     size,
                qty_shares:    qty_shares_entry,
                score,
                btc_vel:       btc_vel_snap,
                ptb_pct:       ptb_pct_snap,
                btc_price:     btc_price,
                eth_price:     eth_price_snap,
                confirm_ticks: confirm_ticks_snap,
            }).await {
                Ok(id) => id,
                Err(e) => { tracing::warn!("[DB] insert_trade failed: {e:#}"); -1 }
            };

            tracing::info!(
                "[ENGINE] DRY_RUN signal={:?} score={score:.3} size=${size:.2} \
                 expiry={expiry}s trade_id={trade_id}",
                direction
            );

            let (tp_pct, sl_pct) = {
                let eng = engine.lock().await;
                (eng.config.take_profit_pct, eng.config.stop_loss_pct)
            };

            let decaying = { engine.lock().await.momentum.is_decaying() };
            state_arc.momentum_decaying.store(decaying as u8, Ordering::Release);

            let state_w = state_arc.clone();
            let clob_w  = clob.clone();
            let db_w    = db.clone();
            let token_id_w = match direction {
                Direction::Up   => state_arc.up_token_id.read().await.clone(),
                Direction::Down => state_arc.down_token_id.read().await.clone(),
            };

            tokio::spawn(async move {
                position_watchdog(
                    fake_id, token_id_w, direction, token_price_snap,
                    size, qty_shares_entry, initial_slug, ptb_at_fill,
                    tp_pct, sl_pct, trade_id, true,
                    state_w, clob_w, db_w,
                ).await;
            });
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

        state_arc.orders.insert(order_id.clone(), crate::state::ActiveOrder {
            order_id: order_id.clone(),
            side:     order_side.clone(),
            price,
            size_usdc: size,
            placed_at: Instant::now(),
            status:   crate::state::OrderStatus::Pending,
        });
        state_arc.bot_status.store(bot_status::POSITION, Ordering::Release);
        tracing::info!(
            "[ENGINE] POSITION {:?} | ${size:.2} | price={price:.4} | ttl={ttl_secs}s | lat={}μs",
            direction, lat_us
        );

        // ── position watchdog ─────────────────────────────────────────────
        let qty_shares_live = if price > 0.0 { size / price } else { 0.0 };
        let ptb_at_fill_live = atomic_to_f64(state_arc.eth_open_price.load(Ordering::Acquire));
        let initial_slug_live = state_arc.current_slug.read().await.clone();
        let btc_vel_snap = engine.lock().await.momentum.velocity();
        let ptb_pct_snap = if ptb > 0.0 && eth_live > 0.0 {
            (eth_live - ptb) / ptb * 100.0
        } else { 0.0 };
        let eth_price_snap = if eth_live > 0.0 { eth_live }
            else { atomic_to_f64(state_arc.eth_spot_raw.load(Ordering::Acquire)) };
        let confirm_ticks_snap = engine.lock().await.config.signal_confirm_ticks;

        let trade_id_live = match db.insert_trade(&crate::db::TradeEntry {
            opened_at:     chrono::Utc::now().to_rfc3339(),
            slug:          initial_slug_live.clone(),
            side:          match direction { Direction::Up => "Up".to_string(), Direction::Down => "Down".to_string() },
            entry_price:   price,
            size_usdc:     size,
            qty_shares:    qty_shares_live,
            score,
            btc_vel:       btc_vel_snap,
            ptb_pct:       ptb_pct_snap,
            btc_price:     btc_price,
            eth_price:     eth_price_snap,
            confirm_ticks: confirm_ticks_snap,
        }).await {
            Ok(id) => id,
            Err(e) => { tracing::warn!("[DB] insert_trade failed: {e:#}"); -1 }
        };

        let (tp_pct, sl_pct) = {
            let eng = engine.lock().await;
            (eng.config.take_profit_pct, eng.config.stop_loss_pct)
        };

        let state_for_watch = state_arc.clone();
        let clob_w = clob.clone();
        let db_w = db.clone();
        tokio::spawn(async move {
            position_watchdog(
                order_id, token_id, direction, price,
                size, qty_shares_live, initial_slug_live, ptb_at_fill_live,
                tp_pct, sl_pct, trade_id_live, false,
                state_for_watch, clob_w, db_w,
            ).await;
        });

        // Update momentum_decaying for TUI
        let decaying = { engine.lock().await.momentum.is_decaying() };
        state_arc.momentum_decaying.store(decaying as u8, Ordering::Release);
    }
}

fn record_trade(state: &AppState, record: crate::state::TradeRecord) {
    let mut h = state.trade_history.lock().unwrap();
    h.push_front(record);
    h.truncate(50);
}

#[allow(clippy::too_many_arguments)]
async fn position_watchdog(
    order_id: String,
    token_id: String,
    direction: Direction,
    entry_price: f64,
    size_usdc: f64,
    qty_shares: f64,
    initial_slug: String,
    ptb_at_fill: f64,
    take_profit_pct: f64,
    stop_loss_pct: f64,
    trade_id: i64,
    dry_run: bool,
    state: Arc<crate::state::AppState>,
    clob: Arc<crate::clob::ClobClient>,
    db: Arc<crate::db::Db>,
) {
    use crate::state::{atomic_to_f64, bot_status, f64_to_atomic};

    let exit_reason: &'static str = loop {
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Priority 1: emergency
        if state.bot_status.load(Ordering::Acquire) == bot_status::EMERGENCY {
            break "Emergency";
        }

        // Priority 2: slug rotation → market resolved
        {
            let slug = state.current_slug.read().await;
            if !slug.is_empty() && *slug != initial_slug {
                break "Resolved";
            }
        }

        // Priority 3: TP / SL on token mid-price
        let current_price = atomic_to_f64(match direction {
            Direction::Up   => state.eth_up_price.load(Ordering::Acquire),
            Direction::Down => state.eth_down_price.load(Ordering::Acquire),
        });
        if current_price > 0.0 {
            let pnl_pct = (current_price - entry_price) / entry_price * 100.0;
            if pnl_pct >= take_profit_pct { break "TakeProfit"; }
            if pnl_pct <= -stop_loss_pct  { break "StopLoss"; }
        }
    };

    let exit_price = atomic_to_f64(match direction {
        Direction::Up   => state.eth_up_price.load(Ordering::Acquire),
        Direction::Down => state.eth_down_price.load(Ordering::Acquire),
    });

    // Calculate proceeds
    let proceeds = if exit_reason == "Resolved" {
        let eth_now = {
            let poly = atomic_to_f64(state.eth_poly_spot.load(Ordering::Acquire));
            if poly > 0.0 { poly }
            else { atomic_to_f64(state.eth_spot_raw.load(Ordering::Acquire)) }
        };
        let won = ptb_at_fill > 0.0 && eth_now > 0.0 && match direction {
            Direction::Up   => eth_now > ptb_at_fill,
            Direction::Down => eth_now < ptb_at_fill,
        };
        if won { qty_shares * 1.0 } else { 0.0 }
    } else {
        qty_shares * exit_price
    };

    let pnl = proceeds - size_usdc;

    // Update shared state
    if dry_run {
        let bal = atomic_to_f64(state.balance_usdc.load(Ordering::Acquire));
        state.balance_usdc.store(f64_to_atomic((bal + proceeds).max(0.0)), Ordering::Release);
    }
    state.pnl_usdc.fetch_add((pnl * 100.0) as i64, Ordering::AcqRel);
    state.orders.remove(&order_id);
    state.bot_status.store(bot_status::HUNTING, Ordering::Release);

    record_trade(&state, crate::state::TradeRecord {
        closed_at:   Utc::now(),
        side:        match direction {
            Direction::Up   => crate::state::OrderSide::Up,
            Direction::Down => crate::state::OrderSide::Down,
        },
        entry_price,
        qty_shares,
        status: if exit_reason == "StopLoss" || exit_reason == "Emergency" {
            crate::state::TradeStatus::Cancelled
        } else {
            crate::state::TradeStatus::Filled
        },
    });

    // Persist close to DB (non-blocking: log on failure, never panic)
    if let Err(e) = db.close_trade(trade_id, exit_price, pnl, exit_reason).await {
        tracing::warn!("[DB] close_trade failed: {e:#}");
    }

    // Live: cancel pending orders and sell if TP/SL/Emergency
    if !dry_run && exit_reason != "Resolved" {
        let _ = clob.cancel_all().await;
        if qty_shares > 0.0 {
            let _ = clob.sell_best_bid(&token_id, qty_shares).await;
        }
    }

    tracing::info!(
        "[ENGINE] Position closed | dir={direction:?} reason={exit_reason} \
         entry={entry_price:.4} exit={exit_price:.4} pnl=${pnl:.2}"
    );
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
    fn test_signal_rising_btc_fires_up() {
        let mut eng = make_engine();
        eng.state.time_to_expiry_secs.store(200, Ordering::Release);
        // +$10/s velocity → btc_norm=1.0 → score=0.60 > threshold 0.15
        eng.compute_signal(0, 60_000.0, 0.0, 0.0);
        let sig = eng.compute_signal(1000, 60_010.0, 0.0, 0.0);
        assert!(matches!(sig, Some((Direction::Up, _))), "rising BTC should fire Up");
    }

    #[test]
    fn test_signal_falling_btc_fires_down() {
        let mut eng = make_engine();
        eng.state.time_to_expiry_secs.store(200, Ordering::Release);
        eng.compute_signal(0, 60_000.0, 0.0, 0.0);
        let sig = eng.compute_signal(1000, 59_990.0, 0.0, 0.0);
        assert!(matches!(sig, Some((Direction::Down, _))), "falling BTC should fire Down");
    }

    #[test]
    fn test_signal_tiny_move_no_trade() {
        let mut eng = make_engine();
        eng.state.time_to_expiry_secs.store(200, Ordering::Release);
        // +$0.5/s → btc_norm=0.05 → score=0.03, below threshold 0.15
        eng.compute_signal(0, 60_000.0, 0.0, 0.0);
        let sig = eng.compute_signal(1000, 60_000.5, 0.0, 0.0);
        assert!(sig.is_none(), "tiny BTC move should not fire");
    }

    #[test]
    fn test_signal_ptb_overrides_weak_btc() {
        let mut eng = make_engine();
        eng.state.time_to_expiry_secs.store(200, Ordering::Release);
        // BTC +$2/s → btc_norm=0.2 → alpha*btc=0.12
        // ETH $2450 vs PTB $2500 → ptb_pct=-2.0% → ptb_norm=-1.0 → beta*ptb=-0.40
        // score = 0.12 - 0.40 = -0.28 → Down
        eng.compute_signal(0, 60_000.0, 0.0, 0.0);
        let sig = eng.compute_signal(1000, 60_002.0, 2450.0, 2500.0);
        assert!(matches!(sig, Some((Direction::Down, _))),
            "PTB displacement should flip weak Up-BTC to Down");
    }

    #[test]
    fn test_signal_ptb_zero_uses_btc_only() {
        let mut eng = make_engine();
        eng.state.time_to_expiry_secs.store(200, Ordering::Release);
        // ptb=0 → ptb_norm=0; strong BTC still fires
        eng.compute_signal(0, 60_000.0, 0.0, 0.0);
        let sig = eng.compute_signal(1000, 60_010.0, 2360.0, 0.0);
        assert!(matches!(sig, Some((Direction::Up, _))),
            "should fire on BTC alone when ptb=0");
    }

    #[test]
    fn test_signal_score_returned_with_direction() {
        let mut eng = make_engine();
        eng.state.time_to_expiry_secs.store(200, Ordering::Release);
        eng.compute_signal(0, 60_000.0, 0.0, 0.0);
        let sig = eng.compute_signal(1000, 60_010.0, 0.0, 0.0);
        if let Some((dir, score)) = sig {
            assert_eq!(dir, Direction::Up);
            assert!(score > 0.15, "score {score} should exceed threshold 0.15");
        } else {
            panic!("expected a signal");
        }
    }

    #[test]
    fn test_signal_suppressed_near_expiry_decaying() {
        let mut eng = make_engine();
        eng.state.time_to_expiry_secs.store(60, Ordering::Release);
        // Feed decaying momentum (copied from old test, updated signature)
        eng.compute_signal(0,    100.0, 0.0, 0.0);
        eng.compute_signal(1000, 102.0, 0.0, 0.0);
        eng.compute_signal(2000, 103.5, 0.0, 0.0);
        eng.compute_signal(3000, 104.5, 0.0, 0.0);
        let sig = eng.compute_signal(4000, 105.0, 0.0, 0.0);
        assert!(sig.is_none(), "signal should be suppressed near expiry with decaying momentum");
    }

    // ── update_streak ─────────────────────────────────────────────────────────

    #[test]
    fn test_streak_builds_and_fires() {
        let mut s = (None::<Direction>, 0u8);

        let (ns, fire) = update_streak(s, Some(Direction::Up), 3);
        assert!(!fire); assert_eq!(ns, (Some(Direction::Up), 1)); s = ns;

        let (ns, fire) = update_streak(s, Some(Direction::Up), 3);
        assert!(!fire); assert_eq!(ns, (Some(Direction::Up), 2)); s = ns;

        let (ns, fire) = update_streak(s, Some(Direction::Up), 3);
        assert!(fire); assert_eq!(ns, (None, 0)); s = ns;

        // After reset, needs a fresh run
        let (ns, fire) = update_streak(s, Some(Direction::Up), 3);
        assert!(!fire); assert_eq!(ns, (Some(Direction::Up), 1));
    }

    #[test]
    fn test_streak_resets_on_direction_flip() {
        let s = (Some(Direction::Up), 2u8);
        let (ns, fire) = update_streak(s, Some(Direction::Down), 3);
        assert!(!fire);
        assert_eq!(ns, (Some(Direction::Down), 1));
    }

    #[test]
    fn test_streak_resets_on_no_signal() {
        let s = (Some(Direction::Up), 2u8);
        let (ns, fire) = update_streak(s, None, 3);
        assert!(!fire);
        assert_eq!(ns, (None, 0));
    }

    #[test]
    fn test_streak_fires_at_threshold_one() {
        let s = (None, 0u8);
        let (ns, fire) = update_streak(s, Some(Direction::Down), 1);
        assert!(fire);
        assert_eq!(ns, (None, 0));
    }
}

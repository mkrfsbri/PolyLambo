use eth5m_bot::config::Config;
use eth5m_bot::engine::{Direction, EntryContext, MomentumWindow, ReversalStatus, TradingEngine};
use eth5m_bot::state::{trend, AppState};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

fn make_engine() -> TradingEngine {
    TradingEngine::new(AppState::new(), Arc::new(Config::from_env().unwrap()))
}

fn engine_with_entry(entry_price: f64, direction: Direction) -> TradingEngine {
    let mut eng = make_engine();
    eng.entry = Some(EntryContext { entry_price, entry_time: Instant::now(), direction });
    eng
}

// ── reversal ──────────────────────────────────────────────────────────────────

#[test]
fn reversal_normal() {
    assert_eq!(make_engine().check_reversal(65_000.0), ReversalStatus::Normal);
}

#[test]
fn reversal_warning() {
    // -0.05% with threshold 0.08 → deviation ≥ 0.6*0.08=0.048% → Warning
    let eng = engine_with_entry(65_000.0, Direction::Up);
    match eng.check_reversal(65_000.0 * (1.0 - 0.0005)) {
        ReversalStatus::Warning(d) => assert!(d > 0.0),
        other => panic!("expected Warning, got {other:?}"),
    }
}

#[test]
fn reversal_flip() {
    // -0.08% adverse move → EmergencyFlip
    let eng = engine_with_entry(65_000.0, Direction::Up);
    assert_eq!(
        eng.check_reversal(65_000.0 * (1.0 - 0.0008)),
        ReversalStatus::EmergencyFlip
    );
}

// ── momentum decay ────────────────────────────────────────────────────────────

#[test]
fn momentum_decay() {
    let mut w = MomentumWindow::new(15);
    // per-tick velocity: 2, 1.5, 1.0, 0.5 → strictly decreasing
    w.push(0,    100.0);
    w.push(1000, 102.0);
    w.push(2000, 103.5);
    w.push(3000, 104.5);
    w.push(4000, 105.0);
    assert!(w.is_decaying());
}

// ── kelly sizing ──────────────────────────────────────────────────────────────

#[test]
fn kelly_sizing() {
    let eng = make_engine();
    // edge=0.05, balance=$100, fraction=0.5 → 2*0.05*0.5=0.05 → $5
    let size = eng.half_kelly_size(0.05, 100.0);
    assert!((size - 5.0).abs() < 0.001, "expected $5, got {size}");
}

#[test]
fn kelly_capped_at_10_pct() {
    let eng = make_engine();
    // edge=0.5 → raw=0.5, capped to 0.1 → $10 on $100
    let size = eng.half_kelly_size(0.5, 100.0);
    assert!((size - 10.0).abs() < 0.001);
}

// ── compute_signal ────────────────────────────────────────────────────────────

#[test]
fn signal_bull_returns_up() {
    let mut eng = make_engine();
    eng.state.btc.trend.store(trend::BULL, Ordering::Release);
    eng.state.time_to_expiry_secs.store(200, Ordering::Release);
    assert_eq!(eng.compute_signal(1000, 65_000.0), Some(Direction::Up));
}

#[test]
fn signal_bear_returns_down() {
    let mut eng = make_engine();
    eng.state.btc.trend.store(trend::BEAR, Ordering::Release);
    eng.state.time_to_expiry_secs.store(200, Ordering::Release);
    assert_eq!(eng.compute_signal(1000, 65_000.0), Some(Direction::Down));
}

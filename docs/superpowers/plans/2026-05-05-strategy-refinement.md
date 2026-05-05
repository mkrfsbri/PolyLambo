# Strategy Refinement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reduce false entries with a signal persistence gate, add a TP/SL position watchdog, and log every trade open/close to SQLite.

**Architecture:** A pure `update_streak` helper gates entries on N consecutive ticks above threshold. A unified `position_watchdog` async function replaces both the dry-run slug-rotation watchdog and the live `order_watchdog`, handling all four exit paths (TakeProfit, StopLoss, Resolved, Emergency). A new `src/db.rs` module wraps sqlx `SqlitePool` and appends rows with `insert_trade` / `close_trade`; the row ID travels from entry to the watchdog closure to correlate open and close.

**Tech Stack:** Rust / Tokio / sqlx 0.7 (SQLite, runtime-tokio-native-tls)

---

## File Map

| File | Action | Responsibility |
|---|---|---|
| `Cargo.toml` | Modify | Add sqlx dep |
| `migrations/001_trades.sql` | Create | SQLite schema (embedded at compile time) |
| `src/db.rs` | Create | `Db`, `TradeEntry`, `open`, `insert_trade`, `close_trade` |
| `src/lib.rs` | Modify | Export `pub mod db` |
| `src/config.rs` | Modify | Add `signal_confirm_ticks`, `take_profit_pct`, `stop_loss_pct`, `db_path` |
| `src/engine.rs` | Modify | Add `signal_streak` to `TradingEngine`; add `update_streak`; wire streak gate; add `position_watchdog`; remove `order_watchdog` + `wait_trend_change`; accept `Arc<Db>` |
| `src/main.rs` | Modify | Init `Db::open`, pass `Arc<Db>` to `run_engine_loop` |

---

### Task 1: Add sqlx dependency and migration schema

**Files:**
- Modify: `Cargo.toml`
- Create: `migrations/001_trades.sql`

- [ ] **Step 1: Add sqlx to Cargo.toml**

In `Cargo.toml` under `[dependencies]`, add after the `anyhow` line:

```toml
sqlx = { version = "0.7", features = ["runtime-tokio-native-tls", "sqlite"] }
```

- [ ] **Step 2: Create migrations/001_trades.sql**

```bash
mkdir -p migrations
```

Create `migrations/001_trades.sql` with this content:

```sql
CREATE TABLE IF NOT EXISTS trades (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    opened_at     TEXT    NOT NULL,
    closed_at     TEXT,
    slug          TEXT    NOT NULL,
    side          TEXT    NOT NULL,
    entry_price   REAL    NOT NULL,
    exit_price    REAL,
    size_usdc     REAL    NOT NULL,
    qty_shares    REAL    NOT NULL,
    pnl_usdc      REAL,
    exit_reason   TEXT,
    score         REAL    NOT NULL,
    btc_vel       REAL    NOT NULL,
    ptb_pct       REAL    NOT NULL,
    btc_price     REAL    NOT NULL,
    eth_price     REAL    NOT NULL,
    confirm_ticks INTEGER NOT NULL
);
```

- [ ] **Step 3: Verify compilation**

```bash
cargo check 2>&1 | tail -5
```

Expected: `Finished dev profile` with zero errors.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock migrations/001_trades.sql
git commit -m "feat: add sqlx dep and trades migration schema"
```

---

### Task 2: Create src/db.rs — Db, TradeEntry, open/insert/close

**Files:**
- Create: `src/db.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Add failing tests to a new src/db.rs**

Create `src/db.rs`:

```rust
use anyhow::Result;
use sqlx::SqlitePool;

pub struct Db {
    pool: SqlitePool,
}

pub struct TradeEntry {
    pub opened_at:     String,
    pub slug:          String,
    pub side:          String,
    pub entry_price:   f64,
    pub size_usdc:     f64,
    pub qty_shares:    f64,
    pub score:         f64,
    pub btc_vel:       f64,
    pub ptb_pct:       f64,
    pub btc_price:     f64,
    pub eth_price:     f64,
    pub confirm_ticks: u8,
}

impl Db {
    pub async fn open(_url: &str) -> Result<Self> {
        todo!()
    }

    pub async fn insert_trade(&self, _e: &TradeEntry) -> Result<i64> {
        todo!()
    }

    pub async fn close_trade(&self, _id: i64, _exit_price: f64, _pnl_usdc: f64, _reason: &str) -> Result<()> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn sample_entry() -> TradeEntry {
        TradeEntry {
            opened_at:     Utc::now().to_rfc3339(),
            slug:          "eth-updown-5m-1746000300".to_string(),
            side:          "Up".to_string(),
            entry_price:   0.52,
            size_usdc:     15.0,
            qty_shares:    28.84,
            score:         0.34,
            btc_vel:       12.5,
            ptb_pct:       1.2,
            btc_price:     62000.0,
            eth_price:     3100.0,
            confirm_ticks: 3,
        }
    }

    #[tokio::test]
    async fn test_insert_returns_positive_id() {
        let db = Db::open("sqlite::memory:").await.unwrap();
        let id = db.insert_trade(&sample_entry()).await.unwrap();
        assert!(id > 0);
    }

    #[tokio::test]
    async fn test_insert_returns_unique_ids() {
        let db = Db::open("sqlite::memory:").await.unwrap();
        let id1 = db.insert_trade(&sample_entry()).await.unwrap();
        let id2 = db.insert_trade(&sample_entry()).await.unwrap();
        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn test_close_trade_and_idempotent() {
        let db = Db::open("sqlite::memory:").await.unwrap();
        let id = db.insert_trade(&sample_entry()).await.unwrap();
        db.close_trade(id, 0.58, 1.73, "TakeProfit").await.unwrap();
        // Second call is a no-op, must not error
        db.close_trade(id, 0.60, 2.00, "TakeProfit").await.unwrap();
    }
}
```

- [ ] **Step 2: Add db to lib.rs**

In `src/lib.rs`, add:

```rust
pub mod db;
```

- [ ] **Step 3: Run tests — expect panic from todo!()**

```bash
cargo test db::tests 2>&1 | tail -15
```

Expected: tests panic with `not yet implemented`.

- [ ] **Step 4: Implement Db::open**

Replace the `todo!()` in `open`:

```rust
pub async fn open(url: &str) -> Result<Self> {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(url)
        .await?;
    sqlx::query(include_str!("../migrations/001_trades.sql"))
        .execute(&pool)
        .await?;
    Ok(Db { pool })
}
```

- [ ] **Step 5: Implement Db::insert_trade**

Replace the `todo!()` in `insert_trade`:

```rust
pub async fn insert_trade(&self, e: &TradeEntry) -> Result<i64> {
    let result = sqlx::query(
        "INSERT INTO trades \
         (opened_at, slug, side, entry_price, size_usdc, qty_shares, \
          score, btc_vel, ptb_pct, btc_price, eth_price, confirm_ticks) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&e.opened_at)
    .bind(&e.slug)
    .bind(&e.side)
    .bind(e.entry_price)
    .bind(e.size_usdc)
    .bind(e.qty_shares)
    .bind(e.score)
    .bind(e.btc_vel)
    .bind(e.ptb_pct)
    .bind(e.btc_price)
    .bind(e.eth_price)
    .bind(e.confirm_ticks as i64)
    .execute(&self.pool)
    .await?;
    Ok(result.last_insert_rowid())
}
```

- [ ] **Step 6: Implement Db::close_trade**

Replace the `todo!()` in `close_trade`:

```rust
pub async fn close_trade(&self, id: i64, exit_price: f64, pnl_usdc: f64, reason: &str) -> Result<()> {
    sqlx::query(
        "UPDATE trades \
         SET closed_at = ?, exit_price = ?, pnl_usdc = ?, exit_reason = ? \
         WHERE id = ? AND closed_at IS NULL"
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(exit_price)
    .bind(pnl_usdc)
    .bind(reason)
    .bind(id)
    .execute(&self.pool)
    .await?;
    Ok(())
}
```

- [ ] **Step 7: Run tests — expect all pass**

```bash
cargo test db::tests 2>&1 | tail -10
```

Expected:
```
test db::tests::test_close_trade_and_idempotent ... ok
test db::tests::test_insert_returns_positive_id ... ok
test db::tests::test_insert_returns_unique_ids ... ok
test result: ok. 3 passed; 0 failed
```

- [ ] **Step 8: Commit**

```bash
git add src/db.rs src/lib.rs
git commit -m "feat(db): Db struct with open/insert_trade/close_trade backed by SQLite"
```

---

### Task 3: Add new config params

**Files:**
- Modify: `src/config.rs`

- [ ] **Step 1: Add failing tests**

In `src/config.rs`, inside the existing `mod tests { ... }` block, add after the existing tests:

```rust
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
```

- [ ] **Step 2: Run tests — expect compile error on missing fields**

```bash
cargo test config::tests 2>&1 | tail -10
```

Expected: compile error — `Config` has no field `signal_confirm_ticks`.

- [ ] **Step 3: Add fields to Config struct**

In `src/config.rs`, add four fields to the `Config` struct after `score_threshold`:

```rust
pub signal_confirm_ticks: u8,
pub take_profit_pct: f64,
pub stop_loss_pct: f64,
pub db_path: String,
```

- [ ] **Step 4: Populate fields in Config::from_env**

In `Config::from_env()`, add after the `score_threshold` line:

```rust
signal_confirm_ticks: env::var("SIGNAL_CONFIRM_TICKS")
    .ok().and_then(|v| v.parse().ok()).unwrap_or(3),
take_profit_pct: env::var("TAKE_PROFIT_PCT")
    .ok().and_then(|v| v.parse().ok()).unwrap_or(8.0),
stop_loss_pct: env::var("STOP_LOSS_PCT")
    .ok().and_then(|v| v.parse().ok()).unwrap_or(5.0),
db_path: env::var("DB_PATH")
    .unwrap_or_else(|_| "./trades.db".to_string()),
```

- [ ] **Step 5: Run tests — expect all pass**

```bash
cargo test config::tests 2>&1 | tail -10
```

Expected: all config tests pass including the two new ones.

- [ ] **Step 6: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add signal_confirm_ticks, take_profit_pct, stop_loss_pct, db_path"
```

---

### Task 4: Add update_streak pure helper + signal_streak field

**Files:**
- Modify: `src/engine.rs`

- [ ] **Step 1: Add failing tests for update_streak**

At the bottom of `src/engine.rs`, inside the existing `mod tests { ... }` block, add:

```rust
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
```

- [ ] **Step 2: Run tests — expect compile error (update_streak not defined)**

```bash
cargo test engine::tests::test_streak 2>&1 | tail -10
```

Expected: compile error — `update_streak` not found.

- [ ] **Step 3: Add update_streak function to src/engine.rs**

Add this function just above the `// ── engine loop` comment:

```rust
/// Returns (new_streak_state, should_enter).
/// Fires when the same direction exceeds the threshold for `confirm_ticks` consecutive ticks.
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
```

- [ ] **Step 4: Add signal_streak field to TradingEngine struct**

In the `TradingEngine` struct definition, add after `entry: Option<EntryContext>`:

```rust
pub signal_streak: (Option<Direction>, u8),
```

In `TradingEngine::new`, add after `entry: None`:

```rust
signal_streak: (None, 0),
```

- [ ] **Step 5: Run tests — expect all streak tests pass**

```bash
cargo test engine::tests::test_streak 2>&1 | tail -10
```

Expected:
```
test engine::tests::test_streak_builds_and_fires ... ok
test engine::tests::test_streak_resets_on_direction_flip ... ok
test engine::tests::test_streak_resets_on_no_signal ... ok
test engine::tests::test_streak_fires_at_threshold_one ... ok
test result: ok. 4 passed; 0 failed
```

- [ ] **Step 6: Commit**

```bash
git add src/engine.rs
git commit -m "feat(engine): update_streak helper + signal_streak field on TradingEngine"
```

---

### Task 5: Wire streak gate into run_engine_loop

**Files:**
- Modify: `src/engine.rs` (inside `run_engine_loop`)

- [ ] **Step 1: Locate the signal block in run_engine_loop**

Find this block in `run_engine_loop` (around line 295–305):

```rust
let signal_result = {
    let mut eng = engine.lock().await;
    eng.compute_signal(ts_ms, btc_price, eth_live, ptb)
};
let (direction, score) = match signal_result {
    Some(pair) => pair,
    None => {
        engine.lock().await.state.signal_score.store(0, Ordering::Release);
        continue;
    }
};
engine.lock().await.state.signal_score.store(
    (score * 1_000_000.0) as i64,
    Ordering::Release,
);
```

- [ ] **Step 2: Replace the signal block with streak-gated version**

Replace the block identified in Step 1 with:

```rust
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
```

- [ ] **Step 3: Verify compilation**

```bash
cargo check 2>&1 | tail -5
```

Expected: `Finished dev profile` with zero errors.

- [ ] **Step 4: Run full test suite**

```bash
cargo test 2>&1 | tail -15
```

Expected: all existing tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/engine.rs
git commit -m "feat(engine): signal persistence gate — require N consecutive ticks before entry"
```

---

### Task 6: Replace watchdog with unified position_watchdog + DB logging

**Files:**
- Modify: `src/engine.rs`

This is the largest task. It:
1. Adds `position_watchdog` async function (replaces both `order_watchdog` and the dry-run slug-rotation watchdog)
2. Calls `db.insert_trade` before spawning the watchdog
3. Removes `order_watchdog` and `wait_trend_change`
4. Changes `run_engine_loop` signature to accept `Arc<crate::db::Db>`

- [ ] **Step 1: Change run_engine_loop signature**

Find the function signature:
```rust
pub async fn run_engine_loop(
    engine: Arc<Mutex<TradingEngine>>,
    clob: Arc<crate::clob::ClobClient>,
) {
```

Replace with:
```rust
pub async fn run_engine_loop(
    engine: Arc<Mutex<TradingEngine>>,
    clob: Arc<crate::clob::ClobClient>,
    db: Arc<crate::db::Db>,
) {
```

- [ ] **Step 2: Add position_watchdog function**

Add this function at the bottom of `src/engine.rs`, just before the `fn record_trade` definition:

```rust
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
```

- [ ] **Step 3: Replace the dry-run watchdog spawn with position_watchdog**

Find the existing dry-run watchdog block. It starts roughly with:
```rust
// Watchdog: hold the position until the market window rotates (= resolution).
tokio::spawn(async move {
    // Loop until slug changes; snapshot ETH price on the last tick.
    let eth_at_close = loop {
```

And ends just before:
```rust
            continue;
        }
```

Identify the full dry-run block (starting from `if dry_run {` and ending just after the `tokio::spawn` call with `continue;`). Replace the content of `if dry_run { ... continue; }` with:

```rust
if dry_run {
    // Fake fill: insert order as Filled immediately.
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

    // Capture btc_vel for DB entry
    let btc_vel_snap = engine.lock().await.momentum.velocity();
    let ptb_pct_snap = if ptb > 0.0 && eth_live > 0.0 {
        (eth_live - ptb) / ptb * 100.0
    } else { 0.0 };
    let eth_price_snap = if eth_live > 0.0 { eth_live }
        else { atomic_to_f64(state_arc.eth_spot_raw.load(Ordering::Acquire)) };
    let slug_snap = initial_slug.clone();
    let confirm_ticks_snap = {
        engine.lock().await.config.signal_confirm_ticks
    };

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
        Err(e) => {
            tracing::warn!("[DB] insert_trade failed: {e:#}");
            -1
        }
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
```

- [ ] **Step 4: Replace the live order_watchdog spawn with position_watchdog**

Find the live watchdog block (after `state_arc.bot_status.store(bot_status::POSITION, ...)` in the live path):

```rust
// ── watchdog ───────────────────────────────────────────────────────
let initial_trend   = state_arc.btc.trend.load(Ordering::Acquire);
let state_for_watch = state_arc.clone();
let clob_w          = clob.clone();
tokio::spawn(async move {
    order_watchdog(order_id, ttl_secs, initial_trend, state_for_watch, clob_w).await;
});
```

Replace this block with:

```rust
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
```

- [ ] **Step 5: Remove order_watchdog and wait_trend_change**

Delete the `async fn order_watchdog(...)` function and the `async fn wait_trend_change(...)` function entirely. They are no longer called.

- [ ] **Step 6: Verify compilation**

```bash
cargo check 2>&1 | grep -E "^error" | head -20
```

Expected: zero errors. Fix any `unused variable` or borrow issues flagged as errors (warnings are OK).

- [ ] **Step 7: Run full test suite**

```bash
cargo test 2>&1 | tail -15
```

Expected: all existing tests still pass.

- [ ] **Step 8: Commit**

```bash
git add src/engine.rs
git commit -m "feat(engine): unified position_watchdog with TP/SL exit + DB logging"
```

---

### Task 7: Wire Db into main.rs and run full test pass

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Init Db in main**

In `src/main.rs`, after the `let clob = ...` block and before the pre-flight checks, add:

```rust
// ── trade database ────────────────────────────────────────────────────────────
let db = Arc::new(
    eth5m_bot::db::Db::open(&cfg.db_path)
        .await
        .context("failed to open trades.db")?,
);
tracing::info!("[PREFLIGHT] Trade DB open: {}", cfg.db_path);
```

- [ ] **Step 2: Pass Arc<Db> to run_engine_loop**

Find this line in `main.rs`:

```rust
let engine_task = tokio::spawn(async move { engine::run_engine_loop(te, clob_e).await });
```

Replace with:

```rust
let db_e = db.clone();
let engine_task = tokio::spawn(async move { engine::run_engine_loop(te, clob_e, db_e).await });
```

- [ ] **Step 3: Verify compilation**

```bash
cargo check 2>&1 | tail -5
```

Expected: `Finished dev profile` with zero errors.

- [ ] **Step 4: Run full test suite**

```bash
cargo test 2>&1 | tail -20
```

Expected: all tests pass. Note the count — should be no regressions.

- [ ] **Step 5: Smoke test in dry-run**

```bash
DRY_RUN=true SIGNAL_CONFIRM_TICKS=2 TAKE_PROFIT_PCT=5.0 STOP_LOSS_PCT=3.0 \
  cargo run -- 2>&1 &
sleep 10
kill %1
ls -lh trades.db 2>/dev/null && echo "DB created" || echo "DB not yet written (no trades in 10s is OK)"
```

Expected: bot starts without panic, logs `[PREFLIGHT] Trade DB open`, creates `trades.db` if a trade was placed.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "feat(main): init Db, pass Arc<Db> to engine loop"
```

---

## Completion Checklist

- [ ] `cargo test` passes with zero failures
- [ ] `cargo check` passes with zero errors
- [ ] `trades.db` created on first run; rows have `opened_at` + `closed_at` after a full trade cycle
- [ ] TUI still renders correctly (no changes to tui/ or state.rs)
- [ ] Dry-run signal does not fire until N consecutive ticks confirm the direction

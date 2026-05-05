# Strategy Refinement — Signal Persistence, TP/SL Exit, Trade DB
**Date:** 2026-05-05
**Branch:** feature/phase-next
**Scope:** Reduce false entries via signal persistence gate; add token-price take-profit/stop-loss exit; log every trade to SQLite for learning

---

## Problem Statement

The composite score signal (alpha * btc_norm + beta * ptb_norm) fires on the first tick
that exceeds `SCORE_THRESHOLD`. BTC can spike and reverse within 200–400ms; the bot enters
on the spike and is immediately offside. High-score entries still lose because the score
reflects a single snapshot of momentum, not a sustained move. Additionally, positions are
held to slug rotation with no upside lock-in and no downside cut, meaning paper gains are
given back and small losses become large ones. There is no durable record of trade
conditions and outcomes to learn from.

---

## 1. Signal Persistence Gate

### 1.1 Mechanism

Require the same direction to exceed `SCORE_THRESHOLD` for `SIGNAL_CONFIRM_TICKS`
consecutive engine ticks before placing an entry order. At 250ms loop rate, the default of
3 ticks = 750ms sustained signal.

```
tick 1: score=0.22 Up  → streak=1, skip
tick 2: score=0.19 Up  → streak=2, skip
tick 3: score=0.24 Up  → streak=3 ✅ enter
tick 4: score=0.12     → streak=0, reset
```

If the direction flips or the score drops below threshold at any point, the streak resets
to zero. The next entry requires a fresh N-tick run.

### 1.2 Implementation

Add to `TradingEngine`:
```rust
pub signal_streak: (Option<Direction>, u8),
```

Logic in `run_engine_loop` after `compute_signal`:
- Direction matches current streak direction → increment counter
- Direction differs or signal is None → reset to (None, 0)
- Counter reaches `SIGNAL_CONFIRM_TICKS` → proceed to entry, reset counter to 0

`compute_signal` itself is unchanged — it stays pure and stateless.

### 1.3 Config

New env var: `SIGNAL_CONFIRM_TICKS` (u8, default `3`)

---

## 2. Token-Price Exit Watchdog

### 2.1 Mechanism

When a position opens, spawn a dedicated `tokio::spawn` task that polls the relevant
token's mid-price from shared state (`eth_up_price` or `eth_down_price`) every 500ms.

```
pnl_pct = (current_price - entry_price) / entry_price * 100
```

Exit triggers (evaluated in priority order):

| Condition | Action | DB `exit_reason` |
|---|---|---|
| `pnl_pct >= TAKE_PROFIT_PCT` | sell + close position | `TakeProfit` |
| `pnl_pct <= -STOP_LOSS_PCT` | sell + close position | `StopLoss` |
| slug rotates (new slug != entry slug) | close at resolution price | `Resolved` |
| `bot_status == EMERGENCY` | cancel all + close | `Emergency` |

### 2.2 Dry-Run Behaviour

Simulates proceeds using token price at the exit moment:
- `TakeProfit` / `StopLoss`: proceeds = qty_shares * exit_price
- `Resolved`: proceeds = qty_shares * 1.0 if won, 0.0 if lost (binary settlement)

Win condition for `Resolved`:
- `Up`: eth_at_close > ptb_at_fill
- `Down`: eth_at_close < ptb_at_fill

### 2.3 Live Behaviour

Calls `clob.cancel_all()` then `clob.sell_best_bid(token_id, qty)`.

### 2.4 Replacement

This watchdog **replaces** the existing slug-rotation-only dry-run watchdog in
`run_engine_loop`. All four exit paths call `Db::close_trade` before returning, so every
trade is fully recorded regardless of how it ends.

### 2.5 Config

| Env var | Type | Default | Meaning |
|---|---|---|---|
| `TAKE_PROFIT_PCT` | f64 | `8.0` | Exit when token is up 8% from entry |
| `STOP_LOSS_PCT` | f64 | `5.0` | Exit when token is down 5% from entry |

---

## 3. Trade Database (SQLite / sqlx)

### 3.1 New Module: `src/db.rs`

Wraps a `sqlx::SqlitePool`. Passed as `Arc<Db>` to `run_engine_loop`.

```rust
pub struct Db { pool: SqlitePool }

impl Db {
    pub async fn open(path: &str) -> Result<Self>
    pub async fn insert_trade(&self, entry: &TradeEntry) -> Result<i64>
    pub async fn close_trade(&self, id: i64, exit_price: f64,
                             pnl_usdc: f64, reason: &str) -> Result<()>
}
```

`insert_trade` returns the auto-incremented row `id`. This `id` is captured in the
watchdog closure so `close_trade` can match exit to entry without any in-memory map.

### 3.2 Schema (`migrations/001_trades.sql`)

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

Column guide:
- `opened_at` / `closed_at`: ISO-8601 UTC timestamps
- `side`: `'Up'` or `'Down'`
- `entry_price` / `exit_price`: Polymarket token mid-price (0.0–1.0)
- `score`: composite signal score at entry
- `btc_vel`: BTC momentum ($/s) at entry
- `ptb_pct`: `(eth_live - ptb) / ptb * 100` at entry
- `btc_price` / `eth_price`: spot prices at entry
- `confirm_ticks`: signal streak length when entry fired
- `exit_reason`: `TakeProfit` | `StopLoss` | `Resolved` | `Emergency`

### 3.3 Cargo.toml

```toml
sqlx = { version = "0.7", features = ["sqlite", "runtime-tokio-native-tls", "macros"] }
```

### 3.4 Config

New env var: `DB_PATH` (String, default `"./trades.db"`)

---

## 4. Files Changed

| File | Change |
|---|---|
| `Cargo.toml` | Add `sqlx` dep |
| `src/config.rs` | Add `signal_confirm_ticks`, `take_profit_pct`, `stop_loss_pct`, `db_path` |
| `src/engine.rs` | Add `signal_streak` field; replace slug-rotation watchdog with TP/SL watchdog; accept `Arc<Db>` param |
| `src/db.rs` | **New** — `Db`, `TradeEntry`, `open`, `insert_trade`, `close_trade` |
| `src/main.rs` | Init `Db::open`, wrap in `Arc<Db>`, pass to `run_engine_loop` |
| `migrations/001_trades.sql` | **New** — schema |

**Unchanged:** `state.rs`, `clob.rs`, `gamma.rs`, `binance.rs`, `poly_ws.rs`, `tui/`

The in-memory `trade_history` in `AppState` is kept for the TUI live panel. The SQLite DB
is the durable, queryable record for post-session analysis.

---

## 5. Error Handling

- DB write failures are logged at `WARN` level and do not block order placement or exit.
- If `Db::open` fails at startup, the bot logs `ERROR` and exits (no silent data loss).
- `close_trade` is idempotent: duplicate calls with the same `id` are a no-op (SQLite
  `UPDATE WHERE closed_at IS NULL`).

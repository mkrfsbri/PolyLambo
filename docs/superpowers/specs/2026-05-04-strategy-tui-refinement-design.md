# Strategy & TUI Refinement — Design Spec
**Date:** 2026-05-04
**Branch:** feature/phase-next
**Scope:** Composite signal strategy + TUI market tab 2-column layout + trade history box

---

## Problem Statement

The current engine never fires a trade because `state.btc.trend` is a single aggTrade
comparison that resets to NEUTRAL on every tick. The engine polls every 250ms; the latest
tick is almost always NEUTRAL, so `compute_signal` returns `None` continuously.
Separately, the price-to-beat (fetched from the Polymarket equity API) is displayed in the
TUI but ignored entirely in signal generation. The TUI market panel mixes unrelated data
in a single column with no visual hierarchy, and there is no record of past trades.

---

## 1. Strategy — Composite Score Signal

### 1.1 Signal Formula

Replace the binary `state.btc.trend` gate with a continuous composite score computed on
every engine tick (250 ms).

```
btc_norm = clamp(velocity / V_SCALE,   -1.0, 1.0)
ptb_norm = clamp(ptb_pct  / PTB_SCALE, -1.0, 1.0)

where:
  velocity = MomentumWindow::velocity()         // BTC $/s over rolling window
  ptb_pct  = (eth_live - ptb) / ptb * 100.0   // % ETH is above/below price-to-beat

score = ALPHA * btc_norm + BETA * ptb_norm

score >  SCORE_THRESHOLD  →  Direction::Up
score < -SCORE_THRESHOLD  →  Direction::Down
otherwise                 →  None
```

`eth_live` is `eth_poly_spot` (Chainlink feed via RTDS); falls back to `eth_spot_raw`
(Binance) if RTDS has not yet delivered a price.
`ptb` is `eth_open_price` (from `GET /api/equity/price-to-beat/{slug}`).
If `ptb == 0.0` the PTB term is zeroed out — BTC-only signal fires until PTB is available.

### 1.2 Edge & Kelly Sizing

```
score_edge       = score.abs() - SCORE_THRESHOLD      // excess conviction
token_mispricing = max(0.0, 0.5 - token_price)        // Poly token underpriced vs fair
edge             = score_edge + token_mispricing

size = half_kelly(edge, balance)    // 2 * edge * KELLY_FRACTION, capped at 10% balance
```

Hard gates (skip trade if any fail):
- `token_price == 0.0` — market prices not yet seeded
- `token_price > 0.65` — market already fully repriced; signal is stale
- `ptb == 0.0` is NOT a hard gate — PTB term is zeroed, BTC signal can still fire
- `size < 1.0` — position too small to be worth placing

### 1.3 compute_signal Signature Change

```rust
// Before
pub fn compute_signal(&mut self, ts_ms: u64, btc_price: f64) -> Option<Direction>

// After
pub fn compute_signal(
    &mut self,
    ts_ms: u64,
    btc_price: f64,
    eth_live: f64,
    ptb: f64,
) -> Option<(Direction, f64)>   // (direction, score)
```

The score travels with the direction into `run_engine_loop` for edge calculation and TUI display.

### 1.4 Signal Score in State

Add to `AppState`:
```rust
pub signal_score: AtomicI64,   // score * 1_000_000, signed (positive=Up signal, negative=Down)
```

Written by `run_engine_loop` after every `compute_signal` call. Read by snapshot builder
for TUI display. `AtomicI64` used because score is signed.

### 1.5 BTC Trend Field

`state.btc.trend` (AtomicU8) is retained and `binance::update_feed` continues writing it.
It is used only for TUI BULL/BEAR/NEUTRAL display. It no longer gates any trade logic.

### 1.6 Expiry Guard

Unchanged: if `time_to_expiry_secs < 90` AND `momentum.is_decaying()` → return `None`.
This wraps the new composite signal identically to the old one.

### 1.7 New Config Parameters

All added to `Config` struct and read from `.env`:

| Env var | Default | Description |
|---|---|---|
| `ALPHA` | `0.6` | Weight of BTC velocity term in score |
| `BETA` | `0.4` | Weight of PTB displacement term in score |
| `V_SCALE` | `10.0` | BTC $/s that normalises velocity to 1.0 |
| `PTB_SCALE` | `2.0` | PTB % displacement that normalises to 1.0 |
| `SCORE_THRESHOLD` | `0.15` | Minimum absolute score to open a position |

Existing params `MOMENTUM_WINDOW_SECS`, `KELLY_FRACTION`, `ORDER_TTL_SECS`,
`REVERSAL_THRESHOLD_PCT` are unchanged.

---

## 2. TUI — Market Panel (2-Column Layout)

### 2.1 Layout Change

The Market block inner area is split horizontally into three sub-areas:
`[55%, 1 char, 44%]`. The 1-char middle column renders repeated `│` to draw a vertical
divider. Left and right are `Paragraph` widgets rendered without borders.

```
┌─ Market ──────────────────────────────────┬─────────────────────────────────┐
│ BTC      $96,432.10  +0.0043%  BULL       │ UP   0.5420 / 0.5480 ↑          │
│ Binance  $2,360.25 ↑                      │ DN   0.4520 / 0.4580 ↓          │
│ Poly ETH $2,360.03 ↑                      │                                 │
│ PTB      $2,358.40   Δ +1.63  ↑           │ Inv  UP 1.250   DN 0.000 shares │
│ Score    +0.42  →  UP                     │ Expiry  03:47                   │
└───────────────────────────────────────────┴─────────────────────────────────┘
```

Height increases from 6 to 7 lines (adds Score line).

### 2.2 Score Line Rendering

```
Score    +0.42  →  UP      // green when positive, UP signal active
Score    -0.31  →  DN      // red when negative, DOWN signal active
Score     0.00  →  –       // white when below threshold
```

Color thresholds:
- `score.abs() >= SCORE_THRESHOLD` and positive → Green
- `score.abs() >= SCORE_THRESHOLD` and negative → Red
- otherwise → White

Score is read from `snap.signal_score` (f64 in snapshot, derived from `AtomicI64`).

### 2.3 Vertical Divider Implementation

```rust
let inner = block.inner(area);
let [left, mid, right] = Layout::horizontal([
    Constraint::Percentage(55),
    Constraint::Length(1),
    Constraint::Percentage(44),
]).areas(inner);

// draw divider
let divider_lines: Vec<Line> = (0..mid.height)
    .map(|_| Line::raw("│"))
    .collect();
frame.render_widget(Paragraph::new(divider_lines), mid);
```

### 2.4 Overall Vertical Layout Change

```rust
Layout::vertical([
    Constraint::Length(4),   // header (unchanged)
    Constraint::Length(7),   // market (was 6)
    Constraint::Min(5),      // active orders (unchanged)
    Constraint::Length(7),   // history (new)
    Constraint::Length(4),   // protection (unchanged)
])
```

---

## 3. TUI — Trade History Box

### 3.1 Data Model

New types in `state.rs`:

```rust
pub enum TradeStatus { Filled, Cancelled }

pub struct TradeRecord {
    pub closed_at:   chrono::DateTime<chrono::Utc>,
    pub side:        OrderSide,
    pub entry_price: f64,
    pub qty:         f64,
    pub status:      TradeStatus,
}
```

New field in `AppState`:
```rust
pub trade_history: std::sync::Mutex<std::collections::VecDeque<TradeRecord>>,
```

Initialised with capacity 50. Not `Arc`-wrapped — `AppState` is already `Arc`-wrapped.

### 3.2 Writing to History

`order_watchdog` in `engine.rs` calls a new helper `record_trade` when it closes a position:

```rust
fn record_trade(state: &AppState, record: TradeRecord) {
    let mut h = state.trade_history.lock().unwrap();
    h.push_front(record);        // newest first
    h.truncate(50);
}
```

Status determination inside `order_watchdog`:
- TTL branch → attempt `clob.cancel_order()` → always records `TradeStatus::Cancelled`
- Trend-flip branch → same
- **Note:** full fill detection (polling CLOB for fill confirmation) is deferred. All
  watchdog-closed trades record as `Cancelled` for now. A fill-status poller is a future
  enhancement (Phase 9 hardening).

In dry-run mode: `order_watchdog` is not spawned (no real orders placed), so history
accumulates simulated `Filled` records written directly in `run_engine_loop` after each
DRY_RUN signal log.

### 3.3 Snapshot

New fields in `TuiSnapshot`:
```rust
pub signal_score:     f64,           // from AtomicI64 / 1_000_000.0
pub score_threshold:  f64,           // from Config — needed by TUI to colour score line
pub history:          Vec<TradeSnap>,
```

```rust
pub struct TradeSnap {
    pub time:        String,    // "HH:MM:SS"
    pub side:        String,    // "UP" | "DN"
    pub price:       f64,
    pub qty:         f64,
    pub status:      String,    // "Filled" | "Cancelled"
}
```

`build_snapshot` reads the mutex, takes the first 5 records, converts to `TradeSnap`.

### 3.4 Rendering

```
┌─ History ────────────────────────────────────────────────────────────────────┐
│ Time       Side   Price    Qty      Status                                   │
│ 09:07:42    UP    0.5400   10.00    Filled                                   │
│ 09:03:18    DN    0.4600   15.00    Cancelled                                │
│ 08:59:55    UP    0.5210    8.00    Filled                                   │
└──────────────────────────────────────────────────────────────────────────────┘
```

Rendered as a `Table` (same component as Active Orders).
- `Filled` rows: `Color::Green`
- `Cancelled` rows: `Color::DarkGray`
- Empty state: single row "No completed trades yet" in `Color::DarkGray`

Column widths: `[10, 5, 7, 8, 10]`.

---

## 4. Files Changed

| File | Change |
|---|---|
| `src/config.rs` | Add `alpha`, `beta`, `v_scale`, `ptb_scale`, `score_threshold` fields |
| `src/state.rs` | Add `signal_score: AtomicI64`, `TradeRecord`, `TradeStatus`, `trade_history` |
| `src/engine.rs` | Refactor `compute_signal` signature + composite score; update edge calc; add `record_trade`; dry-run history write |
| `src/tui/mod.rs` | Add `signal_score`, `history` to `TuiSnapshot` and `TradeSnap` struct; update `build_snapshot` |
| `src/tui/widgets.rs` | 2-column market panel; score line; history table; updated layout constraints |
| `src/binance.rs` | No changes |
| `src/poly_ws.rs` | No changes |
| `src/gamma.rs` | No changes |
| `src/main.rs` | No changes |

---

## 5. Tests

### Modified
- `compute_signal` tests in `engine.rs`: update call sites to pass `eth_live` and `ptb`;
  update return type assertions from `Option<Direction>` to `Option<(Direction, f64)>`.

### New
- `score_above_threshold_fires_up`: velocity=$10/s, ETH +1% above PTB → score≈0.8 → Up
- `score_below_threshold_no_trade`: velocity=$1/s, ETH flat → score≈0.06 → None
- `ptb_overrides_weak_btc`: velocity=$2/s Up but ETH 2% below PTB → score negative → Down
- `ptb_zero_falls_back_to_btc`: ptb=0.0 → PTB term zeroed → BTC-only signal fires
- `trade_record_push_front`: VecDeque newest-first ordering and 50-cap truncation

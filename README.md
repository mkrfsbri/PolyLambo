# eth5m-bot

An automated Rust trading bot for Polymarket's **ETH Up/Down 5-minute prediction markets**. It exploits a lead-lag relationship between Binance BTC spot price momentum and Polymarket's binary ETH outcome tokens: BTC moves ~50–200 ms before Polymarket reprices, creating a repeating edge that the bot captures with EIP-712 signed limit orders on the Polymarket CLOB.

## Table of Contents

- [Strategy](#strategy)
- [Tech Stack](#tech-stack)
- [Prerequisites](#prerequisites)
- [Getting Started](#getting-started)
- [Environment Variables](#environment-variables)
- [Running the Bot](#running-the-bot)
- [Architecture](#architecture)
- [TUI Reference](#tui-reference)
- [Testing](#testing)
- [Project Structure](#project-structure)
- [Troubleshooting](#troubleshooting)

---

## Strategy

Every 5 minutes Polymarket publishes a new binary market: **"Will ETH be higher or lower than now in 5 minutes?"** Traders buy Up or Down outcome tokens priced between $0 and $1.

**The edge:** BTC dominates short-term crypto sentiment. When BTC makes a sharp directional move, ETH tends to follow within seconds — but Polymarket's token prices lag behind by 50–200 ms. The bot:

1. Subscribes to Binance `btcusdt@aggTrade` WebSocket and classifies each tick as BULL, BEAR, or NEUTRAL.
2. Polls the Polymarket Gamma API every 10 s to discover the next 5-minute window's token IDs and live prices.
3. When BTC trend is confirmed by a momentum window and the matching token is still below fair value (< $0.50), it places a limit buy.
4. Position size is computed via half-Kelly criterion capped at 10% of balance.
5. Each order has a hard TTL (default 3 s) and is cancelled immediately if BTC trend flips before it fills.
6. If BTC reverses ≥ 0.08% against an open position, all orders are cancelled and the bot returns to HUNTING.

---

## Tech Stack

| Layer | Library | Purpose |
|---|---|---|
| **Runtime** | `tokio 1` (full features) | Async executor for all tasks |
| **WebSocket** | `tokio-tungstenite 0.21` | Binance aggTrade stream |
| **Blockchain** | `alloy 0.3` | EIP-712 signing, `sol!` macro, Polygon chainId=137 |
| **HTTP** | `reqwest 0.12` (rustls-tls) | Gamma API + CLOB REST |
| **Serialization** | `serde 1` + `serde_json 1` | JSON parsing |
| **Concurrency** | `dashmap 5` | Lock-free order map |
| **TUI** | `ratatui 0.26` + `crossterm 0.27` | Terminal dashboard |
| **Logging** | `tracing` + `tracing-appender` | File-only logs (TUI owns stdout) |
| **Auth** | `hmac 0.12` + `sha2 0.10` + `base64 0.22` | Polymarket HMAC-SHA256 |
| **Config** | `dotenvy 0.15` | `.env` file loading |
| **Retry** | `tokio-retry 0.3` | Exponential backoff for WS reconnects |

---

## Prerequisites

- **Rust 1.75+** (2021 edition) — install via [rustup](https://rustup.rs)
- **For live trading only:**
  - Polymarket CLOB API credentials (`CLOB_API_KEY`, `CLOB_SECRET`, `CLOB_PASSPHRASE`)
  - An EVM wallet private key (`PRIVATE_KEY`) funded with USDC on Polygon
  - Your Polymarket proxy contract address (`POLYMARKET_PROXY_ADDRESS`)
- **Internet access** to `stream.binance.com` and `gamma-api.polymarket.com`

For paper trading (`DRY_RUN=true`) none of the Polymarket credentials are required.

---

## Getting Started

### 1. Clone the repository

```bash
git clone https://github.com/mkrfsbri/PolyLambo.git
cd PolyLambo
```

### 2. Create a `.env` file

Minimum viable `.env` for paper trading:

```env
DRY_RUN=true
LOG_LEVEL=info
```

Full `.env` for live trading — see [Environment Variables](#environment-variables).

### 3. Build

```bash
# Release build (recommended for live trading)
cargo build --release

# Debug build (faster compile, useful during development)
cargo build
```

### 4. Run

```bash
# Paper trading — no real orders, $1000 simulated balance
DRY_RUN=true cargo run

# Release binary
DRY_RUN=true ./target/release/eth5m-bot

# Live trading (requires credentials in .env)
cargo run --release
```

The terminal switches to a full-screen TUI dashboard. Press **`q`** to quit.

---

## Environment Variables

All variables are read at startup via `Config::from_env()`. Optional variables fall back to the listed defaults.

### Required for live trading

| Variable | Description |
|---|---|
| `PRIVATE_KEY` | EVM wallet private key (hex, with or without `0x` prefix). Used to sign EIP-712 orders on Polygon. |
| `CLOB_API_KEY` | Polymarket CLOB API key. |
| `CLOB_SECRET` | CLOB secret for HMAC-SHA256 request signing. |
| `CLOB_PASSPHRASE` | CLOB passphrase. |
| `POLYMARKET_PROXY_ADDRESS` | Your Polymarket proxy contract address (the `maker` field on all orders). |

### Optional / tuning

| Variable | Default | Description |
|---|---|---|
| `DRY_RUN` | `false` | Set to `true` to skip all real HTTP calls. Simulates $1000 balance and logs signals instead of placing orders. |
| `LOG_LEVEL` | `info` | Tracing filter: `trace`, `debug`, `info`, `warn`, or `error`. Logs go to `./logs/eth5m-bot-YYYY-MM-DD.log` only. |
| `REVERSAL_THRESHOLD_PCT` | `0.08` | BTC price move (%) against our position that triggers an emergency flip. Warning fires at 60% of this value (0.048%). |
| `MOMENTUM_WINDOW_SECS` | `15` | Rolling window for the momentum velocity calculation (seconds). |
| `ORDER_TTL_SECS` | `3` | Hard timeout per order. The watchdog cancels automatically after this many seconds regardless of fill status. |
| `KELLY_FRACTION` | `0.5` | Fraction of full-Kelly applied to position sizing. `0.5` = half-Kelly. Position is additionally capped at 10% of balance. |

### Full `.env` example

```env
PRIVATE_KEY=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
CLOB_API_KEY=your-api-key
CLOB_SECRET=your-secret
CLOB_PASSPHRASE=your-passphrase
POLYMARKET_PROXY_ADDRESS=0xYourProxyAddress

DRY_RUN=false
LOG_LEVEL=info
REVERSAL_THRESHOLD_PCT=0.08
MOMENTUM_WINDOW_SECS=15
ORDER_TTL_SECS=3
KELLY_FRACTION=0.5
```

---

## Running the Bot

### Paper trading (DRY_RUN)

`DRY_RUN=true` is the safest way to verify the bot is working:

- CLOB client uses a hardcoded Anvil test key — no real signatures or HTTP calls
- Balance is simulated at $1000
- Gamma pre-flight check is skipped
- Engine loop logs `[ENGINE] DRY_RUN signal=Up size=$12.50 expiry=187s` instead of placing orders
- All TUI panels update normally

```bash
DRY_RUN=true cargo run
```

### Live trading

Before going live:

1. Fund your Polymarket proxy address with USDC on Polygon.
2. Obtain CLOB credentials from your Polymarket account settings.
3. Set all required env vars in `.env`.
4. Run — the bot performs these pre-flight checks at startup and aborts with a clear error if any fail:
   - CLOB `/ok` health check
   - Balance fetch (must be > $1)
   - BTC WebSocket connection (must receive a tick within 5 s)
   - Gamma slug reachability

### Graceful shutdown

Press `Ctrl+C`. The bot cancels all open orders on the CLOB (`DELETE /orders`) before exiting.

### Logs

Logs are written to `./logs/eth5m-bot-YYYY-MM-DD.log` and **never** to stdout (the TUI owns the terminal). Follow them in a separate terminal:

```bash
tail -f logs/eth5m-bot-$(date -u +%Y-%m-%d).log

# Strategy events only
tail -f logs/eth5m-bot-$(date -u +%Y-%m-%d).log | grep -E "ENGINE|GAMMA|BINANCE.*Connected|PREFLIGHT"
```

| Level | Content |
|---|---|
| `TRACE` | Per-tick BTC price updates, ws_recv→store latency |
| `DEBUG` | Per-tick order decisions, edge calculations |
| `INFO` | Strategy events: connected, position opened/closed, slug discovered |
| `WARN` | Reversals, rate-limits (429), reconnects, latency budget exceeded |
| `ERROR` | Crashes, EMERGENCY halt, 401 auth failures |

---

## Architecture

The bot is a single Rust binary with **five concurrent actors** sharing an `Arc<AppState>`:

```
┌─────────────────────────────────────────────────────────┐
│                       main.rs                           │
│  pre-flight → spawn tasks → TUI watch loop → ctrl-c    │
└──────┬──────────┬────────────┬──────────────┬──────────┘
       │          │            │              │
  BTC Feed   Gamma Disc.  Engine Loop    Balance Mon.
  (async)     (async)      (async)         (async)
       │          │            │
       └──────────┴────────────┘
                  │
           Arc<AppState>
           (lock-free hot path)
                  │
              TUI Thread
             (std::thread)
            watch::Receiver
```

### Actor 1 — BTC Feed (`binance.rs`)

- Long-lived WebSocket to `wss://stream.binance.com:9443/ws/btcusdt@aggTrade`
- Parses each `aggTrade` message and writes price + trend atomically into `AppState.btc`
- Trend classification: price change > +0.001% → BULL, < −0.001% → BEAR, otherwise NEUTRAL
- Measures `ws_recv → store` latency; logs a warning if > 100 μs
- Reconnects with exponential backoff (base 1 s, max 30 s, 10 attempts)

### Actor 2 — Gamma Discovery (`gamma.rs`)

- Polls `gamma-api.polymarket.com/events?slug=eth-updown-5m-{next_boundary}` every 10 s
- **Slug algorithm:** `((unix_ts / 300) + 1) * 300` — always the strictly next 5-minute boundary, even when called exactly on a boundary
- Extracts `clobTokenIds[0]` (Up token) and `clobTokenIds[1]` (Down token) plus live outcome prices
- Handles both native JSON array and JSON-string-encoded `clobTokenIds` (both forms appear in production)
- Updates `RwLock<String>` slug/token fields (written rarely, only on slug change) and `AtomicU64` price fields

### Actor 3 — Trading Engine (`engine.rs`)

- 250 ms tick loop
- **`MomentumWindow`:** rolling VecDeque of `(unix_ms, price)` pairs, auto-prunes entries outside the configured window. Computes signed velocity ($/s) and detects decaying momentum via three consecutive decreasing per-tick velocities.
- **`check_reversal`:** if BTC moved > 0.08% against our entry direction → `EmergencyFlip`; if > 0.048% (60% of threshold) → `Warning`
- **`compute_signal`:** returns `Some(Direction::Up/Down)` based on BTC trend; suppresses signals within 90 s of market expiry when momentum is decaying
- **Edge calculation:**
  ```
  edge = max(0, 0.5 − token_price) + 0.02  (lead-lag floor)
  ```
  Stale signal guard: if token already > $0.65, edge = 0 → skip entry
- **Position sizing (half-Kelly, capped at 10% of balance):**
  ```
  size = 2 × edge × kelly_fraction × balance
  ```
- **Per-order watchdog** (`tokio::select!`): cancels on TTL expiry OR BTC trend flip, whichever comes first

### Actor 4 — CLOB Client (`clob.rs`)

- Signs orders with EIP-712 using the `alloy` `sol!` macro (Polygon chainId=137)
- Authenticates REST calls with HMAC-SHA256: `sig = HMAC(secret, timestamp + method + path + body)`
- HTTP retry policy: 429 → wait 1 s and retry; 401 → wait 500 ms, regenerate headers with fresh timestamp, retry once, then halt
- `ClobClient::new_dry_run()` uses a hardcoded Anvil test key and skips all real HTTP — safe for testing
- `ClobClient` is always `Arc`-wrapped; shared across the engine loop and the main graceful-shutdown path

### Actor 5 — TUI (`tui/`)

- Runs on `std::thread::spawn` (blocking — **never** `tokio::spawn`)
- Receives `TuiSnapshot` via `tokio::sync::watch` channel pushed by main at 100 ms intervals
- Zero `Arc`/`Atomic` access inside the render path — all data is copied into `TuiSnapshot` before the send
- Press `q` to exit

### State Design (`state.rs`)

`AppState` is designed for a lock-free hot path:

| Field | Type | Encoding |
|---|---|---|
| BTC price / prev price | `AtomicU64` | `price × 1_000_000` (6-decimal fixed-point) |
| BTC trend | `AtomicU8` | 0=NEUTRAL, 1=BULL, 2=BEAR |
| ETH Up/Down prices | `AtomicU64` | `price × 1_000_000` |
| Balance / PnL | `AtomicI64` | USDC cents (signed) |
| Bot status | `AtomicU8` | 0=HUNTING, 1=POSITION, 2=REVERSAL, 3=EMERGENCY |
| Active orders | `Arc<DashMap>` | Lock-free concurrent map |
| Slug / token IDs | `RwLock<String>` | Written only on slug change (~every 5 min) |

Fixed-point helpers `f64_to_atomic` / `atomic_to_f64` are in `state.rs` and used throughout.

---

## TUI Reference

The terminal dashboard has four panels, refreshed every 100 ms:

```
┌─ eth5m-bot ──────────────────────────────────────────────────────────┐
│ MARKET: eth-updown-5m-1746000300   STATUS: HUNTING                   │
│ BALANCE: $987.34   PNL: +12.66   CLOB: 42ms   WS: 18μs              │
└──────────────────────────────────────────────────────────────────────┘
┌─ Market ─────────────────────────────────────────────────────────────┐
│ BTC  : $ 65432.10  (+0.0023%)  BULL                                  │
│ ETH5m: UP $0.5050  |  DOWN $0.4950    02:47 remaining               │
│ INV  : UP 0.000 shares  |  DOWN 0.000 shares                         │
└──────────────────────────────────────────────────────────────────────┘
┌─ Active Orders ──────────────────────────────────────────────────────┐
│ ID (trunc)   Side  Price  Qty     Status     TTL                     │
│ 3f8a1c2d4e5f UP    0.505  $12.50  Pending    3s                      │
└──────────────────────────────────────────────────────────────────────┘
┌─ Protection ─────────────────────────────────────────────────────────┐
│ Reversal: Normal                                                      │
│ Momentum: OK                                                          │
└──────────────────────────────────────────────────────────────────────┘
```

| Panel | Fields |
|---|---|
| **Header** | Current market slug, bot status (color-coded), USDC balance, cumulative PnL, CLOB API latency (ms), Binance WS latency (μs) |
| **Market** | Live BTC price + % change vs. previous tick + trend, ETH Up/Down token prices, time to market expiry (MM:SS), inventory in shares |
| **Active Orders** | Truncated order ID, direction, limit price, size, status, TTL countdown (white → yellow at ≤ 2 s → red at ≤ 1 s) |
| **Protection** | Reversal deviation (yellow = warning with % shown), momentum decay flag |

**Bot status colors:**

| Color | Status | Meaning |
|---|---|---|
| Cyan | HUNTING | No position, scanning for signal |
| Green | POSITION | Order placed, waiting to fill |
| Yellow | REVERSAL | BTC moving against position (warning threshold crossed) |
| Red | EMERGENCY | Reversal threshold breached — all orders cancelled |

---

## Testing

### Run all tests

```bash
cargo test
```

Expected: **64 tests pass**, 1 ignored (live network test).

### Test breakdown

| Test file | Count | What it covers |
|---|---|---|
| `src/state.rs` (inline) | 6 | Fixed-point roundtrip, AtomicU64 price/trend, DashMap orders |
| `src/binance.rs` (inline) | 7 + 1 ignored | Feed updates, trend classification, invalid price guard, timestamp tracking |
| `src/engine.rs` (inline) | 15 | MomentumWindow velocity, decay detection, reversal check, half-Kelly sizing, signal suppression |
| `src/gamma.rs` (inline) | 9 | Slug generation (boundary, mid-window, off-by-one), expiry countdown, JSON parsing (both array and string-encoded forms) |
| `tests/clob_mock_test.rs` | 4 | HMAC header format, signature uniqueness, dry-run order/cancel |
| `tests/engine_test.rs` | 8 | Integration: Kelly, momentum decay, reversal, signal direction |
| `tests/gamma_test.rs` | 9 | Slug and expiry functions as black-box |

### Run a specific test

```bash
cargo test test_slug_boundary_edge
cargo test momentum_decay
cargo test reversal
```

### Run the live network test (manual only)

The Binance WebSocket test is `#[ignore]` by default:

```bash
cargo test -- --ignored
```

This connects to Binance, waits 5 s for a live price tick, and asserts the price is > $1000.

---

## Project Structure

```
.
├── Cargo.toml              # Package: eth5m-bot (lib + bin, edition 2021)
├── Cargo.lock
├── .env                    # Not committed — create from the example above
├── .gitignore
├── LICENSE
├── logs/                   # Created at runtime
│   └── eth5m-bot-YYYY-MM-DD.log
├── src/
│   ├── lib.rs              # Re-exports all modules
│   ├── main.rs             # Orchestration: pre-flight, spawn tasks, TUI loop, graceful shutdown
│   ├── config.rs           # Config::from_env() — reads all env vars with defaults
│   ├── state.rs            # AppState (Arc), BtcFeed, AtomicU64 fixed-point helpers, order types
│   ├── binance.rs          # WebSocket BTC feed: run_btc_feed(), get_btc_price()
│   ├── gamma.rs            # Polymarket market discovery: compute_next_slug(), discover_and_update()
│   ├── engine.rs           # TradingEngine, MomentumWindow, run_engine_loop(), order_watchdog()
│   ├── clob.rs             # ClobClient — EIP-712 signing, HMAC auth, REST order API
│   └── tui/
│       ├── mod.rs          # run_tui() (blocking thread), TuiSnapshot, watch channel
│       └── widgets.rs      # ratatui render functions for all 4 panels
└── tests/
    ├── clob_mock_test.rs   # HMAC + dry-run order/cancel integration tests
    ├── engine_test.rs      # Engine integration tests (Kelly, reversal, signal)
    └── gamma_test.rs       # Slug + expiry black-box tests
```

---

## Troubleshooting

### `BTC feed not connected within 5 s`

The bot aborts at startup if no Binance tick arrives within 5 s.

- Check internet connectivity to `stream.binance.com:9443`
- Binance may be rate-limiting your IP — wait a minute and retry
- Set `LOG_LEVEL=debug` to see the raw connection error in the log

### `balance $X.XX < $1 — abort`

Pre-flight balance check failed.

- Ensure your Polymarket proxy address is funded with USDC on Polygon
- Confirm `POLYMARKET_PROXY_ADDRESS` is the proxy contract address, not your EOA wallet
- In dry-run mode this check is skipped entirely; balance starts at $1000

### `CLOB 401 Unauthorized`

Authentication failed after one retry attempt.

- Verify `CLOB_API_KEY`, `CLOB_SECRET`, and `CLOB_PASSPHRASE` match your Polymarket account
- The HMAC signature includes a unix timestamp — ensure system clock is accurate (`timedatectl` on Linux)
- Confirm `POLYMARKET_PROXY_ADDRESS` matches the address your API key is registered to

### `gamma: slug not found: eth-updown-5m-XXXXXXXXX`

The next 5-minute market hasn't been listed on Polymarket yet.

- Normal behaviour — Gamma listings appear a few seconds before the window boundary. The discovery loop retries every 10 s and resolves automatically.
- In dry-run mode the pre-flight slug check is skipped.

### Orders not filling

- The limit price is derived from the live token price at signal time. If the market moved by the time the order reaches the book, it will sit unmatched until the TTL cancels it.
- Reduce `ORDER_TTL_SECS` for faster cancellations, or increase it to allow more time to fill.
- Watch the TUI's CLOB latency field — if consistently > 200 ms the signal may be stale on arrival.

### TUI blank or garbled

- The TUI requires a real TTY. It silently degrades in non-TTY environments (piped output, some CI systems). Logs still write to `./logs/` regardless.
- If the terminal window is too narrow, ratatui may clip widgets. Try a wider terminal or resize.

### Build fails with linker errors

Ensure you have a working C linker and TLS headers:

```bash
# Debian / Ubuntu
sudo apt-get install build-essential pkg-config libssl-dev

# macOS
xcode-select --install
```

---

## License

See [LICENSE](LICENSE).

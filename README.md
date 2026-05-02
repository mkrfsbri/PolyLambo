# eth5m-bot

Low-latency lead-lag trading bot for Polymarket ETH 5-minute Up/Down markets.

**Strategy**: BTC price moves ~50–200ms before Polymarket reprices ETH directional outcomes. The bot reads Binance aggTrade ticks in real time, detects a BTC trend, and places limit orders on the corresponding ETH outcome token before the market catches up.

---

## Prerequisites

- Rust toolchain (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
- For **live trading** only: Polymarket CLOB API credentials and a funded Polygon wallet

---

## Quick start — paper trading

```bash
cd .worktrees/phase-0-scaffold

# Build
cargo build --release

# Run with TUI dashboard (paper trading, no real orders)
DRY_RUN=true ./target/release/eth5m-bot
```

The terminal switches to a full-screen dashboard. Press **`q`** to quit.

---

## TUI dashboard

```
┌─ eth5m-bot ──────────────────────────────────────────┐
│ MARKET: eth-updown-5m-1777739100   STATUS: HUNTING   │
│ BALANCE: $1000.00   PNL: +$0.00   CLOB: 0ms  WS: 87μs│
├─ Market ─────────────────────────────────────────────┤
│ BTC  :  $78549.75  (+0.0012%)  BULL                  │
│ ETH5m: UP $0.5150  |  DOWN $0.4850    04:32 remaining│
│ INV  : UP 0.000 shares  |  DOWN 0.000 shares         │
├─ Active Orders ───────────────────────────────────────┤
│ ID (trunc)  │Side│Price │  Qty  │Status    │TTL       │
├─ Protection ──────────────────────────────────────────┤
│ Reversal: Normal                                      │
│ Momentum: OK                                         │
└──────────────────────────────────────────────────────┘
```

| Status colour | Meaning |
|---|---|
| Cyan — HUNTING | Looking for a signal |
| Green — POSITION | Order placed, waiting to fill |
| Yellow — REVERSAL | BTC moved against position (warning) |
| Red — EMERGENCY | Threshold breached, all orders cancelled |

---

## Configuration (`.env`)

Copy and fill in before live trading:

```env
# Required for live trading
PRIVATE_KEY=0x...
CLOB_API_KEY=
CLOB_SECRET=
CLOB_PASSPHRASE=
POLYMARKET_PROXY_ADDRESS=0x...

# Optional tuning (defaults shown)
DRY_RUN=false
LOG_LEVEL=info
REVERSAL_THRESHOLD_PCT=0.08   # 0.08% BTC adverse move → flip
MOMENTUM_WINDOW_SECS=15       # rolling velocity window
ORDER_TTL_SECS=3              # cancel unfilled orders after 3s
KELLY_FRACTION=0.5            # half-Kelly position sizing
```

---

## Live trading

```bash
# Create .env with your credentials, then:
./target/release/eth5m-bot
```

Pre-flight checks on startup:
1. CLOB `/ok` reachable
2. Balance > $1
3. BTC feed connected (≤ 5s)
4. Gamma slug resolvable

All checks must pass or the bot aborts before placing any order.

---

## Logs

Logs are written to `./logs/eth5m-bot-YYYY-MM-DD.log` (stdout is reserved for the TUI).

```bash
# Follow live in a separate terminal
tail -f logs/eth5m-bot-$(date -u +%Y-%m-%d).log

# Filter to strategy events only
tail -f logs/eth5m-bot-$(date -u +%Y-%m-%d).log | grep -E "ENGINE|GAMMA|BINANCE.*Connected|PREFLIGHT"
```

Log levels: `TRACE`=per-tick prices · `DEBUG`=order decisions · `INFO`=strategy events · `WARN`=reversals · `ERROR`=crashes

---

## Edge & position sizing

Entry requires a BTC directional trend. Position size is computed via half-Kelly:

```
edge  = max(0.5 − token_price, 0)  +  0.02   (lead-lag floor)
size  = 2 × edge × kelly_fraction × balance   (capped at 10% of balance)
```

The bot skips entry when `token_price ≥ 0.65` (market fully repriced, signal stale).

---

## Development

```bash
cargo check                   # fast type check
cargo test                    # run all 64 tests
cargo test <name>             # single test by substring
cargo test -- --ignored       # live network tests (requires internet)
```

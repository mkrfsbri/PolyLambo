use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::stdout;
use std::time::Duration;
use tokio::sync::watch;

pub mod widgets;

// ── snapshot ──────────────────────────────────────────────────────────────────

/// All data the TUI thread needs — no Arc/Atomic access from the rendering path.
#[derive(Clone, Default)]
pub struct TuiSnapshot {
    pub slug: String,
    /// Human-readable market question text
    pub question: String,
    pub bot_status: u8,
    pub balance_usdc: f64,
    pub pnl_usdc: f64,
    pub api_latency_ms: u64,
    pub ws_latency_us: u64,

    pub btc_price: f64,
    pub btc_prev_price: f64,
    pub btc_trend: u8,

    /// ETH/USD spot price from Binance
    pub eth_spot_price: f64,
    pub eth_spot_prev: f64,
    /// ETH/USD price when this 5-min window opened ("price to beat")
    pub eth_open_price: f64,
    /// Polymarket live ETH/USD price from RTDS feed
    pub eth_poly_spot: f64,
    pub eth_poly_spot_prev: f64,

    /// Polymarket UP token mid price (0–1)
    pub eth_up_price: f64,
    pub eth_up_prev: f64,
    /// UP token best bid from CLOB orderbook
    pub eth_up_bid: f64,
    /// UP token best ask from CLOB orderbook
    pub eth_up_ask: f64,
    /// Polymarket DOWN token mid price (0–1)
    pub eth_down_price: f64,
    pub eth_down_prev: f64,
    /// DOWN token best bid from CLOB orderbook
    pub eth_down_bid: f64,
    /// DOWN token best ask from CLOB orderbook
    pub eth_down_ask: f64,

    pub time_to_expiry_secs: u64,

    /// inventory_up * 1000 fixed-point
    pub inventory_up: i64,
    /// inventory_down * 1000 fixed-point
    pub inventory_down: i64,

    pub orders: Vec<OrderSnap>,
    pub reversal_warning: Option<f64>,
    pub momentum_decaying: bool,
    /// Composite signal score from engine (-1..+1 range)
    pub signal_score: f64,
    /// Score threshold from Config — used by widgets for colour cutoff
    pub score_threshold: f64,
    /// Last 5 completed trades, newest first
    pub history: Vec<TradeSnap>,
    /// Present only when bot_status == POSITION
    pub active_position: Option<PositionSnap>,
}

#[derive(Clone, Default)]
pub struct OrderSnap {
    pub order_id: String,
    pub side: String,
    pub price: f64,
    pub qty: f64,
    pub status: String,
    /// u64::MAX = unknown/not applicable
    pub ttl_secs: u64,
}

#[derive(Clone, Default)]
pub struct TradeSnap {
    pub time:        String,   // "HH:MM:SS"
    pub side:        String,   // "UP" | "DN"
    pub entry_price: f64,
    pub exit_price:  f64,
    pub pnl_usdc:    f64,
    /// "TakeProfit" | "StopLoss" | "Resolved" | "Emergency"
    pub exit_reason: String,
}

#[derive(Clone, Default)]
pub struct PositionSnap {
    pub side:           String,  // "UP" | "DN"
    pub entry_price:    f64,     // token entry price (0–1)
    pub current_price:  f64,     // current token mid price (0–1)
    pub qty:            f64,     // shares (tokens held = size_usdc / entry_price)
    pub elapsed_secs:   u64,
    pub unrealized_pnl: f64,     // qty_shares * (current_price - entry_price)
}

// ── TUI entry point ───────────────────────────────────────────────────────────

/// Blocking — must be called from std::thread::spawn, NOT tokio::spawn.
/// Exits when the user presses 'q' or the watch sender is dropped.
pub fn run_tui(rx: watch::Receiver<TuiSnapshot>) {
    if enable_raw_mode().is_err() {
        return;
    }
    let mut out = stdout();
    if execute!(out, EnterAlternateScreen).is_err() {
        let _ = disable_raw_mode();
        return;
    }
    let backend = CrosstermBackend::new(out);
    let mut terminal = match Terminal::new(backend) {
        Ok(t) => t,
        Err(_) => {
            let _ = disable_raw_mode();
            return;
        }
    };

    loop {
        let snap = rx.borrow().clone();
        let _ = terminal.draw(|f| widgets::render(f, &snap));

        // 100 ms poll — keeps the screen fresh and responsive
        match event::poll(Duration::from_millis(100)) {
            Ok(true) => {
                if let Ok(Event::Key(k)) = event::read() {
                    if k.kind == KeyEventKind::Press && k.code == KeyCode::Char('q') {
                        break;
                    }
                }
            }
            Err(_) => break,
            _ => {}
        }

        // Exit when the sender side (main) is gone
        if rx.has_changed().is_err() {
            break;
        }
    }

    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
}

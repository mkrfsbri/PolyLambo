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
    pub bot_status: u8,
    pub balance_usdc: f64,
    pub pnl_usdc: f64,
    pub api_latency_ms: u64,

    pub btc_price: f64,
    pub btc_prev_price: f64,
    pub btc_trend: u8,

    pub eth_up_price: f64,
    pub eth_down_price: f64,
    pub time_to_expiry_secs: u64,

    /// inventory_up * 1000 fixed-point
    pub inventory_up: i64,
    /// inventory_down * 1000 fixed-point
    pub inventory_down: i64,

    pub orders: Vec<OrderSnap>,
    pub reversal_warning: Option<f64>,
    pub momentum_decaying: bool,
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

// ── TUI entry point ───────────────────────────────────────────────────────────

/// Blocking — must be called from std::thread::spawn, NOT tokio::spawn.
/// Exits when the user presses 'q' or the watch sender is dropped.
pub fn run_tui(mut rx: watch::Receiver<TuiSnapshot>) {
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

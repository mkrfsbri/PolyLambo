use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, Row, Table},
};

use crate::state::{bot_status, trend};
use super::{OrderSnap, TuiSnapshot};

// ── top-level render ──────────────────────────────────────────────────────────

pub fn render(frame: &mut Frame, snap: &TuiSnapshot) {
    let area = frame.size();
    let chunks = Layout::vertical([
        Constraint::Length(4), // header: slug, status, balance, pnl, latency
        Constraint::Length(6), // market: BTC, ETH spot, POLY prices, inventory
        Constraint::Min(5),    // active orders table
        Constraint::Length(4), // protection status
    ])
    .split(area);

    render_header(frame, chunks[0], snap);
    render_market(frame, chunks[1], snap);
    render_orders(frame, chunks[2], &snap.orders);
    render_protection(frame, chunks[3], snap);
}

// ── sections ──────────────────────────────────────────────────────────────────

fn render_header(frame: &mut Frame, area: Rect, snap: &TuiSnapshot) {
    let s_color = status_color(snap.bot_status);
    let pnl_sign = if snap.pnl_usdc >= 0.0 { "+" } else { "" };

    let text = vec![
        Line::from(vec![
            Span::raw("MARKET: "),
            Span::styled(
                if !snap.question.is_empty() {
                    snap.question.clone()
                } else if !snap.slug.is_empty() {
                    snap.slug.clone()
                } else {
                    "–".into()
                },
                Style::default().fg(Color::White).bold(),
            ),
            Span::raw("   STATUS: "),
            Span::styled(
                status_str(snap.bot_status),
                Style::default().fg(s_color).bold(),
            ),
        ]),
        {
            let clob_lat = if snap.api_latency_ms == 0 {
                "--".to_string()
            } else {
                format!("{}ms", snap.api_latency_ms)
            };
            Line::from(format!(
                "BALANCE: ${:.2}   PNL: {}{:.2}   CLOB: {}   WS: {}μs",
                snap.balance_usdc, pnl_sign, snap.pnl_usdc,
                clob_lat, snap.ws_latency_us
            ))
        },
    ];

    let block = Block::default().borders(Borders::ALL).title(" eth5m-bot ");
    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn render_market(frame: &mut Frame, area: Rect, snap: &TuiSnapshot) {
    let btc_pct = if snap.btc_prev_price > 0.0 {
        (snap.btc_price - snap.btc_prev_price) / snap.btc_prev_price * 100.0
    } else {
        0.0
    };
    let (trend_label, trend_color) = match snap.btc_trend {
        t if t == trend::BULL => ("BULL", Color::Green),
        t if t == trend::BEAR => ("BEAR", Color::Red),
        _ => ("NEUTRAL", Color::White),
    };

    let expiry_mm = snap.time_to_expiry_secs / 60;
    let expiry_ss = snap.time_to_expiry_secs % 60;
    let inv_up   = snap.inventory_up   as f64 / 1000.0;
    let inv_down = snap.inventory_down as f64 / 1000.0;

    // ETH Binance live price arrow
    let (eth_arrow, eth_color) = price_arrow(snap.eth_spot_price, snap.eth_spot_prev);

    // ETH Poly reference / price-to-beat
    let (ref_label, ref_color) = if snap.eth_open_price > 0.0 {
        let diff = snap.eth_spot_price - snap.eth_open_price;
        let sign = if diff >= 0.0 { "+" } else { "" };
        let (_, col) = price_arrow(snap.eth_spot_price, snap.eth_open_price);
        (format!("${:.2}  ({}{:.2})", snap.eth_open_price, sign, diff), col)
    } else {
        ("–".to_string(), Color::White)
    };

    // Polymarket UP/DOWN arrows (based on mid price movement)
    let (up_arrow, up_color)     = price_arrow(snap.eth_up_price, snap.eth_up_prev);
    let (down_arrow, down_color) = price_arrow(snap.eth_down_price, snap.eth_down_prev);

    // UP bid/ask display — show "--" when orderbook not yet populated
    let up_book = if snap.eth_up_bid > 0.0 && snap.eth_up_ask > 0.0 {
        format!("{:.4}/{:.4}", snap.eth_up_bid, snap.eth_up_ask)
    } else if snap.eth_up_price > 0.0 {
        format!("{:.4} (mid)", snap.eth_up_price)
    } else {
        "--".to_string()
    };
    let dn_book = if snap.eth_down_bid > 0.0 && snap.eth_down_ask > 0.0 {
        format!("{:.4}/{:.4}", snap.eth_down_bid, snap.eth_down_ask)
    } else if snap.eth_down_price > 0.0 {
        format!("{:.4} (mid)", snap.eth_down_price)
    } else {
        "--".to_string()
    };

    // POLY ETH = Polymarket live ETH price from RTDS.
    // Falls back to the Binance feed when RTDS hasn't sent a price yet.
    // This is intentionally separate from eth_open_price ("price to beat"),
    // which is fixed at window-open and used as the static reference.
    let (poly_eth_display, poly_eth_prev) = if snap.eth_poly_spot > 0.0 {
        (snap.eth_poly_spot, snap.eth_poly_spot_prev)
    } else {
        (snap.eth_spot_price, snap.eth_spot_prev)
    };
    let (poly_eth_arrow, poly_eth_col) = price_arrow(poly_eth_display, poly_eth_prev);
    let (poly_eth_label, poly_eth_color) = if poly_eth_display > 0.0 {
        (format!("${:.2}{}", poly_eth_display, poly_eth_arrow), poly_eth_col)
    } else {
        ("–".to_string(), Color::White)
    };

    let text = vec![
        // BTC row
        Line::from(vec![
            Span::raw(format!("BTC: ${:>10.2}  ({:+.4}%)  ", snap.btc_price, btc_pct)),
            Span::styled(trend_label, Style::default().fg(trend_color).bold()),
        ]),
        // ETH Binance live + Poly reference price-to-beat
        Line::from(vec![
            Span::raw("ETH Binance: "),
            Span::styled(
                format!("${:.2}", snap.eth_spot_price),
                Style::default().fg(eth_color).bold(),
            ),
            Span::styled(eth_arrow, Style::default().fg(eth_color)),
            Span::raw("   Price to beat: "),
            Span::styled(ref_label, Style::default().fg(ref_color).bold()),
        ]),
        // Polymarket orderbook bid/ask + Polymarket ETH reference price
        Line::from(vec![
            Span::raw("POLY UP (bid/ask): "),
            Span::styled(up_book, Style::default().fg(up_color).bold()),
            Span::styled(up_arrow, Style::default().fg(up_color)),
            Span::raw("  DN: "),
            Span::styled(dn_book, Style::default().fg(down_color).bold()),
            Span::styled(down_arrow, Style::default().fg(down_color)),
            Span::raw("   POLY ETH: "),
            Span::styled(poly_eth_label, Style::default().fg(poly_eth_color).bold()),
            Span::raw(format!("    {:02}:{:02} remaining", expiry_mm, expiry_ss)),
        ]),
        // Inventory
        Line::from(format!(
            "INV: UP {:.3} shares  |  DOWN {:.3} shares",
            inv_up, inv_down
        )),
    ];

    let block = Block::default().borders(Borders::ALL).title(" Market ");
    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn render_orders(frame: &mut Frame, area: Rect, orders: &[OrderSnap]) {
    let header = Row::new(["ID (trunc)", "Side", "Price", "Qty", "Status", "TTL"])
        .style(Style::default().bold().fg(Color::Yellow));

    let rows: Vec<Row> = orders.iter().map(|o| {
        let id_short: String = o.order_id.chars().take(12).collect();
        let ttl_display = if o.ttl_secs == u64::MAX {
            "–".to_string()
        } else {
            format!("{}s", o.ttl_secs)
        };
        let ttl_color = ttl_color(o.ttl_secs);
        Row::new([
            id_short,
            o.side.clone(),
            format!("{:.3}", o.price),
            format!("{:.2}", o.qty),
            o.status.clone(),
            ttl_display,
        ])
        .style(Style::default().fg(ttl_color))
    })
    .collect();

    let widths = [
        Constraint::Length(13),
        Constraint::Length(5),
        Constraint::Length(6),
        Constraint::Length(7),
        Constraint::Length(10),
        Constraint::Length(5),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(" Active Orders "));
    frame.render_widget(table, area);
}

fn render_protection(frame: &mut Frame, area: Rect, snap: &TuiSnapshot) {
    let rev_line = match snap.reversal_warning {
        Some(pct) => Line::from(vec![
            Span::raw("Reversal: "),
            Span::styled(
                format!("WARNING  {pct:.3}% deviation"),
                Style::default().fg(Color::Yellow).bold(),
            ),
        ]),
        None => Line::from(vec![
            Span::raw("Reversal: "),
            Span::styled("Normal", Style::default().fg(Color::Green)),
        ]),
    };

    let mom_line = if snap.momentum_decaying {
        Line::from(vec![
            Span::raw("Momentum: "),
            Span::styled(
                "DECAYING — signals suppressed near expiry",
                Style::default().fg(Color::Yellow),
            ),
        ])
    } else {
        Line::from(vec![
            Span::raw("Momentum: "),
            Span::styled("OK", Style::default().fg(Color::Green)),
        ])
    };

    let block = Block::default().borders(Borders::ALL).title(" Protection ");
    frame.render_widget(
        Paragraph::new(vec![rev_line, mom_line]).block(block),
        area,
    );
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn status_color(s: u8) -> Color {
    match s {
        bot_status::HUNTING   => Color::Cyan,
        bot_status::POSITION  => Color::Green,
        bot_status::REVERSAL  => Color::Yellow,
        bot_status::EMERGENCY => Color::Red,
        _ => Color::White,
    }
}

fn status_str(s: u8) -> &'static str {
    match s {
        bot_status::HUNTING   => "HUNTING",
        bot_status::POSITION  => "POSITION",
        bot_status::REVERSAL  => "REVERSAL",
        bot_status::EMERGENCY => "EMERGENCY",
        _ => "UNKNOWN",
    }
}

/// Returns (arrow_str, color) based on whether current > prev.
/// Returns ("", White) when prev is zero (no prior data yet).
fn price_arrow(current: f64, prev: f64) -> (&'static str, Color) {
    if prev <= 0.0 || current <= 0.0 {
        return ("", Color::White);
    }
    if current > prev {
        ("↑", Color::Green)
    } else if current < prev {
        ("↓", Color::Red)
    } else {
        ("", Color::White)
    }
}

fn ttl_color(secs: u64) -> Color {
    if secs == u64::MAX {
        Color::White
    } else if secs < 1 {
        Color::Red
    } else if secs <= 2 {
        Color::Yellow
    } else {
        Color::White
    }
}

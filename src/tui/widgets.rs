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
        Constraint::Length(5), // market: BTC, ETH5m prices, inventory
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
                if snap.slug.is_empty() { "–".into() } else { snap.slug.clone() },
                Style::default().fg(Color::White).bold(),
            ),
            Span::raw("   STATUS: "),
            Span::styled(
                status_str(snap.bot_status),
                Style::default().fg(s_color).bold(),
            ),
        ]),
        Line::from(format!(
            "BALANCE: ${:.2}   PNL: {}{:.2}   CLOB: {}ms   WS: {}μs",
            snap.balance_usdc, pnl_sign, snap.pnl_usdc,
            snap.api_latency_ms, snap.ws_latency_us
        )),
    ];

    let block = Block::default().borders(Borders::ALL).title(" eth5m-bot ");
    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn render_market(frame: &mut Frame, area: Rect, snap: &TuiSnapshot) {
    let pct = if snap.btc_prev_price > 0.0 {
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

    let text = vec![
        Line::from(vec![
            Span::raw(format!("BTC  : ${:>10.2}  ({:+.4}%)  ", snap.btc_price, pct)),
            Span::styled(trend_label, Style::default().fg(trend_color).bold()),
        ]),
        Line::from(format!(
            "ETH5m: UP ${:.4}  |  DOWN ${:.4}    {:02}:{:02} remaining",
            snap.eth_up_price, snap.eth_down_price, expiry_mm, expiry_ss
        )),
        Line::from(format!(
            "INV  : UP {:.3} shares  |  DOWN {:.3} shares",
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

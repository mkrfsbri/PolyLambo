use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, Row, Table},
};

use crate::state::{bot_status, trend};
use super::{OrderSnap, PositionSnap, TradeSnap, TuiSnapshot};

// ── top-level render ──────────────────────────────────────────────────────────

pub fn render(frame: &mut Frame, snap: &TuiSnapshot) {
    let area = frame.size();
    let chunks = Layout::vertical([
        Constraint::Length(4), // header
        Constraint::Length(7), // market
        Constraint::Min(5),    // active orders table
        Constraint::Length(4), // active position
        Constraint::Length(7), // history
        Constraint::Length(4), // protection status
    ])
    .split(area);

    render_header(frame, chunks[0], snap);
    render_market(frame, chunks[1], snap);
    render_orders(frame, chunks[2], &snap.orders);
    render_position(frame, chunks[3], snap.active_position.as_ref());
    render_history(frame, chunks[4], &snap.history);
    render_protection(frame, chunks[5], snap);
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
    // ── computed values ───────────────────────────────────────────────────────
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

    let (eth_arrow, eth_color)   = price_arrow(snap.eth_spot_price, snap.eth_spot_prev);
    let (poly_arrow, poly_color) = price_arrow(snap.eth_poly_spot, snap.eth_poly_spot_prev);

    let (ptb_label, ptb_color) = if snap.eth_open_price > 0.0 {
        let current = if snap.eth_poly_spot > 0.0 { snap.eth_poly_spot } else { snap.eth_spot_price };
        let diff = current - snap.eth_open_price;
        let sign = if diff >= 0.0 { "+" } else { "" };
        let (_, col) = price_arrow(current, snap.eth_open_price);
        (format!("${:.2}   \u{394} {}{:.2}", snap.eth_open_price, sign, diff), col)
    } else {
        ("\u{2013}".to_string(), Color::White)
    };

    let (score_label, score_color) = {
        let s = snap.signal_score;
        let t = snap.score_threshold;
        if s >= t {
            (format!("{:+.3}  \u{2192}  UP", s), Color::Green)
        } else if s <= -t {
            (format!("{:+.3}  \u{2192}  DN", s), Color::Red)
        } else {
            (format!("{:+.3}  \u{2192}  \u{2013}", s), Color::White)
        }
    };

    let (up_arrow, up_color)     = price_arrow(snap.eth_up_price, snap.eth_up_prev);
    let (down_arrow, down_color) = price_arrow(snap.eth_down_price, snap.eth_down_prev);

    let up_book = if snap.eth_up_bid > 0.0 && snap.eth_up_ask > 0.0 {
        format!("{:.4} / {:.4}", snap.eth_up_bid, snap.eth_up_ask)
    } else if snap.eth_up_price > 0.0 {
        format!("{:.4} (mid)", snap.eth_up_price)
    } else {
        "--".to_string()
    };
    let dn_book = if snap.eth_down_bid > 0.0 && snap.eth_down_ask > 0.0 {
        format!("{:.4} / {:.4}", snap.eth_down_bid, snap.eth_down_ask)
    } else if snap.eth_down_price > 0.0 {
        format!("{:.4} (mid)", snap.eth_down_price)
    } else {
        "--".to_string()
    };

    let expiry_mm = snap.time_to_expiry_secs / 60;
    let expiry_ss = snap.time_to_expiry_secs % 60;
    let inv_up    = snap.inventory_up   as f64 / 1000.0;
    let inv_down  = snap.inventory_down as f64 / 1000.0;

    // ── render ────────────────────────────────────────────────────────────────
    let block = Block::default().borders(Borders::ALL).title(" Market ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [left, mid, right] = Layout::horizontal([
        Constraint::Percentage(55),
        Constraint::Length(1),
        Constraint::Percentage(44),
    ])
    .areas(inner);

    // Vertical divider
    let divider: Vec<Line> = (0..inner.height).map(|_| Line::raw("\u{2502}")).collect();
    frame.render_widget(Paragraph::new(divider), mid);

    // Left column: price feeds + score
    let left_text = vec![
        Line::from(vec![
            Span::raw(format!("BTC      ${:>10.2}  ({:+.4}%)  ", snap.btc_price, btc_pct)),
            Span::styled(trend_label, Style::default().fg(trend_color).bold()),
        ]),
        Line::from(vec![
            Span::raw("Binance  "),
            Span::styled(
                format!("${:.2}", snap.eth_spot_price),
                Style::default().fg(eth_color).bold(),
            ),
            Span::styled(eth_arrow, Style::default().fg(eth_color)),
        ]),
        Line::from(vec![
            Span::raw("Poly ETH "),
            if snap.eth_poly_spot > 0.0 {
                Span::styled(
                    format!("${:.2}", snap.eth_poly_spot),
                    Style::default().fg(poly_color).bold(),
                )
            } else {
                Span::styled("\u{2013}", Style::default().fg(Color::White))
            },
            Span::styled(poly_arrow, Style::default().fg(poly_color)),
        ]),
        Line::from(vec![
            Span::raw("PTB      "),
            Span::styled(ptb_label, Style::default().fg(ptb_color).bold()),
        ]),
        Line::from(vec![
            Span::raw("Score    "),
            Span::styled(score_label, Style::default().fg(score_color).bold()),
        ]),
    ];
    frame.render_widget(Paragraph::new(left_text), left);

    // Right column: token book + inventory + expiry
    let right_text = vec![
        Line::from(vec![
            Span::raw("UP  "),
            Span::styled(up_book, Style::default().fg(up_color).bold()),
            Span::styled(up_arrow, Style::default().fg(up_color)),
        ]),
        Line::from(vec![
            Span::raw("DN  "),
            Span::styled(dn_book, Style::default().fg(down_color).bold()),
            Span::styled(down_arrow, Style::default().fg(down_color)),
        ]),
        Line::raw(""),
        Line::from(format!("Inv  UP {:.3}  DN {:.3} shr", inv_up, inv_down)),
        Line::from(format!("Expiry  {:02}:{:02}", expiry_mm, expiry_ss)),
    ];
    frame.render_widget(Paragraph::new(right_text), right);
}

fn render_orders(frame: &mut Frame, area: Rect, orders: &[OrderSnap]) {
    let header = Row::new(["ID (trunc)", "Side", "Price", "USDC", "Status", "TTL"])
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

fn render_position(frame: &mut Frame, area: Rect, pos: Option<&PositionSnap>) {
    let block = Block::default().borders(Borders::ALL).title(" Active Position ");
    let Some(p) = pos else {
        frame.render_widget(
            Paragraph::new("  No active position")
                .style(Style::default().fg(Color::DarkGray))
                .block(block),
            area,
        );
        return;
    };

    let (side_label, side_color) = if p.side == "UP" {
        ("\u{25b2} UP", Color::Green)
    } else {
        ("\u{25bc} DN", Color::Red)
    };
    let (cur_arrow, cur_color) = price_arrow(p.current_price, p.entry_price);
    let pnl_color = if p.unrealized_pnl >= 0.0 { Color::Green } else { Color::Red };
    let pnl_sign  = if p.unrealized_pnl >= 0.0 { "+" } else { "" };

    let text = vec![
        Line::from(vec![
            Span::raw("  "),
            Span::styled(side_label, Style::default().fg(side_color).bold()),
            Span::raw(format!("  Entry: {:.4}  Current: ", p.entry_price)),
            Span::styled(
                format!("{:.4}", p.current_price),
                Style::default().fg(cur_color).bold(),
            ),
            Span::styled(cur_arrow, Style::default().fg(cur_color)),
            Span::raw(format!("  Qty: {:.2} shr", p.qty)),
        ]),
        Line::from(vec![
            Span::raw(format!("  Elapsed: {}s   Est. P&L: ", p.elapsed_secs)),
            Span::styled(
                format!("{}{:.3} USDC", pnl_sign, p.unrealized_pnl),
                Style::default().fg(pnl_color).bold(),
            ),
        ]),
    ];
    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn render_history(frame: &mut Frame, area: Rect, history: &[TradeSnap]) {
    let header = Row::new(["Time", "Side", "Entry", "Exit", "PnL", "Reason"])
        .style(Style::default().bold().fg(Color::Yellow));

    let rows: Vec<Row> = if history.is_empty() {
        vec![
            Row::new(["No completed trades yet", "", "", "", "", ""])
                .style(Style::default().fg(Color::DarkGray)),
        ]
    } else {
        history
            .iter()
            .map(|t| {
                let reason_abbr = match t.exit_reason.as_str() {
                    "TakeProfit" => "TP",
                    "StopLoss"   => "SL",
                    "Resolved"   => "RES",
                    "Emergency"  => "EMG",
                    _            => "?",
                };
                let pnl_sign = if t.pnl_usdc >= 0.0 { "+" } else { "" };
                let pnl_str  = format!("{pnl_sign}{:.2}", t.pnl_usdc);
                let pnl_color = if t.pnl_usdc >= 0.0 { Color::Green } else { Color::Red };
                let row_color = match t.exit_reason.as_str() {
                    "TakeProfit" | "Resolved" => Color::Green,
                    "StopLoss" | "Emergency"  => Color::Red,
                    _                         => Color::DarkGray,
                };
                Row::new(vec![
                    ratatui::text::Span::styled(t.time.clone(),              Style::default().fg(row_color)),
                    ratatui::text::Span::styled(t.side.clone(),              Style::default().fg(row_color)),
                    ratatui::text::Span::styled(format!("{:.4}", t.entry_price), Style::default().fg(row_color)),
                    ratatui::text::Span::styled(format!("{:.4}", t.exit_price),  Style::default().fg(row_color)),
                    ratatui::text::Span::styled(pnl_str,                     Style::default().fg(pnl_color).bold()),
                    ratatui::text::Span::styled(reason_abbr.to_string(),     Style::default().fg(row_color)),
                ])
            })
            .collect()
    };

    let widths = [
        Constraint::Length(9),
        Constraint::Length(4),
        Constraint::Length(7),
        Constraint::Length(7),
        Constraint::Length(9),
        Constraint::Length(4),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(" History "));
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

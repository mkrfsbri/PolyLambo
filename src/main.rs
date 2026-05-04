use anyhow::{Context, Result};
use eth5m_bot::{binance, clob, config, engine, gamma, poly_ws, state, tui};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::{watch, Mutex};
use tokio::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = Arc::new(config::Config::from_env()?);

    // ── file logging (TUI owns stdout) ────────────────────────────────────────
    std::fs::create_dir_all("logs").context("create logs dir")?;
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let appender = tracing_appender::rolling::never("logs", format!("eth5m-bot-{date}.log"));
    let (nb_writer, _guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_writer(nb_writer)
        .with_env_filter(cfg.log_level.as_str())
        .with_ansi(false)
        .init();

    tracing::info!("eth5m-bot starting | dry_run={}", cfg.dry_run);

    // ── shared state ──────────────────────────────────────────────────────────
    let state = state::AppState::new();

    // ── CLOB client ───────────────────────────────────────────────────────────
    let clob: Arc<clob::ClobClient> = if cfg.dry_run {
        clob::ClobClient::new_dry_run()
    } else {
        clob::ClobClient::new(
            cfg.clob_api_key.clone(),
            cfg.clob_secret.clone(),
            cfg.clob_passphrase.clone(),
            &cfg.private_key,
            cfg.polymarket_proxy_address.clone(),
        )?
    };

    // ── pre-flight ────────────────────────────────────────────────────────────
    if cfg.dry_run {
        // Simulate $1 000 balance
        state.balance_usdc.store(
            state::f64_to_atomic(1_000.0) as i64,
            Ordering::Release,
        );
        tracing::info!("[PREFLIGHT] DRY_RUN — balance=$1000 simulated");
    } else {
        clob.health_check().await.context("CLOB /ok check")?;
        tracing::info!("[PREFLIGHT] CLOB reachable");

        let balance = clob.get_balance().await.context("balance check")?;
        anyhow::ensure!(balance > 1.0, "balance ${balance:.2} < $1 — abort");
        state.balance_usdc.store(
            state::f64_to_atomic(balance) as i64,
            Ordering::Release,
        );
        tracing::info!("[PREFLIGHT] Balance: ${balance:.2}");
    }

    // ── spawn BTC + ETH + gamma tasks early so feeds warm up ─────────────────
    let state_b = state.clone();
    let btc_task = tokio::spawn(async move { binance::run_btc_feed(state_b).await });

    let state_eth = state.clone();
    tokio::spawn(async move { binance::run_eth_feed(state_eth).await });

    let state_g = state.clone();
    let http = reqwest::Client::new();
    tokio::spawn(async move { gamma::discover_and_update(state_g, http).await });

    // CLOB REST fallback: seeds eth_up/down_bid/ask every 5 s so the TUI
    // always has book data even before the CLOB WebSocket delivers its first
    // snapshot.  The CLOB WS overwrites these with real-time values.
    let state_rf = state.clone();
    let http_rf  = reqwest::Client::new();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            gamma::refresh_prices_from_clob(&state_rf, &http_rf).await;
        }
    });

    // CLOB WebSocket: real-time order books. Requires WS auth — skip in dry-run
    // (the REST fallback in refresh_prices_from_clob seeds bid/ask every 5 s).
    if !cfg.dry_run {
        let state_cw = state.clone();
        tokio::spawn(async move { poly_ws::run_clob_ws(state_cw).await });
    }

    // RTDS WebSocket: Polymarket live data — captures ETH/USD price at window
    // open to use as the authoritative "price to beat".
    let state_rt = state.clone();
    tokio::spawn(async move { poly_ws::run_rtds_ws(state_rt).await });

    // Boundary timer: snapshots Binance ETH spot at every 300-second UTC
    // boundary as the fallback price-to-beat when RTDS does not provide one.
    let state_bt = state.clone();
    tokio::spawn(async move { poly_ws::run_open_price_boundary(state_bt).await });

    // Wait up to 5 s for a real BTC price
    let btc_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if binance::get_btc_price(&state) > 0.0 {
            tracing::info!(
                "[PREFLIGHT] BTC feed live: ${:.2}",
                binance::get_btc_price(&state)
            );
            break;
        }
        if tokio::time::Instant::now() >= btc_deadline {
            anyhow::bail!("BTC feed not connected within 5 s");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // ── validate slug reachable (non-blocking for dry_run) ───────────────────
    if !cfg.dry_run {
        let slug = gamma::compute_next_slug();
        gamma::fetch_market_tokens(&slug, &reqwest::Client::new())
            .await
            .with_context(|| format!("[PREFLIGHT] slug {slug} not resolvable"))?;
        tracing::info!("[PREFLIGHT] Gamma slug OK: {slug}");
    }

    // ── trading engine ────────────────────────────────────────────────────────
    let trading_engine = Arc::new(Mutex::new(engine::TradingEngine::new(
        state.clone(),
        cfg.clone(),
    )));
    let clob_e = clob.clone();
    let te = trading_engine.clone();
    let engine_task = tokio::spawn(async move { engine::run_engine_loop(te, clob_e).await });

    // ── TUI watch channel + thread ────────────────────────────────────────────
    let (tui_tx, tui_rx) = watch::channel(tui::TuiSnapshot::default());
    std::thread::spawn(move || tui::run_tui(tui_rx));

    // Push snapshots to TUI at 100 ms
    let state_snap = state.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(100));
        loop {
            ticker.tick().await;
            let snap = build_snapshot(&state_snap).await;
            if tui_tx.send(snap).is_err() {
                break;
            }
        }
    });

    // Balance monitor — check every 30s, halt if < $1
    if !cfg.dry_run {
        let clob_bm  = clob.clone();
        let state_bm = state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                interval.tick().await;
                match clob_bm.get_balance().await {
                    Ok(bal) => {
                        state_bm.balance_usdc.store(
                            state::f64_to_atomic(bal) as i64,
                            Ordering::Release,
                        );
                        if bal < 1.0 {
                            tracing::error!(
                                "[BALANCE] Balance ${bal:.2} < $1 — EMERGENCY halt"
                            );
                            state_bm.bot_status.store(
                                state::bot_status::EMERGENCY,
                                Ordering::Release,
                            );
                        } else {
                            tracing::debug!("[BALANCE] ${bal:.2}");
                        }
                    }
                    Err(e) => tracing::warn!("[BALANCE] refresh failed: {e:#}"),
                }
            }
        });
    }

    // ── graceful shutdown ─────────────────────────────────────────────────────
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("[MAIN] ctrl-c — shutting down");
        }
        res = btc_task => {
            tracing::error!("[MAIN] BTC task exited: {res:?}");
        }
        res = engine_task => {
            tracing::error!("[MAIN] Engine task exited: {res:?}");
        }
    }

    tracing::info!("[MAIN] Cancelling all open orders...");
    let _ = clob.cancel_all().await;
    tracing::info!("[MAIN] Shutdown complete");
    Ok(())
}

// ── snapshot builder ──────────────────────────────────────────────────────────

async fn build_snapshot(state: &state::AppState) -> tui::TuiSnapshot {
    tui::TuiSnapshot {
        slug:           state.current_slug.read().await.clone(),
        question:       state.current_question.read().await.clone(),
        bot_status:     state.bot_status.load(Ordering::Acquire),
        balance_usdc:   state.balance_usdc.load(Ordering::Acquire) as f64 / 1_000_000.0,
        pnl_usdc:       state.pnl_usdc.load(Ordering::Acquire) as f64 / 100.0,
        api_latency_ms: state.api_latency_ms.load(Ordering::Acquire),
        ws_latency_us:  state.ws_latency_us.load(Ordering::Acquire),
        btc_price:      state::atomic_to_f64(state.btc.price_raw.load(Ordering::Acquire)),
        btc_prev_price: state::atomic_to_f64(state.btc.price_prev.load(Ordering::Acquire)),
        btc_trend:      state.btc.trend.load(Ordering::Acquire),
        eth_spot_price:     state::atomic_to_f64(state.eth_spot_raw.load(Ordering::Acquire)),
        eth_spot_prev:      state::atomic_to_f64(state.eth_spot_prev.load(Ordering::Acquire)),
        eth_open_price:     state::atomic_to_f64(state.eth_open_price.load(Ordering::Acquire)),
        eth_poly_spot:      state::atomic_to_f64(state.eth_poly_spot.load(Ordering::Acquire)),
        eth_poly_spot_prev: state::atomic_to_f64(state.eth_poly_spot_prev.load(Ordering::Acquire)),
        eth_up_price:   state::atomic_to_f64(state.eth_up_price.load(Ordering::Acquire)),
        eth_up_prev:    state::atomic_to_f64(state.eth_up_prev.load(Ordering::Acquire)),
        eth_up_bid:     state::atomic_to_f64(state.eth_up_bid.load(Ordering::Acquire)),
        eth_up_ask:     state::atomic_to_f64(state.eth_up_ask.load(Ordering::Acquire)),
        eth_down_price: state::atomic_to_f64(state.eth_down_price.load(Ordering::Acquire)),
        eth_down_prev:  state::atomic_to_f64(state.eth_down_prev.load(Ordering::Acquire)),
        eth_down_bid:   state::atomic_to_f64(state.eth_down_bid.load(Ordering::Acquire)),
        eth_down_ask:   state::atomic_to_f64(state.eth_down_ask.load(Ordering::Acquire)),
        time_to_expiry_secs: state.time_to_expiry_secs.load(Ordering::Acquire),
        inventory_up:   state.inventory_up.load(Ordering::Acquire),
        inventory_down: state.inventory_down.load(Ordering::Acquire),
        orders: state.orders.iter().map(|entry| {
            let o = entry.value();
            tui::OrderSnap {
                order_id: o.order_id.clone(),
                side: match o.side { state::OrderSide::Up => "UP", state::OrderSide::Down => "DOWN" }.to_string(),
                price: o.price,
                qty: o.quantity,
                status: format!("{:?}", o.status),
                ttl_secs: u64::MAX,
            }
        }).collect(),
        reversal_warning: {
            let dev = state::atomic_to_f64(
                state.reversal_deviation.load(Ordering::Acquire)
            );
            if dev > 0.0 { Some(dev) } else { None }
        },
        momentum_decaying: state.momentum_decaying.load(Ordering::Acquire) != 0,
    }
}

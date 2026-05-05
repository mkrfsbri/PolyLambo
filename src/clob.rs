use anyhow::{Context, Result};
use polyfill_rs::{
    ApiCredentials, ClientConfig, ClobClient as PolyfillClient, OrderArgs, Side as PolyfillSide,
};
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

use crate::state::OrderSide;

const SIGNAL_BUDGET_US: u128 = 500;
const CLOB_BASE: &str = "https://clob.polymarket.com";

pub struct ClobClient {
    inner: PolyfillClient,
    dry_run: bool,
}

impl ClobClient {
    /// Dry-run mode — no real orders submitted, uses an unauthenticated client.
    pub fn new_dry_run() -> Arc<Self> {
        let inner = PolyfillClient::new(CLOB_BASE);
        Arc::new(ClobClient { inner, dry_run: true })
    }

    /// Live mode — wraps polyfill-rs with your Polymarket credentials.
    ///
    /// `proxy_address` is the Polymarket proxy/funder wallet address.
    /// Pass an empty string if you are using a plain EOA (no proxy wallet).
    pub fn new(
        api_key: String,
        secret: String,
        passphrase: String,
        private_key: &str,
        proxy_address: String,
    ) -> Result<Arc<Self>> {
        let (sig_type, funder) = if proxy_address.is_empty() {
            (None, None)
        } else {
            // signature_type 1 = PolyProxy (proxy wallet created via Polymarket UI)
            (Some(1u8), Some(proxy_address))
        };

        let inner = PolyfillClient::from_config(ClientConfig {
            base_url: CLOB_BASE.to_string(),
            chain: 137,
            private_key: Some(private_key.trim_start_matches("0x").to_string()),
            api_credentials: Some(ApiCredentials {
                api_key,
                secret,
                passphrase,
            }),
            signature_type: sig_type,
            funder,
            ..ClientConfig::default()
        })
        .context("build polyfill ClobClient")?;

        Ok(Arc::new(ClobClient { inner, dry_run: false }))
    }

    // ── order management ──────────────────────────────────────────────────────

    /// Submit a limit buy order for a directional outcome token.
    /// `price` is 0.0–1.0 USDC per token; `size_usdc` is the USDC notional.
    pub async fn place_limit_order(
        &self,
        token_id: &str,
        side: &OrderSide,
        price: f64,
        size_usdc: f64,
    ) -> Result<String> {
        if self.dry_run {
            tracing::info!(
                "[DRY-RUN] place_limit_order | {side:?} | price: {price:.2} | ${size_usdc:.0}"
            );
            return Ok(format!(
                "dry-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| format!("{:x}", d.as_nanos()))
                    .unwrap_or_default()
            ));
        }

        let t0 = Instant::now();

        // Both Up and Down entries are buys of their respective outcome tokens.
        let poly_side = match side {
            OrderSide::Up | OrderSide::Down => PolyfillSide::BUY,
        };

        let price_dec = Decimal::from_str(&format!("{price:.4}")).context("price → Decimal")?;
        let size_dec = Decimal::from_str(&format!("{size_usdc:.2}")).context("size → Decimal")?;

        let order_args = OrderArgs::new(token_id, price_dec, size_dec, poly_side);
        let resp = self
            .inner
            .create_and_post_order(&order_args, None, None)
            .await
            .context("create_and_post_order")?;

        let lat_us = t0.elapsed().as_micros();
        let order_id = resp.order_id.clone();
        let side_str = match side {
            OrderSide::Up => "UP",
            OrderSide::Down => "DOWN",
        };
        tracing::info!(
            "[CLOB] Order placed | ID: {order_id} | Side: {side_str} \
             | Price: {price:.2} | Size: ${size_usdc:.0} | Lat: {}ms",
            lat_us / 1000
        );
        if lat_us > SIGNAL_BUDGET_US {
            tracing::warn!(
                "[CLOB] signal→order_submit {lat_us}μs exceeded {SIGNAL_BUDGET_US}μs budget"
            );
        }

        Ok(order_id)
    }

    pub async fn cancel_order(&self, order_id: &str) -> Result<()> {
        if self.dry_run {
            tracing::info!("[DRY-RUN] cancel_order | {order_id}");
            return Ok(());
        }
        self.inner.cancel(order_id).await.context("cancel order")?;
        tracing::debug!("[CLOB] Cancelled order {order_id}");
        Ok(())
    }

    pub async fn cancel_all(&self) -> Result<()> {
        if self.dry_run {
            tracing::info!("[DRY-RUN] cancel_all");
            return Ok(());
        }
        self.inner.cancel_all().await.context("cancel_all")?;
        tracing::info!("[CLOB] All orders cancelled");
        Ok(())
    }

    /// Sell `qty` outcome tokens at the current best bid.
    pub async fn sell_best_bid(&self, token_id: &str, qty: f64) -> Result<()> {
        if self.dry_run {
            tracing::info!("[DRY-RUN] sell_best_bid | token: {token_id} | qty: {qty:.2}");
            return Ok(());
        }

        let book = self
            .inner
            .get_order_book(token_id)
            .await
            .context("get_order_book")?;

        let best_bid = book
            .bids
            .first()
            .map(|l| l.price)
            .ok_or_else(|| anyhow::anyhow!("sell_best_bid: no bids for {token_id}"))?;

        if best_bid <= Decimal::ZERO {
            anyhow::bail!("sell_best_bid: best bid is zero for {token_id}");
        }

        let qty_dec = Decimal::from_str(&format!("{qty:.4}")).context("qty → Decimal")?;
        let order_args = OrderArgs::new(token_id, best_bid, qty_dec, PolyfillSide::SELL);
        self.inner
            .create_and_post_order(&order_args, None, None)
            .await
            .context("sell_best_bid order")?;

        tracing::info!(
            "[CLOB] Sell at best bid | token: {token_id} | qty: {qty:.2} | bid: {best_bid:.4}"
        );
        Ok(())
    }

    // ── account ───────────────────────────────────────────────────────────────

    /// Returns the USDC balance for the authenticated wallet.
    pub async fn get_balance(&self) -> Result<f64> {
        if self.dry_run {
            return Ok(1_000.0);
        }
        let val = self
            .inner
            .get_balance_allowance(None)
            .await
            .context("get_balance_allowance")?;

        let balance_str = val
            .get("balance")
            .and_then(|v| v.as_str())
            .unwrap_or("0");

        balance_str.parse::<f64>().context("parse balance")
    }

    /// GET /ok — liveness check.
    pub async fn health_check(&self) -> Result<()> {
        if self.inner.get_ok().await {
            Ok(())
        } else {
            anyhow::bail!("clob /ok returned error")
        }
    }
}

#![allow(non_snake_case)]

use alloy::primitives::{Address, U256, B256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::SignerSync;
use alloy::sol;
use alloy::sol_types::{Eip712Domain, SolStruct};
use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use hmac::{Hmac, Mac};
use reqwest::{Client, Response};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::Deserialize;
use sha2::Sha256;
use std::borrow::Cow;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SIGNAL_BUDGET_US: u128 = 500; // signal→order_submit target < 500 μs

use crate::state::OrderSide;

const CLOB_BASE: &str = "https://clob.polymarket.com";
const CHAIN_ID: u64 = 137;
const ZERO_ADDR: &str = "0x0000000000000000000000000000000000000000";
// USDC on Polygon has 6 decimal places
const USDC_DECIMALS: f64 = 1_000_000.0;

type HmacSha256 = Hmac<Sha256>;

// ── EIP-712 order struct ──────────────────────────────────────────────────────

sol! {
    struct Order {
        address maker;
        address taker;
        uint256 tokenId;
        uint256 makerAmount;
        uint256 takerAmount;
        uint256 expiration;
        uint256 nonce;
        uint256 feeRateBps;
        uint8 side;
        uint8 signatureType;
    }
}

// ── CLOB client ───────────────────────────────────────────────────────────────

pub struct ClobClient {
    client: Client,
    api_key: String,
    secret: String,
    passphrase: String,
    wallet: PrivateKeySigner,
    proxy_address: String,
}

impl ClobClient {
    /// Stub for DRY_RUN mode — uses a well-known test key, never signs real orders.
    pub fn new_dry_run() -> Arc<Self> {
        // Hardhat/Anvil account #0 — safe to hard-code for dry-run only
        let wallet: PrivateKeySigner =
            "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
                .parse()
                .unwrap();
        Arc::new(ClobClient {
            client: Client::new(),
            api_key: String::new(),
            secret: String::new(),
            passphrase: String::new(),
            wallet,
            proxy_address: "0x0000000000000000000000000000000000000000".to_string(),
        })
    }

    pub fn new(
        api_key: String,
        secret: String,
        passphrase: String,
        private_key: &str,
        proxy_address: String,
    ) -> Result<Arc<Self>> {
        let wallet: PrivateKeySigner = private_key
            .trim_start_matches("0x")
            .parse()
            .context("parse private key")?;
        Ok(Arc::new(ClobClient {
            client: Client::new(),
            api_key,
            secret,
            passphrase,
            wallet,
            proxy_address,
        }))
    }

    // ── auth ──────────────────────────────────────────────────────────────────

    pub fn hmac_auth_headers(&self, method: &str, path: &str, body: &str) -> Result<HeaderMap> {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string();

        let msg = format!("{ts}{method}{path}{body}");
        let mut mac = HmacSha256::new_from_slice(self.secret.as_bytes())
            .context("hmac init")?;
        mac.update(msg.as_bytes());
        let sig_b64 = B64.encode(mac.finalize().into_bytes());

        let mut headers = HeaderMap::new();
        headers.insert("POLY_ADDRESS",   HeaderValue::from_str(&self.proxy_address)?);
        headers.insert("POLY_SIGNATURE", HeaderValue::from_str(&sig_b64)?);
        headers.insert("POLY-TIMESTAMP", HeaderValue::from_str(&ts)?);
        headers.insert("POLY-API-KEY",   HeaderValue::from_str(&self.api_key)?);
        headers.insert("POLY-PASSPHRASE",HeaderValue::from_str(&self.passphrase)?);
        Ok(headers)
    }

    // ── EIP-712 signing ───────────────────────────────────────────────────────

    pub fn eip712_domain() -> Eip712Domain {
        Eip712Domain {
            name: Some(Cow::Borrowed("ClobAuthDomain")),
            version: Some(Cow::Borrowed("1")),
            chain_id: Some(U256::from(CHAIN_ID)),
            verifying_contract: None,
            salt: None,
        }
    }

    fn sign_order_sync(&self, order: &Order) -> Result<String> {
        let domain = Self::eip712_domain();
        let hash: B256 = order.eip712_signing_hash(&domain);
        let sig = self.wallet.sign_hash_sync(&hash)?;
        Ok(format!("0x{}", hex::encode(sig.as_bytes())))
    }

    // ── HTTP with retry ───────────────────────────────────────────────────────

    /// Send a single authenticated request, retrying once on 429 and bailing
    /// on 401. Returns the raw `Response` on success.
    async fn http_call(
        &self,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<Response> {
        for attempt in 0..=1u8 {
            let headers = self.hmac_auth_headers(method, path, body)?;
            let url = format!("{CLOB_BASE}{path}");
            let req = match method {
                "POST" => self
                    .client
                    .post(&url)
                    .headers(headers)
                    .header("Content-Type", "application/json")
                    .body(body.to_string()),
                "DELETE" => self.client.delete(&url).headers(headers),
                "GET" => self.client.get(&url).headers(headers),
                other => anyhow::bail!("unsupported HTTP method: {other}"),
            };

            let resp = req.send().await.context("clob http send")?;
            match resp.status().as_u16() {
                429 if attempt == 0 => {
                    tracing::warn!("[CLOB] 429 rate-limited on {method} {path} — retry in 1s");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
                401 => {
                    tracing::error!("[CLOB] 401 Unauthorized on {method} {path} — halting");
                    anyhow::bail!("CLOB 401 Unauthorized")
                }
                _ => return Ok(resp),
            }
        }
        anyhow::bail!("CLOB {method} {path} failed after retry")
    }

    // ── public API ────────────────────────────────────────────────────────────

    /// Place a limit order. `side` is the OUTCOME direction (Up = buy the Up
    /// token, Down = buy the Down token). `price` is 0.0–1.0 USDC per token.
    /// `size_usdc` is the USDC value you commit.
    pub async fn place_limit_order(
        &self,
        token_id: &str,
        side: &OrderSide,
        price: f64,
        size_usdc: f64,
    ) -> Result<String> {
        let t0 = Instant::now();

        let maker: Address = self.proxy_address.parse().context("parse proxy addr")?;
        let taker: Address = ZERO_ADDR.parse().unwrap();
        let token_id_u256: U256 = token_id.parse().context("parse token_id as U256")?;

        // makerAmount = USDC committed (6 decimals)
        // takerAmount = outcome tokens expected (6 decimals)
        let maker_amt = U256::from((size_usdc * USDC_DECIMALS) as u64);
        let taker_amt = if price > 0.0 {
            U256::from((size_usdc / price * USDC_DECIMALS) as u64)
        } else {
            U256::ZERO
        };

        let clob_side: u8 = 0; // BUY — we always buy a directional token

        let order = Order {
            maker,
            taker,
            tokenId: token_id_u256,
            makerAmount: maker_amt,
            takerAmount: taker_amt,
            expiration: U256::ZERO,
            nonce: U256::ZERO,
            feeRateBps: U256::ZERO,
            side: clob_side,
            signatureType: 0,
        };

        let signature = self.sign_order_sync(&order)?;

        let body = serde_json::json!({
            "order": {
                "maker":         self.proxy_address,
                "taker":         ZERO_ADDR,
                "tokenId":       token_id,
                "makerAmount":   maker_amt.to_string(),
                "takerAmount":   taker_amt.to_string(),
                "expiration":    "0",
                "nonce":         "0",
                "feeRateBps":    "0",
                "side":          clob_side,
                "signatureType": 0,
                "signature":     signature,
            },
            "owner":     self.proxy_address,
            "orderType": "GTC",
        })
        .to_string();

        let resp: serde_json::Value = self
            .http_call("POST", "/order", &body)
            .await?
            .json()
            .await
            .context("clob place_order parse")?;

        let order_id = resp
            .get("orderID")
            .or_else(|| resp.get("order_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let lat_ms  = t0.elapsed().as_millis();
        let lat_us  = t0.elapsed().as_micros();
        let side_str = match side { OrderSide::Up => "UP", OrderSide::Down => "DOWN" };
        tracing::info!(
            "[CLOB] Order placed | ID: {order_id} | Side: {side_str} | Price: {price:.2} | Size: ${size_usdc:.0} | Lat: {lat_ms}ms"
        );
        if lat_us > SIGNAL_BUDGET_US {
            tracing::warn!(
                "[CLOB] signal→order_submit {}μs exceeded {}μs budget",
                lat_us, SIGNAL_BUDGET_US
            );
        }

        Ok(order_id)
    }

    pub async fn cancel_order(&self, order_id: &str) -> Result<()> {
        let path = format!("/orders/{order_id}");
        self.http_call("DELETE", &path, "").await?;
        tracing::debug!("[CLOB] Cancelled order {order_id}");
        Ok(())
    }

    pub async fn cancel_all(&self) -> Result<()> {
        self.http_call("DELETE", "/orders", "").await?;
        tracing::info!("[CLOB] All orders cancelled");
        Ok(())
    }

    /// Sell `qty` outcome tokens from `token_id` at the current best bid price.
    pub async fn sell_best_bid(&self, token_id: &str, qty: f64) -> Result<()> {
        #[derive(Deserialize)]
        struct Level {
            price: String,
        }
        #[derive(Deserialize)]
        struct Book {
            bids: Vec<Level>,
        }

        // Order book is public — no auth headers needed, but we still want retry.
        let book: Book = self
            .client
            .get(format!("{CLOB_BASE}/book"))
            .query(&[("token_id", token_id)])
            .send()
            .await
            .context("clob get_book")?
            .json()
            .await
            .context("clob book parse")?;

        let best_bid: f64 = book
            .bids
            .first()
            .and_then(|b| b.price.parse().ok())
            .unwrap_or(0.0);

        if best_bid <= 0.0 {
            anyhow::bail!("sell_best_bid: no bids available for {token_id}");
        }

        // For a sell, makerAmount = tokens sold, takerAmount = USDC received.
        let maker: Address = self.proxy_address.parse().context("parse proxy addr")?;
        let taker: Address = ZERO_ADDR.parse().unwrap();
        let token_id_u256: U256 = token_id.parse().context("parse token_id")?;
        let maker_amt = U256::from((qty * USDC_DECIMALS) as u64);
        let taker_amt = U256::from((qty * best_bid * USDC_DECIMALS) as u64);

        let order = Order {
            maker,
            taker,
            tokenId: token_id_u256,
            makerAmount: maker_amt,
            takerAmount: taker_amt,
            expiration: U256::ZERO,
            nonce: U256::ZERO,
            feeRateBps: U256::ZERO,
            side: 1, // SELL
            signatureType: 0,
        };
        let signature = self.sign_order_sync(&order)?;

        let body = serde_json::json!({
            "order": {
                "maker":         self.proxy_address,
                "taker":         ZERO_ADDR,
                "tokenId":       token_id,
                "makerAmount":   maker_amt.to_string(),
                "takerAmount":   taker_amt.to_string(),
                "expiration":    "0",
                "nonce":         "0",
                "feeRateBps":    "0",
                "side":          1u8,
                "signatureType": 0,
                "signature":     signature,
            },
            "owner":     self.proxy_address,
            "orderType": "GTC",
        })
        .to_string();

        self.http_call("POST", "/order", &body).await?;
        tracing::info!(
            "[CLOB] Sell at best bid | token: {token_id} | qty: {qty:.2} | bid: {best_bid:.4}"
        );
        Ok(())
    }

    pub async fn get_balance(&self) -> Result<f64> {
        #[derive(Deserialize)]
        struct Resp {
            balance: String,
        }
        let resp: Resp = self
            .http_call("GET", "/balance", "")
            .await?
            .json()
            .await
            .context("clob balance parse")?;
        resp.balance.parse::<f64>().context("parse balance")
    }

    /// GET /ok — liveness check.
    pub async fn health_check(&self) -> Result<()> {
        let status = self
            .client
            .get(format!("{CLOB_BASE}/ok"))
            .send()
            .await
            .context("clob health_check")?
            .status();
        if status.is_success() {
            Ok(())
        } else {
            anyhow::bail!("clob /ok returned {status}")
        }
    }
}

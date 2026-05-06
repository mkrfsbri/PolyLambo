use anyhow::Result;
use sqlx::SqlitePool;

pub struct Db {
    pool: SqlitePool,
}

pub struct TradeEntry {
    pub opened_at:     String,
    pub slug:          String,
    pub side:          String,
    pub entry_price:   f64,
    pub size_usdc:     f64,
    pub qty_shares:    f64,
    pub score:         f64,
    pub btc_vel:       f64,
    pub ptb_pct:       f64,
    pub btc_price:     f64,
    pub eth_price:     f64,
    pub confirm_ticks: u8,
}

impl Db {
    pub async fn open(url: &str) -> Result<Self> {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(url)
            .await?;
        sqlx::query(include_str!("../migrations/001_trades.sql"))
            .execute(&pool)
            .await?;
        Ok(Db { pool })
    }

    pub async fn insert_trade(&self, e: &TradeEntry) -> Result<i64> {
        let result = sqlx::query(
            "INSERT INTO trades \
             (opened_at, slug, side, entry_price, size_usdc, qty_shares, \
              score, btc_vel, ptb_pct, btc_price, eth_price, confirm_ticks) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(&e.opened_at)
        .bind(&e.slug)
        .bind(&e.side)
        .bind(e.entry_price)
        .bind(e.size_usdc)
        .bind(e.qty_shares)
        .bind(e.score)
        .bind(e.btc_vel)
        .bind(e.ptb_pct)
        .bind(e.btc_price)
        .bind(e.eth_price)
        .bind(e.confirm_ticks as i64)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    pub async fn close_trade(&self, id: i64, exit_price: f64, pnl_usdc: f64, reason: &str) -> Result<()> {
        sqlx::query(
            "UPDATE trades \
             SET closed_at = ?, exit_price = ?, pnl_usdc = ?, exit_reason = ? \
             WHERE id = ? AND closed_at IS NULL"
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(exit_price)
        .bind(pnl_usdc)
        .bind(reason)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn sample_entry() -> TradeEntry {
        TradeEntry {
            opened_at:     Utc::now().to_rfc3339(),
            slug:          "eth-updown-5m-1746000300".to_string(),
            side:          "Up".to_string(),
            entry_price:   0.52,
            size_usdc:     15.0,
            qty_shares:    28.84,
            score:         0.34,
            btc_vel:       12.5,
            ptb_pct:       1.2,
            btc_price:     62000.0,
            eth_price:     3100.0,
            confirm_ticks: 3,
        }
    }

    #[tokio::test]
    async fn test_insert_returns_positive_id() {
        let db = Db::open("sqlite::memory:").await.unwrap();
        let id = db.insert_trade(&sample_entry()).await.unwrap();
        assert!(id > 0);
    }

    #[tokio::test]
    async fn test_insert_returns_unique_ids() {
        let db = Db::open("sqlite::memory:").await.unwrap();
        let id1 = db.insert_trade(&sample_entry()).await.unwrap();
        let id2 = db.insert_trade(&sample_entry()).await.unwrap();
        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn test_close_trade_and_idempotent() {
        let db = Db::open("sqlite::memory:").await.unwrap();
        let id = db.insert_trade(&sample_entry()).await.unwrap();
        db.close_trade(id, 0.58, 1.73, "TakeProfit").await.unwrap();
        // Second call is a no-op, must not error
        db.close_trade(id, 0.60, 2.00, "TakeProfit").await.unwrap();
    }
}

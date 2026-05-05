CREATE TABLE IF NOT EXISTS trades (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    opened_at     TEXT    NOT NULL,
    closed_at     TEXT,
    slug          TEXT    NOT NULL,
    side          TEXT    NOT NULL,
    entry_price   REAL    NOT NULL,
    exit_price    REAL,
    size_usdc     REAL    NOT NULL,
    qty_shares    REAL    NOT NULL,
    pnl_usdc      REAL,
    exit_reason   TEXT,
    score         REAL    NOT NULL,
    btc_vel       REAL    NOT NULL,
    ptb_pct       REAL    NOT NULL,
    btc_price     REAL    NOT NULL,
    eth_price     REAL    NOT NULL,
    confirm_ticks INTEGER NOT NULL
);

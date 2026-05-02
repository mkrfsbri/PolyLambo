use eth5m_bot::clob::ClobClient;

// ── HMAC header format ────────────────────────────────────────────────────────

#[test]
fn hmac_header_format() {
    let client = ClobClient::new(
        "test-api-key".to_string(),
        "test-secret".to_string(),
        "test-passphrase".to_string(),
        "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
        "0x0000000000000000000000000000000000000000".to_string(),
    )
    .unwrap();

    let headers = client.hmac_auth_headers("GET", "/ok", "").unwrap();

    // All five auth headers must be present
    assert!(headers.contains_key("POLY_ADDRESS"),    "missing POLY_ADDRESS");
    assert!(headers.contains_key("POLY_SIGNATURE"),  "missing POLY_SIGNATURE");
    assert!(headers.contains_key("POLY-TIMESTAMP"),  "missing POLY-TIMESTAMP");
    assert!(headers.contains_key("POLY-API-KEY"),    "missing POLY-API-KEY");
    assert!(headers.contains_key("POLY-PASSPHRASE"), "missing POLY-PASSPHRASE");

    // API key and address are passed through unchanged
    let api_key = headers["POLY-API-KEY"].to_str().unwrap();
    assert_eq!(api_key, "test-api-key");

    let address = headers["POLY_ADDRESS"].to_str().unwrap();
    assert_eq!(address, "0x0000000000000000000000000000000000000000");

    // Signature must be a non-empty base64 string (44 chars for SHA-256 output)
    let sig = headers["POLY_SIGNATURE"].to_str().unwrap();
    assert!(!sig.is_empty(), "signature must not be empty");
    assert_eq!(sig.len(), 44, "SHA-256 HMAC base64 should be 44 chars");

    // Timestamp must be a valid unix seconds string
    let ts_str = headers["POLY-TIMESTAMP"].to_str().unwrap();
    let ts: u64 = ts_str.parse().expect("timestamp must be a number");
    assert!(ts > 1_700_000_000, "timestamp looks too old: {ts}");
}

#[test]
fn hmac_signature_differs_by_method() {
    let client = ClobClient::new(
        "k".to_string(),
        "secret".to_string(),
        "p".to_string(),
        "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
        "0x0000000000000000000000000000000000000000".to_string(),
    )
    .unwrap();

    let h_get  = client.hmac_auth_headers("GET",    "/order", "").unwrap();
    let h_post = client.hmac_auth_headers("POST",   "/order", "{}").unwrap();
    let h_del  = client.hmac_auth_headers("DELETE", "/order", "").unwrap();

    // Signatures over different messages must differ (exceedingly unlikely to collide)
    let sig_get  = h_get ["POLY_SIGNATURE"].to_str().unwrap();
    let sig_post = h_post["POLY_SIGNATURE"].to_str().unwrap();
    let sig_del  = h_del ["POLY_SIGNATURE"].to_str().unwrap();

    assert_ne!(sig_get, sig_post);
    assert_ne!(sig_get, sig_del);
    assert_ne!(sig_post, sig_del);
}

// ── client creation ───────────────────────────────────────────────────────────

#[test]
fn place_order_success_dry_run_client_creates() {
    // Verifies ClobClient::new_dry_run() can be constructed without credentials.
    let client = ClobClient::new_dry_run();
    // Minimal smoke-test: HMAC headers can be generated
    let headers = client.hmac_auth_headers("POST", "/order", "{}").unwrap();
    assert!(headers.contains_key("POLY_SIGNATURE"));
}

#[test]
fn cancel_order_success_dry_run_client_creates() {
    // Verifies cancel-related paths don't panic during header construction.
    let client = ClobClient::new_dry_run();
    let headers = client.hmac_auth_headers("DELETE", "/orders/abc123", "").unwrap();
    assert!(headers.contains_key("POLY-TIMESTAMP"));
}

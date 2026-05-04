use eth5m_bot::clob::ClobClient;

// ── client construction ───────────────────────────────────────────────────────

#[test]
fn dry_run_client_creates() {
    // Should succeed without any credentials.
    let _client = ClobClient::new_dry_run();
}

#[test]
fn live_client_creates_with_valid_key() {
    // Hardhat/Anvil account #0 — safe test private key.
    let client = ClobClient::new(
        "test-api-key".to_string(),
        "test-secret".to_string(),
        "test-passphrase".to_string(),
        "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
        String::new(), // plain EOA, no proxy
    );
    assert!(client.is_ok(), "live client creation failed: {:?}", client.err());
}

#[test]
fn live_client_creates_with_proxy_address() {
    // Use a non-zero proxy address; polyfill-rs rejects the zero address as funder.
    let client = ClobClient::new(
        "k".to_string(),
        "s".to_string(),
        "p".to_string(),
        "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
        "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266".to_string(), // Hardhat #0
    );
    assert!(client.is_ok(), "proxy-mode client creation failed: {:?}", client.err());
}

#[test]
fn invalid_private_key_returns_error() {
    let result = ClobClient::new(
        "k".to_string(),
        "s".to_string(),
        "p".to_string(),
        "not-a-valid-hex-key",
        String::new(),
    );
    assert!(result.is_err(), "expected error for invalid private key");
}

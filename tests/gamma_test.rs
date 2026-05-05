use eth5m_bot::gamma::{slug_for_ts, expiry_secs_for_ts};

// ── slug generation ───────────────────────────────────────────────────────────

#[test]
fn slug_generation() {
    // Slugs use window OPEN time: ts 1746000000 is exactly the open → slug ends in 1746000000
    assert_eq!(slug_for_ts(1_746_000_000), "eth-updown-5m-1746000000");
}

#[test]
fn slug_boundary_edge() {
    // Exactly on a 300s boundary → the new window just opened at that timestamp
    assert_eq!(slug_for_ts(1_746_000_000), "eth-updown-5m-1746000000");
    assert_eq!(slug_for_ts(1_746_000_300), "eth-updown-5m-1746000300");
}

#[test]
fn slug_mid_window() {
    // 150s into the window that opened at 1746000000
    assert_eq!(slug_for_ts(1_746_000_150), "eth-updown-5m-1746000000");
}

#[test]
fn slug_one_before_boundary() {
    // 1s before next boundary → still in the 1746000000 window
    assert_eq!(slug_for_ts(1_746_000_299), "eth-updown-5m-1746000000");
}

#[test]
fn slug_one_after_boundary() {
    // 1s past the 1746000300 boundary → now in the 1746000300 window
    assert_eq!(slug_for_ts(1_746_000_301), "eth-updown-5m-1746000300");
}

#[test]
fn slug_timestamp_is_multiple_of_300() {
    for ts in [0u64, 1, 149, 150, 299, 300, 301, 1_746_000_000] {
        let slug = slug_for_ts(ts);
        let suffix: u64 = slug.trim_start_matches("eth-updown-5m-").parse().unwrap();
        assert_eq!(suffix % 300, 0, "slug for ts={ts} has non-multiple-of-300 suffix");
    }
}

// ── expiry countdown ──────────────────────────────────────────────────────────

#[test]
fn expiry_mid_window() {
    assert_eq!(expiry_secs_for_ts(1_746_000_150), 150);
}

#[test]
fn expiry_on_boundary() {
    assert_eq!(expiry_secs_for_ts(1_746_000_000), 300);
}

#[test]
fn expiry_one_before_boundary() {
    assert_eq!(expiry_secs_for_ts(1_746_000_299), 1);
}

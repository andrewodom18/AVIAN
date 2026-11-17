//! Shared conservative probe admission and bounded exploration for native hosts.

/// Select a fresh, quiet, explicitly unmetered direct session for an automatic probe.
/// The engine checks current authorization and congestion again when admitting it.
pub fn eligible_probe_candidate(link: &serde_json::Value) -> Option<(String, u32)> {
    // The native observation uses a boolean; accepting a numeric count also keeps
    // older observation adapters usable without treating missing data as empty.
    let queue_empty = link["queued_packets"].as_bool() == Some(false)
        || link["queued_packets"].as_u64() == Some(0);
    let loss = link["loss_fraction"].as_f64()?;
    let jitter = link["jitter_ms"].as_f64()?;
    if link["metered"].as_bool() != Some(false)
        || link["policy_valid"].as_bool() != Some(true)
        || link["quality_fresh"].as_bool() != Some(true)
        || !queue_empty
        || !loss.is_finite()
        || !(0.0..=0.05).contains(&loss)
        || !jitter.is_finite()
        || !(0.0..=100.0).contains(&jitter)
    {
        return None;
    }
    let connection = link["connection_id"].as_str()?;
    if connection.is_empty() {
        return None;
    }
    let peer = u32::try_from(link["peer_id"].as_u64()?).ok()?;
    Some((connection.to_owned(), peer))
}

/// Choose the next bounded experiment from authenticated delivery evidence.
/// Only the engine's confirmed delivery lower bound is capacity evidence; this
/// offered rate must never be published as measured capacity. Requests outside
/// the engine's rate range are clamped, and failed/incomplete probes reduce it.
pub fn next_probe_rate(
    offered_bps: u64,
    duration_ms: u16,
    evidence: Option<&serde_json::Value>,
) -> u64 {
    let offered_bps = offered_bps.clamp(64_000, 8_000_000);
    let reduced_rate = (offered_bps / 2).max(64_000);
    let Some(evidence) = evidence else {
        return reduced_rate;
    };
    let Some(rate) = evidence["delivered_rate_lower_bound_bps"].as_u64() else {
        return reduced_rate;
    };
    let offered_bytes = evidence["offered_payload_bytes"].as_u64().unwrap_or(0);
    let actual_offer = offered_bytes
        .checked_mul(8_000)
        .and_then(|bits| bits.checked_div(u64::from(duration_ms)));
    let complete_and_timely = offered_bytes >= 64
        && evidence["accepted_payload_bytes"].as_u64() == Some(offered_bytes)
        && evidence["delivered_payload_bytes"].as_u64() == Some(offered_bytes)
        && evidence["measurement_duration_ms"]
            .as_u64()
            .is_some_and(|elapsed| elapsed > 0 && elapsed <= u64::from(duration_ms) * 2);
    if complete_and_timely && actual_offer.is_some_and(|offer| rate >= offer.div_ceil(2)) {
        // Permit limited exploration despite round-trip overhead. Both the last
        // admitted offer and confirmed delivery constrain the next request.
        return offered_bps
            .saturating_mul(2)
            .min(rate.saturating_mul(2))
            .clamp(64_000, 8_000_000);
    }
    reduced_rate.min(rate.max(64_000))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delivery_evidence(bytes: u64, rate: u64, elapsed_ms: u64) -> serde_json::Value {
        serde_json::json!({"offered_payload_bytes": bytes, "accepted_payload_bytes": bytes,
            "delivered_payload_bytes": bytes, "measurement_duration_ms": elapsed_ms,
            "delivered_rate_lower_bound_bps": rate})
    }

    #[test]
    fn native_boolean_queue_state_permits_only_healthy_authorized_idle_sessions() {
        let mut link = serde_json::json!({"peer_id":42,"connection_id":"direct-session",
            "metered":false,"queued_packets":false,"quality_fresh":true,"policy_valid":true,
            "loss_fraction":0.0,"jitter_ms":2.0});
        assert_eq!(
            eligible_probe_candidate(&link),
            Some(("direct-session".into(), 42))
        );
        for (field, value) in [
            ("queued_packets", serde_json::json!(true)),
            ("metered", serde_json::json!(true)),
            ("policy_valid", serde_json::json!(false)),
            ("quality_fresh", serde_json::json!(false)),
            ("loss_fraction", serde_json::json!(0.2)),
            ("jitter_ms", serde_json::json!(-1)),
            ("connection_id", serde_json::json!("")),
            ("peer_id", serde_json::json!(u64::MAX)),
        ] {
            let previous = link[field].clone();
            link[field] = value;
            assert_eq!(eligible_probe_candidate(&link), None, "{field}");
            link[field] = previous;
        }
    }

    #[test]
    fn rounded_offer_and_round_trip_overhead_allow_bounded_exploration() {
        let evidence = delivery_evidence(384, 43_885, 70);
        assert_eq!(next_probe_rate(64_000, 50, Some(&evidence)), 87_770);
        let evidence = delivery_evidence(384, 61_440, 50);
        assert_eq!(next_probe_rate(64_000, 50, Some(&evidence)), 122_880);
        // A supplied high delivery rate still cannot increase an offer more than twofold.
        let evidence = delivery_evidence(384, 8_000_000, 50);
        assert_eq!(next_probe_rate(64_000, 50, Some(&evidence)), 128_000);
    }

    #[test]
    fn partial_late_or_insufficient_delivery_reduces_the_next_experiment() {
        let mut evidence = delivery_evidence(1536, 200_000, 75);
        evidence["accepted_payload_bytes"] = serde_json::json!(1408);
        assert_eq!(next_probe_rate(256_000, 50, Some(&evidence)), 128_000);
        evidence = delivery_evidence(1536, 200_000, 75);
        evidence["delivered_payload_bytes"] = serde_json::json!(1408);
        assert_eq!(next_probe_rate(256_000, 50, Some(&evidence)), 128_000);
        evidence = delivery_evidence(1536, 200_000, 101);
        assert_eq!(next_probe_rate(256_000, 50, Some(&evidence)), 128_000);
        evidence = delivery_evidence(1536, 100_000, 75);
        assert_eq!(next_probe_rate(256_000, 50, Some(&evidence)), 100_000);
        assert_eq!(next_probe_rate(256_000, 50, None), 128_000);
    }

    #[test]
    fn rate_limits_and_invalid_duration_remain_bounded() {
        let evidence = delivery_evidence(500_000, 8_000_000, 500);
        assert_eq!(next_probe_rate(8_000_000, 500, Some(&evidence)), 8_000_000);
        assert_eq!(next_probe_rate(256_000, 0, Some(&evidence)), 128_000);
        assert_eq!(next_probe_rate(0, 50, None), 64_000);
    }
}

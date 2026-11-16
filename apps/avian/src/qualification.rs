use serde::Serialize;

pub const ENGINE_VERSION: &str = "2.6.4";
pub const ENGINE_COMMIT: &str = "8428a89d2dabc94c97d370ec607c6ca142473626";

#[derive(Clone, Debug, Serialize)]
pub struct QualificationStatus {
    pub engine: &'static str,
    pub version: &'static str,
    pub commit: &'static str,
    pub network_control_enabled: bool,
    pub blocking_findings: &'static [&'static str],
    pub required_checks: &'static [&'static str],
}

impl Default for QualificationStatus {
    fn default() -> Self {
        Self {
            engine: "EasyTier",
            version: ENGINE_VERSION,
            commit: ENGINE_COMMIT,
            network_control_enabled: false,
            blocking_findings: &[
                "Authenticated forwarding is an isolated candidate; traffic policy, live revocation, recovery, and platform qualification are still required before runtime adoption.",
            ],
            required_checks: &[
                "authenticated_hop_forwarding_and_replay_protection",
                "offline_admission_after_restart",
                "new_sessions_through_non_administrator_relays",
                "per_class_physical_path_selection",
                "metered_restrictions_in_both_directions",
                "warm_alternate_recovery_within_two_seconds",
                "congestion_and_progressive_growth",
                "android_network_binding_and_background_operation",
            ],
        }
    }
}

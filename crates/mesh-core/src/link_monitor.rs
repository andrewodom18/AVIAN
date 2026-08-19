use serde::{Deserialize, Serialize};

use crate::{
    RelayLinkObservation, StreamCasterCapabilities, StreamCasterEffectiveSettings,
    StreamCasterRfLink, TransportKind,
};

pub const LINK_MONITOR_SCHEMA_VERSION: u16 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioApiObservation {
    pub name: String,
    pub observed_at_ms: u64,
    pub api_fresh: bool,
    pub capabilities: Option<StreamCasterCapabilities>,
    pub effective_settings: Option<StreamCasterEffectiveSettings>,
    pub rf_links: Vec<StreamCasterRfLink>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerProbeObservation {
    pub peer: String,
    pub underlay: TransportKind,
    pub observed_at_ms: u64,
    pub sample_window_ms: u64,
    pub sent_packets: u16,
    pub received_packets: u16,
    pub latency_ms: Option<f64>,
    pub loss_ratio: f64,
    pub goodput_bps: Option<u64>,
    pub stability: Option<f64>,
    pub reachable: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkMonitorObservation {
    pub schema_version: u16,
    pub observed_at_ms: u64,
    pub radios: Vec<RadioApiObservation>,
    pub probes: Vec<PeerProbeObservation>,
    pub relay_observations: Vec<RelayLinkObservation>,
    pub degradation_reasons: Vec<String>,
}

impl LinkMonitorObservation {
    pub fn validate(&self) -> bool {
        self.schema_version == LINK_MONITOR_SCHEMA_VERSION
            && self.observed_at_ms > 0
            && self.radios.len() <= 16
            && self.probes.len() <= 64
            && self.relay_observations.len() <= 64
            && self.radios.iter().all(|radio| {
                !radio.name.trim().is_empty()
                    && radio.name.len() <= 128
                    && radio.rf_links.len() <= 64
                    && radio.errors.len() <= 16
            })
            && self.probes.iter().all(|probe| {
                !probe.peer.trim().is_empty()
                    && probe.peer.len() <= 128
                    && probe.sample_window_ms > 0
                    && probe.received_packets <= probe.sent_packets
                    && probe.loss_ratio.is_finite()
                    && (0.0..=1.0).contains(&probe.loss_ratio)
                    && probe
                        .latency_ms
                        .is_none_or(|value| value.is_finite() && value >= 0.0)
                    && probe
                        .stability
                        .is_none_or(|value| value.is_finite() && (0.0..=1.0).contains(&value))
            })
            && self
                .relay_observations
                .iter()
                .all(RelayLinkObservation::is_well_formed)
            && self.degradation_reasons.len() <= 64
    }
}

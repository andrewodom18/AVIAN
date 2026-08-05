use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{NodeId, MAX_SUPPORTED_SWARM_SIZE, MIN_SUPPORTED_SWARM_SIZE, SYSTEM_MAX_MSL_FT};

pub const RADIO_CONFIG_SCHEMA_VERSION: u16 = 1;
pub const RADIO_VALIDATION_TARGET_NODES: usize = 150;
pub const DEFAULT_ROUTINE_PACKET_BYTES: u32 = 3 * 1024;
pub const DEFAULT_ROUTINE_PACKETS_PER_SECOND: f64 = 1.0;
pub const DEFAULT_STRESS_EXTRA_BPS_PER_NODE: u64 = 5_500_000;
pub const MAX_STREAMCASTER_LINK_DISTANCE_M: u32 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RadioConfigAuthority {
    Arc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamCasterModel {
    Sc4200,
    Sc4200Ep,
    Sl4200,
    Sc4400,
    Sc4400E,
    Sc4400X,
    Sl5200,
    /// Planning profile derived from the documented SL5200 family plus the
    /// public LITE datasheet. A live capability read is mandatory before use.
    Sl5200LiteEstimated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RadioProfileEvidence {
    Manual,
    EstimatedRequiresLiveCapabilities,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelBandwidthMhz {
    #[serde(rename = "1.25")]
    Mhz1_25,
    #[serde(rename = "2.5")]
    Mhz2_5,
    #[serde(rename = "5")]
    Mhz5,
    #[serde(rename = "10")]
    Mhz10,
    #[serde(rename = "20")]
    Mhz20,
}

impl ChannelBandwidthMhz {
    pub fn as_mhz(self) -> f64 {
        match self {
            Self::Mhz1_25 => 1.25,
            Self::Mhz2_5 => 2.5,
            Self::Mhz5 => 5.0,
            Self::Mhz10 => 10.0,
            Self::Mhz20 => 20.0,
        }
    }

    pub fn api_value(self) -> &'static str {
        match self {
            Self::Mhz1_25 => "1.25",
            Self::Mhz2_5 => "2.5",
            Self::Mhz5 => "5",
            Self::Mhz10 => "10",
            Self::Mhz20 => "20",
        }
    }

    pub fn is_optional_narrowband(self) -> bool {
        matches!(self, Self::Mhz1_25 | Self::Mhz2_5)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RadioNodeRole {
    Airborne,
    ControlStation,
    Relay,
    Gateway,
}

impl RadioNodeRole {
    pub fn is_gateway(self) -> bool {
        matches!(self, Self::ControlStation | Self::Gateway)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum TransmitPowerMode {
    MaxSupported,
    TargetDbm { dbm: u8 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamCasterNetworkSettings {
    pub network_id: String,
    pub center_frequency_mhz: f64,
    pub bandwidth_mhz: ChannelBandwidthMhz,
    pub average_node_distance_m: f64,
    pub maximum_node_distance_m: f64,
    /// If omitted, AVIAN uses 115% of the planned maximum separation.
    pub link_distance_m: Option<u32>,
    pub routing_beacon_period_ms: u32,
    pub encryption_required: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioNodeGroup {
    pub group_id: String,
    pub node_id_prefix: String,
    pub percentage: f64,
    pub model: StreamCasterModel,
    pub role: RadioNodeRole,
    pub altitude_msl_ft: f64,
    pub transmit_power: TransmitPowerMode,
    pub antenna_mask: u8,
    pub beamforming: bool,
    /// Optional measured usable UDP capacity for this installed radio,
    /// antenna, airframe, channel, and environment combination.
    pub field_calibrated_udp_capacity_bps: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioFleetDefinition {
    pub total_nodes: usize,
    pub groups: Vec<RadioNodeGroup>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioTrafficProfile {
    #[serde(default = "default_routine_packet_bytes")]
    pub routine_packet_bytes: u32,
    #[serde(default = "default_routine_packets_per_second")]
    pub routine_packets_per_second: f64,
    #[serde(default = "default_stress_extra_bps_per_node")]
    pub stress_extra_bps_per_node: u64,
}

impl Default for RadioTrafficProfile {
    fn default() -> Self {
        Self {
            routine_packet_bytes: DEFAULT_ROUTINE_PACKET_BYTES,
            routine_packets_per_second: DEFAULT_ROUTINE_PACKETS_PER_SECOND,
            stress_extra_bps_per_node: DEFAULT_STRESS_EXTRA_BPS_PER_NODE,
        }
    }
}

const fn default_routine_packet_bytes() -> u32 {
    DEFAULT_ROUTINE_PACKET_BYTES
}

const fn default_routine_packets_per_second() -> f64 {
    DEFAULT_ROUTINE_PACKETS_PER_SECOND
}

const fn default_stress_extra_bps_per_node() -> u64 {
    DEFAULT_STRESS_EXTRA_BPS_PER_NODE
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArcRadioConfiguration {
    pub schema_version: u16,
    pub authority: RadioConfigAuthority,
    pub generation: u64,
    pub network: StreamCasterNetworkSettings,
    pub fleet: RadioFleetDefinition,
    #[serde(default)]
    pub traffic: RadioTrafficProfile,
    /// Whether the eventual hardware adapter should persist effective values
    /// to radio flash after verification. This is only planning intent.
    pub persist_after_apply: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RadioNodeAssignment {
    pub node_id: NodeId,
    pub group_id: String,
    pub model: StreamCasterModel,
    pub role: RadioNodeRole,
    pub altitude_msl_ft: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RadioTrafficLoad {
    pub per_node_payload_bps: u64,
    pub aggregate_payload_bps: u64,
    /// Evenly divided offered load is a planning floor. Actual gateway and
    /// relay load depends on the measured routing tree and retransmissions.
    pub average_payload_bps_per_gateway: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RadioPlanAssessment {
    pub node_count: usize,
    pub gateway_count: usize,
    pub resolved_link_distance_m: u32,
    pub assignments: Vec<RadioNodeAssignment>,
    pub routine_load: RadioTrafficLoad,
    pub stress_load: RadioTrafficLoad,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SilvusStepEffect {
    Live,
    SoftBoot,
    WaitForReconnect,
    Persist,
    Verify,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SilvusApiStep {
    pub effect: SilvusStepEffect,
    pub method: Option<String>,
    pub params: Vec<String>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SilvusGroupApplyTemplate {
    pub group_id: String,
    pub requires_password_authenticated_api: bool,
    pub requires_live_supported_frequency_profile: bool,
    pub steps: Vec<SilvusApiStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamCasterModelProfile {
    pub antenna_count: u8,
    pub published_peak_data_rate_mbps: u16,
    pub evidence: RadioProfileEvidence,
}

impl StreamCasterModel {
    pub fn profile(self) -> StreamCasterModelProfile {
        match self {
            Self::Sl4200 => StreamCasterModelProfile {
                antenna_count: 2,
                published_peak_data_rate_mbps: 20,
                evidence: RadioProfileEvidence::Manual,
            },
            Self::Sc4200 => StreamCasterModelProfile {
                antenna_count: 2,
                published_peak_data_rate_mbps: 100,
                evidence: RadioProfileEvidence::Manual,
            },
            Self::Sc4200Ep | Self::Sc4400 | Self::Sc4400E | Self::Sc4400X => {
                StreamCasterModelProfile {
                    antenna_count: 4,
                    published_peak_data_rate_mbps: 100,
                    evidence: RadioProfileEvidence::Manual,
                }
            }
            Self::Sl5200 => StreamCasterModelProfile {
                antenna_count: 2,
                published_peak_data_rate_mbps: 100,
                evidence: RadioProfileEvidence::Manual,
            },
            Self::Sl5200LiteEstimated => StreamCasterModelProfile {
                antenna_count: 2,
                published_peak_data_rate_mbps: 100,
                evidence: RadioProfileEvidence::EstimatedRequiresLiveCapabilities,
            },
        }
    }

    pub fn planning_supports(self, bandwidth: ChannelBandwidthMhz) -> bool {
        match self {
            Self::Sl4200 => matches!(
                bandwidth,
                ChannelBandwidthMhz::Mhz1_25
                    | ChannelBandwidthMhz::Mhz2_5
                    | ChannelBandwidthMhz::Mhz5
            ),
            _ => true,
        }
    }
}

impl ArcRadioConfiguration {
    pub fn assess(&self) -> Result<RadioPlanAssessment, RadioConfigError> {
        self.validate_header()?;
        let assignments = self.fleet.expand()?;
        let gateway_count = assignments
            .iter()
            .filter(|assignment| assignment.role.is_gateway())
            .count();
        if gateway_count == 0 {
            return Err(RadioConfigError::NoGateway);
        }
        self.traffic.validate()?;

        let resolved_link_distance_m = self.network.validate()?;
        let routine_per_node = payload_rate_bps(
            self.traffic.routine_packet_bytes,
            self.traffic.routine_packets_per_second,
        )?;
        let stress_per_node = routine_per_node
            .checked_add(self.traffic.stress_extra_bps_per_node)
            .ok_or(RadioConfigError::TrafficOverflow)?;
        let routine_load = traffic_load(routine_per_node, assignments.len(), gateway_count)?;
        let stress_load = traffic_load(stress_per_node, assignments.len(), gateway_count)?;

        let mut warnings = Vec::new();
        if assignments.len() < RADIO_VALIDATION_TARGET_NODES {
            warnings.push(format!(
                "configuration has {} nodes; the requested radio validation target is at least {}",
                assignments.len(),
                RADIO_VALIDATION_TARGET_NODES
            ));
        }
        warnings.push(
            "verify center frequency, bandwidth, and antenna mask against each radio's live supported_frequency_profiles response before apply"
                .to_owned(),
        );
        warnings.push(
            "average gateway load is only an even-share floor; validate the measured routing tree, retransmissions, multicast behavior, and per-link airtime"
                .to_owned(),
        );

        let mut all_calibrated = true;
        for group in &self.fleet.groups {
            if !group.model.planning_supports(self.network.bandwidth_mhz) {
                return Err(RadioConfigError::UnsupportedPlanningBandwidth {
                    group_id: group.group_id.clone(),
                    model: group.model,
                    bandwidth: self.network.bandwidth_mhz,
                });
            }
            group.validate()?;
            if group.model.profile().evidence
                == RadioProfileEvidence::EstimatedRequiresLiveCapabilities
            {
                warnings.push(format!(
                    "group {:?} uses the estimated SL5200 LITE profile; replace estimates with live capabilities and field measurements",
                    group.group_id
                ));
            }
            if self.network.bandwidth_mhz.is_optional_narrowband()
                && !matches!(group.model, StreamCasterModel::Sl4200)
            {
                warnings.push(format!(
                    "group {:?} uses an optional narrow bandwidth that may require the matching hardware option/license",
                    group.group_id
                ));
            }
            if group.field_calibrated_udp_capacity_bps.is_none() {
                all_calibrated = false;
            }
            if matches!(group.transmit_power, TransmitPowerMode::TargetDbm { dbm } if dbm < 10) {
                warnings.push(format!(
                    "group {:?} targets less than 10 dBm; the API manual says actual output accuracy is specified only from 10-39 dBm",
                    group.group_id
                ));
            }
        }
        if !all_calibrated {
            warnings.push(
                "one or more groups lack field-calibrated usable UDP capacity; published peak data rate is not treated as mission capacity"
                    .to_owned(),
            );
        } else {
            warnings.push(
                "field-calibrated per-radio capacity is present, but shared mesh capacity is not summed across nodes; validate routing and airtime contention separately"
                    .to_owned(),
            );
        }

        warnings.sort();
        warnings.dedup();
        Ok(RadioPlanAssessment {
            node_count: assignments.len(),
            gateway_count,
            resolved_link_distance_m,
            assignments,
            routine_load,
            stress_load,
            warnings,
        })
    }

    pub fn silvus_apply_templates(
        &self,
    ) -> Result<Vec<SilvusGroupApplyTemplate>, RadioConfigError> {
        let assessment = self.assess()?;
        self.fleet
            .groups
            .iter()
            .map(|group| {
                let mut steps = vec![SilvusApiStep {
                    effect: SilvusStepEffect::Verify,
                    method: Some("supported_frequency_profiles".to_owned()),
                    params: Vec::new(),
                    detail: "confirm the exact frequency/bandwidth/antenna-mask tuple on this radio"
                        .to_owned(),
                }];
                steps.push(SilvusApiStep {
                    effect: SilvusStepEffect::SoftBoot,
                    method: Some("freq_bw".to_owned()),
                    params: vec![
                        format_frequency(self.network.center_frequency_mhz),
                        self.network.bandwidth_mhz.api_value().to_owned(),
                    ],
                    detail: "set center frequency and bandwidth atomically; the radio services soft-boot"
                        .to_owned(),
                });
                steps.push(SilvusApiStep {
                    effect: SilvusStepEffect::WaitForReconnect,
                    method: None,
                    params: Vec::new(),
                    detail: "wait for the JSON-RPC service and mesh status to recover before issuing another call"
                        .to_owned(),
                });
                steps.extend([
                    api_step("nw_name", vec![self.network.network_id.clone()]),
                    api_step(
                        "max_link_distance",
                        vec![assessment.resolved_link_distance_m.to_string()],
                    ),
                    api_step(
                        "routing_beacon_period",
                        vec![self.network.routing_beacon_period_ms.to_string()],
                    ),
                    api_step("tx_ant_mask", vec![group.antenna_mask.to_string()]),
                    api_step("rx_ant_mask", vec![group.antenna_mask.to_string()]),
                    api_step(
                        "beamform_enable",
                        vec![if group.beamforming { "1" } else { "0" }.to_owned()],
                    ),
                ]);
                match group.transmit_power {
                    TransmitPowerMode::MaxSupported => {
                        steps.push(api_step("enable_max_power", vec!["1".to_owned()]));
                    }
                    TransmitPowerMode::TargetDbm { dbm } => {
                        steps.push(api_step("enable_max_power", vec!["0".to_owned()]));
                        steps.push(api_step("power_dBm", vec![dbm.to_string()]));
                    }
                }
                steps.push(SilvusApiStep {
                    effect: SilvusStepEffect::Verify,
                    method: Some("print_all_settings".to_owned()),
                    params: Vec::new(),
                    detail: "compare effective values with the Arc-owned desired generation"
                        .to_owned(),
                });
                if self.network.encryption_required {
                    steps.push(SilvusApiStep {
                        effect: SilvusStepEffect::Verify,
                        method: Some("enc_disable".to_owned()),
                        params: Vec::new(),
                        detail: "verify RF encryption is enabled without reading or publishing key material"
                            .to_owned(),
                    });
                    steps.push(SilvusApiStep {
                        effect: SilvusStepEffect::Verify,
                        method: Some("enc_profile".to_owned()),
                        params: Vec::new(),
                        detail: "verify the effective encryption profile without reading or publishing key material"
                            .to_owned(),
                    });
                }
                if self.persist_after_apply {
                    let mut settings = vec![
                        "freq",
                        "bw",
                        "nw_name",
                        "max_link_distance",
                        "routing_beacon_period",
                        "tx_ant_mask",
                        "rx_ant_mask",
                        "beamform_enable",
                        "enable_max_power",
                    ];
                    if matches!(group.transmit_power, TransmitPowerMode::TargetDbm { .. }) {
                        settings.push("power_dBm");
                    }
                    for setting in settings {
                        steps.push(SilvusApiStep {
                            effect: SilvusStepEffect::Persist,
                            method: Some("setenvlinsingle".to_owned()),
                            params: vec![setting.to_owned()],
                            detail: format!("persist the verified current {setting} value to flash"),
                        });
                    }
                }
                Ok(SilvusGroupApplyTemplate {
                    group_id: group.group_id.clone(),
                    requires_password_authenticated_api: true,
                    requires_live_supported_frequency_profile: true,
                    steps,
                })
            })
            .collect()
    }

    fn validate_header(&self) -> Result<(), RadioConfigError> {
        if self.schema_version != RADIO_CONFIG_SCHEMA_VERSION {
            return Err(RadioConfigError::UnsupportedSchema(self.schema_version));
        }
        if self.generation == 0 {
            return Err(RadioConfigError::ZeroGeneration);
        }
        if self.authority != RadioConfigAuthority::Arc {
            return Err(RadioConfigError::WrongAuthority);
        }
        Ok(())
    }
}

impl StreamCasterNetworkSettings {
    fn validate(&self) -> Result<u32, RadioConfigError> {
        let network_id_len = self.network_id.chars().count();
        if network_id_len == 0
            || network_id_len > 32
            || !self.network_id.chars().all(|character| {
                character.is_alphanumeric() || character == ' ' || character == '-'
            })
        {
            return Err(RadioConfigError::InvalidNetworkId);
        }
        if !self.center_frequency_mhz.is_finite()
            || !(300.0..=6_000.0).contains(&self.center_frequency_mhz)
            || (self.center_frequency_mhz * 10.0 - (self.center_frequency_mhz * 10.0).round()).abs()
                > 1e-6
        {
            return Err(RadioConfigError::InvalidCenterFrequency(
                self.center_frequency_mhz,
            ));
        }
        if !self.average_node_distance_m.is_finite()
            || !self.maximum_node_distance_m.is_finite()
            || self.average_node_distance_m <= 0.0
            || self.maximum_node_distance_m < self.average_node_distance_m
        {
            return Err(RadioConfigError::InvalidNodeDistances);
        }
        if !(100..=2_000).contains(&self.routing_beacon_period_ms) {
            return Err(RadioConfigError::InvalidRoutingBeaconPeriod(
                self.routing_beacon_period_ms,
            ));
        }
        let recommended = (self.maximum_node_distance_m * 1.15).ceil();
        if recommended > f64::from(MAX_STREAMCASTER_LINK_DISTANCE_M) {
            return Err(RadioConfigError::LinkDistanceTooLarge);
        }
        let minimum = (self.maximum_node_distance_m * 1.10).ceil() as u32;
        let resolved = self.link_distance_m.unwrap_or(recommended as u32);
        if resolved < minimum || resolved > MAX_STREAMCASTER_LINK_DISTANCE_M {
            return Err(RadioConfigError::InvalidConfiguredLinkDistance {
                configured_m: resolved,
                minimum_m: minimum,
            });
        }
        Ok(resolved)
    }
}

impl RadioFleetDefinition {
    pub fn expand(&self) -> Result<Vec<RadioNodeAssignment>, RadioConfigError> {
        if !(MIN_SUPPORTED_SWARM_SIZE..=MAX_SUPPORTED_SWARM_SIZE).contains(&self.total_nodes) {
            return Err(RadioConfigError::UnsupportedNodeCount(self.total_nodes));
        }
        if self.groups.is_empty() {
            return Err(RadioConfigError::EmptyGroups);
        }
        let mut group_ids = BTreeSet::new();
        let mut prefixes = BTreeSet::new();
        let mut total_percentage = 0.0;
        for group in &self.groups {
            group.validate()?;
            if !group_ids.insert(group.group_id.clone()) {
                return Err(RadioConfigError::DuplicateGroupId(group.group_id.clone()));
            }
            if !prefixes.insert(group.node_id_prefix.clone()) {
                return Err(RadioConfigError::DuplicateNodePrefix(
                    group.node_id_prefix.clone(),
                ));
            }
            total_percentage += group.percentage;
        }
        if (total_percentage - 100.0).abs() > 0.001 {
            return Err(RadioConfigError::InvalidPercentageTotal(total_percentage));
        }

        let mut counts = BTreeMap::new();
        let mut assigned = 0_usize;
        let mut remainders = Vec::new();
        for group in &self.groups {
            let exact = self.total_nodes as f64 * group.percentage / 100.0;
            let floor = exact.floor() as usize;
            counts.insert(group.group_id.clone(), floor);
            assigned += floor;
            remainders.push((group.group_id.clone(), exact - floor as f64));
        }
        remainders.sort_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| left.0.cmp(&right.0))
        });
        for (group_id, _) in remainders
            .iter()
            .take(self.total_nodes.saturating_sub(assigned))
        {
            *counts.get_mut(group_id).expect("group count exists") += 1;
        }

        let mut assignments = Vec::with_capacity(self.total_nodes);
        for group in &self.groups {
            for index in 1..=counts[&group.group_id] {
                assignments.push(RadioNodeAssignment {
                    node_id: NodeId::from(format!("{}-{index:03}", group.node_id_prefix)),
                    group_id: group.group_id.clone(),
                    model: group.model,
                    role: group.role,
                    altitude_msl_ft: group.altitude_msl_ft,
                });
            }
        }
        assignments.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        Ok(assignments)
    }
}

impl RadioNodeGroup {
    fn validate(&self) -> Result<(), RadioConfigError> {
        if self.group_id.trim().is_empty() || self.node_id_prefix.trim().is_empty() {
            return Err(RadioConfigError::EmptyGroupIdentity);
        }
        if !self.percentage.is_finite() || self.percentage <= 0.0 || self.percentage > 100.0 {
            return Err(RadioConfigError::InvalidGroupPercentage {
                group_id: self.group_id.clone(),
                percentage: self.percentage,
            });
        }
        if !self.altitude_msl_ft.is_finite()
            || self.altitude_msl_ft < 0.0
            || self.altitude_msl_ft > SYSTEM_MAX_MSL_FT
        {
            return Err(RadioConfigError::InvalidGroupAltitude {
                group_id: self.group_id.clone(),
                altitude_ft: self.altitude_msl_ft,
            });
        }
        let available_mask = (1_u16 << self.model.profile().antenna_count) - 1;
        if self.antenna_mask == 0 || u16::from(self.antenna_mask) > available_mask {
            return Err(RadioConfigError::InvalidAntennaMask {
                group_id: self.group_id.clone(),
                mask: self.antenna_mask,
            });
        }
        if let TransmitPowerMode::TargetDbm { dbm } = self.transmit_power {
            if dbm > 39 {
                return Err(RadioConfigError::InvalidTransmitPower {
                    group_id: self.group_id.clone(),
                    dbm,
                });
            }
        }
        if self.field_calibrated_udp_capacity_bps == Some(0) {
            return Err(RadioConfigError::InvalidCalibratedCapacity(
                self.group_id.clone(),
            ));
        }
        Ok(())
    }
}

impl RadioTrafficProfile {
    fn validate(&self) -> Result<(), RadioConfigError> {
        if self.routine_packet_bytes == 0
            || !self.routine_packets_per_second.is_finite()
            || self.routine_packets_per_second <= 0.0
        {
            return Err(RadioConfigError::InvalidTrafficProfile);
        }
        Ok(())
    }
}

fn payload_rate_bps(packet_bytes: u32, packets_per_second: f64) -> Result<u64, RadioConfigError> {
    let rate = f64::from(packet_bytes) * 8.0 * packets_per_second;
    if !rate.is_finite() || rate > u64::MAX as f64 {
        return Err(RadioConfigError::TrafficOverflow);
    }
    Ok(rate.ceil() as u64)
}

fn traffic_load(
    per_node_payload_bps: u64,
    node_count: usize,
    gateway_count: usize,
) -> Result<RadioTrafficLoad, RadioConfigError> {
    let aggregate_payload_bps = per_node_payload_bps
        .checked_mul(node_count as u64)
        .ok_or(RadioConfigError::TrafficOverflow)?;
    Ok(RadioTrafficLoad {
        per_node_payload_bps,
        aggregate_payload_bps,
        average_payload_bps_per_gateway: aggregate_payload_bps.div_ceil(gateway_count as u64),
    })
}

fn api_step(method: &str, params: Vec<String>) -> SilvusApiStep {
    SilvusApiStep {
        effect: SilvusStepEffect::Live,
        method: Some(method.to_owned()),
        params,
        detail: format!("set {method} on this radio"),
    }
}

fn format_frequency(value: f64) -> String {
    let value = format!("{value:.1}");
    value.trim_end_matches('0').trim_end_matches('.').to_owned()
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum RadioConfigError {
    #[error("unsupported radio configuration schema version {0}")]
    UnsupportedSchema(u16),
    #[error("radio configuration generation must be positive")]
    ZeroGeneration,
    #[error("only Arc may author the desired radio configuration")]
    WrongAuthority,
    #[error("radio fleet must contain between 5 and 200 nodes, got {0}")]
    UnsupportedNodeCount(usize),
    #[error("radio fleet must contain at least one group")]
    EmptyGroups,
    #[error("radio group and node prefix cannot be empty")]
    EmptyGroupIdentity,
    #[error("duplicate radio group ID {0:?}")]
    DuplicateGroupId(String),
    #[error("duplicate node ID prefix {0:?}")]
    DuplicateNodePrefix(String),
    #[error("radio group percentages must total 100, got {0}")]
    InvalidPercentageTotal(f64),
    #[error("radio group {group_id:?} has invalid percentage {percentage}")]
    InvalidGroupPercentage { group_id: String, percentage: f64 },
    #[error("radio group {group_id:?} altitude {altitude_ft} ft is outside 0-30,000 ft MSL")]
    InvalidGroupAltitude { group_id: String, altitude_ft: f64 },
    #[error("radio group {group_id:?} has invalid antenna mask {mask}")]
    InvalidAntennaMask { group_id: String, mask: u8 },
    #[error("radio group {group_id:?} has invalid target power {dbm} dBm")]
    InvalidTransmitPower { group_id: String, dbm: u8 },
    #[error("radio group {0:?} field-calibrated capacity must be positive")]
    InvalidCalibratedCapacity(String),
    #[error("radio network requires at least one control-station or gateway node")]
    NoGateway,
    #[error("network ID must contain 1-32 alphanumeric, space, or hyphen characters")]
    InvalidNetworkId,
    #[error(
        "center frequency {0} MHz must be a 0.1-MHz value in the 300-6000 MHz planning envelope"
    )]
    InvalidCenterFrequency(f64),
    #[error("average and maximum node distances are invalid")]
    InvalidNodeDistances,
    #[error("routing beacon period {0} ms must be in 100-2000 ms")]
    InvalidRoutingBeaconPeriod(u32),
    #[error("recommended StreamCaster link distance exceeds 1,000,000 m")]
    LinkDistanceTooLarge,
    #[error("configured link distance {configured_m} m must be at least {minimum_m} m and no more than 1,000,000 m")]
    InvalidConfiguredLinkDistance { configured_m: u32, minimum_m: u32 },
    #[error(
        "group {group_id:?} model {model:?} does not support {bandwidth:?} in the planning profile"
    )]
    UnsupportedPlanningBandwidth {
        group_id: String,
        model: StreamCasterModel,
        bandwidth: ChannelBandwidthMhz,
    },
    #[error("traffic profile requires a positive packet size and finite positive packet rate")]
    InvalidTrafficProfile,
    #[error("traffic calculation overflowed")]
    TrafficOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(total_nodes: usize) -> ArcRadioConfiguration {
        ArcRadioConfiguration {
            schema_version: RADIO_CONFIG_SCHEMA_VERSION,
            authority: RadioConfigAuthority::Arc,
            generation: 1,
            network: StreamCasterNetworkSettings {
                network_id: "ARC-4000-Series".to_owned(),
                center_frequency_mhz: 2_450.0,
                bandwidth_mhz: ChannelBandwidthMhz::Mhz20,
                average_node_distance_m: 5_000.0,
                maximum_node_distance_m: 10_000.0,
                link_distance_m: None,
                routing_beacon_period_ms: 500,
                encryption_required: true,
            },
            fleet: RadioFleetDefinition {
                total_nodes,
                groups: vec![
                    RadioNodeGroup {
                        group_id: "airborne-5200".to_owned(),
                        node_id_prefix: "air".to_owned(),
                        percentage: 96.0,
                        model: StreamCasterModel::Sl5200LiteEstimated,
                        role: RadioNodeRole::Airborne,
                        altitude_msl_ft: 10_000.0,
                        transmit_power: TransmitPowerMode::MaxSupported,
                        antenna_mask: 3,
                        beamforming: true,
                        field_calibrated_udp_capacity_bps: None,
                    },
                    RadioNodeGroup {
                        group_id: "control-4400".to_owned(),
                        node_id_prefix: "gcs4400".to_owned(),
                        percentage: 2.0,
                        model: StreamCasterModel::Sc4400,
                        role: RadioNodeRole::ControlStation,
                        altitude_msl_ft: 0.0,
                        transmit_power: TransmitPowerMode::TargetDbm { dbm: 36 },
                        antenna_mask: 15,
                        beamforming: true,
                        field_calibrated_udp_capacity_bps: None,
                    },
                    RadioNodeGroup {
                        group_id: "control-4200".to_owned(),
                        node_id_prefix: "gcs4200".to_owned(),
                        percentage: 2.0,
                        model: StreamCasterModel::Sc4200,
                        role: RadioNodeRole::ControlStation,
                        altitude_msl_ft: 0.0,
                        transmit_power: TransmitPowerMode::MaxSupported,
                        antenna_mask: 3,
                        beamforming: true,
                        field_calibrated_udp_capacity_bps: None,
                    },
                ],
            },
            traffic: RadioTrafficProfile::default(),
            persist_after_apply: false,
        }
    }

    #[test]
    fn expands_requested_one_hundred_fifty_node_radio_mix() {
        let assessment = config(150).assess().unwrap();

        assert_eq!(assessment.node_count, 150);
        assert_eq!(assessment.gateway_count, 6);
        assert_eq!(assessment.resolved_link_distance_m, 11_500);
        assert_eq!(
            assessment
                .assignments
                .iter()
                .filter(|node| node.model == StreamCasterModel::Sl5200LiteEstimated)
                .count(),
            144
        );
    }

    #[test]
    fn routine_and_stress_traffic_use_three_kib_and_five_point_five_mbps() {
        let assessment = config(150).assess().unwrap();

        assert_eq!(assessment.routine_load.per_node_payload_bps, 24_576);
        assert_eq!(assessment.routine_load.aggregate_payload_bps, 3_686_400);
        assert_eq!(assessment.stress_load.per_node_payload_bps, 5_524_576);
        assert_eq!(assessment.stress_load.aggregate_payload_bps, 828_686_400);
        assert_eq!(
            assessment.stress_load.average_payload_bps_per_gateway,
            138_114_400
        );
    }

    #[test]
    fn accepts_thirty_thousand_feet_and_rejects_higher_groups() {
        let mut at_ceiling = config(150);
        at_ceiling.fleet.groups[0].altitude_msl_ft = 30_000.0;
        assert!(at_ceiling.assess().is_ok());

        at_ceiling.fleet.groups[0].altitude_msl_ft = 30_000.1;
        assert!(matches!(
            at_ceiling.assess(),
            Err(RadioConfigError::InvalidGroupAltitude { .. })
        ));
    }

    #[test]
    fn supports_two_hundred_nodes_but_not_more() {
        assert_eq!(config(200).assess().unwrap().node_count, 200);
        assert_eq!(
            config(201).assess().unwrap_err(),
            RadioConfigError::UnsupportedNodeCount(201)
        );
    }

    #[test]
    fn sl4200_rejects_twenty_mhz_while_other_models_plan_for_it() {
        let mut request = config(150);
        request.fleet.groups[0].model = StreamCasterModel::Sl4200;
        assert!(matches!(
            request.assess(),
            Err(RadioConfigError::UnsupportedPlanningBandwidth { .. })
        ));
    }

    #[test]
    fn apply_template_models_soft_boot_reconnect_and_capability_check() {
        let templates = config(150).silvus_apply_templates().unwrap();
        let airborne = templates
            .iter()
            .find(|template| template.group_id == "airborne-5200")
            .unwrap();

        assert_eq!(
            airborne.steps[0].method.as_deref(),
            Some("supported_frequency_profiles")
        );
        assert_eq!(airborne.steps[1].method.as_deref(), Some("freq_bw"));
        assert_eq!(airborne.steps[1].effect, SilvusStepEffect::SoftBoot);
        assert_eq!(airborne.steps[2].effect, SilvusStepEffect::WaitForReconnect);
        assert!(airborne
            .steps
            .iter()
            .all(|step| step.method.as_deref() != Some("enc_key")));
    }

    #[test]
    fn rejects_invalid_network_wide_settings() {
        let mut request = config(150);
        request.network.network_id = "invalid/network".to_owned();
        assert_eq!(
            request.assess().unwrap_err(),
            RadioConfigError::InvalidNetworkId
        );

        request = config(150);
        request.network.link_distance_m = Some(10_500);
        assert!(matches!(
            request.assess(),
            Err(RadioConfigError::InvalidConfiguredLinkDistance { .. })
        ));
    }
}

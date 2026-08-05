use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{NodeId, MAX_SUPPORTED_SWARM_SIZE, MIN_SUPPORTED_SWARM_SIZE, SYSTEM_MAX_MSL_FT};

pub const RADIO_CONFIG_SCHEMA_VERSION: u16 = 1;
pub const RADIO_VALIDATION_TARGET_NODES: usize = 150;
pub const DEFAULT_ROUTINE_PACKET_BYTES: u32 = 3 * 1024;
pub const DEFAULT_ROUTINE_PACKETS_PER_SECOND: f64 = 1.0;
pub const DEFAULT_PRIORITY_TRANSFER_BYTES: u64 = 5_500_000;
pub const DEFAULT_PRIORITY_SOURCE_NODES: usize = 1;
pub const DEFAULT_MAX_AIRTIME_RATIO: f64 = 0.80;
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
    Sl5205,
    Sl5210,
    Sl5220,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RadioRegulatoryProfile {
    /// Do not infer legal frequencies or conducted-power limits. A live
    /// capability response and operator-supplied regulatory authorization are
    /// required before hardware apply.
    #[default]
    LiveCapabilitiesRequired,
    /// FCC modular grant N2S-SL52-245-OEM, as documented by the SL5200/LC5200
    /// OEM Integration Manual v1.1. This profile covers the 2.4 GHz module
    /// variants listed by that grant; it is not a general SL5200 band profile.
    FccSl52_245Oem,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OemDimensionsMm {
    pub length: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StreamCasterOemIntegrationProfile {
    pub dimensions_mm: OemDimensionsMm,
    pub mass_g: f64,
    pub input_voltage_min_v: f64,
    pub input_voltage_max_v: f64,
    pub recommended_supply_fuse_a: f64,
    pub has_reverse_polarity_protection: bool,
    pub idle_power_w: f64,
    pub recommended_max_case_temperature_c: f64,
    pub transmit_backoff_temperature_c: f64,
    pub transmit_cutoff_temperature_c: f64,
    pub rf_port_count: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Sl5200PowerProfile {
    pub total_rf_power_w: f64,
    pub conducted_power_per_port_dbm: f64,
    pub l_band_max_input_power_w: f64,
    pub s_band_max_input_power_w: f64,
    pub l_band_peak_input_power_w: f64,
    pub s_band_peak_input_power_w: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamCasterRfBand {
    LBand,
    SBand,
}

impl Sl5200PowerProfile {
    /// Estimates average radio input power from the documented 4 W listening
    /// state and the selected band's 100%-airtime maximum. The integration
    /// guide's 1 W S-band/80% worked example contains an arithmetic error;
    /// this formula intentionally yields 8.0 W rather than 8.5 W.
    pub fn estimated_average_input_power_w(
        self,
        band: StreamCasterRfBand,
        airtime_ratio: f64,
    ) -> Option<f64> {
        if !airtime_ratio.is_finite() || !(0.0..=1.0).contains(&airtime_ratio) {
            return None;
        }
        let maximum = match band {
            StreamCasterRfBand::LBand => self.l_band_max_input_power_w,
            StreamCasterRfBand::SBand => self.s_band_max_input_power_w,
        };
        Some(
            SL5200_OEM_INTEGRATION_PROFILE.idle_power_w
                + airtime_ratio * (maximum - SL5200_OEM_INTEGRATION_PROFILE.idle_power_w),
        )
    }
}

pub const SL5200_OEM_INTEGRATION_PROFILE: StreamCasterOemIntegrationProfile =
    StreamCasterOemIntegrationProfile {
        dimensions_mm: OemDimensionsMm {
            length: 63.5,
            width: 44.5,
            height: 10.4,
        },
        mass_g: 52.9,
        input_voltage_min_v: 9.0,
        input_voltage_max_v: 32.0,
        recommended_supply_fuse_a: 5.0,
        has_reverse_polarity_protection: false,
        idle_power_w: 4.0,
        recommended_max_case_temperature_c: 70.0,
        transmit_backoff_temperature_c: 75.0,
        transmit_cutoff_temperature_c: 85.0,
        rf_port_count: 2,
    };

pub const FCC_SL52_245_20_MHZ_CENTER_FREQUENCY_MHZ: f64 = 2_440.0;
pub const FCC_SL52_245_20_MHZ_MAX_CONDUCTED_POWER_PER_PORT_DBM: u8 = 27;
pub const FCC_SL52_245_10_MHZ_MIN_CENTER_FREQUENCY_MHZ: f64 = 2_416.0;
pub const FCC_SL52_245_10_MHZ_MAX_CENTER_FREQUENCY_MHZ: f64 = 2_457.0;
pub const FCC_SL52_245_10_MHZ_MAX_CONDUCTED_POWER_PER_PORT_DBM: u8 = 24;

impl RadioRegulatoryProfile {
    fn max_conducted_power_per_port_dbm(self, bandwidth: ChannelBandwidthMhz) -> Option<u8> {
        match (self, bandwidth) {
            (Self::FccSl52_245Oem, ChannelBandwidthMhz::Mhz10) => {
                Some(FCC_SL52_245_10_MHZ_MAX_CONDUCTED_POWER_PER_PORT_DBM)
            }
            (Self::FccSl52_245Oem, ChannelBandwidthMhz::Mhz20) => {
                Some(FCC_SL52_245_20_MHZ_MAX_CONDUCTED_POWER_PER_PORT_DBM)
            }
            _ => None,
        }
    }
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
    #[serde(default)]
    pub regulatory_profile: RadioRegulatoryProfile,
    /// Conducted transmit-power intent. `TargetDbm` is interpreted per active
    /// RF port; aggregate MIMO power must never be used as a single-path link
    /// budget input.
    pub transmit_power: TransmitPowerMode,
    pub antenna_mask: u8,
    pub beamforming: bool,
    /// Approximate installed-system EIRP supplied by the operator for planning.
    /// This does not replace the conducted-power, antenna-gain, installation-
    /// loss, or regulatory evidence required for hardware activation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_installed_eirp_dbm: Option<f64>,
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
    #[serde(default = "default_priority_transfer_bytes")]
    pub priority_transfer_bytes: u64,
    #[serde(default = "default_priority_source_nodes")]
    pub priority_source_nodes: usize,
    #[serde(default = "default_priority_source_role")]
    pub priority_source_role: RadioNodeRole,
    #[serde(default = "default_priority_destination_role")]
    pub priority_destination_role: RadioNodeRole,
    #[serde(default = "default_max_airtime_ratio")]
    pub max_airtime_ratio: f64,
    /// Measured end-to-end UDP goodput baseline at full offered channel load,
    /// from the priority source through the installed antennas and operational
    /// route to the control station. The airtime ceiling is applied below.
    pub calibrated_end_to_end_goodput_bps: Option<u64>,
}

impl Default for RadioTrafficProfile {
    fn default() -> Self {
        Self {
            routine_packet_bytes: DEFAULT_ROUTINE_PACKET_BYTES,
            routine_packets_per_second: DEFAULT_ROUTINE_PACKETS_PER_SECOND,
            priority_transfer_bytes: DEFAULT_PRIORITY_TRANSFER_BYTES,
            priority_source_nodes: DEFAULT_PRIORITY_SOURCE_NODES,
            priority_source_role: RadioNodeRole::Airborne,
            priority_destination_role: RadioNodeRole::ControlStation,
            max_airtime_ratio: DEFAULT_MAX_AIRTIME_RATIO,
            calibrated_end_to_end_goodput_bps: None,
        }
    }
}

const fn default_routine_packet_bytes() -> u32 {
    DEFAULT_ROUTINE_PACKET_BYTES
}

const fn default_routine_packets_per_second() -> f64 {
    DEFAULT_ROUTINE_PACKETS_PER_SECOND
}

const fn default_priority_transfer_bytes() -> u64 {
    DEFAULT_PRIORITY_TRANSFER_BYTES
}

const fn default_priority_source_nodes() -> usize {
    DEFAULT_PRIORITY_SOURCE_NODES
}

const fn default_priority_source_role() -> RadioNodeRole {
    RadioNodeRole::Airborne
}

const fn default_priority_destination_role() -> RadioNodeRole {
    RadioNodeRole::ControlStation
}

const fn default_max_airtime_ratio() -> f64 {
    DEFAULT_MAX_AIRTIME_RATIO
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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PriorityTransferAssessment {
    pub payload_bytes_per_source: u64,
    pub concurrent_source_count: usize,
    pub total_payload_bits: u64,
    pub source_role: RadioNodeRole,
    pub destination_role: RadioNodeRole,
    pub max_airtime_ratio: f64,
    pub reserved_airtime_ratio: f64,
    /// Calibrated end-to-end goodput after applying the airtime ceiling.
    pub airtime_limited_goodput_bps: Option<u64>,
    /// Airtime-limited goodput remaining after routine offered load.
    pub residual_priority_goodput_bps: Option<u64>,
    /// Planning estimate only; route hops, retries, queues, and changing RF
    /// conditions must still be measured.
    pub estimated_transfer_time_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RadioPlanAssessment {
    pub node_count: usize,
    pub gateway_count: usize,
    pub resolved_link_distance_m: u32,
    pub assignments: Vec<RadioNodeAssignment>,
    pub routine_load: RadioTrafficLoad,
    pub priority_transfer: PriorityTransferAssessment,
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
    pub regulatory_profile: RadioRegulatoryProfile,
    pub maximum_conducted_power_per_port_dbm: Option<u8>,
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
            Self::Sl5205 | Self::Sl5210 | Self::Sl5220 | Self::Sl5200 => StreamCasterModelProfile {
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

    pub fn oem_integration_profile(self) -> Option<StreamCasterOemIntegrationProfile> {
        self.is_sl5200_family()
            .then_some(SL5200_OEM_INTEGRATION_PROFILE)
    }

    pub fn sl5200_power_profile(self) -> Option<Sl5200PowerProfile> {
        let profile = match self {
            Self::Sl5205 => Sl5200PowerProfile {
                total_rf_power_w: 0.5,
                conducted_power_per_port_dbm: 24.0,
                l_band_max_input_power_w: 9.0,
                s_band_max_input_power_w: 8.0,
                l_band_peak_input_power_w: 10.0,
                s_band_peak_input_power_w: 9.0,
            },
            Self::Sl5210 => Sl5200PowerProfile {
                total_rf_power_w: 1.0,
                conducted_power_per_port_dbm: 27.0,
                l_band_max_input_power_w: 11.0,
                s_band_max_input_power_w: 9.0,
                l_band_peak_input_power_w: 12.0,
                s_band_peak_input_power_w: 10.0,
            },
            Self::Sl5220 => Sl5200PowerProfile {
                total_rf_power_w: 2.0,
                conducted_power_per_port_dbm: 30.0,
                l_band_max_input_power_w: 13.0,
                s_band_max_input_power_w: 11.0,
                l_band_peak_input_power_w: 14.0,
                s_band_peak_input_power_w: 12.0,
            },
            _ => return None,
        };
        Some(profile)
    }

    fn is_sl5200_family(self) -> bool {
        matches!(
            self,
            Self::Sl5205 | Self::Sl5210 | Self::Sl5220 | Self::Sl5200 | Self::Sl5200LiteEstimated
        )
    }

    fn is_listed_by_fcc_sl52_245_oem(self) -> bool {
        matches!(self, Self::Sl5210 | Self::Sl5220)
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
        let routine_load = traffic_load(routine_per_node, assignments.len(), gateway_count)?;

        let mut warnings = Vec::new();
        let priority_transfer = self.traffic.assess_priority_transfer(
            &assignments,
            routine_load.aggregate_payload_bps,
            &mut warnings,
        )?;
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
        let mut all_eirp_estimated = true;
        for group in &self.fleet.groups {
            if !group.model.planning_supports(self.network.bandwidth_mhz) {
                return Err(RadioConfigError::UnsupportedPlanningBandwidth {
                    group_id: group.group_id.clone(),
                    model: group.model,
                    bandwidth: self.network.bandwidth_mhz,
                });
            }
            group.validate()?;
            group.validate_for_network(&self.network)?;
            if group.regulatory_profile == RadioRegulatoryProfile::LiveCapabilitiesRequired {
                warnings.push(format!(
                    "group {:?} has no pinned regulatory profile; live radio capabilities and operator authorization are required before apply",
                    group.group_id
                ));
            }
            if group.regulatory_profile == RadioRegulatoryProfile::FccSl52_245Oem
                && group.transmit_power == TransmitPowerMode::MaxSupported
            {
                warnings.push(format!(
                    "group {:?} requests max-supported power under the FCC SL52 profile; hardware apply must resolve that intent to the bandwidth-specific per-port cap",
                    group.group_id
                ));
            }
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
            if let Some(eirp_dbm) = group.estimated_installed_eirp_dbm {
                warnings.push(format!(
                    "group {:?} uses approximately {eirp_dbm:.2} dBm installed EIRP for planning; underlying conducted power, antenna gain, installation loss, array method, and calibration remain activation gates",
                    group.group_id
                ));
            } else {
                all_eirp_estimated = false;
            }
            if matches!(group.transmit_power, TransmitPowerMode::TargetDbm { dbm } if dbm < 10) {
                warnings.push(format!(
                    "group {:?} targets less than 10 dBm; the API manual says actual output accuracy is specified only from 10-39 dBm",
                    group.group_id
                ));
            }
        }
        if !all_eirp_estimated {
            warnings.push(
                "one or more groups lack even an estimated installed EIRP; link-budget planning remains incomplete"
                    .to_owned(),
            );
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
            priority_transfer,
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
                    regulatory_profile: group.regulatory_profile,
                    maximum_conducted_power_per_port_dbm: group
                        .regulatory_profile
                        .max_conducted_power_per_port_dbm(self.network.bandwidth_mhz),
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
        if self
            .estimated_installed_eirp_dbm
            .is_some_and(|eirp| !eirp.is_finite() || !(-20.0..=80.0).contains(&eirp))
        {
            return Err(RadioConfigError::InvalidEstimatedEirp {
                group_id: self.group_id.clone(),
                eirp_dbm: self.estimated_installed_eirp_dbm.unwrap_or_default(),
            });
        }
        Ok(())
    }

    fn validate_for_network(
        &self,
        network: &StreamCasterNetworkSettings,
    ) -> Result<(), RadioConfigError> {
        if self.regulatory_profile != RadioRegulatoryProfile::FccSl52_245Oem {
            return Ok(());
        }
        if !self.model.is_listed_by_fcc_sl52_245_oem() {
            return Err(RadioConfigError::InvalidRegulatoryProfileForModel {
                group_id: self.group_id.clone(),
                profile: self.regulatory_profile,
                model: self.model,
            });
        }

        let frequency_allowed = match network.bandwidth_mhz {
            ChannelBandwidthMhz::Mhz20 => {
                (network.center_frequency_mhz - FCC_SL52_245_20_MHZ_CENTER_FREQUENCY_MHZ).abs()
                    <= 0.05
            }
            ChannelBandwidthMhz::Mhz10 => (FCC_SL52_245_10_MHZ_MIN_CENTER_FREQUENCY_MHZ
                ..=FCC_SL52_245_10_MHZ_MAX_CENTER_FREQUENCY_MHZ)
                .contains(&network.center_frequency_mhz),
            _ => {
                return Err(RadioConfigError::UnsupportedRegulatoryBandwidth {
                    group_id: self.group_id.clone(),
                    profile: self.regulatory_profile,
                    bandwidth: network.bandwidth_mhz,
                });
            }
        };
        if !frequency_allowed {
            return Err(RadioConfigError::UnsupportedRegulatoryFrequency {
                group_id: self.group_id.clone(),
                profile: self.regulatory_profile,
                bandwidth: network.bandwidth_mhz,
                center_frequency_mhz: network.center_frequency_mhz,
            });
        }

        if let (Some(limit_dbm), TransmitPowerMode::TargetDbm { dbm }) = (
            self.regulatory_profile
                .max_conducted_power_per_port_dbm(network.bandwidth_mhz),
            self.transmit_power,
        ) {
            if dbm > limit_dbm {
                return Err(RadioConfigError::RegulatoryPowerExceeded {
                    group_id: self.group_id.clone(),
                    requested_dbm: dbm,
                    maximum_dbm: limit_dbm,
                });
            }
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
        if self.priority_transfer_bytes == 0 || self.priority_source_nodes == 0 {
            return Err(RadioConfigError::InvalidPriorityTransfer);
        }
        if !self.max_airtime_ratio.is_finite()
            || self.max_airtime_ratio <= 0.0
            || self.max_airtime_ratio > DEFAULT_MAX_AIRTIME_RATIO
        {
            return Err(RadioConfigError::InvalidMaxAirtimeRatio(
                self.max_airtime_ratio,
            ));
        }
        if self.calibrated_end_to_end_goodput_bps == Some(0) {
            return Err(RadioConfigError::InvalidCalibratedGoodput);
        }
        Ok(())
    }

    fn assess_priority_transfer(
        &self,
        assignments: &[RadioNodeAssignment],
        routine_aggregate_bps: u64,
        warnings: &mut Vec<String>,
    ) -> Result<PriorityTransferAssessment, RadioConfigError> {
        if self.priority_source_nodes > assignments.len() {
            return Err(RadioConfigError::PrioritySourceCountExceedsFleet {
                sources: self.priority_source_nodes,
                nodes: assignments.len(),
            });
        }
        let available_sources = assignments
            .iter()
            .filter(|node| node.role == self.priority_source_role)
            .count();
        if self.priority_source_nodes > available_sources {
            return Err(RadioConfigError::InsufficientPrioritySources {
                role: self.priority_source_role,
                requested: self.priority_source_nodes,
                available: available_sources,
            });
        }
        if !assignments
            .iter()
            .any(|node| node.role == self.priority_destination_role)
        {
            return Err(RadioConfigError::MissingPriorityDestination(
                self.priority_destination_role,
            ));
        }

        let total_payload_bits = self
            .priority_transfer_bytes
            .checked_mul(8)
            .and_then(|bits| bits.checked_mul(self.priority_source_nodes as u64))
            .ok_or(RadioConfigError::TrafficOverflow)?;
        let (
            airtime_limited_goodput_bps,
            residual_priority_goodput_bps,
            estimated_transfer_time_ms,
        ) = if let Some(calibrated_goodput_bps) = self.calibrated_end_to_end_goodput_bps {
            let airtime_limited =
                (calibrated_goodput_bps as f64 * self.max_airtime_ratio).floor() as u64;
            let residual = airtime_limited.saturating_sub(routine_aggregate_bps);
            if residual == 0 {
                warnings.push(
                        "the routine offered load consumes the calibrated goodput available under the airtime ceiling; priority transfer duration cannot be estimated"
                            .to_owned(),
                    );
                (Some(airtime_limited), Some(0), None)
            } else {
                let milliseconds = total_payload_bits
                    .checked_mul(1_000)
                    .ok_or(RadioConfigError::TrafficOverflow)?
                    .div_ceil(residual);
                (Some(airtime_limited), Some(residual), Some(milliseconds))
            }
        } else {
            warnings.push(
                    "priority transfer duration is unknown until end-to-end goodput is measured through the installed antennas, airframes, route, and environment"
                        .to_owned(),
                );
            (None, None, None)
        };

        Ok(PriorityTransferAssessment {
            payload_bytes_per_source: self.priority_transfer_bytes,
            concurrent_source_count: self.priority_source_nodes,
            total_payload_bits,
            source_role: self.priority_source_role,
            destination_role: self.priority_destination_role,
            max_airtime_ratio: self.max_airtime_ratio,
            reserved_airtime_ratio: ((1.0 - self.max_airtime_ratio) * 100.0).round() / 100.0,
            airtime_limited_goodput_bps,
            residual_priority_goodput_bps,
            estimated_transfer_time_ms,
        })
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
    #[error(
        "radio group {group_id:?} cannot use regulatory profile {profile:?} with model {model:?}"
    )]
    InvalidRegulatoryProfileForModel {
        group_id: String,
        profile: RadioRegulatoryProfile,
        model: StreamCasterModel,
    },
    #[error("radio group {group_id:?} regulatory profile {profile:?} does not authorize bandwidth {bandwidth:?}")]
    UnsupportedRegulatoryBandwidth {
        group_id: String,
        profile: RadioRegulatoryProfile,
        bandwidth: ChannelBandwidthMhz,
    },
    #[error("radio group {group_id:?} regulatory profile {profile:?} does not authorize {center_frequency_mhz} MHz at {bandwidth:?}")]
    UnsupportedRegulatoryFrequency {
        group_id: String,
        profile: RadioRegulatoryProfile,
        bandwidth: ChannelBandwidthMhz,
        center_frequency_mhz: f64,
    },
    #[error("radio group {group_id:?} requests {requested_dbm} dBm per port, above the regulatory maximum of {maximum_dbm} dBm")]
    RegulatoryPowerExceeded {
        group_id: String,
        requested_dbm: u8,
        maximum_dbm: u8,
    },
    #[error("radio group {0:?} field-calibrated capacity must be positive")]
    InvalidCalibratedCapacity(String),
    #[error("radio group {group_id:?} has invalid estimated installed EIRP {eirp_dbm} dBm")]
    InvalidEstimatedEirp { group_id: String, eirp_dbm: f64 },
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
    #[error("priority transfer requires a positive payload size and source count")]
    InvalidPriorityTransfer,
    #[error("priority transfer source count {sources} exceeds fleet size {nodes}")]
    PrioritySourceCountExceedsFleet { sources: usize, nodes: usize },
    #[error(
        "priority transfer requests {requested} {role:?} sources, but only {available} are assigned"
    )]
    InsufficientPrioritySources {
        role: RadioNodeRole,
        requested: usize,
        available: usize,
    },
    #[error("radio fleet has no node with priority destination role {0:?}")]
    MissingPriorityDestination(RadioNodeRole),
    #[error("maximum airtime ratio {0} must be positive and no greater than 0.8")]
    InvalidMaxAirtimeRatio(f64),
    #[error("calibrated end-to-end goodput must be positive")]
    InvalidCalibratedGoodput,
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
                center_frequency_mhz: 2_440.0,
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
                        regulatory_profile: RadioRegulatoryProfile::LiveCapabilitiesRequired,
                        transmit_power: TransmitPowerMode::MaxSupported,
                        antenna_mask: 3,
                        beamforming: true,
                        estimated_installed_eirp_dbm: Some(34.44),
                        field_calibrated_udp_capacity_bps: None,
                    },
                    RadioNodeGroup {
                        group_id: "control-4400".to_owned(),
                        node_id_prefix: "gcs4400".to_owned(),
                        percentage: 2.0,
                        model: StreamCasterModel::Sc4400,
                        role: RadioNodeRole::ControlStation,
                        altitude_msl_ft: 0.0,
                        regulatory_profile: RadioRegulatoryProfile::LiveCapabilitiesRequired,
                        transmit_power: TransmitPowerMode::TargetDbm { dbm: 36 },
                        antenna_mask: 15,
                        beamforming: true,
                        estimated_installed_eirp_dbm: Some(33.0),
                        field_calibrated_udp_capacity_bps: None,
                    },
                    RadioNodeGroup {
                        group_id: "control-4200".to_owned(),
                        node_id_prefix: "gcs4200".to_owned(),
                        percentage: 2.0,
                        model: StreamCasterModel::Sc4200,
                        role: RadioNodeRole::ControlStation,
                        altitude_msl_ft: 0.0,
                        regulatory_profile: RadioRegulatoryProfile::LiveCapabilitiesRequired,
                        transmit_power: TransmitPowerMode::MaxSupported,
                        antenna_mask: 3,
                        beamforming: true,
                        estimated_installed_eirp_dbm: Some(33.0),
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
    fn estimated_eirp_is_retained_but_reported_as_planning_only() {
        let assessment = config(150).assess().unwrap();
        assert!(assessment.warnings.iter().any(|warning| {
            warning.contains("34.44 dBm installed EIRP") && warning.contains("activation gates")
        }));

        let mut invalid = config(150);
        invalid.fleet.groups[0].estimated_installed_eirp_dbm = Some(100.0);
        assert!(matches!(
            invalid.assess(),
            Err(RadioConfigError::InvalidEstimatedEirp { .. })
        ));
    }

    #[test]
    fn deterministic_radio_plan_assessment_covers_all_validation_scales() {
        for total_nodes in [5, 25, 100, 150, 200] {
            let mut request = config(total_nodes);
            if total_nodes < 50 {
                request.fleet.groups[0].percentage = 60.0;
                request.fleet.groups[1].percentage = 20.0;
                request.fleet.groups[2].percentage = 20.0;
            }
            let assessment = request.assess().unwrap();
            assert_eq!(assessment.node_count, total_nodes);
            assert_eq!(assessment.assignments.len(), total_nodes);
            assert!(assessment
                .assignments
                .iter()
                .any(|node| node.role == RadioNodeRole::Airborne));
            assert!(assessment
                .assignments
                .iter()
                .any(|node| node.role == RadioNodeRole::ControlStation));
        }
    }

    #[test]
    fn routine_and_priority_transfer_use_three_kib_and_five_point_five_mb() {
        let assessment = config(150).assess().unwrap();

        assert_eq!(assessment.routine_load.per_node_payload_bps, 24_576);
        assert_eq!(assessment.routine_load.aggregate_payload_bps, 3_686_400);
        assert_eq!(
            assessment.priority_transfer.payload_bytes_per_source,
            5_500_000
        );
        assert_eq!(assessment.priority_transfer.concurrent_source_count, 1);
        assert_eq!(assessment.priority_transfer.total_payload_bits, 44_000_000);
        assert_eq!(
            assessment.priority_transfer.source_role,
            RadioNodeRole::Airborne
        );
        assert_eq!(assessment.priority_transfer.max_airtime_ratio, 0.8);
        assert!((assessment.priority_transfer.reserved_airtime_ratio - 0.2).abs() < f64::EPSILON);
        assert_eq!(
            assessment.priority_transfer.estimated_transfer_time_ms,
            None
        );
    }

    #[test]
    fn calibrated_goodput_applies_airtime_ceiling_and_routine_load() {
        let mut request = config(150);
        request.traffic.calibrated_end_to_end_goodput_bps = Some(94_000_000);

        let transfer = request.assess().unwrap().priority_transfer;

        assert_eq!(transfer.airtime_limited_goodput_bps, Some(75_200_000));
        assert_eq!(transfer.residual_priority_goodput_bps, Some(71_513_600));
        assert_eq!(transfer.estimated_transfer_time_ms, Some(616));
    }

    #[test]
    fn rejects_more_than_eighty_percent_airtime() {
        let mut request = config(150);
        request.traffic.max_airtime_ratio = 0.81;

        assert!(matches!(
            request.assess(),
            Err(RadioConfigError::InvalidMaxAirtimeRatio(value)) if value == 0.81
        ));
    }

    #[test]
    fn requires_configured_source_and_destination_roles() {
        let mut no_airborne_source = config(150);
        no_airborne_source.fleet.groups[0].role = RadioNodeRole::Relay;
        assert!(matches!(
            no_airborne_source.assess(),
            Err(RadioConfigError::InsufficientPrioritySources {
                role: RadioNodeRole::Airborne,
                requested: 1,
                available: 0
            })
        ));

        let mut no_control_destination = config(150);
        no_control_destination.traffic.priority_destination_role = RadioNodeRole::Gateway;
        assert_eq!(
            no_control_destination.assess().unwrap_err(),
            RadioConfigError::MissingPriorityDestination(RadioNodeRole::Gateway)
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
    fn fcc_sl52_profile_requires_2440_mhz_for_twenty_mhz() {
        let mut request = config(150);
        request.fleet.groups[0].model = StreamCasterModel::Sl5220;
        request.fleet.groups[0].regulatory_profile = RadioRegulatoryProfile::FccSl52_245Oem;
        request.network.center_frequency_mhz = 2_450.0;

        assert!(matches!(
            request.assess(),
            Err(RadioConfigError::UnsupportedRegulatoryFrequency {
                bandwidth: ChannelBandwidthMhz::Mhz20,
                center_frequency_mhz: 2_450.0,
                ..
            })
        ));
    }

    #[test]
    fn fcc_sl52_profile_rejects_power_above_per_port_limit() {
        let mut request = config(150);
        request.fleet.groups[0].model = StreamCasterModel::Sl5220;
        request.fleet.groups[0].regulatory_profile = RadioRegulatoryProfile::FccSl52_245Oem;
        request.fleet.groups[0].transmit_power = TransmitPowerMode::TargetDbm { dbm: 28 };

        assert!(matches!(
            request.assess(),
            Err(RadioConfigError::RegulatoryPowerExceeded {
                requested_dbm: 28,
                maximum_dbm: 27,
                ..
            })
        ));
    }

    #[test]
    fn fcc_sl52_profile_rejects_undocumented_bandwidths() {
        let mut request = config(150);
        request.fleet.groups[0].model = StreamCasterModel::Sl5220;
        request.fleet.groups[0].regulatory_profile = RadioRegulatoryProfile::FccSl52_245Oem;
        request.network.bandwidth_mhz = ChannelBandwidthMhz::Mhz5;

        assert!(matches!(
            request.assess(),
            Err(RadioConfigError::UnsupportedRegulatoryBandwidth {
                bandwidth: ChannelBandwidthMhz::Mhz5,
                ..
            })
        ));
    }

    #[test]
    fn fcc_sl52_profile_requires_an_exact_listed_model() {
        let mut request = config(150);
        request.fleet.groups[0].regulatory_profile = RadioRegulatoryProfile::FccSl52_245Oem;

        assert!(matches!(
            request.assess(),
            Err(RadioConfigError::InvalidRegulatoryProfileForModel {
                model: StreamCasterModel::Sl5200LiteEstimated,
                ..
            })
        ));
    }

    #[test]
    fn sl5200_oem_profile_records_confirmed_power_and_thermal_limits() {
        let integration = StreamCasterModel::Sl5220.oem_integration_profile().unwrap();
        assert_eq!(integration.dimensions_mm.length, 63.5);
        assert_eq!(integration.mass_g, 52.9);
        assert_eq!(integration.input_voltage_min_v, 9.0);
        assert!(!integration.has_reverse_polarity_protection);
        assert_eq!(integration.transmit_cutoff_temperature_c, 85.0);

        let power = StreamCasterModel::Sl5210.sl5200_power_profile().unwrap();
        assert_eq!(power.conducted_power_per_port_dbm, 27.0);
        assert_eq!(
            power.estimated_average_input_power_w(StreamCasterRfBand::SBand, 0.8),
            Some(8.0)
        );
    }

    #[test]
    fn apply_template_models_soft_boot_reconnect_and_capability_check() {
        let mut request = config(150);
        request.fleet.groups[0].model = StreamCasterModel::Sl5220;
        request.fleet.groups[0].regulatory_profile = RadioRegulatoryProfile::FccSl52_245Oem;
        let templates = request.silvus_apply_templates().unwrap();
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
        assert_eq!(
            airborne.maximum_conducted_power_per_port_dbm,
            Some(FCC_SL52_245_20_MHZ_MAX_CONDUCTED_POWER_PER_PORT_DBM)
        );
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

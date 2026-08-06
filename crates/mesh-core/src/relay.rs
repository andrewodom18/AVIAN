use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{NodeId, MAX_SUPPORTED_SWARM_SIZE, MIN_SUPPORTED_SWARM_SIZE, SYSTEM_MAX_MSL_M};

const EARTH_RADIUS_M: f64 = 6_371_000.0;
const FREE_SPACE_PATH_LOSS_CONSTANT_DB: f64 = 32.44;

/// Total RF power class published for the 2 W SL5220. This aggregate value is
/// metadata only and must not be used as a single-path link-budget input.
pub const SILVUS_SL5220_TOTAL_RF_POWER_DBM: f64 = 33.0;
/// Conducted power for one of the two SL5220 RF ports in the documented 2 W
/// configuration. AVIAN does not add beamforming credit here; installed-array
/// gain belongs in measured antenna and airframe evidence.
pub const SILVUS_SL5220_PER_PORT_TX_POWER_DBM: f64 = 30.0;
/// Published SL5200 receive sensitivity at 5 MHz channel bandwidth.
pub const SILVUS_SL5200_5_MHZ_SENSITIVITY_DBM: f64 = -101.0;
/// Published SL5200 receive sensitivity at optional 1.25 MHz bandwidth.
pub const SILVUS_SL5200_1_25_MHZ_SENSITIVITY_DBM: f64 = -107.0;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RadioLinkBudget {
    pub frequency_mhz: f64,
    pub transmitter_power_dbm: f64,
    pub transmitter_antenna_gain_dbi: f64,
    pub receiver_antenna_gain_dbi: f64,
    pub receiver_sensitivity_dbm: f64,
    /// Margin reserved for fading, maneuvering, and unmodeled variation.
    pub fade_margin_db: f64,
    /// Known feeder, installation, terrain, and obstruction losses.
    pub additional_path_loss_db: f64,
}

impl RadioLinkBudget {
    /// A starting profile from published SL5200 5 MHz RF parameters. The
    /// caller must supply the configured frequency and measured losses.
    pub fn silvus_sl5200_5_mhz(frequency_mhz: f64, fade_margin_db: f64) -> Self {
        Self {
            frequency_mhz,
            transmitter_power_dbm: SILVUS_SL5220_PER_PORT_TX_POWER_DBM,
            transmitter_antenna_gain_dbi: 0.0,
            receiver_antenna_gain_dbi: 0.0,
            receiver_sensitivity_dbm: SILVUS_SL5200_5_MHZ_SENSITIVITY_DBM,
            fade_margin_db,
            additional_path_loss_db: 0.0,
        }
    }

    pub fn max_free_space_range_m(self) -> Result<f64, RelayPlanError> {
        let values = [
            self.frequency_mhz,
            self.transmitter_power_dbm,
            self.transmitter_antenna_gain_dbi,
            self.receiver_antenna_gain_dbi,
            self.receiver_sensitivity_dbm,
            self.fade_margin_db,
            self.additional_path_loss_db,
        ];
        if values.iter().any(|value| !value.is_finite())
            || self.frequency_mhz <= 0.0
            || self.fade_margin_db < 0.0
            || self.additional_path_loss_db < 0.0
        {
            return Err(RelayPlanError::InvalidLinkBudget);
        }
        let maximum_path_loss_db = self.transmitter_power_dbm
            + self.transmitter_antenna_gain_dbi
            + self.receiver_antenna_gain_dbi
            - self.receiver_sensitivity_dbm
            - self.fade_margin_db
            - self.additional_path_loss_db;
        let range_km = 10_f64.powf(
            (maximum_path_loss_db
                - FREE_SPACE_PATH_LOSS_CONSTANT_DB
                - 20.0 * self.frequency_mhz.log10())
                / 20.0,
        );
        let range_m = range_km * 1_000.0;
        if !range_m.is_finite() || range_m <= 0.0 {
            return Err(RelayPlanError::InvalidLinkBudget);
        }
        Ok(range_m)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RangeEvidence {
    FieldCalibrated,
    FreeSpaceModel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum RelayRangeModel {
    /// Mission-safe range established from current radio/airframe testing for
    /// the intended bandwidth, altitude, and terrain class.
    FieldCalibrated { usable_segment_m: f64 },
    /// A first-pass path-loss calculation. It must be calibrated against live
    /// measurements and terrain before a mission is activated.
    FreeSpaceLinkBudget { budget: RadioLinkBudget },
}

impl RelayRangeModel {
    fn usable_segment_m(&self) -> Result<f64, RelayPlanError> {
        let value = match self {
            Self::FieldCalibrated { usable_segment_m } => *usable_segment_m,
            Self::FreeSpaceLinkBudget { budget } => budget.max_free_space_range_m()?,
        };
        if !value.is_finite() || value <= 0.0 {
            return Err(RelayPlanError::InvalidUsableRange);
        }
        Ok(value)
    }

    fn evidence(&self) -> RangeEvidence {
        match self {
            Self::FieldCalibrated { .. } => RangeEvidence::FieldCalibrated,
            Self::FreeSpaceLinkBudget { .. } => RangeEvidence::FreeSpaceModel,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GeoPoint {
    pub latitude_deg: f64,
    pub longitude_deg: f64,
    pub msl_m: f64,
}

impl GeoPoint {
    pub fn validate(self) -> Result<Self, RelayPlanError> {
        if !self.latitude_deg.is_finite()
            || !self.longitude_deg.is_finite()
            || !self.msl_m.is_finite()
        {
            return Err(RelayPlanError::InvalidCoordinate);
        }
        if !(-90.0..=90.0).contains(&self.latitude_deg)
            || !(-180.0..=180.0).contains(&self.longitude_deg)
        {
            return Err(RelayPlanError::InvalidCoordinate);
        }
        if self.msl_m > SYSTEM_MAX_MSL_M {
            return Err(RelayPlanError::AboveSystemCeiling(self.msl_m));
        }
        Ok(self)
    }

    pub fn distance_to(self, other: Self) -> f64 {
        let latitude_1 = self.latitude_deg.to_radians();
        let latitude_2 = other.latitude_deg.to_radians();
        let latitude_delta = (other.latitude_deg - self.latitude_deg).to_radians();
        let longitude_delta =
            shortest_longitude_delta(self.longitude_deg, other.longitude_deg).to_radians();
        let haversine = (latitude_delta / 2.0).sin().powi(2)
            + latitude_1.cos() * latitude_2.cos() * (longitude_delta / 2.0).sin().powi(2);
        let haversine = haversine.clamp(0.0, 1.0);
        let surface_distance =
            2.0 * EARTH_RADIUS_M * haversine.sqrt().atan2((1.0 - haversine).sqrt());
        surface_distance.hypot(other.msl_m - self.msl_m)
    }

    fn interpolate(self, other: Self, fraction: f64) -> Self {
        let longitude_delta = shortest_longitude_delta(self.longitude_deg, other.longitude_deg);
        let longitude_deg =
            (self.longitude_deg + longitude_delta * fraction + 180.0).rem_euclid(360.0) - 180.0;
        Self {
            latitude_deg: self.latitude_deg + (other.latitude_deg - self.latitude_deg) * fraction,
            longitude_deg,
            msl_m: self.msl_m + (other.msl_m - self.msl_m) * fraction,
        }
    }
}

fn shortest_longitude_delta(from: f64, to: f64) -> f64 {
    (to - from + 180.0).rem_euclid(360.0) - 180.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayCandidate {
    pub node_id: NodeId,
    pub available: bool,
    pub relay_eligible: bool,
    pub mission_eligible: bool,
    /// Normalized 0.0-1.0 score incorporating radio, endurance, and platform fit.
    pub relay_suitability: f32,
    /// Normalized 0.0-1.0 value of keeping this aircraft on the payload mission.
    pub mission_utility: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayPolicy {
    pub range: RelayRangeModel,
    /// Automatic relay-reservation strategy selected for this mission.
    pub coverage: RelayCoverage,
    /// Required for maximum coverage. This is explicit because a Bluetooth
    /// heartbeat deadline must fit the actual companion, radio, and mission
    /// latency target; AVIAN does not invent a universal failover timer.
    pub paired_handover: Option<RelayPairHandoverPolicy>,
}

impl RelayPolicy {
    pub fn usable_segment_m(&self) -> Result<f64, RelayPlanError> {
        self.range.usable_segment_m()
    }

    fn relays_per_station(&self) -> usize {
        self.coverage.relays_per_station()
    }

    fn validate_handover(&self) -> Result<(), RelayPlanError> {
        if self.coverage == RelayCoverage::Maximum {
            let policy = self
                .paired_handover
                .ok_or(RelayPlanError::MissingPairHandoverPolicy)?;
            if policy.max_bluetooth_heartbeat_age_ms == 0 {
                return Err(RelayPlanError::InvalidPairHandoverPolicy);
            }
        }
        Ok(())
    }
}

/// The two automatic chain-coverage choices exposed to ARC UI.
///
/// Manual relay-count and relay-member allocation can still intentionally
/// create a degraded or more heavily staffed station. Automatic allocation is
/// always exactly one aircraft per station for `Minimum` or an active/standby
/// pair for `Maximum`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayCoverage {
    /// The smallest measured, healthy chain: one transmitting aircraft at
    /// each required relay station.
    Minimum,
    /// Two aircraft at every relay station. Both are assigned to receive
    /// traffic; exactly one is the active radio broadcaster and the other is
    /// its Bluetooth-coordinated standby.
    Maximum,
}

impl RelayCoverage {
    fn relays_per_station(self) -> usize {
        match self {
            Self::Minimum => 1,
            Self::Maximum => 2,
        }
    }
}

/// Explicit local failover timing for a maximum-coverage station pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayPairHandoverPolicy {
    /// The receiving standby takes over only after it has not received a
    /// matching Bluetooth heartbeat from the active broadcaster for this long.
    pub max_bluetooth_heartbeat_age_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RelayAllocationMode {
    Automatic,
    RelayCount {
        relay_count: usize,
        /// Optional manual station count. Omit to preserve required coverage
        /// stations and trade relay count against local redundancy.
        station_count: Option<usize>,
    },
    RelayMembers {
        members: Vec<NodeId>,
        station_count: Option<usize>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayCorridorRequest {
    pub base: GeoPoint,
    pub objective_entry: GeoPoint,
    pub candidates: Vec<RelayCandidate>,
    pub policy: RelayPolicy,
    pub allocation: RelayAllocationMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayFeasibility {
    Healthy,
    Degraded,
    Infeasible,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayStation {
    pub station_index: usize,
    pub position: GeoPoint,
    pub members: Vec<NodeId>,
    /// Exactly one member transmits relay traffic at a time. In maximum
    /// coverage the other member is an already-receiving Bluetooth-linked
    /// standby, so its companion can assume broadcast duty after the local
    /// handover policy detects an active-peer failure.
    pub transmission: RelayStationTransmission,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayStationTransmission {
    pub active_broadcaster: NodeId,
    /// Empty for a minimum-coverage station. Maximum-coverage automatic
    /// stations contain one member here; manual overrides may contain more.
    pub standby_receivers: Vec<NodeId>,
    pub peer_coordination: Option<RelayPeerCoordination>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayPeerCoordination {
    Bluetooth,
}

/// A heartbeat delivered locally over the Bluetooth link from an active relay
/// broadcaster to its designated receiving standby.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayPairHeartbeat {
    pub station_index: usize,
    pub active_broadcaster: NodeId,
    pub standby_receiver: NodeId,
    pub observed_at_ms: u64,
}

/// The local radio mode a paired-station companion must apply. The adapter
/// retains receive capability in every assigned mode; it enables relay
/// broadcast on exactly one member at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayPairBroadcastAction {
    /// The planned primary is the active relay broadcaster and also receives.
    BroadcastAndReceive,
    /// A planned standby is receiving but must keep its relay transmitter
    /// disabled while the active peer's Bluetooth heartbeat is fresh.
    ReceiveOnly,
    /// The designated standby has not observed a fresh active-peer heartbeat
    /// and should locally enable relay broadcast while retaining receive.
    TakeoverBroadcastAndReceive,
    /// This aircraft is not assigned to the station.
    Unassigned,
}

impl RelayStationTransmission {
    /// Determines local broadcast ownership for a planned station. Only the
    /// first standby is a failover peer; manually added extra standbys remain
    /// receive-only until a later mission reconfiguration assigns their role.
    pub fn broadcast_action(
        &self,
        station_index: usize,
        local_node_id: &NodeId,
        now_ms: u64,
        handover: RelayPairHandoverPolicy,
        latest_heartbeat: Option<&RelayPairHeartbeat>,
    ) -> Result<RelayPairBroadcastAction, RelayPairHandoverError> {
        if local_node_id == &self.active_broadcaster {
            return Ok(RelayPairBroadcastAction::BroadcastAndReceive);
        }
        let Some(designated_standby) = self.standby_receivers.first() else {
            return Ok(RelayPairBroadcastAction::Unassigned);
        };
        if local_node_id != designated_standby {
            return Ok(if self.standby_receivers.contains(local_node_id) {
                RelayPairBroadcastAction::ReceiveOnly
            } else {
                RelayPairBroadcastAction::Unassigned
            });
        }
        let heartbeat_is_fresh = match latest_heartbeat {
            Some(heartbeat) => {
                if heartbeat.station_index != station_index
                    || heartbeat.active_broadcaster != self.active_broadcaster
                    || heartbeat.standby_receiver != *designated_standby
                    || heartbeat.observed_at_ms > now_ms
                {
                    return Err(RelayPairHandoverError::MismatchedHeartbeat);
                }
                now_ms.saturating_sub(heartbeat.observed_at_ms)
                    <= handover.max_bluetooth_heartbeat_age_ms
            }
            None => false,
        };
        Ok(if heartbeat_is_fresh {
            RelayPairBroadcastAction::ReceiveOnly
        } else {
            RelayPairBroadcastAction::TakeoverBroadcastAndReceive
        })
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RelayPairHandoverError {
    #[error("Bluetooth heartbeat does not match the assigned relay pair")]
    MismatchedHeartbeat,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayPlan {
    pub route_distance_m: f64,
    pub usable_segment_m: f64,
    pub range_evidence: RangeEvidence,
    /// The automatic coverage policy used to compute the recommendation.
    pub coverage: RelayCoverage,
    /// The explicit local active/standby failover policy for `maximum`
    /// coverage. It is absent for `minimum` coverage.
    pub paired_handover: Option<RelayPairHandoverPolicy>,
    /// `false` when the plan is based only on a free-space model, even if its
    /// geometry and redundancy are otherwise healthy.
    pub activation_ready: bool,
    pub recommended_station_count: usize,
    pub recommended_relay_count: usize,
    pub reserved_relay_count: usize,
    pub mission_drones_remaining: usize,
    pub max_planned_segment_m: f64,
    pub range_utilization: f32,
    pub minimum_relays_per_station: usize,
    pub minimum_station_failure_tolerance: usize,
    pub feasibility: RelayFeasibility,
    pub stations: Vec<RelayStation>,
    pub relay_members: Vec<NodeId>,
    pub mission_members: Vec<NodeId>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RelayPlanner;

impl RelayPlanner {
    pub fn plan(&self, request: &RelayCorridorRequest) -> Result<RelayPlan, RelayPlanError> {
        let base = request.base.validate()?;
        let objective = request.objective_entry.validate()?;
        if !(MIN_SUPPORTED_SWARM_SIZE..=MAX_SUPPORTED_SWARM_SIZE)
            .contains(&request.candidates.len())
        {
            return Err(RelayPlanError::UnsupportedSwarmSize(
                request.candidates.len(),
            ));
        }
        validate_candidates(&request.candidates)?;
        request.policy.validate_handover()?;

        let usable_segment_m = request.policy.usable_segment_m()?;
        let range_evidence = request.policy.range.evidence();
        let route_distance_m = base.distance_to(objective);
        let required_link_count = (route_distance_m / usable_segment_m).ceil().max(1.0) as usize;
        let recommended_station_count = required_link_count.saturating_sub(1);
        let recommended_relay_count =
            recommended_station_count.saturating_mul(request.policy.relays_per_station());

        let eligible = ranked_eligible_candidates(&request.candidates);
        let (relay_members, manual_station_count) = select_relay_members(
            &request.allocation,
            &eligible,
            &request.candidates,
            recommended_relay_count,
        )?;
        let station_count = choose_station_count(
            relay_members.len(),
            recommended_station_count,
            manual_station_count,
        )?;
        let stations = build_stations(
            base,
            objective,
            station_count,
            &relay_members,
            request.policy.coverage,
        );
        let max_planned_segment_m = route_distance_m / (station_count + 1) as f64;
        let range_utilization = (max_planned_segment_m / usable_segment_m) as f32;
        let minimum_relays_per_station = stations
            .iter()
            .map(|station| station.members.len())
            .min()
            .unwrap_or(0);
        let minimum_station_failure_tolerance = minimum_relays_per_station.saturating_sub(1);
        let coverage_ok = max_planned_segment_m <= usable_segment_m * (1.0 + f64::EPSILON);
        let redundancy_ok =
            station_count == 0 || minimum_relays_per_station >= request.policy.relays_per_station();
        let feasibility = if !coverage_ok {
            RelayFeasibility::Infeasible
        } else if !redundancy_ok || relay_members.len() < recommended_relay_count {
            RelayFeasibility::Degraded
        } else {
            RelayFeasibility::Healthy
        };

        let relay_set: BTreeSet<NodeId> = relay_members.iter().cloned().collect();
        let mut mission_members: Vec<NodeId> = request
            .candidates
            .iter()
            .filter(|candidate| {
                candidate.available
                    && candidate.mission_eligible
                    && !relay_set.contains(&candidate.node_id)
            })
            .map(|candidate| candidate.node_id.clone())
            .collect();
        mission_members.sort();

        let mut warnings = Vec::new();
        if recommended_station_count == 0 {
            warnings.push("No relay aircraft are required at the modeled range and margin.".into());
        }
        if relay_members.len() < recommended_relay_count {
            warnings.push(format!(
                "The plan reserves {} relay aircraft; {} are recommended for coverage and local redundancy.",
                relay_members.len(), recommended_relay_count
            ));
        } else if relay_members.len() > recommended_relay_count {
            warnings.push(format!(
                "The plan reserves {} additional relay aircraft above the recommendation.",
                relay_members.len() - recommended_relay_count
            ));
        }
        if !coverage_ok {
            warnings.push(format!(
                "Planned {:.1} m hops exceed the derated {:.1} m usable segment.",
                max_planned_segment_m, usable_segment_m
            ));
        }
        if station_count > 0 && !redundancy_ok {
            warnings.push(format!(
                "At least one relay station has {} aircraft; {} are desired.",
                minimum_relays_per_station,
                request.policy.relays_per_station()
            ));
        }
        if range_evidence == RangeEvidence::FreeSpaceModel {
            warnings.push(
                "This relay count uses a free-space link budget only. Calibrate it against current radio, antenna, altitude, terrain, and link measurements before mission activation."
                    .into(),
            );
        }

        Ok(RelayPlan {
            route_distance_m,
            usable_segment_m,
            range_evidence,
            coverage: request.policy.coverage,
            paired_handover: request.policy.paired_handover,
            activation_ready: feasibility == RelayFeasibility::Healthy
                && range_evidence == RangeEvidence::FieldCalibrated,
            recommended_station_count,
            recommended_relay_count,
            reserved_relay_count: relay_members.len(),
            mission_drones_remaining: mission_members.len(),
            max_planned_segment_m,
            range_utilization,
            minimum_relays_per_station,
            minimum_station_failure_tolerance,
            feasibility,
            stations,
            relay_members,
            mission_members,
            warnings,
        })
    }
}

fn validate_candidates(candidates: &[RelayCandidate]) -> Result<(), RelayPlanError> {
    let mut node_ids = BTreeSet::new();
    for candidate in candidates {
        if !node_ids.insert(candidate.node_id.clone()) {
            return Err(RelayPlanError::DuplicateCandidate(
                candidate.node_id.clone(),
            ));
        }
        for score in [candidate.relay_suitability, candidate.mission_utility] {
            if !score.is_finite() || !(0.0..=1.0).contains(&score) {
                return Err(RelayPlanError::InvalidCandidateScore(
                    candidate.node_id.clone(),
                ));
            }
        }
    }
    Ok(())
}

fn ranked_eligible_candidates(candidates: &[RelayCandidate]) -> Vec<&RelayCandidate> {
    let mut eligible: Vec<&RelayCandidate> = candidates
        .iter()
        .filter(|candidate| candidate.available && candidate.relay_eligible)
        .collect();
    eligible.sort_by(|left, right| {
        right
            .relay_suitability
            .total_cmp(&left.relay_suitability)
            .then_with(|| left.mission_utility.total_cmp(&right.mission_utility))
            .then_with(|| left.node_id.cmp(&right.node_id))
    });
    eligible
}

fn select_relay_members(
    allocation: &RelayAllocationMode,
    eligible: &[&RelayCandidate],
    all_candidates: &[RelayCandidate],
    recommended_relay_count: usize,
) -> Result<(Vec<NodeId>, Option<usize>), RelayPlanError> {
    match allocation {
        RelayAllocationMode::Automatic => Ok((
            eligible
                .iter()
                .take(recommended_relay_count)
                .map(|candidate| candidate.node_id.clone())
                .collect(),
            None,
        )),
        RelayAllocationMode::RelayCount {
            relay_count,
            station_count,
        } => {
            if *relay_count > eligible.len() {
                return Err(RelayPlanError::InsufficientEligibleRelays {
                    requested: *relay_count,
                    available: eligible.len(),
                });
            }
            Ok((
                eligible
                    .iter()
                    .take(*relay_count)
                    .map(|candidate| candidate.node_id.clone())
                    .collect(),
                *station_count,
            ))
        }
        RelayAllocationMode::RelayMembers {
            members,
            station_count,
        } => {
            let candidate_by_id: BTreeMap<&NodeId, &RelayCandidate> = all_candidates
                .iter()
                .map(|candidate| (&candidate.node_id, candidate))
                .collect();
            let mut unique = BTreeSet::new();
            for node_id in members {
                if !unique.insert(node_id.clone()) {
                    return Err(RelayPlanError::DuplicateManualRelay(node_id.clone()));
                }
                let candidate = candidate_by_id
                    .get(node_id)
                    .ok_or_else(|| RelayPlanError::UnknownManualRelay(node_id.clone()))?;
                if !candidate.available || !candidate.relay_eligible {
                    return Err(RelayPlanError::IneligibleManualRelay(node_id.clone()));
                }
            }
            Ok((unique.into_iter().collect(), *station_count))
        }
    }
}

fn choose_station_count(
    relay_count: usize,
    recommended_station_count: usize,
    manual_station_count: Option<usize>,
) -> Result<usize, RelayPlanError> {
    if let Some(station_count) = manual_station_count {
        if (relay_count == 0 && station_count != 0)
            || (relay_count > 0 && (station_count == 0 || station_count > relay_count))
        {
            return Err(RelayPlanError::InvalidManualStationCount {
                relay_count,
                station_count,
            });
        }
        return Ok(station_count);
    }
    Ok(recommended_station_count.min(relay_count))
}

fn build_stations(
    base: GeoPoint,
    objective: GeoPoint,
    station_count: usize,
    relay_members: &[NodeId],
    coverage: RelayCoverage,
) -> Vec<RelayStation> {
    if station_count == 0 {
        return Vec::new();
    }
    let mut members = vec![Vec::new(); station_count];
    for (index, node_id) in relay_members.iter().enumerate() {
        members[index % station_count].push(node_id.clone());
    }
    members
        .into_iter()
        .enumerate()
        .map(|(index, members)| {
            let active_broadcaster = members
                .first()
                .cloned()
                .expect("every relay station has at least one assigned member");
            let standby_receivers: Vec<NodeId> = members.iter().skip(1).cloned().collect();
            let peer_coordination = (coverage == RelayCoverage::Maximum
                && !standby_receivers.is_empty())
            .then_some(RelayPeerCoordination::Bluetooth);
            RelayStation {
                station_index: index,
                position: base
                    .interpolate(objective, (index + 1) as f64 / (station_count + 1) as f64),
                members,
                transmission: RelayStationTransmission {
                    active_broadcaster,
                    standby_receivers,
                    peer_coordination,
                },
            }
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentPool {
    Relay,
    Mission,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorTaskGroup {
    pub group_id: String,
    pub pool: AssignmentPool,
    pub instruction: String,
    pub members: Vec<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissionAllocation {
    pub mission_id: Uuid,
    pub generation: u64,
    pub relay_plan: RelayPlan,
    pub task_groups: Vec<OperatorTaskGroup>,
}

impl MissionAllocation {
    pub fn new(
        mission_id: Uuid,
        generation: u64,
        relay_plan: RelayPlan,
        task_groups: Vec<OperatorTaskGroup>,
    ) -> Result<Self, RelayPlanError> {
        if generation == 0 {
            return Err(RelayPlanError::InvalidMissionGeneration);
        }
        let relay_members: BTreeSet<NodeId> = relay_plan.relay_members.iter().cloned().collect();
        let mission_members: BTreeSet<NodeId> =
            relay_plan.mission_members.iter().cloned().collect();
        let mut group_ids = BTreeSet::new();
        let mut assigned_members = BTreeSet::new();
        for group in &task_groups {
            if group.group_id.trim().is_empty() || group.instruction.trim().is_empty() {
                return Err(RelayPlanError::EmptyGroupField);
            }
            if !group_ids.insert(group.group_id.clone()) {
                return Err(RelayPlanError::DuplicateGroupId(group.group_id.clone()));
            }
            for member in &group.members {
                let allowed = match group.pool {
                    AssignmentPool::Relay => relay_members.contains(member),
                    AssignmentPool::Mission => mission_members.contains(member),
                };
                if !allowed {
                    return Err(RelayPlanError::MemberOutsideGroupPool(member.clone()));
                }
                if !assigned_members.insert(member.clone()) {
                    return Err(RelayPlanError::MemberAssignedTwice(member.clone()));
                }
            }
        }
        Ok(Self {
            mission_id,
            generation,
            relay_plan,
            task_groups,
        })
    }
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum RelayPlanError {
    #[error("relay planning supports formations of 5-1024 aircraft, got {0}")]
    UnsupportedSwarmSize(usize),
    #[error("relay planning coordinate is non-finite or outside latitude/longitude bounds")]
    InvalidCoordinate,
    #[error("relay altitude {0} m MSL exceeds the 9,144 m system ceiling")]
    AboveSystemCeiling(f64),
    #[error("field-calibrated usable radio range must be finite and positive")]
    InvalidUsableRange,
    #[error(
        "radio link-budget inputs must be finite, with positive frequency and non-negative losses"
    )]
    InvalidLinkBudget,
    #[error("desired relays per station must be at least one")]
    InvalidStationRedundancy,
    #[error("maximum relay coverage requires an explicit Bluetooth pair handover policy")]
    MissingPairHandoverPolicy,
    #[error("Bluetooth pair heartbeat age must be greater than zero")]
    InvalidPairHandoverPolicy,
    #[error("duplicate relay candidate {0}")]
    DuplicateCandidate(NodeId),
    #[error("candidate {0} has a score outside 0.0-1.0")]
    InvalidCandidateScore(NodeId),
    #[error("requested {requested} relay aircraft but only {available} are eligible")]
    InsufficientEligibleRelays { requested: usize, available: usize },
    #[error("manual relay member {0} is not in the formation")]
    UnknownManualRelay(NodeId),
    #[error("manual relay member {0} is unavailable or relay-ineligible")]
    IneligibleManualRelay(NodeId),
    #[error("manual relay member {0} is duplicated")]
    DuplicateManualRelay(NodeId),
    #[error("cannot place {relay_count} relay aircraft into {station_count} relay stations")]
    InvalidManualStationCount {
        relay_count: usize,
        station_count: usize,
    },
    #[error("operator group ID and instruction must be non-empty")]
    EmptyGroupField,
    #[error("duplicate operator group ID {0:?}")]
    DuplicateGroupId(String),
    #[error("aircraft {0} is not in the selected relay or mission pool")]
    MemberOutsideGroupPool(NodeId),
    #[error("aircraft {0} is assigned to more than one operator group")]
    MemberAssignedTwice(NodeId),
    #[error("mission allocation generation must be positive")]
    InvalidMissionGeneration,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates(count: usize) -> Vec<RelayCandidate> {
        (0..count)
            .map(|index| RelayCandidate {
                node_id: NodeId::from(format!("aircraft-{index:03}")),
                available: true,
                relay_eligible: true,
                mission_eligible: true,
                relay_suitability: 0.8,
                mission_utility: 0.5,
            })
            .collect()
    }

    fn synthetic_one_mile_request(allocation: RelayAllocationMode) -> RelayCorridorRequest {
        RelayCorridorRequest {
            base: GeoPoint {
                latitude_deg: 0.0,
                longitude_deg: 0.0,
                msl_m: 100.0,
            },
            objective_entry: GeoPoint {
                latitude_deg: 0.0,
                longitude_deg: 0.014_473,
                msl_m: 100.0,
            },
            candidates: candidates(50),
            policy: RelayPolicy {
                range: RelayRangeModel::FieldCalibrated {
                    usable_segment_m: 136.0,
                },
                coverage: RelayCoverage::Maximum,
                paired_handover: Some(RelayPairHandoverPolicy {
                    max_bluetooth_heartbeat_age_ms: 1_500,
                }),
            },
            allocation,
        }
    }

    #[test]
    fn synthetic_calibration_produces_expected_relay_reservation() {
        let plan = RelayPlanner
            .plan(&synthetic_one_mile_request(RelayAllocationMode::Automatic))
            .unwrap();

        assert_eq!(plan.recommended_station_count, 11);
        assert_eq!(plan.recommended_relay_count, 22);
        assert_eq!(plan.reserved_relay_count, 22);
        assert_eq!(plan.mission_drones_remaining, 28);
        assert_eq!(plan.minimum_station_failure_tolerance, 1);
        assert_eq!(plan.coverage, RelayCoverage::Maximum);
        assert_eq!(
            plan.paired_handover,
            Some(RelayPairHandoverPolicy {
                max_bluetooth_heartbeat_age_ms: 1_500,
            })
        );
        assert!(plan.stations.iter().all(|station| {
            station.transmission.peer_coordination == Some(RelayPeerCoordination::Bluetooth)
                && station.transmission.standby_receivers.len() == 1
                && station.transmission.active_broadcaster == station.members[0]
        }));
        assert_eq!(plan.feasibility, RelayFeasibility::Healthy);
        assert_eq!(plan.range_evidence, RangeEvidence::FieldCalibrated);
        assert!(plan.activation_ready);
    }

    #[test]
    fn minimum_coverage_reserves_one_broadcaster_per_station() {
        let mut request = synthetic_one_mile_request(RelayAllocationMode::Automatic);
        request.policy.coverage = RelayCoverage::Minimum;
        request.policy.paired_handover = None;

        let plan = RelayPlanner.plan(&request).unwrap();

        assert_eq!(plan.recommended_station_count, 11);
        assert_eq!(plan.recommended_relay_count, 11);
        assert_eq!(plan.reserved_relay_count, 11);
        assert_eq!(plan.mission_drones_remaining, 39);
        assert_eq!(plan.minimum_station_failure_tolerance, 0);
        assert!(plan.stations.iter().all(|station| {
            station.members.len() == 1
                && station.transmission.standby_receivers.is_empty()
                && station.transmission.peer_coordination.is_none()
                && station.transmission.active_broadcaster == station.members[0]
        }));
    }

    #[test]
    fn paired_station_uses_bluetooth_heartbeat_for_single_broadcaster_takeover() {
        let plan = RelayPlanner
            .plan(&synthetic_one_mile_request(RelayAllocationMode::Automatic))
            .unwrap();
        let station = &plan.stations[0];
        let policy = plan.paired_handover.unwrap();
        let active = station.transmission.active_broadcaster.clone();
        let standby = station.transmission.standby_receivers[0].clone();
        let heartbeat = RelayPairHeartbeat {
            station_index: station.station_index,
            active_broadcaster: active.clone(),
            standby_receiver: standby.clone(),
            observed_at_ms: 10_000,
        };

        assert_eq!(
            station
                .transmission
                .broadcast_action(station.station_index, &active, 12_000, policy, None)
                .unwrap(),
            RelayPairBroadcastAction::BroadcastAndReceive
        );
        assert_eq!(
            station
                .transmission
                .broadcast_action(
                    station.station_index,
                    &standby,
                    11_000,
                    policy,
                    Some(&heartbeat)
                )
                .unwrap(),
            RelayPairBroadcastAction::ReceiveOnly
        );
        assert_eq!(
            station
                .transmission
                .broadcast_action(
                    station.station_index,
                    &standby,
                    12_000,
                    policy,
                    Some(&heartbeat)
                )
                .unwrap(),
            RelayPairBroadcastAction::TakeoverBroadcastAndReceive
        );
    }

    #[test]
    fn maximum_coverage_requires_an_explicit_bluetooth_handover_policy() {
        let mut request = synthetic_one_mile_request(RelayAllocationMode::Automatic);
        request.policy.paired_handover = None;

        assert_eq!(
            RelayPlanner.plan(&request),
            Err(RelayPlanError::MissingPairHandoverPolicy)
        );
    }

    #[test]
    fn decreasing_chain_reports_redundancy_and_coverage_impact() {
        let degraded = RelayPlanner
            .plan(&synthetic_one_mile_request(
                RelayAllocationMode::RelayCount {
                    relay_count: 20,
                    station_count: None,
                },
            ))
            .unwrap();
        assert_eq!(degraded.feasibility, RelayFeasibility::Degraded);
        assert_eq!(degraded.mission_drones_remaining, 30);
        assert_eq!(degraded.minimum_relays_per_station, 1);

        let infeasible = RelayPlanner
            .plan(&synthetic_one_mile_request(
                RelayAllocationMode::RelayCount {
                    relay_count: 10,
                    station_count: None,
                },
            ))
            .unwrap();
        assert_eq!(infeasible.feasibility, RelayFeasibility::Infeasible);
        assert!(infeasible.range_utilization > 1.0);
    }

    #[test]
    fn increasing_chain_can_shorten_hops() {
        let plan = RelayPlanner
            .plan(&synthetic_one_mile_request(
                RelayAllocationMode::RelayCount {
                    relay_count: 24,
                    station_count: Some(12),
                },
            ))
            .unwrap();

        assert_eq!(plan.feasibility, RelayFeasibility::Healthy);
        assert_eq!(plan.reserved_relay_count, 24);
        assert_eq!(plan.mission_drones_remaining, 26);
        assert!(plan.max_planned_segment_m < 125.0);
    }

    #[test]
    fn operator_can_assign_individuals_and_groups_without_crossing_pools() {
        let plan = RelayPlanner
            .plan(&synthetic_one_mile_request(RelayAllocationMode::Automatic))
            .unwrap();
        let relay = plan.relay_members[0].clone();
        let mission_1 = plan.mission_members[0].clone();
        let mission_2 = plan.mission_members[1].clone();
        let allocation = MissionAllocation::new(
            Uuid::from_u128(42),
            1,
            plan,
            vec![
                OperatorTaskGroup {
                    group_id: "relay-west".into(),
                    pool: AssignmentPool::Relay,
                    instruction: "hold the assigned relay station".into(),
                    members: vec![relay],
                },
                OperatorTaskGroup {
                    group_id: "search-alpha".into(),
                    pool: AssignmentPool::Mission,
                    instruction: "search the marked western sector".into(),
                    members: vec![mission_1, mission_2],
                },
            ],
        )
        .unwrap();

        assert_eq!(allocation.task_groups.len(), 2);
    }

    #[test]
    fn duplicate_group_assignment_is_rejected() {
        let plan = RelayPlanner
            .plan(&synthetic_one_mile_request(RelayAllocationMode::Automatic))
            .unwrap();
        let member = plan.mission_members[0].clone();
        let result = MissionAllocation::new(
            Uuid::from_u128(42),
            1,
            plan,
            vec![
                OperatorTaskGroup {
                    group_id: "one".into(),
                    pool: AssignmentPool::Mission,
                    instruction: "first".into(),
                    members: vec![member.clone()],
                },
                OperatorTaskGroup {
                    group_id: "two".into(),
                    pool: AssignmentPool::Mission,
                    instruction: "second".into(),
                    members: vec![member.clone()],
                },
            ],
        );

        assert_eq!(result, Err(RelayPlanError::MemberAssignedTwice(member)));
    }

    #[test]
    fn sl5200_free_space_budget_requires_calibration_before_activation() {
        let budget = RadioLinkBudget::silvus_sl5200_5_mhz(2_350.0, 20.0);
        assert_eq!(budget.transmitter_power_dbm, 30.0);
        assert_eq!(SILVUS_SL5220_TOTAL_RF_POWER_DBM, 33.0);
        let free_space_range_m = budget.max_free_space_range_m().unwrap();
        assert!((3_000.0..=4_000.0).contains(&free_space_range_m));

        let mut request = synthetic_one_mile_request(RelayAllocationMode::Automatic);
        request.policy.range = RelayRangeModel::FreeSpaceLinkBudget { budget };
        let plan = RelayPlanner.plan(&request).unwrap();
        assert_eq!(plan.range_evidence, RangeEvidence::FreeSpaceModel);
        assert!(!plan.activation_ready);
        assert!(plan
            .warnings
            .iter()
            .any(|warning| warning.contains("free-space link budget")));
    }
}

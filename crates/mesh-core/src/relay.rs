use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{NodeId, MAX_SUPPORTED_SWARM_SIZE, MIN_SUPPORTED_SWARM_SIZE, SYSTEM_MAX_MSL_M};

const EARTH_RADIUS_M: f64 = 6_371_000.0;

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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RelayPolicy {
    /// Modeled or measured reliable range before AVIAN's safety derating.
    pub nominal_reliable_range_m: f64,
    /// Fraction of nominal range held back for motion, terrain, and RF variation.
    pub safety_margin_ratio: f32,
    /// Desired aircraft at each relay station. Two tolerates one local loss.
    pub desired_relays_per_station: usize,
}

impl RelayPolicy {
    pub fn usable_segment_m(self) -> Result<f64, RelayPlanError> {
        if !self.nominal_reliable_range_m.is_finite() || self.nominal_reliable_range_m <= 0.0 {
            return Err(RelayPlanError::InvalidNominalRange);
        }
        if !self.safety_margin_ratio.is_finite() || !(0.0..0.9).contains(&self.safety_margin_ratio)
        {
            return Err(RelayPlanError::InvalidSafetyMargin);
        }
        if self.desired_relays_per_station == 0 {
            return Err(RelayPlanError::InvalidStationRedundancy);
        }
        Ok(self.nominal_reliable_range_m * (1.0 - f64::from(self.safety_margin_ratio)))
    }
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayPlan {
    pub route_distance_m: f64,
    pub usable_segment_m: f64,
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

        let usable_segment_m = request.policy.usable_segment_m()?;
        let route_distance_m = base.distance_to(objective);
        let required_link_count = (route_distance_m / usable_segment_m).ceil().max(1.0) as usize;
        let recommended_station_count = required_link_count.saturating_sub(1);
        let recommended_relay_count =
            recommended_station_count.saturating_mul(request.policy.desired_relays_per_station);

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
        let stations = build_stations(base, objective, station_count, &relay_members);
        let max_planned_segment_m = route_distance_m / (station_count + 1) as f64;
        let range_utilization = (max_planned_segment_m / usable_segment_m) as f32;
        let minimum_relays_per_station = stations
            .iter()
            .map(|station| station.members.len())
            .min()
            .unwrap_or(0);
        let minimum_station_failure_tolerance = minimum_relays_per_station.saturating_sub(1);
        let coverage_ok = max_planned_segment_m <= usable_segment_m * (1.0 + f64::EPSILON);
        let redundancy_ok = station_count == 0
            || minimum_relays_per_station >= request.policy.desired_relays_per_station;
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
                minimum_relays_per_station, request.policy.desired_relays_per_station
            ));
        }

        Ok(RelayPlan {
            route_distance_m,
            usable_segment_m,
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
        .map(|(index, members)| RelayStation {
            station_index: index,
            position: base.interpolate(objective, (index + 1) as f64 / (station_count + 1) as f64),
            members,
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
    pub relay_plan: RelayPlan,
    pub task_groups: Vec<OperatorTaskGroup>,
}

impl MissionAllocation {
    pub fn new(
        relay_plan: RelayPlan,
        task_groups: Vec<OperatorTaskGroup>,
    ) -> Result<Self, RelayPlanError> {
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
            relay_plan,
            task_groups,
        })
    }
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum RelayPlanError {
    #[error("relay planning supports formations of 5-200 aircraft, got {0}")]
    UnsupportedSwarmSize(usize),
    #[error("relay planning coordinate is non-finite or outside latitude/longitude bounds")]
    InvalidCoordinate,
    #[error("relay altitude {0} m MSL exceeds the 7,620 m system ceiling")]
    AboveSystemCeiling(f64),
    #[error("nominal reliable radio range must be finite and positive")]
    InvalidNominalRange,
    #[error("radio safety margin must be finite and in [0.0, 0.9)")]
    InvalidSafetyMargin,
    #[error("desired relays per station must be at least one")]
    InvalidStationRedundancy,
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

    fn one_mile_request(allocation: RelayAllocationMode) -> RelayCorridorRequest {
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
                nominal_reliable_range_m: 160.0,
                safety_margin_ratio: 0.15,
                desired_relays_per_station: 2,
            },
            allocation,
        }
    }

    #[test]
    fn fifty_aircraft_one_mile_reserves_twenty_two_relays() {
        let plan = RelayPlanner
            .plan(&one_mile_request(RelayAllocationMode::Automatic))
            .unwrap();

        assert_eq!(plan.recommended_station_count, 11);
        assert_eq!(plan.recommended_relay_count, 22);
        assert_eq!(plan.reserved_relay_count, 22);
        assert_eq!(plan.mission_drones_remaining, 28);
        assert_eq!(plan.minimum_station_failure_tolerance, 1);
        assert_eq!(plan.feasibility, RelayFeasibility::Healthy);
    }

    #[test]
    fn decreasing_chain_reports_redundancy_and_coverage_impact() {
        let degraded = RelayPlanner
            .plan(&one_mile_request(RelayAllocationMode::RelayCount {
                relay_count: 20,
                station_count: None,
            }))
            .unwrap();
        assert_eq!(degraded.feasibility, RelayFeasibility::Degraded);
        assert_eq!(degraded.mission_drones_remaining, 30);
        assert_eq!(degraded.minimum_relays_per_station, 1);

        let infeasible = RelayPlanner
            .plan(&one_mile_request(RelayAllocationMode::RelayCount {
                relay_count: 10,
                station_count: None,
            }))
            .unwrap();
        assert_eq!(infeasible.feasibility, RelayFeasibility::Infeasible);
        assert!(infeasible.range_utilization > 1.0);
    }

    #[test]
    fn increasing_chain_can_shorten_hops() {
        let plan = RelayPlanner
            .plan(&one_mile_request(RelayAllocationMode::RelayCount {
                relay_count: 24,
                station_count: Some(12),
            }))
            .unwrap();

        assert_eq!(plan.feasibility, RelayFeasibility::Healthy);
        assert_eq!(plan.reserved_relay_count, 24);
        assert_eq!(plan.mission_drones_remaining, 26);
        assert!(plan.max_planned_segment_m < 125.0);
    }

    #[test]
    fn operator_can_assign_individuals_and_groups_without_crossing_pools() {
        let plan = RelayPlanner
            .plan(&one_mile_request(RelayAllocationMode::Automatic))
            .unwrap();
        let relay = plan.relay_members[0].clone();
        let mission_1 = plan.mission_members[0].clone();
        let mission_2 = plan.mission_members[1].clone();
        let allocation = MissionAllocation::new(
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
            .plan(&one_mile_request(RelayAllocationMode::Automatic))
            .unwrap();
        let member = plan.mission_members[0].clone();
        let result = MissionAllocation::new(
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
}

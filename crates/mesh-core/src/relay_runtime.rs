use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    GeoPoint, LinkGeometry, LinkMetrics, NodeId, RelayCandidate, Telemetry, TransportKind,
    MAX_SUPPORTED_SWARM_SIZE, MIN_SUPPORTED_SWARM_SIZE,
};

/// A non-aircraft endpoint that must remain reachable, normally the ground
/// station. It deliberately has no special authority over the formation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayAnchor {
    pub node_id: NodeId,
    pub position: GeoPoint,
    /// Estimated height above local terrain. It is required for RF assessment
    /// by the link collector, but remains separate from the MSL system limit.
    pub agl_m: Option<f64>,
}

/// Current, shared state of an aircraft that may be retained for its mission
/// or reassigned to relay duty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveRelayCandidate {
    #[serde(flatten)]
    pub candidate: RelayCandidate,
    pub position: GeoPoint,
    pub agl_m: Option<f64>,
}

/// A rolling, bidirectional aggregate for one underlay between two AVIAN
/// nodes. The collector must only set `bidirectional` after both endpoints
/// have reported the observation window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayLinkObservation {
    pub first: NodeId,
    pub second: NodeId,
    pub transport: TransportKind,
    pub observed_at_ms: u64,
    pub sample_window_ms: u64,
    pub bidirectional: bool,
    pub available: bool,
    pub metrics: LinkMetrics,
    pub geometry: LinkGeometry,
    /// Received power when the radio exposes it. This is informational unless
    /// a margin threshold is configured below.
    pub received_power_dbm: Option<f32>,
    /// Measured margin above the selected receive threshold, if the radio
    /// exposes it. This is stronger evidence than an RSSI percentage.
    pub link_margin_db: Option<f32>,
}

/// Mission-specific acceptance criteria for live radio observations. These
/// values are deliberately supplied by ARC/calibration rather than hidden as
/// generic radio defaults.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RelayHealthPolicy {
    pub max_observation_age_ms: u64,
    pub max_latency_ms: f32,
    pub max_loss_ratio: f32,
    pub min_goodput_bps: f32,
    pub min_signal_quality: f32,
    pub min_stability: f32,
    pub min_fresnel_clearance_ratio: f32,
    pub min_link_margin_db: Option<f32>,
}

/// Runtime manual control applies to reassignment. A member list is exact:
/// automatic logic will never borrow an unlisted aircraft when it cannot form
/// a healthy path from that list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RuntimeRelayAllocationMode {
    Automatic,
    RelayMembers { members: Vec<NodeId> },
}

/// Durable mission policy distributed by ARC UI. Dynamic positions and radio
/// observations are intentionally excluded: each companion obtains those from
/// latest-value mesh records before it evaluates a reconfiguration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayRuntimeConfiguration {
    pub mission_id: Uuid,
    /// Generation of the allocation accepted when this runtime policy was
    /// installed. The onboard state advances it only after publishing a
    /// complete chain or release decision.
    pub generation: u64,
    /// Relay members committed in `generation` when the policy was installed.
    /// This makes an ARC-approved pre-mission chain the initial runtime state
    /// instead of forcing the companions to republish it on their first tick.
    #[serde(default)]
    pub current_relay_members: Vec<NodeId>,
    pub anchor: RelayAnchor,
    pub required_mission_members: Vec<NodeId>,
    pub candidates: Vec<RelayCandidate>,
    pub health_policy: RelayHealthPolicy,
    pub allocation: RuntimeRelayAllocationMode,
    /// Maximum age of a vehicle position before it becomes unavailable for
    /// relay assignment. This is explicit because it depends on vehicle speed
    /// and the mission's communication objective.
    pub max_position_age_ms: u64,
}

/// Latest-value mesh state used with a durable runtime configuration. The
/// current relay members come from the most recently accepted generation, not
/// from whichever node happens to evaluate the snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayRuntimeSnapshot {
    pub observed_at_ms: u64,
    pub current_generation: u64,
    pub current_relay_members: Vec<NodeId>,
    pub telemetry: Vec<Telemetry>,
    pub observations: Vec<RelayLinkObservation>,
}

impl RelayRuntimeConfiguration {
    /// Builds an evaluable request from a durable mission configuration and
    /// the latest synchronized vehicle/radio records. A relay candidate whose
    /// last position is too old remains in the inventory but is unavailable,
    /// so the planner can report reduced capacity rather than assuming it is
    /// still airborne and reachable.
    pub fn build_request(
        &self,
        snapshot: &RelayRuntimeSnapshot,
    ) -> Result<InFlightRelayRequest, RelayRuntimeConfigError> {
        if self.max_position_age_ms == 0 {
            return Err(RelayRuntimeConfigError::InvalidPositionAge);
        }
        let mut latest_telemetry: BTreeMap<NodeId, &Telemetry> = BTreeMap::new();
        for telemetry in &snapshot.telemetry {
            if telemetry.timestamp_ms > snapshot.observed_at_ms {
                continue;
            }
            let replace = latest_telemetry
                .get(&telemetry.source)
                .is_none_or(|existing| telemetry.timestamp_ms >= existing.timestamp_ms);
            if replace {
                latest_telemetry.insert(telemetry.source.clone(), telemetry);
            }
        }

        let candidates = self
            .candidates
            .iter()
            .cloned()
            .map(|mut candidate| {
                let telemetry = latest_telemetry.get(&candidate.node_id).ok_or_else(|| {
                    RelayRuntimeConfigError::MissingCandidateTelemetry(candidate.node_id.clone())
                })?;
                let position_fresh = snapshot
                    .observed_at_ms
                    .saturating_sub(telemetry.timestamp_ms)
                    <= self.max_position_age_ms;
                candidate.available &= position_fresh;
                Ok(LiveRelayCandidate {
                    candidate,
                    position: GeoPoint {
                        latitude_deg: telemetry.latitude_deg,
                        longitude_deg: telemetry.longitude_deg,
                        msl_m: telemetry.altitude.msl_m,
                    },
                    agl_m: telemetry.altitude.agl_m,
                })
            })
            .collect::<Result<Vec<_>, RelayRuntimeConfigError>>()?;

        Ok(InFlightRelayRequest {
            mission_id: self.mission_id,
            current_generation: snapshot.current_generation,
            observed_at_ms: snapshot.observed_at_ms,
            anchor: self.anchor.clone(),
            required_mission_members: self.required_mission_members.clone(),
            current_relay_members: snapshot.current_relay_members.clone(),
            candidates,
            observations: snapshot.observations.clone(),
            health_policy: self.health_policy,
            allocation: self.allocation.clone(),
        })
    }
}

/// The shared input that any AVIAN companion can evaluate independently while
/// a mission is active. It has no leader field; identical observations yield
/// identical chain decisions on every participant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InFlightRelayRequest {
    pub mission_id: Uuid,
    pub current_generation: u64,
    pub observed_at_ms: u64,
    pub anchor: RelayAnchor,
    /// Active mission members that must each have a current route to the
    /// anchor. They are not permitted as hidden intermediate relays.
    pub required_mission_members: Vec<NodeId>,
    /// Relay members committed by the currently accepted mission generation.
    /// Supplying this prevents a healthy chain from being republished every
    /// evaluation and lets AVIAN explicitly release relays when direct
    /// connectivity returns.
    #[serde(default)]
    pub current_relay_members: Vec<NodeId>,
    pub candidates: Vec<LiveRelayCandidate>,
    pub observations: Vec<RelayLinkObservation>,
    pub health_policy: RelayHealthPolicy,
    pub allocation: RuntimeRelayAllocationMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayRuntimeAction {
    /// Every required mission member has a currently healthy direct link.
    MaintainDirect,
    /// The currently committed relay group still matches the observed paths.
    MaintainRelayChain,
    /// Observed paths require one or more aircraft to be reserved as relays.
    FormRelayChain,
    /// Direct connectivity returned, so the prior relay group can be released
    /// back to the mission pool in the next generation.
    ReleaseRelayChain,
    /// No healthy observed path exists for one or more members. In automatic
    /// mode, move only through a measured/probing workflow; do not extrapolate
    /// a free-space estimate into an unobserved chain.
    BeginRangeDiscovery,
    /// The operator's exact manual list cannot create all required paths.
    /// AVIAN preserves the override instead of silently taking other aircraft.
    OperatorActionRequired,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayChainHop {
    pub from: NodeId,
    pub to: NodeId,
    pub transport: TransportKind,
    /// Current three-dimensional separation calculated from the participants'
    /// reported MSL positions, not a radio data-sheet distance.
    pub separation_m: f64,
    /// 0.0-1.0 score derived from the configured live-health thresholds.
    pub health_score: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayChainRoute {
    pub mission_member: NodeId,
    pub relay_members: Vec<NodeId>,
    pub hops: Vec<RelayChainHop>,
    pub minimum_health_score: f32,
}

/// One explicit relay group for ARC UI. Routes show its ordered use for every
/// served mission member, allowing a UI to display chains and branches without
/// pretending that any aircraft is a permanent "mother" node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayRoleGroup {
    pub group_id: String,
    pub members: Vec<NodeId>,
    pub serves_mission_members: Vec<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InFlightRelayDecision {
    pub mission_id: Uuid,
    pub previous_generation: u64,
    /// Incremented only for a complete automatic relay-chain proposal that
    /// can be distributed as a new mission generation.
    pub proposed_generation: u64,
    pub observed_at_ms: u64,
    pub action: RelayRuntimeAction,
    /// Aircraft committed to the complete relay-chain proposal. Partial paths
    /// shown during discovery are intentionally not counted as reservations.
    pub reserved_relay_count: usize,
    /// Available mission-eligible aircraft remaining after the proposed relay
    /// reservation, so ARC UI can show the real task-capacity impact.
    pub mission_drones_remaining: usize,
    pub relay_group: Option<RelayRoleGroup>,
    pub routes: Vec<RelayChainRoute>,
    pub disconnected_mission_members: Vec<NodeId>,
    /// Eligible non-mission aircraft ordered for a measured range-discovery
    /// workflow when no current path exists. This is not a blind movement
    /// command or an assumed radio range.
    pub nominated_probe_members: Vec<NodeId>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct InFlightRelayPlanner;

impl InFlightRelayPlanner {
    pub fn decide(
        &self,
        request: &InFlightRelayRequest,
    ) -> Result<InFlightRelayDecision, RelayRuntimeError> {
        validate_request(request)?;

        let candidate_by_id: BTreeMap<NodeId, &LiveRelayCandidate> = request
            .candidates
            .iter()
            .map(|candidate| (candidate.candidate.node_id.clone(), candidate))
            .collect();
        let required: BTreeSet<NodeId> = request.required_mission_members.iter().cloned().collect();
        let (allowed_relays, manual_override) =
            select_allowed_relays(&request.allocation, &candidate_by_id, &required)?;
        let positions = node_positions(&request.anchor, &candidate_by_id);
        let graph = build_observed_graph(request, &positions)?;

        let mut routes = Vec::new();
        let mut disconnected_mission_members = Vec::new();
        for mission_member in &required {
            if let Some(route) = find_observed_route(
                &request.anchor.node_id,
                mission_member,
                &allowed_relays,
                &candidate_by_id,
                &positions,
                &graph,
            ) {
                routes.push(route);
            } else {
                disconnected_mission_members.push(mission_member.clone());
            }
        }

        let relay_members: BTreeSet<NodeId> = routes
            .iter()
            .flat_map(|route| route.relay_members.iter().cloned())
            .collect();
        let serves_mission_members: Vec<NodeId> = routes
            .iter()
            .filter(|route| !route.relay_members.is_empty())
            .map(|route| route.mission_member.clone())
            .collect();
        let relay_group = (!relay_members.is_empty()).then(|| RelayRoleGroup {
            group_id: "adaptive-relay-chain".to_owned(),
            members: relay_members.iter().cloned().collect(),
            serves_mission_members,
        });

        let current_relays: BTreeSet<NodeId> =
            request.current_relay_members.iter().cloned().collect();
        let (action, proposed_generation, nominated_probe_members, mut warnings) =
            if disconnected_mission_members.is_empty() {
                if relay_members == current_relays {
                    let action = if relay_members.is_empty() {
                        RelayRuntimeAction::MaintainDirect
                    } else {
                        RelayRuntimeAction::MaintainRelayChain
                    };
                    (
                        action,
                        request.current_generation,
                        Vec::new(),
                        vec![
                            if relay_members.is_empty() {
                                "Every required mission member has a fresh, bidirectional direct link that meets the mission health policy."
                            } else {
                                "The committed relay group still matches fresh, bidirectional paths that meet the mission health policy."
                            }
                            .to_owned(),
                        ],
                    )
                } else if relay_members.is_empty() {
                    let generation = request
                        .current_generation
                        .checked_add(1)
                        .ok_or(RelayRuntimeError::GenerationExhausted)?;
                    (
                        RelayRuntimeAction::ReleaseRelayChain,
                        generation,
                        Vec::new(),
                        vec![
                            "Every required mission member has a healthy direct link. Release the committed relay group back to the mission pool."
                                .to_owned(),
                        ],
                    )
                } else {
                    let generation = request
                        .current_generation
                        .checked_add(1)
                        .ok_or(RelayRuntimeError::GenerationExhausted)?;
                    (
                        RelayRuntimeAction::FormRelayChain,
                        generation,
                        Vec::new(),
                        vec![format!(
                            "{} aircraft are required in the currently observed relay chain for {} mission members.",
                            relay_members.len(),
                            required.len()
                        )],
                    )
                }
            } else if manual_override {
                (
                    RelayRuntimeAction::OperatorActionRequired,
                    request.current_generation,
                    Vec::new(),
                    vec![
                        "The manual relay-member override cannot currently connect every required mission member. AVIAN did not borrow any other aircraft."
                            .to_owned(),
                    ],
                )
            } else {
                (
                    RelayRuntimeAction::BeginRangeDiscovery,
                    request.current_generation,
                    ranked_probe_candidates(&candidate_by_id, &required, &relay_members),
                    vec![
                        "One or more members have no fresh, bidirectional path meeting the mission health policy. Begin a measured range-discovery workflow; do not assume an unobserved link from a free-space model."
                            .to_owned(),
                    ],
                )
            };

        if !disconnected_mission_members.is_empty() {
            warnings.push(format!(
                "No current healthy relay path exists for: {}.",
                disconnected_mission_members
                    .iter()
                    .map(NodeId::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        let committed_relays = match action {
            RelayRuntimeAction::FormRelayChain | RelayRuntimeAction::MaintainRelayChain => {
                relay_members.clone()
            }
            RelayRuntimeAction::ReleaseRelayChain | RelayRuntimeAction::MaintainDirect => {
                BTreeSet::new()
            }
            RelayRuntimeAction::BeginRangeDiscovery
            | RelayRuntimeAction::OperatorActionRequired => current_relays,
        };
        let mission_drones_remaining = candidate_by_id
            .values()
            .filter(|candidate| {
                candidate.candidate.available
                    && candidate.candidate.mission_eligible
                    && !committed_relays.contains(&candidate.candidate.node_id)
            })
            .count();

        Ok(InFlightRelayDecision {
            mission_id: request.mission_id,
            previous_generation: request.current_generation,
            proposed_generation,
            observed_at_ms: request.observed_at_ms,
            action,
            reserved_relay_count: committed_relays.len(),
            mission_drones_remaining,
            relay_group,
            routes,
            disconnected_mission_members,
            nominated_probe_members,
            warnings,
        })
    }
}

impl InFlightRelayDecision {
    /// Recomputes the deterministic decision from the shared snapshot. A
    /// companion uses this before accepting a peer's reconfiguration record,
    /// so publishing a decision does not create a coordinator role.
    pub fn verify_against(&self, request: &InFlightRelayRequest) -> Result<(), RelayRuntimeError> {
        let expected = InFlightRelayPlanner.decide(request)?;
        if *self == expected {
            Ok(())
        } else {
            Err(RelayRuntimeError::DecisionMismatch)
        }
    }
}

#[derive(Debug, Clone)]
struct GraphEdge {
    neighbor: NodeId,
    transport: TransportKind,
    health_score: f32,
}

fn validate_request(request: &InFlightRelayRequest) -> Result<(), RelayRuntimeError> {
    if request.current_generation == 0 {
        return Err(RelayRuntimeError::InvalidMissionGeneration);
    }
    if !(MIN_SUPPORTED_SWARM_SIZE..=MAX_SUPPORTED_SWARM_SIZE).contains(&request.candidates.len()) {
        return Err(RelayRuntimeError::UnsupportedSwarmSize(
            request.candidates.len(),
        ));
    }
    validate_endpoint(
        &request.anchor.node_id,
        request.anchor.position,
        request.anchor.agl_m,
    )?;
    validate_health_policy(request.health_policy)?;

    let mut candidate_ids = BTreeSet::new();
    for candidate in &request.candidates {
        let identity = &candidate.candidate.node_id;
        if !candidate_ids.insert(identity.clone()) {
            return Err(RelayRuntimeError::DuplicateCandidate(identity.clone()));
        }
        validate_endpoint(identity, candidate.position, candidate.agl_m)?;
        for score in [
            candidate.candidate.relay_suitability,
            candidate.candidate.mission_utility,
        ] {
            if !score.is_finite() || !(0.0..=1.0).contains(&score) {
                return Err(RelayRuntimeError::InvalidCandidateScore(identity.clone()));
            }
        }
    }

    if request.required_mission_members.is_empty() {
        return Err(RelayRuntimeError::MissingMissionMembers);
    }
    let mut required = BTreeSet::new();
    for member in &request.required_mission_members {
        if !required.insert(member.clone()) {
            return Err(RelayRuntimeError::DuplicateMissionMember(member.clone()));
        }
        let Some(candidate) = request
            .candidates
            .iter()
            .find(|candidate| candidate.candidate.node_id == *member)
        else {
            return Err(RelayRuntimeError::UnknownMissionMember(member.clone()));
        };
        if !candidate.candidate.available || !candidate.candidate.mission_eligible {
            return Err(RelayRuntimeError::UnavailableMissionMember(member.clone()));
        }
    }

    let mut current_relays = BTreeSet::new();
    for member in &request.current_relay_members {
        if !current_relays.insert(member.clone()) {
            return Err(RelayRuntimeError::DuplicateCurrentRelay(member.clone()));
        }
        if required.contains(member) {
            return Err(RelayRuntimeError::MissionMemberSelectedAsRelay(
                member.clone(),
            ));
        }
        let candidate = request
            .candidates
            .iter()
            .find(|candidate| candidate.candidate.node_id == *member)
            .ok_or_else(|| RelayRuntimeError::UnknownCurrentRelay(member.clone()))?;
        if !candidate.candidate.relay_eligible {
            return Err(RelayRuntimeError::IneligibleCurrentRelay(member.clone()));
        }
    }

    let known_nodes: BTreeSet<NodeId> = request
        .candidates
        .iter()
        .map(|candidate| candidate.candidate.node_id.clone())
        .chain(std::iter::once(request.anchor.node_id.clone()))
        .collect();
    for observation in &request.observations {
        if observation.first == observation.second
            || !known_nodes.contains(&observation.first)
            || !known_nodes.contains(&observation.second)
            || observation.observed_at_ms > request.observed_at_ms
            || observation.sample_window_ms == 0
            || !observation.metrics.is_valid()
            || !observation.geometry.is_valid()
            || observation
                .received_power_dbm
                .is_some_and(|value| !value.is_finite())
            || observation
                .link_margin_db
                .is_some_and(|value| !value.is_finite())
        {
            return Err(RelayRuntimeError::InvalidLinkObservation {
                first: observation.first.clone(),
                second: observation.second.clone(),
            });
        }
    }
    Ok(())
}

fn validate_endpoint(
    node_id: &NodeId,
    position: GeoPoint,
    agl_m: Option<f64>,
) -> Result<(), RelayRuntimeError> {
    position
        .validate()
        .map_err(|_| RelayRuntimeError::InvalidPosition(node_id.clone()))?;
    if agl_m.is_some_and(|value| !value.is_finite() || value < 0.0) {
        return Err(RelayRuntimeError::InvalidAgl(node_id.clone()));
    }
    Ok(())
}

fn validate_health_policy(policy: RelayHealthPolicy) -> Result<(), RelayRuntimeError> {
    let fractions = [
        policy.max_loss_ratio,
        policy.min_signal_quality,
        policy.min_stability,
        policy.min_fresnel_clearance_ratio,
    ];
    if policy.max_observation_age_ms == 0
        || !policy.max_latency_ms.is_finite()
        || policy.max_latency_ms <= 0.0
        || !policy.min_goodput_bps.is_finite()
        || policy.min_goodput_bps < 0.0
        || fractions
            .into_iter()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
        || policy
            .min_link_margin_db
            .is_some_and(|value| !value.is_finite())
    {
        return Err(RelayRuntimeError::InvalidHealthPolicy);
    }
    Ok(())
}

fn select_allowed_relays(
    allocation: &RuntimeRelayAllocationMode,
    candidates: &BTreeMap<NodeId, &LiveRelayCandidate>,
    required: &BTreeSet<NodeId>,
) -> Result<(BTreeSet<NodeId>, bool), RelayRuntimeError> {
    match allocation {
        RuntimeRelayAllocationMode::Automatic => Ok((
            candidates
                .values()
                .filter(|candidate| {
                    candidate.candidate.available
                        && candidate.candidate.relay_eligible
                        && !required.contains(&candidate.candidate.node_id)
                })
                .map(|candidate| candidate.candidate.node_id.clone())
                .collect(),
            false,
        )),
        RuntimeRelayAllocationMode::RelayMembers { members } => {
            let mut allowed = BTreeSet::new();
            for member in members {
                if !allowed.insert(member.clone()) {
                    return Err(RelayRuntimeError::DuplicateManualRelay(member.clone()));
                }
                if required.contains(member) {
                    return Err(RelayRuntimeError::MissionMemberSelectedAsRelay(
                        member.clone(),
                    ));
                }
                let candidate = candidates
                    .get(member)
                    .ok_or_else(|| RelayRuntimeError::UnknownManualRelay(member.clone()))?;
                if !candidate.candidate.available || !candidate.candidate.relay_eligible {
                    return Err(RelayRuntimeError::IneligibleManualRelay(member.clone()));
                }
            }
            Ok((allowed, true))
        }
    }
}

fn node_positions(
    anchor: &RelayAnchor,
    candidates: &BTreeMap<NodeId, &LiveRelayCandidate>,
) -> BTreeMap<NodeId, GeoPoint> {
    let mut positions: BTreeMap<NodeId, GeoPoint> = candidates
        .iter()
        .map(|(node_id, candidate)| (node_id.clone(), candidate.position))
        .collect();
    positions.insert(anchor.node_id.clone(), anchor.position);
    positions
}

fn build_observed_graph(
    request: &InFlightRelayRequest,
    positions: &BTreeMap<NodeId, GeoPoint>,
) -> Result<BTreeMap<NodeId, Vec<GraphEdge>>, RelayRuntimeError> {
    let mut latest = BTreeMap::new();
    for observation in &request.observations {
        let (first, second) = ordered_pair(&observation.first, &observation.second);
        let key = (first, second, observation.transport);
        let replace = latest
            .get(&key)
            .is_none_or(|existing: &RelayLinkObservation| {
                observation.observed_at_ms >= existing.observed_at_ms
            });
        if replace {
            latest.insert(key, observation.clone());
        }
    }

    let mut graph: BTreeMap<NodeId, Vec<GraphEdge>> = BTreeMap::new();
    for observation in latest.values() {
        if !observation_is_healthy(observation, request.observed_at_ms, request.health_policy) {
            continue;
        }
        let Some(first_position) = positions.get(&observation.first) else {
            continue;
        };
        let Some(second_position) = positions.get(&observation.second) else {
            continue;
        };
        let distance_m = first_position.distance_to(*second_position);
        if !distance_m.is_finite() {
            return Err(RelayRuntimeError::InvalidLinkObservation {
                first: observation.first.clone(),
                second: observation.second.clone(),
            });
        }
        let score = observation_health_score(observation, request.health_policy);
        graph
            .entry(observation.first.clone())
            .or_default()
            .push(GraphEdge {
                neighbor: observation.second.clone(),
                transport: observation.transport,
                health_score: score,
            });
        graph
            .entry(observation.second.clone())
            .or_default()
            .push(GraphEdge {
                neighbor: observation.first.clone(),
                transport: observation.transport,
                health_score: score,
            });
    }
    Ok(graph)
}

fn ordered_pair(first: &NodeId, second: &NodeId) -> (NodeId, NodeId) {
    if first <= second {
        (first.clone(), second.clone())
    } else {
        (second.clone(), first.clone())
    }
}

fn observation_is_healthy(
    observation: &RelayLinkObservation,
    now_ms: u64,
    policy: RelayHealthPolicy,
) -> bool {
    observation.available
        && observation.bidirectional
        && now_ms.saturating_sub(observation.observed_at_ms) <= policy.max_observation_age_ms
        && observation.metrics.latency_ms <= policy.max_latency_ms
        && observation.metrics.loss_ratio <= policy.max_loss_ratio
        && observation.metrics.goodput_bps >= policy.min_goodput_bps
        && observation.metrics.signal_quality >= policy.min_signal_quality
        && observation.metrics.stability >= policy.min_stability
        && observation.geometry.fresnel_clearance_ratio >= policy.min_fresnel_clearance_ratio
        && policy.min_link_margin_db.is_none_or(|minimum| {
            observation
                .link_margin_db
                .is_some_and(|margin| margin >= minimum)
        })
}

fn observation_health_score(observation: &RelayLinkObservation, policy: RelayHealthPolicy) -> f32 {
    let metrics = observation.metrics;
    let scores = [
        at_or_below(metrics.latency_ms, policy.max_latency_ms),
        at_or_below(metrics.loss_ratio, policy.max_loss_ratio),
        at_or_above(metrics.goodput_bps, policy.min_goodput_bps),
        at_or_above(metrics.signal_quality, policy.min_signal_quality),
        at_or_above(metrics.stability, policy.min_stability),
        at_or_above(
            observation.geometry.fresnel_clearance_ratio,
            policy.min_fresnel_clearance_ratio,
        ),
    ];
    let margin_score = policy.min_link_margin_db.map(|minimum| {
        at_or_above(
            observation.link_margin_db.unwrap_or(f32::NEG_INFINITY),
            minimum,
        )
    });
    let total = scores.iter().copied().sum::<f32>() + margin_score.unwrap_or(0.0);
    total / (scores.len() + usize::from(margin_score.is_some())) as f32
}

fn at_or_above(value: f32, minimum: f32) -> f32 {
    if minimum <= 0.0 {
        1.0
    } else {
        (value / minimum).clamp(0.0, 1.0)
    }
}

fn at_or_below(value: f32, maximum: f32) -> f32 {
    if maximum <= 0.0 {
        f32::from(value <= 0.0)
    } else {
        (1.0 - value / maximum).clamp(0.0, 1.0)
    }
}

fn find_observed_route(
    anchor: &NodeId,
    target: &NodeId,
    allowed_relays: &BTreeSet<NodeId>,
    candidates: &BTreeMap<NodeId, &LiveRelayCandidate>,
    positions: &BTreeMap<NodeId, GeoPoint>,
    graph: &BTreeMap<NodeId, Vec<GraphEdge>>,
) -> Option<RelayChainRoute> {
    let mut queue = VecDeque::from([anchor.clone()]);
    let mut visited = BTreeSet::from([anchor.clone()]);
    let mut previous: BTreeMap<NodeId, (NodeId, GraphEdge)> = BTreeMap::new();

    while let Some(current) = queue.pop_front() {
        let mut edges = graph.get(&current)?.clone();
        edges.retain(|edge| {
            edge.neighbor == *target
                || edge.neighbor == *anchor
                || allowed_relays.contains(&edge.neighbor)
        });
        edges.sort_by(|left, right| rank_edge(left, right, candidates));
        for edge in edges {
            if !visited.insert(edge.neighbor.clone()) {
                continue;
            }
            previous.insert(edge.neighbor.clone(), (current.clone(), edge.clone()));
            if edge.neighbor == *target {
                return Some(build_route(anchor, target, positions, &previous));
            }
            queue.push_back(edge.neighbor);
        }
    }
    None
}

fn rank_edge(
    left: &GraphEdge,
    right: &GraphEdge,
    candidates: &BTreeMap<NodeId, &LiveRelayCandidate>,
) -> std::cmp::Ordering {
    right
        .health_score
        .total_cmp(&left.health_score)
        .then_with(|| {
            let left_candidate = candidates.get(&left.neighbor);
            let right_candidate = candidates.get(&right.neighbor);
            match (left_candidate, right_candidate) {
                (Some(left), Some(right)) => right
                    .candidate
                    .relay_suitability
                    .total_cmp(&left.candidate.relay_suitability)
                    .then_with(|| {
                        left.candidate
                            .mission_utility
                            .total_cmp(&right.candidate.mission_utility)
                    }),
                _ => std::cmp::Ordering::Equal,
            }
        })
        .then_with(|| left.neighbor.cmp(&right.neighbor))
        .then_with(|| left.transport.cmp(&right.transport))
}

fn build_route(
    anchor: &NodeId,
    target: &NodeId,
    positions: &BTreeMap<NodeId, GeoPoint>,
    previous: &BTreeMap<NodeId, (NodeId, GraphEdge)>,
) -> RelayChainRoute {
    let mut nodes = vec![target.clone()];
    let mut edges = Vec::new();
    let mut current = target.clone();
    while current != *anchor {
        let (parent, edge) = previous
            .get(&current)
            .expect("route reconstruction has a predecessor");
        edges.push((parent.clone(), current.clone(), edge.clone()));
        nodes.push(parent.clone());
        current = parent.clone();
    }
    nodes.reverse();
    edges.reverse();

    let hops = edges
        .into_iter()
        .map(|(from, to, edge)| RelayChainHop {
            separation_m: positions[&from].distance_to(positions[&to]),
            from,
            to,
            transport: edge.transport,
            health_score: edge.health_score,
        })
        .collect::<Vec<_>>();
    let relay_members = nodes
        .iter()
        .skip(1)
        .take(nodes.len().saturating_sub(2))
        .cloned()
        .collect();
    let minimum_health_score = hops
        .iter()
        .map(|hop| hop.health_score)
        .min_by(|left, right| left.total_cmp(right))
        .unwrap_or(0.0);
    RelayChainRoute {
        mission_member: target.clone(),
        relay_members,
        hops,
        minimum_health_score,
    }
}

fn ranked_probe_candidates(
    candidates: &BTreeMap<NodeId, &LiveRelayCandidate>,
    required: &BTreeSet<NodeId>,
    current_relays: &BTreeSet<NodeId>,
) -> Vec<NodeId> {
    let mut candidates: Vec<&LiveRelayCandidate> = candidates
        .values()
        .copied()
        .filter(|candidate| {
            candidate.candidate.available
                && candidate.candidate.relay_eligible
                && !required.contains(&candidate.candidate.node_id)
                && !current_relays.contains(&candidate.candidate.node_id)
        })
        .collect();
    candidates.sort_by(|left, right| {
        right
            .candidate
            .relay_suitability
            .total_cmp(&left.candidate.relay_suitability)
            .then_with(|| {
                left.candidate
                    .mission_utility
                    .total_cmp(&right.candidate.mission_utility)
            })
            .then_with(|| left.candidate.node_id.cmp(&right.candidate.node_id))
    });
    candidates
        .into_iter()
        .map(|candidate| candidate.candidate.node_id.clone())
        .collect()
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum RelayRuntimeError {
    #[error("in-flight relay planning requires a positive mission generation")]
    InvalidMissionGeneration,
    #[error("in-flight relay planning supports formations of 5-200 aircraft, got {0}")]
    UnsupportedSwarmSize(usize),
    #[error("duplicate live relay candidate {0}")]
    DuplicateCandidate(NodeId),
    #[error("candidate {0} has an invalid normalized suitability score")]
    InvalidCandidateScore(NodeId),
    #[error("node {0} has an invalid MSL position")]
    InvalidPosition(NodeId),
    #[error("node {0} has an invalid AGL measurement")]
    InvalidAgl(NodeId),
    #[error("the in-flight health policy is invalid")]
    InvalidHealthPolicy,
    #[error("at least one required mission member is needed for in-flight relay planning")]
    MissingMissionMembers,
    #[error("required mission member {0} is duplicated")]
    DuplicateMissionMember(NodeId),
    #[error("required mission member {0} is not in the live inventory")]
    UnknownMissionMember(NodeId),
    #[error("required mission member {0} is unavailable or mission-ineligible")]
    UnavailableMissionMember(NodeId),
    #[error("link observation from {first} to {second} is invalid")]
    InvalidLinkObservation { first: NodeId, second: NodeId },
    #[error("manual relay member {0} is not in the live inventory")]
    UnknownManualRelay(NodeId),
    #[error("manual relay member {0} is unavailable or relay-ineligible")]
    IneligibleManualRelay(NodeId),
    #[error("manual relay member {0} is duplicated")]
    DuplicateManualRelay(NodeId),
    #[error("committed relay member {0} is not in the live inventory")]
    UnknownCurrentRelay(NodeId),
    #[error("committed relay member {0} is not relay-eligible")]
    IneligibleCurrentRelay(NodeId),
    #[error("committed relay member {0} is duplicated")]
    DuplicateCurrentRelay(NodeId),
    #[error("required mission member {0} cannot also be a manual relay")]
    MissionMemberSelectedAsRelay(NodeId),
    #[error("mission generation cannot be incremented further")]
    GenerationExhausted,
    #[error("in-flight relay decision does not match the shared live snapshot")]
    DecisionMismatch,
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum RelayRuntimeConfigError {
    #[error("maximum relay-position age must be greater than zero")]
    InvalidPositionAge,
    #[error("no telemetry position is available for configured candidate {0}")]
    MissingCandidateTelemetry(NodeId),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(node_id: &str, latitude_deg: f64) -> LiveRelayCandidate {
        LiveRelayCandidate {
            candidate: RelayCandidate {
                node_id: node_id.into(),
                available: true,
                relay_eligible: true,
                mission_eligible: true,
                relay_suitability: 0.8,
                mission_utility: 0.5,
            },
            position: GeoPoint {
                latitude_deg,
                longitude_deg: 0.0,
                msl_m: 100.0,
            },
            agl_m: Some(50.0),
        }
    }

    fn healthy_observation(first: &str, second: &str) -> RelayLinkObservation {
        RelayLinkObservation {
            first: first.into(),
            second: second.into(),
            transport: TransportKind::Silvus,
            observed_at_ms: 10_000,
            sample_window_ms: 1_000,
            bidirectional: true,
            available: true,
            metrics: LinkMetrics {
                latency_ms: 20.0,
                loss_ratio: 0.01,
                goodput_bps: 2_000_000.0,
                signal_quality: 0.9,
                stability: 0.9,
                energy_cost: 0.3,
            },
            geometry: LinkGeometry {
                distance_m: 100.0,
                line_of_sight: true,
                fresnel_clearance_ratio: 0.9,
            },
            received_power_dbm: Some(-65.0),
            link_margin_db: Some(25.0),
        }
    }

    fn request(allocation: RuntimeRelayAllocationMode) -> InFlightRelayRequest {
        InFlightRelayRequest {
            mission_id: Uuid::from_u128(7),
            current_generation: 4,
            observed_at_ms: 10_100,
            anchor: RelayAnchor {
                node_id: "ground".into(),
                position: GeoPoint {
                    latitude_deg: 0.0,
                    longitude_deg: 0.0,
                    msl_m: 100.0,
                },
                agl_m: Some(0.0),
            },
            required_mission_members: vec!["search-1".into(), "search-2".into()],
            current_relay_members: Vec::new(),
            candidates: vec![
                candidate("relay-a", 0.001),
                candidate("relay-b", 0.002),
                candidate("reserve", 0.003),
                candidate("search-1", 0.004),
                candidate("search-2", 0.005),
            ],
            observations: vec![
                healthy_observation("ground", "relay-a"),
                healthy_observation("relay-a", "relay-b"),
                healthy_observation("relay-b", "search-1"),
                healthy_observation("relay-b", "search-2"),
            ],
            health_policy: RelayHealthPolicy {
                max_observation_age_ms: 2_000,
                max_latency_ms: 100.0,
                max_loss_ratio: 0.05,
                min_goodput_bps: 500_000.0,
                min_signal_quality: 0.7,
                min_stability: 0.7,
                min_fresnel_clearance_ratio: 0.6,
                min_link_margin_db: Some(10.0),
            },
            allocation,
        }
    }

    fn configuration(request: &InFlightRelayRequest) -> RelayRuntimeConfiguration {
        RelayRuntimeConfiguration {
            mission_id: request.mission_id,
            generation: request.current_generation,
            current_relay_members: request.current_relay_members.clone(),
            anchor: request.anchor.clone(),
            required_mission_members: request.required_mission_members.clone(),
            candidates: request
                .candidates
                .iter()
                .map(|candidate| candidate.candidate.clone())
                .collect(),
            health_policy: request.health_policy,
            allocation: request.allocation.clone(),
            max_position_age_ms: 2_000,
        }
    }

    fn telemetry(candidate: &LiveRelayCandidate, timestamp_ms: u64) -> Telemetry {
        Telemetry {
            source: candidate.candidate.node_id.clone(),
            timestamp_ms,
            latitude_deg: candidate.position.latitude_deg,
            longitude_deg: candidate.position.longitude_deg,
            altitude: crate::Altitude::with_optional_agl(
                candidate.position.msl_m,
                candidate.agl_m,
                0.0,
            )
            .unwrap(),
            velocity_ned_mps: [0.0; 3],
            attitude_rpy_deg: [0.0; 3],
            battery_remaining: Some(0.9),
            control_link_quality: None,
            armed: true,
            landed: Some(false),
            failsafe: false,
        }
    }

    #[test]
    fn live_observations_form_one_grouped_relay_chain_for_multiple_mission_members() {
        let decision = InFlightRelayPlanner
            .decide(&request(RuntimeRelayAllocationMode::Automatic))
            .unwrap();

        assert_eq!(decision.action, RelayRuntimeAction::FormRelayChain);
        assert_eq!(decision.proposed_generation, 5);
        assert_eq!(decision.reserved_relay_count, 2);
        assert_eq!(decision.mission_drones_remaining, 3);
        assert_eq!(
            decision.relay_group.unwrap().members,
            vec![NodeId::from("relay-a"), NodeId::from("relay-b")]
        );
        assert_eq!(decision.routes.len(), 2);
        assert!(decision
            .routes
            .iter()
            .all(|route| route.relay_members
                == vec![NodeId::from("relay-a"), NodeId::from("relay-b")]));
    }

    #[test]
    fn durable_runtime_configuration_rebuilds_a_live_request_from_mesh_state() {
        let request = request(RuntimeRelayAllocationMode::Automatic);
        let configuration = configuration(&request);
        let snapshot = RelayRuntimeSnapshot {
            observed_at_ms: request.observed_at_ms,
            current_generation: request.current_generation,
            current_relay_members: Vec::new(),
            telemetry: request
                .candidates
                .iter()
                .map(|candidate| telemetry(candidate, 10_000))
                .collect(),
            observations: request.observations.clone(),
        };

        let rebuilt = configuration.build_request(&snapshot).unwrap();
        let decision = InFlightRelayPlanner.decide(&rebuilt).unwrap();

        assert_eq!(rebuilt.candidates.len(), 5);
        assert_eq!(decision.action, RelayRuntimeAction::FormRelayChain);
    }

    #[test]
    fn stale_candidate_position_removes_only_that_candidate_from_relay_selection() {
        let request = request(RuntimeRelayAllocationMode::Automatic);
        let configuration = configuration(&request);
        let telemetry = request
            .candidates
            .iter()
            .map(|candidate| {
                let timestamp = if candidate.candidate.node_id == NodeId::from("reserve") {
                    1
                } else {
                    10_000
                };
                telemetry(candidate, timestamp)
            })
            .collect();
        let snapshot = RelayRuntimeSnapshot {
            observed_at_ms: request.observed_at_ms,
            current_generation: request.current_generation,
            current_relay_members: Vec::new(),
            telemetry,
            observations: request.observations.clone(),
        };

        let rebuilt = configuration.build_request(&snapshot).unwrap();
        let reserve = rebuilt
            .candidates
            .iter()
            .find(|candidate| candidate.candidate.node_id == NodeId::from("reserve"))
            .unwrap();

        assert!(!reserve.candidate.available);
        assert_eq!(
            InFlightRelayPlanner.decide(&rebuilt).unwrap().action,
            RelayRuntimeAction::FormRelayChain
        );
    }

    #[test]
    fn configuration_rejects_missing_candidate_telemetry() {
        let request = request(RuntimeRelayAllocationMode::Automatic);
        let configuration = configuration(&request);
        let snapshot = RelayRuntimeSnapshot {
            observed_at_ms: request.observed_at_ms,
            current_generation: request.current_generation,
            current_relay_members: Vec::new(),
            telemetry: request
                .candidates
                .iter()
                .filter(|candidate| candidate.candidate.node_id != NodeId::from("reserve"))
                .map(|candidate| telemetry(candidate, 10_000))
                .collect(),
            observations: request.observations.clone(),
        };

        assert_eq!(
            configuration.build_request(&snapshot),
            Err(RelayRuntimeConfigError::MissingCandidateTelemetry(
                NodeId::from("reserve")
            ))
        );
    }

    #[test]
    fn stale_links_trigger_measured_discovery_instead_of_assuming_a_chain() {
        let mut request = request(RuntimeRelayAllocationMode::Automatic);
        request.observations[1].observed_at_ms = 1;

        let decision = InFlightRelayPlanner.decide(&request).unwrap();

        assert_eq!(decision.action, RelayRuntimeAction::BeginRangeDiscovery);
        assert_eq!(decision.proposed_generation, 4);
        assert_eq!(
            decision.disconnected_mission_members,
            vec![NodeId::from("search-1"), NodeId::from("search-2")]
        );
        assert_eq!(
            decision.nominated_probe_members,
            vec![
                NodeId::from("relay-a"),
                NodeId::from("relay-b"),
                NodeId::from("reserve"),
            ]
        );
    }

    #[test]
    fn manual_relay_members_are_never_silently_replaced() {
        let decision = InFlightRelayPlanner
            .decide(&request(RuntimeRelayAllocationMode::RelayMembers {
                members: vec!["relay-a".into()],
            }))
            .unwrap();

        assert_eq!(decision.action, RelayRuntimeAction::OperatorActionRequired);
        assert!(decision.nominated_probe_members.is_empty());
        assert_eq!(
            decision.disconnected_mission_members,
            vec![NodeId::from("search-1"), NodeId::from("search-2")]
        );
    }

    #[test]
    fn healthy_direct_links_do_not_reserve_a_chain() {
        let mut request = request(RuntimeRelayAllocationMode::Automatic);
        request.observations = vec![
            healthy_observation("ground", "search-1"),
            healthy_observation("ground", "search-2"),
        ];

        let decision = InFlightRelayPlanner.decide(&request).unwrap();

        assert_eq!(decision.action, RelayRuntimeAction::MaintainDirect);
        assert_eq!(decision.proposed_generation, request.current_generation);
        assert_eq!(decision.reserved_relay_count, 0);
        assert_eq!(decision.mission_drones_remaining, 5);
        assert!(decision.relay_group.is_none());
        assert!(decision
            .routes
            .iter()
            .all(|route| route.relay_members.is_empty()));
    }

    #[test]
    fn unchanged_observed_chain_does_not_create_a_new_generation() {
        let first_request = request(RuntimeRelayAllocationMode::Automatic);
        let first = InFlightRelayPlanner.decide(&first_request).unwrap();
        let mut next_request = first_request;
        next_request.current_generation = first.proposed_generation;
        next_request.current_relay_members = first.relay_group.unwrap().members;

        let decision = InFlightRelayPlanner.decide(&next_request).unwrap();

        assert_eq!(decision.action, RelayRuntimeAction::MaintainRelayChain);
        assert_eq!(
            decision.proposed_generation,
            next_request.current_generation
        );
        assert_eq!(decision.reserved_relay_count, 2);
    }

    #[test]
    fn direct_recovery_releases_the_committed_relay_group() {
        let mut request = request(RuntimeRelayAllocationMode::Automatic);
        request.current_relay_members = vec!["relay-a".into(), "relay-b".into()];
        request.observations = vec![
            healthy_observation("ground", "search-1"),
            healthy_observation("ground", "search-2"),
        ];

        let decision = InFlightRelayPlanner.decide(&request).unwrap();

        assert_eq!(decision.action, RelayRuntimeAction::ReleaseRelayChain);
        assert_eq!(decision.proposed_generation, request.current_generation + 1);
        assert_eq!(decision.reserved_relay_count, 0);
        assert_eq!(decision.mission_drones_remaining, 5);
    }

    #[test]
    fn live_failure_reforms_the_group_from_current_observations() {
        let mut request = request(RuntimeRelayAllocationMode::Automatic);
        request.candidates[0].candidate.available = false;
        request.observations[0].available = false;
        request
            .observations
            .push(healthy_observation("ground", "reserve"));
        request
            .observations
            .push(healthy_observation("reserve", "relay-b"));

        let decision = InFlightRelayPlanner.decide(&request).unwrap();

        assert_eq!(decision.action, RelayRuntimeAction::FormRelayChain);
        assert_eq!(
            decision.routes[0].relay_members,
            vec![NodeId::from("reserve"), NodeId::from("relay-b")]
        );
        assert_eq!(
            decision.relay_group.unwrap().members,
            vec![NodeId::from("relay-b"), NodeId::from("reserve")]
        );
    }

    #[test]
    fn every_companion_can_verify_the_same_reconfiguration_without_a_leader() {
        let request = request(RuntimeRelayAllocationMode::Automatic);
        let decision = InFlightRelayPlanner.decide(&request).unwrap();

        assert_eq!(decision.verify_against(&request), Ok(()));

        let mut altered = decision;
        altered.proposed_generation += 1;
        assert_eq!(
            altered.verify_against(&request),
            Err(RelayRuntimeError::DecisionMismatch)
        );
    }
}

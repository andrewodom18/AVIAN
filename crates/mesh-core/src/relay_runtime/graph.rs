//! Healthy observed graph construction and deterministic route selection.
use super::*;
use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub(super) struct GraphEdge {
    pub(super) neighbor: NodeId,
    pub(super) transport: TransportKind,
    pub(super) health_score: f32,
}

pub(super) fn build_observed_graph(
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

pub(super) fn find_observed_route(
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

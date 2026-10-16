//! Distinct receive-ready standby assignment.
use super::graph::GraphEdge;
use super::*;

/// Derives one distinct Bluetooth-linked standby for every active route relay
/// in maximum coverage. The standby must also have a healthy non-Bluetooth
/// radio edge to every neighbor its active peer protects, proving that it can
/// receive the traffic and take over the same hop rather than merely being
/// nearby on Bluetooth.
pub(super) fn select_broadcast_pairs(
    coverage: RelayCoverage,
    active_relays: &BTreeSet<NodeId>,
    routes: &[RelayChainRoute],
    allowed_relays: &BTreeSet<NodeId>,
    candidates: &BTreeMap<NodeId, &LiveRelayCandidate>,
    graph: &BTreeMap<NodeId, Vec<GraphEdge>>,
) -> (Vec<RelayBroadcastPair>, Vec<NodeId>) {
    if coverage == RelayCoverage::Minimum {
        return (Vec::new(), Vec::new());
    }

    let mut protected_neighbors: BTreeMap<NodeId, BTreeSet<NodeId>> = BTreeMap::new();
    for route in routes {
        for hop in &route.hops {
            if active_relays.contains(&hop.from) {
                protected_neighbors
                    .entry(hop.from.clone())
                    .or_default()
                    .insert(hop.to.clone());
            }
            if active_relays.contains(&hop.to) {
                protected_neighbors
                    .entry(hop.to.clone())
                    .or_default()
                    .insert(hop.from.clone());
            }
        }
    }

    let mut eligible_by_active = BTreeMap::new();
    for active in active_relays {
        let neighbors = protected_neighbors.get(active).cloned().unwrap_or_default();
        let mut eligible_standbys: Vec<&LiveRelayCandidate> = allowed_relays
            .iter()
            .filter(|node_id| !active_relays.contains(*node_id))
            .filter_map(|node_id| candidates.get(node_id).copied())
            .filter(|candidate| {
                candidate.candidate.available
                    && has_transport_edge(
                        graph,
                        active,
                        &candidate.candidate.node_id,
                        TransportKind::Bluetooth,
                    )
                    && neighbors.iter().all(|neighbor| {
                        has_non_bluetooth_edge(graph, &candidate.candidate.node_id, neighbor)
                    })
            })
            .collect();
        eligible_standbys.sort_by(|left, right| {
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

        eligible_by_active.insert(
            active.clone(),
            eligible_standbys
                .into_iter()
                .map(|candidate| candidate.candidate.node_id.clone())
                .collect(),
        );
    }

    // Augmenting paths maximize the number of protected active relays. An
    // earlier flexible relay can move to its next choice when a later relay
    // has only the shared standby; candidate ranking and node order keep the
    // result identical on every companion.
    let assignments = match_standbys(&eligible_by_active);
    let mut pairs = Vec::new();
    let mut unpaired = Vec::new();
    for active in active_relays {
        if let Some(standby) = assignments.get(active) {
            pairs.push(RelayBroadcastPair {
                active_broadcaster: active.clone(),
                standby_receiver: standby.clone(),
                peer_coordination: RelayPeerCoordination::Bluetooth,
                protected_neighbors: protected_neighbors
                    .get(active)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .collect(),
            });
        } else {
            unpaired.push(active.clone());
        }
    }
    (pairs, unpaired)
}

fn match_standbys(eligible: &BTreeMap<NodeId, Vec<NodeId>>) -> BTreeMap<NodeId, NodeId> {
    let mut standby_owners = BTreeMap::new();
    for active in eligible.keys() {
        assign_standby(active, eligible, &mut standby_owners, &mut BTreeSet::new());
    }
    standby_owners
        .into_iter()
        .map(|(standby, active)| (active, standby))
        .collect()
}

fn assign_standby(
    active: &NodeId,
    eligible: &BTreeMap<NodeId, Vec<NodeId>>,
    standby_owners: &mut BTreeMap<NodeId, NodeId>,
    visited: &mut BTreeSet<NodeId>,
) -> bool {
    for standby in &eligible[active] {
        if !visited.insert(standby.clone()) {
            continue;
        }
        let previous_owner = standby_owners.get(standby).cloned();
        if previous_owner
            .is_none_or(|owner| assign_standby(&owner, eligible, standby_owners, visited))
        {
            standby_owners.insert(standby.clone(), active.clone());
            return true;
        }
    }
    false
}

fn has_transport_edge(
    graph: &BTreeMap<NodeId, Vec<GraphEdge>>,
    first: &NodeId,
    second: &NodeId,
    transport: TransportKind,
) -> bool {
    graph.get(first).is_some_and(|edges| {
        edges
            .iter()
            .any(|edge| edge.neighbor == *second && edge.transport == transport)
    })
}

fn has_non_bluetooth_edge(
    graph: &BTreeMap<NodeId, Vec<GraphEdge>>,
    first: &NodeId,
    second: &NodeId,
) -> bool {
    graph.get(first).is_some_and(|edges| {
        edges
            .iter()
            .any(|edge| edge.neighbor == *second && edge.transport != TransportKind::Bluetooth)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contested_standby_moves_flexible_relay_to_its_alternative() {
        let eligible = BTreeMap::from([
            (
                NodeId::from("active-a"),
                vec![NodeId::from("standby-x"), NodeId::from("standby-y")],
            ),
            (NodeId::from("active-b"), vec![NodeId::from("standby-x")]),
        ]);
        let assignments = match_standbys(&eligible);
        assert_eq!(
            assignments.get(&NodeId::from("active-a")),
            Some(&NodeId::from("standby-y"))
        );
        assert_eq!(
            assignments.get(&NodeId::from("active-b")),
            Some(&NodeId::from("standby-x"))
        );
    }

    #[test]
    fn ready_standby_selection_finds_complete_pairing_when_preferences_overlap() {
        let active_a = NodeId::from("active-a");
        let active_b = NodeId::from("active-b");
        let target = NodeId::from("target");
        let make_candidate = |name: &str| LiveRelayCandidate {
            candidate: RelayCandidate {
                node_id: name.into(),
                available: true,
                relay_eligible: true,
                mission_eligible: true,
                relay_suitability: 0.8,
                mission_utility: 0.5,
            },
            position: GeoPoint {
                latitude_deg: 0.0,
                longitude_deg: 0.0,
                msl_m: 100.0,
            },
            agl_m: Some(50.0),
        };
        let candidates = [make_candidate("standby-x"), make_candidate("standby-y")];
        let candidate_map = candidates
            .iter()
            .map(|candidate| (candidate.candidate.node_id.clone(), candidate))
            .collect();
        let active = BTreeSet::from([active_a.clone(), active_b.clone()]);
        let allowed = candidates
            .iter()
            .map(|candidate| candidate.candidate.node_id.clone())
            .collect();
        let routes = vec![RelayChainRoute {
            mission_member: target.clone(),
            relay_members: active.iter().cloned().collect(),
            hops: active
                .iter()
                .map(|node| RelayChainHop {
                    from: node.clone(),
                    to: target.clone(),
                    separation_m: 1.0,
                    transport: TransportKind::Silvus,
                    health_score: 1.0,
                })
                .collect(),
            minimum_health_score: 1.0,
        }];
        let mut graph = BTreeMap::<NodeId, Vec<GraphEdge>>::new();
        for (first, second, transport) in [
            ("active-a", "standby-x", TransportKind::Bluetooth),
            ("active-a", "standby-y", TransportKind::Bluetooth),
            ("active-b", "standby-x", TransportKind::Bluetooth),
            ("standby-x", "target", TransportKind::Silvus),
            ("standby-y", "target", TransportKind::Silvus),
        ] {
            for (from, to) in [(first, second), (second, first)] {
                graph.entry(from.into()).or_default().push(GraphEdge {
                    neighbor: to.into(),
                    transport,
                    health_score: 1.0,
                });
            }
        }
        let (pairs, unpaired) = select_broadcast_pairs(
            RelayCoverage::Maximum,
            &active,
            &routes,
            &allowed,
            &candidate_map,
            &graph,
        );
        assert!(unpaired.is_empty());
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].standby_receiver, NodeId::from("standby-y"));
        assert_eq!(pairs[1].standby_receiver, NodeId::from("standby-x"));
        assert!(pairs
            .iter()
            .all(|pair| pair.protected_neighbors == vec![target.clone()]));
    }

    #[test]
    fn incomplete_matching_never_reuses_a_standby() {
        let eligible = BTreeMap::from([
            (NodeId::from("active-a"), vec![NodeId::from("standby-x")]),
            (NodeId::from("active-b"), vec![NodeId::from("standby-x")]),
        ]);
        let assignments = match_standbys(&eligible);
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments.values().collect::<BTreeSet<_>>().len(), 1);
    }
}

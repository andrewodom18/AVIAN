//! Traffic-aware route planning. This crate does not forward packets or configure interfaces.
//!
//! Observation and request timestamps must use the same collector-controlled timebase.
//! Call the planner when topology or policy changes, rather than for each packet.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrafficClass {
    Control,
    Telemetry,
    Video,
    Bulk,
}

impl TrafficClass {
    fn prefers_capacity(self) -> bool {
        matches!(self, Self::Video | Self::Bulk)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkNode {
    pub node_id: String,
    pub relay_allowed: bool,
}

/// A directed physical connection. Parallel connections have different link identifiers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkObservation {
    pub link_id: String,
    pub source: String,
    pub destination: String,
    pub interface_name: String,
    pub available: bool,
    pub metered: bool,
    pub observed_at_ms: u64,
    pub round_trip_time_ms: Option<f64>,
    pub loss_fraction: Option<f64>,
    pub jitter_ms: Option<f64>,
    pub available_capacity_bps: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSnapshot {
    pub nodes: Vec<NetworkNode>,
    pub links: Vec<LinkObservation>,
}

impl NetworkSnapshot {
    pub fn validate(&self) -> Result<(), RoutingError> {
        NetworkIndex::new(self).map(|_| ())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RoutingPolicy {
    pub maximum_observation_age_ms: u64,
    pub allow_metered_video: bool,
    pub allow_metered_bulk: bool,
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self {
            maximum_observation_age_ms: 3_000,
            allow_metered_video: false,
            allow_metered_bulk: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Route {
    pub traffic_class: TrafficClass,
    pub node_ids: Vec<String>,
    pub link_ids: Vec<String>,
    pub estimated_round_trip_time_ms: f64,
    pub estimated_loss_fraction: f64,
    pub estimated_jitter_ms: Option<f64>,
    /// The narrowest measured link. Shared radio airtime can reduce actual path capacity.
    pub bottleneck_capacity_bps: Option<u64>,
    pub uses_metered_link: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingError {
    #[error("invalid network snapshot: {0}")]
    InvalidSnapshot(String),
    #[error("unknown node: {0}")]
    UnknownNode(String),
    #[error("no eligible path from {source_node} to {destination_node}")]
    NoRoute {
        source_node: String,
        destination_node: String,
    },
    #[error("active flow capacity reached ({0})")]
    FlowCapacityReached(usize),
    #[error("flow identifier is already registered for a different source, destination, or class")]
    FlowConflict,
    #[error("invalid flow policy: {0}")]
    InvalidFlowPolicy(String),
    #[error("invalid flow request: {0}")]
    InvalidFlowRequest(String),
}

/// Cost is additive, so a minimum-cost search can safely discard inferior prefixes.
#[derive(Debug, Clone)]
struct PathCost {
    loss_cost: f64,
    round_trip_time_ms: f64,
    hop_count: usize,
    last_link_id: String,
}

impl PartialEq for PathCost {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for PathCost {}
impl PartialOrd for PathCost {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for PathCost {
    fn cmp(&self, other: &Self) -> Ordering {
        self.loss_cost
            .total_cmp(&other.loss_cost)
            .then_with(|| self.round_trip_time_ms.total_cmp(&other.round_trip_time_ms))
            .then_with(|| self.hop_count.cmp(&other.hop_count))
            .then_with(|| self.last_link_id.cmp(&other.last_link_id))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchEntry {
    node_id: String,
    cost: PathCost,
}
impl PartialOrd for SearchEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for SearchEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap is a max-heap; reverse cost to visit the best prefix first.
        other
            .cost
            .cmp(&self.cost)
            .then_with(|| other.node_id.cmp(&self.node_id))
    }
}

struct NetworkIndex<'a> {
    nodes: BTreeMap<&'a str, &'a NetworkNode>,
    links: BTreeMap<&'a str, &'a LinkObservation>,
    outgoing: BTreeMap<&'a str, Vec<&'a LinkObservation>>,
}

impl<'a> NetworkIndex<'a> {
    fn new(snapshot: &'a NetworkSnapshot) -> Result<Self, RoutingError> {
        let mut index = Self {
            nodes: BTreeMap::new(),
            links: BTreeMap::new(),
            outgoing: BTreeMap::new(),
        };
        for node in &snapshot.nodes {
            if node.node_id.is_empty() || index.nodes.insert(&node.node_id, node).is_some() {
                return Err(RoutingError::InvalidSnapshot(
                    "empty or duplicate node identifier".into(),
                ));
            }
        }
        for link in &snapshot.links {
            if link.link_id.is_empty() || index.links.insert(&link.link_id, link).is_some() {
                return Err(RoutingError::InvalidSnapshot(
                    "empty or duplicate link identifier".into(),
                ));
            }
            if !index.nodes.contains_key(link.source.as_str())
                || !index.nodes.contains_key(link.destination.as_str())
            {
                return Err(RoutingError::InvalidSnapshot(format!(
                    "link {} references an unknown node",
                    link.link_id
                )));
            }
            if link.interface_name.is_empty() {
                return Err(RoutingError::InvalidSnapshot(format!(
                    "link {} has no physical interface name",
                    link.link_id
                )));
            }
            if link
                .round_trip_time_ms
                .is_some_and(|value| !value.is_finite() || value < 0.0)
                || link
                    .jitter_ms
                    .is_some_and(|value| !value.is_finite() || value < 0.0)
                || link
                    .loss_fraction
                    .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
            {
                return Err(RoutingError::InvalidSnapshot(format!(
                    "link {} has invalid measurements",
                    link.link_id
                )));
            }
            index.outgoing.entry(&link.source).or_default().push(link);
        }
        for links in index.outgoing.values_mut() {
            links.sort_by(|first, second| first.link_id.cmp(&second.link_id));
        }
        Ok(index)
    }
}

#[derive(Debug, Clone, Default)]
pub struct RoutePlanner {
    pub policy: RoutingPolicy,
}

impl RoutePlanner {
    pub fn new(policy: RoutingPolicy) -> Self {
        Self { policy }
    }

    pub fn plan(
        &self,
        snapshot: &NetworkSnapshot,
        source: &str,
        destination: &str,
        traffic_class: TrafficClass,
        now_ms: u64,
    ) -> Result<Route, RoutingError> {
        let index = NetworkIndex::new(snapshot)?;
        self.check_endpoints(&index, source, destination)?;
        if source == destination {
            return self.describe_path(&index, source, destination, &[], traffic_class, now_ms);
        }
        // Optimizing bottleneck and reliability in one single-label search is incorrect:
        // a narrow downstream link can erase an earlier prefix's capacity advantage.
        let minimum_capacity = if traffic_class.prefers_capacity() {
            Some(
                self.widest_capacity(&index, source, destination, traffic_class, now_ms)
                    .ok_or_else(|| Self::no_route(source, destination))?,
            )
        } else {
            None
        };
        let link_ids = self
            .reliable_path(
                &index,
                source,
                destination,
                traffic_class,
                now_ms,
                minimum_capacity,
            )
            .ok_or_else(|| Self::no_route(source, destination))?;
        self.describe_path(
            &index,
            source,
            destination,
            &link_ids,
            traffic_class,
            now_ms,
        )
    }

    fn check_endpoints(
        &self,
        index: &NetworkIndex<'_>,
        source: &str,
        destination: &str,
    ) -> Result<(), RoutingError> {
        for node_id in [source, destination] {
            if !index.nodes.contains_key(node_id) {
                return Err(RoutingError::UnknownNode(node_id.into()));
            }
        }
        Ok(())
    }

    fn eligible(&self, link: &LinkObservation, traffic_class: TrafficClass, now_ms: u64) -> bool {
        if !link.available
            || link.observed_at_ms > now_ms
            || now_ms - link.observed_at_ms > self.policy.maximum_observation_age_ms
            || link.round_trip_time_ms.is_none()
            || link.loss_fraction.is_none_or(|loss| loss >= 1.0)
        {
            return false;
        }
        if traffic_class.prefers_capacity()
            && link
                .available_capacity_bps
                .is_none_or(|capacity| capacity == 0)
        {
            return false;
        }
        !link.metered
            || match traffic_class {
                TrafficClass::Control | TrafficClass::Telemetry => true,
                TrafficClass::Video => self.policy.allow_metered_video,
                TrafficClass::Bulk => self.policy.allow_metered_bulk,
            }
    }

    fn can_forward(&self, index: &NetworkIndex<'_>, node_id: &str, source: &str) -> bool {
        node_id == source
            || index
                .nodes
                .get(node_id)
                .is_some_and(|node| node.relay_allowed)
    }

    fn widest_capacity(
        &self,
        index: &NetworkIndex<'_>,
        source: &str,
        destination: &str,
        traffic_class: TrafficClass,
        now_ms: u64,
    ) -> Option<u64> {
        let mut capacities = BTreeMap::from([(source.to_owned(), u64::MAX)]);
        let mut pending = BinaryHeap::from([(u64::MAX, source.to_owned())]);
        while let Some((capacity, node_id)) = pending.pop() {
            if node_id == destination {
                return Some(capacity);
            }
            if capacities.get(&node_id) != Some(&capacity)
                || !self.can_forward(index, &node_id, source)
            {
                continue;
            }
            for link in index.outgoing.get(node_id.as_str()).into_iter().flatten() {
                if !self.eligible(link, traffic_class, now_ms) {
                    continue;
                }
                let candidate_capacity = capacity.min(link.available_capacity_bps?);
                if capacities
                    .get(&link.destination)
                    .is_none_or(|known| candidate_capacity > *known)
                {
                    capacities.insert(link.destination.clone(), candidate_capacity);
                    pending.push((candidate_capacity, link.destination.clone()));
                }
            }
        }
        None
    }

    fn reliable_path(
        &self,
        index: &NetworkIndex<'_>,
        source: &str,
        destination: &str,
        traffic_class: TrafficClass,
        now_ms: u64,
        minimum_capacity: Option<u64>,
    ) -> Option<Vec<String>> {
        let initial_cost = PathCost {
            loss_cost: 0.0,
            round_trip_time_ms: 0.0,
            hop_count: 0,
            last_link_id: String::new(),
        };
        let mut best = BTreeMap::from([(source.to_owned(), initial_cost.clone())]);
        let mut predecessors: BTreeMap<String, (String, String)> = BTreeMap::new();
        let mut pending = BinaryHeap::from([SearchEntry {
            node_id: source.into(),
            cost: initial_cost,
        }]);
        while let Some(entry) = pending.pop() {
            if best.get(&entry.node_id) != Some(&entry.cost) {
                continue;
            }
            if entry.node_id == destination {
                // Retaining one predecessor per vertex avoids copying full prefixes,
                // which would consume quadratic memory on long chains.
                let mut link_ids = Vec::new();
                let mut current = destination;
                while current != source {
                    let (previous, link_id) = predecessors.get(current)?;
                    link_ids.push(link_id.clone());
                    current = previous;
                }
                link_ids.reverse();
                return Some(link_ids);
            }
            if !self.can_forward(index, &entry.node_id, source) {
                continue;
            }
            for link in index
                .outgoing
                .get(entry.node_id.as_str())
                .into_iter()
                .flatten()
            {
                if !self.eligible(link, traffic_class, now_ms)
                    || minimum_capacity.is_some_and(|minimum| {
                        link.available_capacity_bps
                            .is_none_or(|capacity| capacity < minimum)
                    })
                {
                    continue;
                }
                let mut cost = entry.cost.clone();
                cost.loss_cost += -(-link.loss_fraction?).ln_1p();
                cost.round_trip_time_ms += link.round_trip_time_ms?;
                if !cost.round_trip_time_ms.is_finite() {
                    continue;
                }
                cost.hop_count += 1;
                cost.last_link_id.clone_from(&link.link_id);
                if best
                    .get(&link.destination)
                    .is_none_or(|known| cost < *known)
                {
                    best.insert(link.destination.clone(), cost.clone());
                    predecessors.insert(
                        link.destination.clone(),
                        (entry.node_id.clone(), link.link_id.clone()),
                    );
                    pending.push(SearchEntry {
                        node_id: link.destination.clone(),
                        cost,
                    });
                }
            }
        }
        None
    }

    fn describe_path(
        &self,
        index: &NetworkIndex<'_>,
        source: &str,
        destination: &str,
        link_ids: &[String],
        traffic_class: TrafficClass,
        now_ms: u64,
    ) -> Result<Route, RoutingError> {
        let mut route = Route {
            traffic_class,
            node_ids: vec![source.into()],
            link_ids: link_ids.into(),
            estimated_round_trip_time_ms: 0.0,
            estimated_loss_fraction: 0.0,
            estimated_jitter_ms: Some(0.0),
            bottleneck_capacity_bps: None,
            uses_metered_link: false,
        };
        let mut survival_probability = 1.0;
        let mut minimum_capacity = Some(u64::MAX);
        let mut visited = BTreeSet::from([source]);
        let mut current = source;
        for link_id in link_ids {
            let link = index
                .links
                .get(link_id.as_str())
                .ok_or_else(|| Self::no_route(source, destination))?;
            if link.source != current
                || !self.can_forward(index, current, source)
                || !self.eligible(link, traffic_class, now_ms)
                || !visited.insert(link.destination.as_str())
            {
                return Err(Self::no_route(source, destination));
            }
            survival_probability *= 1.0 - link.loss_fraction.expect("eligible measurements");
            route.estimated_round_trip_time_ms +=
                link.round_trip_time_ms.expect("eligible measurements");
            route.estimated_jitter_ms = route
                .estimated_jitter_ms
                .zip(link.jitter_ms)
                .map(|(total, jitter)| total + jitter)
                .filter(|total| total.is_finite());
            minimum_capacity = minimum_capacity
                .zip(link.available_capacity_bps)
                .map(|(total, capacity)| total.min(capacity));
            route.uses_metered_link |= link.metered;
            route.node_ids.push(link.destination.clone());
            current = &link.destination;
        }
        if current != destination {
            return Err(Self::no_route(source, destination));
        }
        route.estimated_loss_fraction = 1.0 - survival_probability;
        route.bottleneck_capacity_bps = if link_ids.is_empty() {
            None
        } else {
            minimum_capacity
        };
        Ok(route)
    }

    fn no_route(source: &str, destination: &str) -> RoutingError {
        RoutingError::NoRoute {
            source_node: source.into(),
            destination_node: destination.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowRequest {
    pub flow_id: String,
    pub source: String,
    pub destination: String,
    pub traffic_class: TrafficClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FlowPolicy {
    pub maximum_active_flows: usize,
    pub idle_timeout_ms: u64,
    pub improvement_hold_ms: u64,
}

impl Default for FlowPolicy {
    fn default() -> Self {
        Self {
            maximum_active_flows: 4096,
            idle_timeout_ms: 60_000,
            improvement_hold_ms: 5_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteChangeReason {
    NewFlow,
    Unchanged,
    ImprovementPending,
    ImprovedPath,
    PreviousPathUnavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteDecision {
    pub route: Route,
    pub reason: RouteChangeReason,
}

struct FlowState {
    request: FlowRequest,
    route: Route,
    last_used_ms: u64,
    pending_improvement: Option<(Vec<String>, u64)>,
}

/// A bounded control-plane cache. Packet forwarding and failure detection are external.
pub struct FlowRouter {
    pub planner: RoutePlanner,
    policy: FlowPolicy,
    flows: BTreeMap<String, FlowState>,
}

impl FlowRouter {
    pub fn new(planner: RoutePlanner, policy: FlowPolicy) -> Result<Self, RoutingError> {
        if policy.maximum_active_flows == 0 || policy.idle_timeout_ms == 0 {
            return Err(RoutingError::InvalidFlowPolicy(
                "capacity and idle timeout must be positive".into(),
            ));
        }
        Ok(Self {
            planner,
            policy,
            flows: BTreeMap::new(),
        })
    }

    pub fn active_flow_count(&self) -> usize {
        self.flows.len()
    }

    pub fn expire_idle_flows(&mut self, now_ms: u64) -> usize {
        let previous_count = self.flows.len();
        self.flows.retain(|_, state| {
            now_ms.saturating_sub(state.last_used_ms) < self.policy.idle_timeout_ms
        });
        previous_count - self.flows.len()
    }

    pub fn remove_flow(&mut self, flow_id: &str) -> bool {
        self.flows.remove(flow_id).is_some()
    }

    pub fn route(
        &mut self,
        snapshot: &NetworkSnapshot,
        request: FlowRequest,
        now_ms: u64,
    ) -> Result<RouteDecision, RoutingError> {
        if request.flow_id.is_empty() {
            return Err(RoutingError::InvalidFlowRequest(
                "flow identifier is empty".into(),
            ));
        }
        self.expire_idle_flows(now_ms);
        if let Some(state) = self.flows.get(&request.flow_id) {
            if state.request != request {
                return Err(RoutingError::FlowConflict);
            }
        } else if self.flows.len() >= self.policy.maximum_active_flows {
            return Err(RoutingError::FlowCapacityReached(
                self.policy.maximum_active_flows,
            ));
        }
        let candidate = match self.planner.plan(
            snapshot,
            &request.source,
            &request.destination,
            request.traffic_class,
            now_ms,
        ) {
            Ok(route) => route,
            Err(error) => {
                self.flows.remove(&request.flow_id);
                return Err(error);
            }
        };
        let Some(state) = self.flows.get_mut(&request.flow_id) else {
            self.flows.insert(
                request.flow_id.clone(),
                FlowState {
                    request,
                    route: candidate.clone(),
                    last_used_ms: now_ms,
                    pending_improvement: None,
                },
            );
            return Ok(RouteDecision {
                route: candidate,
                reason: RouteChangeReason::NewFlow,
            });
        };
        state.last_used_ms = now_ms;
        let index = NetworkIndex::new(snapshot)?;
        let existing = self.planner.describe_path(
            &index,
            &request.source,
            &request.destination,
            &state.route.link_ids,
            request.traffic_class,
            now_ms,
        );
        let reason = match existing {
            Err(_) => {
                state.route = candidate;
                state.pending_improvement = None;
                RouteChangeReason::PreviousPathUnavailable
            }
            Ok(current) if current.link_ids == candidate.link_ids => {
                state.route = current;
                state.pending_improvement = None;
                RouteChangeReason::Unchanged
            }
            Ok(current) => {
                state.route = current;
                let pending_since = match &state.pending_improvement {
                    Some((links, since)) if *links == candidate.link_ids => *since,
                    _ => now_ms,
                };
                if now_ms.saturating_sub(pending_since) >= self.policy.improvement_hold_ms {
                    state.route = candidate;
                    state.pending_improvement = None;
                    RouteChangeReason::ImprovedPath
                } else {
                    state.pending_improvement = Some((candidate.link_ids, pending_since));
                    RouteChangeReason::ImprovementPending
                }
            }
        };
        Ok(RouteDecision {
            route: state.route.clone(),
            reason,
        })
    }
}

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
    /// Compatibility name: measured delivered-rate lower bound, never spare bandwidth.
    pub available_capacity_bps: Option<u64>,
    /// Collector-local deadline derived from authenticated receipt; never a remote clock.
    #[serde(default)]
    pub topology_valid_until_ms: Option<u64>,
    #[serde(default)]
    pub capacity_valid_until_ms: Option<u64>,
    #[serde(default)]
    pub capacity_source: Option<CapacitySource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapacitySource {
    BoundedProbe,
    CallerSupplied,
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
    pub degraded: bool,
    pub degraded_reasons: Vec<String>,
    pub capacity_source: Option<CapacitySource>,
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

    /// Plan all destinations using one source tree per class/capacity threshold.
    /// Search state is O(V + E); only the requested route descriptions contain full paths.
    pub fn plan_all(
        &self,
        snapshot: &NetworkSnapshot,
        source: &str,
        traffic_class: TrafficClass,
        now_ms: u64,
    ) -> Result<BTreeMap<String, Route>, RoutingError> {
        let index = NetworkIndex::new(snapshot)?;
        self.check_endpoints(&index, source, source)?;
        let mut groups: BTreeMap<Option<u64>, Vec<&str>> = BTreeMap::new();
        if traffic_class.prefers_capacity() {
            for (destination, capacity) in
                self.widest_capacities(&index, source, traffic_class, now_ms)
            {
                if destination != source
                    && let Some((identifier, _)) = index.nodes.get_key_value(destination.as_str())
                {
                    groups.entry(Some(capacity)).or_default().push(identifier);
                }
            }
        } else {
            groups.insert(
                None,
                index
                    .nodes
                    .keys()
                    .copied()
                    .filter(|node| *node != source)
                    .collect(),
            );
        }
        let mut routes = BTreeMap::from([(
            source.to_owned(),
            self.describe_path(&index, source, source, &[], traffic_class, now_ms)?,
        )]);
        for (capacity, destinations) in groups {
            let tree = self.reliable_tree(&index, source, traffic_class, now_ms, capacity);
            for destination in destinations {
                if let Some(path) = Self::tree_path(&tree, source, destination) {
                    routes.insert(
                        destination.to_owned(),
                        self.describe_path(
                            &index,
                            source,
                            destination,
                            &path,
                            traffic_class,
                            now_ms,
                        )?,
                    );
                }
            }
        }
        Ok(routes)
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

    fn metrics_fresh(&self, link: &LinkObservation, now_ms: u64) -> bool {
        link.observed_at_ms <= now_ms
            && now_ms - link.observed_at_ms <= self.policy.maximum_observation_age_ms
    }

    fn capacity(&self, link: &LinkObservation, now_ms: u64) -> Option<u64> {
        let valid = link.capacity_valid_until_ms.map_or_else(
            || self.metrics_fresh(link, now_ms),
            |deadline| now_ms <= deadline,
        );
        valid
            .then_some(link.available_capacity_bps)
            .flatten()
            .filter(|value| *value > 0)
    }

    fn quality(&self, link: &LinkObservation, now_ms: u64) -> (f64, f64, f64) {
        if self.metrics_fresh(link, now_ms) {
            (
                link.loss_fraction.unwrap_or(0.1),
                link.round_trip_time_ms.unwrap_or(1_000.0),
                link.jitter_ms.unwrap_or(100.0),
            )
        } else {
            (0.1, 1_000.0, 100.0)
        }
    }

    fn eligible(&self, link: &LinkObservation, traffic_class: TrafficClass, now_ms: u64) -> bool {
        let topology_fresh = link.topology_valid_until_ms.map_or_else(
            || self.metrics_fresh(link, now_ms),
            |deadline| now_ms <= deadline,
        );
        if !link.available
            || !topology_fresh
            || link.observed_at_ms > now_ms
            || (self.metrics_fresh(link, now_ms)
                && link.loss_fraction.is_some_and(|loss| loss >= 1.0))
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
        self.widest_capacities(index, source, traffic_class, now_ms)
            .get(destination)
            .copied()
    }

    fn widest_capacities(
        &self,
        index: &NetworkIndex<'_>,
        source: &str,
        traffic_class: TrafficClass,
        now_ms: u64,
    ) -> BTreeMap<String, u64> {
        let mut capacities = BTreeMap::from([(source.to_owned(), u64::MAX)]);
        let mut pending = BinaryHeap::from([(u64::MAX, source.to_owned())]);
        while let Some((capacity, node_id)) = pending.pop() {
            if capacities.get(&node_id) != Some(&capacity)
                || !self.can_forward(index, &node_id, source)
            {
                continue;
            }
            for link in index.outgoing.get(node_id.as_str()).into_iter().flatten() {
                if !self.eligible(link, traffic_class, now_ms) {
                    continue;
                }
                let candidate_capacity = capacity.min(self.capacity(link, now_ms).unwrap_or(0));
                if capacities
                    .get(&link.destination)
                    .is_none_or(|known| candidate_capacity > *known)
                {
                    capacities.insert(link.destination.clone(), candidate_capacity);
                    pending.push((candidate_capacity, link.destination.clone()));
                }
            }
        }
        capacities
    }

    fn tree_path(
        predecessors: &BTreeMap<String, (String, String)>,
        source: &str,
        destination: &str,
    ) -> Option<Vec<String>> {
        let mut path = Vec::new();
        let mut current = destination;
        while current != source {
            let (previous, link) = predecessors.get(current)?;
            path.push(link.clone());
            current = previous;
        }
        path.reverse();
        Some(path)
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
        Self::tree_path(
            &self.reliable_tree(index, source, traffic_class, now_ms, minimum_capacity),
            source,
            destination,
        )
    }

    fn reliable_tree(
        &self,
        index: &NetworkIndex<'_>,
        source: &str,
        traffic_class: TrafficClass,
        now_ms: u64,
        minimum_capacity: Option<u64>,
    ) -> BTreeMap<String, (String, String)> {
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
                    || minimum_capacity
                        .is_some_and(|minimum| self.capacity(link, now_ms).unwrap_or(0) < minimum)
                {
                    continue;
                }
                let mut cost = entry.cost.clone();
                let (loss, latency, jitter) = self.quality(link, now_ms);
                cost.loss_cost += -(-loss).ln_1p();
                cost.round_trip_time_ms += latency + jitter;
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
        predecessors
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
            degraded: false,
            degraded_reasons: Vec::new(),
            capacity_source: None,
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
            let (loss, latency, _) = self.quality(link, now_ms);
            survival_probability *= 1.0 - loss;
            route.estimated_round_trip_time_ms += latency;
            if !self.metrics_fresh(link, now_ms) {
                route.degraded_reasons.push("quality_stale".into());
            } else if link.round_trip_time_ms.is_none()
                || link.loss_fraction.is_none()
                || link.jitter_ms.is_none()
            {
                route.degraded_reasons.push("quality_unknown".into());
            }
            if traffic_class.prefers_capacity() && self.capacity(link, now_ms).is_none() {
                route
                    .degraded_reasons
                    .push(if link.available_capacity_bps.is_some() {
                        "capacity_stale".into()
                    } else {
                        "capacity_unknown".into()
                    });
            }
            route.estimated_jitter_ms = route
                .estimated_jitter_ms
                .zip(
                    self.metrics_fresh(link, now_ms)
                        .then_some(link.jitter_ms)
                        .flatten(),
                )
                .map(|(total, jitter)| total + jitter)
                .filter(|total| total.is_finite());
            minimum_capacity = minimum_capacity
                .zip(self.capacity(link, now_ms))
                .map(|(total, capacity)| total.min(capacity));
            route.capacity_source = match (
                route.capacity_source,
                link.capacity_source
                    .unwrap_or(CapacitySource::CallerSupplied),
            ) {
                (None, source) => Some(source),
                (Some(previous), source) if previous == source => Some(source),
                _ => Some(CapacitySource::CallerSupplied),
            };
            route.uses_metered_link |= link.metered;
            route.node_ids.push(link.destination.clone());
            current = &link.destination;
        }
        if current != destination {
            return Err(Self::no_route(source, destination));
        }
        route.degraded_reasons.sort();
        route.degraded_reasons.dedup();
        route.degraded = !route.degraded_reasons.is_empty();
        route.estimated_loss_fraction = 1.0 - survival_probability;
        route.bottleneck_capacity_bps = if link_ids.is_empty() {
            None
        } else {
            minimum_capacity
        };
        if route.bottleneck_capacity_bps.is_none() {
            route.capacity_source = None;
        }
        Ok(route)
    }

    fn stable_pending_route(
        &self,
        index: &NetworkIndex<'_>,
        request: &FlowRequest,
        current: &Route,
        candidate: &Route,
        pending_links: &[String],
        now_ms: u64,
    ) -> Option<Route> {
        if current.link_ids == candidate.link_ids
            || pending_links == candidate.link_ids
            || !Self::substantial_improvement(current, candidate)
        {
            return None;
        }
        // Revalidate the entire pending path. Minor ranking changes between
        // healthy parallel connections must not restart its hold indefinitely.
        let pending = self
            .describe_path(
                index,
                &request.source,
                &request.destination,
                pending_links,
                request.traffic_class,
                now_ms,
            )
            .ok()?;
        let quality_unknown = pending
            .degraded_reasons
            .iter()
            .any(|reason| reason == "quality_unknown");
        if quality_unknown
            || !Self::substantial_improvement(current, &pending)
            || Self::substantial_improvement(&pending, candidate)
        {
            return None;
        }
        Some(pending)
    }

    pub fn substantial_improvement(current: &Route, candidate: &Route) -> bool {
        if current.degraded && !candidate.degraded {
            return true;
        }
        if current.traffic_class.prefers_capacity() {
            match (
                current.bottleneck_capacity_bps,
                candidate.bottleneck_capacity_bps,
            ) {
                (None, Some(_)) => return true,
                (Some(previous), Some(next)) if next != previous => {
                    return (next as f64) >= (previous as f64) * 1.1;
                }
                (Some(_), None) => return false,
                _ => {}
            }
        }
        let current_delay = current.estimated_round_trip_time_ms
            + current
                .estimated_jitter_ms
                .unwrap_or(100.0 * current.link_ids.len() as f64);
        let candidate_delay = candidate.estimated_round_trip_time_ms
            + candidate
                .estimated_jitter_ms
                .unwrap_or(100.0 * candidate.link_ids.len() as f64);
        current.estimated_loss_fraction - candidate.estimated_loss_fraction >= 0.01
            || (candidate.estimated_loss_fraction <= current.estimated_loss_fraction
                && current_delay - candidate_delay >= 5.0
                && candidate_delay <= current_delay * 0.9)
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
    /// Actual collector-monotonic hold duration of the last healthy route improvement.
    pub improvement_hold_ms: Option<u64>,
}

struct FlowState {
    request: FlowRequest,
    route: Route,
    last_used_ms: u64,
    pending_improvement: Option<(Vec<String>, u64)>,
    improvement_hold_ms: Option<u64>,
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

    pub fn retain_flows(&mut self, flow_ids: &BTreeSet<String>) {
        self.flows
            .retain(|identifier, _| flow_ids.contains(identifier));
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
        self.expire_idle_flows(now_ms);
        let candidate = self.planner.plan(
            snapshot,
            &request.source,
            &request.destination,
            request.traffic_class,
            now_ms,
        );
        let index = NetworkIndex::new(snapshot)?;
        self.apply_candidate(&index, request, candidate, now_ms)
    }

    /// Apply the same hysteresis and failure rules with source-tree planning shared by a batch.
    pub fn route_batch(
        &mut self,
        snapshot: &NetworkSnapshot,
        requests: Vec<FlowRequest>,
        now_ms: u64,
    ) -> Result<Vec<Result<RouteDecision, RoutingError>>, RoutingError> {
        self.expire_idle_flows(now_ms);
        let index = NetworkIndex::new(snapshot)?;
        let mut planned = BTreeMap::new();
        for request in &requests {
            planned
                .entry((request.source.clone(), request.traffic_class))
                .or_insert_with(|| {
                    self.planner
                        .plan_all(snapshot, &request.source, request.traffic_class, now_ms)
                });
        }
        Ok(requests
            .into_iter()
            .map(|request| {
                let candidate = match &planned[&(request.source.clone(), request.traffic_class)] {
                    Err(error) => Err(error.clone()),
                    Ok(_) if !index.nodes.contains_key(request.destination.as_str()) => {
                        Err(RoutingError::UnknownNode(request.destination.clone()))
                    }
                    Ok(routes) => routes.get(&request.destination).cloned().ok_or_else(|| {
                        RoutePlanner::no_route(&request.source, &request.destination)
                    }),
                };
                self.apply_candidate(&index, request, candidate, now_ms)
            })
            .collect())
    }

    fn apply_candidate(
        &mut self,
        index: &NetworkIndex<'_>,
        request: FlowRequest,
        candidate: Result<Route, RoutingError>,
        now_ms: u64,
    ) -> Result<RouteDecision, RoutingError> {
        if request.flow_id.is_empty() {
            return Err(RoutingError::InvalidFlowRequest(
                "flow identifier is empty".into(),
            ));
        }
        if let Some(state) = self.flows.get(&request.flow_id) {
            if state.request != request {
                return Err(RoutingError::FlowConflict);
            }
        } else if self.flows.len() >= self.policy.maximum_active_flows {
            return Err(RoutingError::FlowCapacityReached(
                self.policy.maximum_active_flows,
            ));
        }
        let mut candidate = match candidate {
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
                    improvement_hold_ms: None,
                },
            );
            return Ok(RouteDecision {
                route: candidate,
                reason: RouteChangeReason::NewFlow,
                improvement_hold_ms: None,
            });
        };
        state.last_used_ms = now_ms;
        let existing = self.planner.describe_path(
            index,
            &request.source,
            &request.destination,
            &state.route.link_ids,
            request.traffic_class,
            now_ms,
        );
        if let (Ok(current), Some((pending_links, _))) = (&existing, &state.pending_improvement)
            && let Some(pending) = self.planner.stable_pending_route(
                index,
                &request,
                current,
                &candidate,
                pending_links,
                now_ms,
            )
        {
            candidate = pending;
        }
        let reason = match existing {
            Err(_) => {
                state.route = candidate;
                state.pending_improvement = None;
                state.improvement_hold_ms = None;
                RouteChangeReason::PreviousPathUnavailable
            }
            Ok(current) if current.link_ids == candidate.link_ids => {
                state.route = current;
                state.pending_improvement = None;
                RouteChangeReason::Unchanged
            }
            Ok(current) if !RoutePlanner::substantial_improvement(&current, &candidate) => {
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
                    state.improvement_hold_ms = Some(now_ms.saturating_sub(pending_since));
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
            improvement_hold_ms: state.improvement_hold_ms,
        })
    }
}

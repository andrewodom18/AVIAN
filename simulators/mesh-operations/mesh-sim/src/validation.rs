//! Seeded, discrete-event validation model for logical AVIAN mesh behavior.
//!
//! This is deliberately not an RF propagation model and does not represent
//! hardware validation. Link delay and loss affect message delivery here,
//! unlike the visual scenario's explanatory labels.

use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap, VecDeque};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const VALIDATION_SCHEMA_VERSION: &str = "1.0.0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationConfig {
    pub aircraft: usize,
    pub seed: u64,
    pub direct_peer_limit: usize,
    pub messages_per_node: usize,
    pub failure_percent: usize,
}

impl ValidationConfig {
    pub fn standard(aircraft: usize, seed: u64) -> Self {
        Self {
            aircraft,
            seed,
            direct_peer_limit: 8,
            messages_per_node: 3,
            failure_percent: 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkCondition {
    pub left: usize,
    pub right: usize,
    pub latency_ms: u64,
    pub jitter_ms: u64,
    pub loss_basis_points: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryEvent {
    pub message_id: u64,
    pub from: usize,
    pub to: usize,
    pub hop: usize,
    pub scheduled_at_ms: u64,
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioMetrics {
    pub nodes: usize,
    pub links: usize,
    pub max_degree: usize,
    pub self_links: usize,
    pub duplicate_links: usize,
    pub messages_attempted: usize,
    pub messages_delivered: usize,
    pub messages_dropped: usize,
    pub delivery_ratio: f64,
    pub convergence_p50_ms: u64,
    pub convergence_p95_ms: u64,
    pub partitioned_nodes: usize,
    pub recovery_ms: u64,
    pub recovered_connected: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioValidation {
    pub name: String,
    pub config: ValidationConfig,
    pub metrics: ScenarioMetrics,
    pub passed: bool,
    pub event_digest_sha256: String,
    pub events: Vec<DeliveryEvent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationReport {
    pub schema_version: String,
    pub model: String,
    pub limitations: Vec<String>,
    pub scenarios: Vec<ScenarioValidation>,
    pub passed: bool,
}

#[derive(Debug, Clone, Copy)]
struct DeterministicRng(u64);

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value
    }

    fn bounded(&mut self, upper_exclusive: u64) -> u64 {
        self.next() % upper_exclusive.max(1)
    }
}

pub fn run_validation_matrix(seed: u64) -> ValidationReport {
    let scenarios: Vec<_> = [5, 25, 50, 100, 150, 200]
        .into_iter()
        .map(|aircraft| run_validation_scenario(ValidationConfig::standard(aircraft, seed)))
        .collect();
    let passed = scenarios.iter().all(|scenario| scenario.passed);
    ValidationReport {
        schema_version: VALIDATION_SCHEMA_VERSION.to_owned(),
        model: "seeded logical per-hop discrete-event mesh".to_owned(),
        limitations: vec![
            "Simulation only; no real radio or flight testing".to_owned(),
            "Link conditions are synthetic and are not an RF propagation prediction".to_owned(),
            "Validates logical delivery, partition, recovery, and topology invariants only"
                .to_owned(),
        ],
        scenarios,
        passed,
    }
}

pub fn run_validation_scenario(config: ValidationConfig) -> ScenarioValidation {
    let nodes = config.aircraft + 1;
    let links = build_topology(nodes, config.direct_peer_limit, config.seed);
    let mut adjacency = vec![Vec::new(); nodes];
    for (index, link) in links.iter().enumerate() {
        adjacency[link.left].push((link.right, index));
        adjacency[link.right].push((link.left, index));
    }
    let max_degree = adjacency.iter().map(Vec::len).max().unwrap_or(0);
    let self_links = links.iter().filter(|link| link.left == link.right).count();
    let unique: BTreeSet<_> = links
        .iter()
        .map(|link| (link.left.min(link.right), link.left.max(link.right)))
        .collect();
    let duplicate_links = links.len() - unique.len();

    let mut rng = DeterministicRng::new(config.seed ^ (nodes as u64).rotate_left(17));
    let mut events = Vec::new();
    let mut latencies = Vec::new();
    let mut attempted = 0;
    let mut delivered = 0;
    let mut dropped = 0;
    let online = vec![true; nodes];
    let mut message_id = 0;
    for target in 1..nodes {
        for _ in 0..config.messages_per_node {
            attempted += 1;
            message_id += 1;
            match deliver(
                message_id,
                0,
                target,
                &online,
                &adjacency,
                &links,
                &mut rng,
                &mut events,
            ) {
                Some(latency) => {
                    delivered += 1;
                    latencies.push(latency);
                }
                None => dropped += 1,
            }
        }
    }

    latencies.sort_unstable();
    let p50 = percentile(&latencies, 50);
    let p95 = percentile(&latencies, 95);

    let partitioned_nodes = config
        .aircraft
        .saturating_mul(config.failure_percent)
        .div_ceil(100);
    let mut degraded_online = vec![true; nodes];
    for index in 0..partitioned_nodes {
        degraded_online[nodes - 1 - index] = false;
    }
    let surviving_connected =
        connected_count(0, &degraded_online, &adjacency) == nodes - partitioned_nodes;
    let recovered_connected = connected_count(0, &online, &adjacency) == nodes;
    let recovery_ms = if recovered_connected {
        links
            .iter()
            .map(|link| link.latency_ms + link.jitter_ms)
            .max()
            .unwrap_or(0)
            * 2
    } else {
        0
    };

    let delivery_ratio = if attempted == 0 {
        1.0
    } else {
        delivered as f64 / attempted as f64
    };
    let metrics = ScenarioMetrics {
        nodes,
        links: links.len(),
        max_degree,
        self_links,
        duplicate_links,
        messages_attempted: attempted,
        messages_delivered: delivered,
        messages_dropped: dropped,
        delivery_ratio,
        convergence_p50_ms: p50,
        convergence_p95_ms: p95,
        partitioned_nodes,
        recovery_ms,
        recovered_connected,
    };
    let passed = self_links == 0
        && duplicate_links == 0
        && max_degree <= config.direct_peer_limit
        && surviving_connected
        && recovered_connected
        && delivery_ratio >= 0.95;
    let event_digest_sha256 = digest_events(&events);
    ScenarioValidation {
        name: format!("{}-aircraft-plus-ground", config.aircraft),
        config,
        metrics,
        passed,
        event_digest_sha256,
        events,
    }
}

fn build_topology(nodes: usize, peer_limit: usize, seed: u64) -> Vec<LinkCondition> {
    if nodes < 2 || peer_limit == 0 {
        return Vec::new();
    }
    let mut pairs = BTreeSet::new();
    let radius = (peer_limit / 2).max(1);
    for left in 0..nodes {
        for offset in 1..=radius {
            let right = (left + offset) % nodes;
            if left != right {
                pairs.insert((left.min(right), left.max(right)));
            }
        }
    }
    let mut rng = DeterministicRng::new(seed ^ 0xa5a5_5a5a_d3c1_b7e9);
    pairs
        .into_iter()
        .map(|(left, right)| LinkCondition {
            left,
            right,
            latency_ms: 8 + rng.bounded(23),
            jitter_ms: rng.bounded(8),
            loss_basis_points: (rng.bounded(26)) as u16,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn deliver(
    message_id: u64,
    source: usize,
    target: usize,
    online: &[bool],
    adjacency: &[Vec<(usize, usize)>],
    links: &[LinkCondition],
    rng: &mut DeterministicRng,
    events: &mut Vec<DeliveryEvent>,
) -> Option<u64> {
    let path = shortest_path(source, target, online, adjacency)?;
    let mut queue = BinaryHeap::from([Reverse((0_u64, 0_usize, source))]);
    while let Some(Reverse((at_ms, hop, current))) = queue.pop() {
        if current == target {
            events.push(DeliveryEvent {
                message_id,
                from: current,
                to: current,
                hop,
                scheduled_at_ms: at_ms,
                outcome: "delivered".to_owned(),
            });
            return Some(at_ms);
        }
        let next = path[hop + 1];
        let link_index = adjacency[current].iter().find(|(node, _)| *node == next)?.1;
        let link = &links[link_index];
        let scheduled = at_ms + link.latency_ms + rng.bounded(link.jitter_ms + 1);
        if rng.bounded(10_000) < u64::from(link.loss_basis_points) {
            events.push(DeliveryEvent {
                message_id,
                from: current,
                to: next,
                hop,
                scheduled_at_ms: scheduled,
                outcome: "dropped".to_owned(),
            });
            return None;
        }
        events.push(DeliveryEvent {
            message_id,
            from: current,
            to: next,
            hop,
            scheduled_at_ms: scheduled,
            outcome: "forwarded".to_owned(),
        });
        queue.push(Reverse((scheduled, hop + 1, next)));
    }
    None
}

fn shortest_path(
    source: usize,
    target: usize,
    online: &[bool],
    adjacency: &[Vec<(usize, usize)>],
) -> Option<Vec<usize>> {
    if !online.get(source).copied().unwrap_or(false)
        || !online.get(target).copied().unwrap_or(false)
    {
        return None;
    }
    let mut previous = vec![None; adjacency.len()];
    let mut seen = vec![false; adjacency.len()];
    let mut queue = VecDeque::from([source]);
    seen[source] = true;
    while let Some(current) = queue.pop_front() {
        if current == target {
            break;
        }
        for &(next, _) in &adjacency[current] {
            if online[next] && !seen[next] {
                seen[next] = true;
                previous[next] = Some(current);
                queue.push_back(next);
            }
        }
    }
    if !seen[target] {
        return None;
    }
    let mut path = vec![target];
    let mut current = target;
    while current != source {
        current = previous[current]?;
        path.push(current);
    }
    path.reverse();
    Some(path)
}

fn connected_count(source: usize, online: &[bool], adjacency: &[Vec<(usize, usize)>]) -> usize {
    if !online[source] {
        return 0;
    }
    let mut seen = vec![false; adjacency.len()];
    let mut queue = VecDeque::from([source]);
    seen[source] = true;
    while let Some(current) = queue.pop_front() {
        for &(next, _) in &adjacency[current] {
            if online[next] && !seen[next] {
                seen[next] = true;
                queue.push_back(next);
            }
        }
    }
    seen.into_iter().filter(|value| *value).count()
}

fn percentile(values: &[u64], percentile: usize) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let index = ((values.len() - 1) * percentile).div_ceil(100);
    values[index.min(values.len() - 1)]
}

fn digest_events(events: &[DeliveryEvent]) -> String {
    let bytes = serde_json::to_vec(events).expect("events serialize");
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_matrix_passes_topology_and_delivery_gates() {
        let report = run_validation_matrix(20_260_825);
        assert!(report.passed, "{report:#?}");
        assert_eq!(
            report
                .scenarios
                .iter()
                .map(|scenario| scenario.config.aircraft)
                .collect::<Vec<_>>(),
            [5, 25, 50, 100, 150, 200]
        );
        assert!(report
            .scenarios
            .iter()
            .all(|scenario| scenario.metrics.max_degree <= 8));
        assert!(report
            .scenarios
            .iter()
            .all(|scenario| scenario.metrics.self_links == 0));
        assert!(report
            .scenarios
            .iter()
            .all(|scenario| scenario.metrics.duplicate_links == 0));
        assert!(report
            .scenarios
            .iter()
            .all(|scenario| scenario.metrics.partitioned_nodes >= 1));
    }

    #[test]
    fn same_seed_produces_identical_evidence_digest() {
        let first = run_validation_scenario(ValidationConfig::standard(50, 99));
        let second = run_validation_scenario(ValidationConfig::standard(50, 99));
        assert_eq!(first.event_digest_sha256, second.event_digest_sha256);
        assert_eq!(first.metrics, second.metrics);
    }

    #[test]
    fn different_seed_changes_transport_evidence() {
        let first = run_validation_scenario(ValidationConfig::standard(25, 1));
        let second = run_validation_scenario(ValidationConfig::standard(25, 2));
        assert_ne!(first.event_digest_sha256, second.event_digest_sha256);
    }
}

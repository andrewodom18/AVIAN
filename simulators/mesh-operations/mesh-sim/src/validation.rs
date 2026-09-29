//! Seeded logical mesh validation. Time and link conditions are synthetic.
#[path = "transport.rs"]
pub mod transport;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use transport::{Fault, Link, Message, Policy};

pub const VALIDATION_SCHEMA_VERSION: &str = "2.0.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    Recovery,
    Congestion,
    Loss,
    Duplication,
    Expiry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationConfig {
    pub profile: Profile,
    pub aircraft: usize,
    pub seed: u64,
    pub direct_peer_limit: usize,
    pub messages_per_node: usize,
    pub failure_percent: usize,
    pub transport: Policy,
}

impl ValidationConfig {
    pub fn standard(aircraft: usize, seed: u64) -> Self {
        Self {
            profile: Profile::Recovery,
            aircraft,
            seed,
            direct_peer_limit: 8,
            messages_per_node: 3,
            failure_percent: 10,
            transport: Policy::default(),
        }
    }

    pub fn fault(aircraft: usize, seed: u64, profile: Profile) -> Self {
        let mut config = Self::standard(aircraft, seed);
        config.profile = profile;
        match profile {
            Profile::Recovery => {}
            Profile::Congestion => {
                config.transport.queue_capacity = 1;
                config.transport.bytes_per_ms = 1;
            }
            Profile::Loss => {
                config.transport.loss_basis_points = 10000;
            }
            Profile::Duplication => {
                config.transport.duplicate_basis_points = 10000;
                config.transport.reorder_ms = 100;
            }
            Profile::Expiry => {
                config.transport.ttl_ms = 1;
            }
        }
        config
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
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
    pub delivery_latency_p50_ms: u64,
    pub delivery_latency_p95_ms: u64,
    pub generation_one_convergence_ms: Option<u64>,
    pub generation_two_convergence_ms: Option<u64>,
    pub partitioned_nodes: usize,
    pub recovery_ms: Option<u64>,
    pub latest_generation_converged: bool,
    pub converged_nodes: usize,
    pub peak_queue: usize,
    pub queue_drops: usize,
    pub ttl_expired: usize,
    pub retry_exhausted: usize,
    pub duplicate_rejected: usize,
    pub stale_generation_rejected: usize,
    pub commands_applied: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScenarioValidation {
    pub name: String,
    pub config: ValidationConfig,
    pub links: Vec<Link>,
    pub messages: Vec<Message>,
    pub faults: Vec<Fault>,
    pub metrics: ScenarioMetrics,
    pub passed: bool,
    pub event_digest_sha256: String,
    pub events: Vec<transport::Event>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ValidationReport {
    pub schema_version: String,
    pub events_included: bool,
    pub model: String,
    pub limitations: Vec<String>,
    pub scenarios: Vec<ScenarioValidation>,
    pub fault_scenarios: Vec<ScenarioValidation>,
    pub passed: bool,
}

pub fn run_validation_matrix(seed: u64) -> ValidationReport {
    let scenarios: Vec<_> = [5, 25, 50, 100, 150, 200]
        .into_iter()
        .map(|size| run_validation_scenario(ValidationConfig::standard(size, seed)))
        .collect();
    let fault_scenarios: Vec<_> = [5, 25, 50, 100, 150, 200]
        .into_iter()
        .flat_map(|size| {
            [
                Profile::Congestion,
                Profile::Loss,
                Profile::Duplication,
                Profile::Expiry,
            ]
            .into_iter()
            .map(move |profile| {
                run_validation_scenario(ValidationConfig::fault(size, seed, profile))
            })
        })
        .collect();
    ValidationReport { schema_version: VALIDATION_SCHEMA_VERSION.into(), events_included: true,
        model: "seeded shared-timeline logical mesh; synthetic bounded queues and state propagation".into(),
        limitations: vec!["Simulation only; no real radio or flight testing".into(),
            "Synthetic link rates and delays are not measured RF capacity or propagation predictions".into(),
            "Command execution is a model counter; no hardware command is sent".into()],
        passed: scenarios.iter().chain(&fault_scenarios).all(|s| s.passed), scenarios, fault_scenarios }
}

pub fn run_validation_scenario(config: ValidationConfig) -> ScenarioValidation {
    assert!((5..=200).contains(&config.aircraft));
    assert!(
        (2..=8).contains(&config.direct_peer_limit) && config.direct_peer_limit.is_multiple_of(2)
    );
    assert!(config.failure_percent <= 50 && (1..=10).contains(&config.messages_per_node));
    let nodes = config.aircraft + 1;
    let mut pairs = BTreeSet::new();
    for left in 0..nodes {
        for offset in 1..=config.direct_peer_limit / 2 {
            let right = (left + offset) % nodes;
            if left != right {
                pairs.insert((left.min(right), left.max(right)));
            }
        }
    }
    let links: Vec<_> = pairs
        .into_iter()
        .enumerate()
        .map(|(i, (left, right))| Link {
            left,
            right,
            latency_ms: 8 + (config.seed.wrapping_add(i as u64 * 17) % 23),
            jitter_ms: 7,
        })
        .collect();
    let mut degrees = vec![0; nodes];
    for link in &links {
        degrees[link.left] += 1;
        degrees[link.right] += 1;
    }
    let partitioned_nodes = config
        .aircraft
        .saturating_mul(config.failure_percent)
        .div_ceil(100);
    let faults: Vec<_> = (nodes - partitioned_nodes..nodes)
        .flat_map(|node| {
            [
                Fault {
                    at_ms: 5000,
                    node,
                    link_peer: None,
                    online: false,
                },
                Fault {
                    at_ms: 10000,
                    node,
                    link_peer: None,
                    online: true,
                },
            ]
        })
        .collect();
    let mut messages = Vec::new();
    for target in 1..nodes {
        for index in 0..config.messages_per_node {
            messages.push(Message {
                id: messages.len() as u64 + 1,
                source: 0,
                target,
                generation: 1,
                created_ms: 0,
                command: index == 0,
            });
        }
        // A state update during the partition, then explicit state repair on restoration.
        for created_ms in [5000, 10000] {
            messages.push(Message {
                id: messages.len() as u64 + 1,
                source: 0,
                target,
                generation: 2,
                created_ms,
                command: false,
            });
        }
    }
    let result = transport::run(
        nodes,
        &links,
        &messages,
        &faults,
        &config.transport,
        config.seed,
    );
    let count = |outcome: &str| {
        result
            .events
            .iter()
            .filter(|e| e.outcome == outcome)
            .count()
    };
    let generation_one = result.converged_at.get(&1).copied();
    let generation_two = result.converged_at.get(&2).copied();
    let recovery_ms = generation_two.and_then(|at| at.checked_sub(10000));
    let converged_nodes = result.generations.iter().filter(|g| **g == 2).count();
    let metrics = ScenarioMetrics {
        nodes,
        links: links.len(),
        max_degree: *degrees.iter().max().unwrap_or(&0),
        self_links: links.iter().filter(|l| l.left == l.right).count(),
        duplicate_links: links.len()
            - links
                .iter()
                .map(|l| (l.left.min(l.right), l.left.max(l.right)))
                .collect::<BTreeSet<_>>()
                .len(),
        messages_attempted: messages.len(),
        messages_delivered: result.delivered,
        messages_dropped: result.dropped,
        delivery_ratio: result.delivered as f64 / messages.len() as f64,
        delivery_latency_p50_ms: percentile(&result.latencies, 50),
        delivery_latency_p95_ms: percentile(&result.latencies, 95),
        generation_one_convergence_ms: generation_one,
        generation_two_convergence_ms: generation_two.map(|at| at - 5000),
        partitioned_nodes,
        recovery_ms,
        latest_generation_converged: converged_nodes == nodes,
        converged_nodes,
        peak_queue: result.peak_queue,
        queue_drops: count("queue_full"),
        ttl_expired: count("ttl_expired"),
        retry_exhausted: count("retry_exhausted"),
        duplicate_rejected: count("duplicate_rejected"),
        stale_generation_rejected: count("stale_generation_rejected"),
        commands_applied: result.commands_applied,
    };
    // Fixed fixture budgets, not thresholds derived from observed candidate results.
    let invariant_pass = metrics.max_degree <= config.direct_peer_limit
        && metrics.self_links == 0
        && metrics.duplicate_links == 0
        && metrics.messages_delivered + metrics.messages_dropped == messages.len()
        && metrics.peak_queue <= config.transport.queue_capacity
        && metrics.commands_applied <= config.aircraft;
    let recovered = generation_one.is_some_and(|at| at < 5000)
        && recovery_ms.is_some_and(|elapsed| elapsed < 5000)
        && converged_nodes == nodes;
    let passed = invariant_pass
        && match config.profile {
            Profile::Recovery => {
                recovered
                    && metrics.messages_dropped <= partitioned_nodes
                    && metrics.ttl_expired == 0
            }
            Profile::Duplication => {
                recovered
                    && metrics.duplicate_rejected > 0
                    && metrics.commands_applied == config.aircraft
            }
            Profile::Congestion => {
                metrics.queue_drops > 0 && metrics.messages_dropped > partitioned_nodes
            }
            Profile::Loss => {
                metrics.messages_delivered == 0
                    && metrics.retry_exhausted == messages.len()
                    && metrics.commands_applied == 0
            }
            Profile::Expiry => {
                metrics.messages_delivered == 0
                    && metrics.ttl_expired == messages.len()
                    && metrics.commands_applied == 0
            }
        };
    let event_digest_sha256 = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&result.events).expect("serialize events"))
    );
    ScenarioValidation {
        name: format!(
            "{:?}-{}-aircraft-plus-ground",
            config.profile, config.aircraft
        )
        .to_lowercase(),
        config,
        links,
        messages,
        faults,
        metrics,
        passed,
        event_digest_sha256,
        events: result.events,
    }
}

fn percentile(values: &[u64], percentile: usize) -> u64 {
    if values.is_empty() {
        0
    } else {
        values[((values.len() - 1) * percentile).div_ceil(100)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_scales_reproduce_state_recovery_and_graph_invariants() {
        for seed in [20260825, 20260929, 43] {
            let first = run_validation_matrix(seed);
            let second = run_validation_matrix(seed);
            assert!(
                first.passed,
                "{:#?}",
                first
                    .scenarios
                    .iter()
                    .map(|s| &s.metrics)
                    .collect::<Vec<_>>()
            );
            assert_eq!(first, second);
            for scenario in first.scenarios {
                assert_eq!(scenario.metrics.nodes, scenario.config.aircraft + 1);
                assert!(scenario.metrics.max_degree <= 8);
                assert_eq!(
                    scenario
                        .links
                        .iter()
                        .map(|l| (l.left, l.right))
                        .collect::<BTreeSet<_>>()
                        .len(),
                    scenario.links.len()
                );
                assert!(scenario.links.iter().all(|l| l.left != l.right));
                assert_eq!(
                    scenario.metrics.messages_dropped,
                    scenario.metrics.partitioned_nodes
                );
                assert!(scenario.metrics.recovery_ms.unwrap() > 0);
            }
        }
    }
    #[test]
    fn changed_seed_changes_trace_and_capacity_failure_cannot_pass() {
        let first = run_validation_scenario(ValidationConfig::standard(25, 1));
        let second = run_validation_scenario(ValidationConfig::standard(25, 2));
        assert_ne!(first.event_digest_sha256, second.event_digest_sha256);
        let mut constrained = ValidationConfig::standard(25, 1);
        constrained.transport.queue_capacity = 1;
        constrained.transport.bytes_per_ms = 1;
        constrained.transport.ttl_ms = 100;
        let report = run_validation_scenario(constrained);
        assert!(!report.passed);
        assert!(report.metrics.ttl_expired > 0);
        assert!(report.metrics.queue_drops > 0);
        assert_eq!(report.metrics.recovery_ms, None);
    }
}

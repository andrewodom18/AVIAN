//! Synthetic transport only. No sockets, aircraft commands, or RF predictions.
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub bytes_per_ms: u64,
    pub queue_capacity: usize,
    pub packet_bytes: u64,
    pub ttl_ms: u64,
    pub retry_limit: usize,
    pub retry_ms: u64,
    pub loss_basis_points: u64,
    pub duplicate_basis_points: u64,
    pub reorder_ms: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            bytes_per_ms: 1024,
            queue_capacity: 1024,
            packet_bytes: 256,
            ttl_ms: 30000,
            retry_limit: 3,
            retry_ms: 50,
            loss_basis_points: 0,
            duplicate_basis_points: 0,
            reorder_ms: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub left: usize,
    pub right: usize,
    pub latency_ms: u64,
    pub jitter_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub id: u64,
    pub source: usize,
    pub target: usize,
    pub generation: u64,
    pub created_ms: u64,
    pub command: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fault {
    pub at_ms: u64,
    pub node: usize,
    pub link_peer: Option<usize>,
    pub online: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub sequence: u64,
    pub at_ms: u64,
    pub message_id: u64,
    pub from: usize,
    pub to: usize,
    pub generation: u64,
    pub attempt: usize,
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Result {
    pub events: Vec<Event>,
    pub delivered: usize,
    pub dropped: usize,
    pub latencies: Vec<u64>,
    pub generations: Vec<u64>,
    pub converged_at: BTreeMap<u64, u64>,
    pub peak_queue: usize,
    pub commands_applied: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Action {
    Fault(usize, Option<usize>, bool),
    Send(usize, usize, usize), // message index, current node, attempt
    Arrive(usize, usize, usize, usize), // message index, from, to, attempt
    Expire(usize),
}

struct Engine<'a> {
    policy: &'a Policy,
    links: &'a [Link],
    messages: &'a [Message],
    online: Vec<bool>,
    disabled_links: BTreeSet<(usize, usize)>,
    adjacency: Vec<Vec<(usize, usize)>>,
    queue: BinaryHeap<Reverse<(u64, u64, Action)>>,
    queues: BTreeMap<(usize, usize), VecDeque<u64>>,
    sequence: u64,
    rng: u64,
    terminal: Vec<bool>,
    seen: BTreeSet<(usize, u64)>,
    result: Result,
}

/// Same-time faults precede scheduled messages; all later ties use insertion order.
pub fn run(
    nodes: usize,
    links: &[Link],
    messages: &[Message],
    faults: &[Fault],
    policy: &Policy,
    seed: u64,
) -> Result {
    assert!((2..=201).contains(&nodes));
    assert!(policy.bytes_per_ms > 0 && policy.packet_bytes > 0 && policy.queue_capacity > 0);
    assert!(policy.retry_ms > 0 && policy.retry_limit <= 100 && policy.ttl_ms > 0);
    assert!(policy.loss_basis_points <= 10000 && policy.duplicate_basis_points <= 10000);
    assert!(messages.iter().map(|m| m.id).collect::<BTreeSet<_>>().len() == messages.len());
    let mut engine = Engine {
        policy,
        links,
        messages,
        online: vec![true; nodes],
        disabled_links: BTreeSet::new(),
        adjacency: vec![Vec::new(); nodes],
        queue: BinaryHeap::new(),
        queues: BTreeMap::new(),
        sequence: 0,
        rng: seed.max(1),
        terminal: vec![false; messages.len()],
        seen: BTreeSet::new(),
        result: Result {
            events: Vec::new(),
            delivered: 0,
            dropped: 0,
            latencies: Vec::new(),
            generations: vec![0; nodes],
            converged_at: BTreeMap::new(),
            peak_queue: 0,
            commands_applied: 0,
        },
    };
    for (index, link) in links.iter().enumerate() {
        assert!(link.left < nodes && link.right < nodes && link.left != link.right);
        engine.adjacency[link.left].push((link.right, index));
        engine.adjacency[link.right].push((link.left, index));
    }
    for fault in faults {
        assert!(fault.node < nodes);
        if let Some(peer) = fault.link_peer {
            assert!(peer < nodes && peer != fault.node);
        }
        engine.schedule(
            fault.at_ms,
            Action::Fault(fault.node, fault.link_peer, fault.online),
        );
    }
    for (index, message) in messages.iter().enumerate() {
        assert!(message.source < nodes && message.target < nodes && message.generation > 0);
        engine.schedule(message.created_ms, Action::Send(index, message.source, 0));
        engine.schedule(message.created_ms + policy.ttl_ms, Action::Expire(index));
    }
    while let Some(Reverse((at, _, action))) = engine.queue.pop() {
        match action {
            Action::Fault(node, peer, online) => {
                if let Some(peer) = peer {
                    let pair = (node.min(peer), node.max(peer));
                    if online {
                        engine.disabled_links.remove(&pair);
                    } else {
                        engine.disabled_links.insert(pair);
                    }
                } else {
                    engine.online[node] = online;
                }
                engine.result.events.push(Event {
                    sequence: engine.result.events.len() as u64 + 1,
                    at_ms: at,
                    message_id: 0,
                    from: node,
                    to: peer.unwrap_or(node),
                    generation: 0,
                    attempt: 0,
                    outcome: if peer.is_some() && online {
                        "link_restored"
                    } else if peer.is_some() {
                        "link_offline"
                    } else if online {
                        "node_restored"
                    } else {
                        "node_offline"
                    }
                    .into(),
                });
            }
            Action::Expire(index) => {
                if !engine.terminal[index] {
                    engine.drop_message(at, index, "ttl_expired");
                }
            }
            Action::Send(index, current, attempt) => engine.send(at, index, current, attempt),
            Action::Arrive(index, from, to, attempt) => engine.arrive(at, index, from, to, attempt),
        }
    }
    engine.result.latencies.sort_unstable();
    engine.result
}

impl Engine<'_> {
    fn schedule(&mut self, at: u64, action: Action) {
        self.sequence += 1;
        self.queue.push(Reverse((at, self.sequence, action)));
    }

    fn random(&mut self, bound: u64) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng % bound
    }

    fn event(
        &mut self,
        at: u64,
        index: usize,
        from: usize,
        to: usize,
        attempt: usize,
        outcome: &str,
    ) {
        let message = &self.messages[index];
        self.result.events.push(Event {
            sequence: self.result.events.len() as u64 + 1,
            at_ms: at,
            message_id: message.id,
            from,
            to,
            generation: message.generation,
            attempt,
            outcome: outcome.into(),
        });
    }

    fn drop_message(&mut self, at: u64, index: usize, reason: &str) {
        self.terminal[index] = true;
        self.result.dropped += 1;
        let message = &self.messages[index];
        self.event(at, index, message.source, message.target, 0, reason);
    }

    fn next_hop(&self, source: usize, target: usize) -> Option<(usize, usize)> {
        if !self.online[source] || !self.online[target] {
            return None;
        }
        let mut visited = vec![false; self.online.len()];
        let mut search = VecDeque::from([(source, None)]);
        visited[source] = true;
        while let Some((current, first)) = search.pop_front() {
            if current == target {
                return first;
            }
            for &(next, link) in &self.adjacency[current] {
                if self.online[next]
                    && !visited[next]
                    && !self
                        .disabled_links
                        .contains(&(current.min(next), current.max(next)))
                {
                    visited[next] = true;
                    search.push_back((next, first.or(Some((next, link)))));
                }
            }
        }
        None
    }

    fn retry(&mut self, at: u64, index: usize, current: usize, attempt: usize, reason: &str) {
        self.event(at, index, current, current, attempt, reason);
        if attempt < self.policy.retry_limit {
            self.schedule(
                at + self.policy.retry_ms,
                Action::Send(index, current, attempt + 1),
            );
        } else {
            self.drop_message(at, index, "retry_exhausted");
        }
    }

    fn send(&mut self, at: u64, index: usize, current: usize, attempt: usize) {
        if self.terminal[index] {
            return;
        }
        let message = &self.messages[index];
        if at >= message.created_ms + self.policy.ttl_ms {
            self.drop_message(at, index, "ttl_expired");
            return;
        }
        if self.online[message.source] {
            self.result.generations[message.source] =
                self.result.generations[message.source].max(message.generation);
        }
        if current == message.target {
            self.arrive(at, index, current, current, attempt);
            return;
        }
        let Some((next, link_index)) = self.next_hop(current, message.target) else {
            self.retry(at, index, current, attempt, "no_route");
            return;
        };
        let finishes = self.queues.entry((current, next)).or_default();
        while finishes.front().is_some_and(|time| *time <= at) {
            finishes.pop_front();
        }
        if finishes.len() >= self.policy.queue_capacity {
            self.retry(at, index, current, attempt, "queue_full");
            return;
        }
        let finish = finishes.back().copied().unwrap_or(at).max(at)
            + self.policy.packet_bytes.div_ceil(self.policy.bytes_per_ms);
        finishes.push_back(finish);
        self.result.peak_queue = self.result.peak_queue.max(finishes.len());
        let link = &self.links[link_index];
        let arrival = finish
            + link.latency_ms
            + self.random(link.jitter_ms + 1)
            + self.random(self.policy.reorder_ms + 1);
        self.event(at, index, current, next, attempt, "queued");
        // Loss is evaluated at arrival time so faults and other messages share the timeline.
        self.schedule(arrival, Action::Arrive(index, current, next, attempt));
    }

    fn arrive(&mut self, at: u64, index: usize, from: usize, to: usize, attempt: usize) {
        let message = &self.messages[index];
        // Reject an already accepted hop before evaluating loss/retry on its duplicate.
        if self.seen.contains(&(to, message.id)) {
            self.event(at, index, from, to, attempt, "duplicate_rejected");
            return;
        }
        if self.terminal[index] {
            return;
        }
        if at >= message.created_ms + self.policy.ttl_ms {
            self.drop_message(at, index, "ttl_expired");
            return;
        }
        if !self.online[from]
            || !self.online[to]
            || self.disabled_links.contains(&(from.min(to), from.max(to)))
        {
            self.retry(at, index, from, attempt, "link_unavailable");
            return;
        }
        if self.random(10000) < self.policy.loss_basis_points {
            self.retry(at, index, from, attempt, "link_loss");
            return;
        }
        self.seen.insert((to, message.id));
        if self.random(10000) < self.policy.duplicate_basis_points {
            self.schedule(at + 1, Action::Arrive(index, from, to, attempt));
        }
        if to == message.target {
            self.terminal[index] = true;
            self.result.delivered += 1;
            self.result.latencies.push(at - message.created_ms);
            if message.generation < self.result.generations[to] {
                self.event(at, index, from, to, attempt, "stale_generation_rejected");
            } else {
                self.result.generations[to] = message.generation;
                if message.command {
                    self.result.commands_applied += 1;
                }
                self.event(at, index, from, to, attempt, "delivered");
            }
            if self
                .result
                .generations
                .iter()
                .all(|g| *g >= message.generation)
            {
                self.result
                    .converged_at
                    .entry(message.generation)
                    .or_insert(at);
            }
        } else {
            self.event(at, index, from, to, attempt, "forwarded");
            self.schedule(at, Action::Send(index, to, 0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn link() -> Vec<Link> {
        vec![Link {
            left: 0,
            right: 1,
            latency_ms: 5,
            jitter_ms: 0,
        }]
    }
    fn messages() -> Vec<Message> {
        (1..=2)
            .map(|id| Message {
                id,
                source: 0,
                target: 1,
                generation: id,
                created_ms: 0,
                command: true,
            })
            .collect()
    }
    #[test]
    fn bandwidth_serializes_shared_queue_and_measures_convergence() {
        let policy = Policy {
            bytes_per_ms: 1,
            packet_bytes: 10,
            ..Policy::default()
        };
        let result = run(2, &link(), &messages(), &[], &policy, 7);
        assert_eq!(result.latencies, [15, 25]);
        assert_eq!(result.peak_queue, 2);
        assert_eq!(result.commands_applied, 2);
        assert_eq!(result.converged_at[&2], 25);
    }
    #[test]
    fn overflow_ttl_and_retry_limits_are_real_terminal_outcomes() {
        let policy = Policy {
            bytes_per_ms: 1,
            packet_bytes: 100,
            queue_capacity: 1,
            retry_limit: 0,
            ttl_ms: 20,
            ..Policy::default()
        };
        let result = run(2, &link(), &messages(), &[], &policy, 7);
        assert_eq!(
            (result.delivered, result.dropped, result.commands_applied),
            (0, 2, 0)
        );
        for outcome in ["queue_full", "retry_exhausted", "ttl_expired"] {
            assert!(result.events.iter().any(|e| e.outcome == outcome));
        }
        assert_eq!(result.peak_queue, 1);
        let lost = run(
            2,
            &link(),
            &messages()[..1],
            &[],
            &Policy {
                loss_basis_points: 10000,
                retry_limit: 2,
                ..Policy::default()
            },
            7,
        );
        assert_eq!(
            lost.events
                .iter()
                .filter(|e| e.outcome == "link_loss")
                .count(),
            3
        );
        assert_eq!((lost.delivered, lost.dropped), (0, 1));
    }
    #[test]
    fn duplicate_and_reordered_commands_cannot_replay_or_regress_state() {
        let policy = Policy {
            duplicate_basis_points: 10000,
            reorder_ms: 100,
            ..Policy::default()
        };
        let result = run(2, &link(), &messages(), &[], &policy, 1);
        let single = run(2, &link(), &messages()[..1], &[], &policy, 1);
        assert_eq!(single.commands_applied, 1);
        assert_eq!(
            single
                .events
                .iter()
                .filter(|e| e.outcome == "duplicate_rejected")
                .count(),
            1
        );
        assert_eq!(result.generations, [2, 2]);
        assert!(result.commands_applied <= 2);
        assert_eq!(
            result
                .events
                .iter()
                .filter(|e| e.outcome == "duplicate_rejected")
                .count(),
            2
        );
        assert_eq!(result, run(2, &link(), &messages(), &[], &policy, 1));
        assert_ne!(
            result.events,
            run(2, &link(), &messages(), &[], &policy, 2).events
        );
        assert!(
            (1..100).any(|seed| run(2, &link(), &messages(), &[], &policy, seed)
                .events
                .iter()
                .any(|e| e.outcome == "stale_generation_rejected"))
        );
    }
    #[test]
    fn a_cut_link_blocks_in_flight_packets_until_restoration() {
        let faults = [
            Fault {
                at_ms: 1,
                node: 0,
                link_peer: Some(1),
                online: false,
            },
            Fault {
                at_ms: 100,
                node: 0,
                link_peer: Some(1),
                online: true,
            },
        ];
        let result = run(2, &link(), &messages(), &faults, &Policy::default(), 7);
        assert!(result
            .events
            .iter()
            .any(|event| event.outcome == "link_unavailable"));
        assert!(result.converged_at[&2] > 100);
        assert_eq!(result.delivered, 2);
        let isolated = run(2, &link(), &messages(), &faults[..1], &Policy::default(), 7);
        assert_eq!(isolated.delivered, 0);
        assert_eq!(isolated.dropped, 2);
    }

    #[test]
    fn partition_recovery_requires_delivery_after_restoration() {
        let faults = vec![
            Fault {
                at_ms: 0,
                node: 1,
                link_peer: None,
                online: false,
            },
            Fault {
                at_ms: 100,
                node: 1,
                link_peer: None,
                online: true,
            },
        ];
        let result = run(2, &link(), &messages(), &faults, &Policy::default(), 7);
        assert!(result.converged_at[&2] > 100);
        assert_eq!((result.delivered, result.dropped), (2, 0));
        let isolated = run(2, &link(), &messages(), &faults[..1], &Policy::default(), 7);
        assert!(isolated.converged_at.is_empty());
        assert_eq!(isolated.dropped, 2);
    }
}

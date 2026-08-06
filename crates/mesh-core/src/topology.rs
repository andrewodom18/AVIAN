use std::collections::{BTreeMap, BTreeSet, VecDeque};

use thiserror::Error;

use crate::NodeId;

pub const MIN_SUPPORTED_SWARM_SIZE: usize = 5;
/// Software-side bound for explicitly enrolled nodes. This is not a claim that
/// a particular RF profile has been field-validated at this population.
pub const MAX_SUPPORTED_SWARM_SIZE: usize = 1024;
pub const DEFAULT_MAX_NEIGHBORS: usize = 8;

/// Deterministic bounded-degree overlay planning. Every node with the same
/// membership view computes the same symmetric graph; no coordinator or
/// privileged node is selected.
#[derive(Debug, Clone, Copy)]
pub struct TopologyPlanner {
    pub max_neighbors: usize,
}

impl Default for TopologyPlanner {
    fn default() -> Self {
        Self {
            max_neighbors: DEFAULT_MAX_NEIGHBORS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologyPlan {
    neighbors: BTreeMap<NodeId, BTreeSet<NodeId>>,
}

impl TopologyPlanner {
    pub fn plan(&self, members: &[NodeId]) -> Result<TopologyPlan, TopologyError> {
        if !(MIN_SUPPORTED_SWARM_SIZE..=MAX_SUPPORTED_SWARM_SIZE).contains(&members.len()) {
            return Err(TopologyError::UnsupportedSwarmSize(members.len()));
        }
        if self.max_neighbors < 2 || !self.max_neighbors.is_multiple_of(2) {
            return Err(TopologyError::InvalidMaxNeighbors(self.max_neighbors));
        }

        let ordered: Vec<NodeId> = members
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if ordered.len() != members.len() {
            return Err(TopologyError::DuplicateNodeId);
        }

        let mut neighbors: BTreeMap<NodeId, BTreeSet<NodeId>> = ordered
            .iter()
            .cloned()
            .map(|node| (node, BTreeSet::new()))
            .collect();
        let pair_budget = self.max_neighbors / 2;
        let offsets = connection_offsets(ordered.len(), pair_budget);
        for (index, node) in ordered.iter().enumerate() {
            for offset in &offsets {
                let peer = &ordered[(index + offset) % ordered.len()];
                if peer == node {
                    continue;
                }
                neighbors
                    .get_mut(node)
                    .expect("member exists")
                    .insert(peer.clone());
                neighbors
                    .get_mut(peer)
                    .expect("peer exists")
                    .insert(node.clone());
            }
        }

        let plan = TopologyPlan { neighbors };
        if plan.max_degree() > self.max_neighbors {
            return Err(TopologyError::DegreeLimitExceeded);
        }
        Ok(plan)
    }
}

impl TopologyPlan {
    pub fn node_count(&self) -> usize {
        self.neighbors.len()
    }

    pub fn neighbors(&self, node: &NodeId) -> Option<&BTreeSet<NodeId>> {
        self.neighbors.get(node)
    }

    pub fn edge_count(&self) -> usize {
        self.neighbors.values().map(BTreeSet::len).sum::<usize>() / 2
    }

    pub fn max_degree(&self) -> usize {
        self.neighbors
            .values()
            .map(BTreeSet::len)
            .max()
            .unwrap_or(0)
    }

    pub fn is_connected(&self) -> bool {
        self.is_connected_without(&BTreeSet::new())
    }

    pub fn is_connected_without(&self, offline: &BTreeSet<NodeId>) -> bool {
        let Some(start) = self.neighbors.keys().find(|node| !offline.contains(*node)) else {
            return true;
        };
        let mut visited = BTreeSet::from([start.clone()]);
        let mut queue = VecDeque::from([start.clone()]);
        while let Some(node) = queue.pop_front() {
            for peer in self.neighbors.get(&node).into_iter().flatten() {
                if !offline.contains(peer) && visited.insert(peer.clone()) {
                    queue.push_back(peer.clone());
                }
            }
        }
        visited.len() == self.node_count().saturating_sub(offline.len())
    }

    pub fn diameter(&self) -> usize {
        self.neighbors
            .keys()
            .map(|start| self.farthest_distance(start))
            .max()
            .unwrap_or(0)
    }

    fn farthest_distance(&self, start: &NodeId) -> usize {
        let mut distance = BTreeMap::from([(start.clone(), 0_usize)]);
        let mut queue = VecDeque::from([start.clone()]);
        while let Some(node) = queue.pop_front() {
            let next_distance = distance[&node] + 1;
            for peer in self.neighbors.get(&node).into_iter().flatten() {
                if !distance.contains_key(peer) {
                    distance.insert(peer.clone(), next_distance);
                    queue.push_back(peer.clone());
                }
            }
        }
        distance.values().copied().max().unwrap_or(0)
    }
}

fn connection_offsets(node_count: usize, pair_budget: usize) -> BTreeSet<usize> {
    // Balanced mixed-radix offsets keep diameter bounded as explicit
    // enrollment grows. Using only n/2, n/4, ... leaves a long unit-step
    // residual at 1024 nodes.
    let radix = (node_count as f64)
        .powf(1.0 / pair_budget as f64)
        .ceil()
        .max(2.0) as usize;
    let mut offsets = BTreeSet::new();
    let mut offset = 1_usize;
    for _ in 0..pair_budget {
        offsets.insert(offset.min(node_count / 2).max(1));
        offset = offset.saturating_mul(radix);
    }
    let mut fallback = 2_usize;
    while offsets.len() < pair_budget && fallback <= node_count / 2 {
        offsets.insert(fallback);
        fallback += 1;
    }
    offsets
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TopologyError {
    #[error("AVIAN supports swarms of 5-1024 aircraft, got {0}")]
    UnsupportedSwarmSize(usize),
    #[error("maximum neighbors must be an even value of at least two, got {0}")]
    InvalidMaxNeighbors(usize),
    #[error("swarm membership contains a duplicate node ID")]
    DuplicateNodeId,
    #[error("planned overlay exceeded its peer-degree limit")]
    DegreeLimitExceeded,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members(count: usize) -> Vec<NodeId> {
        (0..count)
            .map(|index| NodeId::from(format!("aircraft-{index:03}")))
            .collect()
    }

    #[test]
    fn scale_profiles_are_connected_and_bounded() {
        for count in [5, 25, 100, 200, 1_024] {
            let plan = TopologyPlanner::default().plan(&members(count)).unwrap();
            assert_eq!(plan.node_count(), count);
            assert!(plan.is_connected(), "{count}-node overlay disconnected");
            assert!(plan.max_degree() <= DEFAULT_MAX_NEIGHBORS);
            assert!(plan.edge_count() <= count * DEFAULT_MAX_NEIGHBORS / 2);
            assert!(plan.diameter() <= 16, "{count}-node diameter too large");
        }
    }

    #[test]
    fn two_hundred_node_overlay_survives_distributed_crashes() {
        let members = members(200);
        let plan = TopologyPlanner::default().plan(&members).unwrap();
        let offline: BTreeSet<NodeId> = members
            .iter()
            .enumerate()
            .filter(|(index, _)| index % 10 == 0)
            .map(|(_, node)| node.clone())
            .collect();
        assert_eq!(offline.len(), 20);
        assert!(plan.is_connected_without(&offline));
    }

    #[test]
    fn removing_any_one_node_does_not_remove_authority() {
        let members = members(25);
        let plan = TopologyPlanner::default().plan(&members).unwrap();
        for node in members {
            assert!(plan.is_connected_without(&BTreeSet::from([node])));
        }
    }

    #[test]
    fn rejects_sizes_outside_contract() {
        for count in [4, 1_025] {
            assert_eq!(
                TopologyPlanner::default().plan(&members(count)),
                Err(TopologyError::UnsupportedSwarmSize(count))
            );
        }
    }

    #[test]
    fn rejects_duplicate_members_and_invalid_degree_limit() {
        let mut duplicate_members = members(5);
        duplicate_members[4] = duplicate_members[0].clone();
        assert_eq!(
            TopologyPlanner::default().plan(&duplicate_members),
            Err(TopologyError::DuplicateNodeId)
        );
        assert_eq!(
            TopologyPlanner { max_neighbors: 3 }.plan(&members(5)),
            Err(TopologyError::InvalidMaxNeighbors(3))
        );
    }
}

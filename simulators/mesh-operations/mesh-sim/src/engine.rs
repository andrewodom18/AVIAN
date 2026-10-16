//! Deterministic model state and convergence.
use crate::SimulationError;
use mesh_core::{DeliveryClass, DeliveryPolicy, MeshPayload, NodeId, NodeProfile};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RecordKey(String);

impl From<&str> for RecordKey {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    logical_clock: u64,
    author: NodeId,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VersionedRecord {
    version: Version,
    pub(crate) payload: MeshPayload,
    pub(crate) expires_at_ms: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct SimNode {
    pub(crate) profile: NodeProfile,
    pub(crate) online: bool,
    pub(crate) records: BTreeMap<RecordKey, VersionedRecord>,
}

#[derive(Debug, Default)]
pub struct SimNetwork {
    pub(crate) nodes: BTreeMap<NodeId, SimNode>,
    pub(crate) links: BTreeMap<(NodeId, NodeId), bool>,
    logical_clock: u64,
    pub(crate) now_ms: u64,
}

impl SimNetwork {
    pub fn add_node(&mut self, profile: NodeProfile) {
        self.nodes.insert(
            profile.node_id.clone(),
            SimNode {
                profile,
                online: true,
                records: BTreeMap::new(),
            },
        );
    }

    pub fn connect(&mut self, left: &NodeId, right: &NodeId) -> Result<(), SimulationError> {
        self.require_node(left)?;
        self.require_node(right)?;
        self.links.insert(ordered_pair(left, right), true);
        Ok(())
    }

    pub fn set_link_enabled(
        &mut self,
        left: &NodeId,
        right: &NodeId,
        enabled: bool,
    ) -> Result<(), SimulationError> {
        let pair = ordered_pair(left, right);
        let link = self
            .links
            .get_mut(&pair)
            .ok_or_else(|| SimulationError::UnknownLink(left.clone(), right.clone()))?;
        *link = enabled;
        Ok(())
    }

    pub fn set_node_online(
        &mut self,
        node_id: &NodeId,
        online: bool,
    ) -> Result<(), SimulationError> {
        self.nodes
            .get_mut(node_id)
            .ok_or_else(|| SimulationError::UnknownNode(node_id.clone()))?
            .online = online;
        Ok(())
    }

    pub fn advance_time(&mut self, milliseconds: u64) {
        self.now_ms = self.now_ms.saturating_add(milliseconds);
    }

    pub fn publish(
        &mut self,
        author: &NodeId,
        key: RecordKey,
        payload: MeshPayload,
        class: DeliveryClass,
    ) -> Result<(), SimulationError> {
        let node = self
            .nodes
            .get_mut(author)
            .ok_or_else(|| SimulationError::UnknownNode(author.clone()))?;
        if !node.online {
            return Err(SimulationError::OfflineNode(author.clone()));
        }
        self.logical_clock = self.logical_clock.saturating_add(1);
        let policy = DeliveryPolicy::for_class(class);
        let expires_at_ms = policy.ttl_ms.map(|ttl| self.now_ms.saturating_add(ttl));
        node.records.insert(
            key,
            VersionedRecord {
                version: Version {
                    logical_clock: self.logical_clock,
                    author: author.clone(),
                },
                payload,
                expires_at_ms,
            },
        );
        Ok(())
    }

    /// Converges every currently connected component. Version ordering is a
    /// deterministic stand-in for network replication during the v0.1 harness.
    pub fn synchronize(&mut self) {
        let components = self.connected_components();
        for component in components {
            let mut merged: BTreeMap<RecordKey, VersionedRecord> = BTreeMap::new();
            for node_id in &component {
                let node = &self.nodes[node_id];
                for (key, record) in &node.records {
                    if record
                        .expires_at_ms
                        .is_some_and(|expires_at| expires_at <= self.now_ms)
                    {
                        continue;
                    }
                    let replace = merged
                        .get(key)
                        .is_none_or(|existing| record.version > existing.version);
                    if replace {
                        merged.insert(key.clone(), record.clone());
                    }
                }
            }
            for node_id in component {
                self.nodes
                    .get_mut(&node_id)
                    .expect("component node exists")
                    .records = merged.clone();
            }
        }
    }

    pub fn payload(&self, node_id: &NodeId, key: &RecordKey) -> Option<&MeshPayload> {
        self.nodes
            .get(node_id)?
            .records
            .get(key)
            .filter(|record| {
                record
                    .expires_at_ms
                    .is_none_or(|expires_at| expires_at > self.now_ms)
            })
            .map(|record| &record.payload)
    }

    pub fn connected_component_count(&self) -> usize {
        self.connected_components().len()
    }

    pub fn node_profile(&self, node_id: &NodeId) -> Option<&NodeProfile> {
        self.nodes.get(node_id).map(|node| &node.profile)
    }

    fn require_node(&self, node_id: &NodeId) -> Result<(), SimulationError> {
        self.nodes
            .contains_key(node_id)
            .then_some(())
            .ok_or_else(|| SimulationError::UnknownNode(node_id.clone()))
    }

    fn connected_components(&self) -> Vec<Vec<NodeId>> {
        let online: BTreeSet<NodeId> = self
            .nodes
            .iter()
            .filter(|(_, node)| node.online)
            .map(|(id, _)| id.clone())
            .collect();
        let mut unseen = online.clone();
        let mut components = Vec::new();

        while let Some(start) = unseen.first().cloned() {
            let mut queue = VecDeque::from([start.clone()]);
            let mut component = Vec::new();
            unseen.remove(&start);

            while let Some(current) = queue.pop_front() {
                component.push(current.clone());
                for candidate in &online {
                    if unseen.contains(candidate)
                        && self
                            .links
                            .get(&ordered_pair(&current, candidate))
                            .copied()
                            .unwrap_or(false)
                    {
                        unseen.remove(candidate);
                        queue.push_back(candidate.clone());
                    }
                }
            }
            components.push(component);
        }
        components
    }
}

pub(crate) fn ordered_pair(left: &NodeId, right: &NodeId) -> (NodeId, NodeId) {
    if left <= right {
        (left.clone(), right.clone())
    } else {
        (right.clone(), left.clone())
    }
}

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{NodeId, RelayLinkObservation, Telemetry, TransportKind};

const TELEMETRY_LATEST_TTL_MS: u64 = 2_000;

/// Mission-configurable source and operator-feed traffic limits. The policy
/// applies before a companion inserts a record into PEAT, so it constrains
/// traffic independently of whichever IP underlay is currently available.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SwarmTrafficPolicy {
    /// Minimum interval between routine individual state updates.
    pub routine_telemetry_interval_ms: u64,
    /// Minimum interval for relay/mission-critical state or a persistent
    /// attention condition. This must not be slower than routine telemetry.
    pub priority_telemetry_interval_ms: u64,
    /// Minimum interval between unchanged rolling radio-link observations for
    /// one endpoint pair and underlay. An availability/direction change still
    /// passes immediately.
    pub relay_observation_interval_ms: u64,
    /// Interval used to rotate the bounded set of operator-summary publishers.
    pub operator_summary_interval_ms: u64,
    /// Maximum number of concurrent summary publishers. They are rotated; this
    /// does not create a permanent gateway or mother drone.
    pub operator_summary_replicas: usize,
    /// Position age at which an aircraft is counted as stale in an operator
    /// summary. It does not declare an aircraft lost.
    pub operator_summary_max_age_ms: u64,
    /// Optional normalized battery threshold included in compact summaries.
    pub low_battery_threshold: Option<f32>,
    /// Bound on identifiers carried in each attention list.
    pub max_attention_members: usize,
}

impl Default for SwarmTrafficPolicy {
    fn default() -> Self {
        Self {
            routine_telemetry_interval_ms: 2_000,
            priority_telemetry_interval_ms: 500,
            relay_observation_interval_ms: 500,
            operator_summary_interval_ms: 1_000,
            operator_summary_replicas: 3,
            operator_summary_max_age_ms: 6_000,
            low_battery_threshold: Some(0.20),
            max_attention_members: 16,
        }
    }
}

impl SwarmTrafficPolicy {
    pub fn validate(self) -> Result<(), TrafficPolicyError> {
        if self.routine_telemetry_interval_ms == 0
            || self.priority_telemetry_interval_ms == 0
            || self.relay_observation_interval_ms == 0
            || self.operator_summary_interval_ms == 0
            || self.operator_summary_max_age_ms == 0
            || self.operator_summary_replicas == 0
            || self.max_attention_members == 0
            || self.priority_telemetry_interval_ms > self.routine_telemetry_interval_ms
            || self.operator_summary_interval_ms > TELEMETRY_LATEST_TTL_MS
            || self.operator_summary_max_age_ms < self.routine_telemetry_interval_ms
            || self
                .low_battery_threshold
                .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
        {
            return Err(TrafficPolicyError::InvalidPolicy);
        }
        Ok(())
    }

    /// Chooses a bounded rotating set of summary publishers from a shared
    /// membership view. Each window advances the contiguous slot, which keeps
    /// selection deterministic without a leader or a fixed gateway role.
    pub fn summary_publishers(
        self,
        members: &[NodeId],
        observed_at_ms: u64,
    ) -> Result<Vec<NodeId>, TrafficPolicyError> {
        self.validate()?;
        let members: Vec<NodeId> = members
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if members.is_empty() {
            return Err(TrafficPolicyError::EmptyMembership);
        }
        let count = self.operator_summary_replicas.min(members.len());
        let window = observed_at_ms / self.operator_summary_interval_ms;
        let start = (window % members.len() as u64) as usize;
        Ok((0..count)
            .map(|offset| members[(start + offset) % members.len()].clone())
            .collect())
    }

    pub fn is_summary_publisher(
        self,
        members: &[NodeId],
        local: &NodeId,
        observed_at_ms: u64,
    ) -> Result<bool, TrafficPolicyError> {
        Ok(self
            .summary_publishers(members, observed_at_ms)?
            .contains(local))
    }

    fn attention_state(self, telemetry: &Telemetry) -> AttentionState {
        AttentionState {
            failsafe: telemetry.failsafe,
            armed: telemetry.armed,
            landed: telemetry.landed,
            low_battery: self.low_battery_threshold.is_some_and(|threshold| {
                telemetry
                    .battery_remaining
                    .is_some_and(|remaining| remaining <= threshold)
            }),
        }
    }
}

/// Whether an individual state record is due for publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryPublication {
    Suppress,
    Routine,
    Priority,
    AttentionChange,
}

/// Local, non-replicated rate-limit state for one companion.
#[derive(Debug, Clone, Default)]
pub struct TelemetryTrafficGovernor {
    last_published_at_ms: Option<u64>,
    last_attention: Option<AttentionState>,
}

/// Source-side latest-value limiter for local radio collectors. Link-state
/// transitions bypass the interval, while repeated rolling samples overwrite
/// no more often than the mission policy allows.
#[derive(Debug, Clone, Default)]
pub struct RelayObservationTrafficGovernor {
    published: BTreeMap<(NodeId, NodeId, TransportKind), ObservationPublicationState>,
}

impl RelayObservationTrafficGovernor {
    pub fn decide(
        &mut self,
        policy: SwarmTrafficPolicy,
        observation: &RelayLinkObservation,
        observed_at_ms: u64,
    ) -> Result<RelayObservationPublication, TrafficPolicyError> {
        policy.validate()?;
        let (first, second) = if observation.first <= observation.second {
            (observation.first.clone(), observation.second.clone())
        } else {
            (observation.second.clone(), observation.first.clone())
        };
        let key = (first, second, observation.transport);
        let state = ObservationPublicationState {
            published_at_ms: observed_at_ms,
            available: observation.available,
            bidirectional: observation.bidirectional,
        };
        let Some(previous) = self.published.get(&key).copied() else {
            self.published.insert(key, state);
            return Ok(RelayObservationPublication::Updated);
        };
        let state_changed =
            previous.available != state.available || previous.bidirectional != state.bidirectional;
        let due = observed_at_ms.saturating_sub(previous.published_at_ms)
            >= policy.relay_observation_interval_ms;
        if !state_changed && !due {
            return Ok(RelayObservationPublication::Suppress);
        }
        self.published.insert(key, state);
        Ok(if state_changed {
            RelayObservationPublication::StateChange
        } else {
            RelayObservationPublication::Updated
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayObservationPublication {
    Suppress,
    Updated,
    StateChange,
}

#[derive(Debug, Clone, Copy)]
struct ObservationPublicationState {
    published_at_ms: u64,
    available: bool,
    bidirectional: bool,
}

impl TelemetryTrafficGovernor {
    pub fn decide(
        &mut self,
        policy: SwarmTrafficPolicy,
        telemetry: &Telemetry,
        observed_at_ms: u64,
        mission_critical: bool,
    ) -> Result<TelemetryPublication, TrafficPolicyError> {
        policy.validate()?;
        let attention = policy.attention_state(telemetry);
        let attention_changed = self
            .last_attention
            .is_some_and(|previous| previous != attention);
        let interval_ms = if mission_critical || attention.requires_priority() {
            policy.priority_telemetry_interval_ms
        } else {
            policy.routine_telemetry_interval_ms
        };
        let due = self
            .last_published_at_ms
            .is_none_or(|last| observed_at_ms.saturating_sub(last) >= interval_ms);
        if !due && !attention_changed {
            return Ok(TelemetryPublication::Suppress);
        }

        self.last_published_at_ms = Some(observed_at_ms);
        self.last_attention = Some(attention);
        Ok(if attention_changed {
            TelemetryPublication::AttentionChange
        } else if mission_critical || attention.requires_priority() {
            TelemetryPublication::Priority
        } else {
            TelemetryPublication::Routine
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AttentionState {
    failsafe: bool,
    armed: bool,
    landed: Option<bool>,
    low_battery: bool,
}

impl AttentionState {
    fn requires_priority(self) -> bool {
        self.failsafe || self.low_battery
    }
}

/// Compact operator-facing state. The payload intentionally contains counts
/// and bounded attention identifiers, not a full per-aircraft position feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmStatusSummary {
    pub publisher: NodeId,
    pub observed_at_ms: u64,
    pub membership_generation: u64,
    pub configured_members: usize,
    pub fresh_members: usize,
    pub stale_members: usize,
    pub failsafe_members: Vec<NodeId>,
    pub low_battery_members: Vec<NodeId>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TrafficPolicyError {
    #[error("swarm traffic policy has invalid intervals, replica count, or thresholds")]
    InvalidPolicy,
    #[error("summary publisher selection requires at least one member")]
    EmptyMembership,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Altitude;

    fn telemetry() -> Telemetry {
        Telemetry {
            source: "aircraft-a".into(),
            timestamp_ms: 0,
            latitude_deg: 0.0,
            longitude_deg: 0.0,
            altitude: Altitude::with_optional_agl(100.0, None, 0.0).unwrap(),
            velocity_ned_mps: [0.0; 3],
            attitude_rpy_deg: [0.0; 3],
            battery_remaining: Some(0.8),
            control_link_quality: None,
            armed: true,
            landed: Some(false),
            failsafe: false,
        }
    }

    #[test]
    fn routine_telemetry_is_source_rate_limited_but_attention_changes_pass() {
        let policy = SwarmTrafficPolicy::default();
        let mut governor = TelemetryTrafficGovernor::default();
        let mut state = telemetry();

        assert_eq!(
            governor.decide(policy, &state, 0, false),
            Ok(TelemetryPublication::Routine)
        );
        assert_eq!(
            governor.decide(policy, &state, 1_000, false),
            Ok(TelemetryPublication::Suppress)
        );
        state.failsafe = true;
        assert_eq!(
            governor.decide(policy, &state, 1_001, false),
            Ok(TelemetryPublication::AttentionChange)
        );
    }

    #[test]
    fn summary_publishers_are_bounded_and_rotate_without_a_leader() {
        let members = (0..5)
            .map(|index| NodeId::from(format!("aircraft-{index}")))
            .collect::<Vec<_>>();
        let policy = SwarmTrafficPolicy::default();

        let first = policy.summary_publishers(&members, 0).unwrap();
        let next = policy
            .summary_publishers(&members, policy.operator_summary_interval_ms)
            .unwrap();

        assert_eq!(first.len(), 3);
        assert_eq!(next.len(), 3);
        assert_ne!(first, next);
        assert!(first.iter().all(|member| members.contains(member)));
    }

    #[test]
    fn unchanged_radio_observations_are_limited_but_state_changes_pass() {
        let policy = SwarmTrafficPolicy::default();
        let mut governor = RelayObservationTrafficGovernor::default();
        let mut observation = RelayLinkObservation {
            first: "left".into(),
            second: "right".into(),
            transport: TransportKind::Silvus,
            observed_at_ms: 0,
            sample_window_ms: 100,
            bidirectional: true,
            available: true,
            metrics: crate::LinkMetrics {
                latency_ms: 10.0,
                loss_ratio: 0.0,
                goodput_bps: 1_000_000.0,
                signal_quality: 0.9,
                stability: 0.9,
                energy_cost: 0.1,
            },
            geometry: crate::LinkGeometry {
                distance_m: 100.0,
                line_of_sight: true,
                fresnel_clearance_ratio: 0.9,
            },
            received_power_dbm: None,
            link_margin_db: None,
        };

        assert_eq!(
            governor.decide(policy, &observation, 0),
            Ok(RelayObservationPublication::Updated)
        );
        assert_eq!(
            governor.decide(policy, &observation, 100),
            Ok(RelayObservationPublication::Suppress)
        );
        observation.available = false;
        assert_eq!(
            governor.decide(policy, &observation, 101),
            Ok(RelayObservationPublication::StateChange)
        );
    }
}

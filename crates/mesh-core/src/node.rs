use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::SYSTEM_MAX_MSL_M;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(String);

impl NodeId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<&str> for NodeId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for NodeId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeRole {
    Aircraft,
    Ground,
    Cloud,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlightStack {
    ArduPilot,
    Px4,
    Betaflight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Telemetry,
    EmergencyControl,
    MissionNavigation,
    PayloadTasking,
    MeshRelay,
    MissionCoordination,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeProfile {
    pub node_id: NodeId,
    pub role: NodeRole,
    pub flight_stack: Option<FlightStack>,
    pub capabilities: BTreeSet<Capability>,
    pub platform_max_msl_m: Option<f64>,
}

impl NodeProfile {
    pub fn aircraft(
        node_id: impl Into<NodeId>,
        flight_stack: FlightStack,
        platform_max_msl_m: f64,
    ) -> Result<Self, ProfileError> {
        if !platform_max_msl_m.is_finite()
            || platform_max_msl_m <= 0.0
            || platform_max_msl_m > SYSTEM_MAX_MSL_M
        {
            return Err(ProfileError::InvalidPlatformCeiling(platform_max_msl_m));
        }

        let mut capabilities = BTreeSet::from([
            Capability::Telemetry,
            Capability::EmergencyControl,
            Capability::MeshRelay,
        ]);
        if matches!(flight_stack, FlightStack::ArduPilot | FlightStack::Px4) {
            capabilities.insert(Capability::MissionNavigation);
            capabilities.insert(Capability::PayloadTasking);
        }

        Ok(Self {
            node_id: node_id.into(),
            role: NodeRole::Aircraft,
            flight_stack: Some(flight_stack),
            capabilities,
            platform_max_msl_m: Some(platform_max_msl_m),
        })
    }

    pub fn ground(node_id: impl Into<NodeId>) -> Self {
        Self {
            node_id: node_id.into(),
            role: NodeRole::Ground,
            flight_stack: None,
            capabilities: BTreeSet::from([
                Capability::Telemetry,
                Capability::EmergencyControl,
                Capability::MissionCoordination,
                Capability::MeshRelay,
            ]),
            platform_max_msl_m: None,
        }
    }

    pub fn supports(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum ProfileError {
    #[error("platform MSL ceiling must be in (0, 7,620] m, got {0}")]
    InvalidPlatformCeiling(f64),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn betaflight_is_capability_gated() {
        let profile = NodeProfile::aircraft("beta-1", FlightStack::Betaflight, 1_000.0).unwrap();

        assert!(profile.supports(Capability::Telemetry));
        assert!(profile.supports(Capability::EmergencyControl));
        assert!(!profile.supports(Capability::MissionNavigation));
        assert!(!profile.supports(Capability::PayloadTasking));
    }

    #[test]
    fn full_flight_stacks_advertise_navigation() {
        for stack in [FlightStack::ArduPilot, FlightStack::Px4] {
            let profile = NodeProfile::aircraft("aircraft", stack, SYSTEM_MAX_MSL_M).unwrap();
            assert!(profile.supports(Capability::MissionNavigation));
        }
    }
}

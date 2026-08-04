//! Flight-controller boundary for MAVLink and MSP implementations.

use async_trait::async_trait;
use mesh_core::{
    EmergencyAction, FlightStack, NodeProfile, ProfileError, Telemetry, SYSTEM_MAX_MSL_M,
};
use thiserror::Error;
use tokio::sync::{Mutex, RwLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmergencyExecution {
    pub action: EmergencyAction,
    /// The semantic native action; the physical adapters will translate this
    /// into MAVLink or MSP messages.
    pub native_action: &'static str,
}

#[async_trait]
pub trait VehicleAdapter: Send + Sync {
    fn profile(&self) -> &NodeProfile;
    async fn telemetry(&self) -> Result<Telemetry, AdapterError>;
    async fn execute_emergency(
        &self,
        action: EmergencyAction,
    ) -> Result<EmergencyExecution, AdapterError>;
}

/// Deterministic adapter used by the v0.1 simulator. It enforces the same
/// capability policy as the future MAVLink/MSP adapters.
pub struct SimulatedVehicleAdapter {
    profile: NodeProfile,
    telemetry: RwLock<Telemetry>,
    executions: Mutex<Vec<EmergencyExecution>>,
}

impl SimulatedVehicleAdapter {
    pub fn new(profile: NodeProfile, initial_telemetry: Telemetry) -> Result<Self, AdapterError> {
        if profile.node_id != initial_telemetry.source {
            return Err(AdapterError::TelemetrySourceMismatch);
        }
        Ok(Self {
            profile,
            telemetry: RwLock::new(initial_telemetry),
            executions: Mutex::new(Vec::new()),
        })
    }

    pub async fn update_telemetry(&self, telemetry: Telemetry) -> Result<(), AdapterError> {
        if telemetry.source != self.profile.node_id {
            return Err(AdapterError::TelemetrySourceMismatch);
        }
        *self.telemetry.write().await = telemetry;
        Ok(())
    }

    pub async fn executions(&self) -> Vec<EmergencyExecution> {
        self.executions.lock().await.clone()
    }
}

#[async_trait]
impl VehicleAdapter for SimulatedVehicleAdapter {
    fn profile(&self) -> &NodeProfile {
        &self.profile
    }

    async fn telemetry(&self) -> Result<Telemetry, AdapterError> {
        Ok(self.telemetry.read().await.clone())
    }

    async fn execute_emergency(
        &self,
        action: EmergencyAction,
    ) -> Result<EmergencyExecution, AdapterError> {
        let stack = self
            .profile
            .flight_stack
            .ok_or(AdapterError::MissingFlightStack)?;
        let telemetry = self.telemetry.read().await;
        let native_action = match (stack, action) {
            (FlightStack::Betaflight, EmergencyAction::GpsRescue)
            | (FlightStack::Betaflight, EmergencyAction::ReturnToLaunch) => "gps_rescue",
            (FlightStack::Betaflight, EmergencyAction::Disarm)
                if telemetry.landed == Some(true) =>
            {
                "disarm"
            }
            (FlightStack::Betaflight, EmergencyAction::Disarm) => {
                return Err(AdapterError::UnsafeAirborneDisarm)
            }
            (FlightStack::Betaflight, EmergencyAction::Land) => {
                return Err(AdapterError::UnsupportedAction { stack, action })
            }
            (FlightStack::ArduPilot | FlightStack::Px4, EmergencyAction::GpsRescue)
            | (FlightStack::ArduPilot | FlightStack::Px4, EmergencyAction::ReturnToLaunch) => {
                "return_to_launch"
            }
            (FlightStack::ArduPilot | FlightStack::Px4, EmergencyAction::Land) => "land",
            (FlightStack::ArduPilot | FlightStack::Px4, EmergencyAction::Disarm)
                if telemetry.landed == Some(true) =>
            {
                "disarm"
            }
            (FlightStack::ArduPilot | FlightStack::Px4, EmergencyAction::Disarm) => {
                return Err(AdapterError::UnsafeAirborneDisarm)
            }
        };
        drop(telemetry);

        let execution = EmergencyExecution {
            action,
            native_action,
        };
        self.executions.lock().await.push(execution.clone());
        Ok(execution)
    }
}

pub fn reference_profile(node_id: &str, stack: FlightStack) -> Result<NodeProfile, ProfileError> {
    NodeProfile::aircraft(node_id, stack, SYSTEM_MAX_MSL_M)
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum AdapterError {
    #[error("telemetry source does not match adapter node")]
    TelemetrySourceMismatch,
    #[error("aircraft profile does not specify a flight stack")]
    MissingFlightStack,
    #[error("{action:?} is not supported by {stack:?}")]
    UnsupportedAction {
        stack: FlightStack,
        action: EmergencyAction,
    },
    #[error("mesh disarm is rejected until the vehicle reports landed")]
    UnsafeAirborneDisarm,
}

#[cfg(test)]
mod tests {
    use mesh_core::{Altitude, NodeId};

    use super::*;

    fn telemetry(node: &str, landed: bool) -> Telemetry {
        Telemetry {
            source: NodeId::from(node),
            timestamp_ms: 1,
            latitude_deg: 0.0,
            longitude_deg: 0.0,
            altitude: Altitude::new(100.0, 50.0, 50.0).unwrap(),
            velocity_ned_mps: [0.0; 3],
            attitude_rpy_deg: [0.0; 3],
            battery_remaining: Some(0.8),
            control_link_quality: Some(0.9),
            armed: !landed,
            landed: Some(landed),
            failsafe: false,
        }
    }

    #[tokio::test]
    async fn betaflight_maps_rtl_to_gps_rescue() {
        let adapter = SimulatedVehicleAdapter::new(
            reference_profile("beta", FlightStack::Betaflight).unwrap(),
            telemetry("beta", false),
        )
        .unwrap();

        let result = adapter
            .execute_emergency(EmergencyAction::ReturnToLaunch)
            .await
            .unwrap();
        assert_eq!(result.native_action, "gps_rescue");
    }

    #[tokio::test]
    async fn airborne_disarm_is_rejected() {
        let adapter = SimulatedVehicleAdapter::new(
            reference_profile("beta", FlightStack::Betaflight).unwrap(),
            telemetry("beta", false),
        )
        .unwrap();

        assert_eq!(
            adapter.execute_emergency(EmergencyAction::Disarm).await,
            Err(AdapterError::UnsafeAirborneDisarm)
        );
    }
}

use mavlink::dialects::common::{MavAutopilot, MavLandedState, MavMessage, MavModeFlag, MavState};
use mavlink::MavHeader;
use mesh_core::{Altitude, FlightStack, NodeId, Telemetry};

use crate::AdapterError;

#[derive(Debug, Clone, Copy)]
struct HeartbeatState {
    armed: bool,
    failsafe: bool,
}

#[derive(Debug, Clone, Copy)]
struct PositionState {
    latitude_deg: f64,
    longitude_deg: f64,
    msl_m: f64,
    above_launch_m: f64,
    velocity_ned_mps: [f32; 3],
}

/// Combines the independent MAVLink telemetry streams emitted by ArduPilot
/// and PX4 into AVIAN's datum-safe common telemetry record.
pub struct MavlinkTelemetryAccumulator {
    source: NodeId,
    expected_stack: FlightStack,
    target_system_id: Option<u8>,
    heartbeat: Option<HeartbeatState>,
    position: Option<PositionState>,
    attitude_rpy_deg: Option<[f32; 3]>,
    agl_m: Option<f64>,
    battery_remaining: Option<f32>,
    control_link_quality: Option<f32>,
    landed: Option<bool>,
}

impl MavlinkTelemetryAccumulator {
    pub fn new(
        source: impl Into<NodeId>,
        expected_stack: FlightStack,
    ) -> Result<Self, AdapterError> {
        if expected_stack == FlightStack::Betaflight {
            return Err(AdapterError::UnsupportedMavlinkStack(expected_stack));
        }
        Ok(Self {
            source: source.into(),
            expected_stack,
            target_system_id: None,
            heartbeat: None,
            position: None,
            attitude_rpy_deg: None,
            agl_m: None,
            battery_remaining: None,
            control_link_quality: None,
            landed: None,
        })
    }

    pub fn target_system_id(&self) -> Option<u8> {
        self.target_system_id
    }

    /// Applies one decoded MAVLink message. Messages from other MAVLink system
    /// IDs are ignored after the flight controller has been identified.
    pub fn ingest(
        &mut self,
        header: MavHeader,
        message: &MavMessage,
        received_at_ms: u64,
    ) -> Result<Option<Telemetry>, AdapterError> {
        if let MavMessage::HEARTBEAT(data) = message {
            let Some(reported_stack) = stack_for_autopilot(data.autopilot) else {
                return Ok(None);
            };
            if reported_stack != self.expected_stack {
                return Err(AdapterError::UnexpectedFlightStack {
                    expected: self.expected_stack,
                    reported: reported_stack,
                });
            }
            if self
                .target_system_id
                .is_some_and(|target| target != header.system_id)
            {
                return Ok(None);
            }
            self.target_system_id = Some(header.system_id);
            self.heartbeat = Some(HeartbeatState {
                armed: data
                    .base_mode
                    .contains(MavModeFlag::MAV_MODE_FLAG_SAFETY_ARMED),
                failsafe: matches!(
                    data.system_status,
                    MavState::MAV_STATE_CRITICAL
                        | MavState::MAV_STATE_EMERGENCY
                        | MavState::MAV_STATE_FLIGHT_TERMINATION
                ),
            });
            return self.telemetry(received_at_ms);
        }

        if self.target_system_id != Some(header.system_id) {
            return Ok(None);
        }

        match message {
            MavMessage::GLOBAL_POSITION_INT(data) => {
                let latitude_deg = f64::from(data.lat) / 10_000_000.0;
                let longitude_deg = f64::from(data.lon) / 10_000_000.0;
                if !(-90.0..=90.0).contains(&latitude_deg)
                    || !(-180.0..=180.0).contains(&longitude_deg)
                {
                    return Err(AdapterError::InvalidMavlinkPosition);
                }
                self.position = Some(PositionState {
                    latitude_deg,
                    longitude_deg,
                    msl_m: f64::from(data.alt) / 1_000.0,
                    above_launch_m: f64::from(data.relative_alt) / 1_000.0,
                    velocity_ned_mps: [
                        f32::from(data.vx) / 100.0,
                        f32::from(data.vy) / 100.0,
                        f32::from(data.vz) / 100.0,
                    ],
                });
            }
            MavMessage::ATTITUDE(data) => {
                self.attitude_rpy_deg = Some([
                    data.roll.to_degrees(),
                    data.pitch.to_degrees(),
                    data.yaw.to_degrees(),
                ]);
            }
            MavMessage::ALTITUDE(data) => {
                self.agl_m = (data.altitude_terrain.is_finite() && data.altitude_terrain >= 0.0)
                    .then_some(f64::from(data.altitude_terrain));
            }
            MavMessage::SYS_STATUS(data) => {
                self.battery_remaining = (0..=100)
                    .contains(&data.battery_remaining)
                    .then_some(f32::from(data.battery_remaining) / 100.0);
            }
            MavMessage::RC_CHANNELS(data) => {
                self.control_link_quality = normalized_rssi(data.rssi);
            }
            MavMessage::RADIO_STATUS(data) => {
                self.control_link_quality = normalized_rssi(data.rssi);
            }
            MavMessage::EXTENDED_SYS_STATE(data) => {
                self.landed = match data.landed_state {
                    MavLandedState::MAV_LANDED_STATE_ON_GROUND => Some(true),
                    MavLandedState::MAV_LANDED_STATE_IN_AIR
                    | MavLandedState::MAV_LANDED_STATE_TAKEOFF
                    | MavLandedState::MAV_LANDED_STATE_LANDING => Some(false),
                    MavLandedState::MAV_LANDED_STATE_UNDEFINED => None,
                };
            }
            _ => return Ok(None),
        }

        self.telemetry(received_at_ms)
    }

    fn telemetry(&self, timestamp_ms: u64) -> Result<Option<Telemetry>, AdapterError> {
        let (Some(heartbeat), Some(position), Some(attitude_rpy_deg)) =
            (self.heartbeat, self.position, self.attitude_rpy_deg)
        else {
            return Ok(None);
        };
        let altitude =
            Altitude::with_optional_agl(position.msl_m, self.agl_m, position.above_launch_m)?;
        Ok(Some(Telemetry {
            source: self.source.clone(),
            timestamp_ms,
            latitude_deg: position.latitude_deg,
            longitude_deg: position.longitude_deg,
            altitude,
            velocity_ned_mps: position.velocity_ned_mps,
            attitude_rpy_deg,
            battery_remaining: self.battery_remaining,
            control_link_quality: self.control_link_quality,
            armed: heartbeat.armed,
            landed: self.landed,
            failsafe: heartbeat.failsafe,
        }))
    }
}

fn stack_for_autopilot(autopilot: MavAutopilot) -> Option<FlightStack> {
    match autopilot {
        MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA => Some(FlightStack::ArduPilot),
        MavAutopilot::MAV_AUTOPILOT_PX4 => Some(FlightStack::Px4),
        _ => None,
    }
}

fn normalized_rssi(value: u8) -> Option<f32> {
    (value != u8::MAX).then_some(f32::from(value) / 254.0)
}

#[cfg(test)]
mod tests {
    use std::f32::consts::{FRAC_PI_2, PI};

    use mavlink::dialects::common::{
        ALTITUDE_DATA, ATTITUDE_DATA, EXTENDED_SYS_STATE_DATA, GLOBAL_POSITION_INT_DATA,
        HEARTBEAT_DATA, RC_CHANNELS_DATA, SYS_STATUS_DATA,
    };

    use super::*;

    fn header(system_id: u8) -> MavHeader {
        MavHeader {
            system_id,
            component_id: 1,
            sequence: 0,
        }
    }

    fn heartbeat(autopilot: MavAutopilot) -> MavMessage {
        MavMessage::HEARTBEAT(HEARTBEAT_DATA {
            autopilot,
            base_mode: MavModeFlag::MAV_MODE_FLAG_SAFETY_ARMED,
            system_status: MavState::MAV_STATE_ACTIVE,
            ..HEARTBEAT_DATA::default()
        })
    }

    fn position() -> MavMessage {
        MavMessage::GLOBAL_POSITION_INT(GLOBAL_POSITION_INT_DATA {
            lat: 350_000_000,
            lon: -1_060_000_000,
            alt: 2_000_000,
            relative_alt: 450_000,
            vx: 1_000,
            vy: -250,
            vz: 50,
            ..GLOBAL_POSITION_INT_DATA::default()
        })
    }

    fn attitude() -> MavMessage {
        MavMessage::ATTITUDE(ATTITUDE_DATA {
            roll: 0.0,
            pitch: FRAC_PI_2,
            yaw: PI,
            ..ATTITUDE_DATA::default()
        })
    }

    #[test]
    fn combines_ardupilot_common_messages_without_inventing_agl() {
        let mut accumulator =
            MavlinkTelemetryAccumulator::new("ardu-1", FlightStack::ArduPilot).unwrap();
        assert!(accumulator
            .ingest(
                header(1),
                &heartbeat(MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA),
                1_000,
            )
            .unwrap()
            .is_none());
        assert!(accumulator
            .ingest(header(1), &position(), 1_010)
            .unwrap()
            .is_none());
        let telemetry = accumulator
            .ingest(header(1), &attitude(), 1_020)
            .unwrap()
            .unwrap();

        assert_eq!(telemetry.latitude_deg, 35.0);
        assert_eq!(telemetry.longitude_deg, -106.0);
        assert_eq!(telemetry.altitude.msl_m, 2_000.0);
        assert_eq!(telemetry.altitude.above_launch_m, 450.0);
        assert_eq!(telemetry.altitude.agl_m, None);
        assert_eq!(telemetry.velocity_ned_mps, [10.0, -2.5, 0.5]);
        assert_eq!(telemetry.attitude_rpy_deg, [0.0, 90.0, 180.0]);
        assert_eq!(telemetry.battery_remaining, None);
        assert_eq!(telemetry.control_link_quality, None);
        assert_eq!(telemetry.landed, None);
        assert!(telemetry.armed);
    }

    #[test]
    fn applies_optional_health_and_altitude_fields() {
        let mut accumulator = MavlinkTelemetryAccumulator::new("px4-1", FlightStack::Px4).unwrap();
        accumulator
            .ingest(
                header(7),
                &heartbeat(MavAutopilot::MAV_AUTOPILOT_PX4),
                1_000,
            )
            .unwrap();
        accumulator.ingest(header(7), &position(), 1_010).unwrap();
        accumulator.ingest(header(7), &attitude(), 1_020).unwrap();
        accumulator
            .ingest(
                header(7),
                &MavMessage::ALTITUDE(ALTITUDE_DATA {
                    altitude_terrain: 123.5,
                    ..ALTITUDE_DATA::default()
                }),
                1_030,
            )
            .unwrap();
        accumulator
            .ingest(
                header(7),
                &MavMessage::SYS_STATUS(SYS_STATUS_DATA {
                    battery_remaining: 76,
                    ..SYS_STATUS_DATA::default()
                }),
                1_040,
            )
            .unwrap();
        accumulator
            .ingest(
                header(7),
                &MavMessage::RC_CHANNELS(RC_CHANNELS_DATA {
                    rssi: 127,
                    ..RC_CHANNELS_DATA::default()
                }),
                1_050,
            )
            .unwrap();
        let telemetry = accumulator
            .ingest(
                header(7),
                &MavMessage::EXTENDED_SYS_STATE(EXTENDED_SYS_STATE_DATA {
                    landed_state: MavLandedState::MAV_LANDED_STATE_IN_AIR,
                    ..EXTENDED_SYS_STATE_DATA::default()
                }),
                1_060,
            )
            .unwrap()
            .unwrap();

        assert_eq!(telemetry.altitude.agl_m, Some(123.5));
        assert_eq!(telemetry.battery_remaining, Some(0.76));
        assert_eq!(telemetry.control_link_quality, Some(0.5));
        assert_eq!(telemetry.landed, Some(false));
    }

    #[test]
    fn ignores_other_systems_after_flight_controller_lock() {
        let mut accumulator = MavlinkTelemetryAccumulator::new("px4-1", FlightStack::Px4).unwrap();
        accumulator
            .ingest(
                header(7),
                &heartbeat(MavAutopilot::MAV_AUTOPILOT_PX4),
                1_000,
            )
            .unwrap();
        assert!(accumulator
            .ingest(header(8), &position(), 1_010)
            .unwrap()
            .is_none());
        assert_eq!(accumulator.target_system_id(), Some(7));
    }

    #[test]
    fn rejects_wrong_flight_stack() {
        let mut accumulator = MavlinkTelemetryAccumulator::new("px4-1", FlightStack::Px4).unwrap();
        assert_eq!(
            accumulator.ingest(
                header(1),
                &heartbeat(MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA),
                1_000,
            ),
            Err(AdapterError::UnexpectedFlightStack {
                expected: FlightStack::Px4,
                reported: FlightStack::ArduPilot,
            })
        );
    }
}

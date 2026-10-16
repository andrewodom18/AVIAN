use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mavlink::dialects::common::{
    MavAutopilot, MavCmd, MavLandedState, MavMessage, MavModeFlag, MavResult, MavState,
    COMMAND_LONG_DATA,
};
use mavlink::error::MessageReadError;
use mavlink::{MavConnection, MavHeader};
use mesh_core::{Altitude, FlightStack, NodeId, Telemetry};
use tokio::sync::{mpsc, oneshot, watch};

use crate::AdapterError;

#[derive(Debug, Clone)]
pub struct MavlinkSourceConfig {
    pub address: String,
    pub source: NodeId,
    pub expected_stack: FlightStack,
    pub reconnect_delay: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MavlinkTelemetryEvent {
    Connected,
    SystemLocked(u8),
    Telemetry(Telemetry),
    ConnectionLost(String),
    Rejected(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MavlinkCommandOutcome {
    Accepted,
    Rejected(String),
    TimedOut,
    TransportUnavailable(String),
}

struct MavlinkCommandRequest {
    target_system_id: u8,
    ack_timeout: Duration,
    retries: u8,
    expires_at: std::time::Instant,
    respond_to: oneshot::Sender<MavlinkCommandOutcome>,
}

#[derive(Clone)]
pub struct MavlinkCommandSender {
    sender: mpsc::Sender<MavlinkCommandRequest>,
}

impl MavlinkCommandSender {
    pub async fn return_to_launch(
        &self,
        target_system_id: u8,
        ack_timeout: Duration,
        retries: u8,
    ) -> MavlinkCommandOutcome {
        let budget = ack_timeout.saturating_mul(u32::from(retries) + 1);
        let expires_at = std::time::Instant::now() + budget;
        let (respond_to, response) = oneshot::channel();
        let request = MavlinkCommandRequest {
            target_system_id,
            ack_timeout,
            retries,
            expires_at,
            respond_to,
        };
        match tokio::time::timeout(budget, async {
            self.sender.send(request).await.map_err(|_| ())?;
            response.await.map_err(|_| ())
        })
        .await
        {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(())) => {
                MavlinkCommandOutcome::TransportUnavailable("MAVLink worker stopped".into())
            }
            Err(_) => MavlinkCommandOutcome::TimedOut,
        }
    }
}

pub struct MavlinkRuntime {
    pub events: MavlinkEventReceiver,
    pub commands: MavlinkCommandSender,
}

/// Starts rust-mavlink's blocking transport on a dedicated OS thread. Latest
/// telemetry and lifecycle state are coalesced without blocking command ACKs.
pub fn spawn_mavlink_source(
    config: MavlinkSourceConfig,
    channel_capacity: usize,
) -> Result<MavlinkEventReceiver, AdapterError> {
    Ok(spawn_mavlink_runtime(config, channel_capacity)?.events)
}

pub fn spawn_mavlink_runtime(
    config: MavlinkSourceConfig,
    channel_capacity: usize,
) -> Result<MavlinkRuntime, AdapterError> {
    if channel_capacity == 0 {
        return Err(AdapterError::InvalidChannelCapacity);
    }
    let (sender, receiver) = event_channel();
    let (command_sender, command_receiver) = mpsc::channel(8);
    thread::Builder::new()
        .name("avian-mavlink".to_owned())
        .spawn(move || run_mavlink_source(config, sender, command_receiver))
        .map_err(|error| AdapterError::Runtime(error.to_string()))?;
    Ok(MavlinkRuntime {
        events: receiver,
        commands: MavlinkCommandSender {
            sender: command_sender,
        },
    })
}

fn run_mavlink_source(
    config: MavlinkSourceConfig,
    sender: watch::Sender<EventState>,
    mut commands: mpsc::Receiver<MavlinkCommandRequest>,
) {
    loop {
        let connection = match mavlink::connect::<MavMessage>(&config.address) {
            Ok(connection) => connection,
            Err(error) => {
                if !send_event(
                    &sender,
                    MavlinkTelemetryEvent::ConnectionLost(error.to_string()),
                ) {
                    return;
                }
                thread::sleep(config.reconnect_delay);
                continue;
            }
        };
        if !send_event(&sender, MavlinkTelemetryEvent::Connected) {
            return;
        }
        if !receive_connection(&config, &connection, &sender, &mut commands) {
            return;
        }
        thread::sleep(config.reconnect_delay);
    }
}

fn receive_connection(
    config: &MavlinkSourceConfig,
    connection: &dyn MavConnection<MavMessage>,
    sender: &watch::Sender<EventState>,
    commands: &mut mpsc::Receiver<MavlinkCommandRequest>,
) -> bool {
    let mut accumulator =
        match MavlinkTelemetryAccumulator::new(config.source.clone(), config.expected_stack) {
            Ok(accumulator) => accumulator,
            Err(error) => {
                send_event(sender, MavlinkTelemetryEvent::Rejected(error.to_string()));
                return false;
            }
        };
    let mut pending: Option<PendingCommand> = None;
    loop {
        if sender.is_closed() {
            if let Some(command) = pending.take() {
                let _ = command.respond(MavlinkCommandOutcome::TransportUnavailable(
                    "MAVLink source stopped".into(),
                ));
            }
            return false;
        }
        process_command_timeout(connection, &mut pending);
        if pending.is_none() {
            if let Ok(request) = commands.try_recv() {
                if request.respond_to.is_closed() || std::time::Instant::now() >= request.expires_at
                {
                    let _ = request.respond_to.send(MavlinkCommandOutcome::TimedOut);
                    continue;
                }
                if accumulator.target_system_id() != Some(request.target_system_id) {
                    let _ = request
                        .respond_to
                        .send(MavlinkCommandOutcome::TransportUnavailable(
                            "MAVLink system lock does not match command target".into(),
                        ));
                } else {
                    match send_rtl(connection, request.target_system_id, 0) {
                        Ok(()) => {
                            let ack_timeout = request.ack_timeout;
                            let deadline =
                                (std::time::Instant::now() + ack_timeout).min(request.expires_at);
                            pending = Some(PendingCommand {
                                request,
                                attempts: 1,
                                deadline,
                            });
                        }
                        Err(error) => {
                            let _ = request.respond_to.send(
                                MavlinkCommandOutcome::TransportUnavailable(error.to_string()),
                            );
                        }
                    }
                }
            }
        }

        let received = connection.try_recv();
        let (header, message) = match received {
            Ok(value) => value,
            Err(MessageReadError::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if process_command_timeout(connection, &mut pending) {
                    continue;
                }
                thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(MessageReadError::Parse(error)) => {
                if !send_event(sender, MavlinkTelemetryEvent::Rejected(error.to_string())) {
                    return false;
                }
                continue;
            }
            Err(error) => {
                if let Some(pending) = pending.take() {
                    let _ = pending.respond(MavlinkCommandOutcome::TransportUnavailable(
                        error.to_string(),
                    ));
                }
                return send_event(
                    sender,
                    MavlinkTelemetryEvent::ConnectionLost(error.to_string()),
                );
            }
        };
        if let MavMessage::COMMAND_ACK(ack) = &message {
            if ack.command == MavCmd::MAV_CMD_NAV_RETURN_TO_LAUNCH
                && pending
                    .as_ref()
                    .is_some_and(|request| request.request.target_system_id == header.system_id)
            {
                if let Some(pending) = pending.take() {
                    let outcome = if ack.result == MavResult::MAV_RESULT_ACCEPTED {
                        MavlinkCommandOutcome::Accepted
                    } else {
                        MavlinkCommandOutcome::Rejected(format!("{:?}", ack.result))
                    };
                    let _ = pending.respond(outcome);
                }
            }
        }
        let previous_target = accumulator.target_system_id();
        match accumulator.ingest(header, &message, unix_time_ms()) {
            Ok(Some(telemetry)) => {
                if previous_target.is_none() {
                    if let Some(target) = accumulator.target_system_id() {
                        if !send_event(sender, MavlinkTelemetryEvent::SystemLocked(target)) {
                            return false;
                        }
                    }
                }
                if !send_event(sender, MavlinkTelemetryEvent::Telemetry(telemetry)) {
                    return false;
                }
            }
            Ok(None) => {
                if previous_target.is_none() {
                    if let Some(target) = accumulator.target_system_id() {
                        if !send_event(sender, MavlinkTelemetryEvent::SystemLocked(target)) {
                            return false;
                        }
                    }
                }
            }
            Err(error @ AdapterError::UnexpectedFlightStack { .. }) => {
                send_event(sender, MavlinkTelemetryEvent::Rejected(error.to_string()));
                return false;
            }
            Err(error) => {
                if !send_event(sender, MavlinkTelemetryEvent::Rejected(error.to_string())) {
                    return false;
                }
            }
        }
    }
}

struct PendingCommand {
    request: MavlinkCommandRequest,
    attempts: u8,
    deadline: std::time::Instant,
}

impl PendingCommand {
    fn respond(self, outcome: MavlinkCommandOutcome) -> Result<(), MavlinkCommandOutcome> {
        self.request.respond_to.send(outcome)
    }
}

fn process_command_timeout(
    connection: &dyn MavConnection<MavMessage>,
    pending: &mut Option<PendingCommand>,
) -> bool {
    let Some(command) = pending.as_mut() else {
        return false;
    };
    if command.request.respond_to.is_closed() {
        pending.take();
        return true;
    }
    if std::time::Instant::now() >= command.request.expires_at {
        if let Some(command) = pending.take() {
            let _ = command.respond(MavlinkCommandOutcome::TimedOut);
        }
        return true;
    }
    if std::time::Instant::now() < command.deadline {
        return false;
    }
    if command.attempts <= command.request.retries {
        let confirmation = command.attempts;
        match send_rtl(connection, command.request.target_system_id, confirmation) {
            Ok(()) => {
                command.attempts = command.attempts.saturating_add(1);
                command.deadline = (std::time::Instant::now() + command.request.ack_timeout)
                    .min(command.request.expires_at);
            }
            Err(error) => {
                if let Some(command) = pending.take() {
                    let _ = command.respond(MavlinkCommandOutcome::TransportUnavailable(
                        error.to_string(),
                    ));
                }
            }
        }
    } else if let Some(command) = pending.take() {
        let _ = command.respond(MavlinkCommandOutcome::TimedOut);
    }
    true
}

fn send_rtl(
    connection: &dyn MavConnection<MavMessage>,
    target_system_id: u8,
    confirmation: u8,
) -> Result<(), mavlink::error::MessageWriteError> {
    let header = MavHeader {
        system_id: 255,
        component_id: 190,
        sequence: 0,
    };
    let message = MavMessage::COMMAND_LONG(COMMAND_LONG_DATA {
        command: MavCmd::MAV_CMD_NAV_RETURN_TO_LAUNCH,
        target_system: target_system_id,
        target_component: 1,
        confirmation,
        ..COMMAND_LONG_DATA::default()
    });
    connection.send(&header, &message)?;
    Ok(())
}

/// The reader exposes the existing event-oriented interface while retaining
/// only the newest telemetry and each current lifecycle field. A slow consumer
/// cannot stop the transport thread from receiving ACKs or checking deadlines.
pub struct MavlinkEventReceiver {
    state: watch::Receiver<EventState>,
    seen: u64,
}

#[derive(Clone, Default)]
struct EventState {
    sequence: u64,
    connection: Option<(u64, MavlinkTelemetryEvent)>,
    target: Option<(u64, MavlinkTelemetryEvent)>,
    telemetry: Option<(u64, MavlinkTelemetryEvent)>,
    rejection: Option<(u64, MavlinkTelemetryEvent)>,
}

impl MavlinkEventReceiver {
    pub async fn recv(&mut self) -> Option<MavlinkTelemetryEvent> {
        loop {
            let next = {
                let state = self.state.borrow_and_update();
                [
                    &state.connection,
                    &state.target,
                    &state.telemetry,
                    &state.rejection,
                ]
                .into_iter()
                .flatten()
                .filter(|(sequence, _)| *sequence > self.seen)
                .min_by_key(|(sequence, _)| *sequence)
                .cloned()
            };
            if let Some((sequence, event)) = next {
                self.seen = sequence;
                return Some(event);
            }
            if self.state.changed().await.is_err() {
                return None;
            }
        }
    }
}

fn event_channel() -> (watch::Sender<EventState>, MavlinkEventReceiver) {
    let (sender, state) = watch::channel(EventState::default());
    (sender, MavlinkEventReceiver { state, seen: 0 })
}

fn send_event(sender: &watch::Sender<EventState>, event: MavlinkTelemetryEvent) -> bool {
    if sender.is_closed() {
        return false;
    }
    sender.send_modify(|state| {
        state.sequence = state.sequence.saturating_add(1);
        let stamped = (state.sequence, event.clone());
        match event {
            MavlinkTelemetryEvent::Connected | MavlinkTelemetryEvent::ConnectionLost(_) => {
                state.connection = Some(stamped);
                state.target = None;
                state.telemetry = None;
                state.rejection = None;
            }
            MavlinkTelemetryEvent::SystemLocked(_) => state.target = Some(stamped),
            MavlinkTelemetryEvent::Telemetry(_) => state.telemetry = Some(stamped),
            MavlinkTelemetryEvent::Rejected(_) => state.rejection = Some(stamped),
        }
    });
    true
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

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
        ALTITUDE_DATA, ATTITUDE_DATA, COMMAND_ACK_DATA, EXTENDED_SYS_STATE_DATA,
        GLOBAL_POSITION_INT_DATA, HEARTBEAT_DATA, RC_CHANNELS_DATA, SYS_STATUS_DATA,
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

    #[tokio::test]
    async fn bidirectional_runtime_sends_rtl_and_correlates_ack() {
        let reservation = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let mut runtime = spawn_mavlink_runtime(
            MavlinkSourceConfig {
                address: format!("udpin:127.0.0.1:{port}"),
                source: NodeId::from("sitl"),
                expected_stack: FlightStack::ArduPilot,
                reconnect_delay: Duration::from_millis(20),
            },
            16,
        )
        .unwrap();
        let fake = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let connection =
                mavlink::connect::<MavMessage>(&format!("udpout:127.0.0.1:{port}")).unwrap();
            connection
                .send(
                    &header(42),
                    &heartbeat(MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA),
                )
                .unwrap();
            let (_, message) = connection.recv().unwrap();
            let MavMessage::COMMAND_LONG(command) = message else {
                panic!("expected COMMAND_LONG");
            };
            assert_eq!(command.command, MavCmd::MAV_CMD_NAV_RETURN_TO_LAUNCH);
            assert_eq!(command.target_system, 42);
            connection
                .send(
                    &header(41),
                    &MavMessage::COMMAND_ACK(COMMAND_ACK_DATA {
                        command: MavCmd::MAV_CMD_NAV_RETURN_TO_LAUNCH,
                        result: MavResult::MAV_RESULT_ACCEPTED,
                    }),
                )
                .unwrap();
            connection
                .send(
                    &header(42),
                    &MavMessage::COMMAND_ACK(COMMAND_ACK_DATA {
                        command: MavCmd::MAV_CMD_NAV_RETURN_TO_LAUNCH,
                        result: MavResult::MAV_RESULT_ACCEPTED,
                    }),
                )
                .unwrap();
        });
        loop {
            let event = tokio::time::timeout(Duration::from_secs(2), runtime.events.recv())
                .await
                .unwrap()
                .unwrap();
            if event == MavlinkTelemetryEvent::SystemLocked(42) {
                break;
            }
        }
        let outcome = runtime
            .commands
            .return_to_launch(42, Duration::from_millis(500), 1)
            .await;
        assert_eq!(outcome, MavlinkCommandOutcome::Accepted);
        fake.join().unwrap();
    }

    /// A continuously readable transport catches deadline starvation without
    /// relying on UDP packet scheduling or a real flight controller.
    struct FloodConnection {
        frames: std::sync::atomic::AtomicUsize,
        sends: std::sync::atomic::AtomicUsize,
        acknowledge: bool,
    }

    impl MavConnection<MavMessage> for FloodConnection {
        fn recv(&self) -> Result<(MavHeader, MavMessage), MessageReadError> {
            self.try_recv()
        }
        fn recv_raw(&self) -> Result<mavlink::MAVLinkMessageRaw, MessageReadError> {
            unreachable!()
        }
        fn try_recv(&self) -> Result<(MavHeader, MavMessage), MessageReadError> {
            use std::sync::atomic::Ordering;
            let frame = self.frames.fetch_add(1, Ordering::Relaxed);
            let message = if self.acknowledge && self.sends.load(Ordering::Relaxed) > 0 {
                MavMessage::COMMAND_ACK(COMMAND_ACK_DATA {
                    command: MavCmd::MAV_CMD_NAV_RETURN_TO_LAUNCH,
                    result: MavResult::MAV_RESULT_ACCEPTED,
                })
            } else {
                match frame % 3 {
                    0 => heartbeat(MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA),
                    1 => position(),
                    _ => attitude(),
                }
            };
            Ok((header(42), message))
        }
        fn send(
            &self,
            _: &MavHeader,
            _: &MavMessage,
        ) -> Result<usize, mavlink::error::MessageWriteError> {
            self.sends
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(1)
        }
        fn set_protocol_version(&mut self, _: mavlink::MavlinkVersion) {}
        fn protocol_version(&self) -> mavlink::MavlinkVersion {
            mavlink::MavlinkVersion::V2
        }
        fn set_allow_recv_any_version(&mut self, _: bool) {}
        fn allow_recv_any_version(&self) -> bool {
            true
        }
    }

    async fn command_under_telemetry_pressure(acknowledge: bool) -> (MavlinkCommandOutcome, usize) {
        use std::sync::atomic::Ordering;
        let (events, mut receiver) = event_channel();
        let (sender, mut commands) = mpsc::channel(1);
        let connection = std::sync::Arc::new(FloodConnection {
            frames: std::sync::atomic::AtomicUsize::new(0),
            sends: std::sync::atomic::AtomicUsize::new(0),
            acknowledge,
        });
        let worker_connection = connection.clone();
        let worker = tokio::task::spawn_blocking(move || {
            receive_connection(
                &MavlinkSourceConfig {
                    address: String::new(),
                    source: NodeId::from("flood"),
                    expected_stack: FlightStack::ArduPilot,
                    reconnect_delay: Duration::from_millis(1),
                },
                worker_connection.as_ref(),
                &events,
                &mut commands,
            );
        });
        loop {
            if matches!(
                tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                    .await
                    .unwrap(),
                Some(MavlinkTelemetryEvent::SystemLocked(42))
            ) {
                break;
            }
        }
        // Intentionally stop draining events. A telemetry consumer cannot hold
        // up ACK handling or retry deadlines even while input remains readable.
        let command = MavlinkCommandSender { sender };
        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            command.return_to_launch(42, Duration::from_millis(50), 1),
        )
        .await
        .unwrap();
        drop(receiver);
        worker.await.unwrap();
        assert!(connection.frames.load(Ordering::Relaxed) > 3);
        (outcome, connection.sends.load(Ordering::Relaxed))
    }

    #[tokio::test]
    async fn undrained_telemetry_does_not_block_acknowledgements() {
        let (outcome, sends) = command_under_telemetry_pressure(true).await;
        assert_eq!(outcome, MavlinkCommandOutcome::Accepted);
        assert_eq!(sends, 1);
    }

    #[tokio::test]
    async fn retry_deadlines_advance_during_continuous_telemetry() {
        let (outcome, sends) = command_under_telemetry_pressure(false).await;
        assert_eq!(outcome, MavlinkCommandOutcome::TimedOut);
        assert_eq!(sends, 2, "retry must occur while reads keep succeeding");
    }

    #[tokio::test]
    async fn latest_events_coalesce_without_losing_the_system_lock() {
        let (sender, mut receiver) = event_channel();
        assert!(send_event(&sender, MavlinkTelemetryEvent::Connected));
        assert!(send_event(&sender, MavlinkTelemetryEvent::SystemLocked(42)));
        let mut accumulator =
            MavlinkTelemetryAccumulator::new("test", FlightStack::ArduPilot).unwrap();
        accumulator
            .ingest(
                header(42),
                &heartbeat(MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA),
                0,
            )
            .unwrap();
        accumulator.ingest(header(42), &attitude(), 0).unwrap();
        for at in 1..=100 {
            if let Some(telemetry) = accumulator.ingest(header(42), &position(), at).unwrap() {
                assert!(send_event(
                    &sender,
                    MavlinkTelemetryEvent::Telemetry(telemetry)
                ));
            }
        }
        assert_eq!(
            receiver.recv().await,
            Some(MavlinkTelemetryEvent::Connected)
        );
        assert_eq!(
            receiver.recv().await,
            Some(MavlinkTelemetryEvent::SystemLocked(42))
        );
        let Some(MavlinkTelemetryEvent::Telemetry(telemetry)) = receiver.recv().await else {
            panic!("latest telemetry")
        };
        assert_eq!(telemetry.timestamp_ms, 100);
        drop(sender);
        assert_eq!(receiver.recv().await, None);
    }

    #[tokio::test]
    async fn expired_queued_commands_are_never_transmitted() {
        use std::sync::atomic::Ordering;
        let (events, receiver) = event_channel();
        let (sender, mut commands) = mpsc::channel(1);
        let (respond_to, response) = oneshot::channel();
        sender
            .send(MavlinkCommandRequest {
                target_system_id: 42,
                ack_timeout: Duration::from_millis(10),
                retries: 1,
                expires_at: std::time::Instant::now() - Duration::from_millis(1),
                respond_to,
            })
            .await
            .unwrap();
        let connection = std::sync::Arc::new(FloodConnection {
            frames: std::sync::atomic::AtomicUsize::new(0),
            sends: std::sync::atomic::AtomicUsize::new(0),
            acknowledge: false,
        });
        let worker_connection = connection.clone();
        let worker = tokio::task::spawn_blocking(move || {
            receive_connection(
                &MavlinkSourceConfig {
                    address: String::new(),
                    source: NodeId::from("expired"),
                    expected_stack: FlightStack::ArduPilot,
                    reconnect_delay: Duration::from_millis(1),
                },
                worker_connection.as_ref(),
                &events,
                &mut commands,
            )
        });
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), response)
                .await
                .unwrap()
                .unwrap(),
            MavlinkCommandOutcome::TimedOut
        );
        drop(receiver);
        worker.await.unwrap();
        assert_eq!(connection.sends.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn command_budget_bounds_time_waiting_for_a_worker() {
        let (sender, _receiver) = mpsc::channel(1);
        let commands = MavlinkCommandSender { sender };
        let outcome = tokio::time::timeout(
            Duration::from_millis(200),
            commands.return_to_launch(42, Duration::from_millis(10), 0),
        )
        .await
        .unwrap();
        assert_eq!(outcome, MavlinkCommandOutcome::TimedOut);
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

    #[test]
    fn rejects_zero_capacity_event_channel() {
        let result = spawn_mavlink_source(
            MavlinkSourceConfig {
                address: "udpin:127.0.0.1:14550".to_owned(),
                source: NodeId::from("px4-1"),
                expected_stack: FlightStack::Px4,
                reconnect_delay: Duration::from_secs(1),
            },
            0,
        );
        assert!(matches!(result, Err(AdapterError::InvalidChannelCapacity)));
    }
}

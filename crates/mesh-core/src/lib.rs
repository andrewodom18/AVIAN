//! Hardware-independent domain types for AVIAN.

mod altitude;
mod command;
mod link;
mod message;
mod node;
mod topology;

pub use altitude::{Altitude, AltitudeError, SYSTEM_MAX_MSL_FT, SYSTEM_MAX_MSL_M};
pub use command::{CommandError, EmergencyAction, EmergencyCommand, ReplayGuard};
pub use link::{
    LinkCandidate, LinkGeometry, LinkId, LinkMetrics, LinkOrchestrator, RoutePlan, TransportKind,
};
pub use message::{
    DeliveryClass, DeliveryPolicy, EmergencyAck, MeshPayload, MissionState, MissionStatus,
    Telemetry,
};
pub use node::{Capability, FlightStack, NodeId, NodeProfile, NodeRole, ProfileError};
pub use topology::{
    TopologyError, TopologyPlan, TopologyPlanner, DEFAULT_MAX_NEIGHBORS, MAX_SUPPORTED_SWARM_SIZE,
    MIN_SUPPORTED_SWARM_SIZE,
};

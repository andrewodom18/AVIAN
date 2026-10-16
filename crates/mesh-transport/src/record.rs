use crate::node::validate_endpoint_id;
use crate::node::MeshNodeError;
use mesh_core::{DeliveryClass, DeliveryPolicy, MeshPayload, NodeId};
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, path::PathBuf, str::FromStr};
const MAX_PEER_ADDRESSES: usize = 8;
pub const AVIAN_SCHEMA_VERSION: u16 = 2;
const MIN_SUPPORTED_SCHEMA_VERSION: u16 = 1;

/// Versioned application record stored in AVIAN. The envelope keeps transport
/// and persistence metadata outside the payload's domain schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AvianRecord {
    pub schema_version: u16,
    pub source: NodeId,
    pub sequence: u64,
    pub class: DeliveryClass,
    pub published_at_ms: u64,
    pub expires_at_ms: Option<u64>,
    pub payload: MeshPayload,
}

impl AvianRecord {
    pub fn new(
        source: NodeId,
        sequence: u64,
        class: DeliveryClass,
        published_at_ms: u64,
        payload: MeshPayload,
    ) -> Result<Self, MeshNodeError> {
        if !payload_matches_class(&payload, class) {
            return Err(MeshNodeError::PayloadClassMismatch);
        }
        let expires_at_ms = DeliveryPolicy::for_class(class)
            .ttl_ms
            .map(|ttl| published_at_ms.saturating_add(ttl));
        let record = Self {
            schema_version: AVIAN_SCHEMA_VERSION,
            source,
            sequence,
            class,
            published_at_ms,
            expires_at_ms,
            payload,
        };
        record.validate()?;
        Ok(record)
    }

    pub fn is_expired_at(&self, now_ms: u64) -> bool {
        self.expires_at_ms
            .is_some_and(|expires_at| expires_at <= now_ms)
    }

    pub(crate) fn validate(&self) -> Result<(), MeshNodeError> {
        if self.source.as_str().trim().is_empty() || self.source.as_str().len() > 256 {
            return Err(MeshNodeError::InvalidRecordSource);
        }
        if !(MIN_SUPPORTED_SCHEMA_VERSION..=AVIAN_SCHEMA_VERSION).contains(&self.schema_version) {
            return Err(MeshNodeError::UnsupportedSchema(self.schema_version));
        }
        if !payload_matches_class(&self.payload, self.class) {
            return Err(MeshNodeError::PayloadClassMismatch);
        }
        Ok(())
    }
}

fn payload_matches_class(payload: &MeshPayload, class: DeliveryClass) -> bool {
    matches!(
        (payload, class),
        (MeshPayload::EmergencyCommand(_), DeliveryClass::Emergency)
            | (MeshPayload::EmergencyAck(_), DeliveryClass::Acknowledgement)
            | (MeshPayload::Mission(_), DeliveryClass::Mission)
            | (MeshPayload::MissionAllocation(_), DeliveryClass::Mission)
            | (
                MeshPayload::RelayRuntimeConfiguration(_),
                DeliveryClass::Mission
            )
            | (MeshPayload::RelayReconfiguration(_), DeliveryClass::Mission)
            | (MeshPayload::RadioConfiguration(_), DeliveryClass::Mission)
            | (
                MeshPayload::RadioAttachmentAssertion(_),
                DeliveryClass::Mission
            )
            | (MeshPayload::NodeAdvertisement(_), DeliveryClass::Mission)
            | (MeshPayload::Detection(_), DeliveryClass::Mission)
            | (MeshPayload::ImageManifest(_), DeliveryClass::Bulk)
            | (MeshPayload::Telemetry(_), DeliveryClass::Telemetry)
            | (MeshPayload::SwarmStatusSummary(_), DeliveryClass::Telemetry)
            | (
                MeshPayload::RelayLinkObservation(_),
                DeliveryClass::Telemetry
            )
            | (
                MeshPayload::StreamCasterMeshObservation(_),
                DeliveryClass::Telemetry
            )
            | (
                MeshPayload::RadioDeviceObservation(_),
                DeliveryClass::Telemetry
            )
            | (
                MeshPayload::LinkMonitorObservation(_),
                DeliveryClass::Telemetry
            )
    )
}

#[derive(Debug, Clone)]
pub struct MeshNodeConfig {
    pub name: String,
    pub formation_id: String,
    pub base64_shared_key: String,
    pub bind_address: SocketAddr,
    pub storage_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerDescriptor {
    pub name: String,
    pub endpoint_id_hex: String,
    /// Ordered reachable addresses. Put the preferred underlay first; AVIAN is
    /// given the complete set when it connects or reconnects.
    addresses: Vec<SocketAddr>,
}

impl PeerDescriptor {
    pub fn new(
        name: impl Into<String>,
        endpoint_id_hex: impl Into<String>,
        address: SocketAddr,
    ) -> Result<Self, MeshNodeError> {
        Self::with_addresses(name, endpoint_id_hex, vec![address])
    }

    pub fn with_addresses(
        name: impl Into<String>,
        endpoint_id_hex: impl Into<String>,
        addresses: Vec<SocketAddr>,
    ) -> Result<Self, MeshNodeError> {
        let endpoint_id_hex = endpoint_id_hex.into();
        validate_endpoint_id(&endpoint_id_hex)?;
        let mut unique_addresses = Vec::with_capacity(addresses.len());
        for address in addresses {
            if !unique_addresses.contains(&address) {
                unique_addresses.push(address);
            }
        }
        if unique_addresses.is_empty() || unique_addresses.len() > MAX_PEER_ADDRESSES {
            return Err(MeshNodeError::InvalidPeerAddressCount(
                unique_addresses.len(),
            ));
        }
        Ok(Self {
            name: name.into(),
            endpoint_id_hex,
            addresses: unique_addresses,
        })
    }

    pub fn addresses(&self) -> &[SocketAddr] {
        &self.addresses
    }

    /// Deployment-safe peer form that preserves the stable node name.
    pub fn named_spec(&self) -> String {
        let addresses = self
            .addresses
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        format!("{}={}@{addresses}", self.name, self.endpoint_id_hex)
    }
}

impl FromStr for PeerDescriptor {
    type Err = MeshNodeError;

    /// Parses `NAME=ENDPOINT_ID_HEX@IP:PORT[,IP:PORT...]`. The legacy form
    /// without `NAME=` remains accepted and receives a short endpoint label.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (identity, addresses) = value
            .split_once('@')
            .ok_or_else(|| MeshNodeError::InvalidPeerSpec(value.to_owned()))?;
        let (name, endpoint_id_hex) = match identity.split_once('=') {
            Some((name, endpoint_id_hex)) if !name.trim().is_empty() => {
                (Some(name.to_owned()), endpoint_id_hex)
            }
            Some(_) => return Err(MeshNodeError::InvalidPeerSpec(value.to_owned())),
            None => (None, identity),
        };
        validate_endpoint_id(endpoint_id_hex)?;
        let addresses = addresses
            .split(',')
            .map(|address| {
                address
                    .parse()
                    .map_err(|_| MeshNodeError::InvalidPeerSpec(value.to_owned()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let short_length = endpoint_id_hex.len().min(12);
        Self::with_addresses(
            name.unwrap_or_else(|| format!("peer-{}", &endpoint_id_hex[..short_length])),
            endpoint_id_hex,
            addresses,
        )
    }
}

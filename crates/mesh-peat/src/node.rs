use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use mesh_core::{DeliveryClass, DeliveryPolicy, MeshPayload, NodeId};
use peat_mesh::network::iroh_transport::derive_iroh_node_secret;
use peat_mesh::storage::SyncTransport;
use peat_mesh::sync::{
    AutomergeBackend, AutomergeBackendConfig, BackendConfig, DataSyncBackend, Document, Query,
    SyncEngine, TransportConfig,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const AVIAN_SCHEMA_VERSION: u16 = 2;
const MIN_SUPPORTED_SCHEMA_VERSION: u16 = 1;

const COMMANDS_COLLECTION: &str = "commands";
const MISSIONS_COLLECTION: &str = "missions";
const TELEMETRY_COLLECTION: &str = "telemetry";
const BULK_COLLECTION: &str = "bulk";
const RECORD_FIELD: &str = "record";
const MAX_PEER_ADDRESSES: usize = 8;
const PEER_ADDRESS_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const PEER_ADDRESS_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Versioned application record stored in PEAT. The envelope keeps transport
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
    ) -> Result<Self, PeatNodeError> {
        if !payload_matches_class(&payload, class) {
            return Err(PeatNodeError::PayloadClassMismatch);
        }
        let expires_at_ms = DeliveryPolicy::for_class(class)
            .ttl_ms
            .map(|ttl| published_at_ms.saturating_add(ttl));
        Ok(Self {
            schema_version: AVIAN_SCHEMA_VERSION,
            source,
            sequence,
            class,
            published_at_ms,
            expires_at_ms,
            payload,
        })
    }

    pub fn is_expired_at(&self, now_ms: u64) -> bool {
        self.expires_at_ms
            .is_some_and(|expires_at| expires_at <= now_ms)
    }

    fn validate(&self) -> Result<(), PeatNodeError> {
        if !(MIN_SUPPORTED_SCHEMA_VERSION..=AVIAN_SCHEMA_VERSION).contains(&self.schema_version) {
            return Err(PeatNodeError::UnsupportedSchema(self.schema_version));
        }
        if !payload_matches_class(&self.payload, self.class) {
            return Err(PeatNodeError::PayloadClassMismatch);
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
pub struct PeatNodeConfig {
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
    /// Ordered reachable addresses. Put the preferred underlay first; PEAT is
    /// given the complete set when it connects or reconnects.
    addresses: Vec<SocketAddr>,
}

impl PeerDescriptor {
    pub fn new(
        name: impl Into<String>,
        endpoint_id_hex: impl Into<String>,
        address: SocketAddr,
    ) -> Result<Self, PeatNodeError> {
        Self::with_addresses(name, endpoint_id_hex, vec![address])
    }

    pub fn with_addresses(
        name: impl Into<String>,
        endpoint_id_hex: impl Into<String>,
        addresses: Vec<SocketAddr>,
    ) -> Result<Self, PeatNodeError> {
        let endpoint_id_hex = endpoint_id_hex.into();
        validate_endpoint_id(&endpoint_id_hex)?;
        let mut unique_addresses = Vec::with_capacity(addresses.len());
        for address in addresses {
            if !unique_addresses.contains(&address) {
                unique_addresses.push(address);
            }
        }
        if unique_addresses.is_empty() || unique_addresses.len() > MAX_PEER_ADDRESSES {
            return Err(PeatNodeError::InvalidPeerAddressCount(
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
    type Err = PeatNodeError;

    /// Parses `NAME=ENDPOINT_ID_HEX@IP:PORT[,IP:PORT...]`. The legacy form
    /// without `NAME=` remains accepted and receives a short endpoint label.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (identity, addresses) = value
            .split_once('@')
            .ok_or_else(|| PeatNodeError::InvalidPeerSpec(value.to_owned()))?;
        let (name, endpoint_id_hex) = match identity.split_once('=') {
            Some((name, endpoint_id_hex)) if !name.trim().is_empty() => {
                (Some(name.to_owned()), endpoint_id_hex)
            }
            Some(_) => return Err(PeatNodeError::InvalidPeerSpec(value.to_owned())),
            None => (None, identity),
        };
        validate_endpoint_id(endpoint_id_hex)?;
        let addresses = addresses
            .split(',')
            .map(|address| {
                address
                    .parse()
                    .map_err(|_| PeatNodeError::InvalidPeerSpec(value.to_owned()))
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

/// A runnable AVIAN node backed by PEAT's persistent Automerge sync engine and
/// formation-authenticated Iroh QUIC transport. Hosted relays remain disabled.
#[derive(Clone)]
pub struct PeatNode {
    name: String,
    backend: Arc<AutomergeBackend>,
}

impl PeatNode {
    pub async fn start(config: PeatNodeConfig) -> Result<Self, PeatNodeError> {
        if config.name.trim().is_empty() {
            return Err(PeatNodeError::EmptyNodeName);
        }
        if config.formation_id.trim().is_empty() {
            return Err(PeatNodeError::EmptyFormationId);
        }

        let formation_secret = normalized_formation_secret(&config.base64_shared_key)?;
        let identity_secret = derive_iroh_node_secret(&formation_secret, &config.name);

        let mut peat_config = AutomergeBackendConfig::default();
        peat_config.data_dir = config.storage_path.clone();
        peat_config.formation_id = config.formation_id;
        peat_config.base64_shared_key = config.base64_shared_key;
        peat_config.iroh_bind_addr = Some(config.bind_address);
        peat_config.iroh_secret_key = Some(identity_secret);

        let backend = AutomergeBackend::with_iroh(peat_config).await?;
        backend
            .initialize(BackendConfig {
                app_id: "avian".to_owned(),
                persistence_dir: config.storage_path,
                shared_key: None,
                transport: TransportConfig::default(),
                extra: HashMap::new(),
            })
            .await?;
        backend.start_sync().await?;

        let node = Self {
            name: config.name,
            backend,
        };
        tokio::time::timeout(PEER_ADDRESS_WAIT_TIMEOUT, async {
            loop {
                if node.backend.blob_store().bound_addr_string().is_some() {
                    break;
                }
                tokio::time::sleep(PEER_ADDRESS_POLL_INTERVAL).await;
            }
        })
        .await
        .map_err(|_| PeatNodeError::NoBoundAddress)?;
        Ok(node)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn endpoint_id_hex(&self) -> String {
        self.backend.blob_store().endpoint().id().to_string()
    }

    pub fn peer_descriptor(&self) -> Result<PeerDescriptor, PeatNodeError> {
        let address = self
            .backend
            .blob_store()
            .bound_addr_string()
            .ok_or(PeatNodeError::NoBoundAddress)?
            .parse()
            .map_err(|_| PeatNodeError::NoBoundAddress)?;
        PeerDescriptor::new(self.name.clone(), self.endpoint_id_hex(), address)
    }

    /// Returns true when this side establishes the connection. PEAT may return
    /// false when deterministic tie-breaking assigns initiation to the peer.
    pub async fn connect(&self, peer: &PeerDescriptor) -> Result<bool, PeatNodeError> {
        let addresses: Vec<String> = peer.addresses().iter().map(ToString::to_string).collect();
        Ok(self
            .backend
            .connect_to_peer(&peer.endpoint_id_hex, &addresses)
            .await?)
    }

    pub fn peer_count(&self) -> usize {
        self.backend.transport().connected_peers().len()
    }

    pub fn is_peer_connected(&self, peer: &PeerDescriptor) -> bool {
        self.is_endpoint_connected(&peer.endpoint_id_hex)
    }

    pub fn is_endpoint_connected(&self, endpoint_id_hex: &str) -> bool {
        self.backend
            .transport()
            .connected_peers()
            .iter()
            .any(|endpoint_id| endpoint_id.to_string() == endpoint_id_hex)
    }

    /// Returns the live transport's current remote address for status and
    /// underlay attribution. This never initiates or changes a connection.
    pub fn peer_remote_address(&self, endpoint_id_hex: &str) -> Option<SocketAddr> {
        let endpoint_id = endpoint_id_hex.parse().ok()?;
        self.backend
            .transport()
            .get_connection(&endpoint_id)
            .and_then(|connection| {
                connection
                    .paths()
                    .iter()
                    .find(|path| path.is_selected())
                    .and_then(|path| match path.remote_addr() {
                        iroh::TransportAddr::Ip(address) => Some(*address),
                        _ => None,
                    })
            })
    }

    /// Closes and forgets the current transport connection for a peer.
    /// Runtime configuration remains the caller's responsibility.
    pub fn disconnect(&self, peer: &PeerDescriptor) -> Result<(), PeatNodeError> {
        self.disconnect_endpoint(&peer.endpoint_id_hex)
    }

    /// Closes and forgets a transport connection using only its endpoint ID.
    /// This supports ground-side path admission when a paired peer deliberately
    /// has no current routing addresses.
    pub fn disconnect_endpoint(&self, endpoint_id_hex: &str) -> Result<(), PeatNodeError> {
        let endpoint_id = endpoint_id_hex
            .parse()
            .map_err(|_| PeatNodeError::InvalidEndpointId)?;
        if let Some(connection) = self.backend.transport().get_connection(&endpoint_id) {
            connection.close(200u32.into(), b"peer path is not locally configured");
        }
        self.backend.transport().remove_connection(&endpoint_id);
        self.backend
            .coordinator()
            .clear_peer_sync_state(endpoint_id);
        Ok(())
    }

    pub async fn put(&self, record_id: &str, record: &AvianRecord) -> Result<(), PeatNodeError> {
        validate_record_id(record_id)?;
        record.validate()?;
        let mut fields = HashMap::new();
        fields.insert(RECORD_FIELD.to_owned(), serde_json::to_value(record)?);
        self.backend
            .document_store()
            .upsert(
                collection_for(record.class),
                Document::with_id(record_id, fields),
            )
            .await?;
        Ok(())
    }

    pub async fn get(
        &self,
        class: DeliveryClass,
        record_id: &str,
    ) -> Result<Option<AvianRecord>, PeatNodeError> {
        validate_record_id(record_id)?;
        let Some(document) = self
            .backend
            .document_store()
            .get(collection_for(class), &record_id.to_owned())
            .await?
        else {
            return Ok(None);
        };
        let record = record_from_document(document)?;
        Ok((record.class == class).then_some(record))
    }

    pub async fn scan(
        &self,
        class: DeliveryClass,
    ) -> Result<Vec<(String, AvianRecord)>, PeatNodeError> {
        let records = self
            .backend
            .document_store()
            .query(collection_for(class), &Query::All)
            .await?
            .into_iter()
            .map(|document| {
                let record_id = document.id.clone().ok_or(PeatNodeError::MissingRecordId)?;
                let record = record_from_document(document)?;
                Ok((record.class == class).then_some((record_id, record)))
            })
            .collect::<Result<Vec<_>, PeatNodeError>>()?;
        Ok(records.into_iter().flatten().collect())
    }

    pub async fn sync_now(&self) -> Result<(), PeatNodeError> {
        self.backend.force_sync().await?;
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<(), PeatNodeError> {
        self.backend.shutdown().await?;
        Ok(())
    }
}

fn collection_for(class: DeliveryClass) -> &'static str {
    match class {
        DeliveryClass::Emergency | DeliveryClass::Acknowledgement => COMMANDS_COLLECTION,
        DeliveryClass::Mission => MISSIONS_COLLECTION,
        DeliveryClass::Telemetry => TELEMETRY_COLLECTION,
        DeliveryClass::Bulk => BULK_COLLECTION,
    }
}

fn record_from_document(document: Document) -> Result<AvianRecord, PeatNodeError> {
    let value = document
        .fields
        .get(RECORD_FIELD)
        .ok_or(PeatNodeError::MissingRecordField)?;
    let record: AvianRecord = serde_json::from_value(value.clone())?;
    record.validate()?;
    Ok(record)
}

fn normalized_formation_secret(value: &str) -> Result<[u8; 32], PeatNodeError> {
    let decoded = STANDARD
        .decode(value.trim())
        .map_err(|_| PeatNodeError::InvalidFormationSecret)?;
    if decoded.is_empty() {
        return Err(PeatNodeError::InvalidFormationSecret);
    }
    if decoded.len() == 32 {
        return decoded
            .try_into()
            .map_err(|_| PeatNodeError::InvalidFormationSecret);
    }
    Ok(Sha256::digest(decoded).into())
}

/// Derives the exact stable Iroh endpoint ID that [`PeatNode::start`] will use
/// for a node, without opening a socket or creating persistent state.
pub fn derive_peat_endpoint_id(
    base64_shared_key: &str,
    node_name: &str,
) -> Result<String, PeatNodeError> {
    if node_name.trim().is_empty() {
        return Err(PeatNodeError::EmptyNodeName);
    }
    let formation_secret = normalized_formation_secret(base64_shared_key)?;
    let identity_secret = derive_iroh_node_secret(&formation_secret, node_name);
    Ok(iroh::SecretKey::from_bytes(&identity_secret)
        .public()
        .to_string())
}

fn validate_endpoint_id(value: &str) -> Result<(), PeatNodeError> {
    let decoded = hex::decode(value).map_err(|_| PeatNodeError::InvalidEndpointId)?;
    if decoded.len() != 32 {
        return Err(PeatNodeError::InvalidEndpointId);
    }
    Ok(())
}

fn validate_record_id(value: &str) -> Result<(), PeatNodeError> {
    if value.is_empty() || value.len() > 256 || value.contains('\0') {
        return Err(PeatNodeError::InvalidRecordId);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum PeatNodeError {
    #[error("node name cannot be empty")]
    EmptyNodeName,
    #[error("formation ID cannot be empty")]
    EmptyFormationId,
    #[error("formation secret must be non-empty standard base64")]
    InvalidFormationSecret,
    #[error("invalid PEAT endpoint ID")]
    InvalidEndpointId,
    #[error("invalid peer specification {0:?}; expected NAME=ENDPOINT_ID_HEX@IP:PORT[,IP:PORT...] (NAME= is optional)")]
    InvalidPeerSpec(String),
    #[error("a peer must have between 1 and 8 unique addresses, got {0}")]
    InvalidPeerAddressCount(usize),
    #[error("PEAT transport did not expose an IP bind address")]
    NoBoundAddress,
    #[error("record ID must contain 1-256 non-NUL characters")]
    InvalidRecordId,
    #[error("a PEAT document is missing its record ID")]
    MissingRecordId,
    #[error("a PEAT document is missing its AVIAN record field")]
    MissingRecordField,
    #[error("payload type does not match its delivery class")]
    PayloadClassMismatch,
    #[error("unsupported AVIAN record schema version {0}")]
    UnsupportedSchema(u16),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Peat(#[from] anyhow::Error),
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mesh_core::{MissionState, MissionStatus};
    use peat_mesh::security::FormationKey;
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;

    fn mission_record() -> AvianRecord {
        AvianRecord::new(
            NodeId::from("node-a"),
            1,
            DeliveryClass::Mission,
            1_000,
            MeshPayload::Mission(MissionState {
                mission_id: Uuid::from_u128(42),
                objective: "prove PEAT convergence".to_owned(),
                generation: 1,
                status: MissionStatus::Active,
            }),
        )
        .unwrap()
    }

    fn node_config(name: &str, storage: &TempDir, shared_key: &str) -> PeatNodeConfig {
        PeatNodeConfig {
            name: name.to_owned(),
            formation_id: "avian-test".to_owned(),
            base64_shared_key: shared_key.to_owned(),
            bind_address: "127.0.0.1:0".parse().unwrap(),
            storage_path: storage.path().to_path_buf(),
        }
    }

    #[test]
    fn record_rejects_mismatched_delivery_class() {
        let result = AvianRecord::new(
            NodeId::from("node-a"),
            1,
            DeliveryClass::Telemetry,
            1_000,
            mission_record().payload,
        );
        assert!(matches!(result, Err(PeatNodeError::PayloadClassMismatch)));
    }

    #[test]
    fn readers_accept_v1_while_new_records_emit_v2() {
        let mut existing = mission_record();
        existing.schema_version = 1;
        existing.validate().unwrap();
        assert_eq!(mission_record().schema_version, AVIAN_SCHEMA_VERSION);
    }

    #[test]
    fn peer_descriptor_parser_rejects_short_ids() {
        assert!(matches!(
            "abcd@127.0.0.1:9000".parse::<PeerDescriptor>(),
            Err(PeatNodeError::InvalidEndpointId)
        ));
    }

    #[test]
    fn peer_descriptor_preserves_multiple_underlay_addresses() {
        let endpoint_id = "01".repeat(32);
        let descriptor = format!("{endpoint_id}@10.10.0.2:9000,172.20.0.2:9000")
            .parse::<PeerDescriptor>()
            .unwrap();

        assert_eq!(descriptor.endpoint_id_hex, endpoint_id);
        assert_eq!(
            descriptor.addresses(),
            vec![
                "10.10.0.2:9000".parse().unwrap(),
                "172.20.0.2:9000".parse().unwrap()
            ]
        );
    }

    #[test]
    fn named_peer_descriptor_round_trips_for_deployment() {
        let endpoint_id = "02".repeat(32);
        let descriptor = format!("drone-017={endpoint_id}@10.40.0.17:4747,172.20.0.17:4747")
            .parse::<PeerDescriptor>()
            .unwrap();

        assert_eq!(descriptor.name, "drone-017");
        assert_eq!(
            descriptor.named_spec(),
            format!("drone-017={endpoint_id}@10.40.0.17:4747,172.20.0.17:4747")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn dual_address_peer_converges_over_available_fallback() {
        let storage_a = TempDir::new().unwrap();
        let storage_b = TempDir::new().unwrap();
        let shared_key = FormationKey::generate_secret();
        let node_a = PeatNode::start(node_config("avian-test/node-a", &storage_a, &shared_key))
            .await
            .unwrap();
        let reservation = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let node_b_address = reservation.local_addr().unwrap();
        drop(reservation);
        let mut node_b_config = node_config("avian-test/node-b", &storage_b, &shared_key);
        node_b_config.bind_address = node_b_address;
        let node_b = PeatNode::start(node_b_config.clone()).await.unwrap();

        assert_eq!(
            derive_peat_endpoint_id(&shared_key, "avian-test/node-a").unwrap(),
            node_a.endpoint_id_hex()
        );

        let node_b_descriptor = node_b.peer_descriptor().unwrap();
        let mut addresses = vec!["127.0.0.1:1".parse().unwrap()];
        addresses.extend_from_slice(node_b_descriptor.addresses());
        let node_b_peer = PeerDescriptor::with_addresses(
            node_b_descriptor.name,
            node_b_descriptor.endpoint_id_hex,
            addresses,
        )
        .unwrap();
        assert!(node_a.connect(&node_b_peer).await.unwrap());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if node_a.peer_count() > 0 || node_b.peer_count() > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("PEAT peers should connect");

        let record = mission_record();
        node_a.put("current", &record).await.unwrap();

        let received = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Some(record) = node_b.get(DeliveryClass::Mission, "current").await.unwrap() {
                    break record;
                }
                node_a.sync_now().await.unwrap();
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .await
        .expect("mission record should converge");

        assert_eq!(received, record);
        node_a.shutdown().await.unwrap();
        node_b.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wildcard_bind_exposes_a_peer_address_before_start_returns() {
        let storage = TempDir::new().unwrap();
        let shared_key = FormationKey::generate_secret();
        let mut config = node_config("avian-test/wildcard", &storage, &shared_key);
        config.bind_address = "0.0.0.0:0".parse().unwrap();
        let node = PeatNode::start(config).await.unwrap();

        let descriptor = node
            .peer_descriptor()
            .expect("a started wildcard PEAT node should expose a reachable address");

        assert!(!descriptor.addresses().is_empty());
        assert!(descriptor
            .addresses()
            .iter()
            .all(|address| !address.ip().is_unspecified()));
        node.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn endpoint_identity_is_stable_across_restart() {
        let storage = TempDir::new().unwrap();
        let shared_key = FormationKey::generate_secret();
        let expected = derive_peat_endpoint_id(&shared_key, "avian-test/stable").unwrap();

        let first = PeatNode::start(node_config("avian-test/stable", &storage, &shared_key))
            .await
            .unwrap();
        assert_eq!(first.endpoint_id_hex(), expected);
        first.shutdown().await.unwrap();
        drop(first);
        tokio::time::sleep(Duration::from_millis(100)).await;

        let second = PeatNode::start(node_config("avian-test/stable", &storage, &shared_key))
            .await
            .unwrap();
        assert_eq!(second.endpoint_id_hex(), expected);
        second.shutdown().await.unwrap();
    }
}

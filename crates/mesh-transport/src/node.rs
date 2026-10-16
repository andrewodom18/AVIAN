use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{anyhow, bail};
use base64::{engine::general_purpose::STANDARD, Engine};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use iroh::{
    endpoint::{presets, Connection, RecvStream, SendStream},
    Endpoint, EndpointAddr, SecretKey,
};
use mesh_core::DeliveryClass;
use rand_core::{OsRng, RngCore};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    sync::{mpsc, watch, Semaphore},
    task::{JoinHandle, JoinSet},
};

use crate::{
    record::{AvianRecord, MeshNodeConfig, PeerDescriptor},
    store::{record_key, InventoryEntry, Store, StoredRecord, MAX_RECORD_BYTES, PAGE_SIZE},
};

const ALPN: &[u8] = b"avian/records/1";
const MAX_PEERS: usize = 16;
const MAX_HANDSHAKES: usize = 16;
const CONTROL_BYTES: usize = 64 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const SYNC_INTERVAL: Duration = Duration::from_secs(2);

type Result<T> = std::result::Result<T, MeshNodeError>;

/// QUIC may already be live when authentication or a caller timeout cancels.
/// Until a supervised session owns the connection, cancellation must close it.
struct PendingConnection {
    connection: Connection,
    armed: bool,
}
impl PendingConnection {
    fn new(connection: &Connection) -> Self {
        Self {
            connection: connection.clone(),
            armed: true,
        }
    }
    fn release(mut self) {
        self.armed = false;
    }
}
impl Drop for PendingConnection {
    fn drop(&mut self) {
        if self.armed {
            self.connection
                .close(2u32.into(), b"connection admission canceled");
        }
    }
}

struct ActivePeer {
    connection: Connection,
    initiated_locally: bool,
}

#[derive(Default)]
struct CriticalChanges {
    version: u64,
    keys: VecDeque<String>,
}

#[cfg(test)]
struct TestExchangeGate {
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}

struct Inner {
    name: String,
    formation: String,
    secret: [u8; 32],
    endpoint: Endpoint,
    store: Arc<Mutex<Option<Store>>>,
    store_admission: Arc<Semaphore>,
    peers: RwLock<HashMap<String, ActivePeer>>,
    connections: mpsc::Sender<Connection>,
    admission: Arc<Semaphore>,
    changed: watch::Sender<u64>,
    critical_changes: Mutex<CriticalChanges>,
    #[cfg(test)]
    test_normal_gate: Mutex<Option<Arc<TestExchangeGate>>>,
    stopped: watch::Sender<bool>,
}

impl Inner {
    async fn store<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Store) -> anyhow::Result<T> + Send + 'static,
    ) -> anyhow::Result<T> {
        let permit = self.store_admission.clone().acquire_owned().await?;
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut guard = store
                .lock()
                .map_err(|_| anyhow!("AVIAN storage lock poisoned"))?;
            operation(
                guard
                    .as_mut()
                    .ok_or_else(|| anyhow!("AVIAN node is shut down"))?,
            )
        })
        .await
        .map_err(|error| anyhow!("AVIAN storage worker: {error}"))?
    }

    async fn release_store(&self) {
        if let Ok(permit) = self.store_admission.clone().acquire_owned().await {
            let store = self.store.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                if let Ok(mut guard) = store.lock() {
                    guard.take();
                }
            })
            .await;
        }
    }

    fn record_changed(&self, key: String) {
        if key.starts_with("0:") || key.starts_with("1:") {
            if let Ok(mut critical) = self.critical_changes.lock() {
                critical.version = critical.version.wrapping_add(1);
                critical.keys.retain(|existing| existing != &key);
                critical.keys.push_back(key);
                while critical.keys.len() > PAGE_SIZE {
                    critical.keys.pop_front();
                }
            }
        }
        self.changed
            .send_modify(|version| *version = version.wrapping_add(1));
    }

    fn install(&self, connection: Connection, initiated_locally: bool) -> anyhow::Result<bool> {
        if *self.stopped.borrow() {
            bail!("AVIAN node is shut down");
        }
        let remote = connection.remote_id().to_string();
        let mut peers = self
            .peers
            .write()
            .map_err(|_| anyhow!("peer lock poisoned"))?;
        if let Some(existing) = peers.get(&remote) {
            // Both endpoints prefer the connection initiated by the lower ID.
            // A sole connection in either direction is nevertheless valid.
            let prefer_outgoing = self.endpoint.id().to_string() < remote;
            if existing.connection.close_reason().is_none()
                && (existing.initiated_locally == prefer_outgoing
                    || initiated_locally != prefer_outgoing)
            {
                connection.close(0u32.into(), b"duplicate peer connection");
                return Ok(false);
            }
        } else if peers.len() >= MAX_PEERS {
            connection.close(1u32.into(), b"peer limit");
            bail!("AVIAN peer limit reached");
        }
        if let Some(previous) = peers.insert(
            remote,
            ActivePeer {
                connection,
                initiated_locally,
            },
        ) {
            previous
                .connection
                .close(0u32.into(), b"replaced duplicate connection");
        }
        Ok(true)
    }

    fn remove(&self, connection: &Connection) {
        if let Ok(mut peers) = self.peers.write() {
            let remote = connection.remote_id().to_string();
            if peers
                .get(&remote)
                .is_some_and(|peer| peer.connection.stable_id() == connection.stable_id())
            {
                peers.remove(&remote);
            }
        }
    }
}

struct Lifecycle {
    stop: watch::Sender<bool>,
    task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    shutdown: tokio::sync::Mutex<()>,
}

impl Drop for Lifecycle {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

/// AVIAN-owned formation-authenticated QUIC transport and durable registers.
/// No hosted relay, discovery service, or permanent coordinator is required.
#[derive(Clone)]
pub struct MeshNode {
    inner: Arc<Inner>,
    lifecycle: Arc<Lifecycle>,
}

impl MeshNode {
    pub async fn start(config: MeshNodeConfig) -> Result<Self> {
        if config.name.trim().is_empty() {
            return Err(MeshNodeError::EmptyNodeName);
        }
        if config.name.len() > 256 || config.formation_id.len() > 256 {
            return Err(MeshNodeError::IdentityTooLong);
        }
        if config.formation_id.trim().is_empty() {
            return Err(MeshNodeError::EmptyFormationId);
        }
        let secret = normalized_formation_secret(&config.base64_shared_key)?;
        let identity = derive_identity(&secret, &config.name);
        let expected_endpoint = SecretKey::from_bytes(&identity).public().to_string();
        let storage_path = config.storage_path.clone();
        let formation = config.formation_id.clone();
        let store = tokio::task::spawn_blocking(move || {
            Store::open(&storage_path, &formation, &expected_endpoint)
        })
        .await
        .map_err(|error| anyhow!("open AVIAN storage worker: {error}"))??;
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::from_bytes(&identity))
            .alpns(vec![ALPN.to_vec()])
            .bind_addr(config.bind_address)
            .map_err(|error| anyhow!("bind address: {error}"))?
            .bind()
            .await
            .map_err(|error| anyhow!("bind AVIAN endpoint: {error}"))?;
        let (stopped, _) = watch::channel(false);
        let (connections, receiver) = mpsc::channel(MAX_PEERS);
        let inner = Arc::new(Inner {
            name: config.name,
            formation: config.formation_id,
            secret,
            endpoint,
            store: Arc::new(Mutex::new(Some(store))),
            store_admission: Arc::new(Semaphore::new(1)),
            peers: RwLock::new(HashMap::new()),
            connections,
            admission: Arc::new(Semaphore::new(MAX_HANDSHAKES)),
            changed: watch::channel(0).0,
            critical_changes: Mutex::new(CriticalChanges::default()),
            #[cfg(test)]
            test_normal_gate: Mutex::new(None),
            stopped: stopped.clone(),
        });
        let task = tokio::spawn(supervise(inner.clone(), receiver));
        let node = Self {
            inner,
            lifecycle: Arc::new(Lifecycle {
                stop: stopped,
                task: tokio::sync::Mutex::new(Some(task)),
                shutdown: tokio::sync::Mutex::new(()),
            }),
        };
        if let Err(error) = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if node.peer_descriptor().is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        {
            node.shutdown().await?;
            return Err(anyhow!("AVIAN endpoint did not expose an IP address: {error}").into());
        }
        Ok(node)
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }
    pub fn endpoint_id_hex(&self) -> String {
        self.inner.endpoint.id().to_string()
    }
    pub fn peer_descriptor(&self) -> Result<PeerDescriptor> {
        let addresses = self
            .inner
            .endpoint
            .addr()
            .ip_addrs()
            .copied()
            .filter(|address| !address.ip().is_unspecified())
            .take(8)
            .collect::<Vec<_>>();
        if addresses.is_empty() {
            return Err(MeshNodeError::NoBoundAddress);
        }
        PeerDescriptor::with_addresses(self.name(), self.endpoint_id_hex(), addresses)
    }

    /// Connects over the complete IP candidate set. Iroh races reachable paths,
    /// including fallback addresses, within a bounded connection attempt.
    pub async fn connect(&self, peer: &PeerDescriptor) -> Result<bool> {
        if *self.inner.stopped.borrow() {
            return Err(MeshNodeError::Stopped);
        }
        if self.is_peer_connected(peer) {
            return Ok(false);
        }
        let _permit = self
            .inner
            .admission
            .clone()
            .try_acquire_owned()
            .map_err(|_| MeshNodeError::Busy)?;
        let endpoint_id = peer
            .endpoint_id_hex
            .parse()
            .map_err(|_| MeshNodeError::InvalidEndpointId)?;
        if endpoint_id == self.inner.endpoint.id() {
            return Ok(false);
        }
        let address = EndpointAddr::from_parts(
            endpoint_id,
            peer.addresses()
                .iter()
                .copied()
                .map(iroh::TransportAddr::Ip),
        );
        let connection =
            tokio::time::timeout(CONNECT_TIMEOUT, self.inner.endpoint.connect(address, ALPN))
                .await
                .map_err(|_| anyhow!("AVIAN connection attempt timed out"))?
                .map_err(|error| anyhow!("connect AVIAN peer: {error}"))?;
        let pending = PendingConnection::new(&connection);
        if let Err(error) = authenticate(&self.inner, &connection, true).await {
            connection.close(2u32.into(), b"formation authentication failed");
            return Err(error.into());
        }
        let installed = self.inner.install(connection.clone(), true)?;
        if installed && self.inner.connections.try_send(connection.clone()).is_err() {
            self.inner.remove(&connection);
            connection.close(1u32.into(), b"session queue full");
            return Err(MeshNodeError::Busy);
        }
        if installed {
            pending.release();
        }
        Ok(installed)
    }

    pub fn peer_count(&self) -> usize {
        self.inner
            .peers
            .read()
            .map(|peers| {
                peers
                    .values()
                    .filter(|peer| peer.connection.close_reason().is_none())
                    .count()
            })
            .unwrap_or(0)
    }
    pub fn is_peer_connected(&self, peer: &PeerDescriptor) -> bool {
        self.is_endpoint_connected(&peer.endpoint_id_hex)
    }
    pub fn is_endpoint_connected(&self, endpoint: &str) -> bool {
        self.inner.peers.read().is_ok_and(|peers| {
            peers
                .get(endpoint)
                .is_some_and(|peer| peer.connection.close_reason().is_none())
        })
    }
    pub fn peer_remote_address(&self, endpoint: &str) -> Option<SocketAddr> {
        let peers = self.inner.peers.read().ok()?;
        let peer = peers.get(endpoint)?;
        if peer.connection.close_reason().is_some() {
            return None;
        }
        peer.connection
            .paths()
            .iter()
            .find(|path| path.is_selected())
            .and_then(|path| match path.remote_addr() {
                iroh::TransportAddr::Ip(address) => Some(*address),
                _ => None,
            })
    }
    pub fn disconnect(&self, peer: &PeerDescriptor) -> Result<()> {
        self.disconnect_endpoint(&peer.endpoint_id_hex)
    }
    pub fn disconnect_endpoint(&self, endpoint: &str) -> Result<()> {
        validate_endpoint_id(endpoint)?;
        if let Some(peer) = self
            .inner
            .peers
            .write()
            .map_err(|_| anyhow!("peer lock poisoned"))?
            .remove(endpoint)
        {
            peer.connection
                .close(0u32.into(), b"peer path is not locally configured");
        }
        Ok(())
    }
    pub async fn put(&self, record_id: &str, record: &AvianRecord) -> Result<()> {
        validate_record_id(record_id)?;
        record.validate()?;
        let stored = StoredRecord {
            record_id: record_id.to_owned(),
            record: record.clone(),
        };
        let key = record_key(record.class, record_id);
        if self.inner.store(move |store| store.merge(stored)).await? {
            self.inner.record_changed(key);
        }
        Ok(())
    }
    pub async fn get(&self, class: DeliveryClass, record_id: &str) -> Result<Option<AvianRecord>> {
        validate_record_id(record_id)?;
        let key = record_key(class, record_id);
        Ok(self
            .inner
            .store(move |store| store.get(&key))
            .await?
            .map(|stored| stored.record))
    }
    pub async fn scan(&self, class: DeliveryClass) -> Result<Vec<(String, AvianRecord)>> {
        Ok(self.inner.store(move |store| store.scan(class)).await?)
    }
    pub async fn sync_now(&self) -> Result<()> {
        if *self.inner.stopped.borrow() {
            return Err(MeshNodeError::Stopped);
        }
        self.inner
            .changed
            .send_modify(|version| *version = version.wrapping_add(1));
        Ok(())
    }
    pub async fn shutdown(&self) -> Result<()> {
        let _guard = self.lifecycle.shutdown.lock().await;
        self.lifecycle.stop.send_replace(true);
        let mut task = self.lifecycle.task.lock().await;
        if let Some(handle) = task.as_mut() {
            // JoinHandle is cancellation-safe while borrowed: canceling this
            // await leaves ownership in Lifecycle for the next shutdown call.
            let result = handle.await;
            task.take();
            result.map_err(|error| anyhow!("AVIAN supervisor: {error}"))?;
        }
        self.inner.release_store().await;
        Ok(())
    }
}

async fn supervise(inner: Arc<Inner>, mut receiver: mpsc::Receiver<Connection>) {
    let mut stopped = inner.stopped.subscribe();
    let mut tasks = JoinSet::new();
    loop {
        if *stopped.borrow() {
            break;
        }
        tokio::select! {
            biased;
            _ = stopped.changed() => break,
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
            Some(connection) = receiver.recv() => {
                let inner = inner.clone();
                tasks.spawn(async move { session(inner, connection).await; });
            }
            incoming = inner.endpoint.accept() => {
                let Some(incoming) = incoming else { break; };
                let Ok(permit) = inner.admission.clone().try_acquire_owned() else { incoming.refuse(); continue; };
                let inner = inner.clone();
                tasks.spawn(async move {
                    let connection = match tokio::time::timeout(CONNECT_TIMEOUT, incoming).await {
                        Ok(Ok(connection)) => connection,
                        _ => return,
                    };
                    let pending = PendingConnection::new(&connection);
                    if authenticate(&inner, &connection, false).await.is_err() {
                        connection.close(2u32.into(), b"formation authentication failed");
                        return;
                    }
                    let installed = inner.install(connection.clone(), false).unwrap_or(false);
                    drop(permit);
                    if installed { pending.release(); session(inner, connection).await; }
                });
            }
        }
    }
    for peer in inner
        .peers
        .write()
        .expect("peer lock")
        .drain()
        .map(|(_, peer)| peer)
    {
        peer.connection.close(0u32.into(), b"node shutdown");
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    inner.endpoint.close().await;
    inner.release_store().await;
}

#[derive(Serialize, Deserialize)]
struct Hello {
    name: String,
    nonce: [u8; 32],
    proof: Vec<u8>,
}

fn auth_bytes(
    inner: &Inner,
    sender: &str,
    receiver: &str,
    name: &str,
    nonce: &[u8; 32],
    challenge: &[u8; 32],
) -> anyhow::Result<Vec<u8>> {
    Ok(serde_json::to_vec(&(
        "avian-auth-v1",
        &inner.formation,
        sender,
        receiver,
        name,
        nonce,
        challenge,
    ))?)
}
fn hello(inner: &Inner, remote: &str, challenge: &[u8; 32]) -> anyhow::Result<Hello> {
    let mut nonce = [0; 32];
    OsRng.fill_bytes(&mut nonce);
    let mut mac = Hmac::<Sha256>::new_from_slice(&inner.secret)?;
    mac.update(&auth_bytes(
        inner,
        &inner.endpoint.id().to_string(),
        remote,
        &inner.name,
        &nonce,
        challenge,
    )?);
    Ok(Hello {
        name: inner.name.clone(),
        nonce,
        proof: mac.finalize().into_bytes().to_vec(),
    })
}
fn verify_hello(
    inner: &Inner,
    remote: &str,
    hello: &Hello,
    challenge: &[u8; 32],
) -> anyhow::Result<()> {
    if hello.name.trim().is_empty() || hello.name.len() > 256 {
        bail!("invalid authenticated peer name");
    }
    let derived = SecretKey::from_bytes(&derive_identity(&inner.secret, &hello.name))
        .public()
        .to_string();
    if derived != remote {
        bail!("formation identity mismatch");
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(&inner.secret)?;
    mac.update(&auth_bytes(
        inner,
        remote,
        &inner.endpoint.id().to_string(),
        &hello.name,
        &hello.nonce,
        challenge,
    )?);
    mac.verify_slice(&hello.proof)
        .map_err(|_| anyhow!("formation authentication rejected"))
}
async fn authenticate(
    inner: &Inner,
    connection: &Connection,
    initiator: bool,
) -> anyhow::Result<()> {
    tokio::time::timeout(IO_TIMEOUT, async {
        let remote = connection.remote_id().to_string();
        if initiator {
            let (mut send, mut recv) = connection.open_bi().await?;
            let ours = hello(inner, &remote, &[0; 32])?;
            write_frame(&mut send, &ours, CONTROL_BYTES).await?;
            let theirs: Hello = read_frame(&mut recv, CONTROL_BYTES).await?;
            verify_hello(inner, &remote, &theirs, &ours.nonce)?;
            // Bind the final acknowledgement to the responder's fresh nonce.
            let confirmation = hello(inner, &remote, &theirs.nonce)?;
            write_frame(&mut send, &confirmation, CONTROL_BYTES).await?;
            send.finish()?;
        } else {
            let (mut send, mut recv) = connection.accept_bi().await?;
            let theirs: Hello = read_frame(&mut recv, CONTROL_BYTES).await?;
            verify_hello(inner, &remote, &theirs, &[0; 32])?;
            let ours = hello(inner, &remote, &theirs.nonce)?;
            write_frame(&mut send, &ours, CONTROL_BYTES).await?;
            let confirmation: Hello = read_frame(&mut recv, CONTROL_BYTES).await?;
            verify_hello(inner, &remote, &confirmation, &ours.nonce)?;
            if confirmation.name != theirs.name {
                bail!("peer identity changed during authentication");
            }
            send.finish()?;
        }
        anyhow::Ok(())
    })
    .await
    .map_err(|_| anyhow!("formation handshake timed out"))?
}

async fn write_frame<T: Serialize>(
    send: &mut SendStream,
    value: &T,
    limit: usize,
) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > limit {
        bail!("AVIAN frame too large");
    }
    send.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    send.write_all(&bytes).await?;
    Ok(())
}
async fn read_frame<T: DeserializeOwned>(recv: &mut RecvStream, limit: usize) -> anyhow::Result<T> {
    let mut header = [0; 4];
    recv.read_exact(&mut header).await?;
    let length = u32::from_be_bytes(header) as usize;
    if length > limit {
        bail!("AVIAN frame exceeds size limit");
    }
    let mut bytes = vec![0; length];
    recv.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn session(inner: Arc<Inner>, connection: Connection) {
    let result = tokio::try_join!(
        outgoing(&inner, &connection),
        outgoing_critical(&inner, &connection),
        incoming(inner.clone(), &connection),
    );
    if let Err(error) = result {
        tracing::debug!(%error, peer = %connection.remote_id(), "AVIAN synchronization ended");
    }
    connection.close(0u32.into(), b"synchronization session ended");
    inner.remove(&connection);
}
async fn outgoing(inner: &Inner, connection: &Connection) -> anyhow::Result<()> {
    let mut cursor: Option<String> = None;
    let mut changed = inner.changed.subscribe();
    loop {
        if cursor.is_none() {
            // Mark the version before reading the snapshot. Changes during a
            // paged exchange remain pending for every peer independently.
            changed.borrow_and_update();
        }
        let after = cursor.clone();
        let (inventory, more) = inner
            .store(move |store| store.inventory(after.as_deref()))
            .await?;
        tokio::time::timeout(IO_TIMEOUT, exchange(inner, connection, &inventory, 0))
            .await
            .map_err(|_| anyhow!("synchronization exchange timed out"))??;
        cursor = if more {
            inventory.last().map(|entry| entry.key.clone())
        } else {
            None
        };
        if more {
            tokio::time::sleep(Duration::from_millis(10)).await;
        } else {
            tokio::select! { _ = changed.changed() => {}, _ = tokio::time::sleep(SYNC_INTERVAL) => {}, _ = connection.closed() => return Ok(()) }
        }
    }
}
async fn outgoing_critical(inner: &Inner, connection: &Connection) -> anyhow::Result<()> {
    let mut changed = inner.changed.subscribe();
    let mut sent_version = 0;
    loop {
        changed.borrow_and_update();
        let (version, keys) = {
            let critical = inner
                .critical_changes
                .lock()
                .map_err(|_| anyhow!("critical-change lock poisoned"))?;
            (
                critical.version,
                critical.keys.iter().cloned().collect::<Vec<_>>(),
            )
        };
        if version != sent_version {
            let inventory = inner
                .store(move |store| store.inventory_keys(&keys))
                .await?;
            tokio::time::timeout(IO_TIMEOUT, exchange(inner, connection, &inventory, 100))
                .await
                .map_err(|_| anyhow!("critical synchronization timed out"))??;
            sent_version = version;
        }
        tokio::select! {
            _ = changed.changed() => {},
            _ = connection.closed() => return Ok(()),
        }
    }
}

async fn exchange(
    inner: &Inner,
    connection: &Connection,
    inventory: &[InventoryEntry],
    priority: i32,
) -> anyhow::Result<()> {
    let (mut send, mut recv) = connection.open_bi().await?;
    send.set_priority(priority)?;
    write_frame(&mut send, &inventory, CONTROL_BYTES).await?;
    let wanted: Vec<String> = read_frame(&mut recv, CONTROL_BYTES).await?;
    if wanted.len() > PAGE_SIZE {
        bail!("too many requested records");
    }
    let mut seen = std::collections::HashSet::new();
    for key in wanted {
        if !seen.insert(key.clone()) || !inventory.iter().any(|entry| entry.key == key) {
            bail!("unadvertised record requested");
        }
        let record = inner.store(move |store| store.get(&key)).await?;
        write_frame(&mut send, &record, MAX_RECORD_BYTES).await?;
    }
    send.finish()?;
    let acknowledged: bool = read_frame(&mut recv, CONTROL_BYTES).await?;
    if !acknowledged {
        bail!("peer did not commit records");
    }
    Ok(())
}
async fn incoming(inner: Arc<Inner>, connection: &Connection) -> anyhow::Result<()> {
    let mut handlers = JoinSet::new();
    loop {
        tokio::select! {
            result = handlers.join_next(), if !handlers.is_empty() => {
                result.expect("guarded inbound handlers").map_err(|error| anyhow!("inbound handler: {error}"))??;
            }
            streams = connection.accept_bi(), if handlers.len() < 2 => {
                let (send, recv) = streams?;
                let inner = inner.clone();
                handlers.spawn(async move {
                    tokio::time::timeout(IO_TIMEOUT, incoming_exchange(&inner, send, recv)).await.map_err(|_| anyhow!("inbound synchronization timed out"))?
                });
            }
        }
    }
}

async fn incoming_exchange(
    inner: &Inner,
    mut send: SendStream,
    mut recv: RecvStream,
) -> anyhow::Result<()> {
    let inventory: Vec<InventoryEntry> = read_frame(&mut recv, CONTROL_BYTES).await?;
    if inventory.len() > PAGE_SIZE {
        bail!("inventory page too large");
    }
    let critical = !inventory.is_empty()
        && inventory
            .iter()
            .all(|entry| entry.key.starts_with("0:") || entry.key.starts_with("1:"));
    send.set_priority(if critical { 100 } else { 0 })?;
    #[cfg(test)]
    if !critical && !inventory.is_empty() {
        let gate = inner.test_normal_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.resume.notified().await;
        }
    }
    let wanted = inner.store(move |store| store.needed(&inventory)).await?;
    write_frame(&mut send, &wanted, CONTROL_BYTES).await?;
    for key in wanted {
        let record: Option<StoredRecord> = read_frame(&mut recv, MAX_RECORD_BYTES).await?;
        if let Some(record) = record {
            if record_key(record.record.class, &record.record_id) != key {
                bail!("unexpected record key");
            }
            if inner.store(move |store| store.merge(record)).await? {
                inner.record_changed(key);
            }
        }
    }
    write_frame(&mut send, &true, CONTROL_BYTES).await?;
    send.finish()?;
    Ok(())
}

fn normalized_formation_secret(value: &str) -> Result<[u8; 32]> {
    let decoded = STANDARD
        .decode(value.trim())
        .map_err(|_| MeshNodeError::InvalidFormationSecret)?;
    if decoded.is_empty() {
        return Err(MeshNodeError::InvalidFormationSecret);
    }
    if decoded.len() == 32 {
        return decoded
            .try_into()
            .map_err(|_| MeshNodeError::InvalidFormationSecret);
    }
    Ok(Sha256::digest(decoded).into())
}
fn derive_identity(secret: &[u8; 32], name: &str) -> [u8; 32] {
    let hkdf = Hkdf::<Sha256>::new(None, secret);
    let mut result = [0; 32];
    hkdf.expand(format!("avian:endpoint:v1:{name}").as_bytes(), &mut result)
        .expect("fixed HKDF output length");
    result
}
/// Stable identity derivation; does not bind a socket or open persistent state.
pub fn derive_endpoint_id(base64_shared_key: &str, node_name: &str) -> Result<String> {
    if node_name.trim().is_empty() {
        return Err(MeshNodeError::EmptyNodeName);
    }
    if node_name.len() > 256 {
        return Err(MeshNodeError::IdentityTooLong);
    }
    let secret = normalized_formation_secret(base64_shared_key)?;
    Ok(SecretKey::from_bytes(&derive_identity(&secret, node_name))
        .public()
        .to_string())
}
pub(crate) fn validate_endpoint_id(value: &str) -> Result<()> {
    if value.len() != 64 || value.parse::<iroh::EndpointId>().is_err() {
        return Err(MeshNodeError::InvalidEndpointId);
    }
    Ok(())
}
pub(crate) fn validate_record_id(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || value.contains('\0') {
        return Err(MeshNodeError::InvalidRecordId);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum MeshNodeError {
    #[error("node name cannot be empty")]
    EmptyNodeName,
    #[error("formation ID cannot be empty")]
    EmptyFormationId,
    #[error("node name and formation ID must be at most 256 bytes")]
    IdentityTooLong,
    #[error("formation secret must be non-empty standard base64")]
    InvalidFormationSecret,
    #[error("invalid AVIAN endpoint ID")]
    InvalidEndpointId,
    #[error("invalid peer specification {0:?}; expected NAME=ENDPOINT_ID_HEX@IP:PORT[,IP:PORT...] (NAME= is optional)")]
    InvalidPeerSpec(String),
    #[error("a peer must have between 1 and 8 unique addresses, got {0}")]
    InvalidPeerAddressCount(usize),
    #[error("AVIAN transport did not expose an IP bind address")]
    NoBoundAddress,
    #[error("record ID must contain 1-256 non-NUL characters")]
    InvalidRecordId,
    #[error("record source must contain 1-256 non-whitespace bytes")]
    InvalidRecordSource,
    #[error("payload type does not match its delivery class")]
    PayloadClassMismatch,
    #[error("unsupported AVIAN record schema version {0}")]
    UnsupportedSchema(u16),
    #[error("AVIAN node is shut down")]
    Stopped,
    #[error("AVIAN connection admission is busy")]
    Busy,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use base64::Engine;
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn canceling_shutdown_retains_the_supervisor_until_it_is_joined() {
        let directory = tempfile::TempDir::new().unwrap();
        let key = STANDARD.encode([12; 32]);
        let node = MeshNode::start(MeshNodeConfig {
            name: "shutdown-cancel".into(),
            formation_id: "test".into(),
            base64_shared_key: key.clone(),
            bind_address: "127.0.0.1:0".parse().unwrap(),
            storage_path: directory.path().to_owned(),
        })
        .await
        .unwrap();
        let peer = PeerDescriptor::new(
            "absent",
            derive_endpoint_id(&key, "absent").unwrap(),
            "127.0.0.1:1".parse().unwrap(),
        )
        .unwrap();
        let _ = tokio::time::timeout(Duration::from_millis(25), node.connect(&peer)).await;
        let first = tokio::time::timeout(Duration::from_millis(25), node.shutdown()).await;
        if first.is_err() {
            assert!(
                node.lifecycle.task.lock().await.is_some(),
                "a canceled shutdown must retain supervisor ownership"
            );
        }
        tokio::time::timeout(Duration::from_secs(5), node.shutdown())
            .await
            .expect("second shutdown must finish the original supervisor")
            .unwrap();
        assert!(node.lifecycle.task.lock().await.is_none());
        assert!(node.inner.store.lock().unwrap().is_none());
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn critical_exchange_finishes_while_normal_exchange_is_held() {
        let adir = tempfile::TempDir::new().unwrap();
        let bdir = tempfile::TempDir::new().unwrap();
        let key = STANDARD.encode([13; 32]);
        let config = |name: &str, path: &std::path::Path| MeshNodeConfig {
            name: name.to_owned(),
            formation_id: "priority-test".into(),
            base64_shared_key: key.clone(),
            bind_address: "127.0.0.1:0".parse().unwrap(),
            storage_path: path.to_owned(),
        };
        let a = MeshNode::start(config("a", adir.path())).await.unwrap();
        let b = MeshNode::start(config("b", bdir.path())).await.unwrap();
        let gate = Arc::new(TestExchangeGate {
            entered: tokio::sync::Notify::new(),
            resume: tokio::sync::Notify::new(),
        });
        *b.inner.test_normal_gate.lock().unwrap() = Some(gate.clone());
        let normal = AvianRecord::new(
            mesh_core::NodeId::from("a"),
            1,
            DeliveryClass::Mission,
            100,
            mesh_core::MeshPayload::Mission(mesh_core::MissionState {
                mission_id: uuid::Uuid::from_u128(1),
                objective: "normal catchup".into(),
                generation: 1,
                status: mesh_core::MissionStatus::Active,
            }),
        )
        .unwrap();
        a.put("normal", &normal).await.unwrap();
        a.connect(&b.peer_descriptor().unwrap()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), gate.entered.notified())
            .await
            .expect("normal exchange should be held");
        let command = mesh_core::EmergencyCommand::issue(
            &ed25519_dalek::SigningKey::from_bytes(&[14; 32]),
            uuid::Uuid::from_u128(2),
            mesh_core::NodeId::from("a"),
            mesh_core::NodeId::from("b"),
            1000,
            3000,
            1,
            mesh_core::EmergencyAction::ReturnToLaunch,
        )
        .unwrap();
        let emergency = AvianRecord::new(
            mesh_core::NodeId::from("a"),
            1,
            DeliveryClass::Emergency,
            1000,
            mesh_core::MeshPayload::EmergencyCommand(command),
        )
        .unwrap();
        a.put("urgent", &emergency).await.unwrap();
        tokio::time::timeout(Duration::from_millis(500), async {
            loop {
                if b.get(DeliveryClass::Emergency, "urgent").await.unwrap()
                    == Some(emergency.clone())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("priority stream must bypass the held normal stream");
        assert!(b
            .get(DeliveryClass::Mission, "normal")
            .await
            .unwrap()
            .is_none());
        *b.inner.test_normal_gate.lock().unwrap() = None;
        gate.resume.notify_waiters();
        tokio::time::timeout(Duration::from_secs(2), async {
            while b.get(DeliveryClass::Mission, "normal").await.unwrap() != Some(normal.clone()) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("normal catchup must resume");
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    }
}

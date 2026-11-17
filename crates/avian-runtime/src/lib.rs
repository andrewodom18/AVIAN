//! Owned engine lifecycle with explicit namespace and native execution modes.

use avian_engine::TrustFileExport;
use avian_membership::{MembershipStore, TrustRoot};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::SystemTime,
};
use thiserror::Error;
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::Mutex,
    task::JoinHandle,
    time::{Duration, timeout},
};

mod coordinator;
mod managed;
mod probe_policy;

pub use probe_policy::{eligible_probe_candidate, next_probe_rate};

const ENGINE_COMMIT: &str = "8428a89d2dabc94c97d370ec607c6ca142473626";
const PRIORITY_PATCH: (&str, &str) = (
    "congestion.patch",
    "a311538d7d983af7d3ab389892b2d0dcdaf0e1491c10fa798194210e7471ecc0",
);
const REQUIRED_PATCHES: &[(&str, &str)] = &[
    (
        "verified-membership.patch",
        "c770d4c146dc188a986b271789831e1b04e73c000755ae3dcc45881c6237a077",
    ),
    (
        "authenticated-forwarding.patch",
        "492fe4748a10721b7743c88e26f411cb1c967ab7110bde7e47ac72cea92092ac",
    ),
    (
        "traffic-policy.patch",
        "7535cb49ad05201208c94c36c720442cf4caab434d9ca153c83e4e73a4c53ced",
    ),
    (
        "live-revocation.patch",
        "9226d7de6fa969ca1f147fc6fe49acd9f5abce8b2a1ce55d8b6fc4d5e88c188f",
    ),
    (
        "failover.patch",
        "35b98f0cbae59d69169def105d2c59c15938292fb85e2be526bf71ee24143b48",
    ),
    (
        "topology-policy.patch",
        "0d26b33a228613b07aae0f8eacc8dd171339337fe23eec14778ffe473cb0cac4",
    ),
];

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("runtime configuration rejected: {0}")]
    Configuration(String),
    #[error("isolated runtime control is supported only on Linux")]
    UnsupportedPlatform,
    #[error("engine did not become ready")]
    StartupFailed,
    #[error("runtime is not running")]
    NotRunning,
    #[error("engine inspection failed")]
    InspectionFailed,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Toml(#[from] toml::ser::Error),
    #[error(transparent)]
    Membership(#[from] avian_membership::MembershipError),
    #[error(transparent)]
    Export(#[from] avian_engine::ExportError),
}
pub type Result<T> = std::result::Result<T, RuntimeError>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    #[default]
    IsolatedNamespace,
    Native,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkProbeRequest {
    pub peer_id: u32,
    pub connection_id: Option<String>,
    #[serde(default = "default_probe_duration")]
    pub duration_ms: u16,
    #[serde(default = "default_probe_rate")]
    pub offered_bps: u64,
    #[serde(default)]
    pub allow_metered: bool,
}
fn default_probe_duration() -> u16 {
    500
}
fn default_probe_rate() -> u64 {
    8_000_000
}
fn default_probe_budget() -> u64 {
    512 * 1024
}
fn default_probe_window() -> u64 {
    30_000
}
fn default_automatic_probes() -> bool {
    true
}
struct ProbeBudget {
    started: tokio::time::Instant,
    used_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeCapabilities {
    pub platform: &'static str,
    pub native_execution: bool,
    pub namespace_execution: bool,
    pub hardware_qualified: bool,
}

pub fn capabilities() -> RuntimeCapabilities {
    RuntimeCapabilities {
        platform: std::env::consts::OS,
        native_execution: cfg!(target_os = "linux"),
        namespace_execution: cfg!(target_os = "linux"),
        hardware_qualified: false,
    }
}
fn default_connection_budget() -> usize {
    1024
}
fn default_discovery_budget() -> usize {
    4096
}
fn default_interface_name() -> String {
    "avian0".into()
}
fn default_prefix_length() -> u8 {
    24
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfiguration {
    #[serde(default)]
    pub execution: ExecutionMode,
    #[serde(default)]
    pub network_helper_socket: Option<PathBuf>,
    #[serde(default = "default_interface_name")]
    pub interface_name: String,
    #[serde(default = "default_prefix_length")]
    pub overlay_prefix_length: u8,
    #[serde(default)]
    pub bootstrap: avian_connectivity::BootstrapConfiguration,
    #[serde(default)]
    pub measured_routing: bool,
    #[serde(default)]
    pub private_rpc: bool,
    #[serde(default)]
    pub underlay_binding: bool,
    #[serde(default = "default_connection_budget")]
    pub connection_budget: usize,
    #[serde(default = "default_discovery_budget")]
    pub discovery_hint_budget: usize,
    #[serde(default = "default_automatic_probes")]
    pub automatic_capacity_probes: bool,
    #[serde(default = "default_probe_budget")]
    pub probe_payload_budget_bytes: u64,
    #[serde(default = "default_probe_window")]
    pub probe_window_ms: u64,
    pub isolation_required: bool,
    #[serde(default)]
    pub namespace_name: String,
    pub engine_directory: PathBuf,
    pub runtime_directory: PathBuf,
    pub membership_file: PathBuf,
    pub trust_root_file: PathBuf,
    pub credential_file: PathBuf,
    pub node_id: String,
    pub overlay_address: Ipv4Addr,
    pub listen: SocketAddr,
    pub rpc_listen: SocketAddr,
    #[serde(default)]
    pub peers: Vec<SocketAddr>,
    #[serde(default)]
    pub metered_addresses: Vec<IpAddr>,
    #[serde(default)]
    pub control_tcp_ports: Vec<u16>,
    #[serde(default)]
    pub allow_metered_video: bool,
    #[serde(default)]
    pub allow_metered_bulk: bool,
    #[serde(default)]
    pub fast_failover: bool,
    #[serde(default)]
    pub priority_forwarding: bool,
}

fn private_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.is_private() || address.is_loopback(),
        IpAddr::V6(address) => address.is_loopback() || address.segments()[0] & 0xfe00 == 0xfc00,
    }
}

impl RuntimeConfiguration {
    pub fn resolve_paths(&mut self, base: &Path) {
        if let Some(path) = &mut self.network_helper_socket
            && path.is_relative()
        {
            *path = base.join(&*path);
        }
        for path in [
            &mut self.engine_directory,
            &mut self.runtime_directory,
            &mut self.membership_file,
            &mut self.trust_root_file,
            &mut self.credential_file,
        ] {
            if path.is_relative() {
                *path = base.join(&*path);
            }
        }
    }
    pub fn validate(&self) -> Result<()> {
        managed::validate_configuration(self)?;
        if self.probe_window_ms < 30_000
            || self.probe_payload_budget_bytes == 0
            || self.probe_payload_budget_bytes > u64::MAX / 16
        {
            return Err(rejected("invalid process-wide probe budget"));
        }
        if self.connection_budget == 0 || self.discovery_hint_budget == 0 {
            return Err(rejected("connection resource budgets must be positive"));
        }
        if self.execution == ExecutionMode::Native
            && !(self.measured_routing
                && self.underlay_binding
                && self.priority_forwarding
                && self.private_rpc)
        {
            return Err(rejected(
                "native execution requires measured routing, bound underlays, and priority forwarding",
            ));
        }
        if self.underlay_binding && !self.measured_routing {
            return Err(rejected("underlay binding requires measured routing"));
        }
        if (self.execution == ExecutionMode::IsolatedNamespace) != self.isolation_required {
            return Err(rejected(
                "execution mode must agree with isolation_required",
            ));
        }
        self.bootstrap
            .validate()
            .map_err(|_| rejected("invalid bootstrap configuration"))?;
        if !(8..=30).contains(&self.overlay_prefix_length)
            || self.interface_name.is_empty()
            || self.interface_name.len() > 15
            || !self
                .interface_name
                .bytes()
                .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'-' | b'_'))
        {
            return Err(rejected("invalid owned interface or overlay prefix"));
        }
        if self.execution == ExecutionMode::Native && !self.namespace_name.is_empty() {
            return Err(rejected("native execution cannot select a namespace"));
        }
        if self.execution == ExecutionMode::IsolatedNamespace
            && (self.namespace_name.is_empty()
                || self.namespace_name.len() > 63
                || !self
                    .namespace_name
                    .bytes()
                    .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'-' | b'_')))
        {
            return Err(rejected("invalid namespace name"));
        }
        if self.node_id.len() != 64 || hex::decode(&self.node_id).is_err() {
            return Err(rejected("invalid node identity"));
        }
        if !self.overlay_address.is_private() {
            return Err(rejected("overlay address must be private IPv4"));
        }
        if (self.execution == ExecutionMode::IsolatedNamespace
            && !private_address(self.listen.ip()))
            || (self.execution == ExecutionMode::IsolatedNamespace
                && self.listen.ip().is_unspecified())
            || self.listen.ip().is_multicast()
            || self.listen.port() == 0
            || self.listen.ip().is_loopback()
        {
            return Err(rejected(
                "engine listener requires an explicit private underlay address",
            ));
        }
        if !self.rpc_listen.ip().is_loopback() || self.rpc_listen.port() == 0 {
            return Err(rejected("engine RPC must use loopback and a nonzero port"));
        }
        if self.peers.iter().any(|peer| {
            (self.execution == ExecutionMode::IsolatedNamespace && !private_address(peer.ip()))
                || peer.ip().is_unspecified()
                || peer.ip().is_multicast()
                || peer.port() == 0
        }) || self
            .metered_addresses
            .iter()
            .any(|address| !private_address(*address))
            || self.control_tcp_ports.contains(&0)
        {
            return Err(rejected("invalid peer, metered address, or control port"));
        }
        for path in [
            &self.engine_directory,
            &self.runtime_directory,
            &self.membership_file,
            &self.trust_root_file,
            &self.credential_file,
        ] {
            if !path.is_absolute() {
                return Err(rejected(
                    "runtime paths must be absolute or resolved against configuration",
                ));
            }
        }
        Ok(())
    }
    fn rpc_token_file(&self) -> PathBuf {
        self.runtime_directory.join("rpc-token")
    }
    fn policy_file(&self) -> PathBuf {
        self.runtime_directory.join("traffic-policy.json")
    }
    fn trust_file(&self) -> PathBuf {
        self.runtime_directory.join("engine-trust.json")
    }
    fn config_file(&self) -> PathBuf {
        self.runtime_directory.join("engine.toml")
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePhase {
    Stopped,
    Starting,
    Running,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeStatus {
    pub execution: ExecutionMode,
    pub capabilities: RuntimeCapabilities,
    pub state: RuntimePhase,
    pub process_id: Option<u32>,
    pub node_id: String,
    pub overlay_address: Ipv4Addr,
    pub namespace_name: String,
    pub isolated: bool,
    pub engine_ready: bool,
    pub last_error: Option<String>,
}

struct Lifecycle {
    child: Option<Child>,
    renewal: Option<JoinHandle<()>>,
    coordinator: Option<JoinHandle<()>>,
    helper_socket: Option<PathBuf>,
    cleanup: Option<Arc<CleanupCompletion>>,
    generation: u64,
    phase: RuntimePhase,
    last_error: Option<String>,
}

pub struct RuntimeSupervisor {
    configuration: RuntimeConfiguration,
    lifecycle: Arc<Mutex<Lifecycle>>,
    probe_budget: Arc<Mutex<ProbeBudget>>,
    probe_slot: Arc<tokio::sync::Semaphore>,
    helper_slot: Arc<tokio::sync::Semaphore>,
    background_abort: Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>,
    closing: Arc<std::sync::atomic::AtomicBool>,
}

impl RuntimeSupervisor {
    pub fn new(configuration: RuntimeConfiguration) -> Self {
        Self {
            configuration,
            probe_budget: Arc::new(Mutex::new(ProbeBudget {
                started: tokio::time::Instant::now(),
                used_bytes: 0,
            })),
            probe_slot: Arc::new(tokio::sync::Semaphore::new(1)),
            helper_slot: Arc::new(tokio::sync::Semaphore::new(1)),
            background_abort: Arc::new(std::sync::Mutex::new(Vec::new())),
            closing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            lifecycle: Arc::new(Mutex::new(Lifecycle {
                child: None,
                renewal: None,
                coordinator: None,
                helper_socket: None,
                cleanup: None,
                generation: 0,
                phase: RuntimePhase::Stopped,
                last_error: None,
            })),
        }
    }
    pub fn configuration(&self) -> &RuntimeConfiguration {
        &self.configuration
    }

    pub async fn start(&self) -> Result<RuntimeStatus> {
        let runtime = lifecycle_runtime()?;
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let owns_start = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (finished, completion) = tokio::sync::oneshot::channel();
        let operation = StartupOperation {
            configuration: self.configuration.clone(),
            lifecycle: self.lifecycle.clone(),
            probe_budget: self.probe_budget.clone(),
            probe_slot: self.probe_slot.clone(),
            helper_slot: self.helper_slot.clone(),
            background_abort: self.background_abort.clone(),
            closing: self.closing.clone(),
            cancelled: cancelled.clone(),
            generation: generation.clone(),
            owns_start: owns_start.clone(),
        };
        let mut cancellation = StartupCancellation {
            armed: true,
            cancelled,
            generation,
            owns_start,
            completion: Some(completion),
            lifecycle: self.lifecycle.clone(),
            configuration: self.configuration.clone(),
        };
        let task = runtime.spawn(async move {
            let result = operation.run().await;
            let _ = finished.send(());
            result
        });
        let result = task
            .await
            .map_err(|_| rejected("owned startup operation failed"))?;
        cancellation.armed = false;
        result
    }

    pub async fn stop(&self) -> Result<RuntimeStatus> {
        let configuration = self.configuration.clone();
        let lifecycle = self.lifecycle.clone();
        lifecycle_runtime()?
            .spawn(async move {
                let mut lifecycle = lifecycle.lock().await;
                cleanup_runtime(&mut lifecycle, &configuration).await?;
                lifecycle.phase = RuntimePhase::Stopped;
                lifecycle.last_error = None;
                Ok(describe(&configuration, &lifecycle))
            })
            .await
            .map_err(|_| rejected("owned stop operation failed"))?
    }

    pub async fn status(&self) -> RuntimeStatus {
        let mut lifecycle = self.lifecycle.lock().await;
        if let Some(child) = &mut lifecycle.child
            && !matches!(child.try_wait(), Ok(None))
            && stop_locked(&mut lifecycle).await.is_ok()
        {
            lifecycle.phase = RuntimePhase::Failed;
            lifecycle.last_error = Some("engine process exited".into());
        }
        let mut status = self.describe(&lifecycle);
        if status.engine_ready {
            status.engine_ready = inspect(&self.configuration, "node")
                .await
                .is_ok_and(|node| node_matches(&self.configuration, &node));
            if !status.engine_ready {
                status.last_error =
                    Some("owned engine RPC is unavailable or has another identity".into());
            }
        }
        status
    }

    pub async fn inspect_underlays(&self) -> Result<Vec<avian_connectivity::Underlay>> {
        collect_underlays(&self.configuration).await
    }

    pub async fn inspect_class_routes(&self) -> Result<serde_json::Value> {
        self.inspect_class_routes_page(None, 100, None).await
    }
    pub async fn inspect_class_routes_page(
        &self,
        after: Option<&str>,
        limit: usize,
        class: Option<avian_routing::TrafficClass>,
    ) -> Result<serde_json::Value> {
        if !self.configuration.measured_routing {
            return Err(rejected("measured route inspection is unavailable"));
        }
        let mut arguments = page_arguments("measured", after, limit)?;
        if let Some(class) = class {
            arguments.extend([
                "--class".into(),
                serde_json::to_value(class)?
                    .as_str()
                    .ok_or(RuntimeError::InspectionFailed)?
                    .to_owned(),
            ]);
        }
        self.inspect_owned_arguments(&arguments).await
    }
    pub async fn inspect_links(&self) -> Result<serde_json::Value> {
        self.inspect_links_page(None, 100).await
    }
    pub async fn inspect_links_page(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> Result<serde_json::Value> {
        if !self.configuration.measured_routing {
            return Err(rejected("link inspection requires measured routing"));
        }
        self.inspect_owned_arguments(&page_arguments("links", after, limit)?)
            .await
    }
    pub async fn probe_link(&self, request: LinkProbeRequest) -> Result<serde_json::Value> {
        perform_probe(
            &self.configuration,
            &self.lifecycle,
            &self.probe_budget,
            &self.probe_slot,
            request,
        )
        .await
    }
    async fn inspect_owned_arguments(&self, arguments: &[String]) -> Result<serde_json::Value> {
        let mut lifecycle = self.lifecycle.lock().await;
        if lifecycle.phase != RuntimePhase::Running
            || lifecycle
                .child
                .as_mut()
                .is_none_or(|child| !matches!(child.try_wait(), Ok(None)))
        {
            return Err(RuntimeError::NotRunning);
        }
        if !inspect(&self.configuration, "node")
            .await
            .is_ok_and(|node| node_matches(&self.configuration, &node))
        {
            return Err(RuntimeError::InspectionFailed);
        }
        let result =
            inspect_arguments(&self.configuration, arguments, Duration::from_secs(1)).await?;
        if lifecycle
            .child
            .as_mut()
            .is_none_or(|child| !matches!(child.try_wait(), Ok(None)))
            || !inspect(&self.configuration, "node")
                .await
                .is_ok_and(|node| node_matches(&self.configuration, &node))
        {
            return Err(RuntimeError::InspectionFailed);
        }
        Ok(result)
    }

    pub async fn inspect_peers(&self) -> Result<serde_json::Value> {
        self.inspect_running("peer").await
    }
    pub async fn inspect_routes(&self) -> Result<serde_json::Value> {
        self.inspect_running("route").await
    }
    async fn inspect_running(&self, operation: &str) -> Result<serde_json::Value> {
        let mut lifecycle = self.lifecycle.lock().await;
        if lifecycle.phase != RuntimePhase::Running {
            return Err(RuntimeError::NotRunning);
        }
        let mut inspected_response = None;
        for inspection in ["node", operation, "node"] {
            if !lifecycle
                .child
                .as_mut()
                .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
            {
                stop_locked(&mut lifecycle).await?;
                lifecycle.phase = RuntimePhase::Failed;
                lifecycle.last_error = Some("engine process exited".into());
                return Err(RuntimeError::NotRunning);
            }
            let response = inspect(&self.configuration, inspection).await?;
            if inspection == "node" && !node_matches(&self.configuration, &response) {
                return Err(RuntimeError::InspectionFailed);
            }
            if inspection == operation {
                inspected_response = Some(response);
            }
        }
        if !lifecycle
            .child
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
        {
            return Err(RuntimeError::NotRunning);
        }
        inspected_response.ok_or(RuntimeError::InspectionFailed)
    }
    fn describe(&self, lifecycle: &Lifecycle) -> RuntimeStatus {
        describe(&self.configuration, lifecycle)
    }
}

impl Drop for RuntimeSupervisor {
    fn drop(&mut self) {
        self.closing
            .store(true, std::sync::atomic::Ordering::SeqCst);
        for task in self
            .background_abort
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain(..)
        {
            task.abort();
        }
        let lifecycle = self.lifecycle.clone();
        let configuration = self.configuration.clone();
        if let Ok(runtime) = lifecycle_runtime() {
            runtime.spawn(async move {
                let mut lifecycle = lifecycle.lock().await;
                let _ = cleanup_runtime(&mut lifecycle, &configuration).await;
            });
        }
    }
}

fn describe(configuration: &RuntimeConfiguration, lifecycle: &Lifecycle) -> RuntimeStatus {
    RuntimeStatus {
        execution: configuration.execution,
        capabilities: capabilities(),
        state: lifecycle.phase.clone(),
        process_id: lifecycle.child.as_ref().and_then(Child::id),
        node_id: configuration.node_id.clone(),
        overlay_address: configuration.overlay_address,
        namespace_name: configuration.namespace_name.clone(),
        isolated: lifecycle.phase == RuntimePhase::Running
            && configuration.execution == ExecutionMode::IsolatedNamespace,
        engine_ready: lifecycle.phase == RuntimePhase::Running,
        last_error: lifecycle.last_error.clone(),
    }
}

// Lifecycle work belongs to the process, rather than a cancelled API request or
// a caller's short-lived reactor. A single dedicated event loop serves all supervisors.
fn lifecycle_runtime() -> Result<&'static tokio::runtime::Handle> {
    static HANDLE: std::sync::OnceLock<std::result::Result<tokio::runtime::Handle, ()>> =
        std::sync::OnceLock::new();
    HANDLE
        .get_or_init(|| {
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("avian-lifecycle".into())
                .spawn(move || {
                    match tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        Ok(runtime) => {
                            let _ = sender.send(Ok(runtime.handle().clone()));
                            runtime.block_on(std::future::pending::<()>());
                        }
                        Err(_) => {
                            let _ = sender.send(Err(()));
                        }
                    }
                })
                .map_err(|_| ())?;
            receiver.recv().map_err(|_| ())?
        })
        .as_ref()
        .map_err(|_| rejected("lifecycle runtime unavailable"))
}

struct StartupOperation {
    configuration: RuntimeConfiguration,
    lifecycle: Arc<Mutex<Lifecycle>>,
    probe_budget: Arc<Mutex<ProbeBudget>>,
    probe_slot: Arc<tokio::sync::Semaphore>,
    helper_slot: Arc<tokio::sync::Semaphore>,
    background_abort: Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>,
    closing: Arc<std::sync::atomic::AtomicBool>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    generation: Arc<std::sync::atomic::AtomicU64>,
    owns_start: Arc<std::sync::atomic::AtomicBool>,
}
impl StartupOperation {
    fn cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::SeqCst)
            || self.closing.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn describe(&self, lifecycle: &Lifecycle) -> RuntimeStatus {
        describe(&self.configuration, lifecycle)
    }
    async fn run(&self) -> Result<RuntimeStatus> {
        let mut lifecycle = self.lifecycle.lock().await;
        if self.cancelled() {
            return Err(rejected("runtime startup cancelled"));
        }
        lifecycle.generation = lifecycle.generation.wrapping_add(1);
        self.generation
            .store(lifecycle.generation, std::sync::atomic::Ordering::SeqCst);
        if lifecycle.phase == RuntimePhase::Running
            && lifecycle
                .child
                .as_mut()
                .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
        {
            let mut status = self.describe(&lifecycle);
            status.engine_ready = inspect(&self.configuration, "node")
                .await
                .is_ok_and(|node| node_matches(&self.configuration, &node));
            if !status.engine_ready {
                status.last_error =
                    Some("owned engine RPC is unavailable or has another identity".into());
            }
            return Ok(status);
        }
        self.owns_start
            .store(true, std::sync::atomic::Ordering::SeqCst);
        stop_locked(&mut lifecycle).await?;
        lifecycle.phase = RuntimePhase::Starting;
        lifecycle.last_error = None;
        let configuration = self.configuration.clone();
        let prepared = tokio::task::spawn_blocking(move || prepare(&configuration))
            .await
            .map_err(|_| rejected("runtime preparation could not complete"))
            .and_then(|prepared| prepared);
        let (membership, credential) = match prepared {
            Ok(prepared) => prepared,
            Err(problem) => {
                cleanup_runtime(&mut lifecycle, &self.configuration).await?;
                lifecycle.phase = RuntimePhase::Failed;
                lifecycle.last_error = Some(problem.to_string());
                return Err(problem);
            }
        };
        if self.cancelled() {
            cleanup_runtime_files(&self.configuration);
            lifecycle.phase = RuntimePhase::Stopped;
            return Err(rejected("runtime startup cancelled"));
        }
        if inspect(&self.configuration, "node").await.is_ok() {
            cleanup_runtime_files(&self.configuration);
            lifecycle.phase = RuntimePhase::Failed;
            lifecycle.last_error = Some("configured RPC portal already serves a process".into());
            return Err(rejected("configured RPC portal already serves a process"));
        }
        if self.configuration.network_helper_socket.is_some() {
            let lease =
                match managed::prepare_network(&self.configuration, self.helper_slot.clone()).await
                {
                    Ok(lease) => lease,
                    Err(problem) => {
                        cleanup_runtime_files(&self.configuration);
                        lifecycle.phase = RuntimePhase::Failed;
                        lifecycle.last_error = Some(problem.to_string());
                        return Err(problem);
                    }
                };
            let (socket, error) = lease.commit();
            // A failed response can follow successful helper-side preparation.
            // Record that scope before attempting cleanup, retaining it on failure.
            lifecycle.helper_socket = Some(socket);
            if let Some(message) = error {
                cleanup_runtime(&mut lifecycle, &self.configuration).await?;
                lifecycle.phase = RuntimePhase::Failed;
                lifecycle.last_error = Some(message.into());
                return Err(rejected(message));
            }
        }
        if self.cancelled() {
            cleanup_runtime(&mut lifecycle, &self.configuration).await?;
            return Err(rejected("runtime startup cancelled"));
        }
        let launched = (|| -> Result<Child> {
            let log = private_log(&self.configuration.runtime_directory.join("engine.log"))?;
            let mut command = namespace_command(&self.configuration, "easytier-core");
            command
                .arg("--config-file")
                .arg(self.configuration.config_file())
                .arg("--rpc-portal")
                .arg(self.configuration.rpc_listen.to_string())
                .arg("--console-log-level")
                .arg("warn")
                .env("ET_CREDENTIAL", credential.as_str())
                .env("ET_REQUIRE_VERIFIED_MEMBERSHIP", "1")
                .env(
                    "AVIAN_TRAFFIC_POLICY",
                    serde_json::to_string(&serde_json::json!({
                        "metered_addresses": self.configuration.metered_addresses,
                        "control_tcp_ports": self.configuration.control_tcp_ports,
                        "allow_metered_video": self.configuration.allow_metered_video,
                        "allow_metered_bulk": self.configuration.allow_metered_bulk,
                    }))?,
                )
                .stdout(Stdio::from(log.try_clone()?))
                .stderr(Stdio::from(log))
                .kill_on_drop(true);
            if let Some(socket) = &self.configuration.network_helper_socket {
                command
                    .env("AVIAN_NETWORK_HELPER_SOCKET", socket)
                    .env("AVIAN_OWNED_CONNECTIVITY_ONLY", "1");
            }
            if self.configuration.underlay_binding {
                command.env(
                    "AVIAN_TRAFFIC_POLICY_FILE",
                    self.configuration.policy_file(),
                );
            }
            if self.configuration.fast_failover {
                command.env("ET_FAST_FAILOVER", "1");
            }
            if self.configuration.priority_forwarding {
                command.env("ET_PRIORITY_FORWARDING", "1");
            }
            if self.configuration.measured_routing {
                command.env("ET_MEASURED_ROUTING", "1");
            }
            Ok(command.spawn()?)
        })();
        let child = match launched {
            Ok(child) => child,
            Err(problem) => {
                cleanup_runtime(&mut lifecycle, &self.configuration).await?;
                lifecycle.phase = RuntimePhase::Failed;
                lifecycle.last_error = Some(problem.to_string());
                return Err(problem);
            }
        };
        lifecycle.child = Some(child);
        let mut ready = false;
        let readiness_deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        while tokio::time::Instant::now() < readiness_deadline && !self.cancelled() {
            if lifecycle
                .child
                .as_mut()
                .is_none_or(|child| !matches!(child.try_wait(), Ok(None)))
            {
                break;
            }
            if let Ok(node) = inspect(&self.configuration, "node").await
                && node_matches(&self.configuration, &node)
                && lifecycle
                    .child
                    .as_mut()
                    .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
            {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if !ready {
            cleanup_runtime(&mut lifecycle, &self.configuration).await?;
            lifecycle.phase = RuntimePhase::Failed;
            lifecycle.last_error =
                Some("engine did not become ready; inspect the private engine log".into());
            return Err(RuntimeError::StartupFailed);
        }
        // The engine has loaded its protected startup configuration. Keep plaintext
        // transport material in memory rather than retaining a second on-disk copy.
        if fs::remove_file(self.configuration.config_file()).is_err() {
            cleanup_runtime(&mut lifecycle, &self.configuration).await?;
            lifecycle.phase = RuntimePhase::Failed;
            return Err(rejected(
                "private startup configuration could not be removed",
            ));
        }
        let network_id = membership.trust_root().network_id.clone();
        let configuration = self.configuration.clone();
        lifecycle.renewal = Some(tokio::spawn(async move {
            let mut previous = membership;
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut last_revision = previous.revocation_revision();
            let mut last_publication = tokio::time::Instant::now();
            loop {
                interval.tick().await;
                if let Ok(current) = MembershipStore::load(
                    &configuration.membership_file,
                    previous.trust_root().clone(),
                ) && current.validate_successor(&previous).is_ok()
                    && (current.revocation_revision() != last_revision
                        || last_publication.elapsed() >= Duration::from_secs(30))
                    && let Ok(export) =
                        TrustFileExport::from_membership(&current, SystemTime::now())
                    && export
                        .write_startup_file(configuration.trust_file())
                        .is_ok()
                {
                    last_revision = current.revocation_revision();
                    last_publication = tokio::time::Instant::now();
                    previous = current;
                }
            }
        }));
        lifecycle.phase = RuntimePhase::Running;
        if self.configuration.underlay_binding {
            lifecycle.coordinator = Some(tokio::spawn(coordinator::run(
                self.configuration.clone(),
                self.lifecycle.clone(),
                network_id,
                self.probe_budget.clone(),
                self.probe_slot.clone(),
            )));
        }
        *self
            .background_abort
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            [lifecycle.coordinator.as_ref(), lifecycle.renewal.as_ref()]
                .into_iter()
                .flatten()
                .map(JoinHandle::abort_handle)
                .collect();
        Ok(self.describe(&lifecycle))
    }
}

struct StartupCancellation {
    armed: bool,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    generation: Arc<std::sync::atomic::AtomicU64>,
    owns_start: Arc<std::sync::atomic::AtomicBool>,
    completion: Option<tokio::sync::oneshot::Receiver<()>>,
    lifecycle: Arc<Mutex<Lifecycle>>,
    configuration: RuntimeConfiguration,
}
impl Drop for StartupCancellation {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let completion = self.completion.take();
        let generation = self.generation.clone();
        let owns_start = self.owns_start.clone();
        let lifecycle = self.lifecycle.clone();
        let configuration = self.configuration.clone();
        if let Ok(runtime) = lifecycle_runtime() {
            runtime.spawn(async move {
                if let Some(completion) = completion {
                    let _ = completion.await;
                }
                if !owns_start.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                let mut lifecycle = lifecycle.lock().await;
                if lifecycle.generation == generation.load(std::sync::atomic::Ordering::SeqCst) {
                    let _ = cleanup_runtime(&mut lifecycle, &configuration).await;
                }
            });
        }
    }
}

struct CleanupCompletion {
    done: std::sync::atomic::AtomicBool,
    changed: tokio::sync::Notify,
    failure: std::sync::Mutex<Option<CleanupFailure>>,
}
struct CleanupFailure {
    child: Option<Child>,
    helper_socket: Option<PathBuf>,
    message: &'static str,
}

async fn cleanup_runtime(
    lifecycle: &mut Lifecycle,
    configuration: &RuntimeConfiguration,
) -> Result<()> {
    let result = stop_locked(lifecycle).await;
    cleanup_runtime_files(configuration);
    result
}

async fn stop_locked(lifecycle: &mut Lifecycle) -> Result<()> {
    if let Some(coordinator) = lifecycle.coordinator.take() {
        coordinator.abort();
    }
    if let Some(renewal) = lifecycle.renewal.take() {
        renewal.abort();
    }
    lifecycle.phase = RuntimePhase::Stopped;
    if lifecycle.child.is_some() || lifecycle.helper_socket.is_some() {
        let child = lifecycle.child.take();
        let socket = lifecycle.helper_socket.take();
        let completion = Arc::new(CleanupCompletion {
            done: std::sync::atomic::AtomicBool::new(false),
            changed: tokio::sync::Notify::new(),
            failure: std::sync::Mutex::new(None),
        });
        lifecycle.cleanup = Some(completion.clone());
        lifecycle_runtime()
            .expect("active lifecycle has a runtime")
            .spawn(async move {
                let failure = cleanup_owned(child, socket).await;
                *completion
                    .failure
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = failure;
                completion
                    .done
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                completion.changed.notify_waiters();
            });
    }
    if let Some(completion) = &lifecycle.cleanup {
        loop {
            let changed = completion.changed.notified();
            if completion.done.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            changed.await;
        }
    }
    let failure = lifecycle.cleanup.take().and_then(|completion| {
        completion
            .failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    });
    if let Some(failure) = failure {
        lifecycle.child = failure.child;
        lifecycle.helper_socket = failure.helper_socket;
        lifecycle.phase = RuntimePhase::Failed;
        lifecycle.last_error = Some(failure.message.into());
        return Err(rejected(failure.message));
    }
    Ok(())
}

async fn cleanup_owned(
    mut child: Option<Child>,
    socket: Option<PathBuf>,
) -> Option<CleanupFailure> {
    if let Some(child) = child.as_mut() {
        let _ = child.start_kill();
        let _ = timeout(Duration::from_secs(3), child.wait()).await;
        let reaping_deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while matches!(child.try_wait(), Ok(None)) && tokio::time::Instant::now() < reaping_deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    if child
        .as_mut()
        .is_some_and(|child| !matches!(child.try_wait(), Ok(Some(_))))
    {
        return Some(CleanupFailure {
            child,
            helper_socket: socket,
            message: "engine could not be reaped; owned scope retained for retry",
        });
    }
    if let Some(socket) = socket
        && managed::release_network(socket.clone()).await.is_err()
    {
        return Some(CleanupFailure {
            child: None,
            helper_socket: Some(socket),
            message: "network helper release failed; owned scope retained for retry",
        });
    }
    None
}

fn page_arguments(operation: &str, after: Option<&str>, limit: usize) -> Result<Vec<String>> {
    if !(1..=1000).contains(&limit)
        || after.is_some_and(|cursor| cursor.len() > 256 || cursor.chars().any(char::is_control))
    {
        return Err(rejected("invalid route observation page"));
    }
    let mut arguments = vec![
        "route".into(),
        operation.into(),
        "--limit".into(),
        limit.to_string(),
    ];
    if let Some(cursor) = after {
        arguments.push(format!("--after={cursor}"));
    }
    Ok(arguments)
}

async fn perform_probe(
    configuration: &RuntimeConfiguration,
    lifecycle: &Arc<Mutex<Lifecycle>>,
    probe_budget: &Arc<Mutex<ProbeBudget>>,
    probe_slot: &Arc<tokio::sync::Semaphore>,
    request: LinkProbeRequest,
) -> Result<serde_json::Value> {
    if !configuration.measured_routing {
        return Err(rejected("bounded probes require measured routing"));
    }
    let _permit = probe_slot
        .clone()
        .try_acquire_owned()
        .map_err(|_| rejected("a capacity probe is already running"))?;
    let bytes = request
        .offered_bps
        .checked_mul(u64::from(request.duration_ms))
        .and_then(|bits| bits.checked_add(7999))
        .map(|bits| bits / 8000)
        .ok_or_else(|| rejected("invalid probe byte request"))?;
    if request.peer_id == 0
        || !(50..=500).contains(&request.duration_ms)
        || request.offered_bps < 64_000
        || request.offered_bps > 8_000_000
        || bytes > configuration.probe_payload_budget_bytes
        || request.connection_id.as_ref().is_some_and(|id| {
            id.len() != 36
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        })
    {
        return Err(rejected("invalid bounded probe request"));
    }
    let mut budget = probe_budget.lock().await;
    if budget.started.elapsed() >= Duration::from_millis(configuration.probe_window_ms) {
        budget.started = tokio::time::Instant::now();
        budget.used_bytes = 0;
    }
    if budget.used_bytes.saturating_add(bytes) > configuration.probe_payload_budget_bytes {
        return Err(rejected("process-wide capacity probe budget exhausted"));
    }
    budget.used_bytes += bytes;
    drop(budget);
    let mut lifecycle = lifecycle.lock().await;
    if lifecycle.phase != RuntimePhase::Running
        || lifecycle
            .child
            .as_mut()
            .is_none_or(|child| !matches!(child.try_wait(), Ok(None)))
    {
        return Err(RuntimeError::NotRunning);
    }
    if !inspect(configuration, "node")
        .await
        .is_ok_and(|node| node_matches(configuration, &node))
    {
        return Err(RuntimeError::InspectionFailed);
    }
    let peer = request.peer_id.to_string();
    let duration = request.duration_ms.to_string();
    let rate = request.offered_bps.to_string();
    let mut arguments = vec![
        "route".to_owned(),
        "probe".to_owned(),
        "--peer-id".to_owned(),
        peer,
        "--duration-ms".to_owned(),
        duration,
        "--offered-bps".to_owned(),
        rate,
    ];
    if let Some(connection) = request.connection_id {
        arguments.extend(["--connection-id".into(), connection]);
    }
    if request.allow_metered {
        arguments.push("--allow-metered".into());
    }
    let result = inspect_arguments(configuration, &arguments, Duration::from_secs(3)).await?;
    if lifecycle
        .child
        .as_mut()
        .is_none_or(|child| !matches!(child.try_wait(), Ok(None)))
    {
        return Err(RuntimeError::NotRunning);
    }
    Ok(result)
}

async fn collect_underlays(
    configuration: &RuntimeConfiguration,
) -> Result<Vec<avian_connectivity::Underlay>> {
    configuration.validate()?;
    let mut underlays = match configuration.execution {
        ExecutionMode::IsolatedNamespace => {
            validate_namespace(&configuration.namespace_name)?;
            avian_connectivity::inventory::collect_namespace_interfaces(
                &configuration.namespace_name,
            )
            .await
        }
        ExecutionMode::Native => avian_connectivity::inventory::collect_interfaces().await,
    }
    .map_err(|_| rejected("fresh interface inventory unavailable"))?;
    for underlay in &mut underlays {
        if let Some(policy) = configuration
            .bootstrap
            .metering
            .get(&underlay.interface_name)
        {
            underlay.metering = *policy;
        }
        if underlay
            .addresses
            .iter()
            .any(|address| configuration.metered_addresses.contains(address))
        {
            underlay.metering = avian_connectivity::Metering::Metered;
        }
    }
    underlays.retain(|underlay| underlay.interface_name != configuration.interface_name);
    Ok(underlays)
}

fn cleanup_runtime_files(configuration: &RuntimeConfiguration) {
    let marker = configuration.runtime_directory.join(".avian-runtime");
    let Ok(contents) = avian_storage::read_private_file(marker, 128) else {
        return;
    };
    if contents.as_slice() != b"AVIAN isolated runtime v1\n"
        && contents.as_slice() != b"AVIAN runtime v1\n"
    {
        return;
    }
    for path in [
        configuration.config_file(),
        configuration.rpc_token_file(),
        configuration.policy_file(),
        configuration.trust_file(),
    ] {
        let _ = fs::remove_file(path);
    }
}

fn engine_binary(configuration: &RuntimeConfiguration, name: &str) -> PathBuf {
    configuration.engine_directory.join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.into()
    })
}

fn namespace_command(configuration: &RuntimeConfiguration, binary: &str) -> Command {
    let executable = engine_binary(configuration, binary);
    let mut command = match configuration.execution {
        ExecutionMode::IsolatedNamespace => {
            let mut command = Command::new("/usr/sbin/ip");
            command
                .args(["netns", "exec", &configuration.namespace_name])
                .arg(executable);
            command
        }
        ExecutionMode::Native => Command::new(executable),
    };
    command
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin");
    if configuration.private_rpc {
        command.env("AVIAN_RPC_TOKEN_FILE", configuration.rpc_token_file());
    }
    #[cfg(windows)]
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
}

async fn inspect(
    configuration: &RuntimeConfiguration,
    operation: &str,
) -> Result<serde_json::Value> {
    let arguments: Vec<String> = match operation {
        "class_routes" => vec!["route".into(), "measured".into()],
        "links" => vec!["route".into(), "links".into()],
        "node" | "peer" | "route" => vec![operation.into()],
        _ => return Err(RuntimeError::InspectionFailed),
    };
    inspect_arguments(configuration, &arguments, Duration::from_secs(1)).await
}
async fn inspect_arguments(
    configuration: &RuntimeConfiguration,
    arguments: &[String],
    deadline: Duration,
) -> Result<serde_json::Value> {
    let mut child = namespace_command(configuration, "easytier-cli")
        .args([
            "--rpc-portal",
            &configuration.rpc_listen.to_string(),
            "-o",
            "json",
        ])
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = child.stdout.take().ok_or(RuntimeError::InspectionFailed)?;
    let reading = async {
        let mut bytes = Vec::new();
        stdout.take(1024 * 1024 + 1).read_to_end(&mut bytes).await?;
        if bytes.len() > 1024 * 1024 || !child.wait().await?.success() {
            return Err(RuntimeError::InspectionFailed);
        }
        Ok(serde_json::from_slice(&bytes)?)
    };
    timeout(deadline, reading)
        .await
        .map_err(|_| RuntimeError::InspectionFailed)?
}

fn rejected(message: &str) -> RuntimeError {
    RuntimeError::Configuration(message.into())
}

fn protected_file(path: &Path, private: bool) -> Result<()> {
    if private {
        return avian_storage::validate_private_file(path)
            .map_err(|_| rejected("private file ownership or permissions are invalid"));
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(rejected("regular files are required"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let forbidden = if private { 0o077 } else { 0o022 };
        if metadata.permissions().mode() & forbidden != 0 {
            return Err(rejected(
                "file permissions permit unauthorized access or modification",
            ));
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct BuildManifest {
    source_commit: String,
    patches: Vec<PatchIdentity>,
    binary_sha256: BTreeMap<String, String>,
    topology_policy: bool,
    #[serde(default)]
    priority_forwarding: bool,
    #[serde(default)]
    measured_routing: bool,
    #[serde(default)]
    underlay_binding: bool,
    #[serde(default)]
    private_rpc: bool,
    #[serde(default)]
    managed_network: bool,
    #[serde(default)]
    shared_network_helper_sha256: Option<BTreeMap<String, String>>,
    #[serde(default)]
    peer_protocol: Option<String>,
    #[serde(default)]
    shared_routing_sha256: Option<BTreeMap<String, String>>,
    #[serde(default)]
    target_os: Option<String>,
    #[serde(default)]
    target_arch: Option<String>,
}
#[derive(Deserialize)]
struct PatchIdentity {
    name: String,
    sha256: String,
}

pub fn verify_engine_directory(directory: &Path) -> Result<()> {
    verify_engine_features(directory, false)
}

fn verify_engine_features(directory: &Path, priority_required: bool) -> Result<()> {
    protected_file(&directory.join("build.json"), false)?;
    let manifest: BuildManifest = serde_json::from_slice(&fs::read(directory.join("build.json"))?)?;
    if manifest.source_commit != ENGINE_COMMIT || !manifest.topology_policy {
        return Err(rejected("engine does not match the pinned topology build"));
    }
    let mut expected: Vec<(String, String)> = REQUIRED_PATCHES
        .iter()
        .map(|(name, hash)| ((*name).into(), (*hash).into()))
        .collect();
    if manifest.priority_forwarding {
        expected.push((PRIORITY_PATCH.0.into(), PRIORITY_PATCH.1.into()));
    }
    if priority_required && !manifest.priority_forwarding {
        return Err(rejected("priority forwarding is required"));
    }
    if manifest.measured_routing {
        if !manifest.priority_forwarding
            || manifest.peer_protocol.as_deref() != Some("avian-peerconn-noise-hop-measured-v1")
            || manifest.shared_routing_sha256.as_ref() != Some(&expected_shared_routing())
        {
            return Err(rejected(
                "measured engine routing provenance differs from the compiled policy",
            ));
        }
        expected.push((
            "measured-routing.patch".into(),
            hex::encode(Sha256::digest(include_bytes!(
                "../../../qualification/patches/measured-routing.patch"
            ))),
        ));
    }
    if manifest.underlay_binding {
        if !manifest.measured_routing {
            return Err(rejected("bound underlays require measured routing"));
        }
        expected.push((
            "underlay-binding.patch".into(),
            "c4420d091b890474fc7643a4e06a50f00c4265fe303de4e1ca94dde3ba6aa692".into(),
        ));
    }
    if manifest.private_rpc {
        if !manifest.underlay_binding {
            return Err(rejected(
                "private native RPC requires the complete managed transport profile",
            ));
        }
        expected.push((
            "private-rpc.patch".into(),
            "68ad1759812e166cc1455f943021a25dabe16f4e9d7f7363d0275b84f9273742".into(),
        ));
    }
    if manifest.managed_network {
        if !manifest.private_rpc
            || manifest.shared_network_helper_sha256.as_ref()
                != Some(&expected_shared_network_helper())
        {
            return Err(rejected(
                "managed networking client provenance differs from the compiled runtime",
            ));
        }
        expected.push((
            "managed-network.patch".into(),
            "8e641b941678811b4d5b96499cd21eab56ddead3abbe3c2395c38cfd5b658495".into(),
        ));
    }
    if expected.len() != manifest.patches.len()
        || expected
            .iter()
            .zip(&manifest.patches)
            .any(|((name, digest), actual)| actual.name != *name || actual.sha256 != *digest)
    {
        return Err(rejected(
            "engine patch identity or ordering differs from the reviewed build",
        ));
    }
    for name in ["easytier-core", "easytier-cli"] {
        let path = directory.join(if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.into()
        });
        protected_file(&path, false)?;
        let mut file = fs::File::open(path)?;
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        if manifest.binary_sha256.get(name) != Some(&hex::encode(digest.finalize())) {
            return Err(rejected("engine binary digest mismatch"));
        }
    }
    Ok(())
}

fn expected_shared_routing() -> BTreeMap<String, String> {
    let manifest = format!(
        r#"[package]
name = "avian-routing"
version = "{}"
edition = "2024"
license-file = "LICENSE"

[dependencies]
serde = {{ version = "1", features = ["derive"] }}
thiserror = "2"
"#,
        env!("CARGO_PKG_VERSION")
    );
    [
        (
            "Cargo.toml",
            hex::encode(Sha256::digest(manifest.as_bytes())),
        ),
        (
            "src/lib.rs",
            hex::encode(Sha256::digest(include_bytes!(
                "../../avian-routing/src/lib.rs"
            ))),
        ),
        (
            "LICENSE",
            hex::encode(Sha256::digest(include_bytes!("../../../LICENSE.md"))),
        ),
    ]
    .into_iter()
    .map(|(name, digest)| (name.into(), digest))
    .collect()
}
fn expected_shared_network_helper() -> BTreeMap<String, String> {
    let manifest = format!(
        r#"[package]
name = "avian-network-helper"
version = "{}"
edition = "2024"
license-file = "LICENSE"

[features]
default = []
server = []

[dependencies]
serde = {{ version = "1", features = ["derive"] }}
serde_json = "1"
thiserror = "2"
ipnet = {{ version = "2", features = ["serde"] }}

[target.'cfg(target_os = "linux")'.dependencies]
rustix = {{ version = "1", features = ["net", "fs", "process", "event"] }}
socket2 = {{ version = "0.6", features = ["all"] }}
"#,
        env!("CARGO_PKG_VERSION")
    );
    [
        ("Cargo.toml", manifest.as_bytes()),
        (
            "src/lib.rs",
            include_bytes!("../../avian-network-helper/src/lib.rs").as_slice(),
        ),
        (
            "src/client.rs",
            include_bytes!("../../avian-network-helper/src/client.rs").as_slice(),
        ),
        (
            "src/transport.rs",
            include_bytes!("../../avian-network-helper/src/transport.rs").as_slice(),
        ),
        ("LICENSE", include_bytes!("../../../LICENSE.md").as_slice()),
    ]
    .into_iter()
    .map(|(name, bytes)| (name.into(), hex::encode(Sha256::digest(bytes))))
    .collect()
}

fn verify_runtime_profile(configuration: &RuntimeConfiguration) -> Result<()> {
    let manifest: BuildManifest = serde_json::from_slice(&fs::read(
        configuration.engine_directory.join("build.json"),
    )?)?;
    if (configuration.measured_routing && !manifest.measured_routing)
        || (configuration.underlay_binding && !manifest.underlay_binding)
        || (configuration.private_rpc && !manifest.private_rpc)
    {
        return Err(rejected(
            "runtime requests capabilities absent from the verified engine",
        ));
    }
    if configuration.network_helper_socket.is_some() && !manifest.managed_network {
        return Err(rejected(
            "network helper requires the managed engine profile",
        ));
    }
    if configuration.execution == ExecutionMode::Native
        && (manifest.target_os.as_deref() != Some(std::env::consts::OS)
            || manifest.target_arch.as_deref() != Some(std::env::consts::ARCH))
    {
        return Err(rejected("native engine target differs from this host"));
    }
    Ok(())
}

fn validate_namespace(name: &str) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let path = Path::new("/run/netns").join(name);
        if fs::symlink_metadata(&path)?.file_type().is_symlink() {
            return Err(rejected("namespace path must not be a symbolic link"));
        }
        let configured = fs::metadata(path)?;
        let own = fs::metadata("/proc/self/ns/net")?;
        if configured.ino() == own.ino() && configured.dev() == own.dev() {
            return Err(rejected(
                "configured namespace is the manager's network namespace",
            ));
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = name;
        Err(RuntimeError::UnsupportedPlatform)
    }
}

fn prepare(
    configuration: &RuntimeConfiguration,
) -> Result<(MembershipStore, zeroize::Zeroizing<String>)> {
    configuration.validate()?;
    match configuration.execution {
        ExecutionMode::IsolatedNamespace => validate_namespace(&configuration.namespace_name)?,
        ExecutionMode::Native if !capabilities().native_execution => {
            return Err(RuntimeError::UnsupportedPlatform);
        }
        ExecutionMode::Native => {}
    }
    verify_engine_features(
        &configuration.engine_directory,
        configuration.priority_forwarding,
    )?;
    verify_runtime_profile(configuration)?;
    protected_file(&configuration.credential_file, true)?;
    protected_file(&configuration.trust_root_file, false)?;
    protected_file(&configuration.membership_file, true)?;
    let protected = avian_storage::read_secret_file(&configuration.credential_file, 4096)
        .map_err(|_| rejected("transport credential storage is unavailable"))?;
    let credential = zeroize::Zeroizing::new(
        std::str::from_utf8(&protected)
            .map_err(|_| rejected("transport credential encoding is invalid"))?
            .trim()
            .to_owned(),
    );
    let secret: [u8; 32] = STANDARD
        .decode(credential.as_bytes())
        .map_err(|_| rejected("invalid transport credential"))?
        .try_into()
        .map_err(|_| rejected("transport credential must contain 32 bytes"))?;
    let public = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(secret));
    let root: TrustRoot = serde_json::from_slice(&fs::read(&configuration.trust_root_file)?)?;
    let membership = MembershipStore::load(&configuration.membership_file, root)?;
    let enrolled = membership.authorize(&configuration.node_id, &hex::encode(public.as_bytes()))?;
    if !enrolled
        .overlay_addresses
        .contains(&IpAddr::V4(configuration.overlay_address))
    {
        return Err(rejected("overlay address does not match signed membership"));
    }
    prepare_directory(&configuration.runtime_directory)?;
    if configuration.private_rpc {
        let mut token = [0u8; 32];
        getrandom::fill(&mut token).map_err(|_| rejected("RPC token randomness unavailable"))?;
        write_private(
            &configuration.rpc_token_file(),
            hex::encode(token).as_bytes(),
        )?;
    }
    TrustFileExport::from_membership(&membership, SystemTime::now())?
        .write_startup_file(configuration.trust_file())?;
    let document = serde_json::json!({
        "instance_name": configuration.node_id,
        "hostname": configuration.node_id,
        "ipv4": format!("{}/{}", configuration.overlay_address, configuration.overlay_prefix_length),
        "listeners": if configuration.network_helper_socket.is_some() { Vec::<String>::new() } else if configuration.underlay_binding { vec![format!("tcp://{}",configuration.listen),format!("udp://{}",configuration.listen)] } else { vec![format!("tcp://{}", configuration.listen)] },
        "rpc_portal": configuration.rpc_listen.to_string(),
        "credential_file": configuration.trust_file().to_string_lossy(),
        "stun_servers": ["127.0.0.1:9"],
        "stun_servers_v6": ["[::1]:9"],
        "network_identity": {"network_name": membership.trust_root().network_id},
        "secure_mode": {"enabled": true, "local_private_key": credential.as_str(), "local_public_key": STANDARD.encode(public.as_bytes())},
        "peer": configuration.peers.iter().filter(|_|!configuration.underlay_binding).map(|peer| format!("tcp://{peer}"))
            .chain(configuration.bootstrap.owned_endpoints.iter().filter(|_|!configuration.underlay_binding).map(|endpoint| endpoint.as_str().to_owned()))
            .map(|uri| serde_json::json!({"uri": uri})).collect::<Vec<_>>(),
        "flags": {"mtu": 1380, "disable_p2p": true, "disable_udp_hole_punching": true, "disable_tcp_hole_punching": true, "disable_upnp": true, "accept_dns": false, "dev_name": configuration.interface_name, "enable_exit_node": false},
    });
    let text = toml::to_string(&document)?;
    write_private(&configuration.config_file(), text.as_bytes())?;
    Ok((membership, credential))
}

fn prepare_directory(path: &Path) -> Result<()> {
    if !path.exists() {
        avian_storage::create_private_directory(path)
            .map_err(|_| rejected("private runtime directory could not be created"))?;
    }
    avian_storage::validate_private_directory(path)
        .map_err(|_| rejected("runtime directory ownership or permissions are invalid"))?;
    let marker = path.join(".avian-runtime");
    if marker.exists() {
        let bytes = avian_storage::read_private_file(&marker, 128)
            .map_err(|_| rejected("runtime marker is not private"))?;
        if bytes.as_slice() != b"AVIAN isolated runtime v1\n"
            && bytes.as_slice() != b"AVIAN runtime v1\n"
        {
            return Err(rejected("runtime directory belongs to another purpose"));
        }
    } else {
        if fs::read_dir(path)?.next().is_some() {
            return Err(rejected(
                "runtime directory must be empty or initialized by this supervisor",
            ));
        }
        avian_storage::create_private_file(&marker, b"AVIAN runtime v1\n")
            .map_err(|_| rejected("runtime marker could not be created"))?;
    }
    Ok(())
}

fn write_private(path: &Path, content: &[u8]) -> Result<()> {
    avian_storage::atomic_write_private_file(path, content)
        .map_err(|_| rejected("protected runtime output could not be persisted"))
}
fn private_log(path: &Path) -> Result<fs::File> {
    use std::io::Seek;
    let mut file = avian_storage::open_private_lock(path)
        .map_err(|_| rejected("private engine log is unavailable"))?;
    file.seek(std::io::SeekFrom::End(0))?;
    Ok(file)
}
fn node_matches(configuration: &RuntimeConfiguration, node: &serde_json::Value) -> bool {
    let Some(encoded_configuration) = node.get("config").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let Ok(reported_configuration) = toml::from_str::<toml::Value>(encoded_configuration) else {
        return false;
    };
    if protected_file(&configuration.credential_file, true).is_err() {
        return false;
    }
    let Ok(encoded) = avian_storage::read_secret_file(&configuration.credential_file, 4096) else {
        return false;
    };
    let Ok(credential) = std::str::from_utf8(&encoded) else {
        return false;
    };
    let Ok(decoded) = STANDARD.decode(credential.trim()) else {
        return false;
    };
    let Ok(secret) = <[u8; 32]>::try_from(decoded) else {
        return false;
    };
    let expected_key = STANDARD.encode(
        x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(secret)).as_bytes(),
    );
    // EasyTier limits its display hostname to 32 characters. The full instance
    // identity and public transport key bind readiness to the configured process.
    let expected_hostname = configuration.node_id.chars().take(32).collect::<String>();
    node.get("hostname").and_then(serde_json::Value::as_str) == Some(expected_hostname.as_str())
        && reported_configuration
            .get("instance_name")
            .and_then(toml::Value::as_str)
            == Some(configuration.node_id.as_str())
        && reported_configuration
            .get("secure_mode")
            .and_then(|secure_mode| secure_mode.get("local_public_key"))
            .and_then(toml::Value::as_str)
            == Some(expected_key.as_str())
        && node
            .get("ipv4_addr")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| value.split('/').next())
            .is_some_and(|value| value == configuration.overlay_address.to_string())
}

#[cfg(test)]
mod readiness_tests {
    use super::*;

    #[test]
    fn native_hostname_truncation_preserves_full_identity_binding() {
        let temporary = tempfile::TempDir::new().unwrap();
        let credential_file = temporary.path().join("credential");
        let private_key = [19u8; 32];
        write_private(&credential_file, STANDARD.encode(private_key).as_bytes()).unwrap();
        let public_key = STANDARD.encode(
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(private_key))
                .as_bytes(),
        );
        let configuration = RuntimeConfiguration {
            execution: Default::default(),
            network_helper_socket: None,
            interface_name: "avian0".into(),
            overlay_prefix_length: 24,
            bootstrap: Default::default(),
            measured_routing: false,
            private_rpc: false,
            underlay_binding: false,
            connection_budget: default_connection_budget(),
            discovery_hint_budget: default_discovery_budget(),
            automatic_capacity_probes: true,
            probe_payload_budget_bytes: default_probe_budget(),
            probe_window_ms: default_probe_window(),
            isolation_required: true,
            namespace_name: "readiness-test".into(),
            engine_directory: temporary.path().join("engine"),
            runtime_directory: temporary.path().join("runtime"),
            membership_file: temporary.path().join("membership.json"),
            trust_root_file: temporary.path().join("root.json"),
            credential_file,
            node_id: "0123456789abcdef".repeat(4),
            overlay_address: "10.201.0.2".parse().unwrap(),
            listen: "172.30.0.2:11010".parse().unwrap(),
            rpc_listen: "127.0.0.1:15888".parse().unwrap(),
            peers: Vec::new(),
            metered_addresses: Vec::new(),
            control_tcp_ports: Vec::new(),
            allow_metered_video: false,
            allow_metered_bulk: false,
            fast_failover: false,
            priority_forwarding: false,
        };
        let native_configuration = |identity: &str, key: &str| {
            format!("instance_name = \"{identity}\"\n[secure_mode]\nlocal_public_key = \"{key}\"\n")
        };
        let mut node = serde_json::json!({
            "hostname": &configuration.node_id[..32],
            "ipv4_addr": "10.201.0.2/24",
            "config": native_configuration(&configuration.node_id, &public_key),
        });
        assert!(node_matches(&configuration, &node));
        let colliding_display_identity = configuration.node_id[..32].to_owned() + &"f".repeat(32);
        node["config"] = native_configuration(&colliding_display_identity, &public_key).into();
        assert!(!node_matches(&configuration, &node));
        node["config"] =
            native_configuration(&configuration.node_id, &STANDARD.encode([0u8; 32])).into();
        assert!(!node_matches(&configuration, &node));
        node["config"] = native_configuration(&configuration.node_id, &public_key).into();
        node["ipv4_addr"] = "10.201.0.3/24".into();
        assert!(!node_matches(&configuration, &node));
    }
}

#[cfg(all(test, unix))]
mod lifecycle_cancellation_tests {
    use super::*;

    fn configuration(root: &Path) -> RuntimeConfiguration {
        toml::from_str(&format!(
            r#"
isolation_required = true
namespace_name = "drop-regression"
engine_directory = "{root}/engine"
runtime_directory = "{root}/runtime"
membership_file = "{root}/membership.json"
trust_root_file = "{root}/root.json"
credential_file = "{root}/credential"
node_id = "{node_id}"
overlay_address = "10.201.0.1"
listen = "172.30.0.1:11010"
rpc_listen = "127.0.0.1:15888"
"#,
            root = root.display(),
            node_id = "a".repeat(64)
        ))
        .unwrap()
    }

    async fn child() -> Child {
        Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    async fn wait_stopped(lifecycle: &Arc<Mutex<Lifecycle>>, process_id: u32) {
        timeout(Duration::from_secs(6), async {
            loop {
                let stopped = {
                    let lifecycle = lifecycle.lock().await;
                    lifecycle.child.is_none() && lifecycle.cleanup.is_none()
                };
                let exists = std::process::Command::new("kill")
                    .args(["-0", &process_id.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
                    .success();
                if stopped && !exists {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("owned child must be killed and reaped");
    }

    #[tokio::test]
    async fn dropping_supervisor_aborts_coordinator_holding_the_lifecycle_lock() {
        let temporary = tempfile::TempDir::new().unwrap();
        let supervisor = RuntimeSupervisor::new(configuration(temporary.path()));
        let lifecycle = supervisor.lifecycle.clone();
        let child = child().await;
        let process_id = child.id().unwrap();
        let (locked, waiting) = tokio::sync::oneshot::channel();
        {
            let mut owner = lifecycle.lock().await;
            owner.child = Some(child);
            let held = lifecycle.clone();
            let task = tokio::spawn(async move {
                let _owner = held.lock().await;
                let _ = locked.send(());
                std::future::pending::<()>().await;
            });
            supervisor
                .background_abort
                .lock()
                .unwrap()
                .push(task.abort_handle());
            owner.coordinator = Some(task);
        }
        waiting.await.unwrap();
        drop(supervisor);
        wait_stopped(&lifecycle, process_id).await;
    }

    #[test]
    fn dropping_caller_runtime_does_not_cancel_supervisor_cleanup() {
        let temporary = tempfile::TempDir::new().unwrap();
        let caller = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (lifecycle, process_id) = caller.block_on(async {
            let supervisor = RuntimeSupervisor::new(configuration(temporary.path()));
            let lifecycle = supervisor.lifecycle.clone();
            let child = child().await;
            let process_id = child.id().unwrap();
            lifecycle.lock().await.child = Some(child);
            drop(supervisor);
            (lifecycle, process_id)
        });
        drop(caller);
        lifecycle_runtime()
            .unwrap()
            .block_on(wait_stopped(&lifecycle, process_id));
    }

    #[tokio::test]
    async fn cancelled_cleanup_remains_owned_and_finishes_before_replacement_start() {
        let temporary = tempfile::TempDir::new().unwrap();
        let supervisor = RuntimeSupervisor::new(configuration(temporary.path()));
        let lifecycle = supervisor.lifecycle.clone();
        let child = child().await;
        let process_id = child.id().unwrap();
        lifecycle.lock().await.child = Some(child);
        let mut owner = lifecycle.lock().await;
        {
            let cleanup = stop_locked(&mut owner);
            tokio::pin!(cleanup);
            // Poll cleanup until it has transferred ownership, then cancel the waiter.
            let _ = timeout(Duration::from_millis(1), &mut cleanup).await;
        }
        // A replacement operation waits for the previously owned cleanup.
        stop_locked(&mut owner).await.unwrap();
        drop(owner);
        wait_stopped(&lifecycle, process_id).await;
    }

    #[tokio::test]
    async fn cancelled_start_waits_for_blocking_preparation_before_removing_plaintext() {
        let temporary = tempfile::TempDir::new().unwrap();
        let configuration = configuration(temporary.path());
        avian_storage::create_private_directory(&configuration.runtime_directory).unwrap();
        write_private(
            &configuration.runtime_directory.join(".avian-runtime"),
            b"AVIAN runtime v1\n",
        )
        .unwrap();
        let supervisor = RuntimeSupervisor::new(configuration.clone());
        let lifecycle = supervisor.lifecycle.clone();
        lifecycle.lock().await.generation = 1;
        let (finished, completion) = tokio::sync::oneshot::channel();
        drop(StartupCancellation {
            armed: true,
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            generation: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            owns_start: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            completion: Some(completion),
            lifecycle: lifecycle.clone(),
            configuration: configuration.clone(),
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        write_private(&configuration.config_file(), b"private startup material").unwrap();
        assert!(configuration.config_file().exists());
        finished.send(()).unwrap();
        timeout(Duration::from_secs(2), async {
            while configuration.config_file().exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn failed_helper_release_is_retained_and_blocks_a_replacement_start() {
        let temporary = tempfile::TempDir::new().unwrap();
        let supervisor = RuntimeSupervisor::new(configuration(temporary.path()));
        let missing_socket = temporary.path().join("missing-helper.sock");
        supervisor.lifecycle.lock().await.helper_socket = Some(missing_socket.clone());
        assert!(supervisor.stop().await.is_err());
        let status = supervisor.status().await;
        assert_eq!(status.state, RuntimePhase::Failed);
        assert!(!status.engine_ready);
        assert!(status.last_error.unwrap().contains("retained for retry"));
        assert!(supervisor.start().await.is_err());
        let mut lifecycle = supervisor.lifecycle.lock().await;
        assert_eq!(lifecycle.helper_socket.as_ref(), Some(&missing_socket));
        assert!(lifecycle.child.is_none());
        lifecycle.helper_socket = None;
        drop(lifecycle);
        assert_eq!(
            supervisor.stop().await.unwrap().state,
            RuntimePhase::Stopped
        );
    }

    #[tokio::test]
    async fn failed_phase_cannot_reuse_a_retained_child_as_successful_start() {
        let temporary = tempfile::TempDir::new().unwrap();
        let supervisor = RuntimeSupervisor::new(configuration(temporary.path()));
        let child = child().await;
        let process_id = child.id().unwrap();
        {
            let mut lifecycle = supervisor.lifecycle.lock().await;
            lifecycle.phase = RuntimePhase::Failed;
            lifecycle.child = Some(child);
        }
        // Missing membership prevents a replacement; the previous child must be
        // cleaned up rather than reported as an idempotently running engine.
        assert!(supervisor.start().await.is_err());
        wait_stopped(&supervisor.lifecycle, process_id).await;
    }

    #[tokio::test]
    async fn stale_cancelled_start_cannot_stop_a_new_generation() {
        let temporary = tempfile::TempDir::new().unwrap();
        let supervisor = RuntimeSupervisor::new(configuration(temporary.path()));
        let lifecycle = supervisor.lifecycle.clone();
        let child = child().await;
        let process_id = child.id().unwrap();
        {
            let mut owner = lifecycle.lock().await;
            owner.generation = 2;
            owner.child = Some(child);
        }
        let (finished, completion) = tokio::sync::oneshot::channel();
        let cancellation = StartupCancellation {
            armed: true,
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            generation: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            owns_start: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            completion: Some(completion),
            lifecycle: lifecycle.clone(),
            configuration: supervisor.configuration.clone(),
        };
        drop(cancellation);
        finished.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            lifecycle
                .lock()
                .await
                .child
                .as_mut()
                .unwrap()
                .try_wait()
                .unwrap(),
            None
        );
        supervisor.stop().await.unwrap();
        wait_stopped(&lifecycle, process_id).await;
    }
}

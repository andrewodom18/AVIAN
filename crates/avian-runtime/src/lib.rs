//! Lifecycle control for an operator-configured engine inside an existing network namespace.

use avian_engine::TrustFileExport;
use avian_membership::{MembershipStore, TrustRoot};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfiguration {
    pub isolation_required: bool,
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
        if !self.isolation_required {
            return Err(rejected("isolation_required must be true"));
        }
        if self.namespace_name.is_empty()
            || self.namespace_name.len() > 63
            || !self
                .namespace_name
                .bytes()
                .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'-' | b'_'))
        {
            return Err(rejected("invalid namespace name"));
        }
        if self.node_id.len() != 64 || hex::decode(&self.node_id).is_err() {
            return Err(rejected("invalid node identity"));
        }
        if !self.overlay_address.is_private() {
            return Err(rejected("overlay address must be private IPv4"));
        }
        if !private_address(self.listen.ip())
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
        if self
            .peers
            .iter()
            .any(|peer| !private_address(peer.ip()) || peer.port() == 0)
            || self
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
    phase: RuntimePhase,
    last_error: Option<String>,
}

pub struct RuntimeSupervisor {
    configuration: RuntimeConfiguration,
    lifecycle: Arc<Mutex<Lifecycle>>,
}

impl RuntimeSupervisor {
    pub fn new(configuration: RuntimeConfiguration) -> Self {
        Self {
            configuration,
            lifecycle: Arc::new(Mutex::new(Lifecycle {
                child: None,
                renewal: None,
                phase: RuntimePhase::Stopped,
                last_error: None,
            })),
        }
    }
    pub fn configuration(&self) -> &RuntimeConfiguration {
        &self.configuration
    }

    pub async fn start(&self) -> Result<RuntimeStatus> {
        let mut lifecycle = self.lifecycle.lock().await;
        if lifecycle
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
        stop_locked(&mut lifecycle).await;
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
                lifecycle.phase = RuntimePhase::Failed;
                lifecycle.last_error = Some(problem.to_string());
                return Err(problem);
            }
        };
        if inspect(&self.configuration, "node").await.is_ok() {
            lifecycle.phase = RuntimePhase::Failed;
            lifecycle.last_error = Some("configured RPC portal already serves a process".into());
            return Err(rejected("configured RPC portal already serves a process"));
        }
        let launched = (|| -> Result<Child> {
            let log = private_log(&self.configuration.runtime_directory.join("engine.log"))?;
            let mut command = namespace_command(&self.configuration, "easytier-core");
            command
                .arg("--config-file")
                .arg(self.configuration.config_file())
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
            if self.configuration.fast_failover {
                command.env("ET_FAST_FAILOVER", "1");
            }
            if self.configuration.priority_forwarding {
                command.env("ET_PRIORITY_FORWARDING", "1");
            }
            Ok(command.spawn()?)
        })();
        let child = match launched {
            Ok(child) => child,
            Err(problem) => {
                lifecycle.phase = RuntimePhase::Failed;
                lifecycle.last_error = Some(problem.to_string());
                return Err(problem);
            }
        };
        lifecycle.child = Some(child);
        let mut ready = false;
        let readiness_deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        while tokio::time::Instant::now() < readiness_deadline {
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
            stop_locked(&mut lifecycle).await;
            lifecycle.phase = RuntimePhase::Failed;
            lifecycle.last_error =
                Some("engine did not become ready; inspect the private engine log".into());
            return Err(RuntimeError::StartupFailed);
        }
        let configuration = self.configuration.clone();
        lifecycle.renewal = Some(tokio::spawn(async move {
            let mut previous = membership;
            let mut interval = tokio::time::interval(Duration::from_secs(1));
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
        Ok(self.describe(&lifecycle))
    }

    pub async fn stop(&self) -> Result<RuntimeStatus> {
        let mut lifecycle = self.lifecycle.lock().await;
        stop_locked(&mut lifecycle).await;
        lifecycle.phase = RuntimePhase::Stopped;
        lifecycle.last_error = None;
        Ok(self.describe(&lifecycle))
    }

    pub async fn status(&self) -> RuntimeStatus {
        let mut lifecycle = self.lifecycle.lock().await;
        if let Some(child) = &mut lifecycle.child
            && !matches!(child.try_wait(), Ok(None))
        {
            stop_locked(&mut lifecycle).await;
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
                stop_locked(&mut lifecycle).await;
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
        RuntimeStatus {
            state: lifecycle.phase.clone(),
            process_id: lifecycle.child.as_ref().and_then(Child::id),
            node_id: self.configuration.node_id.clone(),
            overlay_address: self.configuration.overlay_address,
            namespace_name: self.configuration.namespace_name.clone(),
            isolated: lifecycle.phase == RuntimePhase::Running,
            engine_ready: lifecycle.phase == RuntimePhase::Running,
            last_error: lifecycle.last_error.clone(),
        }
    }
}

impl Drop for RuntimeSupervisor {
    fn drop(&mut self) {
        if let Ok(mut lifecycle) = self.lifecycle.try_lock() {
            if let Some(renewal) = lifecycle.renewal.take() {
                renewal.abort();
            }
            if let Some(child) = lifecycle.child.as_mut() {
                let _ = child.start_kill();
            }
        }
    }
}

async fn stop_locked(lifecycle: &mut Lifecycle) {
    if let Some(renewal) = lifecycle.renewal.take() {
        renewal.abort();
    }
    if let Some(mut child) = lifecycle.child.take() {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

fn namespace_command(configuration: &RuntimeConfiguration, binary: &str) -> Command {
    let mut command = Command::new("/usr/sbin/ip");
    command
        .args(["netns", "exec", &configuration.namespace_name])
        .arg(configuration.engine_directory.join(binary));
    command
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin");
    command
}

async fn inspect(
    configuration: &RuntimeConfiguration,
    operation: &str,
) -> Result<serde_json::Value> {
    let mut command = namespace_command(configuration, "easytier-cli");
    command
        .args([
            "--rpc-portal",
            &configuration.rpc_listen.to_string(),
            "-o",
            "json",
            operation,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().ok_or(RuntimeError::InspectionFailed)?;
    let reading = async {
        let mut output = Vec::new();
        stdout
            .take(1024 * 1024 + 1)
            .read_to_end(&mut output)
            .await?;
        if output.len() > 1024 * 1024 {
            return Err(RuntimeError::InspectionFailed);
        }
        let status = child.wait().await?;
        if !status.success() {
            return Err(RuntimeError::InspectionFailed);
        }
        Ok(serde_json::from_slice(&output)?)
    };
    timeout(Duration::from_secs(1), reading)
        .await
        .map_err(|_| RuntimeError::InspectionFailed)?
}

fn rejected(message: &str) -> RuntimeError {
    RuntimeError::Configuration(message.into())
}

fn protected_file(path: &Path, private: bool) -> Result<()> {
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
    if manifest.source_commit != ENGINE_COMMIT
        || !manifest.topology_policy
        || !matches!(manifest.patches.len(), 6 | 7)
    {
        return Err(rejected("engine does not match the pinned topology build"));
    }
    for (expected_name, expected_digest) in REQUIRED_PATCHES {
        if !manifest
            .patches
            .iter()
            .any(|patch| patch.name == *expected_name && patch.sha256 == *expected_digest)
        {
            return Err(rejected(
                "engine patch identity differs from the reviewed build",
            ));
        }
    }
    let has_priority_patch = manifest
        .patches
        .iter()
        .any(|patch| patch.name == PRIORITY_PATCH.0 && patch.sha256 == PRIORITY_PATCH.1);
    if (manifest.patches.len() == 7 && !has_priority_patch)
        || manifest.priority_forwarding != has_priority_patch
        || (priority_required && !has_priority_patch)
    {
        return Err(rejected(
            "engine priority forwarding feature is not verified",
        ));
    }
    for name in ["easytier-core", "easytier-cli"] {
        let path = directory.join(name);
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
    validate_namespace(&configuration.namespace_name)?;
    verify_engine_features(
        &configuration.engine_directory,
        configuration.priority_forwarding,
    )?;
    protected_file(&configuration.credential_file, true)?;
    protected_file(&configuration.trust_root_file, false)?;
    protected_file(&configuration.membership_file, true)?;
    let credential = zeroize::Zeroizing::new(
        fs::read_to_string(&configuration.credential_file)?
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
    TrustFileExport::from_membership(&membership, SystemTime::now())?
        .write_startup_file(configuration.trust_file())?;
    let document = serde_json::json!({
        "instance_name": configuration.node_id,
        "hostname": configuration.node_id,
        "ipv4": configuration.overlay_address.to_string(),
        "listeners": [format!("tcp://{}", configuration.listen)],
        "rpc_portal": configuration.rpc_listen.to_string(),
        "credential_file": configuration.trust_file().to_string_lossy(),
        "stun_servers": ["127.0.0.1:9"],
        "stun_servers_v6": ["[::1]:9"],
        "network_identity": {"network_name": membership.trust_root().network_id},
        "secure_mode": {"enabled": true, "local_private_key": credential.as_str(), "local_public_key": STANDARD.encode(public.as_bytes())},
        "peer": configuration.peers.iter().map(|peer| serde_json::json!({"uri": format!("tcp://{peer}")})).collect::<Vec<_>>(),
        "flags": {"disable_p2p": true, "disable_udp_hole_punching": true, "disable_tcp_hole_punching": true, "disable_upnp": true, "accept_dns": false},
    });
    let text = toml::to_string(&document)?;
    write_private(&configuration.config_file(), text.as_bytes())?;
    Ok((membership, credential))
}

fn prepare_directory(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::create_dir(path)?;
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        return Err(rejected("runtime directory must not be a symbolic link"));
    }
    let marker = path.join(".avian-runtime");
    if marker.exists() {
        protected_file(&marker, true)?;
        if fs::read(&marker)? != b"AVIAN isolated runtime v1\n" {
            return Err(rejected("runtime directory belongs to another purpose"));
        }
    } else if fs::read_dir(path)?.next().is_some() {
        return Err(rejected(
            "runtime directory must be empty or initialized by this supervisor",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    if !marker.exists() {
        write_private(&marker, b"AVIAN isolated runtime v1\n")?;
    }
    Ok(())
}

fn write_private(path: &Path, content: &[u8]) -> Result<()> {
    use std::fs::OpenOptions;
    if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.file_type().is_file()) {
        return Err(rejected("private output must be a regular file"));
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        output.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    output.write_all(content)?;
    output.sync_all()?;
    Ok(())
}
fn private_log(path: &Path) -> Result<fs::File> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.file_type().is_file()) {
        return Err(rejected("log output must be a regular file"));
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
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
    let Ok(credential) = fs::read_to_string(&configuration.credential_file) else {
        return false;
    };
    let credential = zeroize::Zeroizing::new(credential);
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

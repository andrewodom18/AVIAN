use std::{
    fs,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use avian::{
    configuration::{Configuration, create_private_file, create_secret_file, read_secret},
    qualification::QualificationStatus,
};
use avian_membership::{
    MembershipClaims, MembershipStore, Permissions, SignedMembership, SignedRevocations, TrustRoot,
    fingerprint, generate_signing_key, issue_membership, issue_revocations, public_key_hex,
    read_signing_key, write_signing_key,
};
use avian_routing::{NetworkSnapshot, RoutePlanner, RoutingPolicy, TrafficClass};
use clap::{Parser, Subcommand, ValueEnum};
use ed25519_dalek::VerifyingKey;
use serde::Serialize;

#[derive(Parser)]
#[command(version = avian::version::BUILD_VERSION)]
#[command(about = "AVIAN enrollment, inspection, and route qualification")]
struct Arguments {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new offline network administrator and local observation configuration.
    Init {
        #[arg(long)]
        state_dir: PathBuf,
        #[arg(long)]
        network: String,
        #[arg(long, default_value = "127.0.0.1:4179")]
        listen: SocketAddr,
    },
    /// Generate a device identity; private material is written only to the requested file.
    Keygen {
        #[arg(long)]
        output: PathBuf,
    },
    /// Issue a signed membership after independently verifying the device public keys.
    Enroll {
        #[arg(long)]
        identity_public_key: String,
        #[arg(long)]
        transport_public_key: String,
        #[arg(long, required = true)]
        address: Vec<IpAddr>,
        #[arg(long)]
        relay: bool,
        #[arg(long)]
        administer: bool,
        #[arg(long, default_value_t = 1)]
        revision: u64,
        #[arg(long)]
        output: PathBuf,
    },
    /// Import a signed membership; no network connection is opened.
    Import {
        #[arg(long)]
        membership: PathBuf,
    },
    /// Revoke a device and export the next signed incremental update.
    Revoke {
        #[arg(long)]
        node: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Apply a signed revocation update, rejecting missing or reordered revisions.
    ApplyRevocations {
        #[arg(long)]
        input: PathBuf,
    },
    /// Export an already persisted signed membership without issuing a new revision.
    ExportMembership {
        #[arg(long)]
        node: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Export an already persisted signed revocation update.
    ExportRevocation {
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        output: PathBuf,
    },
    /// Export public trust for an isolated engine startup; does not reload a running engine.
    ExportEngineTrust {
        #[arg(long)]
        output: PathBuf,
    },
    /// Inspect verified local membership without contacting a daemon.
    Members,
    /// Read the authenticated local daemon status.
    Status,
    /// Inspect or control the explicitly configured isolated runtime.
    Runtime {
        #[arg(value_enum)]
        action: RuntimeAction,
    },
    /// Inspect signed service advertisements or replace this node's advertised endpoints.
    Services {
        #[arg(long,conflicts_with_all=["after","limit"])]
        publish: Option<PathBuf>,
        #[arg(long)]
        after: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Read fresh operating-system underlay observations.
    Connections,
    /// Read a bounded page of authenticated direct-link observations.
    Links {
        #[arg(long)]
        after: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Read actual per-class forwarding decisions.
    Paths {
        #[arg(long)]
        after: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, value_enum)]
        class: Option<ClassArgument>,
    },
    /// Request an explicit bounded capacity probe; process and link budgets still apply.
    Probe {
        #[arg(long)]
        peer_id: u32,
        #[arg(long)]
        connection_id: Option<String>,
        #[arg(long, default_value_t = 500)]
        duration_ms: u16,
        #[arg(long, default_value_t = 8_000_000)]
        offered_bps: u64,
        #[arg(long)]
        allow_metered: bool,
    },
    /// Read supported-radio inventory through the configured CHUD adapter.
    Radios,
    /// Show required engine gates; this command never claims runtime qualification.
    Qualification,
    /// Calculate a route from supplied observations, without applying it.
    RoutePreview {
        #[arg(long)]
        snapshot: PathBuf,
        #[arg(long)]
        source: String,
        #[arg(long)]
        destination: String,
        #[arg(long, value_enum)]
        class: ClassArgument,
        #[arg(long)]
        now_ms: u64,
        #[arg(long)]
        allow_metered_video: bool,
        #[arg(long)]
        allow_metered_bulk: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum RuntimeAction {
    Status,
    Start,
    Stop,
}

#[derive(Clone, Copy, ValueEnum)]
enum ClassArgument {
    Control,
    Telemetry,
    Video,
    Bulk,
}

impl From<ClassArgument> for TrafficClass {
    fn from(value: ClassArgument) -> Self {
        match value {
            ClassArgument::Control => Self::Control,
            ClassArgument::Telemetry => Self::Telemetry,
            ClassArgument::Video => Self::Video,
            ClassArgument::Bulk => Self::Bulk,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let arguments = Arguments::parse();
    match arguments.command {
        Command::Init {
            state_dir,
            network,
            listen,
        } => initialize(&state_dir, &network, listen),
        Command::Keygen { output } => {
            let key = generate_signing_key()?;
            write_signing_key(&output, &key)?;
            print_json(
                &serde_json::json!({"identity_public_key": public_key_hex(&key.verifying_key()), "fingerprint": fingerprint(&key.verifying_key())}),
            )
        }
        Command::Qualification => print_json(&QualificationStatus::default()),
        Command::RoutePreview {
            snapshot,
            source,
            destination,
            class,
            now_ms,
            allow_metered_video,
            allow_metered_bulk,
        } => {
            let snapshot: NetworkSnapshot = serde_json::from_slice(&fs::read(snapshot)?)?;
            let policy = RoutingPolicy {
                allow_metered_video,
                allow_metered_bulk,
                ..RoutingPolicy::default()
            };
            let route = RoutePlanner::new(policy).plan(
                &snapshot,
                &source,
                &destination,
                class.into(),
                now_ms,
            )?;
            print_json(
                &serde_json::json!({"source": "caller_supplied", "applied": false, "route": route}),
            )
        }
        command => {
            let config_path = arguments.config.context("this command requires --config")?;
            let configuration = Configuration::load(&config_path)?;
            if matches!(command, Command::Status | Command::Radios) {
                let endpoint = if matches!(command, Command::Status) {
                    "status"
                } else {
                    "radios"
                };
                return inspect_daemon(&configuration, endpoint).await;
            }
            if let Command::Runtime { action } = &command {
                let (endpoint, body) = match action {
                    RuntimeAction::Status => ("runtime", None),
                    RuntimeAction::Start => (
                        "network-control",
                        Some(serde_json::json!({"action": "start"})),
                    ),
                    RuntimeAction::Stop => (
                        "network-control",
                        Some(serde_json::json!({"action": "stop"})),
                    ),
                };
                return request_daemon(&configuration, endpoint, body).await;
            }
            match &command {
                Command::Services {
                    publish: Some(path),
                    ..
                } => {
                    let input = fs::read(path).context("read local endpoint publication")?;
                    let publication: avian::services::LocalPublication =
                        serde_json::from_slice(&input)
                            .context("parse local endpoint publication")?;
                    return request_daemon(
                        &configuration,
                        "services",
                        Some(serde_json::to_value(publication)?),
                    )
                    .await;
                }
                Command::Services { after, limit, .. } | Command::Links { after, limit } => {
                    let name = if matches!(command, Command::Services { .. }) {
                        "services"
                    } else {
                        "links"
                    };
                    return request_page(&configuration, name, after.as_deref(), *limit, None)
                        .await;
                }
                Command::Connections => return inspect_daemon(&configuration, "connections").await,
                Command::Paths {
                    after,
                    limit,
                    class,
                } => {
                    return request_page(
                        &configuration,
                        "paths",
                        after.as_deref(),
                        *limit,
                        class.map(TrafficClass::from),
                    )
                    .await;
                }
                Command::Probe {
                    peer_id,
                    connection_id,
                    duration_ms,
                    offered_bps,
                    allow_metered,
                } => {
                    let probe = avian_runtime::LinkProbeRequest {
                        peer_id: *peer_id,
                        connection_id: connection_id.clone(),
                        duration_ms: *duration_ms,
                        offered_bps: *offered_bps,
                        allow_metered: *allow_metered,
                    };
                    return request_daemon(
                        &configuration,
                        "link-probes",
                        Some(serde_json::to_value(probe)?),
                    )
                    .await;
                }
                _ => {}
            }
            let _operation_lock = lock_operations(&configuration.membership_file)?;
            let root: TrustRoot =
                serde_json::from_slice(&fs::read(&configuration.trust_root_file)?)?;
            let mut store = MembershipStore::load(&configuration.membership_file, root.clone())?;
            match command {
                Command::Enroll {
                    identity_public_key,
                    transport_public_key,
                    address,
                    relay,
                    administer,
                    revision,
                    output,
                } => {
                    require_new_output(&output)?;
                    let identity =
                        VerifyingKey::from_bytes(&decode_public_key(&identity_public_key)?)?;
                    let administrator = read_signing_key(administrator_path(&config_path))?;
                    let claims = MembershipClaims::new(
                        root.network_id.clone(),
                        &identity,
                        decode_public_key(&transport_public_key)?,
                        address,
                        revision,
                        Permissions { relay, administer },
                    );
                    let signed = issue_membership(&root, &administrator, claims)?;
                    store.import_membership(signed.clone())?;
                    store.save(&configuration.membership_file)?;
                    write_export(&output, &signed).context(
                        "membership saved; export failed; use export-membership to retry",
                    )?;
                    print_json(
                        &serde_json::json!({"node_id": signed.claims.node_id, "membership_file": output, "engine_admission_applied": false}),
                    )
                }
                Command::Import { membership } => {
                    let record: SignedMembership = serde_json::from_slice(&fs::read(membership)?)?;
                    let node_id = record.claims.node_id.clone();
                    store.import_membership(record)?;
                    store.save(&configuration.membership_file)?;
                    print_json(
                        &serde_json::json!({"node_id": node_id, "imported": true, "engine_admission_applied": false}),
                    )
                }
                Command::Revoke { node, output } => {
                    require_new_output(&output)?;
                    let administrator = read_signing_key(administrator_path(&config_path))?;
                    let record = issue_revocations(
                        &root,
                        &administrator,
                        store.revocation_revision(),
                        vec![node],
                    )?;
                    store.apply_revocations(record.clone())?;
                    store.save(&configuration.membership_file)?;
                    write_export(&output, &record).context(
                        "revocation saved; export failed; use export-revocation to retry",
                    )?;
                    print_json(
                        &serde_json::json!({"revocation_revision": store.revocation_revision(), "export": output, "engine_admission_applied": false}),
                    )
                }
                Command::ApplyRevocations { input } => {
                    let record: SignedRevocations = serde_json::from_slice(&fs::read(input)?)?;
                    store.apply_revocations(record)?;
                    store.save(&configuration.membership_file)?;
                    print_json(
                        &serde_json::json!({"revocation_revision": store.revocation_revision(), "engine_admission_applied": false}),
                    )
                }
                Command::Members => print_json(&store.memberships().collect::<Vec<_>>()),
                Command::ExportMembership { node, output } => {
                    let record = store
                        .memberships()
                        .find(|member| member.claims.node_id == node)
                        .context("membership not found")?;
                    write_export(&output, record)
                }
                Command::ExportRevocation { revision, output } => {
                    let record = store
                        .revocations_after(revision.saturating_sub(1))
                        .find(|record| record.claims.revision == revision)
                        .context("revocation revision not found")?;
                    write_export(&output, record)
                }
                Command::ExportEngineTrust { output } => {
                    let export = avian_engine::TrustFileExport::from_membership(
                        &store,
                        std::time::SystemTime::now(),
                    )?;
                    export.write_startup_file(&output)?;
                    print_json(&serde_json::json!({
                        "network_id": export.network_id,
                        "revocation_revision": export.revocation_revision,
                        "expires_at_unix": export.expires_at_unix,
                        "refresh_at_unix": export.refresh_at_unix,
                        "engine_admission_applied": false,
                        "requires_qualified_engine_extension": true,
                        "output": output
                    }))
                }
                _ => unreachable!(),
            }
        }
    }
}

fn initialize(directory: &Path, network: &str, listen: SocketAddr) -> Result<()> {
    if !listen.ip().is_loopback() {
        bail!("the local API must listen on loopback");
    }
    let administrator = generate_signing_key()?;
    let root = TrustRoot::new(network, &administrator.verifying_key())?;
    // Initialization never merges with or replaces existing identity material.
    if fs::symlink_metadata(directory).is_ok() {
        bail!("state directory already exists");
    }
    avian_storage::create_private_directory(directory)
        .context("create a new private state directory")?;
    write_signing_key(directory.join("administrator.key"), &administrator)?;
    create_private_file(
        &directory.join("trust-root.json"),
        &serde_json::to_vec_pretty(&root)?,
    )?;
    MembershipStore::new(root.clone())?.save(directory.join("membership.json"))?;
    let mut token_bytes = [0u8; 32];
    getrandom::fill(&mut token_bytes)
        .map_err(|error| anyhow::anyhow!("generate API token: {error}"))?;
    create_secret_file(
        &directory.join("api-token"),
        hex::encode(token_bytes).as_bytes(),
    )?;
    let configuration = Configuration {
        listen,
        token_file: "api-token".into(),
        membership_file: "membership.json".into(),
        trust_root_file: "trust-root.json".into(),
        chud: None,
        runtime: None,
        services: None,
        service_exchange: None,
        service_limits: None,
    };
    create_private_file(
        &directory.join("avian.toml"),
        toml::to_string_pretty(&configuration)?.as_bytes(),
    )?;
    print_json(
        &serde_json::json!({"network_id": network, "administrator_fingerprint": fingerprint(&administrator.verifying_key()), "config": directory.join("avian.toml"), "network_control_enabled": false}),
    )
}

fn administrator_path(configuration: &Path) -> PathBuf {
    configuration
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("administrator.key")
}

fn decode_public_key(encoded: &str) -> Result<[u8; 32]> {
    hex::decode(encoded)?.try_into().map_err(|_| {
        anyhow::anyhow!("public keys must contain exactly 32 bytes encoded as hexadecimal")
    })
}

fn write_export(path: &Path, value: &impl Serialize) -> Result<()> {
    create_private_file(path, &serde_json::to_vec_pretty(value)?)
}

fn require_new_output(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_ok() {
        bail!("export destination already exists");
    }
    Ok(())
}

fn lock_operations(membership_file: &Path) -> Result<fs::File> {
    let parent = membership_file.parent().unwrap_or_else(|| Path::new("."));
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(parent.join("membership-operation.lock"))?;
    // Lease publication and administrator updates share this lock. Wait briefly
    // for a healthy writer instead of failing routine commands during publication.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            Err(problem) if problem.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    bail!("another local membership operation is in progress; retry shortly");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(problem) => return Err(problem).context("lock membership operations"),
        }
    }
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

async fn inspect_daemon(configuration: &Configuration, endpoint: &str) -> Result<()> {
    request_daemon(configuration, endpoint, None).await
}

async fn request_page(
    configuration: &Configuration,
    endpoint: &str,
    after: Option<&str>,
    limit: Option<usize>,
    class: Option<TrafficClass>,
) -> Result<()> {
    let mut url = reqwest::Url::parse(&format!("http://{}/v1/{endpoint}", configuration.listen))?;
    {
        let mut query = url.query_pairs_mut();
        if let Some(cursor) = after {
            query.append_pair("after", cursor);
        }
        if let Some(limit) = limit {
            query.append_pair("limit", &limit.to_string());
        }
        if let Some(class) = class {
            query.append_pair(
                "traffic_class",
                serde_json::to_value(class)?
                    .as_str()
                    .context("traffic class is invalid")?,
            );
        }
    }
    let endpoint = format!("{endpoint}?{}", url.query().unwrap_or(""));
    request_daemon(configuration, &endpoint, None).await
}

async fn request_daemon(
    configuration: &Configuration,
    endpoint: &str,
    body: Option<serde_json::Value>,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let url = format!("http://{}/v1/{endpoint}", configuration.listen);
    let request = match body {
        Some(body) => client.post(url).json(&body),
        None => client.get(url),
    };
    let response = request
        .bearer_auth(read_secret(&configuration.token_file)?)
        .send()
        .await?;
    let status = response.status();
    let body: serde_json::Value = response.json().await?;
    print_json(&body)?;
    if !status.is_success() {
        bail!("local API returned HTTP {}", status.as_u16());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::lock_operations;

    #[test]
    fn membership_commands_wait_for_a_brief_concurrent_publication() {
        let directory = tempfile::tempdir().unwrap();
        let membership_path = directory.path().join("membership.json");
        let active_publication = lock_operations(&membership_path).unwrap();
        let contender_path = membership_path.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let contender = std::thread::spawn(move || {
            let lock = lock_operations(&contender_path).unwrap();
            sender.send(()).unwrap();
            lock
        });
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err()
        );
        drop(active_publication);
        receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        drop(contender.join().unwrap());
        lock_operations(&membership_path).unwrap();
    }
}

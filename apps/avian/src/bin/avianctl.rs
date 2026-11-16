use std::{
    fs,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use avian::{
    configuration::{Configuration, create_private_file, read_secret},
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
    fs::create_dir(directory).context(
        "create a new state directory; parent must exist and destination must not exist",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    write_signing_key(directory.join("administrator.key"), &administrator)?;
    create_private_file(
        &directory.join("trust-root.json"),
        &serde_json::to_vec_pretty(&root)?,
    )?;
    MembershipStore::new(root.clone())?.save(directory.join("membership.json"))?;
    let mut token_bytes = [0u8; 32];
    getrandom::fill(&mut token_bytes)
        .map_err(|error| anyhow::anyhow!("generate API token: {error}"))?;
    create_private_file(
        &directory.join("api-token"),
        hex::encode(token_bytes).as_bytes(),
    )?;
    let configuration = Configuration {
        listen,
        token_file: "api-token".into(),
        membership_file: "membership.json".into(),
        trust_root_file: "trust-root.json".into(),
        chud: None,
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
    fs2::FileExt::try_lock_exclusive(&file)
        .context("another local membership operation is in progress")?;
    Ok(file)
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

async fn inspect_daemon(configuration: &Configuration, endpoint: &str) -> Result<()> {
    let response = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()?
        .get(format!("http://{}/v1/{endpoint}", configuration.listen))
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

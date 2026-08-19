use std::fs::OpenOptions;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::Context;
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use clap::{Parser, Subcommand, ValueEnum};
use ed25519_dalek::SigningKey;
use mesh_agent::config::Underlay;
use mesh_agent::control::request;
use mesh_agent::protocol::{ControlRequest, ControlResponse, PeerConnectionAddress};
use mesh_core::DeliveryClass;
use rand_core::OsRng;

#[derive(Debug, Parser)]
#[command(name = "avianctl", about = "Local AVIAN operator control", version)]
struct Args {
    #[arg(long, default_value = "/run/avian/control.sock")]
    socket: PathBuf,
    #[arg(long, default_value_t = 65_536)]
    max_message_bytes: usize,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Status {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        require_ready: bool,
    },
    Records {
        #[arg(long, value_enum)]
        class: RecordClass,
        #[arg(long, default_value_t = 100)]
        limit: u16,
    },
    Keys {
        #[command(subcommand)]
        command: KeyCommand,
    },
    Emergency {
        #[command(subcommand)]
        command: EmergencyCommand,
    },
    ConnectionCode {
        /// Public aircraft endpoint in UNDERLAY=IP:PORT form. Repeat for fallbacks.
        #[arg(long = "address", required = true)]
        addresses: Vec<ConnectionAddressArg>,
    },
}

#[derive(Debug, Clone)]
struct ConnectionAddressArg(PeerConnectionAddress);

impl FromStr for ConnectionAddressArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (underlay, address) = value
            .split_once('=')
            .ok_or_else(|| "address must use UNDERLAY=IP:PORT".to_owned())?;
        let underlay = match underlay {
            "silvus" => Underlay::Silvus,
            "ethernet" => Underlay::Ethernet,
            "wifi" => Underlay::Wifi,
            "satellite" => Underlay::Satellite,
            "other" => Underlay::Other,
            _ => return Err("underlay must be silvus, ethernet, wifi, satellite, or other".into()),
        };
        let address = address
            .parse::<SocketAddr>()
            .map_err(|_| "address must contain a valid IP and port".to_owned())?;
        Ok(Self(PeerConnectionAddress { underlay, address }))
    }
}

#[derive(Debug, Subcommand)]
enum KeyCommand {
    Generate {
        #[arg(long)]
        private_key: PathBuf,
        #[arg(long)]
        public_key: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum EmergencyCommand {
    Rtl {
        #[arg(long)]
        target: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RecordClass {
    Emergency,
    Acknowledgement,
    Mission,
    Telemetry,
    Bulk,
}

impl From<RecordClass> for DeliveryClass {
    fn from(value: RecordClass) -> Self {
        match value {
            RecordClass::Emergency => Self::Emergency,
            RecordClass::Acknowledgement => Self::Acknowledgement,
            RecordClass::Mission => Self::Mission,
            RecordClass::Telemetry => Self::Telemetry,
            RecordClass::Bulk => Self::Bulk,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    match args.command {
        Command::Keys {
            command:
                KeyCommand::Generate {
                    private_key,
                    public_key,
                },
        } => generate_keys(&private_key, &public_key),
        Command::Status {
            json,
            require_ready,
        } => {
            let response = request(
                &args.socket,
                args.max_message_bytes,
                ControlRequest::Status { require_ready },
            )
            .await?;
            match response {
                ControlResponse::Status { status } => {
                    if json {
                        println!("{}", serde_json::to_string_pretty(&status)?);
                    } else {
                        println!(
                            "{}: {} ({} connected peer(s), MAVLink {})",
                            status.node.name,
                            if status.ready { "ready" } else { "degraded" },
                            status.peers.iter().filter(|peer| peer.connected).count(),
                            if status.mavlink.connected {
                                "connected"
                            } else {
                                "unavailable"
                            }
                        );
                    }
                    anyhow::ensure!(!require_ready || status.ready, "AVIAN is not ready");
                    Ok(())
                }
                other => response_error(other),
            }
        }
        Command::Records { class, limit } => {
            anyhow::ensure!((1..=500).contains(&limit), "record limit must be 1-500");
            let response = request(
                &args.socket,
                args.max_message_bytes,
                ControlRequest::ListRecords {
                    class: class.into(),
                    limit,
                },
            )
            .await?;
            match response {
                ControlResponse::Records { records } => {
                    println!("{}", serde_json::to_string_pretty(&records)?);
                    Ok(())
                }
                other => response_error(other),
            }
        }
        Command::Emergency {
            command: EmergencyCommand::Rtl { target },
        } => {
            let response = request(
                &args.socket,
                args.max_message_bytes,
                ControlRequest::EmergencyRtl { target },
            )
            .await?;
            match response {
                ControlResponse::CommandIssued { command_id } => {
                    println!("{command_id}");
                    Ok(())
                }
                other => response_error(other),
            }
        }
        Command::ConnectionCode { addresses } => {
            anyhow::ensure!(
                (1..=8).contains(&addresses.len()),
                "connection code requires 1-8 addresses"
            );
            let response = request(
                &args.socket,
                args.max_message_bytes,
                ControlRequest::ConnectionInfo {
                    addresses: addresses.into_iter().map(|value| value.0).collect(),
                },
            )
            .await?;
            match response {
                ControlResponse::ConnectionInfo {
                    formation_id,
                    name,
                    endpoint_id,
                    addresses,
                } => {
                    let encoded = serde_json::to_vec(&serde_json::json!({
                        "schema_version": 1,
                        "formation_id": formation_id,
                        "aircraft": {
                            "name": name,
                            "endpoint_id": endpoint_id,
                            "addresses": addresses
                        }
                    }))?;
                    println!("AVIAN1.{}", URL_SAFE_NO_PAD.encode(encoded));
                    Ok(())
                }
                other => response_error(other),
            }
        }
    }
}

fn response_error(response: ControlResponse) -> anyhow::Result<()> {
    match response {
        ControlResponse::Error { code, detail } => anyhow::bail!("{code}: {detail}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

fn generate_keys(private_key: &Path, public_key: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        private_key != public_key,
        "private and public paths must differ"
    );
    let signing_key = SigningKey::generate(&mut OsRng);
    write_new(private_key, &STANDARD.encode(signing_key.to_bytes()), 0o600)?;
    if let Err(error) = write_new(
        public_key,
        &STANDARD.encode(signing_key.verifying_key().to_bytes()),
        0o644,
    ) {
        let _ = std::fs::remove_file(private_key);
        return Err(error);
    }
    println!(
        "created {} and {}",
        private_key.display(),
        public_key.display()
    );
    Ok(())
}

fn write_new(path: &Path, value: &str, mode: u32) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let mut file = options
        .open(path)
        .with_context(|| format!("creating {} without overwrite", path.display()))?;
    writeln!(file, "{value}")?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_address_argument_is_strict() {
        let parsed = "ethernet=192.0.2.4:9000"
            .parse::<ConnectionAddressArg>()
            .unwrap();
        assert_eq!(parsed.0.underlay, Underlay::Ethernet);
        assert_eq!(parsed.0.address, "192.0.2.4:9000".parse().unwrap());
        assert!("ethernet=not-an-address"
            .parse::<ConnectionAddressArg>()
            .is_err());
        assert!("secret=192.0.2.4:9000"
            .parse::<ConnectionAddressArg>()
            .is_err());
    }
}

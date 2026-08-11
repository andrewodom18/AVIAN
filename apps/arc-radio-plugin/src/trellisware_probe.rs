use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context};
use clap::Args;
use mesh_core::{NodeId, RadioDeviceObservation};
use trellisware_control::{HttpsTncAgentTransport, TrellisWareReader};

const RADIO_OBSERVATIONS_TOPIC: &str = "local/link/radio/observations/v1";

#[derive(Debug, Args)]
pub struct TrellisWareProbeArgs {
    /// TW-950 management URL, for example https://10.1.0.11.
    #[arg(long)]
    radio_url: String,
    /// Stable ARC/AVIAN node key for this physical radio.
    #[arg(long)]
    source: String,
    /// Combined PEM client certificate and private key when the radio requires mTLS.
    #[arg(long)]
    client_identity_pem: Option<PathBuf>,
    /// PEM CA certificate used to validate the radio's HTTPS certificate.
    #[arg(long)]
    ca_certificate_pem: Option<PathBuf>,
    /// Lab-only override for a self-signed radio certificate.
    #[arg(long, default_value_t = false)]
    accept_invalid_server_certificate: bool,
    /// Optional ARC comms endpoint. When set, publish observations to the live UI.
    #[arg(long)]
    zenoh_endpoint: Option<String>,
    /// Continue polling instead of performing one read.
    #[arg(long, default_value_t = false)]
    watch: bool,
    /// Poll interval for --watch.
    #[arg(long, default_value_t = 5)]
    interval_seconds: u64,
    /// Optional JSON output path for the latest observation.
    #[arg(long)]
    output: Option<PathBuf>,
}

pub async fn run(args: &TrellisWareProbeArgs) -> anyhow::Result<()> {
    if args.source.trim().is_empty() {
        bail!("--source cannot be empty");
    }
    if args.interval_seconds == 0 {
        bail!("--interval-seconds must be positive");
    }
    let identity = read_optional(args.client_identity_pem.as_deref())?;
    let ca = read_optional(args.ca_certificate_pem.as_deref())?;
    let transport = HttpsTncAgentTransport::new(
        &args.radio_url,
        identity.as_deref(),
        ca.as_deref(),
        args.accept_invalid_server_certificate,
    )
    .context("creating read-only TW-950 HTTPS client")?;
    let reader = TrellisWareReader::new(transport);
    let session = match args.zenoh_endpoint.as_deref() {
        Some(endpoint) => Some(open_zenoh(endpoint).await?),
        None => None,
    };

    loop {
        let observation = reader
            .read_observation(
                NodeId::from(args.source.clone()),
                management_ip(&args.radio_url),
                now_unix_ms(),
                false,
            )
            .await
            .context("reading TW-950 observation")?;
        emit(&observation, args.output.as_deref())?;
        if let Some(session) = session.as_ref() {
            session
                .put(RADIO_OBSERVATIONS_TOPIC, serde_json::to_vec(&observation)?)
                .await
                .map_err(|error| anyhow::anyhow!("publishing TW-950 observation: {error}"))?;
        }
        if !args.watch {
            break;
        }
        tokio::time::sleep(Duration::from_secs(args.interval_seconds)).await;
    }
    Ok(())
}

fn emit(observation: &RadioDeviceObservation, output: Option<&Path>) -> anyhow::Result<()> {
    let encoded = serde_json::to_string_pretty(observation)?;
    if let Some(path) = output {
        std::fs::write(path, format!("{encoded}\n"))
            .with_context(|| format!("writing TW-950 observation to {}", path.display()))?;
    } else {
        println!("{encoded}");
    }
    Ok(())
}

fn read_optional(path: Option<&Path>) -> anyhow::Result<Option<Vec<u8>>> {
    path.map(|path| std::fs::read(path).with_context(|| format!("reading {}", path.display())))
        .transpose()
}

fn management_ip(url: &str) -> Option<String> {
    url.split("://")
        .nth(1)?
        .split(['/', ':'])
        .next()
        .map(str::to_owned)
}

async fn open_zenoh(endpoint: &str) -> anyhow::Result<zenoh::Session> {
    let mut config = zenoh::Config::default();
    config
        .insert_json5("mode", r#""client""#)
        .map_err(|error| anyhow::anyhow!("zenoh mode: {error}"))?;
    config
        .insert_json5("connect/endpoints", &format!(r#"["{endpoint}"]"#))
        .map_err(|error| anyhow::anyhow!("zenoh endpoint: {error}"))?;
    config
        .insert_json5("scouting/multicast/enabled", "false")
        .map_err(|error| anyhow::anyhow!("zenoh multicast: {error}"))?;
    config
        .insert_json5("scouting/gossip/enabled", "false")
        .map_err(|error| anyhow::anyhow!("zenoh gossip: {error}"))?;
    zenoh::open(config)
        .await
        .map_err(|error| anyhow::anyhow!("opening Zenoh: {error}"))
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extracts_management_host_without_credentials_or_path() {
        assert_eq!(
            management_ip("https://10.1.0.11/agent/"),
            Some("10.1.0.11".into())
        );
    }
}

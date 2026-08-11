use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context;
use clap::Args;
use mesh_core::{NodeId, RadioDeviceObservation};
use microhard_control::{MicrohardReader, SimulatedMicrohardTransport};
use serde::Deserialize;

#[derive(Debug, Args)]
pub struct MicrohardProbeArgs {
    /// JSON map of read-only AT commands to captured or simulated responses.
    #[arg(long)]
    input: PathBuf,

    /// AVIAN node identifier associated with the local radio.
    #[arg(long)]
    source: String,

    /// Radio management IP represented by this capture.
    #[arg(long)]
    management_ip: Option<String>,

    /// Output path. Omit to write the normalized observation to stdout.
    #[arg(long)]
    output: Option<PathBuf>,

    /// Observation timestamp. Defaults to the current Unix time.
    #[arg(long)]
    observed_at_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandCapture {
    responses: BTreeMap<String, String>,
}

pub async fn run(args: &MicrohardProbeArgs) -> anyhow::Result<()> {
    let encoded = std::fs::read_to_string(&args.input)
        .with_context(|| format!("reading Microhard capture {}", args.input.display()))?;
    let capture: CommandCapture =
        serde_json::from_str(&encoded).context("decoding Microhard command capture")?;
    let observed_at_ms = args.observed_at_ms.unwrap_or_else(now_unix_ms);
    let observation = observe(
        capture.responses,
        NodeId::from(args.source.clone()),
        args.management_ip.clone(),
        observed_at_ms,
    )
    .await?;
    let encoded = serde_json::to_string_pretty(&observation)
        .context("encoding vendor-neutral Microhard observation")?;
    if let Some(path) = args.output.as_ref() {
        std::fs::write(path, format!("{encoded}\n"))
            .with_context(|| format!("writing Microhard observation to {}", path.display()))?;
    } else {
        println!("{encoded}");
    }
    Ok(())
}

async fn observe(
    responses: BTreeMap<String, String>,
    source: NodeId,
    management_ip: Option<String>,
    observed_at_ms: u64,
) -> anyhow::Result<RadioDeviceObservation> {
    MicrohardReader::new(SimulatedMicrohardTransport::from_responses(responses))
        .read_observation(source, management_ip, observed_at_ms, true)
        .await
        .context("normalizing Microhard command capture")
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

    #[tokio::test]
    async fn sample_capture_flows_through_the_arc_plugin_boundary() {
        let encoded = include_str!("../../../examples/microhard-command-responses.sample.json");
        let capture: CommandCapture = serde_json::from_str(encoded).unwrap();
        let observation = observe(
            capture.responses,
            NodeId::from("air-001"),
            Some("192.168.168.1".into()),
            1_000,
        )
        .await
        .unwrap();

        assert_eq!(observation.identity.unwrap().model, "pmddl2460");
        assert!(observation.simulated);
    }
}

use std::io::{self, Read};
use std::path::PathBuf;

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use mesh_core::{
    ArcRadioConfiguration, DeliveryClass, MeshPayload, NodeId, RadioPlanAssessment,
    SilvusGroupApplyTemplate,
};
use mesh_peat::AvianRecord;
use serde::{Deserialize, Serialize};

mod bootstrap;
mod microhard_probe;
mod service;

#[derive(Debug, Subcommand)]
enum Command {
    /// Generate deterministic PEAT identities and per-node Ansible peer variables.
    Bootstrap(bootstrap::BootstrapArgs),
    /// Normalize captured Microhard read-only AT responses without hardware.
    MicrohardProbe(microhard_probe::MicrohardProbeArgs),
}

#[derive(Debug, Parser)]
#[command(
    name = "arc-radio-plugin",
    about = "Validate Arc-owned StreamCaster plans and encode them for AVIAN PEAT",
    version
)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,

    /// JSON request file. Omit to read JSON from standard input.
    #[arg(long)]
    input: Option<PathBuf>,

    /// JSON response file. Omit to write JSON to standard output.
    #[arg(long)]
    output: Option<PathBuf>,

    /// Run as the ARC Zenoh/PEAT edge sidecar instead of one-shot JSON mode.
    #[arg(long)]
    serve: bool,

    /// ARC comms-router endpoint. The sidecar always uses Zenoh client mode.
    #[arg(long, default_value = "unixsock-stream//run/arc/zenoh.sock")]
    zenoh_endpoint: String,

    /// Stable AVIAN source/node name for PEAT records.
    #[arg(long, default_value = "arc-radio-plugin/local")]
    source: String,

    /// Optional StreamCaster management base URL. Omit for contract-only mode.
    #[arg(long)]
    radio_url: Option<String>,

    /// Use the in-process StreamCaster simulator. Mutually exclusive with --radio-url.
    #[arg(long)]
    simulate_radio: bool,

    /// Optional protected JSON credential file for authenticated read-only API calls.
    #[arg(long)]
    credential_file: Option<PathBuf>,

    /// Directory containing signed/approved antenna installation evidence JSON.
    #[arg(long)]
    installation_evidence_dir: Option<PathBuf>,

    /// Signed/approved local regulatory authorization JSON for this installation.
    #[arg(long)]
    regulatory_evidence_file: Option<PathBuf>,

    /// Optional PEAT formation ID. All PEAT options are required together.
    #[arg(long)]
    peat_formation_id: Option<String>,

    /// File containing the base64 PEAT formation key.
    #[arg(long)]
    peat_formation_key_file: Option<PathBuf>,

    /// PEAT bind address (for example 0.0.0.0:9000).
    #[arg(long)]
    peat_bind: Option<std::net::SocketAddr>,

    /// Persistent PEAT data directory.
    #[arg(long)]
    peat_storage: Option<PathBuf>,

    /// PEAT peer descriptor NAME=ENDPOINT_ID@IP:PORT[,IP:PORT...]. NAME= is optional.
    #[arg(long)]
    peat_peer: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArcRadioPluginRequest {
    source: NodeId,
    sequence: u64,
    published_at_ms: u64,
    configuration: ArcRadioConfiguration,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ArcRadioPluginResponse {
    assessment: RadioPlanAssessment,
    silvus_apply_templates: Vec<SilvusGroupApplyTemplate>,
    peat_record: AvianRecord,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if let Some(Command::Bootstrap(command)) = args.command.as_ref() {
        return bootstrap::run(command);
    }
    if let Some(Command::MicrohardProbe(command)) = args.command.as_ref() {
        return microhard_probe::run(command).await;
    }
    if args.serve {
        return service::serve(args).await;
    }
    let encoded = read_request(args.input.as_ref())?;
    let request: ArcRadioPluginRequest =
        serde_json::from_str(&encoded).context("decoding Arc radio-plugin request")?;
    let response = process(request)?;
    let encoded =
        serde_json::to_string_pretty(&response).context("encoding Arc radio-plugin response")?;
    if let Some(path) = args.output {
        std::fs::write(&path, format!("{encoded}\n"))
            .with_context(|| format!("writing radio-plugin response to {}", path.display()))?;
    } else {
        println!("{encoded}");
    }
    Ok(())
}

fn read_request(path: Option<&PathBuf>) -> anyhow::Result<String> {
    if let Some(path) = path {
        return std::fs::read_to_string(path)
            .with_context(|| format!("reading radio-plugin request from {}", path.display()));
    }
    let mut encoded = String::new();
    io::stdin()
        .read_to_string(&mut encoded)
        .context("reading radio-plugin request from standard input")?;
    Ok(encoded)
}

pub(crate) fn process(request: ArcRadioPluginRequest) -> anyhow::Result<ArcRadioPluginResponse> {
    if request.source.as_str().trim().is_empty() {
        bail!("PEAT record source cannot be empty");
    }
    if request.sequence == 0 {
        bail!("PEAT record sequence must be positive");
    }
    let assessment = request.configuration.assess()?;
    let silvus_apply_templates = request.configuration.silvus_apply_templates()?;
    let peat_record = AvianRecord::new(
        request.source,
        request.sequence,
        DeliveryClass::Mission,
        request.published_at_ms,
        MeshPayload::RadioConfiguration(request.configuration),
    )?;
    Ok(ArcRadioPluginResponse {
        assessment,
        silvus_apply_templates,
        peat_record,
    })
}

#[cfg(test)]
mod tests {
    use mesh_core::{
        ChannelBandwidthMhz, RadioConfigAuthority, RadioFleetDefinition, RadioNodeGroup,
        RadioNodeRole, RadioRegulatoryProfile, RadioTrafficProfile, StreamCasterModel,
        StreamCasterNetworkSettings, TransmitPowerMode, RADIO_CONFIG_SCHEMA_VERSION,
    };

    use super::*;

    fn request() -> ArcRadioPluginRequest {
        ArcRadioPluginRequest {
            source: NodeId::from("arc-configd/device-001"),
            sequence: 1,
            published_at_ms: 1_000,
            configuration: ArcRadioConfiguration {
                schema_version: RADIO_CONFIG_SCHEMA_VERSION,
                authority: RadioConfigAuthority::Arc,
                generation: 7,
                network: StreamCasterNetworkSettings {
                    network_id: "ARC-RADIO".to_owned(),
                    center_frequency_mhz: 2_440.0,
                    bandwidth_mhz: ChannelBandwidthMhz::Mhz20,
                    average_node_distance_m: 2_000.0,
                    maximum_node_distance_m: 5_000.0,
                    link_distance_m: None,
                    routing_beacon_period_ms: 500,
                    encryption_required: true,
                },
                fleet: RadioFleetDefinition {
                    total_nodes: 150,
                    groups: vec![
                        RadioNodeGroup {
                            group_id: "air".to_owned(),
                            node_id_prefix: "air".to_owned(),
                            percentage: 98.0,
                            model: StreamCasterModel::Sl5200LiteEstimated,
                            role: RadioNodeRole::Airborne,
                            altitude_msl_ft: 10_000.0,
                            regulatory_profile: RadioRegulatoryProfile::LiveCapabilitiesRequired,
                            transmit_power: TransmitPowerMode::MaxSupported,
                            antenna_mask: 3,
                            beamforming: true,
                            estimated_installed_eirp_dbm: Some(34.44),
                            field_calibrated_udp_capacity_bps: None,
                        },
                        RadioNodeGroup {
                            group_id: "gcs".to_owned(),
                            node_id_prefix: "gcs".to_owned(),
                            percentage: 2.0,
                            model: StreamCasterModel::Sc4400,
                            role: RadioNodeRole::ControlStation,
                            altitude_msl_ft: 0.0,
                            regulatory_profile: RadioRegulatoryProfile::LiveCapabilitiesRequired,
                            transmit_power: TransmitPowerMode::MaxSupported,
                            antenna_mask: 15,
                            beamforming: true,
                            estimated_installed_eirp_dbm: Some(33.0),
                            field_calibrated_udp_capacity_bps: None,
                        },
                    ],
                },
                traffic: RadioTrafficProfile::default(),
                persist_after_apply: false,
            },
        }
    }

    #[test]
    fn emits_arc_configuration_as_durable_peat_mission_record() {
        let response = process(request()).unwrap();

        assert_eq!(response.assessment.node_count, 150);
        assert_eq!(response.peat_record.class, DeliveryClass::Mission);
        assert!(matches!(
            response.peat_record.payload,
            MeshPayload::RadioConfiguration(_)
        ));
        assert!(response
            .silvus_apply_templates
            .iter()
            .all(|template| template.requires_live_supported_frequency_profile));
    }
}

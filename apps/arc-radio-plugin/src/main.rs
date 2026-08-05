use std::io::{self, Read};
use std::path::PathBuf;

use anyhow::{bail, Context};
use clap::Parser;
use mesh_core::{
    ArcRadioConfiguration, DeliveryClass, MeshPayload, NodeId, RadioPlanAssessment,
    SilvusGroupApplyTemplate,
};
use mesh_peat::AvianRecord;
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(
    name = "arc-radio-plugin",
    about = "Validate Arc-owned StreamCaster plans and encode them for AVIAN PEAT",
    version
)]
struct Args {
    /// JSON request file. Omit to read JSON from standard input.
    #[arg(long)]
    input: Option<PathBuf>,

    /// JSON response file. Omit to write JSON to standard output.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArcRadioPluginRequest {
    source: NodeId,
    sequence: u64,
    published_at_ms: u64,
    configuration: ArcRadioConfiguration,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ArcRadioPluginResponse {
    assessment: RadioPlanAssessment,
    silvus_apply_templates: Vec<SilvusGroupApplyTemplate>,
    peat_record: AvianRecord,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
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

fn process(request: ArcRadioPluginRequest) -> anyhow::Result<ArcRadioPluginResponse> {
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
        RadioNodeRole, RadioTrafficProfile, StreamCasterModel, StreamCasterNetworkSettings,
        TransmitPowerMode, RADIO_CONFIG_SCHEMA_VERSION,
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
                    center_frequency_mhz: 2_450.0,
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
                            transmit_power: TransmitPowerMode::MaxSupported,
                            antenna_mask: 3,
                            beamforming: true,
                            field_calibrated_udp_capacity_bps: None,
                        },
                        RadioNodeGroup {
                            group_id: "gcs".to_owned(),
                            node_id_prefix: "gcs".to_owned(),
                            percentage: 2.0,
                            model: StreamCasterModel::Sc4400,
                            role: RadioNodeRole::ControlStation,
                            altitude_msl_ft: 0.0,
                            transmit_power: TransmitPowerMode::MaxSupported,
                            antenna_mask: 15,
                            beamforming: true,
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

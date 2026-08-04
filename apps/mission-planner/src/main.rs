use std::io::{self, Read};
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use mesh_core::{
    MissionAllocation, OperatorTaskGroup, RelayAllocationMode, RelayCorridorRequest, RelayPlan,
    RelayPlanner,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(
    name = "mission-planner",
    about = "AVIAN relay and mission allocation planner for ARC UI",
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
struct ArcPlanningRequest {
    mission_id: Uuid,
    generation: u64,
    corridor: RelayCorridorRequest,
    #[serde(default)]
    relay_count_previews: Vec<RelayCountPreview>,
    #[serde(default)]
    task_groups: Vec<OperatorTaskGroup>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelayCountPreview {
    relay_count: usize,
    station_count: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ArcPlanningResponse {
    proposed: MissionAllocation,
    alternatives: Vec<RelayAlternative>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct RelayAlternative {
    relay_count: usize,
    station_count: Option<usize>,
    plan: Option<RelayPlan>,
    error: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let encoded = read_request(args.input.as_ref())?;
    let request: ArcPlanningRequest =
        serde_json::from_str(&encoded).context("decoding ARC planning request")?;
    let response = plan_request(request)?;
    let encoded =
        serde_json::to_string_pretty(&response).context("encoding ARC planning response")?;
    if let Some(path) = args.output {
        std::fs::write(&path, format!("{encoded}\n"))
            .with_context(|| format!("writing planning response to {}", path.display()))?;
    } else {
        println!("{encoded}");
    }
    Ok(())
}

fn read_request(path: Option<&PathBuf>) -> anyhow::Result<String> {
    if let Some(path) = path {
        return std::fs::read_to_string(path)
            .with_context(|| format!("reading planning request from {}", path.display()));
    }
    let mut encoded = String::new();
    io::stdin()
        .read_to_string(&mut encoded)
        .context("reading planning request from standard input")?;
    Ok(encoded)
}

fn plan_request(request: ArcPlanningRequest) -> anyhow::Result<ArcPlanningResponse> {
    let planner = RelayPlanner;
    let proposed_plan = planner
        .plan(&request.corridor)
        .context("planning proposed relay corridor")?;
    let proposed = MissionAllocation::new(
        request.mission_id,
        request.generation,
        proposed_plan,
        request.task_groups,
    )
    .context("validating ARC task groups")?;

    let alternatives = request
        .relay_count_previews
        .into_iter()
        .map(|preview| {
            let mut corridor = request.corridor.clone();
            corridor.allocation = RelayAllocationMode::RelayCount {
                relay_count: preview.relay_count,
                station_count: preview.station_count,
            };
            match planner.plan(&corridor) {
                Ok(plan) => RelayAlternative {
                    relay_count: preview.relay_count,
                    station_count: preview.station_count,
                    plan: Some(plan),
                    error: None,
                },
                Err(error) => RelayAlternative {
                    relay_count: preview.relay_count,
                    station_count: preview.station_count,
                    plan: None,
                    error: Some(error.to_string()),
                },
            }
        })
        .collect();

    Ok(ArcPlanningResponse {
        proposed,
        alternatives,
    })
}

#[cfg(test)]
mod tests {
    use mesh_core::{GeoPoint, RelayCandidate, RelayFeasibility, RelayPolicy, RelayRangeModel};

    use super::*;

    fn request() -> ArcPlanningRequest {
        ArcPlanningRequest {
            mission_id: Uuid::from_u128(42),
            generation: 1,
            corridor: RelayCorridorRequest {
                base: GeoPoint {
                    latitude_deg: 0.0,
                    longitude_deg: 0.0,
                    msl_m: 100.0,
                },
                objective_entry: GeoPoint {
                    latitude_deg: 0.0,
                    longitude_deg: 0.014_473,
                    msl_m: 100.0,
                },
                candidates: (0..50)
                    .map(|index| RelayCandidate {
                        node_id: format!("aircraft-{index:03}").into(),
                        available: true,
                        relay_eligible: true,
                        mission_eligible: true,
                        relay_suitability: 0.8,
                        mission_utility: 0.5,
                    })
                    .collect(),
                policy: RelayPolicy {
                    range: RelayRangeModel::FieldCalibrated {
                        usable_segment_m: 136.0,
                    },
                    desired_relays_per_station: 2,
                },
                allocation: RelayAllocationMode::Automatic,
            },
            relay_count_previews: vec![
                RelayCountPreview {
                    relay_count: 10,
                    station_count: None,
                },
                RelayCountPreview {
                    relay_count: 20,
                    station_count: None,
                },
                RelayCountPreview {
                    relay_count: 24,
                    station_count: Some(12),
                },
            ],
            task_groups: Vec::new(),
        }
    }

    #[test]
    fn arc_response_includes_proposal_and_override_impacts() {
        let response = plan_request(request()).unwrap();

        assert_eq!(response.proposed.relay_plan.reserved_relay_count, 22);
        assert_eq!(response.proposed.relay_plan.mission_drones_remaining, 28);
        assert_eq!(response.alternatives.len(), 3);
        assert_eq!(
            response.alternatives[0].plan.as_ref().unwrap().feasibility,
            RelayFeasibility::Infeasible
        );
        assert_eq!(
            response.alternatives[1].plan.as_ref().unwrap().feasibility,
            RelayFeasibility::Degraded
        );
        assert_eq!(
            response.alternatives[2].plan.as_ref().unwrap().feasibility,
            RelayFeasibility::Healthy
        );
    }
}

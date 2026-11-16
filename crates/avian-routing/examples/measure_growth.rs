//! Progressive planner measurement, not a packet-forwarding or capacity qualification.
use avian_routing::{LinkObservation, NetworkNode, NetworkSnapshot, RoutePlanner, TrafficClass};
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().collect();
    let duration_seconds: u64 = arguments
        .get(1)
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(5);
    let input_budget_bytes: usize = arguments
        .get(2)
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(64 * 1024 * 1024);
    let started = Instant::now();
    let mut membership = 2_usize;
    println!("members,links,estimated_input_bytes,planner_microseconds");
    while started.elapsed() < Duration::from_secs(duration_seconds) {
        // This conservative input estimate bounds this harness, not network membership.
        // It is not a measurement of allocator use or the engine's retained state.
        let estimated_input_bytes = membership.checked_mul(1024).ok_or("input size overflow")?;
        if estimated_input_bytes > input_budget_bytes {
            break;
        }
        let names: Vec<String> = (0..membership)
            .map(|number| format!("node-{number}"))
            .collect();
        let mut snapshot = NetworkSnapshot {
            nodes: names
                .iter()
                .map(|name| NetworkNode {
                    node_id: name.clone(),
                    relay_allowed: true,
                })
                .collect(),
            links: Vec::new(),
        };
        // A branching sparse topology exercises growth without compulsory all-to-all links.
        for destination in 1..membership {
            let source = (destination - 1) / 2;
            snapshot.links.push(LinkObservation {
                link_id: format!("link-{source}-{destination}"),
                source: names[source].clone(),
                destination: names[destination].clone(),
                interface_name: "simulated".into(),
                available: true,
                metered: false,
                observed_at_ms: 0,
                round_trip_time_ms: Some(1.0),
                loss_fraction: Some(0.001),
                jitter_ms: None,
                available_capacity_bps: Some(1_000_000),
            });
        }
        let measured = Instant::now();
        let route = RoutePlanner::default().plan(
            &snapshot,
            &names[0],
            names.last().ok_or("empty topology")?,
            TrafficClass::Video,
            0,
        )?;
        assert_eq!(route.node_ids.last(), names.last());
        println!(
            "{membership},{},{estimated_input_bytes},{}",
            snapshot.links.len(),
            measured.elapsed().as_micros()
        );
        membership = membership
            .checked_mul(2)
            .ok_or("membership counter overflow")?;
    }
    Ok(())
}

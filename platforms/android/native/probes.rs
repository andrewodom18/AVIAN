//! Bounded background measurements use the same eligibility and exploration policy as desktop.
use super::overlay::{self, OverlayState};
use anyhow::{Context, Result, bail};
use easytier::proto::{
    api::instance::{ListClassRouteRequest, ProbeLinkRequest},
    rpc_types::controller::BaseController,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};

const WINDOW: Duration = Duration::from_secs(30);
const PAYLOAD_BUDGET: u64 = 512 * 1024;
const PROBE_DURATION_MS: u16 = 50;
static SLOT: LazyLock<tokio::sync::Semaphore> = LazyLock::new(|| tokio::sync::Semaphore::new(1));
static BUDGET: Mutex<Option<Budget>> = Mutex::new(None);
static STATUS: Mutex<Option<(Instant, Value)>> = Mutex::new(None);

struct Budget {
    started: Instant,
    used_bytes: u64,
}
struct ProbeState {
    offered_bps: u64,
    last_attempt: Instant,
}

#[derive(Deserialize, Serialize)]
struct Evidence {
    peer_id: u32,
    connection_id: String,
    method: String,
    delivered_rate_lower_bound_bps: u64,
    offered_payload_bytes: u64,
    accepted_payload_bytes: u64,
    delivered_payload_bytes: u64,
    measurement_duration_ms: u64,
    received_datagrams: u64,
}

pub fn reset(enabled: bool) {
    if let Ok(mut status) = STATUS.lock() {
        *status = Some((
            Instant::now(),
            json!({"state":if enabled {"waiting_for_fresh_eligible_link"} else {"disabled"},"evidence":null}),
        ));
    }
}

pub fn status() -> Value {
    let Ok(status) = STATUS.lock() else {
        return json!({"state":"unavailable"});
    };
    let Some((observed, value)) = status.as_ref() else {
        return json!({"state":"not_started"});
    };
    let mut value = value.clone();
    value["age_ms"] = json!(observed.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
    if observed.elapsed() > WINDOW && value["state"] == "measured_lower_bound" {
        value["state"] = json!("expired");
    }
    value
}

fn record(state: &str, evidence: Option<Value>) {
    if let Ok(mut current) = STATUS.lock() {
        *current = Some((Instant::now(), json!({"state":state,"evidence":evidence})));
    }
}

fn reserve(bytes: u64) -> bool {
    let Ok(mut budget) = BUDGET.lock() else {
        return false;
    };
    let budget = budget.get_or_insert_with(|| Budget {
        started: Instant::now(),
        used_bytes: 0,
    });
    if budget.started.elapsed() >= WINDOW {
        budget.started = Instant::now();
        budget.used_bytes = 0;
    }
    if budget.used_bytes.saturating_add(bytes) > PAYLOAD_BUDGET {
        return false;
    }
    budget.used_bytes += bytes;
    true
}

async fn measure(
    state: &Arc<OverlayState>,
    candidate: &Value,
    connection: &str,
    peer: u32,
    offered_bps: u64,
    bytes: u64,
) -> Result<Value> {
    let identifier = *super::super::INSTANCE_NAME_ID_MAP
        .get("avian-android")
        .context("mobile instance missing")?;
    let service = super::super::INSTANCE_MANAGER
        .get_instance_service(&identifier)
        .context("mobile instance unavailable")?;
    let response = tokio::time::timeout(
        Duration::from_secs(3),
        service.get_peer_manage_service().probe_link(
            BaseController::default(),
            ProbeLinkRequest {
                instance: None,
                peer_id: peer,
                connection_id: connection.to_owned(),
                duration_ms: u32::from(PROBE_DURATION_MS),
                offered_bps,
                allow_metered: false,
            },
        ),
    )
    .await??;
    if response.result_json.len() > 64 * 1024 {
        bail!("probe evidence exceeds budget");
    }
    let evidence: Evidence = serde_json::from_str(&response.result_json)?;
    if evidence.peer_id != peer
        || evidence.connection_id != connection
        || evidence.method != "bounded_probe_delivered_rate_lower_bound"
        || evidence.offered_payload_bytes > bytes
        || evidence.accepted_payload_bytes > evidence.offered_payload_bytes
        || evidence.delivered_payload_bytes > evidence.accepted_payload_bytes
        || evidence.measurement_duration_ms == 0
    {
        bail!("invalid probe evidence");
    }
    let mut details = serde_json::to_value(&evidence)?;
    details["offered_bps"] = json!(offered_bps);
    details["duration_ms"] = json!(PROBE_DURATION_MS);
    details["eligibility"] = json!({
        "quality_fresh":candidate["quality_fresh"],"metered":candidate["metered"],
        "policy_valid":candidate["policy_valid"],"queued_packets":candidate["queued_packets"],
        "loss_fraction":candidate["loss_fraction"],"jitter_ms":candidate["jitter_ms"],
    });
    if evidence.delivered_rate_lower_bound_bps > 0 && evidence.delivered_payload_bytes > 0 {
        record("measured_lower_bound", Some(details.clone()));
    } else {
        record("no_confirmed_capacity", Some(details.clone()));
    }
    overlay::event_with_detail(
        "capacity_probe_completed",
        state.snapshot()?.revocation_revision(),
        Some(details),
    );
    Ok(serde_json::to_value(evidence)?)
}

pub async fn run(state: Arc<OverlayState>) {
    let mut cadence = tokio::time::interval(Duration::from_secs(1));
    cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let ready_at = Instant::now() + Duration::from_secs(5);
    let mut cursor: Option<String> = None;
    let mut probes = BTreeMap::<String, ProbeState>::new();
    loop {
        cadence.tick().await;
        if Instant::now() < ready_at || overlay::tun_state() != "ready" {
            continue;
        }
        // At most one attempt per tick bounds this history independently of mesh size.
        probes.retain(|_, previous| previous.last_attempt.elapsed() < Duration::from_secs(90));
        let Some(identifier) = super::super::INSTANCE_NAME_ID_MAP
            .get("avian-android")
            .map(|entry| *entry)
        else {
            continue;
        };
        let Some(service) = super::super::INSTANCE_MANAGER.get_instance_service(&identifier) else {
            continue;
        };
        let response = tokio::time::timeout(
            Duration::from_secs(1),
            service.get_peer_manage_service().list_class_route(
                BaseController::default(),
                ListClassRouteRequest {
                    instance: None,
                    after: cursor.clone().unwrap_or_default(),
                    limit: 100,
                    traffic_class: String::new(),
                    links: true,
                },
            ),
        )
        .await;
        let Ok(Ok(response)) = response else {
            cursor = None;
            continue;
        };
        if response.links_json.len() > 256 * 1024 {
            cursor = None;
            continue;
        }
        let Ok(page) = serde_json::from_str::<Value>(&response.links_json) else {
            cursor = None;
            continue;
        };
        let selected = page["items"]
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|link| {
                let (connection, peer) = avian_runtime::eligible_probe_candidate(link)?;
                if probes
                    .get(&connection)
                    .is_some_and(|previous| previous.last_attempt.elapsed() < WINDOW)
                {
                    return None;
                }
                Some((connection, peer, link.clone()))
            });
        let Some((connection, peer, candidate)) = selected else {
            cursor = page["next_cursor"].as_str().map(str::to_owned);
            continue;
        };
        cursor = Some(format!("{peer}:{connection}"));
        let Ok(_permit) = SLOT.try_acquire() else {
            continue;
        };
        let previous_rate = probes
            .get(&connection)
            .map_or(64_000, |previous| previous.offered_bps);
        let bytes = previous_rate
            .saturating_mul(u64::from(PROBE_DURATION_MS))
            .saturating_add(7999)
            / 8000;
        if !reserve(bytes) {
            continue;
        }
        let previous = probes.entry(connection.clone()).or_insert(ProbeState {
            offered_bps: previous_rate,
            last_attempt: Instant::now(),
        });
        previous.last_attempt = Instant::now();
        let result = measure(&state, &candidate, &connection, peer, previous_rate, bytes).await;
        previous.offered_bps =
            avian_runtime::next_probe_rate(previous_rate, PROBE_DURATION_MS, result.as_ref().ok());
        if let Err(error) = result {
            let message = error.to_string().to_ascii_lowercase();
            let reason = if message.contains("no authenticated replies") {
                "no_authenticated_probe_replies"
            } else if message.contains("queue") || message.contains("busy") {
                "link_busy"
            } else if message.contains("meter") {
                "metered_link"
            } else if message.contains("quality") || message.contains("fresh") {
                "quality_not_fresh"
            } else if message.contains("policy") {
                "policy_rejected"
            } else if message.contains("elapsed") || message.contains("timed out") {
                "measurement_timeout"
            } else if message.contains("invalid probe evidence") {
                "invalid_engine_evidence"
            } else if error.downcast_ref::<serde_json::Error>().is_some() {
                "invalid_engine_response"
            } else {
                "engine_probe_rejected"
            };
            let detail = json!({"reason":reason,"peer_id":peer,"connection_id":connection,
                "offered_bps":previous_rate,"duration_ms":PROBE_DURATION_MS});
            record("probe_rejected", Some(detail.clone()));
            overlay::event_with_detail(
                "capacity_probe_rejected",
                state
                    .snapshot()
                    .map_or(0, |membership| membership.revocation_revision()),
                Some(detail),
            );
        }
    }
}

use std::{convert::Infallible, sync::Arc, time::Duration};

use avian_routing::{NetworkSnapshot, RoutePlanner, RoutingPolicy, TrafficClass};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

use crate::{qualification::QualificationStatus, state::ApplicationState};

pub fn router(state: Arc<ApplicationState>) -> Router {
    Router::new()
        .route("/v1/status", get(status))
        .route("/v1/nodes", get(nodes))
        .route("/v1/services", get(services))
        .route("/v1/paths", get(paths))
        .route("/v1/radios", get(radios))
        .route("/v1/events", get(events))
        .route("/v1/qualification", get(qualification))
        .route("/v1/route-preview", post(route_preview))
        .route("/v1/network-control", post(network_control))
        .route("/v1/runtime", get(runtime_status))
        .layer(DefaultBodyLimit::max(4 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state)
}

async fn authenticate(
    State(state): State<Arc<ApplicationState>>,
    request: Request,
    next: Next,
) -> Response {
    // Browser clients use the ARC bridge; cross-origin access to this local API is not supported.
    if request.headers().contains_key(header::ORIGIN) {
        return error(
            StatusCode::FORBIDDEN,
            "browser_origin_not_allowed",
            "Use the ARC bridge for browser access.",
        );
    }
    let supplied_token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("");
    if !bool::from(supplied_token.as_bytes().ct_eq(state.api_token.as_bytes())) {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "A valid local API token is required.",
        );
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

async fn status(State(state): State<Arc<ApplicationState>>) -> Json<Value> {
    let runtime = match &state.runtime {
        Some(runtime) => Some(runtime.status().await),
        None => None,
    };
    let engine_ready = runtime.as_ref().is_some_and(|runtime| runtime.engine_ready);
    let membership = state.membership.read().await;
    Json(json!({
        "api_version": 1,
        "version": crate::version::BUILD_VERSION,
        "release": crate::version::RELEASE_TAG,
        "git_sha": crate::version::GIT_SHA,
        "git_dirty": crate::version::GIT_DIRTY,
        "mode": if runtime.is_some() { "isolated_runtime" } else { "observation_only" },
        "network_id": membership.trust_root().network_id,
        "revocation_revision": membership.revocation_revision(),
        "network_control_enabled": engine_ready,
        "runtime": runtime,
        "chud_configured": state.chud.is_some(),
        "membership_reload_error": state.reload_error.read().await.clone(),
        "data_plane": if engine_ready { "isolated_namespace" } else { "not_integrated" }
    }))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PageQuery {
    after: Option<String>,
    limit: Option<usize>,
}

async fn nodes(
    State(state): State<Arc<ApplicationState>>,
    Query(query): Query<PageQuery>,
) -> Response {
    let page_size = query.limit.unwrap_or(100);
    if !(1..=500).contains(&page_size) {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_page_size",
            "Page size must be between 1 and 500; this is a response budget, not a membership limit.",
        );
    }
    let membership = state.membership.read().await;
    let mut page = membership
        .memberships()
        .filter(|member| {
            query
                .after
                .as_ref()
                .is_none_or(|cursor| member.claims.node_id > *cursor)
        })
        .filter(|member| {
            membership
                .authorize(&member.claims.node_id, &member.claims.transport_public_key)
                .is_ok()
        })
        .take(page_size + 1)
        .map(|member| {
            json!({
                "node_id": member.claims.node_id,
                "overlay_addresses": member.claims.overlay_addresses,
                "permissions": member.claims.permissions,
                "membership_revision": member.claims.revision,
                "reachability": "unknown",
                "observed_at": null
            })
        })
        .collect::<Vec<_>>();
    let has_more = page.len() > page_size;
    page.truncate(page_size);
    let next_cursor =
        has_more.then(|| page.last().unwrap()["node_id"].as_str().unwrap().to_owned());
    Json(json!({ "items": page, "next_cursor": next_cursor, "revocation_revision": membership.revocation_revision() })).into_response()
}

async fn services() -> Json<Value> {
    Json(json!({"items": [], "next_cursor": null, "availability": "not_integrated"}))
}

async fn paths(State(state): State<Arc<ApplicationState>>) -> Response {
    let Some(runtime) = &state.runtime else {
        return Json(json!({"items": [], "availability": "not_integrated", "reason": "No isolated runtime is configured."})).into_response();
    };
    match runtime.inspect_routes().await {
        Ok(routes) => Json(json!({"items": routes, "availability": "observed", "kind": "engine_generic_routes", "per_class_route_observation": "unavailable"})).into_response(),
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "runtime_routes_unavailable", "Engine routes are unavailable; cached routes are not presented as fresh."),
    }
}

async fn qualification(State(state): State<Arc<ApplicationState>>) -> Json<QualificationStatus> {
    let mut qualification = QualificationStatus::default();
    if let Some(runtime) = &state.runtime {
        qualification.network_control_enabled = runtime.status().await.engine_ready;
    }
    Json(qualification)
}

async fn radios(State(state): State<Arc<ApplicationState>>) -> Response {
    let Some(chud) = &state.chud else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "chud_not_configured",
            "CHUD is not configured. This does not change network connectivity.",
        );
    };
    match chud.inventory().await {
        Ok(inventory) => Json(
            json!({"source": "chud", "devices": inventory.devices, "configuration_enabled": false}),
        )
        .into_response(),
        Err(error_details) => {
            tracing::warn!(error = %error_details, "CHUD inventory unavailable");
            error(
                StatusCode::BAD_GATEWAY,
                "chud_unavailable",
                "CHUD inventory is unavailable; no cached observation is presented as fresh.",
            )
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutePreviewRequest {
    snapshot: NetworkSnapshot,
    source: String,
    destination: String,
    traffic_class: TrafficClass,
    now_ms: u64,
    #[serde(default)]
    policy: RoutingPolicy,
}

async fn route_preview(
    State(state): State<Arc<ApplicationState>>,
    Json(request): Json<RoutePreviewRequest>,
) -> Response {
    let Ok(permit) = state.preview_slots.clone().try_acquire_owned() else {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "preview_capacity_reached",
            "Local route-preview workers are busy; retry later.",
        );
    };
    // Preview accepts caller-provided observations and never changes forwarding state.
    let outcome = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        RoutePlanner::new(request.policy).plan(
            &request.snapshot,
            &request.source,
            &request.destination,
            request.traffic_class,
            request.now_ms,
        )
    })
    .await;
    match outcome {
        Ok(Ok(route)) => {
            Json(json!({"source": "caller_supplied", "applied": false, "route": route}))
                .into_response()
        }
        Ok(Err(problem)) => error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "no_valid_preview",
            &problem.to_string(),
        ),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "preview_failed",
            "Route preview could not complete.",
        ),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeCommand {
    action: RuntimeAction,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum RuntimeAction {
    Start,
    Stop,
}

async fn network_control(State(state): State<Arc<ApplicationState>>, body: Bytes) -> Response {
    let Some(runtime) = &state.runtime else {
        return error(
            StatusCode::CONFLICT,
            "qualification_required",
            "Network control requires an explicitly configured isolated runtime.",
        );
    };
    let Ok(command) = serde_json::from_slice::<RuntimeCommand>(&body) else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_runtime_command",
            "Expected an action of start or stop; additional fields are rejected.",
        );
    };
    let result = match command.action {
        RuntimeAction::Start => runtime.start().await,
        RuntimeAction::Stop => runtime.stop().await,
    };
    match result {
        Ok(status) => Json(json!({"runtime": status})).into_response(),
        Err(problem) => error(
            StatusCode::CONFLICT,
            "runtime_control_rejected",
            &problem.to_string(),
        ),
    }
}

async fn runtime_status(State(state): State<Arc<ApplicationState>>) -> Response {
    match &state.runtime {
        Some(runtime) => Json(json!({"runtime": runtime.status().await})).into_response(),
        None => Json(json!({"runtime": null, "availability": "not_configured"})).into_response(),
    }
}

#[derive(Clone, Serialize)]
pub struct StateEvent {
    pub kind: String,
    pub revocation_revision: u64,
}

async fn events(State(state): State<Arc<ApplicationState>>) -> impl IntoResponse {
    let receiver = state.events.subscribe();
    let revision = state.membership.read().await.revocation_revision();
    let mode = if state.runtime.is_some() {
        "isolated_runtime"
    } else {
        "observation_only"
    };
    let initial = tokio_stream::iter([Ok::<_, Infallible>(
        Event::default()
            .event("snapshot")
            .data(json!({"revocation_revision": revision, "mode": mode}).to_string()),
    )]);
    let updates = BroadcastStream::new(receiver).map(|update| {
        let event = match update {
            Ok(update) => Event::default()
                .event("change")
                .data(serde_json::to_string(&update).unwrap()),
            Err(_) => Event::default().event("resync_required").data("{}"),
        };
        Ok::<_, Infallible>(event)
    });
    Sse::new(initial.chain(updates)).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"error": {"code": code, "message": message}})),
    )
        .into_response()
}

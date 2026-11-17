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
        .route("/v1/services", get(services).post(publish_services))
        .route("/v1/connections", get(connections))
        .route("/v1/paths", get(paths))
        .route("/v1/link-probes", post(probe_link))
        .route("/v1/links", get(links))
        .route("/v1/radios", get(radios))
        .route("/v1/radios/apply", post(apply_radio))
        .route("/v1/radios/confirm", post(confirm_radio))
        .route("/v1/radios/operations", get(radio_operations))
        .route("/v1/radios/snapshot", get(radio_snapshot))
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
        "mode": runtime.as_ref().map_or("observation_only", |runtime| match runtime.execution { avian_runtime::ExecutionMode::IsolatedNamespace=>"isolated_runtime", avian_runtime::ExecutionMode::Native=>"native_runtime" }),
        "network_id": membership.trust_root().network_id,
        "revocation_revision": membership.revocation_revision(),
        "network_control_enabled": engine_ready,
        "runtime": runtime,
        "chud_configured": state.chud.is_some(),
        "membership_reload_error": state.reload_error.read().await.clone(),
        "data_plane": if engine_ready { runtime.as_ref().map_or("not_integrated", |runtime| if runtime.isolated { "isolated_namespace" } else { "native" }) } else { "not_integrated" },
        "service_discovery_configured": state.services.is_some()
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

async fn services(
    State(state): State<Arc<ApplicationState>>,
    Query(query): Query<PageQuery>,
) -> Response {
    let Some(directory) = &state.services else {
        return Json(json!({"items": [], "next_cursor": null, "availability": "not_integrated"}))
            .into_response();
    };
    let Ok(now) = crate::service_exchange::now_unix() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "clock_unavailable",
            "The service clock is unavailable.",
        );
    };
    let membership = state.membership.read().await;
    match directory.page(
        &membership,
        query.after.as_deref(),
        query.limit.unwrap_or(100),
        now,
    ) {
        Ok(page) => Json(page).into_response(),
        Err(_) => error(
            StatusCode::BAD_REQUEST,
            "invalid_service_page",
            "Invalid service page or unavailable registry.",
        ),
    }
}

async fn publish_services(
    State(state): State<Arc<ApplicationState>>,
    Json(publication): Json<crate::services::LocalPublication>,
) -> Response {
    let Some(directory) = state.services.clone() else {
        return error(
            StatusCode::CONFLICT,
            "services_not_configured",
            "No service registry is configured.",
        );
    };
    let Ok(now) = crate::service_exchange::now_unix() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "clock_unavailable",
            "The service clock is unavailable.",
        );
    };
    let membership = state.membership.read().await.clone();
    match tokio::task::spawn_blocking(move || directory.publish(&membership, publication, now))
        .await
    {
        Ok(Ok(announcement)) => {
            let revision = state.membership.read().await.revocation_revision();
            let _ = state.events.send(StateEvent {
                kind: "services_updated".into(),
                revocation_revision: revision,
            });
            Json(json!({"announcement":announcement,"availability":"advertised"})).into_response()
        }
        _ => error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "service_publication_rejected",
            "Service publication failed verification or protected persistence.",
        ),
    }
}

async fn connections(State(state): State<Arc<ApplicationState>>) -> Response {
    let Some(runtime) = &state.runtime else {
        return Json(json!({"availability":"not_configured","items":[]})).into_response();
    };
    match runtime.inspect_underlays().await {
        Ok(interfaces)=>Json(json!({"availability":"observed","items":interfaces,"local_discovery_authenticates_membership":false})).into_response(),
        Err(_)=>error(StatusCode::SERVICE_UNAVAILABLE,"interfaces_unavailable","No fresh interface inventory is available."),
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PathQuery {
    after: Option<String>,
    limit: Option<usize>,
    #[serde(alias = "class")]
    traffic_class: Option<TrafficClass>,
}
async fn paths(
    State(state): State<Arc<ApplicationState>>,
    Query(query): Query<PathQuery>,
) -> Response {
    let Some(runtime) = &state.runtime else {
        return Json(json!({"items": [], "availability": "not_integrated", "reason": "No isolated runtime is configured."})).into_response();
    };
    if runtime.configuration().measured_routing {
        return match runtime
            .inspect_class_routes_page(
                query.after.as_deref(),
                query.limit.unwrap_or(100),
                query.traffic_class,
            )
            .await
        {
            Ok(mut page) if page.is_object() => {
                page["availability"] = json!("observed");
                page["kind"] = json!("per_class_routes");
                page["per_class_route_observation"] = json!("observed");
                Json(page).into_response()
            }
            _ => error(
                StatusCode::SERVICE_UNAVAILABLE,
                "runtime_routes_unavailable",
                "No fresh per-class forwarding decisions are available.",
            ),
        };
    }
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
            json!({"source": "chud", "devices": inventory.devices, "configuration_enabled": chud.configuration_allowed()}),
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
    let mode = state
        .runtime
        .as_ref()
        .map_or("observation_only", |runtime| {
            match runtime.configuration().execution {
                avian_runtime::ExecutionMode::IsolatedNamespace => "isolated_runtime",
                avian_runtime::ExecutionMode::Native => "native_runtime",
            }
        });
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RadioQuery {
    mac: Option<String>,
}
async fn apply_radio(
    State(state): State<Arc<ApplicationState>>,
    Json(request): Json<crate::chud::RadioApply>,
) -> Response {
    let Some(chud) = &state.chud else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "chud_not_configured",
            "CHUD is not configured.",
        );
    };
    match chud.apply(request).await {
        Ok(result) => (StatusCode::ACCEPTED, Json(result)).into_response(),
        Err(_) => error(
            StatusCode::CONFLICT,
            "radio_apply_rejected",
            "Radio changes were refused or have an uncertain outcome. Inspect CHUD operations before trying again.",
        ),
    }
}
async fn confirm_radio(
    State(state): State<Arc<ApplicationState>>,
    Json(request): Json<crate::chud::RadioConfirm>,
) -> Response {
    let Some(chud) = &state.chud else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "chud_not_configured",
            "CHUD is not configured.",
        );
    };
    match chud.confirm(request).await {
        Ok(result) => Json(result).into_response(),
        Err(_) => error(
            StatusCode::CONFLICT,
            "radio_confirmation_rejected",
            "CHUD did not confirm the radio transaction.",
        ),
    }
}
async fn radio_operations(
    State(state): State<Arc<ApplicationState>>,
    Query(query): Query<RadioQuery>,
) -> Response {
    let Some(chud) = &state.chud else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "chud_not_configured",
            "CHUD is not configured.",
        );
    };
    match chud.operations(query.mac.as_deref()).await {
        Ok(result) => Json(result).into_response(),
        Err(_) => error(
            StatusCode::BAD_GATEWAY,
            "radio_operations_unavailable",
            "CHUD operations are unavailable.",
        ),
    }
}
async fn radio_snapshot(
    State(state): State<Arc<ApplicationState>>,
    Query(query): Query<RadioQuery>,
) -> Response {
    let Some(chud) = &state.chud else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "chud_not_configured",
            "CHUD is not configured.",
        );
    };
    let Some(mac) = query.mac else {
        return error(
            StatusCode::BAD_REQUEST,
            "radio_required",
            "A radio MAC address is required.",
        );
    };
    match chud.snapshot(&mac).await {
        Ok(result) => Json(result).into_response(),
        Err(_) => error(
            StatusCode::BAD_GATEWAY,
            "radio_snapshot_unavailable",
            "CHUD radio status is unavailable.",
        ),
    }
}

async fn probe_link(
    State(state): State<Arc<ApplicationState>>,
    Json(request): Json<avian_runtime::LinkProbeRequest>,
) -> Response {
    let Some(runtime) = &state.runtime else {
        return error(
            StatusCode::CONFLICT,
            "runtime_not_configured",
            "No runtime is configured.",
        );
    };
    match runtime.probe_link(request).await {
        Ok(result) => Json(result).into_response(),
        Err(_) => error(
            StatusCode::CONFLICT,
            "probe_rejected",
            "Capacity probe was refused by lifecycle, link policy, or resource budgets.",
        ),
    }
}

async fn links(
    State(state): State<Arc<ApplicationState>>,
    Query(query): Query<PageQuery>,
) -> Response {
    let Some(runtime) = &state.runtime else {
        return error(
            StatusCode::CONFLICT,
            "runtime_not_configured",
            "No runtime is configured.",
        );
    };
    match runtime
        .inspect_links_page(query.after.as_deref(), query.limit.unwrap_or(100))
        .await
    {
        Ok(mut page) if page.is_object() => {
            page["availability"] = json!("observed");
            page["capacity_is_spare_bandwidth"] = json!(false);
            Json(page).into_response()
        }
        _ => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "links_unavailable",
            "No fresh direct-link observations are available.",
        ),
    }
}

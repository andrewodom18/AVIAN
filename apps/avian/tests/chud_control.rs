use avian::{
    chud::{ChudClient, RadioApply, RadioConfirm},
    configuration::{ChudConfiguration, create_secret_file},
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

async fn apply(
    State(calls): State<Arc<Mutex<Vec<Value>>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    assert_eq!(headers["X-API-Key"], "a".repeat(64));
    calls.lock().unwrap().push(body);
    (
        StatusCode::ACCEPTED,
        Json(json!({"operation_id":"operation-one","defer_persist":true})),
    )
}
async fn confirm(
    State(calls): State<Arc<Mutex<Vec<Value>>>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    calls.lock().unwrap().push(body);
    Json(json!({"ok":true}))
}
async fn snapshot() -> Json<Value> {
    Json(
        json!({"config":{"password":{"value":"do-not-return","meta":{"sensitive":true}},"channel":{"value":7,"meta":{"sensitive":false}}},"mutation_allowed":true}),
    )
}

#[tokio::test]
async fn radio_writes_require_explicit_actions_and_remain_deferred() {
    let directory = tempfile::tempdir().unwrap();
    let credential = directory.path().join("chud.key");
    create_secret_file(&credential, "a".repeat(64).as_bytes()).unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let router = Router::new()
        .route("/api/radio/apply", post(apply))
        .route("/api/radio/confirm", post(confirm))
        .route("/api/radio/snapshot", get(snapshot))
        .with_state(calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut configuration = ChudConfiguration {
        allow_configuration: false,
        url: format!("http://{address}"),
        api_key_file: credential,
        certificate_file: None,
    };
    let request = || {
        serde_json::from_value::<RadioApply>(json!({"mac":"02:00:00:00:00:01","desired":{"rf.channel":{"value":7,"block":"rf"}},"acknowledge_disruption":true})).unwrap()
    };
    assert!(
        ChudClient::new(&configuration)
            .unwrap()
            .apply(request())
            .await
            .is_err()
    );
    assert!(calls.lock().unwrap().is_empty());
    configuration.allow_configuration = true;
    let client = ChudClient::new(&configuration).unwrap();
    let mut unacknowledged = request();
    unacknowledged.acknowledge_disruption = false;
    assert!(client.apply(unacknowledged).await.is_err());
    assert!(calls.lock().unwrap().is_empty());
    let status = client.apply(request()).await.unwrap();
    assert_eq!(status["operation_id"], "operation-one");
    {
        let operations = calls.lock().unwrap();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0]["defer_persist"], true);
        assert!(operations[0].get("acknowledge_disruption").is_none());
    }
    assert!(
        client
            .confirm(RadioConfirm {
                operation_id: "operation-one".into(),
                acknowledge_persistence: false
            })
            .await
            .is_err()
    );
    assert_eq!(calls.lock().unwrap().len(), 1);
    client
        .confirm(RadioConfirm {
            operation_id: "operation-one".into(),
            acknowledge_persistence: true,
        })
        .await
        .unwrap();
    assert_eq!(calls.lock().unwrap().len(), 2);
    let result = client.snapshot("02:00:00:00:00:01").await.unwrap();
    assert_eq!(result["config"]["password"]["value"], "<redacted>");
    assert_eq!(result["config"]["channel"]["value"], 7);
    assert!(client.snapshot("../../credentials").await.is_err());
    assert!(serde_json::from_value::<RadioApply>(json!({"mac":"02:00:00:00:00:01","desired":{},"acknowledge_disruption":true,"defer_persist":false})).is_err());
    server.abort();
}

use axum::{
    Json, Router,
    extract::{Path, Query},
    http::HeaderMap,
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};
use tempfile::TempDir;

fn cli(home: &TempDir, state: &TempDir, args: &[&str]) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_opencode-scheduler"))
        .env("OPENCODE_SCHEDULER_HOME", home.path().join("private"))
        .env("XDG_STATE_HOME", state.path())
        .args(args)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

async fn fixture(state: &TempDir, posts: Arc<Mutex<Vec<Value>>>) -> tokio::task::JoinHandle<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::fs::create_dir_all(state.path().join("opencode")).unwrap();
    std::fs::write(
        state.path().join("opencode/service.json"),
        serde_json::to_vec(&json!({
            "url":url,"pid":std::process::id(),"version":"2.2.5","password":"fixture-password"
        }))
        .unwrap(),
    )
    .unwrap();
    let app = Router::new()
        .route("/api/info", get(|h:HeaderMap| async move {
            assert_eq!(h["authorization"], "Basic b3BlbmNvZGU6Zml4dHVyZS1wYXNzd29yZA==");
            Json(json!({"version":"2.2.5","pid":std::process::id(),"urls":[],"paths":{"tmp":"fixture"}}))
        }))
        .route("/api/session", get(|Query(q):Query<HashMap<String,String>>| async move {
            let (id,next) = match q.get("cursor").map(String::as_str) { None => ("ses_a",Some("second")),Some("second") => ("ses_b",Some("third")),_ => ("ses_c",None) };
            Json(json!({"data":[{"id":id,"title":"Pilot exact"},{"id":"ses_other","title":"pilot exact"}],"cursor":{"next":next}}))
        }))
        .route("/api/session/{id}", get(|Path(id):Path<String>| async move {Json(json!({"data":{"id":id,"title":"Pilot exact"}}))}))
        .route("/api/session/{id}/inbox", get(|| async {Json(json!({"data":[]}))}))
        .route("/api/session/{id}/message/{msg}", get(|| async {(axum::http::StatusCode::NOT_FOUND,Json(json!({"_tag":"MessageNotFoundError","sessionID":"ses_a","messageID":"msg_none","message":"not found"})))}))
        .route("/api/session/{id}/prompt", post(move |Path(id):Path<String>,Json(v):Json<Value>| {let posts=posts.clone(); async move {
            posts.lock().unwrap().push(v.clone());
            Json(json!({"data":{"id":v["id"],"sessionID":id,"type":"user","time":{"created":1},"payload":{"text":"canonical transformed"},"delivery":v["delivery"]}}))
        }}));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_detached_title_batch_frozen_create_show_cancel_log_and_delivery() {
    // Catches partial title fanout, transport-dependent duplicate creation, non-detached launch,
    // missing auth, lost stable message identity and CLI/store bypasses through the real binary.
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let posts = Arc::new(Mutex::new(Vec::new()));
    let server = fixture(&state, posts.clone()).await;
    let absent = cli(&home, &state, &["status"]);
    assert_eq!(absent["result"]["data"]["daemon"]["reachable"], false);
    assert!(!home.path().join("private/endpoint.json").exists());
    std::thread::scope(|scope| {
        let first = scope.spawn(|| cli(&home, &state, &["daemon", "start"]));
        let second = scope.spawn(|| cli(&home, &state, &["daemon", "start"]));
        assert_eq!(
            first.join().unwrap()["result"]["data"]["daemon"]["reachable"],
            true
        );
        assert_eq!(
            second.join().unwrap()["result"]["data"]["daemon"]["reachable"],
            true
        );
    });
    let descriptor: Value =
        serde_json::from_slice(&std::fs::read(home.path().join("private/endpoint.json")).unwrap())
            .unwrap();
    let concurrent = cli(&home, &state, &["daemon", "start"]);
    assert_eq!(concurrent["result"]["data"]["daemon"]["reachable"], true);
    let descriptor2: Value =
        serde_json::from_slice(&std::fs::read(home.path().join("private/endpoint.json")).unwrap())
            .unwrap();
    assert_eq!(descriptor["instance_id"], descriptor2["instance_id"]);
    let client = reqwest::Client::new();
    let base = descriptor["base_url"].as_str().unwrap();
    assert_eq!(
        client
            .get(format!("{base}/v1/health"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .get(format!("{base}/v1/health"))
            .bearer_auth(descriptor["token"].as_str().unwrap())
            .header("X-Scheduler-Instance", uuid::Uuid::new_v4().to_string())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .get(format!("{base}/v1/health"))
            .bearer_auth(descriptor["token"].as_str().unwrap())
            .header(
                "X-Scheduler-Instance",
                descriptor["instance_id"].as_str().unwrap()
            )
            .header("Origin", "https://foreign.invalid")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let invalid = client
        .post(format!("{base}/v1/rpc"))
        .bearer_auth(descriptor["token"].as_str().unwrap())
        .header(
            "X-Scheduler-Instance",
            descriptor["instance_id"].as_str().unwrap(),
        )
        .json(&json!({"op":"status","input":{"unexpected":true}}))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), 400);
    let at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let rid = uuid::Uuid::new_v4().to_string();
    let args = [
        "schedule",
        "create",
        "--session-title",
        "Pilot exact",
        "--at",
        &at,
        "--prompt",
        "fixture text",
        "--request-id",
        &rid,
    ];
    let created = cli(&home, &state, &args);
    let schedules = created["result"]["data"]["schedules"].as_array().unwrap();
    assert_eq!(schedules.len(), 3);
    assert_eq!(
        schedules
            .iter()
            .map(|s| s["target"]["session_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ses_a", "ses_b", "ses_c"]
    );
    server.abort();
    assert_eq!(cli(&home, &state, &args), created);
    for s in schedules {
        let id = s["id"].as_str().unwrap();
        assert_eq!(
            cli(&home, &state, &["schedule", "show", id])["result"]["data"]["state"],
            "pending"
        );
        assert_eq!(
            cli(&home, &state, &["schedule", "cancel", id])["result"]["data"]["disposition"],
            "cancelled"
        );
        assert_eq!(
            cli(&home, &state, &["schedule", "log", id])["result"]["data"]["items"][0]["outcome"],
            "cancelled"
        );
    }
    assert_eq!(
        cli(&home, &state, &["schedule", "list"])["result"]["data"]["items"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert!(posts.lock().unwrap().is_empty());
    cli(&home, &state, &["daemon", "stop"]);
    assert!(!home.path().join("private/endpoint.json").exists());
    let server = fixture(&state, posts.clone()).await;
    // Restart keeps all cancelled rows; a new due prompt goes through the worker exactly once.
    cli(&home, &state, &["daemon", "start"]);
    let at = (chrono::Utc::now() + chrono::Duration::seconds(3)).to_rfc3339();
    let created = cli(
        &home,
        &state,
        &[
            "schedule",
            "create",
            "--session",
            "ses_a",
            "--at",
            &at,
            "--prompt",
            "exact payload",
            "--delivery",
            "queue",
        ],
    );
    let s = &created["result"]["data"]["schedules"][0];
    let id = s["id"].as_str().unwrap();
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        cli(&home, &state, &["schedule", "show", id])["result"]["data"]["state"],
        "admitted"
    );
    assert_eq!(
        posts.lock().unwrap().as_slice(),
        [json!({"id":s["message_id"],"text":"exact payload","delivery":"queue"})]
    );
    cli(&home, &state, &["daemon", "stop"]);
    server.abort();
}

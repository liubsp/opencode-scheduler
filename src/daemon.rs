use crate::{
    api::Service,
    runtime::{Endpoint, Paths, private_file, write_json},
    types::*,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use uuid::Uuid;

#[derive(Clone)]
struct StateData {
    service: Arc<Service>,
    endpoint: Endpoint,
}
fn authorized(headers: &HeaderMap, endpoint: &Endpoint) -> bool {
    !headers.contains_key("origin")
        && headers.get("authorization").and_then(|h| h.to_str().ok())
            == Some(&format!("Bearer {}", endpoint.token))
        && headers
            .get("X-Scheduler-Instance")
            .and_then(|h| h.to_str().ok())
            == Some(&endpoint.instance_id)
}
fn error_response(error: ApiError) -> Response {
    let error = error.sanitized().unwrap_or_else(|_| ApiError::storage());
    let status = match error.code.as_str() {
        "authentication_failure" => 401,
        "unsupported_protocol" => 426,
        "missing_session" | "schedule_missing" => 404,
        "request_conflict" | "already_admitted" | "outcome_uncertain" => 409,
        "resource_limit" => 413,
        "invalid_input" | "invalid_time" | "time_in_past" | "local_time_invalid" => 400,
        _ => 503,
    };
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        axum::Json(json!({"ok":false,"error":error})),
    )
        .into_response()
}

async fn health(State(state): State<StateData>, headers: HeaderMap) -> Response {
    if !authorized(&headers, &state.endpoint) {
        return error_response(ApiError::new(
            "authentication_failure",
            "Service authentication failed.",
        ));
    }
    axum::Json(json!({"protocol":1,"instance_id":state.endpoint.instance_id,"binary_version":env!("CARGO_PKG_VERSION")})).into_response()
}
async fn rpc(State(state): State<StateData>, request: Request) -> Response {
    if !authorized(request.headers(), &state.endpoint) {
        return error_response(ApiError::new(
            "authentication_failure",
            "Service authentication failed.",
        ));
    }
    let bytes = match to_bytes(request.into_body(), MAX_REQUEST_BYTES).await {
        Ok(v) => v,
        Err(_) => {
            return error_response(ApiError::new(
                "resource_limit",
                "Scheduler resource limit exceeded.",
            ));
        }
    };
    let operation: Operation = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return error_response(ApiError::invalid()),
    };
    match state.service.execute(operation).await {
        Ok(mut result) => {
            result.canonicalize_errors();
            let data = match serde_json::to_vec(&json!({"ok":true,"result":result})) {
                Ok(data) if data.len() <= 16 * 1024 * 1024 => data,
                _ => {
                    return error_response(ApiError::new(
                        "resource_limit",
                        "Scheduler resource limit exceeded.",
                    ));
                }
            };
            ([("content-type", "application/json")], Body::from(data)).into_response()
        }
        Err(error) => error_response(error),
    }
}
async fn worker(service: Arc<Service>, unfinished: Vec<AttemptIntent>) -> Result<(), ApiError> {
    let mut adapter = service.adapter.clone();
    for intent in unfinished {
        if service.stopping.load(Ordering::SeqCst) {
            return Ok(());
        }
        let result = adapter.reconcile(&intent).await;
        service.store()?.finish_attempt(&intent, result)?;
    }
    loop {
        if service.stopping.load(Ordering::SeqCst) {
            return Ok(());
        }
        let intent = service
            .store()?
            .claim_due(Utc::now(), 1)?
            .into_iter()
            .next();
        if let Some(intent) = intent {
            let result = adapter
                .deliver(&intent, || {
                    service
                        .store()?
                        .mark_submit_intent(&intent.schedule_id, intent.ordinal)
                })
                .await;
            service.store()?.finish_attempt(&intent, result)?;
        } else {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}
pub async fn run(paths: Paths) -> Result<(), ApiError> {
    paths.prepare()?;
    #[cfg(windows)]
    let _diagnostics = crate::launch::diagnostics(&paths.file("diagnostic.log"))?;
    let lifetime = private_file(&paths.file("daemon.lock"))?;
    lifetime
        .try_lock_exclusive()
        .map_err(|_| ApiError::coded("daemon_startup_failure"))?;
    let service = Service::new(paths.clone())?;
    // Recovery is captured once before exposing the API or starting the live worker.
    let unfinished = service.store()?.unfinished(32)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| ApiError::coded("daemon_startup_failure"))?;
    let endpoint = Endpoint {
        protocol: 1,
        instance_id: Uuid::new_v4().to_string(),
        base_url: format!(
            "http://{}",
            listener
                .local_addr()
                .map_err(|_| ApiError::coded("daemon_startup_failure"))?
        ),
        pid: std::process::id(),
        token: format!(
            "{}{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        ),
    };
    write_json(&paths.file("endpoint.json"), &endpoint)?;
    let app = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/rpc", post(rpc))
        .with_state(StateData {
            service: service.clone(),
            endpoint: endpoint.clone(),
        });
    let mut server = tokio::spawn(async move { axum::serve(listener, app).await });
    let mut worker = tokio::spawn(worker(service.clone(), unfinished));
    let result = tokio::select! {
        _=service.stop.notified()=>Ok(()),
        result=&mut worker=>match result{Ok(result)=>result,Err(_)=>Err(ApiError::storage())},
        _=&mut server=>Err(ApiError::coded("daemon_unavailable")),
    };
    service.stopping.store(true, Ordering::SeqCst);
    if !worker.is_finished()
        && tokio::time::timeout(Duration::from_secs(10), &mut worker)
            .await
            .is_err()
    {
        worker.abort();
        let _ = worker.await;
    }
    // Leave unfinished submit intents durable if shutdown cut a network wait short.
    tokio::time::sleep(Duration::from_millis(50)).await;
    server.abort();
    let _ = server.await;
    if let Ok(bytes) = crate::runtime::read_bounded(&paths.file("endpoint.json"), 64 * 1024)
        && serde_json::from_slice::<Value>(&bytes)
            .is_ok_and(|value| value["instance_id"] == endpoint.instance_id)
    {
        std::fs::remove_file(paths.file("endpoint.json")).map_err(|_| ApiError::storage())?;
    }
    drop(lifetime);
    result
}

#[cfg(test)]
mod error_tests {
    use super::*;

    #[tokio::test]
    async fn rpc_errors_have_recovery_hints_and_correct_safe_status() {
        for (code, status, hint) in [
            ("missing_session", 404, "ALL"),
            ("invalid_time", 400, "2099-11-06T05:00:00+01:00"),
            ("time_in_past", 400, "future"),
            ("local_time_invalid", 400, "offset"),
            ("timezone_unavailable", 503, "offset"),
        ] {
            let response = error_response(ApiError::new(code, "private SQL Bearer token"));
            assert_eq!(response.status().as_u16(), status);
            let bytes = to_bytes(response.into_body(), 2048).await.unwrap();
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            let message = value["error"]["message"].as_str().unwrap();
            assert!(message.contains(hint));
            assert!(!message.contains("Bearer"));
        }
    }
}

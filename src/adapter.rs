use crate::types::*;
use chrono::Utc;
use reqwest::{Client, Method, StatusCode, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    time::Duration,
};

pub fn unavailable() -> ApiError {
    ApiError::new("opencode_unavailable", "OpenCode service is unavailable.")
}
fn unsupported() -> ApiError {
    ApiError::new("unsupported_service", "OpenCode service is unsupported.")
}
fn missing() -> ApiError {
    ApiError::new("missing_session", "Target session does not exist.")
}

#[derive(Clone)]
pub struct Adapter {
    client: Client,
    record_path: PathBuf,
    record: Option<Record>,
    discovery_transport_failed: bool,
}
#[derive(Clone, Deserialize)]
struct Record {
    url: String,
    pid: u32,
    version: Option<String>,
    password: Option<String>,
}
#[derive(Clone, Deserialize)]
pub struct ServiceInfo {
    pub version: String,
    pub pid: u32,
    pub urls: Vec<String>,
    pub paths: Value,
}

pub fn local_url(input: &str, ipv4_only: bool) -> Result<Url, ApiError> {
    let url = Url::parse(input).map_err(|_| unsupported())?;
    let local = match url.host_str() {
        Some("localhost") if !ipv4_only => true,
        Some(host) => host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback() && (!ipv4_only || ip.is_ipv4())),
        _ => false,
    };
    if url.scheme() != "http"
        || !local
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(unsupported());
    }
    Ok(url)
}

pub async fn bounded_json(mut response: reqwest::Response, max: usize) -> Result<Value, ApiError> {
    if response.content_length().is_some_and(|n| n > max as u64) {
        return Err(ApiError::new(
            "resource_limit",
            "Scheduler resource limit exceeded.",
        ));
    }
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if data.len() + chunk.len() > max {
            return Err(ApiError::new(
                "resource_limit",
                "Scheduler resource limit exceeded.",
            ));
        }
        data.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&data).map_err(|_| unsupported())
}

impl Adapter {
    pub fn new() -> Result<Self, ApiError> {
        let root = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .or_else(|| std::env::var_os("HOME"))
                    .map(|p| PathBuf::from(p).join(".local/state"))
            })
            .ok_or_else(unavailable)?;
        Self::from_record(root.join("opencode/service.json"))
    }
    fn from_record(record_path: PathBuf) -> Result<Self, ApiError> {
        Ok(Self {
            client: Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .connect_timeout(Duration::from_secs(2))
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(|_| unavailable())?,
            record_path,
            record: None,
            discovery_transport_failed: false,
        })
    }
    async fn response(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<reqwest::Response, ApiError> {
        let record = self.record.as_ref().ok_or_else(unavailable)?;
        let base = local_url(&record.url, false)?;
        let mut request = self
            .client
            .request(method, base.join(path).map_err(|_| unsupported())?);
        if let Some(password) = &record.password {
            request = request.basic_auth("opencode", Some(password));
        }
        if let Some(body) = body {
            request = request.json(&body)
        }
        let response = request.send().await.map_err(|_| unavailable())?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(ApiError::new(
                "authentication_failure",
                "Service authentication failed.",
            ));
        }
        if status.is_redirection() {
            return Err(unsupported());
        }
        Ok(response)
    }
    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<(StatusCode, Value), ApiError> {
        let response = self.response(method, path, body).await?;
        let status = response.status();
        Ok((status, bounded_json(response, 8 * 1024 * 1024).await?))
    }
    async fn probe(&mut self) -> Result<ServiceInfo, ApiError> {
        self.discovery_transport_failed = false;
        let bytes = crate::runtime::read_bounded(&self.record_path, 64 * 1024)
            .map_err(|_| unavailable())?;
        let record: Record = serde_json::from_slice(&bytes).map_err(|_| unsupported())?;
        local_url(&record.url, false)?;
        self.record = Some(record.clone());
        let response = match self.response(Method::GET, "/api/info", None).await {
            Ok(response) => response,
            Err(error) => {
                self.discovery_transport_failed = error.code == "opencode_unavailable";
                return Err(error);
            }
        };
        let status = response.status();
        if !status.is_success() {
            return Err(unavailable());
        }
        let value = bounded_json(response, 8 * 1024 * 1024).await?;
        let info: ServiceInfo = serde_json::from_value(value).map_err(|_| unsupported())?;
        let version = semver::Version::parse(&info.version).map_err(|_| unsupported())?;
        if version.major != 2
            || version < semver::Version::new(2, 2, 0)
            || info.pid != record.pid
            || record.version.as_ref().is_some_and(|v| v != &info.version)
            || !info.paths.get("tmp").is_some_and(Value::is_string)
        {
            return Err(unsupported());
        }
        Ok(info)
    }
    pub async fn discover(&mut self) -> Result<ServiceInfo, ApiError> {
        // Observation only, including status. Registration/auth rotation is read each cycle.
        self.probe().await
    }
    async fn ensure(&mut self) -> Result<ServiceInfo, ApiError> {
        self.ensure_with(
            crate::bootstrap::command(),
            Duration::from_secs(15),
            crate::bootstrap::verified_dead,
        )
        .await
    }
    async fn ensure_with(
        &mut self,
        command: tokio::process::Command,
        bound: Duration,
        verified_dead: fn(u32) -> bool,
    ) -> Result<ServiceInfo, ApiError> {
        let error = match self.discover().await {
            Ok(info) => return Ok(info),
            Err(error) => error,
        };
        if error.code != "opencode_unavailable" {
            return Err(error);
        }
        let eligible = match std::fs::symlink_metadata(&self.record_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Ok(meta)
                if self.discovery_transport_failed
                    && meta.is_file()
                    && !meta.file_type().is_symlink() =>
            {
                crate::runtime::read_bounded(&self.record_path, 64 * 1024)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok())
                    .is_some_and(|record| {
                        record.pid != 0
                            && local_url(&record.url, false).is_ok()
                            && verified_dead(record.pid)
                    })
            }
            _ => false,
        };
        if !eligible {
            return Err(error);
        }
        crate::bootstrap::run(command, bound).await?;
        self.discover().await
    }
    async fn target(&self, id: &str) -> Result<Target, ApiError> {
        validate_session_id(id)?;
        let (status, value) = self
            .request(Method::GET, &format!("/api/session/{id}"), None)
            .await?;
        if status == StatusCode::NOT_FOUND {
            return Err(missing());
        }
        if !status.is_success() {
            return Err(unavailable());
        }
        let data = &value["data"];
        if data["id"].as_str() != Some(id) {
            return Err(unsupported());
        }
        let title = match data.get("title") {
            Some(Value::String(s)) => Some(s.clone()),
            None | Some(Value::Null) => None,
            _ => return Err(unsupported()),
        };
        Ok(Target {
            session_id: id.into(),
            title,
        })
    }
    pub async fn resolve_targets(
        &mut self,
        selector: &TargetSelector,
    ) -> Result<Vec<Target>, ApiError> {
        tokio::time::timeout(Duration::from_secs(30), self.resolve_inner(selector))
            .await
            .map_err(|_| unavailable())?
    }
    async fn resolve_inner(&mut self, selector: &TargetSelector) -> Result<Vec<Target>, ApiError> {
        self.ensure().await?;
        if let TargetSelector::Id(id) = selector {
            return Ok(vec![self.target(id).await?]);
        }
        let TargetSelector::Title(title) = selector else {
            unreachable!()
        };
        let mut matches = BTreeMap::new();
        let mut seen = HashSet::new();
        let mut cursor: Option<String> = None;
        for page in 0..100 {
            let mut path = Url::parse("http://localhost/api/session").map_err(|_| unsupported())?;
            path.query_pairs_mut().append_pair("limit", "100");
            if let Some(cursor) = &cursor {
                path.query_pairs_mut().append_pair("cursor", cursor);
            }
            let path = format!("{}?{}", path.path(), path.query().unwrap_or_default());
            let (status, value) = self.request(Method::GET, &path, None).await?;
            if !status.is_success() {
                return Err(unavailable());
            }
            let data = value["data"].as_array().ok_or_else(unsupported)?;
            if data.len() > 100 {
                return Err(ApiError::new(
                    "resource_limit",
                    "Scheduler resource limit exceeded.",
                ));
            }
            for item in data {
                let id = item["id"].as_str().ok_or_else(unsupported)?;
                validate_session_id(id).map_err(|_| unsupported())?;
                if item["title"].as_str() == Some(title) {
                    matches.insert(id.to_string(), ());
                }
                if matches.len() > 100 {
                    return Err(ApiError::new(
                        "resource_limit",
                        "Scheduler resource limit exceeded.",
                    ));
                }
            }
            if !value["cursor"].is_object() {
                return Err(unsupported());
            }
            cursor = match value["cursor"].get("next") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) if !s.is_empty() && s.len() <= 512 => Some(s.clone()),
                _ => return Err(unsupported()),
            };
            if let Some(next) = &cursor {
                if !seen.insert(next.clone()) || page == 99 {
                    return Err(ApiError::new(
                        "resource_limit",
                        "Scheduler resource limit exceeded.",
                    ));
                }
            } else {
                break;
            }
        }
        if matches.is_empty() {
            return Err(missing());
        }
        let mut targets = Vec::new();
        for id in matches.keys() {
            targets.push(self.target(id).await?)
        }
        Ok(targets)
    }
    async fn evidence(&self, intent: &AttemptIntent) -> Result<bool, ApiError> {
        let (status, value) = self
            .request(
                Method::GET,
                &format!("/api/session/{}/inbox", intent.session_id),
                None,
            )
            .await?;
        if status == StatusCode::NOT_FOUND {
            return Err(missing());
        }
        if !status.is_success() {
            return Err(unavailable());
        }
        for item in value["data"].as_array().ok_or_else(unsupported)? {
            if item["id"].as_str() == Some(&intent.message_id) {
                if admission_matches(item, intent, true) {
                    return Ok(true);
                } else {
                    return Err(unsupported());
                }
            }
        }
        let (status, value) = self
            .request(
                Method::GET,
                &format!(
                    "/api/session/{}/message/{}",
                    intent.session_id, intent.message_id
                ),
                None,
            )
            .await?;
        if status == StatusCode::NOT_FOUND {
            return match value["_tag"].as_str() {
                Some("MessageNotFoundError") => Ok(false),
                Some("SessionNotFoundError") => Err(missing()),
                _ => Err(unsupported()),
            };
        }
        if !status.is_success() {
            return Err(unavailable());
        }
        if admission_matches(&value["data"], intent, false) {
            Ok(true)
        } else {
            Err(unsupported())
        }
    }
    pub async fn reconcile(&mut self, intent: &AttemptIntent) -> AttemptResult {
        let result = tokio::time::timeout(Duration::from_secs(60), async {
            self.ensure().await?;
            self.evidence(intent).await
        })
        .await;
        match result {
            Ok(Ok(true)) => admitted(AttemptPhase::Reconcile),
            Ok(Ok(false)) => failure(
                AttemptPhase::Reconcile,
                intent.previous_certainty,
                unavailable(),
            ),
            Ok(Err(error)) => failure(AttemptPhase::Reconcile, intent.previous_certainty, error),
            Err(_) => failure(
                AttemptPhase::Reconcile,
                intent.previous_certainty,
                unavailable(),
            ),
        }
    }
    pub async fn deliver<F>(&mut self, intent: &AttemptIntent, before_submit: F) -> AttemptResult
    where
        F: FnOnce() -> Result<(), ApiError>,
    {
        let mut phase = AttemptPhase::Discovery;
        let mut certainty = intent.previous_certainty;
        let result=tokio::time::timeout(Duration::from_secs(60),async {
            self.ensure().await?;
            if intent.ordinal>1 || intent.previous_certainty==AdmissionCertainty::Unconfirmed {
                phase=AttemptPhase::Reconcile;
                if self.evidence(intent).await? {return Ok(true)}
            }
            self.target(&intent.session_id).await?;
            before_submit()?;
            phase=AttemptPhase::Submit;certainty=AdmissionCertainty::Unconfirmed;
            let (status,value)=self.request(Method::POST,&format!("/api/session/{}/prompt",intent.session_id),Some(json!({"id":intent.message_id,"text":intent.prompt,"delivery":intent.delivery}))).await?;
            if status.is_success() {
                if admission_matches(&value["data"],intent,true){return Ok(true)}
                return Err(unsupported())
            }
            // Only an explicit current request rejection establishes non-admission.
            if status==StatusCode::NOT_FOUND && value["_tag"]=="SessionNotFoundError" {certainty=AdmissionCertainty::NotAdmitted;return Err(missing())}
            if status==StatusCode::BAD_REQUEST {certainty=AdmissionCertainty::NotAdmitted;return Err(ApiError::invalid())}
            Err(unavailable())
        }).await;
        match result {
            Ok(Ok(true)) => admitted(phase),
            Ok(Ok(false)) => failure(phase, certainty, unavailable()),
            Ok(Err(error)) => failure(phase, certainty, error),
            Err(_) => failure(phase, certainty, unavailable()),
        }
    }
}
fn admission_matches(value: &Value, intent: &AttemptIntent, inbox: bool) -> bool {
    value["id"].as_str() == Some(&intent.message_id)
        && value["type"] == "user"
        && (!inbox
            || (value["sessionID"].as_str() == Some(&intent.session_id)
                && value["time"]["created"].is_number()
                && value["payload"]["text"].is_string()
                && matches!(value["delivery"].as_str(), Some("steer" | "queue"))))
}
pub fn failure(
    phase: AttemptPhase,
    certainty: AdmissionCertainty,
    error: ApiError,
) -> AttemptResult {
    let outcome = if matches!(error.code.as_str(), "missing_session" | "invalid_input") {
        AttemptOutcome::Definitive
    } else {
        AttemptOutcome::Transient
    };
    AttemptResult {
        finished_at: Utc::now(),
        phase,
        outcome,
        certainty,
        error: Some(error),
    }
}
fn admitted(phase: AttemptPhase) -> AttemptResult {
    AttemptResult {
        finished_at: Utc::now(),
        phase,
        outcome: AttemptOutcome::Admitted,
        certainty: AdmissionCertainty::Admitted,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::{Path, State},
        http::StatusCode,
        routing::{get, post},
    };
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    #[test]
    fn bootstrap_fixture_process() {
        let Some(dir) = std::env::var_os("SCHEDULER_BOOTSTRAP_FIXTURE") else {
            return;
        };
        let dir = PathBuf::from(dir);
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(dir.join("calls"))
            .unwrap();
        writeln!(file, "{}", std::process::id()).unwrap();
        if std::env::var_os("SCHEDULER_FIXTURE_FAIL").is_some() {
            eprintln!("fixture-secret-must-not-escape");
            std::process::exit(17);
        }
        if std::env::var_os("SCHEDULER_FIXTURE_OVERFLOW").is_some() {
            println!("{}", "x".repeat(70 * 1024));
        }
        if let Some(record) = std::env::var_os("SCHEDULER_FIXTURE_RECORD") {
            std::fs::write(dir.join("service.json"), record.to_str().unwrap()).unwrap();
        }
        if std::env::var_os("SCHEDULER_FIXTURE_HANG").is_some() {
            println!("fixture-secret-must-not-escape");
            std::thread::sleep(Duration::from_secs(120));
        }
    }

    fn helper(
        dir: &std::path::Path,
        record: Option<&Value>,
        hang: bool,
    ) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "adapter::tests::bootstrap_fixture_process",
                "--nocapture",
            ])
            .env("SCHEDULER_BOOTSTRAP_FIXTURE", dir);
        if let Some(record) = record {
            command.env("SCHEDULER_FIXTURE_RECORD", record.to_string());
        }
        if hang {
            command.env("SCHEDULER_FIXTURE_HANG", "1");
        }
        command
    }

    #[tokio::test]
    async fn bootstrap_only_for_absent_or_verified_dead_service() {
        // Catches helpers replacing live/unhealthy/auth-failing/unsupported services, or
        // failure to rediscover the newly written registration after a single bootstrap.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().route(
            "/api/info",
            get(|| async {
                Json(json!({"version":"2.2.5","pid":42,"urls":[],"paths":{"tmp":"fixture"}}))
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let record = json!({"url":url,"pid":42,"version":"2.2.5"});
        for dead in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            if dead {
                std::fs::write(
                    dir.path().join("service.json"),
                    r#"{"url":"http://127.0.0.1:1","pid":42}"#,
                )
                .unwrap();
            }
            let mut adapter = Adapter::from_record(dir.path().join("service.json")).unwrap();
            assert!(adapter.discover().await.is_err()); // observation never starts anything
            assert!(!dir.path().join("calls").exists());
            let info = adapter
                .ensure_with(
                    helper(dir.path(), Some(&record), false),
                    Duration::from_secs(15),
                    |_| true,
                )
                .await
                .unwrap();
            assert_eq!(info.version, "2.2.5");
            assert_eq!(
                std::fs::read_to_string(dir.path().join("calls"))
                    .unwrap()
                    .lines()
                    .count(),
                1
            );
            adapter
                .ensure_with(
                    helper(dir.path(), Some(&record), false),
                    Duration::from_secs(15),
                    |_| true,
                )
                .await
                .unwrap();
            assert_eq!(
                std::fs::read_to_string(dir.path().join("calls"))
                    .unwrap()
                    .lines()
                    .count(),
                1
            );
        }
        for (status, body, expected) in [
            (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({}),
                "opencode_unavailable",
            ),
            (
                StatusCode::UNAUTHORIZED,
                json!({}),
                "authentication_failure",
            ),
            (
                StatusCode::OK,
                json!({"version":"3.0.0","pid":42,"urls":[],"paths":{"tmp":"fixture"}}),
                "unsupported_service",
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let app = Router::new().route(
                "/api/info",
                get(move || {
                    let body = body.clone();
                    async move { (status, Json(body)) }
                }),
            );
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join("service.json"),
                json!({"url":url,"pid":42}).to_string(),
            )
            .unwrap();
            let mut adapter = Adapter::from_record(dir.path().join("service.json")).unwrap();
            let error = adapter
                .ensure_with(
                    helper(dir.path(), Some(&record), false),
                    Duration::from_secs(15),
                    |_| true,
                )
                .await
                .err()
                .unwrap();
            assert_eq!(error.code, expected);
            assert!(!dir.path().join("calls").exists());
            server.abort();
        }
        // A live PID or inconclusive ownership must also block bootstrap when HTTP is unreachable.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("service.json"),
            json!({"url":"http://127.0.0.1:1","pid":std::process::id()}).to_string(),
        )
        .unwrap();
        let mut adapter = Adapter::from_record(dir.path().join("service.json")).unwrap();
        assert!(
            adapter
                .ensure_with(
                    helper(dir.path(), Some(&record), false),
                    Duration::from_secs(15),
                    |_| false
                )
                .await
                .is_err()
        );
        assert!(!dir.path().join("calls").exists());
        // An unreadable/malformed registration is not evidence of a dead process.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("service.json"), "invalid").unwrap();
        let mut adapter = Adapter::from_record(dir.path().join("service.json")).unwrap();
        assert!(
            adapter
                .ensure_with(
                    helper(dir.path(), Some(&record), false),
                    Duration::from_secs(15),
                    |_| true
                )
                .await
                .is_err()
        );
        assert!(!dir.path().join("calls").exists());
        server.abort();
    }

    #[tokio::test]
    async fn bootstrap_helper_is_bounded_secret_free_and_reaped_on_cancellation() {
        // Catches an unbounded helper, leaked output or a child surviving cancellation.
        let dir = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let error = crate::bootstrap::run(helper(dir.path(), None, true), Duration::from_secs(15))
            .await
            .unwrap_err();
        assert!(started.elapsed() >= Duration::from_secs(14));
        assert!(started.elapsed() < Duration::from_secs(18));
        assert_eq!(
            error.to_string(),
            "opencode_unavailable: OpenCode service is unavailable."
        );
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        assert_eq!(calls.lines().count(), 1);
        let pid = calls.trim().parse::<u32>().unwrap();
        #[cfg(windows)]
        assert!(crate::bootstrap::verified_dead(pid));
        #[cfg(not(windows))]
        let _ = pid;
        std::fs::remove_file(dir.path().join("calls")).unwrap();
        let command = helper(dir.path(), None, true);
        let task =
            tokio::spawn(
                async move { crate::bootstrap::run(command, Duration::from_secs(15)).await },
            );
        let pid = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(pid) = std::fs::read_to_string(dir.path().join("calls"))
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok())
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        let _ = task.await;
        #[cfg(windows)]
        tokio::time::timeout(Duration::from_secs(3), async {
            while !crate::bootstrap::verified_dead(pid) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        #[cfg(not(windows))]
        let _ = pid;
    }

    #[tokio::test]
    async fn bootstrap_helper_failure_and_output_limits_are_private() {
        for mode in ["SCHEDULER_FIXTURE_FAIL", "SCHEDULER_FIXTURE_OVERFLOW"] {
            let dir = tempfile::tempdir().unwrap();
            let mut command = helper(dir.path(), None, false);
            command.env(mode, "1");
            let error = crate::bootstrap::run(command, Duration::from_secs(15))
                .await
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                "opencode_unavailable: OpenCode service is unavailable."
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join("calls"))
                    .unwrap()
                    .lines()
                    .count(),
                1
            );
        }
    }

    #[tokio::test]
    async fn stable_id_lost_response_reconciles_without_second_post_and_callback_failure_never_sends()
     {
        let admitted_item = Arc::new(Mutex::new(None::<Value>));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app=Router::new()
            .route("/api/info",get(||async {Json(json!({"version":"2.2.5","pid":42,"urls":[],"paths":{"tmp":"fixture"}}))}))
            .route("/api/session/{id}",get(|Path(id):Path<String>|async move {Json(json!({"data":{"id":id}}))}))
            .route("/api/session/{id}/inbox",get(|State(state):State<Arc<Mutex<Option<Value>>>>|async move {Json(json!({"data":state.lock().unwrap().iter().cloned().collect::<Vec<_>>()}))}))
            .route("/api/session/{id}/prompt",post(|State(state):State<Arc<Mutex<Option<Value>>>>,Path(id):Path<String>,Json(body):Json<Value>|async move {
                let mut item=state.lock().unwrap();assert!(item.is_none(),"duplicate prompt POST");
                assert_eq!(body.as_object().unwrap().len(),3);
                *item=Some(json!({"id":body["id"],"sessionID":id,"type":"user","time":{"created":1},"payload":{"text":body["text"]},"delivery":body["delivery"]}));
                (StatusCode::SERVICE_UNAVAILABLE,Json(json!({"error":"response lost after admission"})))
            })).with_state(admitted_item.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("service.json");
        std::fs::write(
            &record,
            serde_json::to_vec(&json!({"url":url,"pid":42,"version":"2.2.5"})).unwrap(),
        )
        .unwrap();
        let mut adapter = Adapter::from_record(record).unwrap();
        let mut intent = AttemptIntent {
            schedule_id: Uuid::new_v4().to_string(),
            ordinal: 1,
            session_id: "ses_fixture".into(),
            message_id: format!("msg_{}", Uuid::new_v4().simple()),
            prompt: "original payload".into(),
            delivery: DeliveryMode::Queue,
            started_at: Utc::now(),
            due_at: Utc::now(),
            previous_certainty: AdmissionCertainty::NotAdmitted,
        };
        let blocked = adapter.deliver(&intent, || Err(ApiError::storage())).await;
        assert_eq!(blocked.outcome, AttemptOutcome::Transient);
        assert!(admitted_item.lock().unwrap().is_none());
        let uncertain = adapter.deliver(&intent, || Ok(())).await;
        assert_eq!(uncertain.certainty, AdmissionCertainty::Unconfirmed);
        assert_eq!(uncertain.phase, AttemptPhase::Submit);
        let recovered = adapter.reconcile(&intent).await;
        assert_eq!(recovered.outcome, AttemptOutcome::Admitted);
        assert_eq!(recovered.phase, AttemptPhase::Reconcile);
        intent.ordinal = 2;
        intent.previous_certainty = AdmissionCertainty::Unconfirmed;
        let retry = adapter
            .deliver(&intent, || {
                panic!("existing admission must not grant POST permission")
            })
            .await;
        assert_eq!(retry.outcome, AttemptOutcome::Admitted);
        assert_eq!(
            admitted_item.lock().unwrap().as_ref().unwrap()["id"],
            intent.message_id
        );
        server.abort();
    }
}

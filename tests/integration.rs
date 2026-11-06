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

// These process-heavy chains share the host's bounded PowerShell startup budget.
// Keep chains sequential; the singleton test still launches concurrent CLI starts itself.
static CLI_FIXTURES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[test]
fn malformed_time_is_rejected_before_private_directory_or_startup() {
    let home = TempDir::new().unwrap();
    let root = home.path().join("not-a-directory");
    std::fs::write(&root, "do not touch").unwrap();
    for json in [false, true] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_opencode-scheduler"));
        cmd.env("OPENCODE_SCHEDULER_HOME", &root).args([
            "schedule",
            "create",
            "--session",
            "ses_fixture",
            "--at",
            "5:0",
            "--prompt",
            "fixture",
        ]);
        if json {
            cmd.arg("--json");
        }
        let out = cmd.output().unwrap();
        assert!(!out.status.success());
        let text = if json {
            String::from_utf8(out.stdout).unwrap()
        } else {
            String::from_utf8(out.stderr).unwrap()
        };
        assert!(text.contains("2099-11-06T05:00:00+01:00"), "{text}");
        assert!(!text.contains("storage_failure"), "{text}");
        assert_eq!(std::fs::read_to_string(&root).unwrap(), "do not touch");
    }
}

#[test]
fn help_teaches_every_visible_command_and_time_contract() {
    for (args, phrases) in [
        (vec![], vec!["Manage", "Inspect", "Install", "Examples:"]),
        (
            vec!["schedule"],
            vec!["Create", "List", "Cancel", "delivery"],
        ),
        (
            vec!["schedule", "create"],
            vec![
                "ALL",
                "next strictly future",
                "YYYY-MM-DD",
                "RFC 3339",
                "daemon host",
                "offset",
                "identical",
                "admission",
                "completion",
                "Examples:",
            ],
        ),
        (vec!["schedule", "list"], vec!["1-100", "cursor", "prompt"]),
        (vec!["schedule", "show"], vec!["UUID", "prompt"]),
        (
            vec!["schedule", "cancel"],
            vec!["UUID", "retract", "uncertain"],
        ),
        (
            vec!["schedule", "log"],
            vec!["UUID", "six", "10/20/40/80/160", "1-100"],
        ),
        (vec!["daemon"], vec!["Start", "Stop", "Inspect"]),
        (vec!["daemon", "start"], vec!["pending", "startup"]),
        (vec!["daemon", "stop"], vec!["preserv", "OpenCode"]),
        (vec!["daemon", "status"], vec!["read-only", "start"]),
        (vec!["status"], vec!["read-only", "start"]),
        (vec!["install"], vec!["PATH", "preserv", "no-path"]),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_opencode-scheduler"))
            .args(&args)
            .arg("--help")
            .output()
            .unwrap();
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        for phrase in phrases {
            assert!(text.contains(phrase), "{args:?} missing {phrase}: {text}");
        }
    }
}

#[test]
fn parser_errors_offer_help_and_valid_delivery_choices() {
    for (args, hint) in [
        (vec!["schedule", "create"], "--help"),
        (
            vec![
                "schedule",
                "create",
                "--session",
                "ses_fixture",
                "--at",
                "5:00",
                "--prompt",
                "fixture",
                "--delivery",
                "invalid",
            ],
            "steer, queue",
        ),
        (vec!["schedule", "unknown"], "--help"),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_opencode-scheduler"))
            .args(args)
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8(out.stderr).unwrap().contains(hint));
    }
}

struct DaemonCleanup(std::path::PathBuf);
impl Drop for DaemonCleanup {
    fn drop(&mut self) {
        if self.0.join("endpoint.json").exists() {
            let _ = Command::new(env!("CARGO_BIN_EXE_opencode-scheduler"))
                .env("OPENCODE_SCHEDULER_HOME", &self.0)
                .args(["daemon", "stop", "--json"])
                .output();
        }
    }
}

fn human(home: &TempDir, state: &TempDir, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_opencode-scheduler"))
        .env("OPENCODE_SCHEDULER_HOME", home.path().join("private"))
        .env("XDG_STATE_HOME", state.path())
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        !text.contains(['{', '}']),
        "human output must not be JSON: {text}"
    );
    text
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn human_output_is_readable_for_all_operations() {
    let _fixture = CLI_FIXTURES.lock().await;
    // Catches accidentally sending the automation envelope to human stdout.
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let _cleanup = DaemonCleanup(home.path().join("private"));
    let server = fixture(&state, Arc::new(Mutex::new(Vec::new()))).await;
    let status = human(&home, &state, &["status"]);
    assert!(status.contains("Daemon: not running"));
    assert!(!home.path().join("private").exists());
    let installed = human(&home, &state, &["install", "--no-path"]);
    assert!(installed.contains("Binary: installed"));
    let at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let rid = uuid::Uuid::new_v4().to_string();
    let args = [
        "schedule",
        "create",
        "--session",
        "ses_a",
        "--at",
        &at,
        "--prompt",
        "fixture",
        "--request-id",
        &rid,
    ];
    let created = human(&home, &state, &args);
    let value = cli(&home, &state, &args);
    let id = value["result"]["data"]["schedules"][0]["id"]
        .as_str()
        .unwrap();
    for text in [
        created,
        human(&home, &state, &["schedule", "list"]),
        human(&home, &state, &["schedule", "show", id]),
    ] {
        assert!(text.contains(id));
        assert!(text.contains("ses_a"));
        assert!(text.contains("pending"));
        assert!(
            text.contains(
                value["result"]["data"]["schedules"][0]["time"]["display"]
                    .as_str()
                    .unwrap()
            )
        );
        assert!(text.contains("0/6"));
    }
    assert!(human(&home, &state, &["schedule", "cancel", id]).contains("cancelled"));
    let log = human(&home, &state, &["schedule", "log", id]);
    assert!(log.contains(id) && log.contains("cancelled"));
    human(&home, &state, &["daemon", "stop"]);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clock_creation_echoes_full_local_instant_in_isolated_fixture() {
    let _fixture = CLI_FIXTURES.lock().await;
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let _cleanup = DaemonCleanup(home.path().join("private"));
    let posts = Arc::new(Mutex::new(Vec::new()));
    let server = fixture(&state, posts.clone()).await;
    let clock = (chrono::Local::now() + chrono::Duration::hours(1))
        .format("%H:%M:%S")
        .to_string();
    let request = uuid::Uuid::new_v4().to_string();
    let args = [
        "schedule",
        "create",
        "--session",
        "ses_a",
        "--at",
        &clock,
        "--prompt",
        "fixture only",
        "--request-id",
        &request,
    ];
    let created = cli(&home, &state, &args);
    let summary = &created["result"]["data"]["schedules"][0];
    let time = &summary["time"];
    assert_eq!(time["original"], clock);
    assert!(time["timezone"].is_string());
    let display = time["display"].as_str().unwrap();
    assert!(display.len() > 25, "{display}");
    let due: chrono::DateTime<chrono::Utc> = time["due_at"].as_str().unwrap().parse().unwrap();
    assert!(due > chrono::Utc::now());
    assert_eq!(cli(&home, &state, &args), created);
    human(
        &home,
        &state,
        &["schedule", "cancel", summary["id"].as_str().unwrap()],
    );
    assert!(posts.lock().unwrap().is_empty());
    human(&home, &state, &["daemon", "stop"]);
    server.abort();
}

#[test]
fn json_automation_contract_unchanged() {
    use opencode_scheduler::types::OperationResult;
    // Independent wire fixtures pin every existing result shape, not human renderer wording.
    let schedule = json!({
        "id":"2b8e5cb4-b18d-4a36-827d-cc941d8eb3a1","request_id":"df99554f-a8d4-4cf9-a20c-cc78a53415dd",
        "created_at":"2026-11-06T00:00:00Z","time":{"due_at":"2026-11-07T01:30:00Z","original":"2026-11-07 02:30","display":"2026-11-07 02:30:00 +01:00","timezone":"Europe/Stockholm"},
        "target":{"session_id":"ses_fixture","title":"Fixture"},"prompt":"fixture","delivery":"queue",
        "message_id":"msg_f68959e33dfa45c3a397b77dd267a9aa","state":"pending","admitted_at":null,
        "attempts_used":0,"next_attempt_at":"2026-11-07T01:30:00Z","certainty":"not_admitted","last_error":null,"lateness_ms":null
    });
    let mut summary = schedule.clone();
    summary.as_object_mut().unwrap().remove("prompt");
    for result in [
        json!({"kind":"created","data":{"request_id":"df99554f-a8d4-4cf9-a20c-cc78a53415dd","schedules":[summary]}}),
        json!({"kind":"schedule","data":schedule}),
        json!({"kind":"page","data":{"items":[summary],"next_cursor":null}}),
        json!({"kind":"cancel","data":{"schedule":schedule,"disposition":"cancelled"}}),
        json!({"kind":"log","data":{"items":[{"seq":1,"schedule_id":"2b8e5cb4-b18d-4a36-827d-cc941d8eb3a1","ordinal":1,"started_at":"2026-11-07T01:30:00Z","finished_at":null,"phase":"discovery","outcome":"started","certainty":"not_admitted","error":null,"next_attempt_at":null}],"next_cursor":null}}),
        json!({"kind":"stopping","data":{"accepted":true}}),
    ] {
        let parsed: OperationResult = serde_json::from_value(result.clone()).unwrap();
        assert_eq!(
            json!({"ok":true,"result":parsed}),
            json!({"ok":true,"result":result})
        );
    }
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    assert_eq!(
        cli(&home, &state, &["status"]),
        json!({"ok":true,"result":{"kind":"status","data":{
            "installed":{"binary":false,"plugin":false},
            "daemon":{"reachable":false,"protocol_compatible":false},
            "opencode":{"state":"unknown","service_version":null},
            "schedules":{"pending":null,"blocked":null,"failed":null,"admission_unconfirmed":null}
        }}})
    );
    let out = Command::new(env!("CARGO_BIN_EXE_opencode-scheduler"))
        .env("OPENCODE_SCHEDULER_HOME", home.path().join("private"))
        .args([
            "schedule",
            "create",
            "--session",
            "ses_a",
            "--at",
            "bad",
            "--prompt",
            "x",
            "--request-id",
            "bad",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 2);
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"].as_object().unwrap().len(), 2);
    assert_eq!(value["error"]["code"], "invalid_time");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Hint:")
    );
    assert!(!home.path().join("private").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn historical_errors_are_canonical_in_every_outward_envelope_without_rewriting_data() {
    use opencode_scheduler::{runtime::Paths, store::Store, time::resolve_at, types::*};
    let _fixture = CLI_FIXTURES.lock().await;
    for old_message in [
        "OpenCode service is unavailable.",
        "synthetic SQL private/error/path",
    ] {
        let home = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        let paths = Paths {
            root: home.path().join("private"),
        };
        let _cleanup = DaemonCleanup(paths.root.clone());
        paths.prepare().unwrap();
        let input = CreateInput {
            request_id: uuid::Uuid::new_v4().to_string(), target: TargetSelector::Id("ses_fixture".into()),
            at: "2099-11-06T05:00:00+01:00".into(),
            prompt: r#"literal {"last_error":{"code":"opencode_unavailable","message":"do not normalize literal"}}"#.into(),
            delivery: DeliveryMode::Queue,
        };
        let mut store = Store::open(&paths.file("schedules.sqlite3")).unwrap();
        let created = store
            .create(
                ValidatedCreate {
                    input: input.clone(),
                    time: resolve_at(&input.at, &chrono::Utc, None, chrono::Utc::now()).unwrap(),
                    recipients: vec![ResolvedRecipient {
                        target: Target {
                            session_id: "ses_fixture".into(),
                            title: None,
                        },
                        message_id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
                    }],
                },
                chrono::Utc::now(),
            )
            .unwrap();
        let id = created.schedules[0].id.clone();
        drop(store);
        // Seed a preserved pre-upgrade/adversarial record through raw fixture SQL, not product writes.
        let conn = rusqlite::Connection::open(paths.file("schedules.sqlite3")).unwrap();
        let mut schedule: Value = serde_json::from_str(
            &conn
                .query_row("SELECT data_json FROM schedules WHERE id=?1", [&id], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
        )
        .unwrap();
        let error = json!({"code":"opencode_unavailable","message":old_message});
        schedule["last_error"] = error.clone();
        conn.execute(
            "UPDATE schedules SET data_json=?1 WHERE id=?2",
            rusqlite::params![schedule.to_string(), id],
        )
        .unwrap();
        let mut cached = serde_json::to_value(&created).unwrap();
        cached["schedules"][0]["last_error"] = error.clone();
        conn.execute(
            "UPDATE create_requests SET result_json=?1",
            [cached.to_string()],
        )
        .unwrap();
        let event = json!({"seq":1,"schedule_id":id,"ordinal":1,"started_at":"2026-11-06T00:00:00Z","finished_at":"2026-11-06T00:00:01Z","phase":"discovery","outcome":"transient","certainty":"not_admitted","error":error,"next_attempt_at":null});
        conn.execute(
            "INSERT INTO delivery_log(schedule_id,data_json) VALUES (?1,?2)",
            rusqlite::params![id, event.to_string()],
        )
        .unwrap();
        drop(conn);
        human(&home, &state, &["daemon", "start"]);
        let endpoint: Value =
            serde_json::from_slice(&std::fs::read(paths.file("endpoint.json")).unwrap()).unwrap();
        for args in [
            vec!["schedule", "list"],
            vec!["schedule", "show", &id],
            vec!["schedule", "log", &id],
            vec![
                "schedule",
                "create",
                "--session",
                "ses_fixture",
                "--at",
                &input.at,
                "--prompt",
                &input.prompt,
                "--request-id",
                &input.request_id,
                "--delivery",
                "queue",
            ],
            vec!["schedule", "cancel", &id],
        ] {
            let value = cli(&home, &state, &args);
            let data = &value["result"]["data"];
            let (operation, pointer) = match args[1] {
                "list" => (
                    Operation::ScheduleList(PageInput::default()),
                    "/result/data/items/0/last_error",
                ),
                "log" => (
                    Operation::ScheduleLog(LogInput {
                        id: id.clone(),
                        limit: 50,
                        cursor: None,
                    }),
                    "/result/data/items/0/error",
                ),
                "create" => (
                    Operation::ScheduleCreate(input.clone()),
                    "/result/data/schedules/0/last_error",
                ),
                "cancel" => (
                    Operation::ScheduleCancel(IdInput { id: id.clone() }),
                    "/result/data/schedule/last_error",
                ),
                _ => (
                    Operation::ScheduleShow(IdInput { id: id.clone() }),
                    "/result/data/last_error",
                ),
            };
            let nested = value.pointer(pointer).unwrap();
            assert_eq!(nested["code"], "opencode_unavailable");
            assert!(
                nested["message"].as_str().unwrap().contains("Hint:"),
                "{args:?}: {nested}"
            );
            assert!(
                !nested["message"]
                    .as_str()
                    .unwrap()
                    .contains("private/error/path")
            );
            // Inspect the daemon's wire response independently of the CLI's defensive normalization.
            let direct: Value = reqwest::Client::new()
                .post(format!("{}/v1/rpc", endpoint["base_url"].as_str().unwrap()))
                .bearer_auth(endpoint["token"].as_str().unwrap())
                .header(
                    "X-Scheduler-Instance",
                    endpoint["instance_id"].as_str().unwrap(),
                )
                .json(&operation)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(direct.pointer(pointer).unwrap(), nested);
            let human = opencode_scheduler::output::render(&value).unwrap();
            assert!(human.contains("Hint:"));
            assert!(!human.contains("private/error/path"));
            if args[1] == "show" {
                assert_eq!(data["prompt"], input.prompt);
            }
            if args[1] == "cancel" {
                assert_eq!(data["schedule"]["prompt"], input.prompt);
            }
        }
        let conn = rusqlite::Connection::open(paths.file("schedules.sqlite3")).unwrap();
        for sql in [
            "SELECT data_json FROM schedules",
            "SELECT data_json FROM delivery_log WHERE seq=1",
            "SELECT result_json FROM create_requests",
        ] {
            let raw: String = conn.query_row(sql, [], |r| r.get(0)).unwrap();
            assert!(
                raw.contains(old_message),
                "reading/returning results must not rewrite persisted wording"
            );
        }
        drop(conn);
        human(&home, &state, &["daemon", "stop"]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn install_preserves_store_and_handles_running_binary() {
    let _fixture = CLI_FIXTURES.lock().await;
    // Catches in-place copying over a loaded executable, lost metadata/store and daemon downtime.
    let home = tempfile::Builder::new()
        .prefix("scheduler install spaces ")
        .tempdir()
        .unwrap();
    let state = TempDir::new().unwrap();
    let _cleanup = DaemonCleanup(home.path().join("private"));
    let server = fixture(&state, Arc::new(Mutex::new(Vec::new()))).await;
    cli(&home, &state, &["install", "--no-path"]);
    let root = home.path().join("private");
    let mut metadata: Value =
        serde_json::from_slice(&std::fs::read(root.join("install.json")).unwrap()).unwrap();
    metadata["plugin"] = json!({"managed":"fixture"});
    metadata["future_field"] = json!(["preserve"]);
    std::fs::write(
        root.join("install.json"),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();
    let at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
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
            "preserve",
        ],
    );
    let id = created["result"]["data"]["schedules"][0]["id"]
        .as_str()
        .unwrap();
    let installed = root.join(if cfg!(windows) {
        "app/bin/opencode-scheduler.exe"
    } else {
        "app/bin/opencode-scheduler"
    });
    let out = Command::new(&installed)
        .env("OPENCODE_SCHEDULER_HOME", &root)
        .env("XDG_STATE_HOME", state.path())
        .args(["install", "--no-path", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        cli(&home, &state, &["status"])["result"]["data"]["daemon"]["reachable"],
        true
    );
    assert_eq!(
        cli(&home, &state, &["schedule", "show", id])["result"]["data"]["state"],
        "pending"
    );
    let after: Value =
        serde_json::from_slice(&std::fs::read(root.join("install.json")).unwrap()).unwrap();
    assert_eq!(after["plugin"], json!({"managed":"fixture"}));
    assert_eq!(after["future_field"], json!(["preserve"]));
    cli(&home, &state, &["daemon", "stop"]);
    server.abort();
}

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
        "{args:?}: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bare_powershell_create_preserves_literal_prompt_arguments() {
    let _fixture = CLI_FIXTURES.lock().await;
    // Catches command-interpreter expansion of literal prompt text on the installed path.
    let home = tempfile::Builder::new()
        .prefix("scheduler literal spaces ")
        .tempdir()
        .unwrap();
    let state = TempDir::new().unwrap();
    let root = home.path().join("private");
    let _cleanup = DaemonCleanup(root.clone());
    let server = fixture(&state, Arc::new(Mutex::new(Vec::new()))).await;
    cli(&home, &state, &["install", "--no-path"]);
    let paths = opencode_scheduler::runtime::Paths { root: root.clone() };
    assert!(paths.binary().exists());
    assert!(!root.join("bin").exists());
    let mut isolated_path = paths.command_dir().into_os_string();
    isolated_path.push(";");
    isolated_path.push(std::env::var_os("PATH").unwrap_or_default());
    let prompt = "Keep %SCHEDULER_LITERAL_PROBE% and %MISSING_LITERAL_PROBE% literal; & | < > ^ ! ( ) $value `tick 'single' — åäö 日本語 with spaces";
    let at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command",
            "if (@(Get-Command opencode-scheduler -CommandType Application)[0].Source -ne $env:SCHEDULER_TEST_BINARY) { throw 'Unexpected installed command' }; opencode-scheduler schedule create --session ses_a --at $env:SCHEDULER_TEST_AT --prompt $env:SCHEDULER_TEST_PROMPT --json; exit $LASTEXITCODE"])
        .current_dir(home.path())
        .env("OPENCODE_SCHEDULER_HOME", &root)
        .env("XDG_STATE_HOME", state.path())
        .env("PATH", isolated_path)
        .env("SCHEDULER_LITERAL_PROBE", "expanded-by-cmd")
        .env_remove("MISSING_LITERAL_PROBE")
        .env("SCHEDULER_TEST_AT", at)
        .env("SCHEDULER_TEST_BINARY", paths.binary())
        .env("SCHEDULER_TEST_PROMPT", prompt)
        .output().unwrap();
    assert!(
        out.status.success(),
        "{} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let created: Value = serde_json::from_slice(&out.stdout).unwrap();
    let id = created["result"]["data"]["schedules"][0]["id"]
        .as_str()
        .unwrap();
    let stored = cli(&home, &state, &["schedule", "show", id]);
    assert_eq!(stored["result"]["data"]["prompt"], prompt);
    cli(&home, &state, &["daemon", "stop"]);
    server.abort();
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
    let _fixture = CLI_FIXTURES.lock().await;
    // Catches partial title fanout, transport-dependent duplicate creation, non-detached launch,
    // missing auth, lost stable message identity and CLI/store bypasses through the real binary.
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let _cleanup = DaemonCleanup(home.path().join("private"));
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

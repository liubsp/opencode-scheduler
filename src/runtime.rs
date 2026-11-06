use crate::{
    adapter::{bounded_json, local_url},
    types::*,
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
use uuid::Uuid;

#[derive(Clone)]
pub struct Paths {
    pub root: PathBuf,
}
impl Paths {
    pub fn discover() -> Result<Self, ApiError> {
        let root = if let Some(root) = std::env::var_os("OPENCODE_SCHEDULER_HOME") {
            PathBuf::from(root)
        } else {
            #[cfg(windows)]
            let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
            #[cfg(not(windows))]
            let base = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share"))
                });
            base.ok_or_else(ApiError::storage)?
                .join("opencode-scheduler")
        };
        if !root.is_absolute() {
            return Err(ApiError::invalid());
        }
        Ok(Self { root })
    }
    pub fn file(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
    pub fn command_dir(&self) -> PathBuf {
        self.file("app/bin")
    }
    pub fn binary(&self) -> PathBuf {
        self.command_dir().join(if cfg!(windows) {
            "opencode-scheduler.exe"
        } else {
            "opencode-scheduler"
        })
    }
    pub fn prepare(&self) -> Result<(), ApiError> {
        let failure = || ApiError::coded("private_directory_failure");
        if self.root.exists() {
            let meta = fs::symlink_metadata(&self.root).map_err(|_| failure())?;
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Err(failure());
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // Use the OS ACL implementation, not a second home-grown Windows security model.
            // Supply the ACL at directory creation: a concurrent validator must never observe
            // an intermediate inherited/public ACL between mkdir and Set-Acl.
            let script = r#"$ErrorActionPreference='Stop'; $p=$env:SCHEDULER_PRIVATE_ROOT; $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User; if(!(Test-Path -LiteralPath $p)){$a=New-Object Security.AccessControl.DirectorySecurity; $a.SetOwner($sid); $a.SetAccessRuleProtection($true,$false); $r=New-Object Security.AccessControl.FileSystemAccessRule($sid,'FullControl','ContainerInherit,ObjectInherit','None','Allow'); $a.AddAccessRule($r); $d=New-Object IO.DirectoryInfo($p); $d.Create($a)}; $i=Get-Item -LiteralPath $p; if($i.Attributes -band [IO.FileAttributes]::ReparsePoint){exit 2}; $a=Get-Acl -LiteralPath $p; if($a.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $sid.Value){exit 3}; foreach($r in $a.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier])){if($r.AccessControlType -eq 'Allow' -and $r.IdentityReference.Value -ne $sid.Value){exit 4}}"#;
            let mut child = Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", script])
                // PowerShell 7 callers export incompatible module search paths to
                // native children. Let Windows PowerShell resolve its own ACL module.
                .env_remove("PSModulePath")
                .env("SCHEDULER_PRIVATE_ROOT", &self.root)
                .creation_flags(0x08000000)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|_| failure())?;
            let status = wait_private_helper(&mut child, PRIVATE_HELPER_TIMEOUT)?;
            if !status.success() {
                return Err(failure());
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, MetadataExt};
            if !self.root.exists() {
                let mut builder = fs::DirBuilder::new();
                builder.recursive(true).mode(0o700);
                builder.create(&self.root).map_err(|_| failure())?;
            }
            let meta = fs::metadata(&self.root).map_err(|_| failure())?;
            // Compare ownership against the current process's own newly-created file.
            let probe = self.file(&format!(".owner-{}", Uuid::new_v4()));
            let file = private_file(&probe).map_err(|_| failure())?;
            let uid = file.metadata().map_err(|_| failure())?.uid();
            drop(file);
            fs::remove_file(probe).map_err(|_| failure())?;
            if meta.uid() != uid || meta.mode() & 0o077 != 0 {
                return Err(failure());
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
// Cold PowerShell startup can exceed five seconds under concurrent process-heavy work.
// Keep security validation mandatory, bounded and separately observable from daemon startup.
const PRIVATE_HELPER_TIMEOUT: Duration = Duration::from_secs(15);

#[cfg(windows)]
fn wait_private_helper(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<std::process::ExitStatus, ApiError> {
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(20))
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ApiError::coded(if result.is_err() {
                    "private_directory_failure"
                } else {
                    "private_directory_timeout"
                }));
            }
        }
    }
}
pub fn read_bounded(path: &Path, max: usize) -> Result<Vec<u8>, ApiError> {
    let mut data = Vec::new();
    File::open(path)
        .map_err(|_| ApiError::storage())?
        .take(max as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|_| ApiError::storage())?;
    if data.len() > max {
        return Err(ApiError::new(
            "resource_limit",
            "Scheduler resource limit exceeded.",
        ));
    }
    Ok(data)
}
pub fn private_file(path: &Path) -> Result<File, ApiError> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|_| ApiError::storage())
}
pub fn write_json(path: &Path, value: &impl Serialize) -> Result<(), ApiError> {
    let tmp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let mut file = private_file(&tmp)?;
    file.write_all(&serde_json::to_vec(value).map_err(|_| ApiError::storage())?)
        .map_err(|_| ApiError::storage())?;
    file.sync_all().map_err(|_| ApiError::storage())?;
    drop(file);
    // Same-directory rename replaces the old file atomically, including on Windows.
    // The lifetime/startup locks serialize descriptor writers.
    fs::rename(tmp, path).map_err(|_| ApiError::storage())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub protocol: u8,
    pub instance_id: String,
    pub base_url: String,
    pub pid: u32,
    pub token: String,
}
impl Endpoint {
    fn validate(&self) -> Result<(), ApiError> {
        if self.protocol != 1 {
            return Err(ApiError::new(
                "unsupported_protocol",
                "Scheduler protocol is unsupported.",
            ));
        }
        validate_uuid(&self.instance_id)?;
        local_url(&self.base_url, true).map_err(scheduler_error)?;
        if !(64..=128).contains(&self.token.len())
            || !self.token.bytes().all(|b| b.is_ascii_hexdigit())
            || self.pid == 0
        {
            return Err(ApiError::invalid());
        }
        Ok(())
    }
}
fn client() -> Result<reqwest::Client, ApiError> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(35))
        .build()
        .map_err(|_| ApiError::coded("daemon_unavailable"))
}
// The shared bounded HTTP reader uses OpenCode error categories. At the scheduler
// boundary, retain limits but identify the correct service without exposing bodies.
fn scheduler_error(error: ApiError) -> ApiError {
    match error.code.as_str() {
        "opencode_unavailable" => ApiError::coded("daemon_unavailable"),
        "unsupported_service" => ApiError::coded("unsupported_protocol"),
        _ => error,
    }
}
pub async fn discover_daemon(paths: &Paths) -> Result<Option<Endpoint>, ApiError> {
    let path = paths.file("endpoint.json");
    if !path.exists() {
        return Ok(None);
    }
    let endpoint: Endpoint = serde_json::from_slice(&read_bounded(&path, 64 * 1024)?)
        .map_err(|_| ApiError::storage())?;
    endpoint.validate()?;
    let response = match client()?
        .get(format!("{}/v1/health", endpoint.base_url))
        .timeout(Duration::from_secs(2))
        .bearer_auth(&endpoint.token)
        .header("X-Scheduler-Instance", &endpoint.instance_id)
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };
    if !response.status().is_success() {
        return Err(ApiError::new(
            "authentication_failure",
            "Service authentication failed.",
        ));
    }
    let value = bounded_json(response, 64 * 1024)
        .await
        .map_err(scheduler_error)?;
    if value["protocol"] != 1
        || value["instance_id"] != endpoint.instance_id
        || !value["binary_version"].is_string()
    {
        return Err(ApiError::new(
            "unsupported_protocol",
            "Scheduler protocol is unsupported.",
        ));
    }
    Ok(Some(endpoint))
}
pub async fn ensure_daemon(paths: &Paths) -> Result<Endpoint, ApiError> {
    paths.prepare()?;
    tokio::time::timeout(
        // The daemon repeats private-directory validation before opening its database.
        Duration::from_secs(30),
        async {
            let startup = private_file(&paths.file("startup.lock"))?;
            loop {
                if startup.try_lock_exclusive().is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            if let Some(endpoint) = discover_daemon(paths).await? {
                return Ok(endpoint);
            }
            let lifetime = private_file(&paths.file("daemon.lock"))?;
            lifetime
                .try_lock_exclusive()
                .map_err(|_| ApiError::coded("daemon_startup_failure"))?;
            drop(lifetime);
            let log = paths.file("diagnostic.log");
            if fs::metadata(&log).is_ok_and(|m| m.len() >= 1024 * 1024) {
                let backup = paths.file("diagnostic.log.1");
                if backup.exists() {
                    fs::remove_file(&backup).map_err(|_| ApiError::storage())?;
                }
                fs::rename(&log, backup).map_err(|_| ApiError::storage())?;
            }
            let exe = if paths.binary().exists() {
                paths.binary()
            } else {
                std::env::current_exe().map_err(|_| ApiError::storage())?
            };
            #[cfg(windows)]
            let mut child = crate::launch::spawn(&exe, &paths.root)
                .map_err(|_| ApiError::coded("daemon_startup_failure"))?;
            #[cfg(not(windows))]
            let mut child = {
                let out = private_file(&log)?;
                let err = private_file(&log)?;
                let mut command = Command::new(exe);
                command
                    .arg("daemon-run")
                    .env("OPENCODE_SCHEDULER_HOME", &paths.root)
                    .current_dir(&paths.root)
                    .stdin(Stdio::null())
                    .stdout(out)
                    .stderr(err);
                #[cfg(unix)]
                {
                    use std::os::unix::process::CommandExt;
                    command.process_group(0);
                }
                command
                    .spawn()
                    .map_err(|_| ApiError::coded("daemon_startup_failure"))?
            };
            loop {
                if let Some(endpoint) = discover_daemon(paths).await? {
                    return Ok(endpoint);
                }
                if child
                    .try_wait()
                    .map_err(|_| ApiError::coded("daemon_startup_failure"))?
                    .is_some()
                {
                    return Err(ApiError::coded("daemon_startup_failure"));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        },
    )
    .await
    .map_err(|_| ApiError::coded("daemon_startup_timeout"))?
}
pub async fn rpc(endpoint: &Endpoint, operation: &Operation) -> Result<Value, ApiError> {
    endpoint.validate()?;
    let response = client()?
        .post(format!("{}/v1/rpc", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .header("X-Scheduler-Instance", &endpoint.instance_id)
        .json(operation)
        .send()
        .await
        .map_err(|_| ApiError::coded("daemon_unavailable"))?;
    let mut value = bounded_json(response, 16 * 1024 * 1024)
        .await
        .map_err(scheduler_error)?;
    // Also protect callers talking to an older daemon with historical error wording.
    if value["ok"] == true {
        let mut result: OperationResult = serde_json::from_value(value["result"].clone())
            .map_err(|_| ApiError::coded("unsupported_protocol"))?;
        result.canonicalize_errors();
        value["result"] = json!(result);
    }
    Ok(value)
}
pub fn installed_plugin_path(paths: &Paths) -> Option<PathBuf> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Config {
        protocol: u8,
        binary: PathBuf,
        data_home: PathBuf,
    }
    let read = |path: &Path| -> Option<Value> {
        let meta = fs::symlink_metadata(path).ok()?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            return None;
        }
        serde_json::from_slice(&read_bounded(path, 64 * 1024).ok()?).ok()
    };
    let metadata = read(&paths.file("product-install.json"))?;
    if metadata["protocol"] != 1 {
        return None;
    }
    let plugin = PathBuf::from(metadata["plugin_path"].as_str()?);
    if !plugin.is_absolute() || !paths.binary().is_file() {
        return None;
    }
    let config: Config = serde_json::from_value(read(&plugin.join("install.json"))?).ok()?;
    let package = read(&plugin.join("package.json"))?;
    let entry = fs::symlink_metadata(plugin.join("dist/index.js")).ok()?;
    (config.protocol == 1
        && config.binary == paths.binary()
        && config.data_home == paths.root
        && package["name"] == "opencode-scheduler-plugin"
        && package["type"] == "module"
        && package["exports"]["."] == "./dist/index.js"
        && entry.is_file()
        && !entry.file_type().is_symlink())
    .then_some(plugin)
}
pub fn absent_status(paths: &Paths) -> Value {
    json!({"installed":{"binary":paths.binary().exists(),"plugin":installed_plugin_path(paths).is_some()},"plugin_host":{"state":"unknown"},"daemon":{"reachable":false,"protocol_compatible":false},"opencode":{"state":"unknown","service_version":null},"schedules":{"pending":null,"blocked":null,"failed":null,"admission_unconfirmed":null}})
}
pub async fn observed_absent_status(paths: &Paths) -> Value {
    let mut value = absent_status(paths);
    if let Some(plugin) = installed_plugin_path(paths)
        && let Ok(mut adapter) = crate::adapter::Adapter::new()
        && let Ok(info) = adapter.discover().await
    {
        value["opencode"] = json!({"state":"ready","service_version":info.version});
        value["plugin_host"]["state"] = json!(adapter.plugin_host_state(&plugin).await);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_plugin_requires_payload_and_matching_private_configuration() {
        // Catches hardcoded status, stale metadata, or a config pointing at another store.
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            root: dir.path().join("data"),
        };
        fs::create_dir_all(paths.command_dir()).unwrap();
        fs::write(paths.binary(), "fixture").unwrap();
        let plugin = dir.path().join("plugin");
        fs::create_dir_all(plugin.join("dist")).unwrap();
        fs::write(
            plugin.join("package.json"),
            r#"{"name":"opencode-scheduler-plugin","type":"module","exports":{".":"./dist/index.js"}}"#,
        )
        .unwrap();
        fs::write(plugin.join("dist/index.js"), "fixture").unwrap();
        write_json(
            &paths.file("product-install.json"),
            &json!({"protocol":1,"plugin_path":plugin}),
        )
        .unwrap();
        write_json(
            &plugin.join("install.json"),
            &json!({"protocol":1,"binary":paths.binary(),"data_home":paths.root}),
        )
        .unwrap();
        assert_eq!(absent_status(&paths)["installed"]["plugin"], true);
        assert_eq!(absent_status(&paths)["plugin_host"]["state"], "unknown");
        write_json(
            &plugin.join("install.json"),
            &json!({"protocol":1,"binary":paths.binary(),"data_home":dir.path()}),
        )
        .unwrap();
        assert_eq!(absent_status(&paths)["installed"]["plugin"], false);
        write_json(
            &plugin.join("install.json"),
            &json!({"protocol":1,"binary":paths.binary(),"data_home":paths.root}),
        )
        .unwrap();
        assert_eq!(absent_status(&paths)["installed"]["plugin"], true);
        fs::remove_file(plugin.join("dist/index.js")).unwrap();
        assert_eq!(absent_status(&paths)["installed"]["plugin"], false);
    }

    #[tokio::test]
    async fn rpc_from_an_older_daemon_canonicalizes_nested_errors() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = Endpoint {
            protocol: 1,
            instance_id: Uuid::new_v4().to_string(),
            base_url: format!("http://{}", listener.local_addr().unwrap()),
            pid: 1,
            token: "ab".repeat(48),
        };
        let body = json!({"ok":true,"result":{"kind":"log","data":{"items":[{"seq":1,"schedule_id":"fixture","ordinal":1,"started_at":"2026-11-06T00:00:00Z","finished_at":null,"phase":"discovery","outcome":"transient","certainty":"not_admitted","error":{"code":"opencode_unavailable","message":"synthetic SQL private/error/path"},"next_attempt_at":null}],"next_cursor":null}}}).to_string();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let result = rpc(
            &endpoint,
            &Operation::ScheduleLog(LogInput {
                id: Uuid::new_v4().to_string(),
                limit: 50,
                cursor: None,
            }),
        )
        .await
        .unwrap();
        server.await.unwrap();
        let message = result["result"]["data"]["items"][0]["error"]["message"]
            .as_str()
            .unwrap();
        assert!(message.contains("Hint:"));
        assert!(!message.contains("private/error/path"));
    }

    #[tokio::test]
    async fn daemon_connection_failure_preserves_uncertain_create_recovery() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let endpoint = Endpoint {
            protocol: 1,
            instance_id: Uuid::new_v4().to_string(),
            base_url,
            pid: 1,
            token: "ab".repeat(48),
        };
        let error = rpc(&endpoint, &Operation::Status(EmptyInput {}))
            .await
            .unwrap_err();
        assert_eq!(error.code, "daemon_unavailable");
        assert!(
            error
                .message
                .contains("same request ID and identical input")
        );
        assert!(!error.message.contains(&endpoint.token));
    }

    #[tokio::test]
    async fn malformed_scheduler_responses_are_not_mislabeled_as_opencode_failures() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (response, code) in [
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nbad",
                "unsupported_protocol",
            ),
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{",
                "daemon_unavailable",
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = Endpoint {
                protocol: 1,
                instance_id: Uuid::new_v4().to_string(),
                base_url: format!("http://{}", listener.local_addr().unwrap()),
                pid: 1,
                token: "ab".repeat(48),
            };
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = [0; 4096];
                assert!(socket.read(&mut bytes).await.unwrap() > 0);
                socket.write_all(response.as_bytes()).await.unwrap();
            });
            let error = rpc(&endpoint, &Operation::Status(EmptyInput {}))
                .await
                .unwrap_err()
                .sanitized()
                .unwrap();
            server.await.unwrap();
            assert_eq!(error.code, code);
            assert!(error.message.contains("Hint:"));
        }
    }

    #[tokio::test]
    async fn invalid_daemon_executable_reports_startup_stage_without_paths() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            root: dir.path().join("private"),
        };
        paths.prepare().unwrap();
        fs::create_dir_all(paths.command_dir()).unwrap();
        fs::write(paths.binary(), "not an executable").unwrap();
        let error = ensure_daemon(&paths)
            .await
            .unwrap_err()
            .sanitized()
            .unwrap();
        assert_eq!(error.code, "daemon_startup_failure");
        assert!(error.message.contains("diagnostic.log"));
        assert!(
            !error
                .message
                .contains(&dir.path().to_string_lossy().to_string())
        );
    }

    #[cfg(windows)]
    #[test]
    fn private_helper_allows_slow_startup_and_reaps_on_deadline() {
        use std::os::windows::process::CommandExt;
        let launch = |script: &str| {
            Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", script])
                .creation_flags(0x08000000)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        };
        let mut slow = launch("Start-Sleep -Milliseconds 5200");
        assert!(
            wait_private_helper(&mut slow, PRIVATE_HELPER_TIMEOUT)
                .unwrap()
                .success()
        );
        let mut stuck = launch("Start-Sleep -Seconds 60");
        let error = wait_private_helper(&mut stuck, Duration::from_millis(50)).unwrap_err();
        assert_eq!(error.code, "private_directory_timeout");
        assert!(stuck.try_wait().unwrap().is_some());
    }

    #[test]
    fn private_directory_failure_identifies_security_preparation_stage() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("file");
        fs::write(&root, "keep").unwrap();
        let error = Paths { root: root.clone() }
            .prepare()
            .unwrap_err()
            .sanitized()
            .unwrap();
        assert_eq!(error.code, "private_directory_failure");
        assert!(error.message.contains("permissions"));
        assert_eq!(fs::read_to_string(root).unwrap(), "keep");
    }
}

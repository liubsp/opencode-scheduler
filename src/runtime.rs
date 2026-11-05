use crate::{
    adapter::{bounded_json, local_url, unavailable},
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
        if self.root.exists()
            && fs::symlink_metadata(&self.root)
                .map_err(|_| ApiError::storage())?
                .file_type()
                .is_symlink()
        {
            return Err(ApiError::storage());
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
                .env("SCHEDULER_PRIVATE_ROOT", &self.root)
                .creation_flags(0x08000000)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|_| ApiError::storage())?;
            let started = std::time::Instant::now();
            let status = loop {
                if let Some(status) = child.try_wait().map_err(|_| ApiError::storage())? {
                    break status;
                }
                if started.elapsed() >= Duration::from_secs(5) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ApiError::storage());
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            if !status.success() {
                return Err(ApiError::storage());
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, MetadataExt};
            if !self.root.exists() {
                let mut builder = fs::DirBuilder::new();
                builder.recursive(true).mode(0o700);
                builder
                    .create(&self.root)
                    .map_err(|_| ApiError::storage())?;
            }
            let meta = fs::metadata(&self.root).map_err(|_| ApiError::storage())?;
            // Compare ownership against the current process's own newly-created file.
            let probe = self.file(&format!(".owner-{}", Uuid::new_v4()));
            let file = private_file(&probe)?;
            let uid = file.metadata().map_err(|_| ApiError::storage())?.uid();
            drop(file);
            fs::remove_file(probe).map_err(|_| ApiError::storage())?;
            if meta.uid() != uid || meta.mode() & 0o077 != 0 {
                return Err(ApiError::storage());
            }
        }
        Ok(())
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
        local_url(&self.base_url, true)?;
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
        .map_err(|_| unavailable())
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
    let value = bounded_json(response, 64 * 1024).await?;
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
    let started = std::time::Instant::now();
    paths.prepare()?;
    tokio::time::timeout(
        Duration::from_secs(15).saturating_sub(started.elapsed()),
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
            lifetime.try_lock_exclusive().map_err(|_| unavailable())?;
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
            let mut child = crate::launch::spawn(&exe, &paths.root)?;
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
                command.spawn().map_err(|_| unavailable())?
            };
            loop {
                if let Some(endpoint) = discover_daemon(paths).await? {
                    return Ok(endpoint);
                }
                if child.try_wait().map_err(|_| unavailable())?.is_some() {
                    return Err(unavailable());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        },
    )
    .await
    .map_err(|_| unavailable())?
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
        .map_err(|_| unavailable())?;
    bounded_json(response, 16 * 1024 * 1024).await
}
pub fn absent_status(paths: &Paths) -> Value {
    json!({"installed":{"binary":paths.binary().exists(),"plugin":false},"daemon":{"reachable":false,"protocol_compatible":false},"opencode":{"state":"unknown","service_version":null},"schedules":{"pending":null,"blocked":null,"failed":null,"admission_unconfirmed":null}})
}

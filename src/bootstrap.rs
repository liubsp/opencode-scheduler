use crate::{adapter::unavailable, types::ApiError};
use std::process::Stdio;
use std::time::Duration;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::oneshot,
};

pub fn command() -> tokio::process::Command {
    let mut command = tokio::process::Command::new("opencode");
    command.args(["api", "get", "/api/info"]);
    command
}

async fn capture(stream: impl AsyncRead + Unpin) -> Result<(), ApiError> {
    let mut bytes = Vec::new();
    stream
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| unavailable())?;
    if bytes.len() > 64 * 1024 {
        return Err(unavailable());
    }
    // Never parse, persist or surface CLI output, which may contain service credentials.
    Ok(())
}

pub async fn run(mut command: tokio::process::Command, bound: Duration) -> Result<(), ApiError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command.spawn().map_err(|_| unavailable())?;
    let stdout = child.stdout.take().ok_or_else(unavailable)?;
    let stderr = child.stderr.take().ok_or_else(unavailable)?;
    let (send, mut cancel) = oneshot::channel();
    struct Cancel(Option<oneshot::Sender<()>>);
    impl Drop for Cancel {
        fn drop(&mut self) {
            if let Some(send) = self.0.take() {
                let _ = send.send(());
            }
        }
    }
    let _cancel = Cancel(Some(send));
    // This owned supervisor survives cancellation of its caller long enough to kill and reap.
    let supervisor = tokio::spawn(async move {
        let result = {
            let operation = async {
                let (status, (), ()) = tokio::try_join!(
                    async { child.wait().await.map_err(|_| unavailable()) },
                    capture(stdout),
                    capture(stderr)
                )?;
                if status.success() {
                    Ok(())
                } else {
                    Err(unavailable())
                }
            };
            tokio::pin!(operation);
            tokio::select! {
                result = &mut operation => result,
                _ = tokio::time::sleep(bound.min(Duration::from_secs(15))) => Err(unavailable()),
                _ = &mut cancel => Err(unavailable()),
            }
        };
        if child.id().is_some() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        result
    });
    supervisor.await.map_err(|_| unavailable())?
}

#[cfg(windows)]
pub fn verified_dead(pid: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, WAIT_OBJECT_0},
        System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    };
    if pid == 0 {
        return false;
    }
    // SAFETY: the handle is checked before use and closed on every successful open.
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return GetLastError() == ERROR_INVALID_PARAMETER;
        }
        let dead = WaitForSingleObject(handle, 0) == WAIT_OBJECT_0;
        CloseHandle(handle);
        dead
    }
}
#[cfg(not(windows))]
pub fn verified_dead(_: u32) -> bool {
    // shortcut: outside validated Windows, an existing registration has inconclusive ownership;
    // fail closed rather than launching a CLI that could replace a live service.
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supported_helper_and_live_process_are_not_ambiguous() {
        let command = command();
        assert_eq!(command.as_std().get_program(), "opencode");
        assert_eq!(
            command.as_std().get_args().collect::<Vec<_>>(),
            ["api", "get", "/api/info"]
        );
        assert!(!verified_dead(std::process::id()));
        assert!(!verified_dead(0));
    }
}

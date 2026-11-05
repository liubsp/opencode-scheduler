//! Staged executable installation and native user command registration.
use crate::{
    adapter::unavailable,
    runtime::{self, Paths, private_file, read_bounded, write_json},
    types::*,
};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{fs, path::Path, time::Duration};
use uuid::Uuid;

pub fn registered_path(existing: &str, bin: &str) -> String {
    let normalize = |s: &str| {
        s.trim()
            .trim_matches('"')
            .trim_end_matches(['\\', '/'])
            .replace('/', "\\")
            .to_lowercase()
    };
    if existing.split(';').any(|p| normalize(p) == normalize(bin)) {
        return existing.to_owned();
    }
    format!(
        "{existing}{}{bin}",
        if existing.is_empty() || existing.ends_with(';') {
            ""
        } else {
            ";"
        }
    )
}

#[cfg(windows)]
fn register(bin: &Path) -> Result<(), ApiError> {
    use windows_sys::{
        Win32::{
            Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
            System::Registry::*,
            UI::WindowsAndMessaging::*,
        },
        core::w,
    };
    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }
    // SAFETY: all buffers remain live during the calls, registry handles are owned by Key.
    unsafe {
        let mut raw = std::ptr::null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Environment"),
            0,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            &mut raw,
        ) != ERROR_SUCCESS
        {
            return Err(ApiError::storage());
        }
        let key = Key(raw);
        let mut kind = REG_EXPAND_SZ;
        let mut size = 0;
        let status = RegQueryValueExW(
            key.0,
            w!("Path"),
            std::ptr::null(),
            &mut kind,
            std::ptr::null_mut(),
            &mut size,
        );
        if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND {
            return Err(ApiError::storage());
        }
        let existing = if status == ERROR_FILE_NOT_FOUND {
            kind = REG_EXPAND_SZ;
            String::new()
        } else {
            if ![REG_SZ, REG_EXPAND_SZ].contains(&kind) || size > 128 * 1024 || size % 2 != 0 {
                return Err(ApiError::storage());
            }
            let mut bytes = vec![0u16; size as usize / 2];
            if RegQueryValueExW(
                key.0,
                w!("Path"),
                std::ptr::null(),
                &mut kind,
                bytes.as_mut_ptr().cast(),
                &mut size,
            ) != ERROR_SUCCESS
            {
                return Err(ApiError::storage());
            }
            while bytes.last() == Some(&0) {
                bytes.pop();
            }
            String::from_utf16(&bytes).map_err(|_| ApiError::storage())?
        };
        let updated = registered_path(&existing, bin.to_str().ok_or_else(ApiError::storage)?);
        if updated != existing {
            let wide: Vec<u16> = updated.encode_utf16().chain(Some(0)).collect();
            if RegSetValueExW(
                key.0,
                w!("Path"),
                0,
                kind,
                wide.as_ptr().cast(),
                (wide.len() * 2) as u32,
            ) != ERROR_SUCCESS
            {
                return Err(ApiError::storage());
            }
            // Notify shells; no process environment or unrelated registry values are replaced.
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                w!("Environment") as isize,
                SMTO_ABORTIFHUNG,
                1000,
                std::ptr::null_mut(),
            );
        }
    }
    Ok(())
}
#[cfg(not(windows))]
fn register(_: &Path) -> Result<(), ApiError> {
    // shortcut: native global registration is Windows-only until another platform is validated.
    Err(ApiError::invalid())
}

fn atomic_bytes(path: &Path, bytes: &[u8]) -> Result<(), ApiError> {
    use std::io::Write;
    let stage = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = private_file(&stage)?;
        file.write_all(bytes).map_err(|_| ApiError::storage())?;
        file.sync_all().map_err(|_| ApiError::storage())?;
        drop(file);
        fs::rename(&stage, path).map_err(|_| ApiError::storage())
    })();
    let _ = fs::remove_file(stage);
    result
}

pub async fn install(paths: &Paths, register_path: bool) -> Result<(), ApiError> {
    paths.prepare()?;
    let install_lock = private_file(&paths.file("install.lock"))?;
    install_lock
        .try_lock_exclusive()
        .map_err(|_| ApiError::storage())?;
    let binary = paths.binary();
    let parent = binary.parent().ok_or_else(ApiError::storage)?;
    fs::create_dir_all(parent).map_err(|_| ApiError::storage())?;
    let bin = paths.command_dir();
    let metadata_path = paths.file("install.json");
    let old_metadata = if metadata_path.exists() {
        Some(read_bounded(&metadata_path, 64 * 1024)?)
    } else {
        None
    };
    let mut metadata: Value = match &old_metadata {
        Some(bytes) => serde_json::from_slice(bytes).map_err(|_| ApiError::storage())?,
        None => json!({"plugin":null}),
    };
    if !metadata.is_object() {
        return Err(ApiError::storage());
    }
    metadata["binary"] = json!(binary);
    metadata["data_home"] = json!(paths.root);
    // Stage the complete candidate before asking any authenticated scheduler to stop.
    let stage = parent.join(format!(".next-{}.exe", Uuid::new_v4()));
    struct Stage(std::path::PathBuf);
    impl Drop for Stage {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let _stage = Stage(stage.clone());
    fs::copy(
        std::env::current_exe().map_err(|_| ApiError::storage())?,
        &stage,
    )
    .map_err(|_| ApiError::storage())?;
    private_file(&stage)?
        .sync_all()
        .map_err(|_| ApiError::storage())?;
    let startup = private_file(&paths.file("startup.lock"))?;
    startup.try_lock_exclusive().map_err(|_| unavailable())?;
    let endpoint = runtime::discover_daemon(paths).await?;
    let lifetime = private_file(&paths.file("daemon.lock"))?;
    if let Some(endpoint) = &endpoint {
        let response = runtime::rpc(endpoint, &Operation::DaemonStop(EmptyInput {})).await?;
        if response["ok"] != true {
            return Err(unavailable());
        }
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if !paths.file("endpoint.json").exists() && lifetime.try_lock_exclusive().is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .map_err(|_| unavailable())?;
    } else {
        // An unreachable but still live scheduler is not ours to replace blindly.
        lifetime.try_lock_exclusive().map_err(|_| unavailable())?;
    }
    let backup = binary.with_extension("previous.exe");
    let had_binary = binary.exists();
    let mut switched = false;
    let activated = (|| {
        if backup.exists() {
            fs::remove_file(&backup).map_err(|_| ApiError::storage())?;
        }
        if had_binary {
            fs::rename(&binary, &backup).map_err(|_| ApiError::storage())?;
        }
        if fs::rename(&stage, &binary).is_err() {
            if had_binary {
                let _ = fs::rename(&backup, &binary);
            }
            return Err(ApiError::storage());
        }
        switched = true;
        Ok(())
    })();
    let result = activated.and_then(|()| {
        write_json(&metadata_path, &metadata)?;
        if register_path {
            register(&bin)?;
        }
        Ok(())
    });
    if result.is_err() && switched {
        let _ = fs::remove_file(&binary);
        if had_binary {
            fs::rename(&backup, &binary).map_err(|_| ApiError::storage())?;
        }
        if let Some(bytes) = old_metadata {
            atomic_bytes(&metadata_path, &bytes)?;
        } else if metadata_path.exists() {
            fs::remove_file(&metadata_path).map_err(|_| ApiError::storage())?;
        }
    }
    drop(lifetime);
    drop(startup);
    if endpoint.is_some() {
        runtime::ensure_daemon(paths).await?;
    }
    // Windows can retain a renamed executable while the installer itself is running.
    // The next upgrade removes this owned backup before activation.
    if result.is_ok() {
        let _ = fs::remove_file(backup);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn path_registration_preserves_entries_and_is_idempotent() {
        // Catches replacing/normalizing unrelated PATH entries or duplicating our own bin.
        let bin = r"Scheduler Data\app\bin";
        for existing in [r"System;Other Tools;;", r"%LOCALAPPDATA%\Tools"] {
            let expected = format!("{existing}{bin}");
            let expected = if existing.ends_with(';') {
                expected
            } else {
                format!("{existing};{bin}")
            };
            let once = registered_path(existing, bin);
            assert_eq!(once, expected);
            assert_eq!(registered_path(&once, bin), once);
        }
        for existing in [
            r"System;SCHEDULER DATA\app\bin\;Other",
            r#"System;"Scheduler Data\app\bin\";Other"#,
            r"Scheduler Data\app\bin;Scheduler Data\app\bin",
        ] {
            assert_eq!(registered_path(existing, bin), existing);
        }
        assert_eq!(registered_path("", bin), bin);
    }
}

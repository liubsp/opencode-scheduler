//! Native Windows launch deliberately inherits no caller handles, including capture pipes.
#[cfg(windows)]
mod windows {
    use crate::{adapter::unavailable, types::ApiError};
    use std::{
        mem::{size_of, zeroed},
        os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        },
        path::Path,
        ptr::null,
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::{
            Console::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle},
            Threading::{
                CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CreateProcessW, DETACHED_PROCESS,
                GetExitCodeProcess, INFINITE, PROCESS_INFORMATION, STARTUPINFOW, TerminateProcess,
                WaitForSingleObject,
            },
        },
    };
    pub struct Child(OwnedHandle);
    impl Child {
        pub fn kill_and_wait(&mut self) {
            unsafe {
                TerminateProcess(self.0.as_raw_handle(), 1);
                WaitForSingleObject(self.0.as_raw_handle(), INFINITE);
            }
        }
        pub fn try_wait(&mut self) -> Result<Option<u32>, ApiError> {
            // SAFETY: the process handle is owned and remains live throughout each OS call.
            unsafe {
                match WaitForSingleObject(self.0.as_raw_handle(), 0) {
                    WAIT_TIMEOUT => Ok(None),
                    WAIT_OBJECT_0 => {
                        let mut code = 0;
                        if GetExitCodeProcess(self.0.as_raw_handle(), &mut code) == 0 {
                            return Err(unavailable());
                        }
                        Ok(Some(code))
                    }
                    _ => Err(unavailable()),
                }
            }
        }
    }
    #[cfg(test)]
    impl From<std::process::Child> for Child {
        fn from(child: std::process::Child) -> Self {
            Self(child.into())
        }
    }

    #[cfg(not(test))]
    pub fn spawn_private(exe: &Path, root: &Path) -> Result<Child, ApiError> {
        let app: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
        let root: Vec<u16> = root.as_os_str().encode_wide().collect();
        // Windows paths cannot contain quotes/NUL. Double trailing backslashes
        // before the closing quote so the CRT receives the literal path.
        if app[..app.len() - 1].contains(&0) || root.iter().any(|c| *c == 0 || *c == b'"' as u16) {
            return Err(ApiError::coded("private_directory_failure"));
        }
        let mut line = vec![b'"' as u16];
        line.extend_from_slice(&app[..app.len() - 1]);
        line.extend("\" --internal-prepare-private-directory \"".encode_utf16());
        line.extend_from_slice(&root);
        line.extend(
            root.iter()
                .rev()
                .take_while(|c| **c == b'\\' as u16)
                .copied(),
        );
        line.extend([b'"' as u16, 0]);
        unsafe {
            let mut startup: STARTUPINFOW = zeroed();
            startup.cb = size_of::<STARTUPINFOW>() as u32;
            // Explicit null standard handles, no console, and bInheritHandles=FALSE.
            startup.dwFlags = windows_sys::Win32::System::Threading::STARTF_USESTDHANDLES;
            let mut process: PROCESS_INFORMATION = zeroed();
            if CreateProcessW(
                app.as_ptr(),
                line.as_mut_ptr(),
                null(),
                null(),
                0,
                CREATE_NO_WINDOW,
                null(),
                null(),
                &startup,
                &mut process,
            ) == 0
            {
                return Err(ApiError::coded("private_directory_failure"));
            }
            CloseHandle(process.hThread);
            Ok(Child(OwnedHandle::from_raw_handle(process.hProcess)))
        }
    }
    pub fn spawn(exe: &Path, root: &Path) -> Result<Child, ApiError> {
        let app: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
        let dir: Vec<u16> = root.as_os_str().encode_wide().chain(Some(0)).collect();
        // Windows filenames cannot contain a quote and an executable path has no trailing slash.
        let mut line: Vec<u16> =
            format!("\"{}\" daemon-run", exe.to_str().ok_or_else(unavailable)?)
                .encode_utf16()
                .chain(Some(0))
                .collect();
        // SAFETY: all buffers are NUL-terminated and live for this call. No handles are
        // inherited. Returned thread handle closes immediately; process handle is owned.
        unsafe {
            let mut startup: STARTUPINFOW = zeroed();
            startup.cb = size_of::<STARTUPINFOW>() as u32;
            let mut process: PROCESS_INFORMATION = zeroed();
            if CreateProcessW(
                app.as_ptr(),
                line.as_mut_ptr(),
                null(),
                null(),
                0,
                CREATE_NO_WINDOW | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP,
                null(),
                dir.as_ptr(),
                &startup,
                &mut process,
            ) == 0
            {
                return Err(unavailable());
            }
            CloseHandle(process.hThread);
            Ok(Child(OwnedHandle::from_raw_handle(process.hProcess)))
        }
    }
    pub fn diagnostics(path: &Path) -> Result<std::fs::File, ApiError> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|_| ApiError::storage())?;
        // SAFETY: the caller retains this separately opened log throughout the daemon lifetime.
        unsafe {
            if SetStdHandle(STD_ERROR_HANDLE, file.as_raw_handle()) == 0
                || SetStdHandle(STD_OUTPUT_HANDLE, file.as_raw_handle()) == 0
            {
                return Err(ApiError::storage());
            }
        }
        Ok(file)
    }
}
#[cfg(windows)]
pub use windows::*;

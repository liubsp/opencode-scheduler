//! The killable child operation. No runtime, RPC, shell or permission repair lives here.
use std::{
    ffi::{OsStr, c_void},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{
        ERROR_ALREADY_EXISTS, ERROR_INSUFFICIENT_BUFFER, ERROR_PATH_NOT_FOUND, GetLastError,
        INVALID_HANDLE_VALUE, LocalFree,
    },
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
        },
        *,
    },
    Storage::FileSystem::*,
    System::{
        SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE, FILE_PERSISTENT_ACLS},
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

type Result<T> = std::io::Result<T>;
fn refused() -> std::io::Error {
    std::io::Error::from(std::io::ErrorKind::PermissionDenied)
}
fn checked(ok: i32) -> Result<()> {
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn wide(value: &OsStr) -> Result<Vec<u16>> {
    let mut value: Vec<u16> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(refused());
    }
    value.push(0);
    Ok(value)
}

struct LocalAllocation(*mut c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}

// TokenUser contains borrowed pointers into this aligned buffer. Never return a SID
// independently of its owner, or use a thread impersonation token in this child.
struct User {
    data: Vec<usize>,
    length: usize,
}
impl User {
    fn query() -> Result<Self> {
        unsafe {
            let mut token = null_mut();
            checked(OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY,
                &mut token,
            ))?;
            let token = OwnedHandle::from_raw_handle(token);
            let mut length = 0;
            let status =
                GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut length);
            if status != 0
                || GetLastError() != ERROR_INSUFFICIENT_BUFFER
                || !(size_of::<TOKEN_USER>()..=65536).contains(&(length as usize))
            {
                return Err(refused());
            }
            let mut data = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
            let capacity = length;
            checked(GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                data.as_mut_ptr().cast(),
                capacity,
                &mut length,
            ))?;
            if length > capacity || (length as usize) < size_of::<TOKEN_USER>() {
                return Err(refused());
            }
            let user = Self {
                data,
                length: length as usize,
            };
            if !sid_in_region(user.sid(), user.data.as_ptr().cast(), user.length) {
                return Err(refused());
            }
            Ok(user)
        }
    }
    fn sid(&self) -> PSID {
        // The query established TOKEN_USER size/alignment; its SID is validated
        // before any consumer can receive User.
        unsafe { (*(self.data.as_ptr().cast::<TOKEN_USER>())).User.Sid }
    }
    fn string(&self) -> Result<String> {
        unsafe {
            let mut sid = null_mut();
            checked(ConvertSidToStringSidW(self.sid(), &mut sid))?;
            let _allocation = LocalAllocation(sid.cast());
            // A textual SID is bounded by the OS SID representation (15 subauthorities).
            let mut length = 0;
            while length < 256 && *sid.add(length) != 0 {
                length += 1;
            }
            if length == 256 {
                return Err(refused());
            }
            String::from_utf16(std::slice::from_raw_parts(sid, length)).map_err(|_| refused())
        }
    }
}

fn descriptor(sddl: &str) -> Result<LocalAllocation> {
    let text = wide(OsStr::new(sddl))?;
    unsafe {
        let mut sd = null_mut();
        checked(ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            null_mut(),
        ))?;
        Ok(LocalAllocation(sd))
    }
}

// Check a SID's complete variable-sized representation before asking the OS to
// validate it. Regions here are OS-owned ACLs or the bounded token query buffer.
unsafe fn sid_in_region(sid: PSID, start: *const c_void, length: usize) -> bool {
    let offset = (sid as usize).checked_sub(start as usize);
    let Some(offset) = offset else {
        return false;
    };
    if sid.is_null()
        || !(sid as usize).is_multiple_of(align_of::<u32>())
        || offset.checked_add(8).is_none_or(|end| end > length)
    {
        return false;
    }
    unsafe {
        let count = *sid.cast::<u8>().add(1) as usize;
        offset
            .checked_add(8 + count * 4)
            .is_some_and(|end| end <= length)
            && IsValidSid(sid) != 0
    }
}

unsafe fn validate_descriptor(sd: PSECURITY_DESCRIPTOR, user: PSID) -> Result<()> {
    unsafe {
        if sd.is_null() {
            return Err(refused());
        }
        checked(IsValidSecurityDescriptor(sd))?;
        let mut owner = null_mut();
        let mut defaulted = 0;
        checked(GetSecurityDescriptorOwner(sd, &mut owner, &mut defaulted))?;
        if owner.is_null() || IsValidSid(owner) == 0 || EqualSid(owner, user) == 0 {
            return Err(refused());
        }
        let mut present = 0;
        let mut acl = null_mut();
        checked(GetSecurityDescriptorDacl(
            sd,
            &mut present,
            &mut acl,
            &mut defaulted,
        ))?;
        if present == 0 || acl.is_null() {
            return Err(refused());
        }
        checked(IsValidAcl(acl))?;
        let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
        checked(GetAclInformation(
            acl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        ))?;
        let size = (*acl).AclSize as usize;
        if (info.AclBytesInUse as usize) > size {
            return Err(refused());
        }
        for index in 0..info.AceCount {
            let mut ace = null_mut();
            checked(GetAce(acl, index, &mut ace))?;
            let offset = (ace as usize)
                .checked_sub(acl as usize)
                .ok_or_else(refused)?;
            if offset
                .checked_add(size_of::<ACE_HEADER>())
                .is_none_or(|end| end > size)
            {
                return Err(refused());
            }
            let header = &*ace.cast::<ACE_HEADER>();
            let length = header.AceSize as usize;
            if length < size_of::<ACCESS_ALLOWED_ACE>()
                || offset.checked_add(length).is_none_or(|end| end > size)
            {
                return Err(refused());
            }
            // Only the standard Allow/Deny representations are supported. Object,
            // callback, conditional and unknown ACEs refuse, including Deny forms:
            // none can silently disappear from the exact foreign-Allow policy.
            let kind = header.AceType as u32;
            if kind != ACCESS_ALLOWED_ACE_TYPE && kind != ACCESS_DENIED_ACE_TYPE {
                return Err(refused());
            }
            let sid = ace
                .cast::<u8>()
                .add(std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart))
                .cast();
            if !sid_in_region(sid, ace, length)
                || std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart) + GetLengthSid(sid) as usize
                    != length
            {
                return Err(refused());
            }
            if kind == ACCESS_ALLOWED_ACE_TYPE && EqualSid(sid, user) == 0 {
                return Err(refused());
            }
        }
        Ok(())
    }
}

fn inspect(root: &Path, user: PSID) -> Result<()> {
    let path = wide(root.as_os_str())?;
    unsafe {
        let raw = CreateFileW(
            path.as_ptr(),
            READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        );
        if raw == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        let handle = OwnedHandle::from_raw_handle(raw);
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        checked(GetFileInformationByHandle(
            handle.as_raw_handle(),
            &mut info,
        ))?;
        if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
            || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(refused());
        }
        let mut flags = 0;
        checked(GetVolumeInformationByHandleW(
            handle.as_raw_handle(),
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            &mut flags,
            null_mut(),
            0,
        ))?;
        if flags & FILE_PERSISTENT_ACLS == 0 {
            return Err(refused());
        }
        let mut sd = null_mut();
        let status = GetSecurityInfo(
            handle.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sd,
        );
        if status != 0 {
            return Err(std::io::Error::from_raw_os_error(status as i32));
        }
        let sd = LocalAllocation(sd);
        validate_descriptor(sd.0, user)
    }
}

/// Prepare only this absolute root. Called synchronously by the hidden child entry.
pub fn prepare(root: &Path) -> Result<()> {
    if !root.is_absolute() {
        return Err(refused());
    }
    let user = User::query()?;
    let sid = user.string()?;
    let sd = descriptor(&format!("O:{sid}D:P(A;OICI;FA;;;{sid})"))?;
    let attrs = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    // Work outward only when a parent is missing. Each component is retried once
    // after its parent; an existing non-directory parent must not cause a loop.
    // gets the protected descriptor at creation, never a public intermediate ACL.
    let mut missing = vec![(root, false)];
    while let Some((path, parent_attempted)) = missing.pop() {
        let path_wide = wide(path.as_os_str())?;
        let error = unsafe {
            if CreateDirectoryW(path_wide.as_ptr(), &attrs) != 0 {
                continue;
            }
            GetLastError()
        };
        match error {
            ERROR_ALREADY_EXISTS => {} // Never rewrite an existing ACL.
            ERROR_PATH_NOT_FOUND if !parent_attempted => {
                let parent = path
                    .parent()
                    .filter(|parent| *parent != path)
                    .ok_or_else(refused)?;
                missing.push((path, true));
                missing.push((parent, false));
            }
            _ => return Err(std::io::Error::from_raw_os_error(error as i32)),
        }
    }
    inspect(root, user.sid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Paths;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
    };

    fn create_fixture(path: &Path, sddl: &str) {
        let text: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            let mut sd = null_mut();
            assert_ne!(
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    text.as_ptr(),
                    1,
                    &mut sd,
                    null_mut()
                ),
                0
            );
            let attrs = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd,
                bInheritHandle: 0,
            };
            let result = CreateDirectoryW(path.as_ptr(), &attrs);
            LocalFree(sd);
            assert_ne!(result, 0, "{}", std::io::Error::last_os_error());
        }
    }

    fn snapshot(root: &Path) -> String {
        let path = wide(root.as_os_str()).unwrap();
        unsafe {
            let mut sd = null_mut();
            assert_eq!(
                GetNamedSecurityInfoW(
                    path.as_ptr(),
                    SE_FILE_OBJECT,
                    OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    null_mut(),
                    null_mut(),
                    &mut sd
                ),
                0
            );
            let sd = LocalAllocation(sd);
            let mut text = null_mut();
            let mut length = 0;
            assert_ne!(
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    sd.0,
                    1,
                    OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                    &mut text,
                    &mut length
                ),
                0
            );
            let _text = LocalAllocation(text.cast());
            String::from_utf16(std::slice::from_raw_parts(text, length as usize - 1)).unwrap()
        }
    }

    #[test]
    fn creation_is_private_for_missing_parents_and_concurrent_callers() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("parent").join("child").join("root");
        let original_parent = snapshot(temp.path());
        std::thread::scope(|scope| {
            let calls: Vec<_> = (0..4)
                .map(|_| scope.spawn(|| Paths { root: root.clone() }.prepare()))
                .collect();
            for call in calls {
                call.join().unwrap().unwrap();
            }
        });
        assert_eq!(snapshot(temp.path()), original_parent);
        let user = User::query().unwrap();
        for directory in [
            root.as_path(),
            root.parent().unwrap(),
            root.parent().unwrap().parent().unwrap(),
        ] {
            inspect(directory, user.sid()).unwrap();
            let text = snapshot(directory);
            assert!(text.contains("D:P(A;OICI;FA;;;"), "{text}");
        }
        let before = snapshot(&root);
        std::fs::write(root.join("keep"), "unchanged").unwrap();
        Paths { root: root.clone() }.prepare().unwrap();
        assert_eq!(snapshot(&root), before);
        assert_eq!(
            std::fs::read_to_string(root.join("keep")).unwrap(),
            "unchanged"
        );
    }

    #[test]
    fn foreign_allows_are_refused_regardless_of_flags_masks_or_deny_offsets() {
        let temp = tempfile::tempdir().unwrap();
        let user = User::query().unwrap().string().unwrap();
        for (index, foreign) in [
            "(A;;FR;;;WD)",
            "(A;OICIIO;0x2;;;WD)",
            "(D;;0x2;;;WD)(A;;0x2;;;WD)",
        ]
        .iter()
        .enumerate()
        {
            let root = temp.path().join(index.to_string());
            create_fixture(&root, &format!("O:{user}D:P{foreign}(A;OICI;FA;;;{user})"));
            let before = snapshot(&root);
            assert!(Paths { root: root.clone() }.prepare().is_err(), "{foreign}");
            assert_eq!(snapshot(&root), before);
        }
        let parent = temp.path().join("inherited");
        create_fixture(
            &parent,
            &format!("O:{user}D:P(A;OICI;FR;;;WD)(A;OICI;FA;;;{user})"),
        );
        let root = parent.join("root");
        std::fs::create_dir(&root).unwrap();
        let before = snapshot(&root);
        assert!(
            before.contains("ID"),
            "fixture must really inherit: {before}"
        );
        assert!(Paths { root: root.clone() }.prepare().is_err());
        assert_eq!(snapshot(&root), before);
    }

    #[test]
    fn empty_and_deny_only_are_not_mistaken_for_unrestricted_or_unsupported_descriptors() {
        let user = User::query().unwrap();
        let sid = user.string().unwrap();
        for dacl in ["D:P", "D:P(D;;0x2;;;WD)"] {
            let sd = descriptor(&format!("O:{sid}{dacl}")).unwrap();
            assert!(
                unsafe { validate_descriptor(sd.0, user.sid()) }.is_ok(),
                "{dacl}"
            );
        }
        // OS constructs the uncommon representation; no binary ACL fabrication.
        for sddl in [
            format!("O:{sid}"), // absent DACL
            format!("O:{sid}D:NO_ACCESS_CONTROL"),
            format!("O:SYD:P(A;;FA;;;{sid})"), // different owner, regardless of grant
            format!("O:{sid}D:P(XA;;FR;;;WD;(@User.Department == \"Finance\"))"), // unsupported conditional Allow
        ] {
            let sd = descriptor(&sddl).unwrap();
            assert!(
                unsafe { validate_descriptor(sd.0, user.sid()) }.is_err(),
                "{sddl}"
            );
        }
    }

    #[test]
    fn current_user_object_allow_is_refused_after_confirming_its_os_representation() {
        use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_OBJECT_ACE_TYPE;

        let user = User::query().unwrap();
        let sid = user.string().unwrap();
        // A GUID prevents OS conversion from normalizing OA to a standard Allow.
        // The current-user trustee isolates unsupported-form refusal from the
        // separate foreign-trustee policy. This is an opaque fixture GUID only.
        let sd = descriptor(&format!(
            "O:{sid}D:P(OA;;FR;00000000-0000-0000-0000-000000000001;;{sid})"
        ))
        .unwrap();
        unsafe {
            let mut present = 0;
            let mut defaulted = 0;
            let mut acl = null_mut();
            checked(GetSecurityDescriptorDacl(
                sd.0,
                &mut present,
                &mut acl,
                &mut defaulted,
            ))
            .unwrap();
            assert_ne!(present, 0);
            assert!(!acl.is_null());
            let mut ace = null_mut();
            checked(GetAce(acl, 0, &mut ace)).unwrap();
            assert_eq!(
                (*ace.cast::<ACE_HEADER>()).AceType as u32,
                ACCESS_ALLOWED_OBJECT_ACE_TYPE,
                "OS conversion must preserve a real object Allow ACE"
            );
            assert!(validate_descriptor(sd.0, user.sid()).is_err());
        }
    }

    #[test]
    fn refused_roots_leave_data_and_no_open_directory_handles() {
        let temp = tempfile::tempdir().unwrap();
        let user = User::query().unwrap().string().unwrap();
        let root = temp.path().join("foreign");
        create_fixture(&root, &format!("O:{user}D:P(A;;FR;;;WD)(A;;FA;;;{user})"));
        std::fs::write(root.join("keep"), "unchanged").unwrap();
        for _ in 0..16 {
            assert!(prepare(&root).is_err());
        }
        let path = wide(root.as_os_str()).unwrap();
        unsafe {
            let handle = CreateFileW(
                path.as_ptr(),
                READ_CONTROL,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                null_mut(),
            );
            assert_ne!(
                handle, INVALID_HANDLE_VALUE,
                "a refused inspection leaked a directory handle"
            );
            drop(OwnedHandle::from_raw_handle(handle));
        }
        let file = temp.path().join("file");
        std::fs::write(&file, "keep").unwrap();
        assert_eq!(
            Paths {
                root: file.join("missing")
            }
            .prepare()
            .unwrap_err()
            .code,
            "private_directory_failure"
        );
        assert!(prepare(&file).is_err());
        assert_eq!(std::fs::read_to_string(file).unwrap(), "keep");
        assert_eq!(
            std::fs::read_to_string(root.join("keep")).unwrap(),
            "unchanged"
        );
    }

    #[test]
    fn root_reparse_point_is_not_followed() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        prepare(&target).unwrap();
        let link = temp.path().join("link");
        // Junctions need no developer mode/admin privileges. Only owned fixtures.
        let output = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(prepare(&link).is_err());
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
        std::fs::remove_dir(&link).unwrap();
    }

    #[test]
    fn unrestricted_directory_is_refused_without_repair() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("null-dacl");
        // Omitted owner becomes the creator; NO_ACCESS_CONTROL is a NULL DACL,
        // not an empty DACL. An ACL enumerator must not mistake it for private.
        create_fixture(&root, "D:NO_ACCESS_CONTROL");
        let before = snapshot(&root);
        std::fs::write(root.join("keep"), "unchanged").unwrap();
        assert!(Paths { root: root.clone() }.prepare().is_err());
        assert_eq!(snapshot(&root), before);
        assert_eq!(
            std::fs::read_to_string(root.join("keep")).unwrap(),
            "unchanged"
        );
    }
}

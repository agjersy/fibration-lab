//! Windows implementation: AppContainer + temporary ACEs + job object.
//!
//! Order of operations for one run (`run`):
//!
//! 1. Take the machine-wide ACL mutex (two helpers editing the same DACL would race).
//! 2. Undo what dead runs left behind (journals that can be opened exclusively).
//! 3. Create the AppContainer profile (no capabilities), write this run's journal, grant ACEs.
//! 4. Release the mutex; create the job, start the child suspended inside the AppContainer,
//!    assign it to the job, resume, wait.
//! 5. Kill whatever is left in the job, retake the mutex, revoke the ACEs, delete the profile,
//!    remove the journal.
//!
//! Step 5 runs from `Drop`, so it also happens on error paths. It cannot run if the helper is
//! killed; that is what the journal and step 2 are for.

use std::ffi::{c_void, OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use windows_sys::core::{BOOL, HRESULT, PWSTR};
use windows_sys::Win32::Foundation::{
    LocalFree, SetHandleInformation, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, HANDLE,
    HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, WAIT_ABANDONED, WAIT_FAILED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSidToSidW, GetNamedSecurityInfoW, SetEntriesInAclW,
    SetNamedSecurityInfoW, EXPLICIT_ACCESS_W, GRANT_ACCESS, NO_MULTIPLE_TRUSTEE, REVOKE_ACCESS,
    SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    CreateWellKnownSid, EqualSid, FreeSid, GetAce, WinBuiltinAnyPackageSid, ACCESS_ALLOWED_ACE,
    ACE_HEADER, ACL, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, INHERIT_ONLY_ACE,
    NO_INHERITANCE, OBJECT_INHERIT_ACE, PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Console::{
    GetStdHandle, SetConsoleCtrlHandler, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicUIRestrictions,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
    JOBOBJECT_BASIC_UI_RESTRICTIONS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
    JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOB_OBJECT_UILIMIT_DESKTOP,
    JOB_OBJECT_UILIMIT_DISPLAYSETTINGS, JOB_OBJECT_UILIMIT_EXITWINDOWS,
    JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES, JOB_OBJECT_UILIMIT_READCLIPBOARD,
    JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS, JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateMutexW, CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, ReleaseMutex, ResumeThread, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
    STARTF_USESTDHANDLES, STARTUPINFOEXW,
};
use windows_sys::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath};

use crate::journal::{self, Journal};
use crate::{quote, Output, SandboxSpec, Stdio, PROFILE_PREFIX};

/// Read + execute, as Explorer's "Read & execute" (0x1200A9).
const READ_MASK: u32 = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;
/// Explorer's "Modify" (0x1301BF): read, write, execute, delete; no WRITE_DAC, no WRITE_OWNER.
const WRITE_MASK: u32 = FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE;

/// Name of the mutex serialising DACL edits across helper processes of this logon session.
const ACL_MUTEX: &str = "Local\\wb-sandbox-acl";
/// Overrides the journal directory (default `%LOCALAPPDATA%\wb-sandbox\journal`).
const STATE_DIR_ENV: &str = "WB_SANDBOX_STATE_DIR";
/// When set to a non-empty value, one line of phase timings is printed to stderr after the run.
const TRACE_ENV: &str = "WB_SANDBOX_TRACE";

fn warn(message: impl std::fmt::Display) {
    eprintln!("present-sandbox: warning: {message}");
}

fn last_error(context: &str) -> io::Error {
    let e = io::Error::last_os_error();
    io::Error::new(e.kind(), format!("{context}: {e}"))
}

fn win32_error(context: &str, code: u32) -> io::Error {
    let e = io::Error::from_raw_os_error(code as i32);
    io::Error::new(e.kind(), format!("{context}: {e}"))
}

fn hresult_error(context: &str, hr: HRESULT) -> io::Error {
    let bits = hr as u32;
    if bits & 0xFFFF_0000 == 0x8007_0000 {
        win32_error(context, bits & 0xFFFF)
    } else {
        io::Error::other(format!("{context}: HRESULT 0x{bits:08X}"))
    }
}

/// NUL-terminated UTF-16; interior NULs are refused because they would silently truncate.
fn wide(s: &OsStr) -> io::Result<Vec<u16>> {
    let mut w: Vec<u16> = s.encode_wide().collect();
    if w.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "string contains a NUL character",
        ));
    }
    w.push(0);
    Ok(w)
}

/// Copy a NUL-terminated wide string.
///
/// # Safety
/// `p` must point to a valid NUL-terminated UTF-16 string.
unsafe fn from_wide_ptr(p: *const u16) -> OsString {
    let mut len = 0usize;
    while *p.add(len) != 0 {
        len += 1;
    }
    OsString::from_wide(std::slice::from_raw_parts(p, len))
}

fn owned(handle: HANDLE) -> OwnedHandle {
    // SAFETY: callers pass a handle they own and have checked to be valid.
    unsafe { OwnedHandle::from_raw_handle(handle) }
}

fn raw(handle: &OwnedHandle) -> HANDLE {
    handle.as_raw_handle()
}

// ---- ACL mutex ---------------------------------------------------------------------------------

struct AclLock(Option<OwnedHandle>);

impl AclLock {
    /// Best effort: if the mutex cannot be created the run proceeds unserialised.
    fn acquire() -> AclLock {
        let Ok(name) = wide(OsStr::new(ACL_MUTEX)) else {
            return AclLock(None);
        };
        // SAFETY: plain Win32 calls with valid arguments.
        unsafe {
            let handle = CreateMutexW(null(), 0, name.as_ptr());
            if handle.is_null() {
                warn(last_error("the ACL mutex could not be created"));
                return AclLock(None);
            }
            let handle = owned(handle);
            match WaitForSingleObject(raw(&handle), INFINITE) {
                // An abandoned mutex means a previous holder died mid-edit; its journal covers that.
                WAIT_OBJECT_0 | WAIT_ABANDONED => AclLock(Some(handle)),
                _ => AclLock(None),
            }
        }
    }
}

impl Drop for AclLock {
    fn drop(&mut self) {
        if let Some(handle) = &self.0 {
            // SAFETY: this thread owns the mutex.
            unsafe { ReleaseMutex(raw(handle)) };
        }
    }
}

// ---- DACL edits --------------------------------------------------------------------------------

/// Frees a `LocalAlloc` allocation.
struct LocalMem(*mut c_void);

impl Drop for LocalMem {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from an API documented to need LocalFree.
            unsafe { LocalFree(self.0) };
        }
    }
}

/// Read the DACL of `path`. The returned descriptor owns the memory the ACL pointer refers to.
fn read_dacl(path: &[u16]) -> io::Result<(LocalMem, *mut ACL)> {
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor: *mut c_void = null_mut();
    // SAFETY: `path` is NUL-terminated; out-pointers are valid.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(win32_error("reading the access control list", rc));
    }
    Ok((LocalMem(descriptor), dacl))
}

/// Add (`GRANT_ACCESS`) or remove (`REVOKE_ACCESS`: every explicit ACE of `sid`) an entry in the
/// DACL of `path`. Inherited ACEs are untouched and `SetNamedSecurityInfoW` propagates the
/// change to existing children.
fn edit_dacl(path: &[u16], sid: PSID, mode: i32, mask: u32, inheritance: u32) -> io::Result<()> {
    let (_descriptor, old) = read_dacl(path)?;
    if old.is_null() {
        // A NULL DACL allows everyone everything. Writing a DACL with our single ACE would lock
        // everybody else out, so leave such objects alone.
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the object has a NULL access control list; refusing to replace it",
        ));
    }
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: mask,
        grfAccessMode: mode,
        grfInheritance: inheritance,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
            ptstrName: sid as PWSTR,
        },
    };
    let mut new: *mut ACL = null_mut();
    // SAFETY: `entry` and `old` are valid for the call; `new` receives a LocalAlloc'd ACL.
    let rc = unsafe { SetEntriesInAclW(1, &entry, old, &mut new) };
    if rc != ERROR_SUCCESS {
        return Err(win32_error("building the access control list", rc));
    }
    let _new_owner = LocalMem(new.cast());
    // SAFETY: `path` is NUL-terminated and `new` is a valid ACL.
    let rc = unsafe {
        SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            new,
            null(),
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(win32_error("writing the access control list", rc));
    }
    Ok(())
}

fn revoke(path: &[u16], sid: PSID) -> io::Result<()> {
    edit_dacl(path, sid, REVOKE_ACCESS, 0, NO_INHERITANCE)
}

/// Whether the object's own DACL already lets every AppContainer ("ALL APPLICATION PACKAGES",
/// S-1-15-2-1) do `mask`, with an ACE that applies to the object and, for directories, is
/// inherited by files and subdirectories. True for the Windows directory and Program Files, and
/// for a runtime directory an installer prepared; such directories need no per-run ACE.
fn all_packages_allowed(path: &[u16], mask: u32, is_dir: bool) -> bool {
    let Ok((_descriptor, dacl)) = read_dacl(path) else {
        return false;
    };
    if dacl.is_null() {
        return false;
    }
    // u32 storage keeps the SID aligned; SECURITY_MAX_SID_SIZE is 68 bytes.
    let mut sid_buf = [0u32; 17];
    let mut sid_len = (sid_buf.len() * size_of::<u32>()) as u32;
    let all_packages: PSID = sid_buf.as_mut_ptr().cast();
    // SAFETY: the buffer is large enough for any SID; the ACL belongs to `_descriptor`, and each
    // ACE pointer returned by GetAce is valid while it lives.
    unsafe {
        if CreateWellKnownSid(
            WinBuiltinAnyPackageSid,
            null_mut(),
            all_packages,
            &mut sid_len,
        ) == 0
        {
            return false;
        }
        let inherit = (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) as u8;
        for index in 0..u32::from((*dacl).AceCount) {
            let mut ace: *mut c_void = null_mut();
            if GetAce(dacl, index, &mut ace) == 0 || ace.is_null() {
                continue;
            }
            let header = &*(ace as *const ACE_HEADER);
            // ACCESS_ALLOWED_ACE_TYPE is 0.
            if header.AceType != 0 || header.AceFlags & (INHERIT_ONLY_ACE as u8) != 0 {
                continue;
            }
            if is_dir && header.AceFlags & inherit != inherit {
                continue;
            }
            let allowed = ace as *mut ACCESS_ALLOWED_ACE;
            if (*allowed).Mask & mask != mask {
                continue;
            }
            let ace_sid: PSID = std::ptr::addr_of_mut!((*allowed).SidStart).cast();
            if EqualSid(ace_sid, all_packages) != 0 {
                return true;
            }
        }
    }
    false
}

// ---- AppContainer profile ----------------------------------------------------------------------

struct Profile {
    name: Vec<u16>,
    sid: PSID,
    sid_string: String,
}

impl Profile {
    /// Create the profile with no capabilities. If a profile of that name is left over from a
    /// dead run (same pid, counter, and clock tick: practically impossible), its SID is derived
    /// and the profile is reused, then deleted with this run.
    fn create(name: &str) -> io::Result<Profile> {
        let wide_name = wide(OsStr::new(name))?;
        let display = wide(OsStr::new("Personal Present code sandbox"))?;
        let mut sid: PSID = null_mut();
        // SAFETY: all strings are NUL-terminated; no capabilities are passed.
        let mut hr = unsafe {
            CreateAppContainerProfile(
                wide_name.as_ptr(),
                display.as_ptr(),
                display.as_ptr(),
                null(),
                0,
                &mut sid,
            )
        };
        if hr as u32 == (0x8007_0000 | ERROR_ALREADY_EXISTS) {
            // SAFETY: as above.
            hr = unsafe { DeriveAppContainerSidFromAppContainerName(wide_name.as_ptr(), &mut sid) };
        }
        if hr < 0 || sid.is_null() {
            return Err(hresult_error(
                "the AppContainer profile could not be created",
                hr,
            ));
        }
        // From here on Drop frees the SID and deletes the profile.
        let mut profile = Profile {
            name: wide_name,
            sid,
            sid_string: String::new(),
        };
        let mut text: PWSTR = null_mut();
        // SAFETY: `sid` is valid; `text` receives a LocalAlloc'd string.
        unsafe {
            if ConvertSidToStringSidW(sid, &mut text) == 0 {
                return Err(last_error("the AppContainer SID could not be formatted"));
            }
            let _text_owner = LocalMem(text.cast());
            profile.sid_string = from_wide_ptr(text).to_string_lossy().into_owned();
        }
        Ok(profile)
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        // SAFETY: the SID came from CreateAppContainerProfile / Derive...; the name is NUL-terminated.
        unsafe {
            FreeSid(self.sid);
            let hr = DeleteAppContainerProfile(self.name.as_ptr());
            if hr < 0 {
                warn(hresult_error(
                    "the AppContainer profile could not be deleted",
                    hr,
                ));
            }
        }
    }
}

fn unique_profile_name() -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tick = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    // At most 64 characters: letters, digits, '.', '-'.
    format!(
        "{PROFILE_PREFIX}.{}.{counter}.{:x}",
        std::process::id(),
        tick & 0xFFFF_FFFF
    )
}

// ---- journal -----------------------------------------------------------------------------------

/// `%LOCALAPPDATA%\wb-sandbox\journal`, resolved through the shell rather than the environment:
/// the kernel launches the helper with a minimal environment in which `USERPROFILE` points at
/// the scratch directory.
fn state_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(STATE_DIR_ENV).filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    let mut path: PWSTR = null_mut();
    // SAFETY: the out-pointer is valid; the string is freed with CoTaskMemFree as documented.
    let base = unsafe {
        let hr = SHGetKnownFolderPath(&FOLDERID_LocalAppData, 0, null_mut(), &mut path);
        let base = if hr >= 0 && !path.is_null() {
            Some(PathBuf::from(from_wide_ptr(path)))
        } else {
            None
        };
        CoTaskMemFree(path.cast());
        base
    }?;
    Some(base.join("wb-sandbox").join("journal"))
}

/// Open a journal so that no other process can open it at all while the handle lives.
fn open_exclusive(path: &Path, create: bool) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).share_mode(0);
    if create {
        options.create_new(true);
    }
    options.open(path)
}

/// Undo runs whose helper died: revoke their ACEs, delete their profiles, remove their journals.
/// A journal still held open by a live helper cannot be opened and is skipped.
fn clean_stale_runs(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(OsStr::to_str) != Some(journal::EXTENSION) {
            continue;
        }
        let Ok(mut file) = open_exclusive(&path, false) else {
            continue;
        };
        let mut text = String::new();
        let parsed = file
            .read_to_string(&mut text)
            .ok()
            .and_then(|_| Journal::parse(&text).ok());
        if let Some(stale) = parsed {
            undo_journal(&stale);
        }
        drop(file);
        let _ = fs::remove_file(&path);
    }
}

fn undo_journal(stale: &Journal) {
    let Ok(sid_text) = wide(OsStr::new(&stale.sid)) else {
        return;
    };
    let mut sid: PSID = null_mut();
    // SAFETY: NUL-terminated input; the SID is freed with LocalFree as documented.
    unsafe {
        if ConvertStringSidToSidW(sid_text.as_ptr(), &mut sid) != 0 && !sid.is_null() {
            let _sid_owner = LocalMem(sid);
            for path in &stale.paths {
                if let Ok(path) = wide(OsStr::new(path)) {
                    // Missing paths and paths we may no longer edit are skipped silently.
                    let _ = revoke(&path, sid);
                }
            }
        }
        // Only ever delete profiles this helper family created.
        if stale.profile.starts_with(PROFILE_PREFIX) {
            if let Ok(name) = wide(OsStr::new(&stale.profile)) {
                let _ = DeleteAppContainerProfile(name.as_ptr());
            }
        }
    }
}

// ---- one run's on-disk state -------------------------------------------------------------------

struct Grant {
    path: Vec<u16>,
    display: String,
    mask: u32,
    inheritance: u32,
}

/// Everything that must be undone after the child has exited. Field order matters only for
/// readability; `Drop` sequences the teardown explicitly.
struct Session {
    profile: Option<Profile>,
    granted: Vec<Vec<u16>>,
    journal: Option<(PathBuf, File)>,
}

impl Session {
    fn begin(spec: &SandboxSpec) -> io::Result<Session> {
        let _lock = AclLock::acquire();
        let dir = state_dir();
        if let Some(dir) = &dir {
            if let Err(e) = fs::create_dir_all(dir) {
                warn(format!(
                    "the journal directory {} could not be created: {e}",
                    dir.display()
                ));
            }
            clean_stale_runs(dir);
        }

        let name = unique_profile_name();
        let mut session = Session {
            profile: Some(Profile::create(&name)?),
            granted: Vec::new(),
            journal: None,
        };
        let (sid, sid_string) = match &session.profile {
            Some(p) => (p.sid, p.sid_string.clone()),
            None => unreachable!("the profile was just created"),
        };

        let (read, write) = spec.effective_dirs();
        let mut plan: Vec<Grant> = Vec::new();
        for (paths, mask, skip_if_public) in [(&read, READ_MASK, true), (&write, WRITE_MASK, false)]
        {
            for path in paths {
                let is_dir = path.is_dir();
                let wide_path = wide(path.as_os_str())?;
                if skip_if_public && all_packages_allowed(&wide_path, mask, is_dir) {
                    continue;
                }
                plan.push(Grant {
                    path: wide_path,
                    display: path.to_string_lossy().into_owned(),
                    mask,
                    inheritance: if is_dir {
                        OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
                    } else {
                        NO_INHERITANCE
                    },
                });
            }
        }

        // The journal goes to disk before the first DACL changes.
        if let Some(dir) = &dir {
            let record = Journal {
                profile: name.clone(),
                sid: sid_string,
                paths: plan.iter().map(|g| g.display.clone()).collect(),
            };
            let path = dir.join(format!("{name}.{}", journal::EXTENSION));
            let written = record.to_text().map_err(io::Error::other).and_then(|text| {
                let mut file = open_exclusive(&path, true)?;
                file.write_all(text.as_bytes())?;
                file.sync_all()?;
                Ok(file)
            });
            match written {
                Ok(file) => session.journal = Some((path, file)),
                Err(e) => warn(format!("no cleanup journal for this run ({e}); if the helper is killed, access entries for {name} stay on the granted directories")),
            }
        } else {
            warn("the local application data folder was not found; running without a cleanup journal");
        }

        for grant in plan {
            // Record first: a failed SetNamedSecurityInfoW may still have changed some children.
            session.granted.push(grant.path.clone());
            edit_dacl(
                &grant.path,
                sid,
                GRANT_ACCESS,
                grant.mask,
                grant.inheritance,
            )
            .map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("granting the sandbox access to {}: {e}", grant.display),
                )
            })?;
        }
        Ok(session)
    }

    fn sid(&self) -> PSID {
        self.profile.as_ref().map_or(null_mut(), |p| p.sid)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _lock = AclLock::acquire();
        let mut clean = true;
        if let Some(profile) = &self.profile {
            for path in &self.granted {
                if let Err(e) = revoke(path, profile.sid) {
                    clean = false;
                    warn(format!(
                        "access for the sandbox could not be removed from {}: {e}",
                        OsString::from_wide(&path[..path.len() - 1]).to_string_lossy()
                    ));
                }
            }
        }
        self.profile = None; // frees the SID and deletes the profile
        if let Some((path, file)) = self.journal.take() {
            drop(file);
            // Keep the journal if an ACE could not be removed, so a later run retries.
            if clean {
                let _ = fs::remove_file(path);
            }
        }
    }
}

// ---- process creation --------------------------------------------------------------------------

struct AttributeList {
    // usize storage keeps the opaque list pointer-aligned.
    buffer: Vec<usize>,
}

impl AttributeList {
    fn new(count: u32) -> io::Result<AttributeList> {
        let mut size = 0usize;
        // SAFETY: the sizing call is documented to fail with ERROR_INSUFFICIENT_BUFFER and set `size`.
        unsafe { InitializeProcThreadAttributeList(null_mut(), count, 0, &mut size) };
        if size == 0 {
            return Err(last_error("sizing the process attribute list"));
        }
        let mut list = AttributeList {
            buffer: vec![0usize; size.div_ceil(size_of::<usize>())],
        };
        // SAFETY: the buffer is at least `size` bytes.
        if unsafe { InitializeProcThreadAttributeList(list.ptr(), count, 0, &mut size) } == 0 {
            return Err(last_error("initialising the process attribute list"));
        }
        Ok(list)
    }

    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.buffer.as_mut_ptr().cast()
    }

    /// # Safety
    /// `value` must stay valid and unmoved until the list is dropped.
    unsafe fn set(&mut self, attribute: u32, value: *const c_void, size: usize) -> io::Result<()> {
        if UpdateProcThreadAttribute(
            self.ptr(),
            0,
            attribute as usize,
            value,
            size,
            null_mut(),
            null(),
        ) == 0
        {
            return Err(last_error("setting a process attribute"));
        }
        Ok(())
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: the list was initialised in `new`.
        unsafe { DeleteProcThreadAttributeList(self.ptr()) };
    }
}

fn create_job(spec: &SandboxSpec, ui_restrictions: bool) -> io::Result<OwnedHandle> {
    // SAFETY: plain Win32 calls; the structures are fully initialised (zeroed, then filled).
    unsafe {
        let job = CreateJobObjectW(null(), null());
        if job.is_null() {
            return Err(last_error("creating the job object"));
        }
        let job = owned(job);

        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
        let mut flags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
        if spec.memory > 0 {
            let bytes = usize::try_from(spec.memory).unwrap_or(usize::MAX);
            flags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_JOB_MEMORY;
            limits.ProcessMemoryLimit = bytes;
            limits.JobMemoryLimit = bytes;
        }
        if spec.processes > 0 {
            flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            limits.BasicLimitInformation.ActiveProcessLimit = spec.processes;
        }
        limits.BasicLimitInformation.LimitFlags = flags;
        if SetInformationJobObject(
            raw(&job),
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
        {
            return Err(last_error("setting the job limits"));
        }

        if ui_restrictions {
            let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
                UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
                    | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                    | JOB_OBJECT_UILIMIT_EXITWINDOWS
                    | JOB_OBJECT_UILIMIT_GLOBALATOMS
                    | JOB_OBJECT_UILIMIT_HANDLES
                    | JOB_OBJECT_UILIMIT_READCLIPBOARD
                    | JOB_OBJECT_UILIMIT_WRITECLIPBOARD
                    | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS,
            };
            if SetInformationJobObject(
                raw(&job),
                JobObjectBasicUIRestrictions,
                (&raw const ui).cast(),
                size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
            ) == 0
            {
                return Err(last_error("setting the job UI restrictions"));
            }
        }
        Ok(job)
    }
}

/// Put the (still suspended) child into a job. A job with UI restrictions may not be nestable
/// inside another job, which is the case when the helper's parent already runs in one; then the
/// child gets the same job without the UI restrictions and a warning is printed.
fn assign_to_job(spec: &SandboxSpec, process: HANDLE) -> io::Result<OwnedHandle> {
    let job = create_job(spec, true)?;
    // SAFETY: both handles are valid.
    if unsafe { AssignProcessToJobObject(raw(&job), process) } != 0 {
        return Ok(job);
    }
    let first = io::Error::last_os_error();
    let job = create_job(spec, false)?;
    // SAFETY: both handles are valid.
    if unsafe { AssignProcessToJobObject(raw(&job), process) } != 0 {
        warn(format!(
            "job UI restrictions are not applied ({first}); memory and process limits are"
        ));
        return Ok(job);
    }
    Err(last_error("assigning the process to the job object"))
}

/// A Unicode environment block: `NAME=value\0` ... `\0`.
fn environment_block(env: &[(OsString, OsString)]) -> Vec<u16> {
    let mut block = Vec::new();
    for (key, value) in env {
        block.extend(key.encode_wide());
        block.push(b'=' as u16);
        block.extend(value.encode_wide());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

fn usable(handle: HANDLE) -> bool {
    !handle.is_null() && handle != INVALID_HANDLE_VALUE
}

/// Mark `handle` inheritable; a handle that cannot be (closed, or not a real handle) is replaced
/// by "no handle" so that process creation does not fail on it.
fn inheritable(handle: HANDLE) -> HANDLE {
    // SAFETY: SetHandleInformation tolerates invalid handles by failing.
    if usable(handle)
        && unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } != 0
    {
        handle
    } else {
        null_mut()
    }
}

fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read: HANDLE = null_mut();
    let mut write: HANDLE = null_mut();
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 0,
    };
    // SAFETY: out-pointers and attributes are valid.
    if unsafe { CreatePipe(&mut read, &mut write, &attributes, 0) } == 0 {
        return Err(last_error("creating a pipe"));
    }
    Ok((owned(read), owned(write)))
}

fn drain(read: OwnedHandle) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = File::from(read).read_to_end(&mut buf);
        buf
    })
}

/// Terminates the child if it is dropped before `disarm` (an error between creation and resume).
struct KillOnDrop(HANDLE, bool);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if self.1 {
            // SAFETY: the process handle is still open; the caller closes it afterwards.
            unsafe { TerminateProcess(self.0, 1) };
        }
    }
}

fn launch(spec: &SandboxSpec, stdio: Stdio, app_container_sid: PSID) -> io::Result<Output> {
    let argv: Vec<Vec<u16>> = spec
        .command
        .iter()
        .map(|a| a.encode_wide().collect())
        .collect();
    let mut command_line =
        quote::command_line(&argv).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    command_line.push(0);
    let cwd = wide(spec.cwd.as_os_str())?;
    let environment = spec.env.as_deref().map(environment_block);

    // Standard handles. Owned ends live in these bindings until the function returns or they
    // are explicitly dropped after process creation.
    let mut null_in: Option<File> = None;
    let mut out_pipe: Option<(OwnedHandle, OwnedHandle)> = None;
    let mut err_pipe: Option<(OwnedHandle, OwnedHandle)> = None;
    let std_handles: [HANDLE; 3] = match stdio {
        // SAFETY: GetStdHandle has no preconditions.
        Stdio::Inherit => unsafe {
            [
                GetStdHandle(STD_INPUT_HANDLE),
                GetStdHandle(STD_OUTPUT_HANDLE),
                GetStdHandle(STD_ERROR_HANDLE),
            ]
        },
        Stdio::Capture => {
            let nul = null_in.insert(File::open("NUL")?).as_raw_handle();
            let out = raw(&out_pipe.insert(pipe()?).1);
            let err = raw(&err_pipe.insert(pipe()?).1);
            [nul, out, err]
        }
    };
    let std_handles = std_handles.map(inheritable);
    // The explicit handle list keeps every other inheritable handle of this process (for
    // example the kernel's own pipes, inherited by the helper) out of the sandbox. Entries must
    // be unique.
    let mut handle_list: Vec<HANDLE> = Vec::new();
    for handle in std_handles {
        if usable(handle) && !handle_list.contains(&handle) {
            handle_list.push(handle);
        }
    }

    let mut capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: app_container_sid,
        Capabilities: null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };
    let mut attributes = AttributeList::new(if handle_list.is_empty() { 1 } else { 2 })?;
    // SAFETY: `capabilities` and `handle_list` outlive `attributes` (declared earlier, dropped
    // later) and are not moved or resized after this point.
    unsafe {
        attributes.set(
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
            (&raw mut capabilities).cast(),
            size_of::<SECURITY_CAPABILITIES>(),
        )?;
        if !handle_list.is_empty() {
            attributes.set(
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                handle_list.as_ptr().cast(),
                handle_list.len() * size_of::<HANDLE>(),
            )?;
        }
    }

    // SAFETY: STARTUPINFOEXW is plain data; zero is a valid initial state.
    let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = std_handles[0];
    startup.StartupInfo.hStdOutput = std_handles[1];
    startup.StartupInfo.hStdError = std_handles[2];
    startup.lpAttributeList = attributes.ptr();

    // SAFETY: PROCESS_INFORMATION is plain data.
    let mut info: PROCESS_INFORMATION = unsafe { zeroed() };
    let inherit: BOOL = if handle_list.is_empty() { 0 } else { 1 };
    // SAFETY: every pointer is valid for the call; the command line buffer is mutable and
    // NUL-terminated as CreateProcessW requires.
    let created = unsafe {
        CreateProcessW(
            null(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            inherit,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
            environment
                .as_ref()
                .map_or(null(), |block| block.as_ptr().cast()),
            cwd.as_ptr(),
            &startup.StartupInfo,
            &mut info,
        )
    };
    if created == 0 {
        let program = spec.command[0].to_string_lossy();
        return Err(last_error(&format!("starting {program} in the sandbox")));
    }
    let process = owned(info.hProcess);
    let thread_handle = owned(info.hThread);
    let mut guard = KillOnDrop(raw(&process), true);

    // This process's copies of the pipe write ends must close, or the readers never see EOF.
    let out_reader = out_pipe.take().map(|(read, _write)| drain(read));
    let err_reader = err_pipe.take().map(|(read, _write)| drain(read));
    drop(null_in.take());

    let job = assign_to_job(spec, raw(&process))?;
    // SAFETY: the thread handle is valid; the process is suspended and inside the job.
    if unsafe { ResumeThread(raw(&thread_handle)) } == u32::MAX {
        return Err(last_error("resuming the sandboxed process"));
    }
    guard.1 = false;
    drop(thread_handle);

    let wait_ms = match spec.timeout {
        // INFINITE is u32::MAX, so clamp just below it.
        Some(t) => u32::try_from(t.as_millis())
            .unwrap_or(INFINITE - 1)
            .min(INFINITE - 1),
        None => INFINITE,
    };
    let mut timed_out = false;
    // SAFETY: valid handles throughout.
    let exit_code = unsafe {
        match WaitForSingleObject(raw(&process), wait_ms) {
            WAIT_OBJECT_0 => {}
            WAIT_TIMEOUT => {
                timed_out = true;
                TerminateJobObject(raw(&job), 1);
                WaitForSingleObject(raw(&process), INFINITE);
            }
            WAIT_FAILED => return Err(last_error("waiting for the sandboxed process")),
            other => return Err(io::Error::other(format!("unexpected wait result {other}"))),
        }
        let mut code = 0u32;
        if GetExitCodeProcess(raw(&process), &mut code) == 0 {
            return Err(last_error("reading the exit code"));
        }
        // The sandbox ends with its main process: kill stragglers before the ACEs are revoked
        // (and so that the captured pipes reach end-of-file).
        TerminateJobObject(raw(&job), 1);
        code
    };
    drop(job);

    Ok(Output {
        exit_code: exit_code as i32,
        stdout: out_reader
            .map(|r| r.join().unwrap_or_default())
            .unwrap_or_default(),
        stderr: err_reader
            .map(|r| r.join().unwrap_or_default())
            .unwrap_or_default(),
        timed_out,
    })
}

pub(crate) fn run(spec: &SandboxSpec, stdio: Stdio) -> io::Result<Output> {
    let trace = std::env::var_os(TRACE_ENV).is_some_and(|v| !v.is_empty());
    let started = Instant::now();
    let session = Session::begin(spec)?;
    let setup = started.elapsed();
    let result = launch(spec, stdio, session.sid());
    let ran = started.elapsed();
    drop(session);
    if trace {
        let teardown = started.elapsed() - ran;
        eprintln!(
            "present-sandbox: trace: setup {} ms (stale cleanup, profile, access entries), child {} ms, teardown {} ms",
            setup.as_millis(),
            (ran - setup).as_millis(),
            teardown.as_millis()
        );
    }
    result
}

unsafe extern "system" fn ctrl_handler(_ctrl_type: u32) -> BOOL {
    // "Handled": the default handler (ExitProcess) does not run. The child shares the console
    // and receives the same event; the helper exits after it, once cleanup is done.
    1
}

pub(crate) fn install_ctrl_handler() {
    // SAFETY: the handler is a valid function for the life of the process.
    unsafe { SetConsoleCtrlHandler(Some(ctrl_handler), 1) };
}

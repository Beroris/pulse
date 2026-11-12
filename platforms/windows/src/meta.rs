//! Process metadata that never changes for a process's life: path, owner, start time, and the
//! executable's description, publisher and icon. Runs on a low-priority background thread.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tm_core::model::ProcKey;
use tm_core::platform::{FileInfo, Identity, MetaResolver};
use windows::Win32::Foundation::{FILETIME, HANDLE};
use windows::Win32::Security::{GetTokenInformation, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, QueryDosDeviceW, VerQueryValueW};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::RemoteDesktop::{WTS_PROCESS_INFO_EXW, WTSEnumerateProcessesExW, WTSFreeMemoryExW, WTSTypeProcessInfoLevel1};
use windows::Win32::System::Threading::*;
use windows::core::{PCWSTR, PWSTR};

use crate::sampler::account_name;
use crate::util::{Owned, filetime, from_wide, local_time, wide};

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn NtQuerySystemInformation(class: i32, info: *mut core::ffi::c_void, len: u32, ret: *mut u32) -> i32;
}

/// SYSTEM_PROCESS_ID_INFORMATION: asks for one process's image path by PID, without opening it.
#[repr(C)]
struct ProcessIdInfo {
    pid: usize,
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

const SYSTEM_PROCESS_ID_INFORMATION: i32 = 88;
const KERNEL_NAMES: [&str; 5] = ["Registry", "Memory Compression", "Secure System", "System Interrupts", "vmmem"];

pub struct WinResolver {
    com: bool,
    devices: Vec<(String, String)>,
    devices_at: Option<Instant>,
    windows_dir: String,
    wts: HashMap<u32, Option<Arc<str>>>,
    wts_at: Option<Instant>,
    sids: HashMap<Vec<u8>, Option<Arc<str>>>,
    path_buf: Vec<u16>,
    svc_accounts: HashMap<u32, Arc<str>>,
    svc_at: Option<Instant>,
}

impl WinResolver {
    pub fn new() -> WinResolver {
        WinResolver {
            com: false,
            devices: Vec::new(),
            devices_at: None,
            windows_dir: crate::util::windows_dir(),
            wts: HashMap::new(),
            wts_at: None,
            sids: HashMap::new(),
            path_buf: vec![0u16; 1024],
            svc_accounts: HashMap::new(),
            svc_at: None,
        }
    }

    /// "\Device\HarddiskVolume3" → "C:" for every drive letter.
    fn refresh_devices(&mut self) {
        if self.devices_at.is_some_and(|t| t.elapsed().as_secs() < 5) {
            return;
        }
        self.devices_at = Some(Instant::now());
        self.devices.clear();
        let mut buf = [0u16; 512];
        for l in b'A'..=b'Z' {
            let drive = format!("{}:", l as char);
            let d = wide(&drive);
            let n = unsafe { QueryDosDeviceW(PCWSTR(d.as_ptr()), Some(&mut buf)) };
            if n > 0 {
                self.devices.push((from_wide(&buf), drive));
            }
        }
    }

    fn dos_path(&mut self, nt: &str) -> Option<String> {
        for attempt in 0..2 {
            if attempt == 1 || self.devices.is_empty() {
                self.refresh_devices();
            }
            for (dev, drive) in &self.devices {
                if let Some(rest) = nt.strip_prefix(dev.as_str()) {
                    if rest.starts_with('\\') {
                        return Some(format!("{drive}{rest}"));
                    }
                }
            }
        }
        // Not on a lettered volume (\Device\Mup is a network share).
        nt.strip_prefix(r"\Device\Mup").map(|r| format!(r"\{r}"))
    }

    /// The image path of any process, by PID, without opening it.
    pub fn image_path_of(&mut self, pid: u32) -> Option<String> {
        self.image_path(pid)
    }

    /// The image path of any process, by PID.
    fn image_path(&mut self, pid: u32) -> Option<String> {
        for _ in 0..2 {
            let mut info =
                ProcessIdInfo { pid: pid as usize, length: 0, maximum_length: (self.path_buf.len() * 2) as u16, buffer: self.path_buf.as_mut_ptr() };
            let status = unsafe {
                NtQuerySystemInformation(SYSTEM_PROCESS_ID_INFORMATION, &mut info as *mut _ as *mut _, std::mem::size_of::<ProcessIdInfo>() as u32, std::ptr::null_mut())
            };
            if status == 0xC000_0004_u32 as i32 && (info.maximum_length as usize) > self.path_buf.len() * 2 {
                self.path_buf = vec![0u16; info.maximum_length as usize / 2 + 1];
                continue;
            }
            if status < 0 || info.length == 0 {
                return None;
            }
            let nt = String::from_utf16_lossy(&self.path_buf[..info.length as usize / 2]);
            return self.dos_path(&nt);
        }
        None
    }

    fn token_user(&mut self, h: HANDLE) -> Option<Arc<str>> {
        let mut token = HANDLE::default();
        unsafe { OpenProcessToken(h, TOKEN_QUERY, &mut token) }.ok()?;
        let token = Owned(token);
        let mut buf = [0u64; 64];
        let mut len = 0u32;
        unsafe { GetTokenInformation(token.get(), TokenUser, Some(buf.as_mut_ptr() as *mut _), 512, &mut len) }.ok()?;
        let tu = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
        self.sid_name(tu.User.Sid)
    }

    fn sid_name(&mut self, sid: PSID) -> Option<Arc<str>> {
        if sid.0.is_null() {
            return None;
        }
        let p = sid.0 as *const u8;
        let count = unsafe { *p.add(1) } as usize;
        if count > 15 {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(p, 8 + 4 * count) }.to_vec();
        if let Some(n) = self.sids.get(&bytes) {
            return n.clone();
        }
        let name = account_name(sid).map(Arc::from);
        self.sids.insert(bytes, name.clone());
        name
    }

    /// Owners from the session service, for processes this app can't open.
    fn wts_user(&mut self, pid: u32) -> Option<Arc<str>> {
        let fresh = self.wts_at.is_some_and(|t| t.elapsed().as_millis() < 1500);
        if !fresh && (!self.wts.contains_key(&pid) || self.wts_at.is_none_or(|t| t.elapsed().as_secs() > 10)) {
            self.wts_at = Some(Instant::now());
            let mut level = 1u32;
            let mut list = PWSTR::null();
            let mut count = 0u32;
            // WTS_ANY_SESSION
            if unsafe { WTSEnumerateProcessesExW(None, &mut level, 0xFFFF_FFFE, &mut list, &mut count) }.is_ok() {
                let items = unsafe { std::slice::from_raw_parts(list.0 as *const WTS_PROCESS_INFO_EXW, count as usize) };
                self.wts.clear();
                let found: Vec<(u32, PSID)> = items.iter().map(|i| (i.ProcessId, i.pUserSid)).collect();
                for (p, sid) in found {
                    let name = self.sid_name(sid);
                    self.wts.insert(p, name);
                }
                unsafe {
                    let _ = WTSFreeMemoryExW(WTSTypeProcessInfoLevel1, list.0 as *const _, count);
                }
            }
        }
        self.wts.get(&pid).cloned().flatten()
    }
}

impl WinResolver {
    /// For service hosts: the account their services are configured to run as.
    fn service_account(&mut self, pid: u32) -> Option<Arc<str>> {
        if !self.svc_accounts.contains_key(&pid) && self.svc_at.is_none_or(|t| t.elapsed().as_secs() >= 3) {
            self.svc_at = Some(Instant::now());
            self.svc_accounts = crate::services::process_accounts().into_iter().map(|(p, a)| (p, Arc::from(a))).collect();
        }
        self.svc_accounts.get(&pid).cloned()
    }
}

impl Default for WinResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl MetaResolver for WinResolver {
    fn identify(&mut self, key: ProcKey, name: &str) -> Identity {
        let mut id = Identity { started: local_time(key.created).map(Arc::from), ..Default::default() };
        if key.pid == 0 || key.pid == 4 || KERNEL_NAMES.iter().any(|k| k.eq_ignore_ascii_case(name)) {
            id.system = true;
            id.user = Some("SYSTEM".into());
            return id;
        }
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, key.pid) }.ok().map(Owned);
        // Check it's still the same process before trusting anything read through the handle.
        let same = handle.as_ref().is_none_or(|h| {
            let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
            unsafe { GetProcessTimes(h.get(), &mut c, &mut e, &mut k, &mut u) }.is_ok() && (key.created == 0 || filetime(c) == key.created)
        });
        if !same {
            return id;
        }
        let mut path = None;
        if let Some(h) = &handle {
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            if unsafe { QueryFullProcessImageNameW(h.get(), PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.is_ok() {
                path = Some(String::from_utf16_lossy(&buf[..len as usize]));
            }
            id.user = self.token_user(h.get());
        }
        if path.is_none() {
            path = self.image_path(key.pid);
        }
        if id.user.is_none() {
            id.user = self.wts_user(key.pid);
        }
        if id.user.is_none() {
            id.user = self.service_account(key.pid);
        }
        if let Some(p) = &path {
            id.system = p.to_lowercase().starts_with(&self.windows_dir);
        }
        id.path = path.map(Arc::from);
        id
    }

    fn file_info(&mut self, path: &str) -> FileInfo {
        if !self.com {
            // Icon extraction can load shell handlers, which want COM.
            let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            self.com = true;
        }
        let v = version_info(path);
        FileInfo {
            description: v.description.map(Arc::from),
            company: v.company.map(Arc::from),
            version: v.version.map(Arc::from),
            icon: crate::icon::file_icon(path, 0, 32),
        }
    }
}

#[derive(Default)]
pub struct Version {
    pub description: Option<String>,
    pub company: Option<String>,
    pub version: Option<String>,
    pub product: Option<String>,
}

/// The version resource's description, company and version.
pub fn version_info(path: &str) -> Version {
    let p = wide(path);
    let mut out = Version::default();
    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(p.as_ptr()), None) };
    if size == 0 {
        return out;
    }
    let mut data = vec![0u8; size as usize];
    if unsafe { GetFileVersionInfoW(PCWSTR(p.as_ptr()), None, size, data.as_mut_ptr() as *mut _) }.is_err() {
        return out;
    }
    let query = |sub: &str| -> Option<(*const u8, u32)> {
        let s = wide(sub);
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        unsafe { VerQueryValueW(data.as_ptr() as *const _, PCWSTR(s.as_ptr()), &mut ptr, &mut len) }.as_bool().then_some((ptr as *const u8, len))
    };
    // Try the file's own languages first, then common fallbacks.
    let mut langs: Vec<String> = Vec::new();
    if let Some((ptr, len)) = query(r"\VarFileInfo\Translation") {
        for i in 0..(len as usize / 4) {
            let (lang, cp) = unsafe { (*(ptr as *const u16).add(i * 2), *(ptr as *const u16).add(i * 2 + 1)) };
            langs.push(format!("{lang:04x}{cp:04x}"));
        }
    }
    langs.extend(["040904b0".into(), "040904e4".into(), "04090000".into()]);
    let string = |name: &str| -> Option<String> {
        for l in &langs {
            if let Some((ptr, len)) = query(&format!(r"\StringFileInfo\{l}\{name}")) {
                if len > 0 && !ptr.is_null() {
                    let s = unsafe { std::slice::from_raw_parts(ptr as *const u16, len as usize) };
                    let v = from_wide(s).trim().to_string();
                    if !v.is_empty() {
                        return Some(v);
                    }
                }
            }
        }
        None
    };
    out.description = string("FileDescription");
    out.company = string("CompanyName");
    out.version = string("FileVersion").map(|v| v.split_whitespace().next().unwrap_or(&v).to_string());
    out.product = string("ProductName");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_itself_and_a_system_process() {
        let mut r = WinResolver::new();
        let me = std::process::id();
        let id = r.identify(ProcKey { pid: me, created: 0 }, "x");
        let path = id.path.expect("own path");
        assert!(path.to_lowercase().ends_with(".exe"), "{path}");
        assert!(id.user.is_some());
        // An svchost: owned by another account, path readable without opening it.
        let mut snap = crate::sampler::Snapshot::new();
        let (list, _) = snap.sample().unwrap();
        let svc = list.iter().find(|p| &*p.name == "svchost.exe").unwrap();
        let id = r.identify(ProcKey { pid: svc.pid, created: svc.created }, "svchost.exe");
        eprintln!("svchost {:?} {:?} system={}", id.path, id.user, id.system);
        assert!(id.path.as_deref().is_some_and(|p| p.to_lowercase().ends_with(r"system32\svchost.exe")));
        assert!(id.system);
        let lsass = list.iter().find(|p| &*p.name == "lsass.exe").unwrap();
        let id = r.identify(ProcKey { pid: lsass.pid, created: lsass.created }, "lsass.exe");
        eprintln!("lsass {:?} {:?}", id.path, id.user);
    }

    #[test]
    fn reads_version_info() {
        let v = version_info(&format!("{}explorer.exe", crate::util::windows_dir()));
        assert!(v.company.as_deref().is_some_and(|c| c.contains("Microsoft")));
        assert!(v.description.is_some());
    }
}


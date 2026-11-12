//! Small Win32 helpers: wide strings, owned handles, the registry, folders, time.

use std::path::PathBuf;

use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, FILETIME, HANDLE, SYSTEMTIME};
use windows::Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx, DATE_SHORTDATE, TIME_FORMAT_FLAGS};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KNOWN_FOLDER_FLAG};
use windows::core::{GUID, PCWSTR};

pub const APP_NAME: &str = "Bero Task Manager";
/// The value the app's "Start when I sign in" switch and the installer both write.
pub const RUN_VALUE: &str = "Bero Task Manager";
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Held while the app runs, so the installer and a second launch can tell.
pub const MUTEX_NAME: &str = r"Local\BeroTaskManager.Instance";
/// Class of the hidden window a second launch talks to.
pub const IPC_CLASS: &str = "BeroTaskManager.Ipc";

/// Null-terminated UTF-16.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// UTF-16 up to the first null.
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Reads a null-terminated UTF-16 string from a raw pointer.
///
/// # Safety
/// `p` must be null or point at a null-terminated UTF-16 string.
pub unsafe fn from_pwstr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    unsafe {
        while *p.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
}

/// A handle closed on drop.
pub struct Owned(pub HANDLE);

impl Owned {
    pub fn get(&self) -> HANDLE {
        self.0
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_invalid() && !self.0.0.is_null() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

unsafe impl Send for Owned {}

pub fn filetime(ft: FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

/// A FILETIME (100 ns since 1601, UTC) as a short local date and time in the user's format.
pub fn local_time(ft: u64) -> Option<String> {
    if ft == 0 {
        return None;
    }
    let ft = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: (ft >> 32) as u32 };
    unsafe {
        let mut utc = SYSTEMTIME::default();
        FileTimeToSystemTime(&ft, &mut utc).ok()?;
        let mut local = SYSTEMTIME::default();
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
        let mut date = [0u16; 64];
        let mut time = [0u16; 64];
        let dn = GetDateFormatEx(PCWSTR::null(), DATE_SHORTDATE, Some(&local), PCWSTR::null(), Some(&mut date), PCWSTR::null());
        let tn = GetTimeFormatEx(PCWSTR::null(), TIME_FORMAT_FLAGS(0), Some(&local), PCWSTR::null(), Some(&mut time));
        if dn == 0 || tn == 0 {
            return None;
        }
        Some(format!("{} {}", from_wide(&date), from_wide(&time)))
    }
}

// ─── Registry ──────────────────────────────────────────────────────────────

pub struct RegKey(pub HKEY);

impl Drop for RegKey {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

pub enum RegValue {
    Str(String),
    ExpandStr(String),
    Dword(u32),
    Binary(Vec<u8>),
    Other,
}

impl RegKey {
    pub fn open(root: HKEY, path: &str, access: REG_SAM_FLAGS) -> Option<RegKey> {
        let mut key = HKEY::default();
        let p = wide(path);
        let r = unsafe { RegOpenKeyExW(root, PCWSTR(p.as_ptr()), Some(0), access, &mut key) };
        (r == ERROR_SUCCESS).then_some(RegKey(key))
    }

    pub fn create(root: HKEY, path: &str) -> Option<RegKey> {
        let mut key = HKEY::default();
        let p = wide(path);
        let r = unsafe { RegCreateKeyExW(root, PCWSTR(p.as_ptr()), Some(0), PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_READ | KEY_WRITE, None, &mut key, None) };
        (r == ERROR_SUCCESS).then_some(RegKey(key))
    }

    pub fn query(&self, name: &str) -> Option<RegValue> {
        let n = wide(name);
        let mut ty = REG_VALUE_TYPE::default();
        let mut len = 0u32;
        unsafe {
            if RegQueryValueExW(self.0, PCWSTR(n.as_ptr()), None, Some(&mut ty), None, Some(&mut len)) != ERROR_SUCCESS {
                return None;
            }
            let mut buf = vec![0u8; len as usize + 2];
            if RegQueryValueExW(self.0, PCWSTR(n.as_ptr()), None, Some(&mut ty), Some(buf.as_mut_ptr()), Some(&mut len)) != ERROR_SUCCESS {
                return None;
            }
            buf.truncate(len as usize);
            Some(decode(ty, buf))
        }
    }

    pub fn string(&self, name: &str) -> Option<String> {
        match self.query(name)? {
            RegValue::Str(s) => Some(s),
            RegValue::ExpandStr(s) => Some(expand_env(&s)),
            _ => None,
        }
    }

    pub fn dword(&self, name: &str) -> Option<u32> {
        match self.query(name)? {
            RegValue::Dword(v) => Some(v),
            _ => None,
        }
    }

    /// Every value as (name, value).
    pub fn values(&self) -> Vec<(String, RegValue)> {
        let mut out = Vec::new();
        let mut i = 0u32;
        loop {
            let mut name = vec![0u16; 16_384];
            let mut name_len = name.len() as u32;
            let mut ty = 0u32;
            let mut data_len = 0u32;
            let r = unsafe { RegEnumValueW(self.0, i, Some(windows::core::PWSTR(name.as_mut_ptr())), &mut name_len, None, Some(&mut ty), None, Some(&mut data_len)) };
            if r != ERROR_SUCCESS {
                break;
            }
            let mut data = vec![0u8; data_len as usize + 2];
            let mut name_len2 = name.len() as u32;
            let r = unsafe {
                RegEnumValueW(self.0, i, Some(windows::core::PWSTR(name.as_mut_ptr())), &mut name_len2, None, Some(&mut ty), Some(data.as_mut_ptr()), Some(&mut data_len))
            };
            i += 1;
            if r != ERROR_SUCCESS {
                continue;
            }
            data.truncate(data_len as usize);
            out.push((String::from_utf16_lossy(&name[..name_len2 as usize]), decode(REG_VALUE_TYPE(ty), data)));
        }
        out
    }

    /// Subkey names.
    pub fn subkeys(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut i = 0u32;
        loop {
            let mut name = vec![0u16; 512];
            let mut len = name.len() as u32;
            let r = unsafe { RegEnumKeyExW(self.0, i, Some(windows::core::PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None) };
            if r != ERROR_SUCCESS {
                break;
            }
            out.push(String::from_utf16_lossy(&name[..len as usize]));
            i += 1;
        }
        out
    }

    pub fn set_string(&self, name: &str, value: &str) -> bool {
        let n = wide(name);
        let v = wide(value);
        let bytes = unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 2) };
        unsafe { RegSetValueExW(self.0, PCWSTR(n.as_ptr()), None, REG_SZ, Some(bytes)) == ERROR_SUCCESS }
    }

    pub fn set_binary(&self, name: &str, value: &[u8]) -> windows::Win32::Foundation::WIN32_ERROR {
        let n = wide(name);
        unsafe { RegSetValueExW(self.0, PCWSTR(n.as_ptr()), None, REG_BINARY, Some(value)) }
    }

    pub fn set_dword(&self, name: &str, value: u32) -> windows::Win32::Foundation::WIN32_ERROR {
        let n = wide(name);
        unsafe { RegSetValueExW(self.0, PCWSTR(n.as_ptr()), None, REG_DWORD, Some(&value.to_le_bytes())) }
    }

    pub fn delete_value(&self, name: &str) -> bool {
        let n = wide(name);
        unsafe { RegDeleteValueW(self.0, PCWSTR(n.as_ptr())) == ERROR_SUCCESS }
    }
}

fn decode(ty: REG_VALUE_TYPE, buf: Vec<u8>) -> RegValue {
    let as_str = |b: &[u8]| {
        let w: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        from_wide(&w)
    };
    match ty {
        REG_SZ => RegValue::Str(as_str(&buf)),
        REG_EXPAND_SZ => RegValue::ExpandStr(as_str(&buf)),
        REG_DWORD if buf.len() >= 4 => RegValue::Dword(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]])),
        REG_BINARY => RegValue::Binary(buf),
        _ => RegValue::Other,
    }
}

pub fn expand_env(s: &str) -> String {
    use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
    if !s.contains('%') {
        return s.to_string();
    }
    let src = wide(s);
    let mut buf = vec![0u16; 4096];
    let n = unsafe { ExpandEnvironmentStringsW(PCWSTR(src.as_ptr()), Some(&mut buf)) };
    if n == 0 || n as usize > buf.len() {
        return s.to_string();
    }
    from_wide(&buf)
}

// ─── Folders and paths ─────────────────────────────────────────────────────

pub fn known_folder(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KNOWN_FOLDER_FLAG(0), None).ok()?;
        let s = p.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

/// The Windows folder, lower-case, with a trailing backslash: "c:\windows\".
pub fn windows_dir() -> String {
    let w = std::env::var("SystemRoot").or_else(|_| std::env::var("windir")).unwrap_or_else(|_| "C:\\Windows".into());
    let mut w = w.to_lowercase();
    if !w.ends_with('\\') {
        w.push('\\');
    }
    w
}

/// A copy whose file name contains "portable" keeps its settings beside itself.
pub fn is_portable() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_stem().map(|s| s.to_string_lossy().to_ascii_lowercase()))
        .is_some_and(|stem| stem.contains("portable"))
}

/// Where settings live, created if needed: a "Bero Task Manager settings" folder beside a portable
/// copy, otherwise `%APPDATA%\Bero Task Manager`.
pub fn data_dir() -> PathBuf {
    if is_portable() {
        if let Some(beside) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|p| p.join(format!("{APP_NAME} settings")))) {
            if std::fs::create_dir_all(&beside).is_ok() {
                return beside;
            }
        }
    }
    let base = known_folder(&windows::Win32::UI::Shell::FOLDERID_RoamingAppData).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(APP_NAME);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn exe_path() -> String {
    std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Whether this process runs elevated (as administrator).
pub fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let token = Owned(token);
        let mut e = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        GetTokenInformation(token.get(), TokenElevation, Some(&mut e as *mut _ as *mut _), std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut len).is_ok() && e.TokenIsElevated != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_round_trip() {
        let w = wide("Bero ✓");
        assert_eq!(*w.last().unwrap(), 0);
        assert_eq!(from_wide(&w), "Bero ✓");
    }

    #[test]
    fn expands_environment() {
        let s = expand_env("%SystemRoot%\\System32");
        assert!(!s.contains('%'), "{s}");
    }

    #[test]
    fn local_time_formats() {
        // 2024-01-01 00:00 UTC.
        assert!(local_time(133_485_408_000_000_000).is_some());
        assert_eq!(local_time(0), None);
    }
}

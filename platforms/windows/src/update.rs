//! Asking GitHub for new versions of Pulse, downloading them, and installing them.
//!
//! - The installed copy runs the new Setup in its update mode, with no window. Setup closes
//!   Pulse, replaces the exe, leaves the user's choices alone and starts the new version.
//! - The portable copy swaps its own exe for the new one under the same name, so shortcuts to
//!   it keep working, and starts it.
//! - Any other copy, such as a build from source, gets a link to the release page instead.
//!
//! The check asks GitHub for the latest release and nothing else. Downloads come only from
//! github.com, and must match the size and SHA-256 GitHub lists. When to look is the app's
//! business (see `tm_app`); everything here blocks and runs on its worker thread.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tm_core::update::{self as feed, Download, Kind, Release, UpdateHost};
use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_OPTION_ENABLE_HTTP_PROTOCOL, WINHTTP_PROTOCOL_FLAG_HTTP2, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts,
};
use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, KEY_READ};
use windows::core::PCWSTR;

use crate::util::{self, RegKey, wide};

/// The version this copy compares against. A test build can pretend to be older.
const CURRENT: &str = match option_env!("PULSE_TEST_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};
/// For testing the whole path without publishing: a local copy of GitHub's answer, whose files'
/// addresses start `https://github.com/local/` and are read from beside it. Never set for a
/// release.
const TEST_FEED: Option<&str> = option_env!("PULSE_TEST_FEED");

/// The installer's AppId, which Windows lists the installed app under.
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{33E6D0C0-1277-4F01-9D80-759B7C8AAED9}_is1";
/// The installed copy's download waits here, in Windows' temp folder.
const DOWNLOAD_FOLDER: &str = "Pulse update";
/// Passed to the new portable copy with the old one's process id, so it waits for it to go.
const AFTER_UPDATE_ARG: &str = "--after-update";
/// Passed by Setup when it starts Pulse again after an update.
const UPDATED_ARG: &str = "--updated";

pub struct WinUpdates {
    kind: Kind,
}

impl WinUpdates {
    pub fn new() -> WinUpdates {
        WinUpdates { kind: this_copy() }
    }
}

impl UpdateHost for WinUpdates {
    fn kind(&self) -> Kind {
        self.kind
    }

    fn check(&self) -> Result<Option<Release>, String> {
        let body = get(feed::LATEST_RELEASE, true, 1 << 20)?;
        feed::parse_release(&body, self.kind, CURRENT)
    }

    fn download(&self, d: &Download) -> Result<PathBuf, String> {
        fetch(self.kind, d)
    }

    fn install(&self, file: &Path) -> Result<(), String> {
        start_install(self.kind, file)
    }

    fn clean_up(&self) {
        clean_up();
    }
}

// ─── Downloading ───────────────────────────────────────────────────────────

/// Downloads `d`, checks it, and puts it where this kind of copy installs it from.
fn fetch(kind: Kind, d: &Download) -> Result<PathBuf, String> {
    let destination = download_path(kind, d)?;
    let limit = if d.size > 0 { d.size as usize } else { 64 << 20 };
    let body = get(&d.url, false, limit).map_err(|e| if e.starts_with("Couldn't reach") { "Couldn't download the update. Check your connection.".to_string() } else { e })?;
    if d.size > 0 && body.len() as u64 != d.size {
        return Err("The download was cut short. Try again.".into());
    }
    if let Some(sha) = &d.sha256 {
        if sha256_hex(&body).as_deref() != Some(sha.as_str()) {
            return Err("The download didn't match the release. Try again.".into());
        }
    }
    if !body.starts_with(b"MZ") {
        return Err("The download wasn't a program. Try again.".into());
    }
    let cant_save = |folder: &Path| format!("Couldn't save the update in {}.", folder.display());
    let folder = destination.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(folder).map_err(|_| cant_save(folder))?;
    let partial = sibling(&destination, ".part");
    std::fs::write(&partial, &body).map_err(|_| cant_save(folder))?;
    let _ = std::fs::remove_file(&destination);
    if std::fs::rename(&partial, &destination).is_err() {
        let _ = std::fs::remove_file(&partial);
        return Err(cant_save(folder));
    }
    Ok(destination)
}

fn sha256_hex(data: &[u8]) -> Option<String> {
    let mut out = [0u8; 32];
    let status = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out) };
    status.is_ok().then(|| out.iter().map(|b| format!("{b:02x}")).collect())
}

/// Closes a WinHTTP handle when it goes.
struct Net(*mut c_void);

impl Drop for Net {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// GETs `url` over HTTPS, following redirects, with no more than `limit` bytes back.
fn get(url: &str, api: bool, limit: usize) -> Result<Vec<u8>, String> {
    if let Some(feed_file) = TEST_FEED {
        return test_get(feed_file, url);
    }
    let offline = || "Couldn't reach GitHub. Check your connection.".to_string();
    let rest = url.strip_prefix("https://").ok_or_else(offline)?;
    let (host, path) = match rest.split_once('/') {
        Some((host, path)) => (host, format!("/{path}")),
        None => (rest, "/".to_string()),
    };
    unsafe {
        let agent = wide(&format!("Pulse/{}", env!("CARGO_PKG_VERSION")));
        let session = Net(WinHttpOpen(PCWSTR(agent.as_ptr()), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if session.0.is_null() {
            return Err(offline());
        }
        // Each step gives up rather than wait for ever: resolving, connecting, sending, and
        // each read.
        let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000);
        let _ = WinHttpSetOption(Some(session.0), WINHTTP_OPTION_ENABLE_HTTP_PROTOCOL, Some(&WINHTTP_PROTOCOL_FLAG_HTTP2.to_le_bytes()));
        let host = wide(host);
        let connection = Net(WinHttpConnect(session.0, PCWSTR(host.as_ptr()), 443, 0));
        if connection.0.is_null() {
            return Err(offline());
        }
        let (verb, path) = (wide("GET"), wide(&path));
        let request = Net(WinHttpOpenRequest(connection.0, PCWSTR(verb.as_ptr()), PCWSTR(path.as_ptr()), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE));
        if request.0.is_null() {
            return Err(offline());
        }
        let headers: Vec<u16> = if api { "Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2022-11-28\r\n" } else { "" }.encode_utf16().collect();
        WinHttpSendRequest(request.0, (!headers.is_empty()).then_some(&headers[..]), None, 0, 0, 0).map_err(|_| offline())?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|_| offline())?;
        let (mut status, mut size) = (0u32, 4u32);
        WinHttpQueryHeaders(request.0, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut status as *mut u32 as *mut c_void), &mut size, std::ptr::null_mut())
            .map_err(|_| offline())?;
        match status {
            200 => {}
            404 => return Err("Couldn't find Pulse's releases on GitHub.".into()),
            403 | 429 => return Err("GitHub is busy. Try again later.".into()),
            other => return Err(format!("GitHub answered {other}. Try again later.")),
        }
        let mut body = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            WinHttpReadData(request.0, chunk.as_mut_ptr() as *mut c_void, chunk.len() as u32, &mut read).map_err(|_| "The download stopped. Check your connection and try again.".to_string())?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read as usize]);
            if body.len() > limit {
                return Err("GitHub sent more than the release lists. Try again later.".into());
            }
        }
        Ok(body)
    }
}

/// [`get`] for a test build: GitHub's answer and the files come from a folder.
fn test_get(feed_file: &str, url: &str) -> Result<Vec<u8>, String> {
    std::thread::sleep(Duration::from_millis(300));
    let path = if url == feed::LATEST_RELEASE {
        PathBuf::from(feed_file)
    } else {
        let name = url.strip_prefix("https://github.com/local/").filter(|n| feed::is_plain_file_name(n)).ok_or("Not a test file.")?;
        Path::new(feed_file).with_file_name(name)
    };
    std::fs::read(path).map_err(|_| "Couldn't reach GitHub. Check your connection.".to_string())
}

// ─── Installing ────────────────────────────────────────────────────────────

/// Where this kind of copy keeps the new version until it's installed.
fn download_path(kind: Kind, d: &Download) -> Result<PathBuf, String> {
    match kind {
        Kind::Installed => Ok(std::env::temp_dir().join(DOWNLOAD_FOLDER).join(&d.name)),
        // Beside the exe, so moving it into place can't fail for being on another drive.
        Kind::Portable => std::env::current_exe().map(|exe| sibling(&exe, ".new")).map_err(|_| "Couldn't find where Pulse is.".to_string()),
        Kind::Other => Err("This copy of Pulse can't update itself.".into()),
    }
}

/// `path` with `suffix` added to its whole file name.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Starts installing the downloaded version. Pulse quits straight after.
fn start_install(kind: Kind, file: &Path) -> Result<(), String> {
    match kind {
        Kind::Installed => {
            // Setup's update mode keeps the user's choices as they are, shows nothing, and
            // starts Pulse again when it's done.
            std::process::Command::new(file)
                .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/UPDATE"])
                .spawn()
                .map_err(|_| "Couldn't start the installer. Try again.".to_string())?;
        }
        Kind::Portable => {
            let exe = std::env::current_exe().map_err(|_| "Couldn't find where Pulse is.".to_string())?;
            swap_in(&exe, file)?;
            if std::process::Command::new(&exe).args([AFTER_UPDATE_ARG, &std::process::id().to_string()]).spawn().is_err() {
                // The new version is in place, so opening Pulse again starts it.
                return Err("Updated. Open Pulse again to start the new version.".into());
            }
        }
        Kind::Other => return Err("This copy of Pulse can't update itself.".into()),
    }
    Ok(())
}

/// Puts `new` where `exe` is, keeping the old one as "….old" until it has quit. Windows lets a
/// running exe be renamed, though not deleted.
fn swap_in(exe: &Path, new: &Path) -> Result<(), String> {
    let old = sibling(exe, ".old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).map_err(|_| "Couldn't make room for the new version.".to_string())?;
    if std::fs::rename(new, exe).is_err() {
        let _ = std::fs::rename(&old, exe);
        return Err("Couldn't put the new version in place.".into());
    }
    Ok(())
}

/// Run first thing at startup, before Pulse checks it's the only copy. After a portable update
/// it waits for the old version to quit, so the new one isn't turned away as a second copy, and
/// removes it. Says whether this start follows an update.
pub fn finish_update(args: &[String]) -> bool {
    if let Some(at) = args.iter().position(|a| a == AFTER_UPDATE_ARG) {
        if let Some(pid) = args.get(at + 1).and_then(|id| id.parse::<u32>().ok()) {
            crate::control::wait_for_exit(pid, 10_000);
        }
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::fs::remove_file(sibling(&exe, ".old"));
        }
        return true;
    }
    args.iter().any(|a| a == UPDATED_ARG)
}

/// Clears what an update left: the old portable exe, a download that never finished, and the
/// Setup that ran.
fn clean_up() {
    if let Ok(exe) = std::env::current_exe() {
        for suffix in [".old", ".new", ".new.part"] {
            let leftover = sibling(&exe, suffix);
            if leftover.exists() {
                let _ = std::fs::remove_file(leftover);
            }
        }
    }
    let folder = std::env::temp_dir().join(DOWNLOAD_FOLDER);
    if folder.exists() {
        let _ = std::fs::remove_dir_all(folder);
    }
}

/// How this copy updates: by the portable name, or by being where Setup installed Pulse.
fn this_copy() -> Kind {
    if util::is_portable() {
        return Kind::Portable;
    }
    let Some(installed) = RegKey::open(HKEY_CURRENT_USER, UNINSTALL_KEY, KEY_READ).and_then(|k| k.string("InstallLocation")) else { return Kind::Other };
    let same = |a: &Path, b: &Path| match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
        _ => false,
    };
    match std::env::current_exe().ok().as_deref().and_then(Path::parent) {
        Some(folder) if same(folder, Path::new(installed.trim())) => Kind::Installed,
        _ => Kind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_standard() {
        assert_eq!(sha256_hex(b"abc").unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(sha256_hex(b"").unwrap(), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    /// Asks the real GitHub for the latest release as a 0.9.0 copy would, downloads its Setup and
    /// checks it against the SHA-256 GitHub lists. Installs nothing.
    /// `cargo test -p tm-windows -- --ignored --nocapture downloads_the_latest_release`
    #[test]
    #[ignore]
    fn downloads_the_latest_release() {
        let body = get(feed::LATEST_RELEASE, true, 1 << 20).expect("asking GitHub");
        let release = feed::parse_release(&body, Kind::Installed, "0.9.0").expect("reading the answer").expect("a release newer than 0.9.0");
        let d = release.download.expect("a Setup file");
        println!("{} {} {} bytes, sha256 {:?}", release.version, d.name, d.size, d.sha256);
        assert!(d.sha256.is_some(), "GitHub lists a digest");
        let file = fetch(Kind::Installed, &d).expect("downloading");
        assert_eq!(std::fs::metadata(&file).unwrap().len(), d.size);
        clean_up();
        assert!(!file.exists());
    }

    #[test]
    fn a_portable_swap_keeps_the_name() {
        let dir = std::env::temp_dir().join(format!("pulse-swap-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("Pulse-1.0.0-Portable.exe");
        let new = sibling(&exe, ".new");
        std::fs::write(&exe, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();
        swap_in(&exe, &new).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::read(sibling(&exe, ".old")).unwrap(), b"old");
        assert!(!new.exists());
        // Nothing to swap in: the old one stays where it was.
        assert!(swap_in(&exe, &new).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

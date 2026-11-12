//! Programs Windows starts at sign-in, and switching them off and on again.
//!
//! Disabling never deletes anything. It sets the same "StartupApproved" flag that Windows Task
//! Manager and Settings → Apps → Startup use, so the change shows up there too and is undone by
//! enabling the entry in any of the three.
//!
//! Sources, in the order Windows itself lists them:
//! - the Run keys for the current user and for all users (64- and 32-bit),
//! - the current user's and the shared Startup folders,
//! - Store apps' startup tasks.

use std::path::Path;

use tm_core::platform::{ActionError, ActionResult, StartupEntry};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::Storage::FileSystem::{GetFileAttributesW, INVALID_FILE_ATTRIBUTES, WIN32_FIND_DATAW};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IPersistFile, STGM_READ};
use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::UI::Shell::{FOLDERID_CommonStartup, FOLDERID_Startup, IShellLinkW, SHLoadIndirectString, SLGP_RAWPATH, ShellLink};
use windows::core::{Interface, PCWSTR};

use crate::util::{RegKey, RegValue, expand_env, from_wide, is_elevated, known_folder, wide};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN32: &str = r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Run";
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved";
const APP_DATA: &str = r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\SystemAppData";
const PACKAGES: &str = r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    UserRun,
    MachineRun,
    MachineRun32,
    UserFolder,
    CommonFolder,
}

impl Source {
    fn tag(self) -> &'static str {
        match self {
            Source::UserRun => "user-run",
            Source::MachineRun => "machine-run",
            Source::MachineRun32 => "machine-run32",
            Source::UserFolder => "user-folder",
            Source::CommonFolder => "common-folder",
        }
    }
    fn from_tag(t: &str) -> Option<Source> {
        [Source::UserRun, Source::MachineRun, Source::MachineRun32, Source::UserFolder, Source::CommonFolder].into_iter().find(|s| s.tag() == t)
    }
    fn approved_key(self) -> (HKEY, String) {
        match self {
            Source::UserRun => (HKEY_CURRENT_USER, format!(r"{APPROVED}\Run")),
            Source::MachineRun => (HKEY_LOCAL_MACHINE, format!(r"{APPROVED}\Run")),
            Source::MachineRun32 => (HKEY_LOCAL_MACHINE, format!(r"{APPROVED}\Run32")),
            Source::UserFolder => (HKEY_CURRENT_USER, format!(r"{APPROVED}\StartupFolder")),
            Source::CommonFolder => (HKEY_LOCAL_MACHINE, format!(r"{APPROVED}\StartupFolder")),
        }
    }
    fn all_users(self) -> bool {
        matches!(self, Source::MachineRun | Source::MachineRun32 | Source::CommonFolder)
    }
    fn label(self) -> &'static str {
        match self {
            Source::UserRun => "Registry · you",
            Source::MachineRun => "Registry · all users",
            Source::MachineRun32 => "Registry · all users (32-bit)",
            Source::UserFolder => "Startup folder · you",
            Source::CommonFolder => "Startup folder · all users",
        }
    }
}

/// Whether Windows will start this entry: true unless its StartupApproved flag says disabled.
pub fn approved(source: Source, name: &str) -> bool {
    let (root, path) = source.approved_key();
    let Some(k) = RegKey::open(root, &path, KEY_READ) else { return true };
    match k.query(name) {
        // The first byte is even when enabled (2 or 6) and odd when disabled (3 or 7).
        Some(RegValue::Binary(b)) => b.first().is_none_or(|v| v % 2 == 0),
        _ => true,
    }
}

pub fn set_approved(source: Source, name: &str, enabled: bool) -> ActionResult {
    let (root, path) = source.approved_key();
    if !enabled || RegKey::open(root, &path, KEY_READ).is_some_and(|k| k.query(name).is_some()) {
        let k = RegKey::create(root, &path).ok_or_else(|| denied(source))?;
        // 12 bytes: a flag, three zero bytes, and when it was disabled (zero when enabled).
        let mut v = [0u8; 12];
        if enabled {
            v[0] = 2;
        } else {
            v[0] = 3;
            let ft: FILETIME = unsafe { GetSystemTimeAsFileTime() };
            v[4..8].copy_from_slice(&ft.dwLowDateTime.to_le_bytes());
            v[8..12].copy_from_slice(&ft.dwHighDateTime.to_le_bytes());
        }
        if k.set_binary(name, &v).0 != 0 {
            return Err(denied(source));
        }
    }
    Ok(())
}

fn denied(source: Source) -> ActionError {
    if source.all_users() && !is_elevated() {
        ActionError::NeedsElevation("This entry applies to all users, so changing it needs administrator rights.".into())
    } else {
        ActionError::Failed("Windows didn't allow the change.".into())
    }
}

/// The program a command line starts: the quoted part, or everything up to ".exe".
pub fn program_of(command: &str) -> Option<String> {
    let c = command.trim();
    let path = if let Some(rest) = c.strip_prefix('"') {
        rest.split('"').next()?.to_string()
    } else {
        let lower = c.to_ascii_lowercase();
        match lower.find(".exe") {
            Some(i) => c[..i + 4].to_string(),
            None => c.split_whitespace().next()?.to_string(),
        }
    };
    let p = expand_env(path.trim());
    (!p.is_empty()).then_some(p)
}

fn exists(path: &str) -> bool {
    let p = wide(path);
    unsafe { GetFileAttributesW(PCWSTR(p.as_ptr())) != INVALID_FILE_ATTRIBUTES }
}

/// Where a shortcut points, with its arguments.
fn resolve_link(path: &Path) -> Option<(String, String)> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        let p = wide(&path.display().to_string());
        file.Load(PCWSTR(p.as_ptr()), STGM_READ).ok()?;
        let mut target = [0u16; 1024];
        let mut fd = WIN32_FIND_DATAW::default();
        link.GetPath(&mut target, &mut fd, SLGP_RAWPATH.0 as u32).ok()?;
        let mut args = [0u16; 1024];
        let _ = link.GetArguments(&mut args);
        let t = expand_env(&from_wide(&target));
        (!t.is_empty()).then(|| (t, from_wide(&args)))
    }
}

fn describe(e: &mut StartupEntry) {
    let Some(path) = e.path.clone() else { return };
    e.missing = !exists(&path);
    if e.missing {
        return;
    }
    let v = crate::meta::version_info(&path);
    e.publisher = v.company;
    e.description = v.description;
    e.icon = crate::icon::file_icon(&path, 0, 32);
}

fn run_entries(out: &mut Vec<StartupEntry>, source: Source) {
    let (root, key) = match source {
        Source::UserRun => (HKEY_CURRENT_USER, RUN),
        Source::MachineRun => (HKEY_LOCAL_MACHINE, RUN),
        Source::MachineRun32 => (HKEY_LOCAL_MACHINE, RUN32),
        _ => return,
    };
    let Some(k) = RegKey::open(root, key, KEY_READ) else { return };
    let hive = if root == HKEY_CURRENT_USER { "HKCU" } else { "HKLM" };
    for (name, value) in k.values() {
        let command = match value {
            RegValue::Str(s) | RegValue::ExpandStr(s) => s,
            _ => continue,
        };
        if name.is_empty() || command.trim().is_empty() {
            continue;
        }
        let mut e = StartupEntry {
            id: format!("{}|{}", source.tag(), name),
            name: name.clone(),
            path: program_of(&command),
            command,
            publisher: None,
            description: None,
            enabled: approved(source, &name),
            source: source.label().into(),
            how: format!(
                "Windows starts it when {} sign{} in, because it's listed under {hive}\\{key}.",
                if source.all_users() { "anyone" } else { "you" },
                if source.all_users() { "s" } else { "" }
            ),
            needs_admin: source.all_users() && !is_elevated(),
            missing: false,
            icon: None,
        };
        describe(&mut e);
        out.push(e);
    }
}

fn folder_entries(out: &mut Vec<StartupEntry>, source: Source) {
    let id = if source == Source::UserFolder { &FOLDERID_Startup } else { &FOLDERID_CommonStartup };
    let Some(dir) = known_folder(id) else { return };
    let Ok(read) = std::fs::read_dir(&dir) else { return };
    for item in read.flatten() {
        let file = item.file_name().to_string_lossy().to_string();
        if file.eq_ignore_ascii_case("desktop.ini") || item.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let full = item.path();
        let (target, args) = if file.to_ascii_lowercase().ends_with(".lnk") {
            resolve_link(&full).unwrap_or_else(|| (full.display().to_string(), String::new()))
        } else {
            (full.display().to_string(), String::new())
        };
        let shown = Path::new(&file).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or(file.clone());
        let mut e = StartupEntry {
            id: format!("{}|{}", source.tag(), file),
            name: shown,
            command: if args.is_empty() { format!("\"{target}\"") } else { format!("\"{target}\" {args}") },
            path: Some(target),
            publisher: None,
            description: None,
            enabled: approved(source, &file),
            source: source.label().into(),
            how: format!("Windows opens it when {} sign{} in, because it's in {}.", if source.all_users() { "anyone" } else { "you" }, if source.all_users() { "s" } else { "" }, dir.display()),
            needs_admin: source.all_users() && !is_elevated(),
            missing: false,
            icon: None,
        };
        describe(&mut e);
        out.push(e);
    }
}

fn package_name(family: &str) -> (String, Option<String>) {
    // Family "Publisher.App_hash"; full names "Publisher.App_1.2.3.0_x64__hash".
    let (name, hash) = family.rsplit_once('_').unwrap_or((family, ""));
    if let Some(pk) = RegKey::open(HKEY_CURRENT_USER, PACKAGES, KEY_READ) {
        let prefix = format!("{name}_").to_lowercase();
        let suffix = format!("_{hash}").to_lowercase();
        if let Some(full) = pk.subkeys().into_iter().find(|s| {
            let l = s.to_lowercase();
            l.starts_with(&prefix) && l.ends_with(&suffix)
        }) {
            if let Some(k) = RegKey::open(HKEY_CURRENT_USER, &format!(r"{PACKAGES}\{full}"), KEY_READ) {
                let root = k.string("PackageRootFolder");
                if let Some(d) = k.string("DisplayName") {
                    let shown = if d.starts_with('@') {
                        let src = wide(&d);
                        let mut buf = [0u16; 512];
                        unsafe { SHLoadIndirectString(PCWSTR(src.as_ptr()), &mut buf, None) }.ok().map(|_| from_wide(&buf))
                    } else {
                        Some(d)
                    };
                    if let Some(s) = shown.filter(|s| !s.is_empty() && !s.starts_with("ms-resource")) {
                        return (s, root);
                    }
                }
                return (name.rsplit('.').next().unwrap_or(name).to_string(), root);
            }
        }
    }
    (name.rsplit('.').next().unwrap_or(name).to_string(), None)
}

fn package_entries(out: &mut Vec<StartupEntry>) {
    let Some(data) = RegKey::open(HKEY_CURRENT_USER, APP_DATA, KEY_READ) else { return };
    for family in data.subkeys() {
        let Some(fk) = RegKey::open(HKEY_CURRENT_USER, &format!(r"{APP_DATA}\{family}"), KEY_READ) else { continue };
        for task in fk.subkeys() {
            let Some(tk) = RegKey::open(HKEY_CURRENT_USER, &format!(r"{APP_DATA}\{family}\{task}"), KEY_READ) else { continue };
            let Some(state) = tk.dword("State") else { continue };
            let (name, root) = package_name(&family);
            out.push(StartupEntry {
                id: format!("package|{family}|{task}"),
                name,
                command: format!("{family} ({task})"),
                path: root,
                publisher: None,
                description: None,
                enabled: state == 2 || state == 4,
                source: "Store app".into(),
                how: if state >= 3 {
                    "A Store app's startup task, set by your organization's policy.".into()
                } else {
                    "A Store app's startup task. Windows starts the app when you sign in while it's enabled.".into()
                },
                needs_admin: false,
                missing: false,
                icon: None,
            });
        }
    }
}

pub fn entries() -> Vec<StartupEntry> {
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let mut out = Vec::new();
    run_entries(&mut out, Source::UserRun);
    run_entries(&mut out, Source::MachineRun);
    run_entries(&mut out, Source::MachineRun32);
    folder_entries(&mut out, Source::UserFolder);
    folder_entries(&mut out, Source::CommonFolder);
    package_entries(&mut out);
    out
}

pub fn set_enabled(id: &str, enabled: bool) -> ActionResult {
    let mut parts = id.splitn(3, '|');
    let tag = parts.next().unwrap_or("");
    if tag == "package" {
        let (family, task) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let path = format!(r"{APP_DATA}\{family}\{task}");
        let k = RegKey::open(HKEY_CURRENT_USER, &path, KEY_READ).ok_or(ActionError::Gone)?;
        let state = k.dword("State").ok_or(ActionError::Gone)?;
        if state >= 3 {
            return Err(ActionError::Protected("Your organization's policy sets this, so it can't be changed here.".into()));
        }
        let k = RegKey::create(HKEY_CURRENT_USER, &path).ok_or(ActionError::Gone)?;
        // 2 = enabled, 1 = disabled by the user (as Settings writes it).
        return if k.set_dword("State", if enabled { 2 } else { 1 }).0 == 0 { Ok(()) } else { Err(ActionError::Failed("Windows didn't allow the change.".into())) };
    }
    let source = Source::from_tag(tag).ok_or_else(|| ActionError::Failed("Unknown startup entry.".into()))?;
    let name = parts.next().ok_or(ActionError::Gone)?;
    set_approved(source, name, enabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_program_in_a_command() {
        assert_eq!(program_of(r#""C:\Program Files\App\app.exe" --min"#).as_deref(), Some(r"C:\Program Files\App\app.exe"));
        assert_eq!(program_of(r"C:\Tools\tool.exe /background").as_deref(), Some(r"C:\Tools\tool.exe"));
        assert_eq!(program_of(r"C:\Program Files\Thing\thing.EXE -x").as_deref(), Some(r"C:\Program Files\Thing\thing.EXE"));
        assert!(program_of("  ").is_none());
    }

    #[test]
    fn disables_and_restores_a_user_entry() {
        // A throwaway value in this user's Run key, removed again at the end.
        let run = RegKey::create(HKEY_CURRENT_USER, RUN).unwrap();
        let name = "BeroTaskManagerTestEntry";
        assert!(run.set_string(name, r#""C:\Windows\System32\cmd.exe" /c exit"#));
        let find = || entries().into_iter().find(|e| e.name == name);
        let e = find().expect("listed");
        assert!(e.enabled && !e.missing);
        assert_eq!(e.publisher.as_deref().map(|p| p.contains("Microsoft")), Some(true));
        set_enabled(&e.id, false).unwrap();
        assert!(!find().unwrap().enabled);
        set_enabled(&e.id, true).unwrap();
        assert!(find().unwrap().enabled);
        // Clean up: the Run value and its approval flag.
        run.delete_value(name);
        if let Some(k) = RegKey::create(HKEY_CURRENT_USER, &format!(r"{APPROVED}\Run")) {
            k.delete_value(name);
        }
        assert!(find().is_none());
    }
}

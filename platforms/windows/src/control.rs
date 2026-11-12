//! Actions: ending processes, priorities, services, startup entries, and small shell helpers.
//!
//! The app runs with the user's normal rights. When Windows refuses an action for lack of admin
//! rights, the action comes back as `NeedsElevation`. If the user agrees, the same action runs
//! again in a short-lived elevated copy of this exe (`--admin-action ...`), which performs the
//! same safety checks before doing anything, then exits.

use tm_core::model::ProcKey;
use tm_core::platform::*;
use tm_core::safety::{self, Protection};
use windows::Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation};
use windows::Win32::Foundation::*;
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Registry::HKEY_CURRENT_USER;
use windows::Win32::System::SystemInformation::IMAGE_FILE_MACHINE;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, w};

use crate::util::{self, Owned, RegKey, filetime, wide};

pub struct WinControl;

// ─── Processes ─────────────────────────────────────────────────────────────

fn open_error(e: &windows::core::Error) -> ActionError {
    if e.code() == ERROR_ACCESS_DENIED.to_hresult() {
        ActionError::NeedsElevation("Windows needs administrator rights to change this process.".into())
    } else {
        ActionError::Gone
    }
}

/// Opens a process and checks it's still the one the user picked (PIDs are reused quickly).
fn open_verified(key: ProcKey, access: PROCESS_ACCESS_RIGHTS) -> ActionResult<Owned> {
    let h = unsafe { OpenProcess(access | PROCESS_QUERY_LIMITED_INFORMATION, false, key.pid) }.map_err(|e| open_error(&e))?;
    let h = Owned(h);
    let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetProcessTimes(h.get(), &mut c, &mut e, &mut k, &mut u) }.map_err(|_| ActionError::Gone)?;
    if key.created != 0 && filetime(c) != key.created {
        return Err(ActionError::Gone);
    }
    let mut code = 0u32;
    if unsafe { GetExitCodeProcess(h.get(), &mut code) }.is_ok() && code != STILL_ACTIVE.0 as u32 {
        return Err(ActionError::Gone);
    }
    Ok(h)
}

fn critical_flag(h: HANDLE) -> Option<bool> {
    let mut b = BOOL::default();
    unsafe { IsProcessCritical(h, &mut b) }.ok()?;
    Some(b.as_bool())
}

fn image(pid: u32) -> Option<String> {
    crate::meta::WinResolver::new().image_path_of(pid)
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

fn assess(key: ProcKey) -> Protection {
    let path = image(key.pid);
    let name = path.as_deref().map(file_name).unwrap_or(if key.pid == 4 { "System" } else { "" }).to_string();
    let in_system_dir = path.as_deref().is_some_and(|p| p.to_lowercase().starts_with(&util::windows_dir()));
    let critical = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, key.pid) }.ok().map(Owned).and_then(|h| critical_flag(h.get()));
    // Kernel pseudo-processes have no image path.
    if path.is_none() && key.pid > 4 {
        let mut snap = crate::sampler::Snapshot::new();
        if let Some((list, _)) = snap.sample() {
            if let Some(p) = list.iter().find(|p| p.pid == key.pid) {
                return safety::assess_windows(key.pid, &p.name, false, critical, key.pid == std::process::id());
            }
        }
    }
    safety::assess_windows(key.pid, &name, in_system_dir, critical, key.pid == std::process::id())
}

struct CloseScan {
    pid: u32,
    windows: Vec<HWND>,
}

unsafe extern "system" fn collect_windows(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let scan = unsafe { &mut *(lparam.0 as *mut CloseScan) };
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return true.into();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == scan.pid {
            if GetWindow(hwnd, GW_OWNER).map_or(true, |o| o.0.is_null()) {
                scan.windows.push(hwnd);
            }
            return true.into();
        }
        // A Store app's window belongs to ApplicationFrameHost; its content to the app.
        if let Ok(child) = FindWindowExW(Some(hwnd), None, w!("Windows.UI.Core.CoreWindow"), PCWSTR::null()) {
            let mut cpid = 0u32;
            GetWindowThreadProcessId(child, Some(&mut cpid));
            if cpid == scan.pid {
                scan.windows.push(hwnd);
            }
        }
    }
    true.into()
}

const PRIORITY_CLASSES: [(Priority, PROCESS_CREATION_FLAGS); 6] = [
    (Priority::Low, IDLE_PRIORITY_CLASS),
    (Priority::BelowNormal, BELOW_NORMAL_PRIORITY_CLASS),
    (Priority::Normal, NORMAL_PRIORITY_CLASS),
    (Priority::AboveNormal, ABOVE_NORMAL_PRIORITY_CLASS),
    (Priority::High, HIGH_PRIORITY_CLASS),
    (Priority::Realtime, REALTIME_PRIORITY_CLASS),
];

fn priority_of(class: u32) -> Option<Priority> {
    PRIORITY_CLASSES.iter().find(|(_, c)| c.0 == class).map(|(p, _)| *p)
}

pub fn terminate_now(key: ProcKey) -> ActionResult {
    if let Protection::Refused(why) = assess(key) {
        return Err(ActionError::Protected(why));
    }
    let h = open_verified(key, PROCESS_TERMINATE)?;
    if critical_flag(h.get()) == Some(true) {
        return Err(ActionError::Protected("Windows marks this as a critical process.".into()));
    }
    unsafe { TerminateProcess(h.get(), 1) }.map_err(|e| {
        if e.code() == ERROR_ACCESS_DENIED.to_hresult() {
            ActionError::NeedsElevation("Windows needs administrator rights to end this process.".into())
        } else {
            ActionError::Failed(e.message())
        }
    })
}

pub fn set_priority_now(key: ProcKey, p: Priority) -> ActionResult {
    let h = open_verified(key, PROCESS_SET_INFORMATION)?;
    let class = PRIORITY_CLASSES.iter().find(|(x, _)| *x == p).map(|(_, c)| *c).unwrap_or(NORMAL_PRIORITY_CLASS);
    unsafe { SetPriorityClass(h.get(), class) }.map_err(|e| open_error(&e))?;
    // Without admin rights Windows quietly turns Realtime into High.
    if p == Priority::Realtime && unsafe { GetPriorityClass(h.get()) } != REALTIME_PRIORITY_CLASS.0 {
        return Err(ActionError::NeedsElevation("Windows only allows Realtime priority with administrator rights. It was set to High.".into()));
    }
    Ok(())
}

fn command_line(h: HANDLE) -> Option<String> {
    let mut buf = vec![0u64; 512];
    let mut ret = 0u32;
    let mut status = unsafe { NtQueryInformationProcess(h, ProcessCommandLineInformation, buf.as_mut_ptr() as *mut _, (buf.len() * 8) as u32, &mut ret) };
    if status.0 == 0xC000_0004_u32 as i32 && ret as usize > buf.len() * 8 {
        buf = vec![0u64; (ret as usize).div_ceil(8)];
        status = unsafe { NtQueryInformationProcess(h, ProcessCommandLineInformation, buf.as_mut_ptr() as *mut _, (buf.len() * 8) as u32, &mut ret) };
    }
    if status.0 < 0 {
        return None;
    }
    // UNICODE_STRING { u16 length, u16 max, pointer } at the start, pointing into the buffer.
    let bytes = buf.as_ptr() as *const u8;
    let len = unsafe { *(bytes as *const u16) } as usize / 2;
    let ptr = unsafe { *(bytes.add(8) as *const *const u16) };
    if ptr.is_null() || len == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len) }))
}

fn token_elevated(h: HANDLE) -> Option<bool> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(h, TOKEN_QUERY, &mut token) }.ok()?;
    let token = Owned(token);
    let mut e = TOKEN_ELEVATION::default();
    let mut len = 0u32;
    unsafe { GetTokenInformation(token.get(), TokenElevation, Some(&mut e as *mut _ as *mut _), std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut len) }.ok()?;
    Some(e.TokenIsElevated != 0)
}

fn architecture(h: HANDLE) -> Option<String> {
    let (mut process, mut native) = (IMAGE_FILE_MACHINE(0), IMAGE_FILE_MACHINE(0));
    unsafe { IsWow64Process2(h, &mut process, Some(&mut native)) }.ok()?;
    let m = if process.0 == 0 { native.0 } else { process.0 };
    Some(
        match (m, process.0 != 0) {
            (0x8664, _) => "64-bit (x64)",
            (0xAA64, _) => "64-bit (ARM64)",
            (0x014C, _) => "32-bit (x86)",
            (0x01C4, _) => "32-bit (ARM)",
            _ => return None,
        }
        .to_string(),
    )
}

fn efficiency_mode(h: HANDLE) -> Option<bool> {
    let mut s = PROCESS_POWER_THROTTLING_STATE { Version: 1, ..Default::default() };
    unsafe { GetProcessInformation(h, ProcessPowerThrottling, &mut s as *mut _ as *mut _, std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32) }.ok()?;
    // Efficiency mode is execution-speed throttling (EcoQoS) switched on.
    Some(s.ControlMask & 1 != 0 && s.StateMask & 1 != 0)
}

// ─── The elevated helper ───────────────────────────────────────────────────

/// Runs `args` in an elevated copy of this exe and waits for it. Windows shows its consent prompt.
fn elevated(args: &[String]) -> ActionResult {
    let result_file = std::env::temp_dir().join(format!("bero-tm-{}-{}.txt", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)));
    let mut all = vec!["--admin-action".to_string(), "--result".to_string(), result_file.display().to_string()];
    all.extend(args.iter().cloned());
    let params = all.iter().map(|a| quote_arg(a)).collect::<Vec<_>>().join(" ");
    let exe = wide(&util::exe_path());
    let p = wide(&params);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(p.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    if let Err(e) = unsafe { ShellExecuteExW(&mut info) } {
        return Err(if e.code() == ERROR_CANCELLED.to_hresult() { ActionError::Cancelled } else { ActionError::Failed(e.message()) });
    }
    let h = Owned(info.hProcess);
    unsafe { WaitForSingleObject(h.get(), 120_000) };
    let mut code = 1u32;
    let _ = unsafe { GetExitCodeProcess(h.get(), &mut code) };
    let message = std::fs::read_to_string(&result_file).unwrap_or_default();
    let _ = std::fs::remove_file(&result_file);
    match code {
        0 => Ok(()),
        2 => Err(ActionError::Gone),
        3 => Err(ActionError::Protected(message)),
        _ => Err(ActionError::Failed(if message.is_empty() { "The elevated action failed.".into() } else { message })),
    }
}

/// Quotes one command-line argument the way the C runtime parses it back.
fn quote_arg(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '"']) {
        return a.to_string();
    }
    let mut out = String::from('"');
    let mut slashes = 0;
    for c in a.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}

fn start_type_arg(t: StartType) -> &'static str {
    match t {
        StartType::Automatic => "auto",
        StartType::AutomaticDelayed => "delayed",
        StartType::Manual => "manual",
        StartType::Disabled => "disabled",
        _ => "",
    }
}

/// Entry point of the elevated helper: `--admin-action --result <file> <verb> <args...>`.
/// Returns the process exit code.
pub fn run_admin_action(args: &[String]) -> i32 {
    let mut it = args.iter();
    let mut result_file = None;
    let mut rest = Vec::new();
    while let Some(a) = it.next() {
        if a == "--result" {
            result_file = it.next().cloned();
        } else {
            rest.push(a.clone());
        }
    }
    let r: ActionResult = (|| {
        let get = |i: usize| rest.get(i).cloned().ok_or_else(|| ActionError::Failed("Missing argument.".into()));
        let key = |a: usize, b: usize| -> ActionResult<ProcKey> {
            Ok(ProcKey { pid: get(a)?.parse().map_err(|_| ActionError::Failed("Bad PID.".into()))?, created: get(b)?.parse().unwrap_or(0) })
        };
        match get(0)?.as_str() {
            "terminate" => terminate_now(key(1, 2)?),
            "priority" => {
                let p = match get(3)?.as_str() {
                    "low" => Priority::Low,
                    "below" => Priority::BelowNormal,
                    "normal" => Priority::Normal,
                    "above" => Priority::AboveNormal,
                    "high" => Priority::High,
                    "realtime" => Priority::Realtime,
                    _ => return Err(ActionError::Failed("Bad priority.".into())),
                };
                set_priority_now(key(1, 2)?, p)
            }
            "service" => {
                let name = get(2)?;
                let action = match get(1)?.as_str() {
                    "start" => ServiceAction::Start,
                    "stop" => ServiceAction::Stop,
                    "restart" => ServiceAction::Restart,
                    "start-type" => ServiceAction::SetStart(match get(3)?.as_str() {
                        "auto" => StartType::Automatic,
                        "delayed" => StartType::AutomaticDelayed,
                        "manual" => StartType::Manual,
                        "disabled" => StartType::Disabled,
                        _ => return Err(ActionError::Failed("Bad startup type.".into())),
                    }),
                    _ => return Err(ActionError::Failed("Bad service action.".into())),
                };
                crate::services::action(&name, action)
            }
            "startup" => crate::startup::set_enabled(&get(2)?, get(1)? == "enable"),
            _ => Err(ActionError::Failed("Unknown action.".into())),
        }
    })();
    let (code, message) = match r {
        Ok(()) => (0, String::new()),
        Err(ActionError::Gone) => (2, String::new()),
        Err(ActionError::Protected(m)) => (3, m),
        Err(e) => (1, e.message()),
    };
    if let Some(f) = result_file {
        let _ = std::fs::write(f, message);
    }
    code
}

fn priority_arg(p: Priority) -> &'static str {
    match p {
        Priority::Low => "low",
        Priority::BelowNormal => "below",
        Priority::Normal => "normal",
        Priority::AboveNormal => "above",
        Priority::High => "high",
        Priority::Realtime => "realtime",
    }
}

// ─── Autostart (this app) ──────────────────────────────────────────────────

fn own_run_command() -> String {
    format!("\"{}\" --background", util::exe_path())
}

// ─── SystemControl ─────────────────────────────────────────────────────────

impl SystemControl for WinControl {
    fn is_elevated(&self) -> bool {
        util::is_elevated()
    }

    fn restart_elevated(&self) -> ActionResult {
        let exe = wide(&util::exe_path());
        let params = wide(&format!("--replace {}", std::process::id()));
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_FLAG_NO_UI,
            lpVerb: w!("runas"),
            lpFile: PCWSTR(exe.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        unsafe { ShellExecuteExW(&mut info) }.map_err(|e| if e.code() == ERROR_CANCELLED.to_hresult() { ActionError::Cancelled } else { ActionError::Failed(e.message()) })
    }

    fn process_details(&self, key: ProcKey) -> Option<ProcDetails> {
        let mut d = ProcDetails { path: image(key.pid), ..Default::default() };
        if let Some(p) = &d.path {
            let v = crate::meta::version_info(p);
            d.description = v.description;
            d.company = v.company;
            d.version = v.version;
        }
        d.started = util::local_time(key.created);
        if let Ok(h) = open_verified(key, PROCESS_ACCESS_RIGHTS(0)) {
            d.command_line = command_line(h.get());
            d.priority = priority_of(unsafe { GetPriorityClass(h.get()) });
            d.elevated = token_elevated(h.get());
            d.architecture = architecture(h.get());
            d.efficiency_mode = efficiency_mode(h.get());
            d.critical = critical_flag(h.get());
            let mut m = PROCESS_MEMORY_COUNTERS { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
            if unsafe { GetProcessMemoryInfo(h.get(), &mut m, m.cb) }.is_ok() {
                d.peak_working_set = Some(m.PeakWorkingSetSize as u64);
                d.page_faults = Some(m.PageFaultCount as u64);
            }
        }
        let mut r = crate::meta::WinResolver::new();
        d.user = tm_core::platform::MetaResolver::identify(&mut r, key, "").user.map(|u| u.to_string());
        Some(d)
    }

    fn is_critical(&self, key: ProcKey) -> Option<bool> {
        open_verified(key, PROCESS_ACCESS_RIGHTS(0)).ok().and_then(|h| critical_flag(h.get()))
    }

    fn protection(&self, key: ProcKey) -> Protection {
        assess(key)
    }

    fn end_task(&self, key: ProcKey) -> ActionResult<EndOutcome> {
        if let Protection::Refused(why) = assess(key) {
            return Err(ActionError::Protected(why));
        }
        // Confirms the process is still the same one. (Other users' processes can't be opened, but
        // their windows can still be asked to close.)
        match open_verified(key, PROCESS_ACCESS_RIGHTS(0)) {
            Ok(_) | Err(ActionError::NeedsElevation(_)) => {}
            Err(e) => return Err(e),
        }
        let mut scan = CloseScan { pid: key.pid, windows: Vec::new() };
        unsafe {
            let _ = EnumWindows(Some(collect_windows), LPARAM(&mut scan as *mut CloseScan as isize));
        }
        if scan.windows.is_empty() {
            return Ok(EndOutcome::NoWindows);
        }
        for hwnd in scan.windows {
            // The same message the window's close button sends: the app may ask to save work.
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        Ok(EndOutcome::CloseRequested)
    }

    fn terminate(&self, key: ProcKey, elevate: bool) -> ActionResult {
        match terminate_now(key) {
            Err(ActionError::NeedsElevation(_)) if elevate => elevated(&["terminate".into(), key.pid.to_string(), key.created.to_string()]),
            r => r,
        }
    }

    fn set_priority(&self, key: ProcKey, priority: Priority, elevate: bool) -> ActionResult {
        match set_priority_now(key, priority) {
            Err(ActionError::NeedsElevation(_)) if elevate => {
                elevated(&["priority".into(), key.pid.to_string(), key.created.to_string(), priority_arg(priority).into()])
            }
            r => r,
        }
    }

    fn services(&self, with_config: bool) -> Result<Vec<ServiceInfo>, String> {
        crate::services::list(with_config)
    }

    fn service_dependents(&self, name: &str) -> Vec<(String, String)> {
        crate::services::dependents(name)
    }

    fn service_action(&self, name: &str, action: ServiceAction, elevate: bool) -> ActionResult {
        match crate::services::action(name, action) {
            Err(ActionError::NeedsElevation(_)) if elevate => {
                let mut args = vec!["service".to_string()];
                match action {
                    ServiceAction::Start => args.extend(["start".into(), name.into()]),
                    ServiceAction::Stop => args.extend(["stop".into(), name.into()]),
                    ServiceAction::Restart => args.extend(["restart".into(), name.into()]),
                    ServiceAction::SetStart(t) => args.extend(["start-type".into(), name.into(), start_type_arg(t).into()]),
                }
                elevated(&args)
            }
            r => r,
        }
    }

    fn startup_entries(&self) -> Vec<StartupEntry> {
        crate::startup::entries()
    }

    fn set_startup_enabled(&self, id: &str, enabled: bool, elevate: bool) -> ActionResult {
        match crate::startup::set_enabled(id, enabled) {
            Err(ActionError::NeedsElevation(_)) if elevate => {
                elevated(&["startup".into(), if enabled { "enable" } else { "disable" }.into(), id.into()])
            }
            r => r,
        }
    }

    fn autostart(&self) -> bool {
        let Some(k) = RegKey::open(HKEY_CURRENT_USER, util::RUN_KEY, windows::Win32::System::Registry::KEY_READ) else { return false };
        let Some(v) = k.string(util::RUN_VALUE) else { return false };
        v.to_lowercase().contains(&util::exe_path().to_lowercase()) && crate::startup::approved(crate::startup::Source::UserRun, util::RUN_VALUE)
    }

    fn set_autostart(&self, on: bool) -> ActionResult {
        let k = RegKey::create(HKEY_CURRENT_USER, util::RUN_KEY).ok_or_else(|| ActionError::Failed("Couldn't open the Run key.".into()))?;
        if on {
            if !k.set_string(util::RUN_VALUE, &own_run_command()) {
                return Err(ActionError::Failed("Couldn't write the Run key.".into()));
            }
            // If it was switched off in Task Manager or Settings, switch it back on there too.
            crate::startup::set_approved(crate::startup::Source::UserRun, util::RUN_VALUE, true)
        } else {
            k.delete_value(util::RUN_VALUE);
            Ok(())
        }
    }

    fn copy_text(&self, text: &str) {
        unsafe {
            // A message-only window owns the clipboard data; the data outlives it.
            let Ok(owner) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), PCWSTR::null(), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, None, None) else {
                return;
            };
            if OpenClipboard(Some(owner)).is_ok() {
                let _ = EmptyClipboard();
                let data: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                if let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, data.len() * 2) {
                    let p = GlobalLock(mem) as *mut u16;
                    if !p.is_null() {
                        std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
                        let _ = GlobalUnlock(mem);
                        let _ = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(mem.0)));
                    }
                }
                let _ = CloseClipboard();
            }
            let _ = DestroyWindow(owner);
        }
    }

    fn open_location(&self, path: &str) {
        let args = wide(&format!("/select,\"{path}\""));
        unsafe {
            ShellExecuteW(None, w!("open"), w!("explorer.exe"), PCWSTR(args.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL);
        }
    }

    fn open_properties(&self, path: &str) {
        let p = wide(path);
        unsafe {
            let _ = SHObjectProperties(None, SHOP_FILEPATH, PCWSTR(p.as_ptr()), PCWSTR::null());
        }
    }

    fn open_path(&self, path: &str) {
        let p = wide(path);
        unsafe {
            ShellExecuteW(None, w!("open"), PCWSTR(p.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
        }
    }

    fn open_url(&self, url: &str) {
        if url.starts_with("https://") || url.starts_with("ms-settings:") {
            self.open_path(url);
        }
    }
}

/// Used by `restart_elevated` and the installer's `--quit`: waits for another instance to exit.
pub fn wait_for_exit(pid: u32, ms: u32) {
    if let Ok(h) = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
        let h = Owned(h);
        unsafe { WaitForSingleObject(h.get(), ms) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_like_the_crt() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg("has space"), "\"has space\"");
        assert_eq!(quote_arg(r"C:\dir\"), r"C:\dir\");
        assert_eq!(quote_arg(r"C:\my dir\"), r#""C:\my dir\\""#);
        assert_eq!(quote_arg(r#"a"b"#), r#""a\"b""#);
    }

    #[test]
    fn refuses_kernel_and_self() {
        let c = WinControl;
        assert!(c.protection(ProcKey { pid: 4, created: 0 }).is_refused());
        assert!(c.protection(ProcKey { pid: std::process::id(), created: 0 }).is_refused());
        assert!(matches!(c.terminate(ProcKey { pid: 4, created: 0 }, false), Err(ActionError::Protected(_))));
        let mut snap = crate::sampler::Snapshot::new();
        let (list, _) = snap.sample().unwrap();
        for name in ["csrss.exe", "wininit.exe", "Registry", "Memory Compression"] {
            if let Some(p) = list.iter().find(|p| &*p.name == name) {
                let r = c.terminate(ProcKey { pid: p.pid, created: p.created }, false);
                assert!(matches!(r, Err(ActionError::Protected(_))), "{name}: {r:?}");
            }
        }
    }

    #[test]
    fn ends_a_child_process_and_detects_pid_reuse() {
        let mut child = std::process::Command::new(format!("{}System32\\ping.exe", util::windows_dir())).args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
        let pid = child.id();
        let mut snap = crate::sampler::Snapshot::new();
        let (list, _) = snap.sample().unwrap();
        let created = list.iter().find(|p| p.pid == pid).unwrap().created;
        let c = WinControl;
        // A stale creation time means "a different process now": nothing is touched.
        assert_eq!(c.terminate(ProcKey { pid, created: created + 1 }, false), Err(ActionError::Gone));
        let d = c.process_details(ProcKey { pid, created }).unwrap();
        assert!(d.command_line.as_deref().is_some_and(|l| l.contains("127.0.0.1")), "{d:?}");
        assert_eq!(d.priority, Some(Priority::Normal));
        c.set_priority(ProcKey { pid, created }, Priority::BelowNormal, false).unwrap();
        assert_eq!(c.process_details(ProcKey { pid, created }).unwrap().priority, Some(Priority::BelowNormal));
        assert_eq!(c.end_task(ProcKey { pid, created }), Ok(EndOutcome::NoWindows));
        c.terminate(ProcKey { pid, created }, false).unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success());
    }
}

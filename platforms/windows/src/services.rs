//! Windows services through the Service Control Manager.
//!
//! Reading works for every user. Starting, stopping and reconfiguring usually needs admin rights;
//! an "access denied" comes back as [`ActionError::NeedsElevation`] so the UI can offer to retry
//! through the elevated helper. Nothing here forces a service: a stop is a normal stop request.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tm_core::platform::{ActionError, ActionResult, ServiceAction, ServiceInfo, ServiceState, StartType};
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_NOT_ACTIVE};
use windows::Win32::System::Services::*;
use windows::core::{HRESULT, PCWSTR};

use crate::util::{from_pwstr, wide};

struct Sc(SC_HANDLE);

impl Drop for Sc {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}

fn manager(access: u32) -> Result<Sc, ActionError> {
    unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), access) }.map(Sc).map_err(map_err)
}

fn open(scm: &Sc, name: &str, access: u32) -> Result<Sc, ActionError> {
    let n = wide(name);
    unsafe { OpenServiceW(scm.0, PCWSTR(n.as_ptr()), access) }.map(Sc).map_err(map_err)
}

fn map_err(e: windows::core::Error) -> ActionError {
    if e.code() == ERROR_ACCESS_DENIED.to_hresult() {
        ActionError::NeedsElevation("Windows needs administrator rights for this.".into())
    } else {
        ActionError::Failed(e.message().trim().to_string())
    }
}

fn state(v: u32) -> ServiceState {
    match v {
        1 => ServiceState::Stopped,
        2 => ServiceState::StartPending,
        3 => ServiceState::StopPending,
        4 => ServiceState::Running,
        5 => ServiceState::ContinuePending,
        6 => ServiceState::PausePending,
        7 => ServiceState::Paused,
        _ => ServiceState::Unknown,
    }
}

fn config2(svc: &Sc, level: SERVICE_CONFIG) -> Option<Vec<u64>> {
    let mut needed = 0u32;
    let _ = unsafe { QueryServiceConfig2W(svc.0, level, None, &mut needed) };
    if needed == 0 {
        return None;
    }
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    let bytes = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8) };
    unsafe { QueryServiceConfig2W(svc.0, level, Some(bytes), &mut needed) }.ok()?;
    Some(buf)
}

pub struct Config {
    pub start: StartType,
    pub binary: Option<String>,
    pub account: Option<String>,
    pub dependencies: Vec<String>,
    pub group: Option<String>,
}

fn query_config(svc: &Sc) -> Option<Config> {
    let mut needed = 0u32;
    let _ = unsafe { QueryServiceConfigW(svc.0, None, 0, &mut needed) };
    if needed == 0 {
        return None;
    }
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    unsafe { QueryServiceConfigW(svc.0, Some(buf.as_mut_ptr() as *mut _), needed, &mut needed) }.ok()?;
    let c = unsafe { &*(buf.as_ptr() as *const QUERY_SERVICE_CONFIGW) };
    let s = |p: windows::core::PWSTR| {
        let v = unsafe { from_pwstr(p.0) };
        (!v.is_empty()).then_some(v)
    };
    // Dependencies are a double-null-terminated list; group dependencies start with '+'.
    let mut dependencies = Vec::new();
    if !c.lpDependencies.0.is_null() {
        let mut p = c.lpDependencies.0 as *const u16;
        loop {
            let item = unsafe { from_pwstr(p) };
            if item.is_empty() {
                break;
            }
            p = unsafe { p.add(item.encode_utf16().count() + 1) };
            dependencies.push(item.trim_start_matches('+').to_string());
        }
    }
    let delayed = config2(svc, SERVICE_CONFIG_DELAYED_AUTO_START_INFO)
        .is_some_and(|b| unsafe { (*(b.as_ptr() as *const SERVICE_DELAYED_AUTO_START_INFO)).fDelayedAutostart.as_bool() });
    let start = match c.dwStartType {
        SERVICE_AUTO_START if delayed => StartType::AutomaticDelayed,
        SERVICE_AUTO_START => StartType::Automatic,
        SERVICE_DEMAND_START => StartType::Manual,
        SERVICE_DISABLED => StartType::Disabled,
        SERVICE_BOOT_START => StartType::Boot,
        SERVICE_SYSTEM_START => StartType::System,
        _ => StartType::Unknown,
    };
    Some(Config { start, binary: s(c.lpBinaryPathName), account: s(c.lpServiceStartName), dependencies, group: s(c.lpLoadOrderGroup) })
}

/// "LocalSystem" → "SYSTEM", "NT AUTHORITY\LocalService" → "LOCAL SERVICE", as Task Manager names them.
pub fn account_label(account: &str) -> String {
    match account.to_ascii_lowercase().as_str() {
        "localsystem" | ".\\localsystem" => "SYSTEM".into(),
        "nt authority\\localservice" | "nt authority\\local service" => "LOCAL SERVICE".into(),
        "nt authority\\networkservice" | "nt authority\\network service" => "NETWORK SERVICE".into(),
        _ => account.rsplit('\\').next().unwrap_or(account).to_string(),
    }
}

/// Every Win32 service. With `with_config`, also start type, description, binary and account
/// (several calls per service, so the UI asks for it once per visit, off the UI thread).
pub fn list(with_config: bool) -> Result<Vec<ServiceInfo>, String> {
    let scm = manager(SC_MANAGER_ENUMERATE_SERVICE).map_err(|e| e.message())?;
    let mut out = Vec::new();
    // The service manager rejects buffers over 256 KB.
    let mut buf: Vec<u64> = vec![0; 32 * 1024];
    let mut resume = 0u32;
    loop {
        let mut needed = 0u32;
        let mut count = 0u32;
        let bytes = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8) };
        let r = unsafe { EnumServicesStatusExW(scm.0, SC_ENUM_PROCESS_INFO, SERVICE_WIN32, SERVICE_STATE_ALL, Some(bytes), &mut needed, &mut count, Some(&mut resume), PCWSTR::null()) };
        let more = matches!(&r, Err(e) if e.code() == windows::Win32::Foundation::ERROR_MORE_DATA.to_hresult());
        if let Err(e) = &r {
            if !more {
                return Err(e.message());
            }
        }
        let items = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const ENUM_SERVICE_STATUS_PROCESSW, count as usize) };
        for s in items {
            let p = &s.ServiceStatusProcess;
            out.push(ServiceInfo {
                name: unsafe { from_pwstr(s.lpServiceName.0) },
                display: unsafe { from_pwstr(s.lpDisplayName.0) },
                description: None,
                state: state(p.dwCurrentState.0),
                start: StartType::Unknown,
                pid: p.dwProcessId,
                shared_process: p.dwServiceType.0 & SERVICE_WIN32_SHARE_PROCESS.0 != 0,
                can_stop: p.dwControlsAccepted & SERVICE_ACCEPT_STOP != 0,
                binary: None,
                account: None,
                dependencies: Vec::new(),
                group: None,
                triggered: false,
            });
        }
        if !more {
            break;
        }
        if count == 0 {
            buf = vec![0; ((needed as usize).div_ceil(8) + 1024).min(32 * 1024)];
        }
    }
    // Only services that actually share their process with others count as shared.
    let mut per_pid: HashMap<u32, u32> = HashMap::new();
    for s in out.iter().filter(|s| s.pid != 0) {
        *per_pid.entry(s.pid).or_default() += 1;
    }
    for s in &mut out {
        s.shared_process = s.shared_process && per_pid.get(&s.pid).copied().unwrap_or(0) > 1;
    }
    if with_config {
        for s in &mut out {
            let Ok(svc) = open(&scm, &s.name, SERVICE_QUERY_CONFIG) else { continue };
            if let Some(c) = query_config(&svc) {
                s.start = c.start;
                s.binary = c.binary;
                s.account = c.account.map(|a| account_label(&a));
                s.dependencies = c.dependencies;
                s.group = c.group;
            }
            if let Some(b) = config2(&svc, SERVICE_CONFIG_DESCRIPTION) {
                let d = unsafe { from_pwstr((*(b.as_ptr() as *const SERVICE_DESCRIPTIONW)).lpDescription.0) };
                // Some descriptions are resource references that didn't resolve.
                if !d.is_empty() && !d.starts_with('@') {
                    s.description = Some(d);
                }
            }
            if let Some(b) = config2(&svc, SERVICE_CONFIG_TRIGGER_INFO) {
                s.triggered = unsafe { (*(b.as_ptr() as *const SERVICE_TRIGGER_INFO)).cTriggers } > 0;
            }
        }
    }
    Ok(out)
}

/// The account each service-hosting process runs as, from its services' configuration.
pub fn process_accounts() -> HashMap<u32, String> {
    let mut out = HashMap::new();
    let Ok(scm) = manager(SC_MANAGER_ENUMERATE_SERVICE) else { return out };
    for (name, _, pid, _, _) in crate::sampler::enum_services(scm.0, SERVICE_ACTIVE) {
        if pid == 0 || out.contains_key(&pid) {
            continue;
        }
        let Ok(svc) = open(&scm, &name, SERVICE_QUERY_CONFIG) else { continue };
        if let Some(a) = query_config(&svc).and_then(|c| c.account) {
            out.insert(pid, account_label(&a));
        }
    }
    out
}

/// Running services that depend on `name` (they stop with it).
pub fn dependents(name: &str) -> Vec<(String, String)> {
    let Ok(scm) = manager(SC_MANAGER_CONNECT) else { return Vec::new() };
    let Ok(svc) = open(&scm, name, SERVICE_ENUMERATE_DEPENDENTS) else { return Vec::new() };
    let mut needed = 0u32;
    let mut count = 0u32;
    let _ = unsafe { EnumDependentServicesW(svc.0, SERVICE_ACTIVE, None, 0, &mut needed, &mut count) };
    if needed == 0 {
        return Vec::new();
    }
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    if unsafe { EnumDependentServicesW(svc.0, SERVICE_ACTIVE, Some(buf.as_mut_ptr() as *mut _), needed, &mut needed, &mut count) }.is_err() {
        return Vec::new();
    }
    let items = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const ENUM_SERVICE_STATUSW, count as usize) };
    items.iter().map(|s| unsafe { (from_pwstr(s.lpServiceName.0), from_pwstr(s.lpDisplayName.0)) }).collect()
}

fn current_state(svc: &Sc) -> Option<ServiceState> {
    let mut buf = [0u64; 8];
    let bytes = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, 64) };
    let mut needed = 0u32;
    unsafe { QueryServiceStatusEx(svc.0, SC_STATUS_PROCESS_INFO, Some(bytes), &mut needed) }.ok()?;
    Some(state(unsafe { (*(buf.as_ptr() as *const SERVICE_STATUS_PROCESS)).dwCurrentState.0 }))
}

fn wait_for(svc: &Sc, want: ServiceState, timeout: Duration) -> ActionResult {
    let start = Instant::now();
    loop {
        match current_state(svc) {
            Some(s) if s == want => return Ok(()),
            Some(ServiceState::Stopped) if want == ServiceState::Running => {
                return Err(ActionError::Failed("The service stopped right after starting.".into()));
            }
            _ => {}
        }
        if start.elapsed() > timeout {
            return Err(ActionError::Failed("The service didn't respond in time. It may still finish on its own.".into()));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn stop_one(scm: &Sc, name: &str) -> ActionResult {
    let svc = open(scm, name, SERVICE_STOP | SERVICE_QUERY_STATUS)?;
    let mut status = SERVICE_STATUS::default();
    match unsafe { ControlService(svc.0, SERVICE_CONTROL_STOP, &mut status) } {
        Ok(()) => {}
        Err(e) if e.code() == ERROR_SERVICE_NOT_ACTIVE.to_hresult() => return Ok(()),
        Err(e) => return Err(map_err(e)),
    }
    wait_for(&svc, ServiceState::Stopped, Duration::from_secs(30))
}

fn start_one(scm: &Sc, name: &str) -> ActionResult {
    let svc = open(scm, name, SERVICE_START | SERVICE_QUERY_STATUS)?;
    match unsafe { StartServiceW(svc.0, None) } {
        Ok(()) => {}
        Err(e) if e.code() == ERROR_SERVICE_ALREADY_RUNNING.to_hresult() => return Ok(()),
        Err(e) if e.code() == HRESULT::from_win32(1058) => {
            return Err(ActionError::Failed("The service is disabled. Change its startup type first.".into()));
        }
        Err(e) => return Err(map_err(e)),
    }
    wait_for(&svc, ServiceState::Running, Duration::from_secs(30))
}

pub fn action(name: &str, action: ServiceAction) -> ActionResult {
    let scm = manager(SC_MANAGER_CONNECT)?;
    match action {
        ServiceAction::Start => start_one(&scm, name),
        ServiceAction::Stop | ServiceAction::Restart => {
            // Windows won't stop a service while services that depend on it run: stop those
            // first (the UI listed them in its confirmation), and start them again on restart.
            let deps = dependents(name);
            for (d, _) in &deps {
                stop_one(&scm, d)?;
            }
            stop_one(&scm, name)?;
            if action == ServiceAction::Restart {
                start_one(&scm, name)?;
                for (d, _) in deps.iter().rev() {
                    let _ = start_one(&scm, d);
                }
            }
            Ok(())
        }
        ServiceAction::SetStart(t) => {
            let svc = open(&scm, name, SERVICE_CHANGE_CONFIG)?;
            let start = match t {
                StartType::Automatic | StartType::AutomaticDelayed => SERVICE_AUTO_START,
                StartType::Manual => SERVICE_DEMAND_START,
                StartType::Disabled => SERVICE_DISABLED,
                _ => return Err(ActionError::Failed("That startup type can't be chosen.".into())),
            };
            unsafe {
                ChangeServiceConfigW(
                    svc.0,
                    ENUM_SERVICE_TYPE(SERVICE_NO_CHANGE),
                    start,
                    SERVICE_ERROR(SERVICE_NO_CHANGE),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    None,
                    PCWSTR::null(),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    PCWSTR::null(),
                )
            }
            .map_err(map_err)?;
            if matches!(t, StartType::Automatic | StartType::AutomaticDelayed) {
                let info = SERVICE_DELAYED_AUTO_START_INFO { fDelayedAutostart: (t == StartType::AutomaticDelayed).into() };
                unsafe { ChangeServiceConfig2W(svc.0, SERVICE_CONFIG_DELAYED_AUTO_START_INFO, Some(&info as *const _ as *const _)) }.map_err(map_err)?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_services_with_config() {
        let t = Instant::now();
        let list = list(true).unwrap();
        eprintln!("{} services with config in {:?}", list.len(), t.elapsed());
        let rpc = list.iter().find(|s| s.name.eq_ignore_ascii_case("RpcSs")).expect("RpcSs");
        assert_eq!(rpc.state, ServiceState::Running);
        assert!(!rpc.can_stop);
        assert_eq!(rpc.start, StartType::Automatic);
        assert!(rpc.description.is_some());
        let accounts = process_accounts();
        assert!(accounts.values().any(|a| a == "SYSTEM"));
    }

    #[test]
    fn labels_accounts() {
        assert_eq!(account_label("LocalSystem"), "SYSTEM");
        assert_eq!(account_label(r"NT AUTHORITY\LocalService"), "LOCAL SERVICE");
        assert_eq!(account_label(r"NT AUTHORITY\NetworkService"), "NETWORK SERVICE");
        assert_eq!(account_label(r".\alex"), "alex");
    }
}

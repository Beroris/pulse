//! The process list, from one `NtQuerySystemInformation` call per tick.
//!
//! The call returns every process with its memory, CPU time, thread and I/O counters, with no need
//! to open any process (so it works for protected and other users' processes without admin
//! rights). Elevated, the "full" variant also gives true disk counters and each owner's SID.

use std::collections::HashMap;
use std::sync::Arc;

use tm_core::model::{RawProcess, ServiceOwner, WindowOwner};
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::Security::{LookupAccountSidW, PSID, SID_NAME_USE};
use windows::Win32::System::Services::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, PWSTR};

use super::Interner;
use crate::util::from_pwstr;

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn NtQuerySystemInformation(class: i32, info: *mut core::ffi::c_void, len: u32, ret: *mut u32) -> i32;
}

const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
const CLASS_PROCESS: i32 = 5;
const CLASS_FULL_PROCESS: i32 = 148;

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}

/// SYSTEM_PROCESS_INFORMATION.
#[repr(C)]
struct ProcessEntry {
    next_entry_offset: u32,
    number_of_threads: u32,
    working_set_private_size: i64,
    hard_fault_count: u32,
    threads_high_watermark: u32,
    cycle_time: u64,
    create_time: i64,
    user_time: i64,
    kernel_time: i64,
    image_name: UnicodeString,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
    handle_count: u32,
    session_id: u32,
    unique_process_key: usize,
    peak_virtual_size: usize,
    virtual_size: usize,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_page_count: usize,
    read_operation_count: i64,
    write_operation_count: i64,
    other_operation_count: i64,
    read_transfer_count: i64,
    write_transfer_count: i64,
    other_transfer_count: i64,
}

/// SYSTEM_THREAD_INFORMATION.
#[repr(C)]
struct ThreadEntry {
    kernel_time: i64,
    user_time: i64,
    create_time: i64,
    wait_time: u32,
    start_address: usize,
    client_id: [usize; 2],
    priority: i32,
    base_priority: i32,
    context_switches: u32,
    thread_state: u32,
    wait_reason: u32,
}

/// SYSTEM_EXTENDED_THREAD_INFORMATION adds five pointers and three reserved words.
const EXTENDED_THREAD_SIZE: usize = std::mem::size_of::<ThreadEntry>() + 7 * std::mem::size_of::<usize>();

/// The start of SYSTEM_PROCESS_INFORMATION_EXTENSION, which follows the threads in the full list.
#[repr(C)]
struct ProcessExtension {
    bytes_read: u64,
    bytes_written: u64,
    read_operations: u64,
    write_operations: u64,
    flush_operations: u64,
    context_switches: u64,
    flags: u32,
    /// From the start of the process entry; 0 if there is none.
    user_sid_offset: u32,
}

const THREAD_STATE_WAITING: u32 = 5;
const WAIT_REASON_SUSPENDED: u32 = 5;

pub struct Snapshot {
    buf: Vec<u64>,
    /// Whether to try the full list first. Cleared once Windows refuses it (needs admin).
    try_full: bool,
    names: Interner,
    users: HashMap<Vec<u8>, Option<Arc<str>>>,
    name_buf: String,
}

impl Snapshot {
    pub fn new() -> Snapshot {
        Snapshot { buf: Vec::new(), try_full: crate::util::is_elevated(), names: Interner::default(), users: HashMap::new(), name_buf: String::new() }
    }

    fn query(&mut self, class: i32) -> bool {
        if self.buf.is_empty() {
            self.buf = vec![0u64; 512 * 1024 / 8];
        }
        for _ in 0..8 {
            let mut ret = 0u32;
            let len = (self.buf.len() * 8) as u32;
            let status = unsafe { NtQuerySystemInformation(class, self.buf.as_mut_ptr() as *mut _, len, &mut ret) };
            if status == STATUS_INFO_LENGTH_MISMATCH {
                // Room for processes that start between the two calls.
                let want = (ret as usize).max(len as usize) + 64 * 1024;
                self.buf = vec![0u64; want.div_ceil(8)];
                continue;
            }
            return status >= 0;
        }
        false
    }

    /// Every process, and whether the read/write counters are physical disk I/O.
    pub fn sample(&mut self) -> Option<(Vec<RawProcess>, bool)> {
        let mut full = false;
        if self.try_full {
            full = self.query(CLASS_FULL_PROCESS);
            if !full {
                self.try_full = false;
            }
        }
        if !full && !self.query(CLASS_PROCESS) {
            return None;
        }
        let thread_size = if full { EXTENDED_THREAD_SIZE } else { std::mem::size_of::<ThreadEntry>() };
        let base = self.buf.as_ptr() as *const u8;
        let end = self.buf.len() * 8;
        let mut out = Vec::with_capacity(400);
        let mut off = 0usize;
        loop {
            if off + std::mem::size_of::<ProcessEntry>() > end {
                break;
            }
            // SAFETY: the kernel wrote a chain of entries into the buffer; offsets are checked
            // against its length before each read.
            let e = unsafe { &*(base.add(off) as *const ProcessEntry) };
            let pid = e.unique_process_id as u32;
            let name = if pid == 0 {
                self.names.get("System Idle Process")
            } else if e.image_name.buffer.is_null() || e.image_name.length == 0 {
                self.names.get(if pid == 4 { "System" } else { "Unknown" })
            } else {
                let units = unsafe { std::slice::from_raw_parts(e.image_name.buffer, e.image_name.length as usize / 2) };
                self.name_buf.clear();
                self.name_buf.extend(char::decode_utf16(units.iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')));
                self.names.get(&self.name_buf)
            };

            let threads_at = off + std::mem::size_of::<ProcessEntry>();
            let n = e.number_of_threads as usize;
            let mut suspended = n > 0 && pid > 4;
            if threads_at + n * thread_size <= end {
                for t in 0..n {
                    let th = unsafe { &*(base.add(threads_at + t * thread_size) as *const ThreadEntry) };
                    if th.thread_state != THREAD_STATE_WAITING || th.wait_reason != WAIT_REASON_SUSPENDED {
                        suspended = false;
                        break;
                    }
                }
            } else {
                suspended = false;
            }

            let (mut read, mut write) = (e.read_transfer_count as u64, e.write_transfer_count as u64);
            let mut user = None;
            if full {
                let ext_at = threads_at + n * thread_size;
                if ext_at + std::mem::size_of::<ProcessExtension>() <= end {
                    let x = unsafe { &*(base.add(ext_at) as *const ProcessExtension) };
                    read = x.bytes_read;
                    write = x.bytes_written;
                    if x.user_sid_offset != 0 && off + (x.user_sid_offset as usize) < end {
                        user = self.user_name(unsafe { base.add(off + x.user_sid_offset as usize) });
                    }
                }
            }

            out.push(RawProcess {
                pid,
                ppid: e.inherited_from_unique_process_id as u32,
                created: e.create_time.max(0) as u64,
                name,
                threads: e.number_of_threads,
                handles: e.handle_count,
                cpu_time: (e.user_time.max(0) + e.kernel_time.max(0)) as u64,
                private_ws: e.working_set_private_size.max(0) as u64,
                working_set: e.working_set_size as u64,
                commit: e.pagefile_usage as u64,
                read_bytes: read,
                write_bytes: write,
                base_priority: e.base_priority,
                session: e.session_id,
                suspended,
                user,
            });
            if e.next_entry_offset == 0 {
                break;
            }
            off += e.next_entry_offset as usize;
        }
        // Don't hold on to a large buffer between ticks if the list shrank a lot.
        Some((out, full))
    }

    fn user_name(&mut self, sid: *const u8) -> Option<Arc<str>> {
        // A SID is 8 bytes plus 4 per sub-authority (count in byte 1).
        let count = unsafe { *sid.add(1) } as usize;
        if count > 15 {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(sid, 8 + 4 * count) }.to_vec();
        if let Some(n) = self.users.get(&bytes) {
            return n.clone();
        }
        let name = account_name(PSID(bytes.as_ptr() as *mut _)).map(Arc::from);
        self.users.insert(bytes, name.clone());
        name
    }
}

/// The account name of a SID without the domain, as Task Manager shows it ("SYSTEM", "alex").
pub fn account_name(sid: PSID) -> Option<String> {
    let mut name = [0u16; 256];
    let mut domain = [0u16; 256];
    let (mut nl, mut dl) = (name.len() as u32, domain.len() as u32);
    let mut use_ = SID_NAME_USE::default();
    unsafe { LookupAccountSidW(PCWSTR::null(), sid, Some(PWSTR(name.as_mut_ptr())), &mut nl, Some(PWSTR(domain.as_mut_ptr())), &mut dl, &mut use_) }.ok()?;
    let n = crate::util::from_wide(&name);
    (!n.is_empty()).then_some(n)
}

// ─── Windows ───────────────────────────────────────────────────────────────

struct WindowScan {
    owners: Vec<WindowOwner>,
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

unsafe extern "system" fn on_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let scan = unsafe { &mut *(lparam.0 as *mut WindowScan) };
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || GetWindowTextLengthW(hwnd) == 0 {
            return true.into();
        }
        if GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.0.is_null()) {
            return true.into();
        }
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 {
            return true.into();
        }
        let class = class_name(hwnd);
        let mut pid = 0u32;
        match class.as_str() {
            // The desktop and taskbar, not apps.
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" => return true.into(),
            // Top-level core windows are shell surfaces (the touch keyboard, Start); real UWP apps
            // live inside an ApplicationFrameWindow.
            "Windows.UI.Core.CoreWindow" => return true.into(),
            "ApplicationFrameWindow" => {
                // The app is the core window inside the frame, owned by the app's own process. A
                // suspended app has none and doesn't count.
                let mut after: Option<HWND> = None;
                let frame_pid = {
                    let mut p = 0u32;
                    GetWindowThreadProcessId(hwnd, Some(&mut p));
                    p
                };
                while let Ok(child) = FindWindowExW(Some(hwnd), after, windows::core::w!("Windows.UI.Core.CoreWindow"), PCWSTR::null()) {
                    if child.0.is_null() {
                        break;
                    }
                    let mut p = 0u32;
                    GetWindowThreadProcessId(child, Some(&mut p));
                    if p != 0 && p != frame_pid {
                        pid = p;
                        break;
                    }
                    after = Some(child);
                }
                if pid == 0 {
                    return true.into();
                }
            }
            _ => {
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
            }
        }
        if pid == 0 {
            return true.into();
        }
        let hung = IsHungAppWindow(hwnd).as_bool();
        if let Some(o) = scan.owners.iter_mut().find(|o| o.pid == pid) {
            o.hung |= hung;
        } else {
            scan.owners.push(WindowOwner { pid, hung });
        }
    }
    true.into()
}

/// Processes with a visible top-level window, and whether any of their windows is hung.
pub fn top_windows() -> Vec<WindowOwner> {
    let mut scan = WindowScan { owners: Vec::with_capacity(32) };
    unsafe {
        let _ = EnumWindows(Some(on_window), LPARAM(&mut scan as *mut WindowScan as isize));
    }
    scan.owners
}

// ─── Services ──────────────────────────────────────────────────────────────

/// A handle to the service control manager, opened only to enumerate.
pub struct Scm(SC_HANDLE);

unsafe impl Send for Scm {}

impl Scm {
    pub fn open() -> Option<Scm> {
        unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ENUMERATE_SERVICE) }.ok().map(Scm)
    }
}

impl Drop for Scm {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}

/// Running services and the process each runs in.
pub fn enum_service_owners(scm: &Scm) -> Vec<ServiceOwner> {
    enum_services(scm.0, SERVICE_ACTIVE)
        .into_iter()
        .filter(|s| s.2 != 0)
        .map(|(name, display, pid, _, _)| ServiceOwner { pid, name: name.into(), display: display.into() })
        .collect()
}

/// (name, display name, pid, state, process flags) for each Win32 service in the given state.
pub fn enum_services(scm: SC_HANDLE, state: ENUM_SERVICE_STATE) -> Vec<(String, String, u32, u32, u32)> {
    let mut out = Vec::new();
    let mut buf: Vec<u64> = vec![0; 32 * 1024];
    let mut resume = 0u32;
    loop {
        let mut needed = 0u32;
        let mut count = 0u32;
        let bytes = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8) };
        let r = unsafe { EnumServicesStatusExW(scm, SC_ENUM_PROCESS_INFO, SERVICE_WIN32, state, Some(bytes), &mut needed, &mut count, Some(&mut resume), PCWSTR::null()) };
        let more = matches!(&r, Err(e) if e.code() == windows::Win32::Foundation::ERROR_MORE_DATA.to_hresult());
        if r.is_err() && !more {
            break;
        }
        let items = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const ENUM_SERVICE_STATUS_PROCESSW, count as usize) };
        for s in items {
            let p = &s.ServiceStatusProcess;
            out.push(unsafe {
                (from_pwstr(s.lpServiceName.0), from_pwstr(s.lpDisplayName.0), p.dwProcessId, p.dwCurrentState.0, p.dwServiceFlags.0)
            });
        }
        if !more {
            break;
        }
        if count == 0 {
            // Didn't fit even one: grow.
            buf = vec![0; ((needed as usize).div_ceil(8) + 1024).min(32 * 1024)];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_the_kernel() {
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(std::mem::size_of::<ProcessEntry>(), 256);
            assert_eq!(std::mem::size_of::<ThreadEntry>(), 80);
            assert_eq!(EXTENDED_THREAD_SIZE, 136);
        }
    }

    #[test]
    fn lists_processes_and_services() {
        let mut s = Snapshot::new();
        let (list, _) = s.sample().expect("process list");
        let me = list.iter().find(|p| p.pid == std::process::id()).expect("self");
        assert!(me.threads > 0 && me.private_ws > 0 && me.cpu_time > 0);
        assert!(list.iter().any(|p| p.pid == 4 && &*p.name == "System"));
        assert!(list.iter().any(|p| p.pid == 0));
        let scm = Scm::open().expect("scm");
        let services = enum_service_owners(&scm);
        assert!(services.len() > 10, "{}", services.len());
        let _ = top_windows();
    }
}

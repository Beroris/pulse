//! One running copy: a named mutex says whether another is running, and a hidden message-only
//! window lets a second launch (or the installer's `--quit`) hand it a command.

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, FindWindowExW, GetWindowThreadProcessId, HWND_MESSAGE, MSGFLT_ALLOW, RegisterClassW,
    SMTO_ABORTIFHUNG, SendMessageTimeoutW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_COPYDATA, WNDCLASSW,
};
use windows::core::PCWSTR;

use crate::util::{IPC_CLASS, MUTEX_NAME, wide};

/// Marks a WM_COPYDATA payload as one of ours.
const MAGIC: usize = 0x504c_5331; // "PLS1"

/// Held for the life of the process; Windows releases it when the process ends.
pub struct Instance(#[allow(dead_code)] HANDLE);

/// Takes the single-instance mutex. `None` when another copy already holds it.
pub fn claim() -> Option<Instance> {
    let name = wide(MUTEX_NAME);
    let h = unsafe { CreateMutexW(None, true, PCWSTR(name.as_ptr())) }.ok()?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        // Someone else's. Holding the handle would keep the mutex alive after that copy exits,
        // and a later claim (taking over from it) would never succeed.
        unsafe {
            let _ = CloseHandle(h);
        }
        return None;
    }
    Some(Instance(h))
}

fn find_running() -> Option<HWND> {
    let class = wide(IPC_CLASS);
    let h = unsafe { FindWindowExW(Some(HWND_MESSAGE), None, PCWSTR(class.as_ptr()), PCWSTR::null()) }.ok()?;
    (!h.is_invalid()).then_some(h)
}

/// Process ID of the running copy, if its window is up.
pub fn running_pid() -> Option<u32> {
    let h = find_running()?;
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    (pid != 0).then_some(pid)
}

/// Sends `command` to the running copy. Returns its process ID when it was reached. Waits up to
/// `wait_ms` for its window to appear, for a copy that is still starting.
pub fn send(command: &str, wait_ms: u32) -> Option<u32> {
    let mut waited = 0;
    let h = loop {
        if let Some(h) = find_running() {
            break h;
        }
        if waited >= wait_ms {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        waited += 100;
    };
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    if pid != 0 {
        // Lets it bring its window to the front: Windows only allows that to the app the user is using.
        unsafe {
            let _ = AllowSetForegroundWindow(pid);
        }
    }
    let bytes = command.as_bytes();
    let data = COPYDATASTRUCT { dwData: MAGIC, cbData: bytes.len() as u32, lpData: bytes.as_ptr() as *mut _ };
    let mut result = 0usize;
    unsafe {
        SendMessageTimeoutW(h, WM_COPYDATA, WPARAM(0), LPARAM(&data as *const _ as isize), SMTO_ABORTIFHUNG, 5000, Some(&mut result));
    }
    Some(pid)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_COPYDATA && lp.0 != 0 {
        let data = unsafe { &*(lp.0 as *const COPYDATASTRUCT) };
        if data.dwData == MAGIC && !data.lpData.is_null() && data.cbData <= 4096 {
            let bytes = unsafe { std::slice::from_raw_parts(data.lpData as *const u8, data.cbData as usize) };
            let command = String::from_utf8_lossy(bytes).into_owned();
            if command == "quit" {
                // Asked to quit (by the installer, or a launcher): if the app hasn't closed itself
                // in a few seconds, end it anyway. Settings are saved as they change, so nothing
                // is lost, and an install never waits on a copy that has stopped responding.
                std::thread::spawn(|| {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    crate::end_now(0);
                });
            }
            tm_app::external_command(command);
            return LRESULT(1);
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// Creates the window that receives commands. On the UI thread, whose event loop delivers its
/// messages.
pub fn listen() {
    let class = wide(IPC_CLASS);
    unsafe {
        let module = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: module.into(), lpszClassName: PCWSTR(class.as_ptr()), ..Default::default() };
        RegisterClassW(&wc);
        let window = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class.as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(module.into()),
            None,
        );
        // A copy running as administrator (restarted that way, or started by Ctrl+Shift+Esc in
        // place of Task Manager) still hears launches and the installer's --quit, which run
        // without it. Windows drops their messages unless it says so.
        if let Ok(window) = window {
            let _ = ChangeWindowMessageFilterEx(window, WM_COPYDATA, MSGFLT_ALLOW, None);
        }
    }
}

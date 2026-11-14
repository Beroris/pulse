#![cfg_attr(not(test), windows_subsystem = "windows")]

//! Pulse for Windows.
//!
//! Command line:
//! - `--background`: start without showing the main window (used when signing in).
//! - `--quit`: ask a running copy to close, and wait for it (used by the installer).
//! - `--replace <pid>`: wait for that process to exit first (used when restarting as administrator).
//!   Any launch as administrator also takes the place of a running copy that isn't.
//! - `--admin-action ...`: the elevated helper that carries out one action and exits.
//! - `--updated`, `--after-update <pid>`: started again after an update (see update.rs).
//! - `<Task Manager's path> [its arguments]`: Windows starting Pulse in place of Task Manager
//!   (see taskmgr.rs). Shows the window like a normal launch; `/startup`, from links to Task
//!   Manager's startup apps, opens on the Startup page.

mod control;
mod icon;
mod instance;
mod meta;
mod pdh;
mod sampler;
mod services;
mod shell;
mod startup;
mod taskmgr;
mod update;
mod util;

use std::rc::Rc;
use std::sync::Arc;

use windows::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SEM_NOOPENFILEERRORBOX, SetErrorMode};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // No "insert a disk" or "file not found" boxes from reading paths and icons of other programs.
    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX);
    }
    if args.first().map(String::as_str) == Some("--admin-action") {
        std::process::exit(control::run_admin_action(&args[1..]));
    }
    if args.iter().any(|a| a == "--quit") {
        if let Some(pid) = instance::send("quit", 0) {
            control::wait_for_exit(pid, 10_000);
        }
        return;
    }
    if let Some(pid) = args.iter().position(|a| a == "--replace").and_then(|i| args.get(i + 1)).and_then(|p| p.parse::<u32>().ok()) {
        control::wait_for_exit(pid, 15_000);
    }
    // After a portable update this waits for the old version to go, so it isn't turned away below.
    let updated = update::finish_update(&args);
    let background = args.iter().any(|a| a == "--background");
    let startup_page = args.iter().any(|a| a.eq_ignore_ascii_case("/startup"));

    let mut claimed = instance::claim();
    let mut replacing = args.iter().any(|a| a == "--replace");
    if claimed.is_none() && !background && util::is_elevated() {
        // Started as administrator (Run as administrator on a shortcut, or Ctrl+Shift+Esc) while a
        // copy without it runs. Bringing that one forward would leave FPS and the CPU sensor
        // asking for administrator rights, so take its place, as the sidebar's button does.
        if let Some(pid) = instance::running_pid().filter(|&pid| control::process_elevated(pid) == Some(false)) {
            instance::send("quit", 0);
            control::wait_for_exit(pid, 10_000);
            replacing = true;
        }
    }
    if claimed.is_none() && replacing {
        // The old copy has gone but Windows may not have released its mutex yet.
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            claimed = instance::claim();
            if claimed.is_some() {
                break;
            }
        }
    }
    let Some(_instance) = claimed else {
        // Already running: a sign-in launch has nothing to add, anything else brings it forward.
        if !background {
            instance::send(if startup_page { "startup" } else { "show" }, 3000);
        }
        return;
    };
    instance::listen();

    let platform = tm_app::Platform {
        sampler: Box::new(sampler::WinSampler::new()),
        resolver: Box::new(meta::WinResolver::new()),
        control: Arc::new(control::WinControl),
        shell: Rc::new(shell::WinShell),
        settings_path: util::data_dir().join("settings.json"),
        start_hidden: background,
        open_startup: startup_page,
        updates: Arc::new(update::WinUpdates::new()),
        just_updated: updated,
    };
    let code = match tm_app::run(platform) {
        Ok(()) => 0,
        Err(e) => {
            show_error(&format!("Pulse couldn't start.\n\n{e}"));
            1
        }
    };
    end_now(code);
}

/// Ends the process here, with settings already saved. A normal exit would first stop the worker
/// threads, then tear down thread-locals, including the controller, whose collector waits for
/// those threads and so panicked and aborted on every exit. Nothing left needs tearing down.
pub fn end_now(code: u32) -> ! {
    use windows::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
    // A frame-rate trace session would outlive the process.
    sampler::fps::stop_on_exit();
    unsafe {
        let _ = TerminateProcess(GetCurrentProcess(), code);
    }
    std::process::exit(code as i32)
}

fn show_error(text: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::PCWSTR;
    let t = util::wide(text);
    let c = util::wide(util::APP_NAME);
    unsafe {
        MessageBoxW(None, PCWSTR(t.as_ptr()), PCWSTR(c.as_ptr()), MB_OK | MB_ICONERROR);
    }
}

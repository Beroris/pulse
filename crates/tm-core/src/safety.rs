//! Termination safeguards.
//!
//! Before a process is ended, it is classified. Some processes are refused outright because ending
//! them stops or crashes Windows; others get a stronger warning. The backend repeats these checks
//! right before acting, so the UI can't be talked past them.

/// How careful to be with ending a process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Protection {
    /// An ordinary process. End it after the usual confirmation.
    Normal,
    /// Allowed, with an explanation of what will happen.
    Caution(String),
    /// Never ended by this app.
    Refused(String),
}

impl Protection {
    pub fn is_refused(&self) -> bool {
        matches!(self, Protection::Refused(_))
    }
}

/// Kernel pseudo-processes: they aren't real processes and can't be ended by anyone. None has an
/// ".exe" name, and Windows always starts real programs from a file with an extension, so an
/// ordinary program can't take one of these names.
const KERNEL: &[&str] = &["system idle process", "idle", "system", "registry", "memory compression", "secure system", "system interrupts"];

/// Ending any of these stops Windows (a "critical process died" stop error) or signs you out.
const CRITICAL: &[&str] = &["smss.exe", "csrss.exe", "wininit.exe", "winlogon.exe", "services.exe", "lsass.exe", "lsaiso.exe", "lsm.exe"];

/// Allowed, but worth a specific warning.
const CAUTION: &[(&str, &str)] = &[
    ("explorer.exe", "The taskbar, Start menu and desktop disappear until Windows Explorer is started again."),
    ("dwm.exe", "The screen may flicker or go black for a moment while Windows restarts the Desktop Window Manager."),
    ("svchost.exe", "This hosts Windows services. Ending it stops them all, which can make Windows unstable."),
    ("sihost.exe", "This runs parts of the Windows shell. Notifications and the Start menu may stop working until you sign in again."),
    ("fontdrvhost.exe", "Windows uses this to draw fonts. Text may stop showing until you sign in again."),
    ("ctfmon.exe", "Typing in some apps, including the search box, may stop working until it's started again."),
    ("audiodg.exe", "Sound stops until an app plays something again."),
    ("spoolsv.exe", "Printing stops until the Print Spooler service is started again."),
];

/// Classifies a process for ending.
///
/// - `pid` 0 and 4 are always kernel processes on Windows.
/// - `in_system_dir` is true when the executable lives in the Windows folder. Names alone can be
///   spoofed by any program, so the critical list only applies there.
/// - `critical_flag` is the OS's own "critical process" bit, when it could be read.
/// - `is_self` is this app's own process.
pub fn assess_windows(pid: u32, name: &str, in_system_dir: bool, critical_flag: Option<bool>, is_self: bool) -> Protection {
    let lower = name.to_ascii_lowercase();
    if pid == 0 || pid == 4 || KERNEL.contains(&lower.as_str()) {
        return Protection::Refused("This is part of the Windows kernel and can't be ended.".into());
    }
    if critical_flag == Some(true) {
        return Protection::Refused("Windows marks this as a critical process. Ending it would stop Windows with an error.".into());
    }
    if in_system_dir && CRITICAL.contains(&lower.as_str()) {
        return Protection::Refused("This is a core Windows process. Ending it would stop Windows or sign you out.".into());
    }
    if is_self {
        return Protection::Refused("This is Bero Task Manager itself. Quit it from the tray icon or with Ctrl+Q.".into());
    }
    if in_system_dir {
        if let Some((_, why)) = CAUTION.iter().find(|(n, _)| *n == lower) {
            return Protection::Caution((*why).into());
        }
        return Protection::Caution("This is a Windows process. Ending it may make parts of Windows stop working.".into());
    }
    Protection::Normal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_pids_are_refused() {
        assert!(assess_windows(0, "System Idle Process", false, None, false).is_refused());
        assert!(assess_windows(4, "System", false, None, false).is_refused());
        assert!(assess_windows(160, "Registry", false, None, false).is_refused());
        assert!(assess_windows(3712, "Memory Compression", false, None, false).is_refused(), "high PIDs too");
    }

    #[test]
    fn critical_flag_wins() {
        assert!(assess_windows(1234, "whatever.exe", false, Some(true), false).is_refused());
    }

    #[test]
    fn critical_names_only_count_in_the_windows_folder() {
        assert!(assess_windows(700, "csrss.exe", true, None, false).is_refused());
        assert!(assess_windows(700, "lsass.exe", true, Some(false), false).is_refused(), "the name list applies even if the flag read says otherwise");
        // A program elsewhere that merely calls itself csrss.exe is treated as ordinary.
        assert_eq!(assess_windows(7000, "csrss.exe", false, None, false), Protection::Normal);
    }

    #[test]
    fn explorer_gets_a_specific_warning() {
        match assess_windows(5000, "explorer.exe", true, Some(false), false) {
            Protection::Caution(why) => assert!(why.contains("taskbar")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn other_windows_processes_get_a_general_warning() {
        assert!(matches!(assess_windows(5000, "SearchHost.exe", true, None, false), Protection::Caution(_)));
    }

    #[test]
    fn ordinary_apps_are_normal() {
        assert_eq!(assess_windows(9000, "notepad++.exe", false, Some(false), false), Protection::Normal);
    }

    #[test]
    fn self_is_refused() {
        assert!(assess_windows(9000, "bero-task-manager.exe", false, None, true).is_refused());
    }
}

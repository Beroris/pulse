//! Opening Pulse in place of Task Manager.
//!
//! When something starts taskmgr.exe, Windows starts the program in the `Debugger` value of
//! taskmgr.exe's Image File Execution Options key instead, with Task Manager's command line
//! after it. Process Explorer's "Replace Task Manager" works the same way. It catches every way
//! of opening Task Manager: Ctrl+Shift+Esc, the taskbar's menu, Ctrl+Alt+Del and `taskmgr`.
//! The key is under HKLM, so it applies to every account and only the elevated helper can
//! change it.

use std::path::Path;

use tm_core::platform::{ActionError, ActionResult, TaskManagerTarget};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE};

use crate::util::{self, RegKey};

const KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\taskmgr.exe";
const VALUE: &str = "Debugger";

pub fn target() -> TaskManagerTarget {
    let command = RegKey::open(HKEY_LOCAL_MACHINE, KEY, KEY_READ).and_then(|k| k.string(VALUE));
    match classify(command.as_deref(), &util::exe_path(), |p| Path::new(p).is_file()) {
        TaskManagerTarget::Other(path) => {
            let name = crate::meta::version_info(&path).description.filter(|d| !d.trim().is_empty());
            TaskManagerTarget::Other(name.unwrap_or_else(|| file_name(&path).to_string()))
        }
        t => t,
    }
}

/// Points Task Manager at this copy, or takes this copy off it. Off leaves another program's
/// value alone, and changes nothing (so asks for nothing) when this copy isn't set.
pub fn set(this_app: bool) -> ActionResult {
    let own = util::exe_path();
    if own.is_empty() {
        return Err(ActionError::Failed("Couldn't find Pulse's own file.".into()));
    }
    if this_app {
        let k = RegKey::create(HKEY_LOCAL_MACHINE, KEY).ok_or_else(refused)?;
        if !k.set_string(VALUE, &format!("\"{own}\"")) {
            return Err(refused());
        }
    } else if target() == TaskManagerTarget::ThisApp {
        let k = RegKey::open(HKEY_LOCAL_MACHINE, KEY, KEY_READ | KEY_SET_VALUE).ok_or_else(refused)?;
        if !k.delete_value(VALUE) {
            return Err(refused());
        }
    }
    Ok(())
}

fn refused() -> ActionError {
    if util::is_elevated() {
        ActionError::Failed("Windows didn't allow the change.".into())
    } else {
        ActionError::NeedsElevation("Windows only lets administrators change what opens in place of Task Manager.".into())
    }
}

fn classify(command: Option<&str>, own: &str, exists: impl Fn(&str) -> bool) -> TaskManagerTarget {
    let program = command.map(program).unwrap_or("").trim();
    if program.is_empty() {
        TaskManagerTarget::TaskManager
    } else if !own.is_empty() && program.to_lowercase() == own.to_lowercase() {
        TaskManagerTarget::ThisApp
    } else if program.contains(['\\', '/']) && !exists(program) {
        // A bare name is looked up on the PATH when it runs, so only a full path can be checked.
        TaskManagerTarget::Missing(program.to_string())
    } else {
        TaskManagerTarget::Other(program.to_string())
    }
}

/// The program a command line starts: its first argument, quoted or not.
fn program(command: &str) -> &str {
    let c = command.trim_start();
    match c.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(rest),
        None => c.split_whitespace().next().unwrap_or(""),
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use TaskManagerTarget::*;

    const OWN: &str = r"C:\Users\Me\AppData\Local\Programs\Pulse\pulse.exe";

    fn check(command: Option<&str>) -> TaskManagerTarget {
        classify(command, OWN, |p| p.to_lowercase().contains("procexp"))
    }

    #[test]
    fn reads_the_debugger_value() {
        assert_eq!(check(None), TaskManager);
        assert_eq!(check(Some("  ")), TaskManager);
        assert_eq!(check(Some(r#""C:\Users\Me\AppData\Local\Programs\Pulse\pulse.exe""#)), ThisApp);
        assert_eq!(check(Some(r"c:\users\me\appdata\local\programs\pulse\PULSE.EXE")), ThisApp);
        assert_eq!(check(Some(r#""C:\Tools\procexp64.exe" /e"#)), Other(r"C:\Tools\procexp64.exe".into()));
        assert_eq!(check(Some(r#""D:\Old\Pulse-Portable.exe""#)), Missing(r"D:\Old\Pulse-Portable.exe".into()));
        assert_eq!(check(Some("ntsd -d")), Other("ntsd".into()));
    }
}

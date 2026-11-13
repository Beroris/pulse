//! What a platform backend provides. Windows implements these in `platforms/windows`; a macOS
//! backend would implement the same traits and reuse the engine and the whole UI.

use std::sync::Arc;

use crate::model::*;

/// What the engine needs this tick. Expensive parts are only asked for when something shows them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SampleRequest {
    pub processes: bool,
    /// Which processes own visible windows (app grouping, "Not responding").
    pub windows: bool,
    /// Which services each process hosts.
    pub services: bool,
    pub process_gpu: bool,
    /// Standby, modified and pool sizes.
    pub memory_detail: bool,
    /// Temperatures and GPU clocks.
    pub sensors: bool,
    /// Read every GPU engine each time. Otherwise a backend may read engines that have been idle
    /// for a while less often, and so notice one starting work a few seconds late.
    pub gpu_engines: bool,
    /// Count the frames the app in front presents.
    pub fps: bool,
}

/// Reads counters. Owned by the collector thread.
pub trait Sampler: Send {
    /// Describes the hardware. `full` also includes slow parts (memory modules, GPU drivers),
    /// which a backend may cache. Called at start and every so often to pick up new devices.
    fn describe(&mut self, full: bool) -> SystemInfo;
    /// Reads every requested counter. Must not fail as a whole: a counter that can't be read is
    /// left out or `None`.
    fn sample(&mut self, req: &SampleRequest) -> RawSample;
    /// Sampling is paused: let go of anything that only runs while sampling, such as a frame trace.
    fn pause(&mut self) {}
}

/// Raw image data for an icon, RGBA8, not premultiplied.
#[derive(Clone, Debug)]
pub struct IconPixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct FileInfo {
    pub description: Option<Arc<str>>,
    pub company: Option<Arc<str>>,
    pub version: Option<Arc<str>>,
    pub icon: Option<IconPixels>,
}

/// Finds slow-changing process metadata. Owned by a low-priority background thread.
pub trait MetaResolver: Send {
    /// Executable path, owner and start time. Quick; called once per new process.
    fn identify(&mut self, key: ProcKey, name: &str) -> Identity;
    /// Description, publisher and icon of an executable. Slow; called once per path.
    fn file_info(&mut self, path: &str) -> FileInfo;
}

#[derive(Clone, Debug, Default)]
pub struct Identity {
    pub path: Option<Arc<str>>,
    pub user: Option<Arc<str>>,
    pub started: Option<Arc<str>>,
    pub system: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Priority {
    Low,
    BelowNormal,
    Normal,
    AboveNormal,
    High,
    Realtime,
}

impl Priority {
    pub const ALL: [Priority; 6] =
        [Priority::Realtime, Priority::High, Priority::AboveNormal, Priority::Normal, Priority::BelowNormal, Priority::Low];

    pub fn label(self) -> &'static str {
        match self {
            Priority::Low => "Low",
            Priority::BelowNormal => "Below normal",
            Priority::Normal => "Normal",
            Priority::AboveNormal => "Above normal",
            Priority::High => "High",
            Priority::Realtime => "Realtime",
        }
    }

    /// Windows base priority of a process's main class, used to label the priority column without
    /// opening every process.
    pub fn from_base(base: i32) -> Option<Priority> {
        Some(match base {
            1..=5 => Priority::Low,
            6 | 7 => Priority::BelowNormal,
            8 => Priority::Normal,
            9 | 10 => Priority::AboveNormal,
            11..=15 => Priority::High,
            16..=31 => Priority::Realtime,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionError {
    /// Windows refused because this needs administrator rights. The UI can offer to retry elevated.
    NeedsElevation(String),
    /// The OS protects this process or service; nothing will be attempted.
    Protected(String),
    /// The process or service no longer exists (or its PID now belongs to a different process).
    Gone,
    /// The user cancelled the elevation prompt.
    Cancelled,
    Failed(String),
}

impl ActionError {
    pub fn message(&self) -> String {
        match self {
            ActionError::NeedsElevation(m) | ActionError::Protected(m) | ActionError::Failed(m) => m.clone(),
            ActionError::Gone => "It has already exited.".into(),
            ActionError::Cancelled => "Cancelled.".into(),
        }
    }
}

pub type ActionResult<T = ()> = Result<T, ActionError>;

/// How a graceful end went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndOutcome {
    /// Close was requested from the app's windows. It may still ask to save work.
    CloseRequested,
    /// The process has no windows to close; only forcing it to end is possible.
    NoWindows,
}

#[derive(Clone, Debug, Default)]
pub struct ProcDetails {
    pub command_line: Option<String>,
    pub path: Option<String>,
    pub description: Option<String>,
    pub company: Option<String>,
    pub version: Option<String>,
    pub user: Option<String>,
    pub started: Option<String>,
    pub priority: Option<Priority>,
    pub elevated: Option<bool>,
    pub architecture: Option<String>,
    pub efficiency_mode: Option<bool>,
    pub critical: Option<bool>,
    pub peak_working_set: Option<u64>,
    pub page_faults: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceState {
    Stopped,
    StartPending,
    StopPending,
    Running,
    ContinuePending,
    PausePending,
    Paused,
    Unknown,
}

impl ServiceState {
    pub fn label(self) -> &'static str {
        match self {
            ServiceState::Stopped => "Stopped",
            ServiceState::StartPending => "Starting",
            ServiceState::StopPending => "Stopping",
            ServiceState::Running => "Running",
            ServiceState::ContinuePending => "Resuming",
            ServiceState::PausePending => "Pausing",
            ServiceState::Paused => "Paused",
            ServiceState::Unknown => "Unknown",
        }
    }
    pub fn is_running(self) -> bool {
        !matches!(self, ServiceState::Stopped | ServiceState::Unknown)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartType {
    Automatic,
    AutomaticDelayed,
    Manual,
    Disabled,
    /// Drivers only.
    Boot,
    System,
    Unknown,
}

impl StartType {
    pub const CHOOSABLE: [StartType; 4] = [StartType::Automatic, StartType::AutomaticDelayed, StartType::Manual, StartType::Disabled];
    pub fn label(self) -> &'static str {
        match self {
            StartType::Automatic => "Automatic",
            StartType::AutomaticDelayed => "Automatic (delayed)",
            StartType::Manual => "Manual",
            StartType::Disabled => "Disabled",
            StartType::Boot => "Boot",
            StartType::System => "System",
            StartType::Unknown => "Unknown",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ServiceInfo {
    pub name: String,
    pub display: String,
    pub description: Option<String>,
    pub state: ServiceState,
    pub start: StartType,
    pub pid: u32,
    pub shared_process: bool,
    pub can_stop: bool,
    pub binary: Option<String>,
    pub account: Option<String>,
    pub dependencies: Vec<String>,
    pub group: Option<String>,
    /// Starts when something asks for it (a trigger), besides its start type.
    pub triggered: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
    SetStart(StartType),
}

#[derive(Clone, Debug)]
pub struct StartupEntry {
    /// Opaque to the UI; the backend uses it to find the entry again.
    pub id: String,
    pub name: String,
    pub command: String,
    pub path: Option<String>,
    pub publisher: Option<String>,
    pub description: Option<String>,
    pub enabled: bool,
    /// Short label of where it's registered: "Registry · this user".
    pub source: String,
    /// A sentence on how it starts, for people who want to know.
    pub how: String,
    pub needs_admin: bool,
    /// The program it starts no longer exists.
    pub missing: bool,
    pub icon: Option<IconPixels>,
}

/// Actions on the system. Called from worker threads, so it must be shareable.
pub trait SystemControl: Send + Sync {
    fn is_elevated(&self) -> bool;
    /// Starts an elevated copy of the app and returns once it's running (the caller then quits).
    fn restart_elevated(&self) -> ActionResult;

    fn process_details(&self, key: ProcKey) -> Option<ProcDetails>;
    /// Whether ending this process would crash or stop the OS. `None` if it can't be checked.
    fn is_critical(&self, key: ProcKey) -> Option<bool>;
    /// How careful to be about ending this process. The backend checks again before acting.
    fn protection(&self, key: ProcKey) -> crate::safety::Protection;
    /// Asks a process to close its windows, like clicking their close buttons.
    fn end_task(&self, key: ProcKey) -> ActionResult<EndOutcome>;
    /// Ends a process immediately. Unsaved work is lost.
    fn terminate(&self, key: ProcKey, elevate: bool) -> ActionResult;
    fn set_priority(&self, key: ProcKey, priority: Priority, elevate: bool) -> ActionResult;

    fn services(&self, with_config: bool) -> Result<Vec<ServiceInfo>, String>;
    /// Running services that depend on this one and would stop with it.
    fn service_dependents(&self, name: &str) -> Vec<(String, String)>;
    fn service_action(&self, name: &str, action: ServiceAction, elevate: bool) -> ActionResult;

    fn startup_entries(&self) -> Vec<StartupEntry>;
    fn set_startup_enabled(&self, id: &str, enabled: bool, elevate: bool) -> ActionResult;

    /// Empties the standby list: cached file and program data that is kept in RAM in case it’s
    /// needed again. Needs administrator rights.
    fn clear_standby(&self, elevate: bool) -> ActionResult;

    fn autostart(&self) -> bool;
    fn set_autostart(&self, on: bool) -> ActionResult;

    fn copy_text(&self, text: &str);
    fn open_location(&self, path: &str);
    fn open_properties(&self, path: &str);
    fn open_path(&self, path: &str);
    fn open_url(&self, url: &str);
}

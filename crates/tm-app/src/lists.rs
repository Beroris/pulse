//! Startup apps and services: the lists, search, selection and the facts shown beside them.

use slint::{Image, SharedString};
use tm_core::platform::{ServiceInfo, ServiceState, StartType, StartupEntry};

use crate::util::stat;
use crate::{ServiceRow, StartupRow, StatData};

// ─── Startup ───────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct Startup {
    pub loaded: bool,
    pub loading: bool,
    pub entries: Vec<(StartupEntry, Option<Image>)>,
    pub search: String,
    /// Indexes into `entries` of the rows shown, in order.
    pub shown: Vec<usize>,
    /// Selected entry, by id.
    pub selected: Option<String>,
}

fn has(hay: &str, needle: &str) -> bool {
    hay.to_lowercase().contains(needle)
}

impl Startup {
    pub fn set(&mut self, list: Vec<(StartupEntry, Option<Image>)>) {
        let mut list = list;
        list.sort_by(|a, b| a.0.name.to_lowercase().cmp(&b.0.name.to_lowercase()));
        self.entries = list;
        self.loaded = true;
        self.loading = false;
        if self.selected.as_ref().is_some_and(|id| !self.entries.iter().any(|(e, _)| &e.id == id)) {
            self.selected = None;
        }
        self.filter();
    }

    pub fn filter(&mut self) {
        let n = self.search.trim().to_lowercase();
        self.shown = (0..self.entries.len())
            .filter(|&i| {
                let e = &self.entries[i].0;
                n.is_empty() || has(&e.name, &n) || e.publisher.as_deref().is_some_and(|p| has(p, &n)) || has(&e.command, &n) || has(&e.source, &n)
            })
            .collect();
    }

    pub fn enabled_count(&self) -> u32 {
        self.entries.iter().filter(|(e, _)| e.enabled).count() as u32
    }

    pub fn summary(&self) -> String {
        if !self.loaded {
            return String::new();
        }
        let total = self.entries.len();
        format!("{} of {} enabled", self.enabled_count(), total)
    }

    pub fn rows(&self) -> Vec<StartupRow> {
        self.shown
            .iter()
            .map(|&i| {
                let (e, icon) = &self.entries[i];
                StartupRow {
                    name: e.name.as_str().into(),
                    publisher: e.publisher.as_deref().unwrap_or("").into(),
                    enabled: e.enabled,
                    source: e.source.as_str().into(),
                    icon: icon.clone().unwrap_or_default(),
                    has_icon: icon.is_some(),
                    needs_admin: e.needs_admin,
                    missing: e.missing,
                    selected: self.selected.as_deref() == Some(e.id.as_str()),
                }
            })
            .collect()
    }

    pub fn entry_at(&self, row: usize) -> Option<&StartupEntry> {
        self.shown.get(row).map(|&i| &self.entries[i].0)
    }

    pub fn selected_entry(&self) -> Option<&StartupEntry> {
        let id = self.selected.as_deref()?;
        self.entries.iter().map(|(e, _)| e).find(|e| e.id == id)
    }

    pub fn selected_row(&self) -> i32 {
        self.selected.as_deref().and_then(|id| self.shown.iter().position(|&i| self.entries[i].0.id == id)).map_or(-1, |i| i as i32)
    }

    pub fn set_enabled(&mut self, id: &str, on: bool) {
        if let Some((e, _)) = self.entries.iter_mut().find(|(e, _)| e.id == id) {
            e.enabled = on;
        }
    }
}

// ─── Services ──────────────────────────────────────────────────────────────

/// Services Windows itself depends on; stopping one is allowed, with a warning.
const IMPORTANT: &[&str] = &[
    "rpcss", "dcomlaunch", "rpceptmapper", "lsm", "brokerinfrastructure", "power", "plugplay", "samss", "eventlog", "winmgmt", "cryptsvc", "dhcp", "dnscache", "nsi",
    "profsvc", "schedule", "systemeventsbroker", "themes", "audiosrv", "audioendpointbuilder", "coremessagingregistrar", "gpsvc", "mpssvc", "bfe", "windefend", "wscsvc",
    "usermanager", "staterepository", "appxsvc", "clipsvc", "wuauserv", "trustedinstaller", "netprofm", "nlasvc", "lanmanworkstation", "eventsystem", "sens", "fontcache",
    "tokenbroker", "cbdhsvc", "textinputmanagementservice",
];

#[derive(Default)]
pub struct Services {
    pub loaded: bool,
    pub loading: bool,
    pub list: Vec<ServiceInfo>,
    pub search: String,
    /// 0 all, 1 running, 2 stopped
    pub filter: i32,
    pub shown: Vec<usize>,
    pub selected: Option<String>,
    pub busy: Option<String>,
}

impl Services {
    pub fn set(&mut self, mut list: Vec<ServiceInfo>) {
        list.sort_by(|a, b| a.display.to_lowercase().cmp(&b.display.to_lowercase()));
        self.list = list;
        self.loaded = true;
        self.loading = false;
        self.apply_filter();
    }

    /// Takes new states and PIDs from a quick read without configuration.
    pub fn merge_states(&mut self, quick: Vec<ServiceInfo>) {
        let mut changed_set = false;
        for q in quick {
            match self.list.iter_mut().find(|s| s.name.eq_ignore_ascii_case(&q.name)) {
                Some(s) => {
                    s.state = q.state;
                    s.pid = q.pid;
                    s.can_stop = q.can_stop;
                }
                None => {
                    // A service installed since the full read: show it, details come with the next full read.
                    self.list.push(q);
                    changed_set = true;
                }
            }
        }
        if changed_set {
            self.list.sort_by(|a, b| a.display.to_lowercase().cmp(&b.display.to_lowercase()));
        }
        self.apply_filter();
    }

    pub fn apply_filter(&mut self) {
        let raw = self.search.trim().to_lowercase();
        let pid = raw.strip_prefix("pid:").and_then(|p| p.trim().parse::<u32>().ok());
        self.shown = (0..self.list.len())
            .filter(|&i| {
                let s = &self.list[i];
                let state_ok = match self.filter {
                    1 => s.state.is_running(),
                    2 => !s.state.is_running(),
                    _ => true,
                };
                if !state_ok {
                    return false;
                }
                if let Some(p) = pid {
                    return s.pid == p && p != 0;
                }
                raw.is_empty()
                    || has(&s.name, &raw)
                    || has(&s.display, &raw)
                    || s.description.as_deref().is_some_and(|d| has(d, &raw))
                    || (s.pid != 0 && s.pid.to_string() == raw)
            })
            .collect();
    }

    pub fn summary(&self) -> String {
        if !self.loaded {
            return String::new();
        }
        let running = self.list.iter().filter(|s| s.state.is_running()).count();
        format!("{} services · {} running", self.list.len(), running)
    }

    pub fn rows(&self) -> Vec<ServiceRow> {
        self.shown
            .iter()
            .map(|&i| {
                let s = &self.list[i];
                ServiceRow {
                    name: s.name.as_str().into(),
                    display: s.display.as_str().into(),
                    status: s.state.label().into(),
                    running: s.state.is_running(),
                    start: start_label(s).into(),
                    pid: if s.pid != 0 { s.pid.to_string().into() } else { SharedString::new() },
                    selected: self.selected.as_deref() == Some(s.name.as_str()),
                }
            })
            .collect()
    }

    pub fn at(&self, row: usize) -> Option<&ServiceInfo> {
        self.shown.get(row).map(|&i| &self.list[i])
    }

    pub fn selected_info(&self) -> Option<&ServiceInfo> {
        let n = self.selected.as_deref()?;
        self.list.iter().find(|s| s.name == n)
    }

    pub fn selected_row(&self) -> i32 {
        self.selected.as_deref().and_then(|n| self.shown.iter().position(|&i| self.list[i].name == n)).map_or(-1, |i| i as i32)
    }
}

pub fn start_label(s: &ServiceInfo) -> String {
    if s.triggered && matches!(s.start, StartType::Manual | StartType::Automatic | StartType::AutomaticDelayed) {
        format!("{} (trigger)", s.start.label())
    } else {
        s.start.label().into()
    }
}

pub fn is_important(name: &str) -> bool {
    IMPORTANT.contains(&name.to_ascii_lowercase().as_str())
}

/// A warning to show above a service's details, if any.
pub fn warning(s: &ServiceInfo) -> String {
    if is_important(&s.name) {
        return "Windows relies on this service. Stopping it or changing how it starts can make parts of Windows stop working until it runs again.".into();
    }
    if s.state == ServiceState::Running && !s.can_stop {
        return "This service doesn't accept requests to stop while it's running.".into();
    }
    String::new()
}

pub fn facts(s: &ServiceInfo) -> Vec<StatData> {
    let mut v = vec![stat("Startup type", start_label(s))];
    if let Some(a) = &s.account {
        v.push(stat("Runs as", a.clone()));
    }
    if s.pid != 0 {
        v.push(stat("Process ID", format!("{}{}", s.pid, if s.shared_process { " (shared with other services)" } else { "" })));
    }
    if let Some(b) = &s.binary {
        v.push(stat("Program", b.clone()));
    }
    if let Some(g) = &s.group {
        v.push(stat("Group", g.clone()));
    }
    if !s.dependencies.is_empty() {
        v.push(stat("Needs", s.dependencies.join(", ")));
    }
    if s.triggered {
        v.push(stat("Triggers", "Windows also starts it when something it watches for happens.".to_string()));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(name: &str, state: ServiceState, pid: u32) -> ServiceInfo {
        ServiceInfo {
            name: name.into(),
            display: name.to_uppercase(),
            description: None,
            state,
            start: StartType::Manual,
            pid,
            shared_process: false,
            can_stop: true,
            binary: None,
            account: None,
            dependencies: vec![],
            group: None,
            triggered: false,
        }
    }

    #[test]
    fn services_filter_and_search() {
        let mut s = Services::default();
        s.set(vec![svc("alpha", ServiceState::Running, 100), svc("beta", ServiceState::Stopped, 0), svc("gamma", ServiceState::Running, 200)]);
        assert_eq!(s.shown.len(), 3);
        s.filter = 1;
        s.apply_filter();
        assert_eq!(s.rows().iter().map(|r| r.name.to_string()).collect::<Vec<_>>(), vec!["alpha", "gamma"]);
        s.filter = 0;
        s.search = "pid:200".into();
        s.apply_filter();
        assert_eq!(s.rows().len(), 1);
        assert_eq!(s.rows()[0].name, "gamma");
    }

    #[test]
    fn quick_refresh_keeps_configuration() {
        let mut s = Services::default();
        let mut a = svc("alpha", ServiceState::Stopped, 0);
        a.account = Some("Local System".into());
        s.set(vec![a]);
        s.merge_states(vec![svc("alpha", ServiceState::Running, 42)]);
        assert_eq!(s.list[0].pid, 42);
        assert_eq!(s.list[0].account.as_deref(), Some("Local System"));
    }

    #[test]
    fn important_services_warn() {
        assert!(!warning(&svc("RpcSs", ServiceState::Running, 1)).is_empty());
        assert!(warning(&svc("SomethingElse", ServiceState::Running, 1)).is_empty());
    }
}

//! Startup apps and services: the lists, search, selection and the facts shown beside them.

use std::cmp::Ordering;

use slint::{Image, SharedString};
use tm_core::platform::{ServiceInfo, ServiceState, StartType, StartupEntry};
use tm_core::settings::TableLayout;

use crate::util::stat;
use crate::{ColumnData, ServiceRow, StartupRow, StatData};

// ─── Columns ───────────────────────────────────────────────────────────────

/// A column of a simple table. One column per table stretches: it takes the room the others leave
/// until a column is resized, then keeps its own width so dragging an edge moves only that edge.
pub struct ColSpec {
    pub key: &'static str,
    pub title: &'static str,
    pub width: f32,
    pub right: bool,
    pub stretch: bool,
}

const fn col(key: &'static str, title: &'static str, width: f32) -> ColSpec {
    ColSpec { key, title, width, right: false, stretch: false }
}

pub const STARTUP_COLS: [ColSpec; 4] = [
    ColSpec { stretch: true, ..col("name", "Name", 0.0) },
    col("publisher", "Publisher", 190.0),
    col("source", "Registered in", 210.0),
    col("status", "Status", 150.0),
];
pub const STARTUP_SORT: &str = "name";

pub const SERVICE_COLS: [ColSpec; 5] = [
    col("name", "Name", 190.0),
    ColSpec { stretch: true, ..col("display", "Display name", 0.0) },
    col("status", "Status", 120.0),
    col("start", "Startup type", 170.0),
    ColSpec { right: true, ..col("pid", "PID", 80.0) },
];
pub const SERVICE_SORT: &str = "display";

pub const MIN_WIDTH: f32 = 48.0;

fn sort_key<'a>(t: &'a TableLayout, default: &'a str) -> &'a str {
    if t.sort.is_empty() { default } else { &t.sort }
}

/// The columns to show, and the summed width of all but the stretching one. A stretching column
/// that hasn't been resized has width 0: the view gives it what's left.
pub fn columns(specs: &[ColSpec], t: &TableLayout, default_sort: &str) -> (Vec<ColumnData>, f32) {
    let key = sort_key(t, default_sort);
    let mut fixed = 0.0;
    let cols = specs
        .iter()
        .map(|c| {
            let width = t.widths.get(c.key).map(|w| *w as f32).unwrap_or(c.width);
            if !c.stretch {
                fixed += width;
            }
            ColumnData { title: c.title.into(), sub: SharedString::new(), width, right: c.right, sort: if c.key == key { if t.desc { 2 } else { 1 } } else { 0 } }
        })
        .collect();
    (cols, fixed)
}

/// A click on a column title: sort by it, or turn the order round if it already is.
pub fn sort_clicked(specs: &[ColSpec], t: &mut TableLayout, default_sort: &str, i: usize) {
    let Some(c) = specs.get(i) else { return };
    if sort_key(t, default_sort) == c.key {
        t.desc = !t.desc;
    } else {
        t.sort = c.key.into();
        t.desc = false;
    }
}

pub fn resized(specs: &[ColSpec], t: &mut TableLayout, i: usize, w: f32) {
    if let Some(c) = specs.get(i) {
        t.widths.insert(c.key.into(), w.clamp(MIN_WIDTH, 1600.0).round() as u32);
    }
}

/// Before any edge moves, the stretching column keeps the width it has on screen. Returns whether
/// anything changed.
pub fn pin_stretch(specs: &[ColSpec], t: &mut TableLayout, current: f32) -> bool {
    match specs.iter().find(|c| c.stretch) {
        Some(c) if !t.widths.contains_key(c.key) => {
            t.widths.insert(c.key.into(), current.clamp(MIN_WIDTH, 2000.0).round() as u32);
            true
        }
        _ => false,
    }
}

fn lower_cmp(a: &str, b: &str) -> Ordering {
    a.to_lowercase().cmp(&b.to_lowercase())
}

/// Text that may be missing: missing goes last.
fn opt_cmp(a: Option<&str>, b: Option<&str>) -> Ordering {
    match (a.filter(|x| !x.is_empty()), b.filter(|x| !x.is_empty())) {
        (Some(x), Some(y)) => lower_cmp(x, y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

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
    /// 0 all, 1 enabled, 2 disabled
    pub show: i32,
    /// 0 anywhere, 1 registry, 2 startup folders, 3 Store apps
    pub location: i32,
    pub layout: TableLayout,
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
                let state_ok = match self.show {
                    1 => e.enabled,
                    2 => !e.enabled,
                    _ => true,
                };
                let place_ok = match self.location {
                    1 => e.source.starts_with("Registry"),
                    2 => e.source.starts_with("Startup folder"),
                    3 => e.source.starts_with("Store"),
                    _ => true,
                };
                state_ok
                    && place_ok
                    && (n.is_empty() || has(&e.name, &n) || e.publisher.as_deref().is_some_and(|p| has(p, &n)) || has(&e.command, &n) || has(&e.source, &n))
            })
            .collect();
        let key = sort_key(&self.layout, STARTUP_SORT).to_string();
        let desc = self.layout.desc;
        let entries = &self.entries;
        self.shown.sort_by(|&a, &b| {
            let (x, y) = (&entries[a].0, &entries[b].0);
            let o = match key.as_str() {
                "publisher" => opt_cmp(x.publisher.as_deref(), y.publisher.as_deref()),
                "source" => lower_cmp(&x.source, &y.source),
                // Enabled first.
                "status" => y.enabled.cmp(&x.enabled),
                _ => lower_cmp(&x.name, &y.name),
            };
            let o = if desc { o.reverse() } else { o };
            // Ties by name, A to Z.
            o.then_with(|| lower_cmp(&x.name, &y.name))
        });
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
    /// 0 any, 1 automatic, 2 manual, 3 disabled
    pub start_filter: i32,
    pub layout: TableLayout,
}

/// Order of startup types when sorting: what starts on its own first.
fn start_rank(t: StartType) -> u8 {
    match t {
        StartType::Boot => 0,
        StartType::System => 1,
        StartType::Automatic => 2,
        StartType::AutomaticDelayed => 3,
        StartType::Manual => 4,
        StartType::Disabled => 5,
        StartType::Unknown => 6,
    }
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
                let start_ok = match self.start_filter {
                    1 => matches!(s.start, StartType::Automatic | StartType::AutomaticDelayed | StartType::Boot | StartType::System),
                    2 => s.start == StartType::Manual,
                    3 => s.start == StartType::Disabled,
                    _ => true,
                };
                if !state_ok || !start_ok {
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
        let key = sort_key(&self.layout, SERVICE_SORT).to_string();
        let desc = self.layout.desc;
        let list = &self.list;
        self.shown.sort_by(|&a, &b| {
            let (x, y) = (&list[a], &list[b]);
            let o = match key.as_str() {
                "name" => lower_cmp(&x.name, &y.name),
                // Running first.
                "status" => y.state.is_running().cmp(&x.state.is_running()).then_with(|| lower_cmp(x.state.label(), y.state.label())),
                "start" => start_rank(x.start).cmp(&start_rank(y.start)).then_with(|| x.triggered.cmp(&y.triggered)),
                // Services without a process of their own go last.
                "pid" => (x.pid == 0).cmp(&(y.pid == 0)).then_with(|| x.pid.cmp(&y.pid)),
                _ => lower_cmp(&x.display, &y.display),
            };
            let o = if desc { o.reverse() } else { o };
            o.then_with(|| lower_cmp(&x.display, &y.display))
        });
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
        match s.start.label().strip_suffix(")") {
            // "Automatic (delayed)" becomes "Automatic (delayed, trigger)".
            Some(open) => format!("{open}, trigger)"),
            None => format!("{} (trigger)", s.start.label()),
        }
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
    fn services_sort_and_filter_by_startup_type() {
        let mut s = Services::default();
        let mut a = svc("alpha", ServiceState::Running, 300);
        a.start = StartType::Automatic;
        let mut b = svc("beta", ServiceState::Stopped, 0);
        b.start = StartType::Disabled;
        let c = svc("gamma", ServiceState::Running, 100);
        s.set(vec![a, b, c]);
        s.layout.sort = "pid".into();
        s.apply_filter();
        assert_eq!(s.rows().iter().map(|r| r.name.to_string()).collect::<Vec<_>>(), vec!["gamma", "alpha", "beta"]);
        s.layout.desc = true;
        s.apply_filter();
        assert_eq!(s.rows()[0].name, "beta");
        s.start_filter = 3;
        s.apply_filter();
        assert_eq!(s.rows().iter().map(|r| r.name.to_string()).collect::<Vec<_>>(), vec!["beta"]);
    }

    #[test]
    fn a_stretching_column_is_pinned_once_and_sorting_flips() {
        let mut t = TableLayout::default();
        let (cols, fixed) = columns(&SERVICE_COLS, &t, SERVICE_SORT);
        assert_eq!(cols[1].width, 0.0);
        assert_eq!(fixed, 190.0 + 120.0 + 170.0 + 80.0);
        assert_eq!(cols[1].sort, 1);
        assert!(pin_stretch(&SERVICE_COLS, &mut t, 333.0));
        assert!(!pin_stretch(&SERVICE_COLS, &mut t, 500.0));
        assert_eq!(columns(&SERVICE_COLS, &t, SERVICE_SORT).0[1].width, 333.0);
        sort_clicked(&SERVICE_COLS, &mut t, SERVICE_SORT, 1);
        assert!(t.desc);
        sort_clicked(&SERVICE_COLS, &mut t, SERVICE_SORT, 4);
        assert_eq!((t.sort.as_str(), t.desc), ("pid", false));
        resized(&SERVICE_COLS, &mut t, 0, 10.0);
        assert_eq!(t.widths["name"], MIN_WIDTH as u32);
    }

    #[test]
    fn important_services_warn() {
        assert!(!warning(&svc("RpcSs", ServiceState::Running, 1)).is_empty());
        assert!(warning(&svc("SomethingElse", ServiceState::Running, 1)).is_empty());
    }
}

//! The process table: rows, cells, selection and keyboard movement. Pure state, no Slint windows,
//! so the controller decides when to push it to the view.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use slint::{Image, ModelRc, SharedString, VecModel};
use tm_core::format;
use tm_core::platform::{Priority, ProcDetails};
use tm_core::settings::{Column, Settings};
use tm_core::table::{self, Agg, Query, Row, RowId, RowKind};
use tm_core::{Frame, ProcKey, ProcNow, ProcStatus};

use crate::{CellData, ColumnData, ProcRowData};

pub const NAME_MIN: f32 = 240.0;

pub fn default_width(c: Column) -> f32 {
    match c {
        Column::Name => NAME_MIN,
        Column::Pid => 72.0,
        Column::Status => 118.0,
        Column::User => 130.0,
        Column::Cpu => 78.0,
        Column::Memory => 100.0,
        Column::Disk => 92.0,
        Column::Gpu => 72.0,
        Column::GpuEngine => 110.0,
        Column::Path => 300.0,
        Column::CommandLine => 300.0,
        Column::Started => 150.0,
        Column::Priority => 104.0,
        Column::Threads => 80.0,
        Column::Handles => 82.0,
        Column::Commit => 104.0,
        Column::WorkingSet => 104.0,
        Column::CpuTime => 92.0,
        Column::Description => 210.0,
        Column::Publisher => 170.0,
    }
}

pub fn width(s: &Settings, c: Column) -> f32 {
    s.widths.get(&c).map(|w| *w as f32).unwrap_or_else(|| default_width(c))
}

/// Text columns read left to right; numbers line up on the right.
fn right_aligned(c: Column) -> bool {
    c.numeric() && c != Column::Started
}

#[derive(Default)]
pub struct Table {
    pub procs: Arc<Vec<ProcNow>>,
    pub rows: Vec<Row>,
    pub search: String,
    pub toggled: HashSet<RowId>,
    pub selected: HashSet<RowId>,
    pub anchor: Option<RowId>,
    pub cursor: Option<RowId>,
    /// The process whose details panel is open, and what was read about it.
    pub details: Option<ProcKey>,
    pub details_static: Option<(ProcKey, ProcDetails)>,
    /// Ids of the rows currently in the view, to tell a refresh from a rearrangement.
    shown: Vec<RowId>,
    /// While the pointer is over the rows, they keep their order instead of re-sorting each tick.
    pub hold: bool,
    order: Vec<RowId>,
}

pub struct CellContext<'a> {
    pub frame: &'a Frame,
    pub mem_total: u64,
    pub icons: &'a HashMap<u32, Image>,
}

impl Table {
    pub fn set_frame(&mut self, f: &Frame) {
        if f.has_processes {
            self.procs = f.processes.clone();
            // Forget selected processes that have exited.
            let alive: HashSet<ProcKey> = self.procs.iter().map(|p| p.key).collect();
            self.selected.retain(|id| !matches!(id, RowId::Proc(k) if !alive.contains(k)));
            if self.anchor.as_ref().is_some_and(|id| matches!(id, RowId::Proc(k) if !alive.contains(k))) {
                self.anchor = None;
            }
        }
    }

    pub fn rebuild(&mut self, s: &Settings) {
        let rows = table::build(
            &self.procs,
            &Query { view: s.view, sort: s.sort, desc: s.sort_desc, search: &self.search, toggled: &self.toggled, friendly: s.friendly_names },
        );
        self.rows = if self.hold { table::keep_order(rows, &self.order) } else { rows };
        self.order.clear();
        self.order.extend(self.rows.iter().map(|r| r.id.clone()));
    }

    pub fn index_of(&self, id: &RowId) -> Option<usize> {
        self.rows.iter().position(|r| &r.id == id)
    }

    fn proc_by_key(&self, key: ProcKey) -> Option<&ProcNow> {
        self.procs.iter().find(|p| p.key == key)
    }

    /// The processes behind the selection, each once.
    pub fn selected_procs(&self) -> Vec<&ProcNow> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        // Keep table order for a predictable confirmation list.
        let mut ids: Vec<&RowId> = self.selected.iter().collect();
        ids.sort_by_key(|id| self.index_of(id).unwrap_or(usize::MAX));
        for id in ids {
            match id {
                RowId::Proc(k) => {
                    if let Some(p) = self.proc_by_key(*k) {
                        if seen.insert(p.key) {
                            out.push(p);
                        }
                    }
                }
                RowId::Group(name) => {
                    let members: Vec<&ProcNow> = match self.rows.iter().find(|r| &r.id == id).map(|r| &r.kind) {
                        Some(RowKind::Group { members }) => members.iter().map(|&i| &self.procs[i]).collect(),
                        _ => self.procs.iter().filter(|p| p.name.to_lowercase() == **name).collect(),
                    };
                    for p in members {
                        if seen.insert(p.key) {
                            out.push(p);
                        }
                    }
                }
                RowId::Section(_) => {}
            }
        }
        out
    }

    /// The process that stands for the selection when only one thing is selected.
    pub fn single(&self) -> Option<&ProcNow> {
        if self.selected.len() != 1 {
            return None;
        }
        match self.selected.iter().next()? {
            RowId::Proc(k) => self.proc_by_key(*k),
            RowId::Group(_) => self.selected_procs().into_iter().next(),
            RowId::Section(_) => None,
        }
    }

    pub fn single_is_group(&self) -> bool {
        self.selected.len() == 1 && matches!(self.selected.iter().next(), Some(RowId::Group(_)))
    }

    pub fn select_only(&mut self, id: RowId) {
        self.selected.clear();
        self.selected.insert(id.clone());
        self.anchor = Some(id.clone());
        self.cursor = Some(id);
    }

    /// A click on row `i`.
    pub fn press(&mut self, i: usize, ctrl: bool, shift: bool) {
        let Some(row) = self.rows.get(i) else { return };
        let id = row.id.clone();
        if matches!(id, RowId::Section(_)) {
            self.toggle(i);
            return;
        }
        if shift {
            let from = self.anchor.as_ref().and_then(|a| self.index_of(a)).unwrap_or(i);
            let (a, b) = if from <= i { (from, i) } else { (i, from) };
            if !ctrl {
                self.selected.clear();
            }
            for r in &self.rows[a..=b] {
                if !matches!(r.id, RowId::Section(_)) {
                    self.selected.insert(r.id.clone());
                }
            }
            self.cursor = Some(id);
        } else if ctrl {
            if !self.selected.remove(&id) {
                self.selected.insert(id.clone());
            }
            self.anchor = Some(id.clone());
            self.cursor = Some(id);
        } else {
            self.select_only(id);
        }
    }

    pub fn toggle(&mut self, i: usize) {
        if let Some(r) = self.rows.get(i) {
            if r.expandable && !self.toggled.remove(&r.id) {
                self.toggled.insert(r.id.clone());
            }
        }
    }

    /// Opens or closes the row under the cursor (arrow keys).
    pub fn expand_cursor(&mut self, open: bool) {
        let Some(i) = self.cursor.as_ref().and_then(|c| self.index_of(c)) else { return };
        let r = &self.rows[i];
        if r.expandable && r.expanded != open {
            self.toggle(i);
        } else if !open && r.depth > 0 {
            // Left on a child moves to its parent, as in a tree view.
            if let Some(p) = (0..i).rev().find(|&j| self.rows[j].depth < r.depth) {
                let id = self.rows[p].id.clone();
                if !matches!(id, RowId::Section(_)) {
                    self.select_only(id);
                }
            }
        }
    }

    /// Moves the cursor by `delta` rows. Returns the new cursor index.
    pub fn move_cursor(&mut self, delta: i32, shift: bool) -> Option<usize> {
        if self.rows.is_empty() {
            return None;
        }
        let cur = self.cursor.as_ref().and_then(|c| self.index_of(c));
        let last = self.rows.len() as i64 - 1;
        let mut i = match cur {
            None => if delta >= 0 { 0 } else { last },
            Some(c) => (c as i64 + delta as i64).clamp(0, last),
        } as usize;
        // Section headings aren't selectable: step past them.
        let step: i64 = if delta >= 0 { 1 } else { -1 };
        while matches!(self.rows[i].id, RowId::Section(_)) {
            let next = i as i64 + step;
            if !(0..=last).contains(&next) {
                // Nothing beyond: try the other way.
                let back = (0..self.rows.len()).rev().find(|&j| !matches!(self.rows[j].id, RowId::Section(_)));
                i = back?;
                break;
            }
            i = next as usize;
        }
        let id = self.rows[i].id.clone();
        if shift {
            let a = self.anchor.as_ref().and_then(|a| self.index_of(a)).unwrap_or(i);
            let (lo, hi) = if a <= i { (a, i) } else { (i, a) };
            self.selected.clear();
            for r in &self.rows[lo..=hi] {
                if !matches!(r.id, RowId::Section(_)) {
                    self.selected.insert(r.id.clone());
                }
            }
            self.cursor = Some(id);
        } else {
            self.select_only(id);
        }
        Some(i)
    }

    pub fn select_all(&mut self) {
        self.selected = self.rows.iter().filter(|r| !matches!(r.id, RowId::Section(_))).map(|r| r.id.clone()).collect();
    }

    /// Makes sure `key` is visible (its group open) and selects it.
    pub fn reveal(&mut self, key: ProcKey, s: &Settings) {
        self.search.clear();
        if let Some(p) = self.proc_by_key(key) {
            let group = RowId::Group(p.name.to_lowercase().into());
            self.toggled.insert(group);
        }
        self.rebuild(s);
        if self.index_of(&RowId::Proc(key)).is_none() {
            // In the grouped view the toggled set may have closed it instead; undo that.
            if let Some(p) = self.proc_by_key(key) {
                self.toggled.remove(&RowId::Group(p.name.to_lowercase().into()));
            }
            self.rebuild(s);
        }
        self.select_only(RowId::Proc(key));
    }

    /// Rows for the view. Returns them and whether the arrangement changed since last time
    /// (otherwise the caller updates rows in place).
    pub fn view_rows(&mut self, s: &Settings, cx: &CellContext) -> (Vec<ProcRowData>, bool) {
        let ids: Vec<RowId> = self.rows.iter().map(|r| r.id.clone()).collect();
        let changed = ids != self.shown;
        self.shown = ids;
        let status_col = s.columns.contains(&Column::Status);
        let rows = self.rows.iter().map(|r| self.row_data(r, s, cx, status_col)).collect();
        (rows, changed)
    }

    fn row_data(&self, r: &Row, s: &Settings, cx: &CellContext, status_col: bool) -> ProcRowData {
        let mut d = ProcRowData {
            depth: r.depth as i32,
            expandable: r.expandable,
            expanded: r.expanded,
            selected: self.selected.contains(&r.id),
            dim: r.dim,
            ..Default::default()
        };
        let cells: Vec<CellData> = match &r.kind {
            RowKind::Section { section, groups } => {
                d.kind = 0;
                d.name = section.label().into();
                d.count = format!("({groups})").into();
                d.selected = false;
                s.columns.iter().map(|_| CellData::default()).collect()
            }
            RowKind::Group { members } => {
                let first = &self.procs[members[0]];
                let agg = r.agg.unwrap_or_else(|| Agg::of(members.iter().map(|&i| &self.procs[i])));
                d.kind = 1;
                d.name = table::group_name(first, s.friendly_names).as_ref().into();
                d.count = format!("({})", members.len()).into();
                if let Some(img) = members.iter().find_map(|&i| self.procs[i].meta.as_ref().and_then(|m| m.icon)).and_then(|id| cx.icons.get(&id)) {
                    d.icon = img.clone();
                    d.has_icon = true;
                }
                let hung = members.iter().any(|&i| self.procs[i].status == ProcStatus::NotResponding);
                let suspended = members.iter().all(|&i| self.procs[i].status == ProcStatus::Suspended);
                let status = if hung { ProcStatus::NotResponding } else if suspended { ProcStatus::Suspended } else { ProcStatus::Running };
                if !status_col {
                    d.status = status_code(status);
                }
                let same_user = members.iter().all(|&i| self.procs[i].user() == first.user());
                s.columns.iter().map(|&c| group_cell(c, first, &agg, status, same_user, cx)).collect()
            }
            RowKind::Proc { index } => {
                let p = &self.procs[*index];
                d.kind = 2;
                d.name = table::display_name(p, s.friendly_names).as_ref().into();
                if let Some(img) = p.meta.as_ref().and_then(|m| m.icon).and_then(|id| cx.icons.get(&id)) {
                    d.icon = img.clone();
                    d.has_icon = true;
                }
                if !status_col {
                    d.status = status_code(p.status);
                }
                s.columns.iter().map(|&c| proc_cell(c, p, cx)).collect()
            }
        };
        d.cells = ModelRc::new(VecModel::from(cells));
        d
    }

    pub fn columns(&self, s: &Settings, f: &Frame) -> (ColumnData, Vec<ColumnData>, f32) {
        let sort = |c: Column| if s.sort == c { if s.sort_desc { 2 } else { 1 } } else { 0 };
        let name = ColumnData { title: "Name".into(), sub: SharedString::new(), width: width(s, Column::Name), right: false, sort: sort(Column::Name) };
        let cols: Vec<ColumnData> = s
            .columns
            .iter()
            .map(|&c| {
                let title = if c == Column::Disk && !f.process_io_is_disk { "I/O" } else { c.label() };
                ColumnData { title: title.into(), sub: column_sub(c, f).into(), width: width(s, c), right: right_aligned(c), sort: sort(c) }
            })
            .collect();
        let total = cols.iter().map(|c| c.width).sum();
        (name, cols, total)
    }

    /// "312 processes" or "4 of 312 match".
    pub fn summary(&self, f: &Frame) -> String {
        let total = self.procs.iter().filter(|p| p.key.pid != 0).count();
        if total == 0 {
            return String::new();
        }
        let mut s = if self.search.trim().is_empty() {
            format!("{} processes", format::count(total as u64))
        } else {
            let shown = self.rows.iter().filter(|r| matches!(r.kind, RowKind::Proc { .. }) && !r.dim).count()
                + self.rows.iter().filter_map(|r| if let RowKind::Group { members } = &r.kind { (!r.expanded).then_some(members.len()) } else { None }).sum::<usize>();
            format!("{} of {} match", format::count(shown as u64), format::count(total as u64))
        };
        if let Some(cpu) = f.cpu.total {
            s.push_str(&format!(" · CPU {}", format::pct(cpu)));
        }
        if let Some(m) = f.mem {
            s.push_str(&format!(" · Memory {}", format::pct(m.used_pct())));
        }
        s
    }
}

fn status_code(s: ProcStatus) -> i32 {
    match s {
        ProcStatus::Running => 0,
        ProcStatus::Suspended => 1,
        ProcStatus::NotResponding => 2,
    }
}

fn status_text(s: ProcStatus) -> &'static str {
    match s {
        ProcStatus::Running => "",
        ProcStatus::Suspended => "Suspended",
        ProcStatus::NotResponding => "Not responding",
    }
}

fn column_sub(c: Column, f: &Frame) -> String {
    match c {
        Column::Cpu => f.cpu.total.map(format::pct).unwrap_or_default(),
        Column::Memory => f.mem.map(|m| format::pct(m.used_pct())).unwrap_or_default(),
        Column::Disk => f.disks.iter().filter_map(|d| d.active).fold(None, |m: Option<f32>, v| Some(m.map_or(v, |m| m.max(v)))).map(format::pct).unwrap_or_default(),
        Column::Gpu => f.gpus.iter().filter_map(|g| g.util).fold(None, |m: Option<f32>, v| Some(m.map_or(v, |m| m.max(v)))).map(format::pct).unwrap_or_default(),
        _ => String::new(),
    }
}

fn heat_pct(v: Option<f32>) -> f32 {
    v.map_or(0.0, |v| (v / 50.0).clamp(0.0, 1.0))
}

fn heat_mem(b: u64, total: u64) -> f32 {
    if total == 0 { 0.0 } else { (b as f64 / total as f64 * 8.0).clamp(0.0, 1.0) as f32 }
}

fn heat_rate(v: Option<f64>) -> f32 {
    v.map_or(0.0, |v| (v / (20.0 * 1024.0 * 1024.0)).clamp(0.0, 1.0) as f32)
}

fn cell(text: impl Into<SharedString>, heat: f32) -> CellData {
    CellData { text: text.into(), heat: if heat < 0.02 { 0.0 } else { heat } }
}

const UNKNOWN: &str = "—";

fn proc_cell(c: Column, p: &ProcNow, cx: &CellContext) -> CellData {
    let meta = p.meta.as_deref();
    match c {
        Column::Name => cell(p.name.as_ref(), 0.0),
        Column::Pid => cell(p.key.pid.to_string(), 0.0),
        Column::Status => cell(status_text(p.status), 0.0),
        Column::User => cell(p.user().unwrap_or(""), 0.0),
        Column::Cpu => cell(p.cpu.map(format::pct_column).unwrap_or_else(|| UNKNOWN.into()), heat_pct(p.cpu)),
        Column::Memory => cell(format::mem_column(p.private_ws), heat_mem(p.private_ws, cx.mem_total)),
        Column::Disk => cell(p.disk().map(format::rate_column).unwrap_or_else(|| UNKNOWN.into()), heat_rate(p.disk())),
        Column::Gpu => {
            if !cx.frame.process_gpu {
                cell(UNKNOWN, 0.0)
            } else {
                cell(p.gpu.map(format::pct_column).unwrap_or_else(|| UNKNOWN.into()), heat_pct(p.gpu))
            }
        }
        Column::GpuEngine => cell(if cx.frame.process_gpu { p.gpu_engine.as_deref().unwrap_or("") } else { "" }, 0.0),
        Column::Path | Column::CommandLine => cell(p.path().unwrap_or(""), 0.0),
        Column::Started => cell(meta.and_then(|m| m.started.as_deref()).unwrap_or(""), 0.0),
        Column::Priority => cell(Priority::from_base(p.base_priority).map_or("", |p| p.label()), 0.0),
        Column::Threads => cell(format::count(p.threads as u64), 0.0),
        Column::Handles => cell(format::count(p.handles as u64), 0.0),
        Column::Commit => cell(format::mem_column(p.commit), 0.0),
        Column::WorkingSet => cell(format::mem_column(p.working_set), 0.0),
        Column::CpuTime => cell(format::cpu_time(p.cpu_time), 0.0),
        Column::Description => cell(p.description().unwrap_or(""), 0.0),
        Column::Publisher => cell(meta.and_then(|m| m.company.as_deref()).unwrap_or(""), 0.0),
    }
}

fn group_cell(c: Column, first: &ProcNow, a: &Agg, status: ProcStatus, same_user: bool, cx: &CellContext) -> CellData {
    match c {
        Column::Pid | Column::Started | Column::Priority | Column::GpuEngine => cell("", 0.0),
        Column::Status => cell(status_text(status), 0.0),
        Column::User => cell(if same_user { first.user().unwrap_or("") } else { "" }, 0.0),
        Column::Cpu => cell(a.cpu.map(format::pct_column).unwrap_or_else(|| UNKNOWN.into()), heat_pct(a.cpu)),
        Column::Memory => cell(format::mem_column(a.private_ws), heat_mem(a.private_ws, cx.mem_total)),
        Column::Disk => cell(a.disk().map(format::rate_column).unwrap_or_else(|| UNKNOWN.into()), heat_rate(a.disk())),
        Column::Gpu => {
            if !cx.frame.process_gpu {
                cell(UNKNOWN, 0.0)
            } else {
                cell(a.gpu.map(format::pct_column).unwrap_or_else(|| UNKNOWN.into()), heat_pct(a.gpu))
            }
        }
        Column::Threads => cell(format::count(a.threads), 0.0),
        Column::Handles => cell(format::count(a.handles), 0.0),
        Column::Commit => cell(format::mem_column(a.commit), 0.0),
        Column::WorkingSet => cell(format::mem_column(a.working_set), 0.0),
        Column::CpuTime => cell(format::cpu_time(a.cpu_time), 0.0),
        _ => proc_cell(c, first, cx),
    }
}

/// Whether two rows look the same, so an unchanged row isn't pushed to the view again.
pub fn same_row(a: &ProcRowData, b: &ProcRowData) -> bool {
    use slint::Model;
    a.kind == b.kind
        && a.depth == b.depth
        && a.expandable == b.expandable
        && a.expanded == b.expanded
        && a.selected == b.selected
        && a.dim == b.dim
        && a.name == b.name
        && a.count == b.count
        && a.has_icon == b.has_icon
        && a.icon == b.icon
        && a.status == b.status
        && a.cells.row_count() == b.cells.row_count()
        && a.cells.iter().zip(b.cells.iter()).all(|(x, y)| x == y)
}

/// Pushes rows to the model: in place when only values changed, otherwise all at once.
pub fn push_rows(model: &Rc<VecModel<ProcRowData>>, rows: Vec<ProcRowData>, rearranged: bool) {
    use slint::Model;
    if rearranged || model.row_count() != rows.len() {
        model.set_vec(rows);
        return;
    }
    for (i, r) in rows.into_iter().enumerate() {
        if model.row_data(i).is_none_or(|old| !same_row(&old, &r)) {
            model.set_row_data(i, r);
        }
    }
}

/// Text for "Copy → All details" and the details panel.
pub fn describe(p: &ProcNow, d: Option<&ProcDetails>, friendly: bool) -> Vec<(&'static str, String)> {
    let mut v: Vec<(&'static str, String)> = Vec::new();
    let mut push = |label: &'static str, value: Option<String>| {
        if let Some(value) = value.filter(|s| !s.is_empty()) {
            v.push((label, value));
        }
    };
    push("Name", Some(table::display_name(p, friendly).into_owned()));
    push("File", Some(p.name.to_string()));
    push("PID", Some(p.key.pid.to_string()));
    push("Status", Some(match p.status {
        ProcStatus::Running => "Running".into(),
        s => status_text(s).into(),
    }));
    push("Description", d.and_then(|d| d.description.clone()).or_else(|| p.description().map(str::to_string)));
    push("Publisher", d.and_then(|d| d.company.clone()).or_else(|| p.meta.as_ref().and_then(|m| m.company.as_deref().map(str::to_string))));
    push("Version", d.and_then(|d| d.version.clone()));
    push("Path", d.and_then(|d| d.path.clone()).or_else(|| p.path().map(str::to_string)));
    push("Command line", d.and_then(|d| d.command_line.clone()));
    push("User", d.and_then(|d| d.user.clone()).or_else(|| p.user().map(str::to_string)));
    push("Started", d.and_then(|d| d.started.clone()).or_else(|| p.meta.as_ref().and_then(|m| m.started.as_deref().map(str::to_string))));
    push("Priority", d.and_then(|d| d.priority).or_else(|| Priority::from_base(p.base_priority)).map(|p| p.label().to_string()));
    push("Architecture", d.and_then(|d| d.architecture.clone()));
    push("Administrator", d.and_then(|d| d.elevated).map(|e| if e { "Yes" } else { "No" }.to_string()));
    push("Efficiency mode", d.and_then(|d| d.efficiency_mode).map(|e| if e { "On" } else { "Off" }.to_string()));
    push("CPU", p.cpu.map(format::pct_column));
    push("CPU time", Some(format::cpu_time(p.cpu_time)));
    push("Memory (private working set)", Some(format::bytes(p.private_ws)));
    push("Working set", Some(format::bytes(p.working_set)));
    push("Peak working set", d.and_then(|d| d.peak_working_set).map(format::bytes));
    push("Commit size", Some(format::bytes(p.commit)));
    push("Page faults", d.and_then(|d| d.page_faults).map(format::count));
    push("Threads", Some(format::count(p.threads as u64)));
    push("Handles", Some(format::count(p.handles as u64)));
    push("Session", Some(p.session.to_string()));
    if !p.services.is_empty() {
        push("Services", Some(p.services.iter().map(|(n, disp)| format!("{disp} ({n})")).collect::<Vec<_>>().join("\n")));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use tm_core::settings::ViewMode;

    fn p(pid: u32, name: &str) -> ProcNow {
        ProcNow { key: ProcKey { pid, created: pid as u64 }, name: name.into(), cpu: Some(1.0), ..Default::default() }
    }

    fn table() -> (Table, Settings) {
        let mut s = Settings::default();
        s.view = ViewMode::List;
        s.sort = Column::Pid;
        s.sort_desc = false;
        let mut t = Table::default();
        t.procs = Arc::new(vec![p(10, "a.exe"), p(20, "b.exe"), p(30, "c.exe"), p(40, "d.exe")]);
        t.rebuild(&s);
        (t, s)
    }

    #[test]
    fn shift_click_selects_a_range() {
        let (mut t, _) = table();
        t.press(0, false, false);
        t.press(2, false, true);
        let pids: Vec<u32> = t.selected_procs().iter().map(|p| p.key.pid).collect();
        assert_eq!(pids, vec![10, 20, 30]);
        t.press(3, true, false);
        assert_eq!(t.selected_procs().len(), 4);
        t.press(1, true, false);
        assert_eq!(t.selected_procs().len(), 3, "ctrl-click toggles");
    }

    #[test]
    fn keyboard_moves_and_clamps() {
        let (mut t, _) = table();
        assert_eq!(t.move_cursor(1, false), Some(0), "first press lands on the first row");
        assert_eq!(t.move_cursor(100, false), Some(3));
        assert_eq!(t.move_cursor(-1, true), Some(2));
        assert_eq!(t.selected_procs().len(), 2);
    }

    #[test]
    fn exited_processes_leave_the_selection() {
        let (mut t, s) = table();
        t.press(1, false, false);
        let f = Frame { has_processes: true, processes: Arc::new(vec![p(10, "a.exe")]), ..Default::default() };
        t.set_frame(&f);
        t.rebuild(&s);
        assert!(t.selected.is_empty());
    }

    #[test]
    fn unknown_values_are_not_zero() {
        let f = Frame::default();
        let icons = HashMap::new();
        let cx = CellContext { frame: &f, mem_total: 0, icons: &icons };
        let mut x = p(1, "x.exe");
        x.cpu = None;
        assert_eq!(proc_cell(Column::Cpu, &x, &cx).text, UNKNOWN);
        assert_eq!(proc_cell(Column::Gpu, &x, &cx).text, UNKNOWN, "GPU wasn't sampled");
        assert_eq!(proc_cell(Column::Disk, &x, &cx).text, UNKNOWN);
    }
}

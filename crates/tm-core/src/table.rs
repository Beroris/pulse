//! The process table: search, sorting and the three views.
//!
//! - **Grouped**: Apps (programs with a window), Background processes and Windows processes, each
//!   with processes of the same program gathered under one expandable row.
//! - **Tree**: processes under the process that started them.
//! - **List**: every process on its own.
//!
//! This only arranges indexes into the frame's process list; the UI formats the visible rows.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::model::*;
use crate::settings::{Column, ViewMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Section {
    Apps,
    Background,
    Windows,
}

impl Section {
    pub fn label(self) -> &'static str {
        match self {
            Section::Apps => "Apps",
            Section::Background => "Background processes",
            Section::Windows => "Windows processes",
        }
    }
}

/// What a row stands for. Used for selection and expand/collapse state across refreshes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RowId {
    Section(Section),
    /// Lower-case executable name.
    Group(Arc<str>),
    Proc(ProcKey),
}

/// Summed values of a group of processes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Agg {
    pub cpu: Option<f32>,
    pub private_ws: u64,
    pub working_set: u64,
    pub commit: u64,
    pub read: Option<f64>,
    pub write: Option<f64>,
    pub gpu: Option<f32>,
    pub threads: u64,
    pub handles: u64,
    pub cpu_time: u64,
    pub count: usize,
}

impl Agg {
    pub fn add(&mut self, p: &ProcNow) {
        fn sum_f32(a: Option<f32>, b: Option<f32>) -> Option<f32> {
            match (a, b) {
                (None, None) => None,
                (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
            }
        }
        fn sum_f64(a: Option<f64>, b: Option<f64>) -> Option<f64> {
            match (a, b) {
                (None, None) => None,
                (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
            }
        }
        self.cpu = sum_f32(self.cpu, p.cpu).map(|v| v.min(100.0));
        self.private_ws += p.private_ws;
        self.working_set += p.working_set;
        self.commit += p.commit;
        self.read = sum_f64(self.read, p.read);
        self.write = sum_f64(self.write, p.write);
        self.gpu = sum_f32(self.gpu, p.gpu).map(|v| v.min(100.0));
        self.threads += p.threads as u64;
        self.handles += p.handles as u64;
        self.cpu_time += p.cpu_time;
        self.count += 1;
    }

    pub fn of<'a>(procs: impl IntoIterator<Item = &'a ProcNow>) -> Agg {
        let mut a = Agg::default();
        for p in procs {
            a.add(p);
        }
        a
    }

    pub fn disk(&self) -> Option<f64> {
        match (self.read, self.write) {
            (None, None) => None,
            (r, w) => Some(r.unwrap_or(0.0) + w.unwrap_or(0.0)),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RowKind {
    Section { section: Section, groups: usize },
    Group { members: Vec<usize> },
    Proc { index: usize },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: RowId,
    pub kind: RowKind,
    pub depth: u8,
    pub expandable: bool,
    pub expanded: bool,
    /// Shown only as context for a search result below it.
    pub dim: bool,
    pub agg: Option<Agg>,
}

pub struct Query<'a> {
    pub view: ViewMode,
    pub sort: Column,
    pub desc: bool,
    pub search: &'a str,
    /// Rows whose expanded state differs from the default. Groups are collapsed by default;
    /// sections and tree nodes are expanded by default.
    pub toggled: &'a HashSet<RowId>,
    pub friendly: bool,
}

/// The name shown for a process.
pub fn display_name(p: &ProcNow, friendly: bool) -> Cow<'_, str> {
    if !friendly {
        return Cow::Borrowed(&p.name);
    }
    if !p.services.is_empty() && p.name.eq_ignore_ascii_case("svchost.exe") {
        let (_, display) = &p.services[0];
        return if p.services.len() == 1 {
            Cow::Owned(format!("Service Host: {display}"))
        } else {
            Cow::Owned(format!("Service Host: {display} (+{})", p.services.len() - 1))
        };
    }
    group_name(p, friendly)
}

/// The name shown for a group of processes of the same program.
pub fn group_name(p: &ProcNow, friendly: bool) -> Cow<'_, str> {
    if friendly {
        if let Some(d) = p.description().map(str::trim).filter(|d| !d.is_empty()) {
            return Cow::Borrowed(d);
        }
    }
    Cow::Borrowed(&p.name)
}

/// Search terms. Matches name, description, path, owner, publisher, services and PID.
pub struct Matcher {
    needle: String,
    pid: Option<u32>,
    pid_only: bool,
}

impl Matcher {
    pub fn new(search: &str) -> Option<Matcher> {
        let s = search.trim();
        if s.is_empty() {
            return None;
        }
        let lower = s.to_lowercase();
        if let Some(rest) = lower.strip_prefix("pid:") {
            return Some(Matcher { needle: String::new(), pid: rest.trim().parse().ok(), pid_only: true });
        }
        let pid = lower.parse().ok();
        Some(Matcher { needle: lower, pid, pid_only: false })
    }

    pub fn matches(&self, p: &ProcNow) -> bool {
        if self.pid_only {
            return self.pid == Some(p.key.pid);
        }
        if let Some(pid) = self.pid {
            if pid == p.key.pid || p.key.pid.to_string().starts_with(&self.needle) {
                return true;
            }
        }
        let n = self.needle.as_str();
        let has = |s: &str| contains_ci(s, n);
        if has(&p.name) {
            return true;
        }
        if let Some(m) = &p.meta {
            if m.description.as_deref().is_some_and(has) || m.path.as_deref().is_some_and(has) || m.user.as_deref().is_some_and(has) || m.company.as_deref().is_some_and(has) {
                return true;
            }
        }
        p.services.iter().any(|(name, display)| has(name) || has(display))
    }
}

/// Case-insensitive substring test without allocating for ASCII haystacks.
fn contains_ci(haystack: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if haystack.is_ascii() && needle_lower.is_ascii() {
        let h = haystack.as_bytes();
        let n = needle_lower.as_bytes();
        if n.len() > h.len() {
            return false;
        }
        return h.windows(n.len()).any(|w| w.iter().zip(n).all(|(a, b)| a.to_ascii_lowercase() == *b));
    }
    haystack.to_lowercase().contains(needle_lower)
}

fn cmp_opt_f32(a: Option<f32>, b: Option<f32>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.total_cmp(&b),
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => Ordering::Equal,
    }
}

fn cmp_opt_f64(a: Option<f64>, b: Option<f64>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.total_cmp(&b),
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => Ordering::Equal,
    }
}

fn cmp_text(a: Option<&str>, b: Option<&str>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => cmp_ci(a, b),
        // Unknown text sorts last when ascending.
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn cmp_ci(a: &str, b: &str) -> Ordering {
    a.chars().flat_map(char::to_lowercase).cmp(b.chars().flat_map(char::to_lowercase))
}

fn status_rank(s: ProcStatus) -> u8 {
    match s {
        ProcStatus::Running => 0,
        ProcStatus::Suspended => 1,
        ProcStatus::NotResponding => 2,
    }
}

/// Compares two processes by `col`, ascending.
pub fn cmp_proc(a: &ProcNow, b: &ProcNow, col: Column, friendly: bool) -> Ordering {
    match col {
        Column::Name => cmp_ci(&display_name(a, friendly), &display_name(b, friendly)),
        Column::Pid => a.key.pid.cmp(&b.key.pid),
        Column::Status => status_rank(a.status).cmp(&status_rank(b.status)),
        Column::User => cmp_text(a.user(), b.user()),
        Column::Cpu => cmp_opt_f32(a.cpu, b.cpu),
        Column::Memory => a.private_ws.cmp(&b.private_ws),
        Column::Disk => cmp_opt_f64(a.disk(), b.disk()),
        Column::Gpu => cmp_opt_f32(a.gpu, b.gpu),
        Column::GpuEngine => cmp_text(a.gpu_engine.as_deref(), b.gpu_engine.as_deref()),
        Column::Path | Column::CommandLine => cmp_text(a.path(), b.path()),
        Column::Started => a.key.created.cmp(&b.key.created),
        Column::Priority => a.base_priority.cmp(&b.base_priority),
        Column::Threads => a.threads.cmp(&b.threads),
        Column::Handles => a.handles.cmp(&b.handles),
        Column::Commit => a.commit.cmp(&b.commit),
        Column::WorkingSet => a.working_set.cmp(&b.working_set),
        Column::CpuTime => a.cpu_time.cmp(&b.cpu_time),
        Column::Description => cmp_text(a.description(), b.description()),
        Column::Publisher => cmp_text(a.meta.as_ref().and_then(|m| m.company.as_deref()), b.meta.as_ref().and_then(|m| m.company.as_deref())),
    }
}

fn cmp_agg(a: &Agg, b: &Agg, col: Column) -> Option<Ordering> {
    Some(match col {
        Column::Cpu => cmp_opt_f32(a.cpu, b.cpu),
        Column::Memory => a.private_ws.cmp(&b.private_ws),
        Column::Disk => cmp_opt_f64(a.disk(), b.disk()),
        Column::Gpu => cmp_opt_f32(a.gpu, b.gpu),
        Column::Threads => a.threads.cmp(&b.threads),
        Column::Handles => a.handles.cmp(&b.handles),
        Column::Commit => a.commit.cmp(&b.commit),
        Column::WorkingSet => a.working_set.cmp(&b.working_set),
        Column::CpuTime => a.cpu_time.cmp(&b.cpu_time),
        _ => return None,
    })
}

fn sorted_indexes(procs: &[ProcNow], idx: &mut [usize], q: &Query) {
    idx.sort_by(|&a, &b| {
        let (pa, pb) = (&procs[a], &procs[b]);
        let o = cmp_proc(pa, pb, q.sort, q.friendly);
        let o = if q.desc { o.reverse() } else { o };
        o.then_with(|| cmp_ci(&display_name(pa, q.friendly), &display_name(pb, q.friendly))).then_with(|| pa.key.pid.cmp(&pb.key.pid))
    });
}

/// The processes that never appear in the table: the idle pseudo-process is not a program.
fn hidden(p: &ProcNow) -> bool {
    p.key.pid == 0
}

fn is_system(p: &ProcNow) -> bool {
    p.key.pid <= 4 || p.meta.as_ref().is_some_and(|m| m.system)
}

/// Lays out the table rows for `procs`.
pub fn build(procs: &[ProcNow], q: &Query) -> Vec<Row> {
    let matcher = Matcher::new(q.search);
    match q.view {
        ViewMode::List => build_list(procs, q, matcher.as_ref()),
        ViewMode::Grouped => build_grouped(procs, q, matcher.as_ref()),
        ViewMode::Tree => build_tree(procs, q, matcher.as_ref()),
    }
}

fn build_list(procs: &[ProcNow], q: &Query, m: Option<&Matcher>) -> Vec<Row> {
    let mut idx: Vec<usize> = (0..procs.len()).filter(|&i| !hidden(&procs[i]) && m.is_none_or(|m| m.matches(&procs[i]))).collect();
    sorted_indexes(procs, &mut idx, q);
    idx.into_iter()
        .map(|i| Row { id: RowId::Proc(procs[i].key), kind: RowKind::Proc { index: i }, depth: 0, expandable: false, expanded: false, dim: false, agg: None })
        .collect()
}

fn build_grouped(procs: &[ProcNow], q: &Query, m: Option<&Matcher>) -> Vec<Row> {
    // Group by program (lower-case executable name), keeping first-seen order for stability.
    let mut order: Vec<Arc<str>> = Vec::new();
    let mut groups: HashMap<Arc<str>, Vec<usize>> = HashMap::new();
    for (i, p) in procs.iter().enumerate() {
        if hidden(p) || m.is_some_and(|m| !m.matches(p)) {
            continue;
        }
        let key: Arc<str> = p.name.to_lowercase().into();
        groups
            .entry(key.clone())
            .or_insert_with(|| {
                order.push(key);
                Vec::new()
            })
            .push(i);
    }
    let mut sections: [(Section, Vec<(Arc<str>, Vec<usize>, Agg)>); 3] = [(Section::Apps, vec![]), (Section::Background, vec![]), (Section::Windows, vec![])];
    for key in order {
        let mut members = groups.remove(&key).unwrap_or_default();
        let section = if members.iter().any(|&i| procs[i].has_window) {
            Section::Apps
        } else if members.iter().all(|&i| is_system(&procs[i])) {
            Section::Windows
        } else {
            Section::Background
        };
        sorted_indexes(procs, &mut members, q);
        let agg = Agg::of(members.iter().map(|&i| &procs[i]));
        let slot = sections.iter_mut().find(|(s, _)| *s == section).expect("section");
        slot.1.push((key, members, agg));
    }
    let searching = m.is_some();
    let mut rows = Vec::new();
    for (section, mut list) in sections {
        if list.is_empty() {
            continue;
        }
        list.sort_by(|a, b| {
            let (pa, pb) = (&procs[a.1[0]], &procs[b.1[0]]);
            let o = match cmp_agg(&a.2, &b.2, q.sort) {
                Some(o) => o,
                None if q.sort == Column::Name => cmp_ci(&group_name(pa, q.friendly), &group_name(pb, q.friendly)),
                None => cmp_proc(pa, pb, q.sort, q.friendly),
            };
            let o = if q.desc { o.reverse() } else { o };
            o.then_with(|| cmp_ci(&group_name(pa, q.friendly), &group_name(pb, q.friendly)))
        });
        let sid = RowId::Section(section);
        let section_open = !q.toggled.contains(&sid);
        rows.push(Row {
            id: sid,
            kind: RowKind::Section { section, groups: list.len() },
            depth: 0,
            expandable: true,
            expanded: section_open,
            dim: false,
            agg: None,
        });
        if !section_open {
            continue;
        }
        for (key, members, agg) in list {
            if members.len() == 1 {
                let i = members[0];
                rows.push(Row { id: RowId::Proc(procs[i].key), kind: RowKind::Proc { index: i }, depth: 0, expandable: false, expanded: false, dim: false, agg: None });
                continue;
            }
            let gid = RowId::Group(key);
            // Groups start collapsed; while searching they start open so matches are visible.
            let open = q.toggled.contains(&gid) != searching;
            let children: Vec<Row> = if open {
                members
                    .iter()
                    .map(|&i| Row { id: RowId::Proc(procs[i].key), kind: RowKind::Proc { index: i }, depth: 1, expandable: false, expanded: false, dim: false, agg: None })
                    .collect()
            } else {
                Vec::new()
            };
            rows.push(Row { id: gid, kind: RowKind::Group { members }, depth: 0, expandable: true, expanded: open, dim: false, agg: Some(agg) });
            rows.extend(children);
        }
    }
    rows
}

fn build_tree(procs: &[ProcNow], q: &Query, m: Option<&Matcher>) -> Vec<Row> {
    let by_pid: HashMap<u32, usize> = procs.iter().enumerate().filter(|(_, p)| !hidden(p)).map(|(i, p)| (p.key.pid, i)).collect();
    // A parent only counts if it started before the child: otherwise the parent's PID was reused
    // by an unrelated process after the real parent exited.
    let parent_of = |i: usize| -> Option<usize> {
        let p = &procs[i];
        let &pi = by_pid.get(&p.ppid)?;
        (pi != i && procs[pi].key.created <= p.key.created).then_some(pi)
    };
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();
    for &i in by_pid.values() {
        match parent_of(i) {
            Some(pi) => children.entry(pi).or_default().push(i),
            None => roots.push(i),
        }
    }
    // With a search, show matches plus the chain of parents leading to them.
    let (visible, matched): (Option<HashSet<usize>>, HashSet<usize>) = match m {
        None => (None, HashSet::new()),
        Some(m) => {
            let matched: HashSet<usize> = by_pid.values().copied().filter(|&i| m.matches(&procs[i])).collect();
            let mut visible = matched.clone();
            for &i in &matched {
                let mut cur = i;
                let mut guard = 0;
                while let Some(pi) = parent_of(cur) {
                    if !visible.insert(pi) || guard > 64 {
                        break;
                    }
                    cur = pi;
                    guard += 1;
                }
            }
            (Some(visible), matched)
        }
    };
    let is_visible = |i: usize| visible.as_ref().is_none_or(|v| v.contains(&i));
    let searching = m.is_some();
    sorted_indexes(procs, &mut roots, q);
    for list in children.values_mut() {
        sorted_indexes(procs, list, q);
    }
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    let mut stack: Vec<(usize, u8)> = roots.iter().rev().filter(|&&i| is_visible(i)).map(|&i| (i, 0)).collect();
    while let Some((i, depth)) = stack.pop() {
        if !seen.insert(i) {
            continue;
        }
        let kids: Vec<usize> = children.get(&i).map(|c| c.iter().copied().filter(|&c| is_visible(c)).collect()).unwrap_or_default();
        let id = RowId::Proc(procs[i].key);
        let open = searching || !q.toggled.contains(&id);
        rows.push(Row {
            id,
            kind: RowKind::Proc { index: i },
            depth,
            expandable: !kids.is_empty(),
            expanded: open && !kids.is_empty(),
            dim: searching && !matched.contains(&i),
            agg: None,
        });
        if open {
            for &c in kids.iter().rev() {
                stack.push((c, depth.saturating_add(1)));
            }
        }
    }
    rows
}

/// Puts `rows` back in the order of `previous`, so the table holds still while the pointer is over
/// it. Siblings keep their earlier order; rows that are new go after them, in their new order.
/// Children stay under their parents.
pub fn keep_order(rows: Vec<Row>, previous: &[RowId]) -> Vec<Row> {
    if previous.is_empty() {
        return rows;
    }
    let rank: HashMap<&RowId, usize> = previous.iter().enumerate().map(|(i, id)| (id, i)).collect();
    struct Node {
        row: Row,
        children: Vec<Node>,
    }
    // Rebuild the tree from the depths, which only ever grow by one from a row to the next.
    // Section headings share depth 0 with their rows, but hold them: they sit a level above.
    fn level(r: &Row) -> u16 {
        if matches!(r.kind, RowKind::Section { .. }) { 0 } else { r.depth as u16 + 1 }
    }
    fn take(rows: &mut std::iter::Peekable<std::vec::IntoIter<Row>>, at: u16) -> Vec<Node> {
        let mut out = Vec::new();
        while let Some(r) = rows.next_if(|r| level(r) >= at) {
            let children = take(rows, level(&r) + 1);
            out.push(Node { row: r, children });
        }
        out
    }
    fn order(nodes: &mut [Node], rank: &HashMap<&RowId, usize>) {
        nodes.sort_by_key(|n| rank.get(&n.row.id).copied().unwrap_or(usize::MAX));
        for n in nodes.iter_mut() {
            order(&mut n.children, rank);
        }
    }
    fn flatten(nodes: Vec<Node>, out: &mut Vec<Row>) {
        for n in nodes {
            out.push(n.row);
            flatten(n.children, out);
        }
    }
    let len = rows.len();
    let mut it = rows.into_iter().peekable();
    let mut tree = take(&mut it, 0);
    order(&mut tree, &rank);
    let mut out = Vec::with_capacity(len);
    flatten(tree, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_order_holds_rows_still() {
        let mut procs = sample();
        let t = HashSet::new();
        let before = build(&procs, &q(ViewMode::Tree, Column::Cpu, true, "", &t));
        let ids: Vec<RowId> = before.iter().map(|r| r.id.clone()).collect();
        // Notepad gets busy and a new process starts: a fresh sort would move rows around.
        procs[7].cpu = Some(50.0);
        procs.push(p(500, 200, "new.exe", 9.0, 1));
        let fresh = build(&procs, &q(ViewMode::Tree, Column::Cpu, true, "", &t));
        assert_ne!(pids(&fresh, &procs)[..4], pids(&before, &procs)[..4]);
        let held = keep_order(fresh, &ids);
        let mut expected = pids(&before, &procs);
        let held_pids = pids(&held, &procs);
        // Same rows in the same order, with the new one placed after its old siblings.
        held_pids.iter().filter(|p| **p != 500).zip(expected.iter()).for_each(|(a, b)| assert_eq!(a, b));
        expected.push(500);
        assert_eq!(held_pids.len(), expected.len());
        // Every child still follows its parent.
        let depth_ok = held.windows(2).all(|w| w[1].depth <= w[0].depth + 1);
        assert!(depth_ok);
    }

    #[test]
    fn keep_order_keeps_new_rows_in_their_section() {
        let mut procs = sample();
        let t = HashSet::new();
        let before = build(&procs, &q(ViewMode::Grouped, Column::Cpu, true, "", &t));
        let ids: Vec<RowId> = before.iter().map(|r| r.id.clone()).collect();
        // A new app with a window: it belongs in the first section, not at the end of the list.
        let mut app = p(600, 200, "newapp.exe", 0.0, 1);
        app.has_window = true;
        procs.push(app);
        let held = keep_order(build(&procs, &q(ViewMode::Grouped, Column::Cpu, true, "", &t)), &ids);
        let section_of = |rows: &[Row], pid: u32| {
            let mut current = None;
            for r in rows {
                if let RowKind::Section { section, .. } = r.kind {
                    current = Some(section);
                }
                if r.id == RowId::Proc(ProcKey { pid, created: pid as u64 }) {
                    return current;
                }
            }
            None
        };
        let fresh = build(&procs, &q(ViewMode::Grouped, Column::Cpu, true, "", &t));
        assert_eq!(section_of(&held, 600), section_of(&fresh, 600));
        assert!(section_of(&held, 600).is_some());
        assert_eq!(held.len(), fresh.len());
    }

    fn p(pid: u32, ppid: u32, name: &str, cpu: f32, mem: u64) -> ProcNow {
        ProcNow { key: ProcKey { pid, created: pid as u64 }, ppid, name: name.into(), cpu: Some(cpu), private_ws: mem, ..Default::default() }
    }

    fn q<'a>(view: ViewMode, sort: Column, desc: bool, search: &'a str, toggled: &'a HashSet<RowId>) -> Query<'a> {
        Query { view, sort, desc, search, toggled, friendly: false }
    }

    fn pids(rows: &[Row], procs: &[ProcNow]) -> Vec<u32> {
        rows.iter()
            .filter_map(|r| match r.kind {
                RowKind::Proc { index } => Some(procs[index].key.pid),
                _ => None,
            })
            .collect()
    }

    fn sample() -> Vec<ProcNow> {
        let mut v = vec![
            p(0, 0, "System Idle Process", 95.0, 0),
            p(4, 0, "System", 0.1, 100),
            p(100, 4, "smss.exe", 0.0, 1000),
            p(200, 1, "explorer.exe", 1.0, 50_000),
            p(300, 200, "chrome.exe", 5.0, 300_000),
            p(301, 300, "chrome.exe", 2.0, 100_000),
            p(302, 300, "chrome.exe", 0.5, 50_000),
            p(400, 200, "notepad.exe", 0.0, 9_000),
        ];
        v[1].meta = Some(Arc::new(ProcMeta { system: true, ..Default::default() }));
        v[2].meta = Some(Arc::new(ProcMeta { system: true, path: Some("C:\\Windows\\System32\\smss.exe".into()), ..Default::default() }));
        v[3].has_window = true;
        v[4].has_window = true;
        v
    }

    #[test]
    fn list_sorts_by_cpu_and_hides_idle() {
        let procs = sample();
        let t = HashSet::new();
        let rows = build(&procs, &q(ViewMode::List, Column::Cpu, true, "", &t));
        // Equal CPU falls back to name order: notepad.exe before smss.exe.
        assert_eq!(pids(&rows, &procs), vec![300, 301, 200, 302, 4, 400, 100]);
    }

    #[test]
    fn ascending_text_sort_and_ties() {
        let procs = sample();
        let t = HashSet::new();
        let rows = build(&procs, &q(ViewMode::List, Column::Name, false, "", &t));
        let names: Vec<&str> = rows
            .iter()
            .map(|r| match r.kind {
                RowKind::Proc { index } => &*procs[index].name,
                _ => "",
            })
            .collect();
        assert_eq!(names, vec!["chrome.exe", "chrome.exe", "chrome.exe", "explorer.exe", "notepad.exe", "smss.exe", "System"]);
        // Equal names fall back to PID order.
        assert_eq!(pids(&rows, &procs)[..3], [300, 301, 302]);
    }

    #[test]
    fn search_matches_name_and_pid() {
        let procs = sample();
        let t = HashSet::new();
        assert_eq!(pids(&build(&procs, &q(ViewMode::List, Column::Pid, false, "CHROME", &t)), &procs), vec![300, 301, 302]);
        assert_eq!(pids(&build(&procs, &q(ViewMode::List, Column::Pid, false, "40", &t)), &procs), vec![400]);
        assert_eq!(pids(&build(&procs, &q(ViewMode::List, Column::Pid, false, "pid:4", &t)), &procs), vec![4]);
        assert!(build(&procs, &q(ViewMode::List, Column::Pid, false, "nothing-like-this", &t)).is_empty());
    }

    #[test]
    fn grouped_view_sections_and_groups() {
        let procs = sample();
        let t = HashSet::new();
        let rows = build(&procs, &q(ViewMode::Grouped, Column::Memory, true, "", &t));
        let kinds: Vec<String> = rows
            .iter()
            .map(|r| match &r.kind {
                RowKind::Section { section, groups } => format!("[{} {groups}]", section.label()),
                RowKind::Group { members } => format!("{} x{}", procs[members[0]].name, members.len()),
                RowKind::Proc { index } => procs[*index].name.to_string(),
            })
            .collect();
        assert_eq!(kinds, vec!["[Apps 2]", "chrome.exe x3", "explorer.exe", "[Background processes 1]", "notepad.exe", "[Windows processes 2]", "smss.exe", "System"]);
        let chrome = rows.iter().find(|r| matches!(r.kind, RowKind::Group { .. })).unwrap();
        assert_eq!(chrome.agg.unwrap().private_ws, 450_000);
        assert!(!chrome.expanded, "groups start collapsed");
    }

    #[test]
    fn expanding_a_group_shows_its_members_sorted() {
        let procs = sample();
        let mut t = HashSet::new();
        t.insert(RowId::Group("chrome.exe".into()));
        let rows = build(&procs, &q(ViewMode::Grouped, Column::Cpu, true, "", &t));
        let start = rows.iter().position(|r| matches!(r.kind, RowKind::Group { .. })).unwrap();
        assert!(rows[start].expanded);
        let members: Vec<u32> = rows[start + 1..start + 4]
            .iter()
            .map(|r| match r.kind {
                RowKind::Proc { index } => procs[index].key.pid,
                _ => 0,
            })
            .collect();
        assert_eq!(members, vec![300, 301, 302]);
        assert!(rows[start + 1..start + 4].iter().all(|r| r.depth == 1));
    }

    #[test]
    fn collapsing_a_section_hides_it() {
        let procs = sample();
        let mut t = HashSet::new();
        t.insert(RowId::Section(Section::Windows));
        let rows = build(&procs, &q(ViewMode::Grouped, Column::Cpu, true, "", &t));
        assert!(!pids(&rows, &procs).contains(&100));
        assert!(rows.iter().any(|r| r.id == RowId::Section(Section::Windows) && !r.expanded));
    }

    #[test]
    fn tree_nests_children_under_parents() {
        let procs = sample();
        let t = HashSet::new();
        let rows = build(&procs, &q(ViewMode::Tree, Column::Pid, false, "", &t));
        let shape: Vec<(u32, u8)> = rows
            .iter()
            .map(|r| match r.kind {
                RowKind::Proc { index } => (procs[index].key.pid, r.depth),
                _ => (0, 0),
            })
            .collect();
        assert_eq!(shape, vec![(4, 0), (100, 1), (200, 0), (300, 1), (301, 2), (302, 2), (400, 1)]);
    }

    #[test]
    fn tree_ignores_parents_that_started_later() {
        // PID 900's recorded parent PID now belongs to a process that started after it.
        let mut procs = sample();
        let mut late_parent = p(901, 1, "unrelated.exe", 0.0, 1);
        late_parent.key.created = 10_000;
        let mut child = p(900, 901, "orphan.exe", 0.0, 1);
        child.key.created = 50;
        procs.push(late_parent);
        procs.push(child);
        let t = HashSet::new();
        let rows = build(&procs, &q(ViewMode::Tree, Column::Pid, false, "", &t));
        let orphan = rows.iter().find(|r| matches!(r.kind, RowKind::Proc { index } if procs[index].key.pid == 900)).unwrap();
        assert_eq!(orphan.depth, 0);
    }

    #[test]
    fn tree_search_keeps_ancestors_dimmed() {
        let procs = sample();
        let t = HashSet::new();
        let rows = build(&procs, &q(ViewMode::Tree, Column::Pid, false, "notepad", &t));
        let shape: Vec<(u32, bool)> = rows
            .iter()
            .map(|r| match r.kind {
                RowKind::Proc { index } => (procs[index].key.pid, r.dim),
                _ => (0, false),
            })
            .collect();
        assert_eq!(shape, vec![(200, true), (400, false)]);
    }

    #[test]
    fn collapsed_tree_node_hides_descendants() {
        let procs = sample();
        let mut t = HashSet::new();
        t.insert(RowId::Proc(ProcKey { pid: 300, created: 300 }));
        let rows = build(&procs, &q(ViewMode::Tree, Column::Pid, false, "", &t));
        let ids = pids(&rows, &procs);
        assert!(ids.contains(&300) && !ids.contains(&301) && !ids.contains(&302));
    }

    #[test]
    fn friendly_names() {
        let mut a = p(10, 0, "svchost.exe", 0.0, 0);
        a.services = vec![("Dnscache".into(), "DNS Client".into())];
        assert_eq!(display_name(&a, true), "Service Host: DNS Client");
        assert_eq!(display_name(&a, false), "svchost.exe");
        let mut b = p(11, 0, "chrome.exe", 0.0, 0);
        b.meta = Some(Arc::new(ProcMeta { description: Some("Google Chrome".into()), ..Default::default() }));
        assert_eq!(display_name(&b, true), "Google Chrome");
    }
}

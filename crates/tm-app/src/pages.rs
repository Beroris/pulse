//! Overview cards, insights, the Performance page and the compact monitor, built from a frame and
//! the histories. Everything here is formatting: no state, no windows.

use slint::SharedString;
use tm_core::engine::Histories;
use tm_core::format;
use tm_core::graph::nice_ceiling;
use tm_core::history::History;
use tm_core::insights::{Insight, Severity, Target};
use tm_core::settings::Metric;
use tm_core::*;

use crate::util::{combine, graph, stat, stat_tip};
use crate::{CardData, CompactMetric, DeviceData, GraphData, InsightData, PerfGraph, Segment, StatData};


/// Icon paths from the Slint `Icons` global, read once.
#[derive(Clone, Default)]
pub struct IconSet {
    pub chip: SharedString,
    pub thermometer: SharedString,
    pub memory: SharedString,
    pub gpu: SharedString,
    pub disk: SharedString,
    pub network: SharedString,
    pub clock: SharedString,
    pub processes: SharedString,
}

/// A device on the Performance page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DevKey {
    Cpu,
    Memory,
    Disk(u32),
    Net(u64),
    Gpu(u64),
}

const CARD_POINTS: usize = 60;
const BIG_POINTS: usize = 120;
const SMALL_POINTS: usize = 80;
const MINI_POINTS: usize = 40;
/// A network graph never scales below 100 Kbps, so idle chatter stays near the bottom.
const NET_FLOOR: f64 = 100_000.0 / 8.0;
/// A disk transfer graph never scales below 1 MB/s.
const DISK_FLOOR: f64 = 1024.0 * 1024.0;

fn max_f32(it: impl Iterator<Item = f32>) -> Option<f32> {
    it.fold(None, |m: Option<f32>, v| Some(m.map_or(v, |m| m.max(v))))
}

fn sum_opt(it: impl Iterator<Item = Option<f64>>) -> Option<f64> {
    it.fold(None, |acc, v| match (acc, v) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
    })
}

fn level(pct: Option<f32>) -> f32 {
    pct.map_or(-1.0, |p| (p / 100.0).clamp(0.0, 1.0))
}

fn short_cpu_name(name: &str) -> String {
    // "AMD Ryzen 7 5800X 8-Core Processor" → "AMD Ryzen 7 5800X"; Intel names have "(R)" noise.
    let n = name.replace("(R)", "").replace("(TM)", "").replace("(tm)", "");
    let n = n.split(" with ").next().unwrap_or(&n).to_string();
    let words: Vec<&str> = n.split_whitespace().filter(|w| !w.ends_with("-Core") && *w != "Processor" && *w != "CPU").collect();
    let s = words.join(" ");
    s.split(" @ ").next().unwrap_or(&s).trim().to_string()
}

pub fn machine_line(info: &SystemInfo) -> String {
    [info.host.as_str(), info.os.as_str(), &short_cpu_name(&info.cpu.name)].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · ")
}

fn disk_kind(d: &DiskInfo) -> String {
    let kind = match d.kind {
        DiskKind::Ssd => "SSD",
        DiskKind::Hdd => "HDD",
        DiskKind::Unknown => "",
    };
    let mut parts: Vec<String> = Vec::new();
    if !kind.is_empty() {
        parts.push(kind.into());
    }
    if let Some(b) = &d.bus {
        parts.push(b.clone());
    }
    if d.removable {
        parts.push("Removable".into());
    }
    parts.join(" · ")
}

// ─── Overview ──────────────────────────────────────────────────────────────

pub fn cards(f: &Frame, info: &SystemInfo, h: &Histories, icons: &IconSet) -> Vec<CardData> {
    let mut out = Vec::new();
    let empty = GraphData::default();

    // CPU
    let cpu_sub = match f.cpu.speed_mhz {
        Some(m) => format!("{} · {} logical processors", format::mhz(m), info.cpu.logical),
        None => format!("{} logical processors", info.cpu.logical),
    };
    out.push(CardData {
        key: "cpu".into(),
        title: "CPU".into(),
        value: f.cpu.total.map(format::pct).unwrap_or_default().into(),
        sub: cpu_sub.into(),
        icon: icons.chip.clone(),
        level: level(f.cpu.total),
        graph: graph(&h.cpu, 100.0, CARD_POINTS),
        graph2: empty.clone(),
        second: false,
        available: f.cpu.total.is_some() || h.cpu.is_empty(),
    });

    // CPU temperature: only a real CPU sensor counts. A firmware thermal zone is shown as what it is.
    let zone = max_f32(f.thermal.iter().filter(|t| t.kind == ThermalKind::Zone).map(|t| t.celsius));
    let (t_title, t_value, t_sub, t_ok) = match (f.cpu.temperature_c, zone) {
        (Some(c), _) => ("CPU temperature", format::celsius(c), "CPU sensor".to_string(), true),
        (None, Some(z)) => ("System temperature", format::celsius(z), "Firmware thermal zone, not the CPU itself".to_string(), true),
        (None, None) => (
            "CPU temperature",
            String::new(),
            match f.cpu.sensor {
                SensorAccess::NeedsAdmin => "Run as administrator to read the CPU's sensor".to_string(),
                _ => "Windows doesn't get a reading from this PC".to_string(),
            },
            false,
        ),
    };
    let t_val = f.cpu.temperature_c.or(zone);
    out.push(CardData {
        key: "cpu-temp".into(),
        title: t_title.into(),
        value: t_value.into(),
        sub: t_sub.into(),
        icon: icons.thermometer.clone(),
        level: t_val.map_or(-1.0, |c| (c / 100.0).clamp(0.0, 1.0)),
        graph: empty.clone(),
        graph2: empty.clone(),
        second: false,
        available: t_ok,
    });

    // Memory
    out.push(match f.mem {
        Some(m) => CardData {
            key: "mem".into(),
            title: "Memory".into(),
            value: format::pct(m.used_pct()).into(),
            sub: format!("{} of {} in use", format::bytes(m.used()), format::bytes(m.total)).into(),
            icon: icons.memory.clone(),
            level: level(Some(m.used_pct())),
            graph: graph(&h.mem_pct, 100.0, CARD_POINTS),
            graph2: empty.clone(),
            second: false,
            available: true,
        },
        None => CardData { key: "mem".into(), title: "Memory".into(), icon: icons.memory.clone(), level: -1.0, available: false, ..Default::default() },
    });

    // GPU: the first adapter (the one Windows lists first is the one driving the main display).
    let gpu = info.gpus.first().and_then(|gi| f.gpus.iter().find(|g| g.id == gi.id).map(|g| (gi, g)));
    match gpu {
        Some((gi, g)) => {
            let hist = h.gpu(g.id);
            out.push(CardData {
                key: "gpu".into(),
                title: "GPU".into(),
                value: g.util.map(format::pct).unwrap_or_default().into(),
                sub: gi.name.clone().into(),
                icon: icons.gpu.clone(),
                level: level(g.util),
                graph: hist.map(|x| graph(&x.util, 100.0, CARD_POINTS)).unwrap_or_default(),
                graph2: empty.clone(),
                second: false,
                available: g.util.is_some() || hist.is_none_or(|x| x.util.len() < 2),
            });
            out.push(CardData {
                key: "gpu-temp".into(),
                title: "GPU temperature".into(),
                value: g.temperature_c.map(format::celsius).unwrap_or_default().into(),
                sub: if g.temperature_c.is_some() { gi.name.clone() } else { "The graphics driver doesn't report it".into() }.into(),
                icon: icons.thermometer.clone(),
                level: g.temperature_c.map_or(-1.0, |c| (c / 100.0).clamp(0.0, 1.0)),
                graph: empty.clone(),
                graph2: empty.clone(),
                second: false,
                available: g.temperature_c.is_some(),
            });
        }
        None => {
            out.push(CardData { key: "gpu".into(), title: "GPU".into(), sub: "No graphics adapter reported".into(), icon: icons.gpu.clone(), level: -1.0, ..Default::default() });
        }
    }

    // Disk: the busiest disk's active time; transfer summed over all disks.
    let active = max_f32(f.disks.iter().filter_map(|d| d.active));
    let read = sum_opt(f.disks.iter().map(|d| d.read));
    let write = sum_opt(f.disks.iter().map(|d| d.write));
    let actives: Vec<&History> = h.disks.iter().map(|d| &d.active).collect();
    out.push(CardData {
        key: "disk".into(),
        title: "Disk".into(),
        value: active.map(format::pct).unwrap_or_default().into(),
        sub: match (read, write) {
            (Some(r), Some(w)) => format!("Read {} · Write {}", format::rate(r), format::rate(w)),
            _ => format!("{} disks", info.disks.len()),
        }
        .into(),
        icon: icons.disk.clone(),
        level: level(active),
        graph: if actives.is_empty() { empty.clone() } else { graph(&combine(&actives, f32::max), 100.0, CARD_POINTS) },
        graph2: empty.clone(),
        second: false,
        available: !f.disks.is_empty(),
    });

    // Network: summed over shown adapters.
    let recv = sum_opt(f.nets.iter().map(|n| n.recv));
    let send = sum_opt(f.nets.iter().map(|n| n.send));
    let rh: Vec<&History> = h.nets.iter().map(|n| &n.recv).collect();
    let sh: Vec<&History> = h.nets.iter().map(|n| &n.send).collect();
    let (rg, sg) = if rh.is_empty() {
        (empty.clone(), empty.clone())
    } else {
        let r = combine(&rh, |a, b| a + b);
        let s = combine(&sh, |a, b| a + b);
        let top = nice_ceiling(r.max().unwrap_or(0.0).max(s.max().unwrap_or(0.0)) as f64, NET_FLOOR) as f32;
        (graph(&r, top, CARD_POINTS), graph(&s, top, CARD_POINTS))
    };
    out.push(CardData {
        key: "net".into(),
        title: "Network".into(),
        value: recv.map(format::bits).unwrap_or_default().into(),
        sub: match send {
            Some(s) => format!("Down · up {}", format::bits(s)),
            None => "Down · up".into(),
        }
        .into(),
        icon: icons.network.clone(),
        level: -1.0,
        graph: rg,
        graph2: sg,
        second: true,
        available: !f.nets.is_empty(),
    });

    // Uptime and processes.
    out.push(CardData {
        key: "uptime".into(),
        title: "Up time".into(),
        value: f.sys.uptime_secs.map(format::uptime).unwrap_or_default().into(),
        sub: "Since Windows last started".into(),
        icon: icons.clock.clone(),
        level: -1.0,
        available: f.sys.uptime_secs.is_some(),
        ..Default::default()
    });
    out.push(CardData {
        key: "procs".into(),
        title: "Processes".into(),
        value: if f.sys.processes > 0 { format::count(f.sys.processes as u64) } else { String::new() }.into(),
        sub: match f.sys.handles {
            Some(hd) => format!("{} threads · {} handles", format::count(f.sys.threads as u64), format::count(hd)),
            None => format!("{} threads", format::count(f.sys.threads as u64)),
        }
        .into(),
        icon: icons.processes.clone(),
        level: -1.0,
        available: f.sys.processes > 0,
        ..Default::default()
    });
    out
}

pub fn insights(list: &[Insight]) -> Vec<InsightData> {
    list.iter()
        .map(|n| InsightData {
            id: n.id.as_str().into(),
            severity: if n.severity == Severity::Warning { 1 } else { 0 },
            title: n.title.as_str().into(),
            detail: n.detail.as_str().into(),
            action: match n.target {
                Target::Process(_) => "Show process",
                Target::Startup => "Review",
                _ => "Show",
            }
            .into(),
        })
        .collect()
}

// ─── Performance ───────────────────────────────────────────────────────────

pub fn device_keys(info: &SystemInfo) -> Vec<DevKey> {
    let mut v = vec![DevKey::Cpu, DevKey::Memory];
    v.extend(info.disks.iter().map(|d| DevKey::Disk(d.id)));
    v.extend(info.nets.iter().map(|n| DevKey::Net(n.id)));
    v.extend(info.gpus.iter().map(|g| DevKey::Gpu(g.id)));
    v
}

fn gpu_index(info: &SystemInfo, id: u64) -> usize {
    info.gpus.iter().position(|g| g.id == id).unwrap_or(0)
}

pub fn devices(keys: &[DevKey], f: &Frame, info: &SystemInfo, h: &Histories) -> Vec<DeviceData> {
    let empty = GraphData::default();
    keys.iter()
        .map(|k| match k {
            DevKey::Cpu => DeviceData {
                kind: 0,
                title: "CPU".into(),
                sub: short_cpu_name(&info.cpu.name).into(),
                value: {
                    let mut parts: Vec<String> = Vec::new();
                    if let Some(t) = f.cpu.total {
                        parts.push(format::pct(t));
                        if let Some(m) = f.cpu.speed_mhz {
                            parts.push(format::mhz(m));
                        }
                        if let Some(c) = f.cpu.temperature_c {
                            parts.push(format!("({})", format::celsius(c)));
                        }
                    }
                    parts.join("  ")
                }
                .into(),
                graph: graph(&h.cpu, 100.0, MINI_POINTS),
                graph2: empty.clone(),
                second: false,
            },
            DevKey::Memory => DeviceData {
                kind: 1,
                title: "Memory".into(),
                sub: format::bytes(info.mem.installed.unwrap_or(info.mem.total)).into(),
                value: f.mem.map(|m| format!("{} / {} ({})", format::bytes(m.used()), format::bytes(m.total), format::pct(m.used_pct()))).unwrap_or_default().into(),
                graph: graph(&h.mem_pct, 100.0, MINI_POINTS),
                graph2: empty.clone(),
                second: false,
            },
            DevKey::Disk(id) => {
                let d = info.disks.iter().find(|d| d.id == *id);
                let now = f.disks.iter().find(|x| x.id == *id);
                DeviceData {
                    kind: 2,
                    title: d.map(|d| d.title()).unwrap_or_else(|| format!("Disk {id}")).into(),
                    sub: d.map(disk_kind).unwrap_or_default().into(),
                    value: now.and_then(|n| n.active).map(format::pct).unwrap_or_default().into(),
                    graph: h.disk(*id).map(|x| graph(&x.active, 100.0, MINI_POINTS)).unwrap_or_default(),
                    graph2: empty.clone(),
                    second: false,
                }
            }
            DevKey::Net(id) => {
                let n = info.nets.iter().find(|n| n.id == *id);
                let now = f.nets.iter().find(|x| x.id == *id);
                let (g1, g2) = net_graphs(h.net(*id), MINI_POINTS);
                DeviceData {
                    kind: 3,
                    title: n.map(|n| n.name.clone()).unwrap_or_else(|| "Network".into()).into(),
                    sub: n.map(|n| n.kind.clone()).unwrap_or_default().into(),
                    value: match now {
                        Some(x) => match (x.send, x.recv) {
                            (Some(s), Some(r)) => format!("↑ {}  ↓ {}", format::bits(s), format::bits(r)),
                            _ => String::new(),
                        },
                        None => String::new(),
                    }
                    .into(),
                    graph: g1,
                    graph2: g2,
                    second: true,
                }
            }
            DevKey::Gpu(id) => {
                let g = info.gpus.iter().find(|g| g.id == *id);
                let now = f.gpus.iter().find(|x| x.id == *id);
                DeviceData {
                    kind: 4,
                    title: format!("GPU {}", gpu_index(info, *id)).into(),
                    sub: g.map(|g| g.name.clone()).unwrap_or_default().into(),
                    value: match now {
                        Some(n) => match (n.util, n.temperature_c) {
                            (Some(u), Some(t)) => format!("{} ({})", format::pct(u), format::celsius(t)),
                            (Some(u), None) => format::pct(u),
                            _ => String::new(),
                        },
                        None => String::new(),
                    }
                    .into(),
                    graph: h.gpu(*id).map(|x| graph(&x.util, 100.0, MINI_POINTS)).unwrap_or_default(),
                    graph2: empty.clone(),
                    second: false,
                }
            }
        })
        .collect()
}

fn net_graphs(h: Option<&tm_core::engine::NetHist>, points: usize) -> (GraphData, GraphData) {
    match h {
        Some(n) => {
            let top = nice_ceiling(n.recv.max().unwrap_or(0.0).max(n.send.max().unwrap_or(0.0)) as f64, NET_FLOOR) as f32;
            (graph(&n.recv, top, points), graph(&n.send, top, points))
        }
        None => (GraphData::default(), GraphData::default()),
    }
}

/// Everything the detail side of the Performance page shows for one device.
#[derive(Default)]
pub struct Detail {
    pub title: String,
    pub subtitle: String,
    pub graphs: Vec<PerfGraph>,
    pub is_cpu: bool,
    pub cores: Vec<GraphData>,
    pub live: Vec<StatData>,
    pub fixed: Vec<StatData>,
    pub segments: Vec<Segment>,
    pub note: String,
}

fn span_label(h: &History, interval: f32) -> String {
    let secs = (h.capacity().saturating_sub(1)) as f32 * interval;
    format::span(secs.round() as u64)
}

fn pg(title: &str, right: String, g: GraphData, big: bool) -> PerfGraph {
    PerfGraph { title: title.into(), right: right.into(), graph: g, graph2: GraphData::default(), second: false, legend: SharedString::new(), legend2: SharedString::new(), big }
}

fn opt(v: Option<String>) -> String {
    v.unwrap_or_else(|| "Not available".into())
}

pub fn detail(key: &DevKey, f: &Frame, info: &SystemInfo, h: &Histories, per_core: bool) -> Detail {
    let interval = f.interval_secs.max(0.25);
    match key {
        DevKey::Cpu => cpu_detail(f, info, h, per_core, interval),
        DevKey::Memory => mem_detail(f, info, h, interval),
        DevKey::Disk(id) => disk_detail(*id, f, info, h, interval),
        DevKey::Net(id) => net_detail(*id, f, info, h, interval),
        DevKey::Gpu(id) => gpu_detail(*id, f, info, h, interval),
    }
}

fn cpu_detail(f: &Frame, info: &SystemInfo, h: &Histories, per_core: bool, interval: f32) -> Detail {
    let c = &info.cpu;
    let mut g = pg("Utilization", format!("100% · {}", span_label(&h.cpu, interval)), graph(&h.cpu, 100.0, BIG_POINTS), true);
    g.graph2 = graph(&h.cpu_kernel, 100.0, BIG_POINTS);
    g.second = true;
    g.legend = "Total".into();
    g.legend2 = "Kernel".into();
    let cores = if per_core { h.cores.iter().map(|x| graph(x, 100.0, SMALL_POINTS)).collect() } else { Vec::new() };
    let mut live = vec![
        stat("Utilization", f.cpu.total.map(format::pct).unwrap_or_default()),
        stat_tip("Speed", opt(f.cpu.speed_mhz.map(format::mhz)), "The current clock, from the processor's performance counter."),
        stat("Processes", format::count(f.sys.processes as u64)),
        stat("Threads", format::count(f.sys.threads as u64)),
    ];
    if let Some(hd) = f.sys.handles {
        live.push(stat("Handles", format::count(hd)));
    }
    live.push(stat("Up time", f.sys.uptime_secs.map(format::uptime).unwrap_or_default()));
    if let Some(t) = f.cpu.temperature_c {
        live.push(stat_tip("Temperature", format::celsius(t), "The CPU's own sensor, read through PawnIO. On Ryzen this is Tctl, what the CPU manages its fans and boost by."));
    }
    if let Some(w) = f.cpu.power_w {
        live.push(stat_tip("Power", format!("{w:.0} W"), "The whole package, from the CPU's own energy counter."));
    }
    let mut fixed = vec![
        stat("Base speed", opt(c.base_mhz.map(format::mhz))),
        stat("Sockets", c.sockets.to_string()),
        stat("Cores", c.cores.to_string()),
        stat("Logical processors", c.logical.to_string()),
        stat("Virtualization", opt(c.virtualization.map(|v| if v { "Enabled" } else { "Disabled" }.to_string()))),
        stat("L1 cache", opt(c.l1_bytes.map(format::bytes))),
        stat("L2 cache", opt(c.l2_bytes.map(format::bytes))),
        stat("L3 cache", opt(c.l3_bytes.map(format::bytes))),
    ];
    if !c.vendor.is_empty() {
        fixed.push(stat("Maker", c.vendor.clone()));
    }
    let zones: Vec<String> = f.thermal.iter().filter(|t| t.kind == ThermalKind::Zone).map(|t| format!("{} {}", t.name, format::celsius(t.celsius))).collect();
    let note = if f.cpu.temperature_c.is_some() {
        String::new()
    } else {
        let mut n = if zones.is_empty() {
            "Windows doesn't get a CPU temperature reading on this PC, so none is shown.".to_string()
        } else {
            format!("Windows has no reading from the CPU's own sensor. Firmware thermal zones report: {}. These are usually motherboard sensors, not the CPU.", zones.join(", "))
        };
        if let Some(hint) = sensor_hint(f.cpu.sensor) {
            n.push(' ');
            n.push_str(hint);
        }
        n
    };
    Detail { title: "CPU".into(), subtitle: c.name.clone(), graphs: vec![g], is_cpu: true, cores, live, fixed, segments: Vec::new(), note }
}

/// What would give a CPU temperature, when there's none.
fn sensor_hint(s: SensorAccess) -> Option<&'static str> {
    match s {
        SensorAccess::NeedsAdmin => Some("PawnIO is installed, so Run as administrator shows the temperature and power from the CPU itself."),
        SensorAccess::NoDriver => Some("Hardware tools read the CPU's own sensor through a driver. If PawnIO is installed (LibreHardwareMonitor and OpenRGB install it, or get it from pawnio.eu), Run as administrator shows it here."),
        SensorAccess::Unsupported => Some("PawnIO is installed but can't read this CPU's sensor."),
        SensorAccess::Ok | SensorAccess::Unknown => None,
    }
}

fn mem_detail(f: &Frame, info: &SystemInfo, h: &Histories, interval: f32) -> Detail {
    let mi = &info.mem;
    let mut d = Detail { title: "Memory".into(), ..Default::default() };
    let mut sub: Vec<String> = vec![format::bytes(mi.installed.unwrap_or(mi.total))];
    if let Some(k) = &mi.kind {
        sub.push(k.clone());
    }
    if let Some(s) = mi.speed_mts {
        sub.push(format!("{s} MT/s"));
    }
    d.subtitle = sub.join(" · ");
    let Some(m) = f.mem else {
        d.note = "Memory figures couldn't be read.".into();
        return d;
    };
    d.graphs.push(pg("Memory in use", format!("{} · {}", format::bytes(m.total), span_label(&h.mem_used, interval)), graph(&h.mem_used, m.total as f32, BIG_POINTS), true));
    if m.commit_limit > 0 {
        d.graphs.push(pg("Committed", format!("{} limit", format::bytes(m.commit_limit)), graph(&h.commit, m.commit_limit as f32, SMALL_POINTS), false));
    }
    // Composition: in use, modified, standby, free. Without the detailed lists, in use and available.
    let total = m.total.max(1) as f64;
    match (m.modified, m.standby, m.free) {
        (Some(md), Some(sb), Some(fr)) => {
            let in_use = m.total.saturating_sub(m.available).saturating_sub(md);
            d.segments = vec![
                Segment { label: "In use".into(), value: format::bytes(in_use).into(), tip: "Memory used by programs, drivers and Windows.".into(), frac: (in_use as f64 / total) as f32, tone: 0 },
                Segment { label: "Modified".into(), value: format::bytes(md).into(), tip: "Changed data that must be written to disk before the memory can be reused.".into(), frac: (md as f64 / total) as f32, tone: 1 },
                Segment { label: "Standby".into(), value: format::bytes(sb).into(), tip: "Cached data and code, kept in case it's needed again. Given to programs whenever they ask.".into(), frac: (sb as f64 / total) as f32, tone: 2 },
                Segment { label: "Free".into(), value: format::bytes(fr).into(), tip: "Not holding anything useful; available right away.".into(), frac: (fr as f64 / total) as f32, tone: 3 },
            ];
        }
        _ => {
            d.segments = vec![
                Segment { label: "In use".into(), value: format::bytes(m.used()).into(), tip: "Memory used by programs, drivers and Windows.".into(), frac: (m.used() as f64 / total) as f32, tone: 0 },
                Segment { label: "Available".into(), value: format::bytes(m.available).into(), tip: "Free or cached memory that programs can have right away.".into(), frac: (m.available as f64 / total) as f32, tone: 3 },
            ];
        }
    }
    d.live = vec![
        stat_tip("In use", format::bytes(m.used()), "Total memory minus what's available."),
        stat_tip("Available", format::bytes(m.available), "Free plus standby memory: what programs can get without anything being written out."),
        stat_tip("Committed", format!("{} / {}", format::bytes(m.commit), format::bytes(m.commit_limit)), "Memory programs have reserved, against the limit of RAM plus page file."),
    ];
    if let Some(c) = m.cached {
        d.live.push(stat_tip("Cached", format::bytes(c), "Standby and modified memory."));
    }
    if let Some(c) = m.compressed.filter(|c| *c > 0) {
        d.live.push(stat_tip("Compressed", format::bytes(c), "RAM taken up by the store of compressed memory. It is part of the memory in use."));
    }
    if let Some(p) = m.paged_pool {
        d.live.push(stat_tip("Paged pool", format::bytes(p), "Kernel memory that can be written to the page file."));
    }
    if let Some(p) = m.nonpaged_pool {
        d.live.push(stat_tip("Non-paged pool", format::bytes(p), "Kernel memory that always stays in RAM."));
    }
    d.fixed.push(stat("Speed", opt(mi.speed_mts.map(|s| format!("{s} MT/s")))));
    d.fixed.push(stat(
        "Slots used",
        match (mi.slots_used, mi.slots_total) {
            (Some(u), Some(t)) => format!("{u} of {t}"),
            (Some(u), None) => u.to_string(),
            _ => "Not available".into(),
        },
    ));
    d.fixed.push(stat("Form factor", opt(mi.form_factor.clone())));
    d.fixed.push(stat("Type", opt(mi.kind.clone())));
    if let Some(inst) = mi.installed {
        d.fixed.push(stat("Installed", format::bytes(inst)));
        d.fixed.push(stat("Hardware reserved", format::bytes(inst.saturating_sub(mi.total))));
    }
    d.fixed.push(stat("Usable", format::bytes(mi.total)));
    d
}

fn disk_detail(id: u32, f: &Frame, info: &SystemInfo, h: &Histories, interval: f32) -> Detail {
    let di = info.disks.iter().find(|d| d.id == id);
    let now = f.disks.iter().find(|d| d.id == id);
    let hist = h.disk(id);
    let mut d = Detail {
        title: di.map(|d| d.title()).unwrap_or_else(|| format!("Disk {id}")),
        subtitle: di.map(|d| d.model.clone()).unwrap_or_default(),
        ..Default::default()
    };
    if let Some(x) = hist {
        d.graphs.push(pg("Active time", format!("100% · {}", span_label(&x.active, interval)), graph(&x.active, 100.0, BIG_POINTS), true));
        let top = nice_ceiling(x.read.max().unwrap_or(0.0).max(x.write.max().unwrap_or(0.0)) as f64, DISK_FLOOR) as f32;
        let mut t = pg("Transfer rate", format::rate(top as f64), graph(&x.read, top, SMALL_POINTS), false);
        t.graph2 = graph(&x.write, top, SMALL_POINTS);
        t.second = true;
        t.legend = "Read".into();
        t.legend2 = "Write".into();
        d.graphs.push(t);
    }
    if let Some(n) = now {
        d.live = vec![
            stat("Active time", opt(n.active.map(format::pct))),
            stat_tip("Average response time", opt(n.response_ms.map(|r| format!("{r:.1} ms"))), "How long each read or write took on average over the last interval."),
            stat("Read speed", opt(n.read.map(format::rate))),
            stat("Write speed", opt(n.write.map(format::rate))),
            stat("Queue", n.queue_depth.to_string()),
        ];
    }
    if let Some(di) = di {
        d.fixed.push(stat("Capacity", opt(di.size.map(format::capacity))));
        d.fixed.push(stat("Type", { let k = disk_kind(di); if k.is_empty() { "Not available".into() } else { k } }));
        d.fixed.push(stat("System disk", if di.system { "Yes" } else { "No" }.to_string()));
        d.fixed.push(stat("Page file", if di.pagefile { "Yes" } else { "No" }.to_string()));
        for v in &di.volumes {
            let label = if v.label.is_empty() { v.mount.clone() } else { format!("{} ({})", v.mount, v.label) };
            let value = match (v.free, v.total) {
                (Some(fr), Some(t)) => format!("{} free of {} · {}", format::capacity(fr), format::capacity(t), v.filesystem),
                _ => v.filesystem.clone(),
            };
            d.fixed.push(StatData { label: label.into(), value: value.into(), tip: SharedString::new() });
        }
    }
    d
}

fn net_detail(id: u64, f: &Frame, info: &SystemInfo, h: &Histories, interval: f32) -> Detail {
    let ni = info.nets.iter().find(|n| n.id == id);
    let now = f.nets.iter().find(|n| n.id == id);
    let mut d = Detail {
        title: ni.map(|n| n.name.clone()).unwrap_or_else(|| "Network".into()),
        subtitle: ni.map(|n| n.description.clone()).unwrap_or_default(),
        ..Default::default()
    };
    if let Some(x) = h.net(id) {
        let top = nice_ceiling(x.recv.max().unwrap_or(0.0).max(x.send.max().unwrap_or(0.0)) as f64, NET_FLOOR);
        let mut g = pg("Throughput", format!("{} · {}", format::bits(top), span_label(&x.recv, interval)), graph(&x.recv, top as f32, BIG_POINTS), true);
        g.graph2 = graph(&x.send, top as f32, BIG_POINTS);
        g.second = true;
        g.legend = "Receive".into();
        g.legend2 = "Send".into();
        d.graphs.push(g);
    }
    if let Some(n) = now {
        d.live = vec![stat("Receive", opt(n.recv.map(format::bits))), stat("Send", opt(n.send.map(format::bits)))];
    }
    if let Some(ni) = ni {
        d.fixed.push(stat("Adapter", ni.description.clone()));
        d.fixed.push(stat("Connection type", ni.kind.clone()));
        d.fixed.push(stat("Link speed", opt(now.and_then(|n| n.link_bps).or(ni.link_bps).map(format::link_speed))));
        if !ni.ipv4.is_empty() {
            d.fixed.push(stat("IPv4 address", ni.ipv4.join(", ")));
        }
        for (i, a) in ni.ipv6.iter().take(3).enumerate() {
            d.fixed.push(stat(if i == 0 { "IPv6 address" } else { "" }, a.clone()));
        }
        if let Some(s) = &ni.dns_suffix {
            d.fixed.push(stat("DNS suffix", s.clone()));
        }
        if let Some(m) = &ni.mac {
            d.fixed.push(stat("Physical address", m.clone()));
        }
    }
    d
}

fn gpu_detail(id: u64, f: &Frame, info: &SystemInfo, h: &Histories, interval: f32) -> Detail {
    let gi = info.gpus.iter().find(|g| g.id == id);
    let now = f.gpus.iter().find(|g| g.id == id);
    let hist = h.gpu(id);
    let mut d = Detail {
        title: format!("GPU {}", gpu_index(info, id)),
        subtitle: gi.map(|g| g.name.clone()).unwrap_or_default(),
        ..Default::default()
    };
    if let Some(x) = hist {
        d.graphs.push(pg("Utilization (busiest engine)", format!("100% · {}", span_label(&x.util, interval)), graph(&x.util, 100.0, BIG_POINTS), true));
        // Engines: the common ones, plus any that did work recently. At most six.
        if let Some(gi) = gi {
            let common = ["3D", "Copy", "Video Decode", "Video Encode", "Compute"];
            let mut picks: Vec<usize> = Vec::new();
            for (i, name) in gi.engines.iter().enumerate() {
                let busy = x.engines.get(i).and_then(|e| e.max()).unwrap_or(0.0) > 0.5;
                if common.iter().any(|c| name.eq_ignore_ascii_case(c)) || busy {
                    picks.push(i);
                }
            }
            for i in picks.into_iter().take(6) {
                if let Some(e) = x.engines.get(i) {
                    d.graphs.push(pg(&gi.engines[i], e.last().map(format::pct).unwrap_or_default(), graph(e, 100.0, SMALL_POINTS), false));
                }
            }
        }
        if let Some(n) = now {
            if let Some(limit) = n.dedicated_limit.or(gi.and_then(|g| g.dedicated)).filter(|l| *l > 0) {
                d.graphs.push(pg("Dedicated memory", format::bytes(limit), graph(&x.dedicated, limit as f32, SMALL_POINTS), false));
            }
            if let Some(limit) = n.shared_limit.or(gi.and_then(|g| g.shared)).filter(|l| *l > 0) {
                d.graphs.push(pg("Shared memory", format::bytes(limit), graph(&x.shared, limit as f32, SMALL_POINTS), false));
            }
        }
    }
    if let Some(n) = now {
        d.live.push(stat("Utilization", opt(n.util.map(format::pct))));
        let mem = |used: Option<u64>, limit: Option<u64>| match (used, limit) {
            (Some(u), Some(l)) if l > 0 => format!("{} / {}", format::bytes(u), format::bytes(l)),
            (Some(u), _) => format::bytes(u),
            _ => "Not available".into(),
        };
        d.live.push(stat_tip("Dedicated memory", mem(n.dedicated_used, n.dedicated_limit.or(gi.and_then(|g| g.dedicated))), "Memory on the graphics card itself."));
        d.live.push(stat_tip("Shared memory", mem(n.shared_used, n.shared_limit.or(gi.and_then(|g| g.shared))), "System memory the GPU can use when its own runs short."));
        // Only what the driver reports: nothing is shown as zero when it simply isn't known.
        if let Some(t) = n.temperature_c {
            d.live.push(stat("Temperature", format::celsius(t)));
        }
        if let Some(c) = n.clock_mhz {
            d.live.push(stat("Clock", format::mhz(c)));
        }
        if let Some(c) = n.mem_clock_mhz {
            d.live.push(stat("Memory clock", format::mhz(c)));
        }
        if let Some(r) = n.fan_rpm {
            d.live.push(stat("Fan", format!("{} RPM", format::count(r as u64))));
        }
        if let Some(p) = n.power_pct {
            d.live.push(stat_tip("Power", format!("{p:.0}%"), "Power draw as a share of the card's limit."));
        }
        let missing: Vec<&str> = [(n.temperature_c.is_none(), "temperature"), (n.clock_mhz.is_none(), "clock speed"), (n.fan_rpm.is_none(), "fan speed")]
            .iter()
            .filter(|(m, _)| *m)
            .map(|(_, s)| *s)
            .collect();
        if !missing.is_empty() {
            d.note = format!("The graphics driver doesn't report {} for this GPU, so {} not shown.", crate::util::names(&missing.iter().map(|s| s.to_string()).collect::<Vec<_>>()), if missing.len() == 1 { "it's" } else { "they're" });
        }
    }
    if let Some(gi) = gi {
        d.fixed.push(stat("Driver version", opt(gi.driver_version.clone())));
        d.fixed.push(stat("Driver date", opt(gi.driver_date.clone())));
        d.fixed.push(stat("Location", opt(gi.location.clone())));
        d.fixed.push(stat("Dedicated memory", opt(gi.dedicated.map(format::bytes))));
        d.fixed.push(stat("Shared memory", opt(gi.shared.map(format::bytes))));
        if !gi.vendor.is_empty() {
            d.fixed.push(stat("Maker", gi.vendor.clone()));
        }
    }
    d
}

// ─── Compact monitor ───────────────────────────────────────────────────────

pub fn compact(metrics: &[Metric], f: &Frame, info: &SystemInfo, h: &Histories) -> Vec<CompactMetric> {
    const POINTS: usize = 40;
    metrics
        .iter()
        .map(|m| match m {
            Metric::Cpu => CompactMetric {
                label: "CPU".into(),
                value: match (f.cpu.total, f.cpu.temperature_c) {
                    (Some(u), Some(t)) => format!("{} · {}", format::pct(u), format::celsius(t)),
                    (Some(u), None) => format::pct(u),
                    _ => "—".into(),
                }
                .into(),
                level: level(f.cpu.total),
                graph: graph(&h.cpu, 100.0, POINTS),
            },
            Metric::Fps => {
                let (label, value) = match (&f.fps, f.fps_access) {
                    (Some(x), _) => (format!("FPS · {}", x.name), format!("{:.0}", x.fps)),
                    (None, FpsAccess::NeedsPermission) => ("FPS · needs admin".to_string(), "—".to_string()),
                    (None, FpsAccess::Failed) => ("FPS unavailable".to_string(), "—".to_string()),
                    (None, _) => ("FPS".to_string(), "—".to_string()),
                };
                let top = nice_ceiling(h.fps.max().unwrap_or(0.0) as f64, 60.0) as f32;
                CompactMetric { label: label.into(), value: value.into(), level: -1.0, graph: graph(&h.fps, top, POINTS) }
            }
            Metric::GpuMemory => {
                let gi = info.gpus.first();
                let g = gi.and_then(|gi| f.gpus.iter().find(|g| g.id == gi.id));
                let used = g.and_then(|g| g.dedicated_used);
                let limit = g.and_then(|g| g.dedicated_limit).or(gi.and_then(|gi| gi.dedicated)).filter(|l| *l > 0);
                let value = match (used, limit) {
                    (Some(u), Some(l)) => gpu_memory(u, l),
                    (Some(u), None) => format::bytes(u),
                    _ => if g.is_none() { "None".to_string() } else { "—".to_string() },
                };
                let pct = used.zip(limit).map(|(u, l)| (u as f64 / l as f64 * 100.0) as f32);
                let top = limit.map_or_else(|| g.and_then(|g| h.gpu(g.id)).and_then(|x| x.dedicated.max()).unwrap_or(1.0), |l| l as f32);
                CompactMetric { label: "GPU memory".into(), value: value.into(), level: level(pct), graph: g.and_then(|g| h.gpu(g.id)).map(|x| graph(&x.dedicated, top, POINTS)).unwrap_or_default() }
            }
            Metric::Memory => CompactMetric {
                label: "Memory".into(),
                value: f.mem.map(|m| format!("{} · {}", format::pct(m.used_pct()), format::bytes(m.used()))).unwrap_or_else(|| "—".into()).into(),
                level: level(f.mem.map(|m| m.used_pct())),
                graph: graph(&h.mem_pct, 100.0, POINTS),
            },
            Metric::Gpu => {
                let g = info.gpus.first().and_then(|gi| f.gpus.iter().find(|g| g.id == gi.id));
                let value = match g {
                    Some(g) => match (g.util, g.temperature_c) {
                        (Some(u), Some(t)) => format!("{} · {}", format::pct(u), format::celsius(t)),
                        (Some(u), None) => format::pct(u),
                        _ => "—".into(),
                    },
                    None => "None".into(),
                };
                CompactMetric { label: "GPU".into(), value: value.into(), level: level(g.and_then(|g| g.util)), graph: g.and_then(|g| h.gpu(g.id)).map(|x| graph(&x.util, 100.0, POINTS)).unwrap_or_default() }
            }
            Metric::Disk => {
                let active = max_f32(f.disks.iter().filter_map(|d| d.active));
                let actives: Vec<&History> = h.disks.iter().map(|d| &d.active).collect();
                CompactMetric {
                    label: "Disk".into(),
                    value: active.map(format::pct).unwrap_or_else(|| "—".into()).into(),
                    level: level(active),
                    graph: if actives.is_empty() { GraphData::default() } else { graph(&combine(&actives, f32::max), 100.0, POINTS) },
                }
            }
            Metric::Network => {
                let recv = sum_opt(f.nets.iter().map(|n| n.recv));
                let send = sum_opt(f.nets.iter().map(|n| n.send));
                let rh: Vec<&History> = h.nets.iter().map(|n| &n.recv).collect();
                let g = if rh.is_empty() {
                    GraphData::default()
                } else {
                    let r = combine(&rh, |a, b| a + b);
                    let top = nice_ceiling(r.max().unwrap_or(0.0) as f64, NET_FLOOR) as f32;
                    graph(&r, top, POINTS)
                };
                CompactMetric {
                    // Short, because the value beside it is two speeds wide.
                    label: "Net".into(),
                    value: match (recv, send) {
                        (Some(r), Some(s)) => format!("↓ {}  ↑ {}", format::bits(r), format::bits(s)),
                        _ => "—".into(),
                    }
                    .into(),
                    level: -1.0,
                    graph: g,
                }
            }
        })
        .collect()
}

/// "5.2 / 16 GB": the limit is a round size, so its decimals are noise.
fn gpu_memory(used: u64, limit: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    if limit as f64 >= GB {
        format!("{:.1} / {:.0} GB", used as f64 / GB, limit as f64 / GB)
    } else {
        format!("{} / {}", format::bytes(used), format::bytes(limit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_memory_reads_short() {
        let gb = 1024 * 1024 * 1024u64;
        assert_eq!(gpu_memory(5 * gb + gb / 5, 16 * gb - 1024 * 1024), "5.2 / 16 GB");
    }

    #[test]
    fn compact_rows_say_why_there_is_no_value() {
        let info = SystemInfo::default();
        let h = test_hist();
        let mut f = Frame::default();
        f.fps_access = FpsAccess::NeedsPermission;
        let rows = compact(&[Metric::Fps, Metric::GpuMemory], &f, &info, &h);
        assert_eq!(rows[0].label.as_str(), "FPS · needs admin");
        assert_eq!(rows[0].value.as_str(), "—");
        assert_eq!(rows[1].value.as_str(), "None", "no GPU");
        f.fps = Some(FpsNow { pid: 42, name: "Overwatch".into(), fps: 143.6 });
        f.fps_access = FpsAccess::Ok;
        let rows = compact(&[Metric::Fps], &f, &info, &h);
        assert_eq!(rows[0].label.as_str(), "FPS · Overwatch");
        assert_eq!(rows[0].value.as_str(), "144");
    }

    #[test]
    fn missing_cpu_sensor_explains_how_to_get_one() {
        let info = SystemInfo::default();
        let h = test_hist();
        let mut f = Frame::default();
        f.cpu.sensor = SensorAccess::NeedsAdmin;
        let d = cpu_detail(&f, &info, &h, false, 1.0);
        assert!(d.note.contains("Run as administrator"), "{}", d.note);
        f.cpu.temperature_c = Some(52.5);
        f.cpu.power_w = Some(64.6);
        f.cpu.sensor = SensorAccess::Ok;
        let d = cpu_detail(&f, &info, &h, false, 1.0);
        assert!(d.note.is_empty());
        assert!(d.live.iter().any(|s| s.label == "Power" && s.value == "65 W"));
    }

    #[test]
    fn cpu_names_are_shortened() {
        assert_eq!(short_cpu_name("AMD Ryzen 7 5800X 8-Core Processor"), "AMD Ryzen 7 5800X");
        assert_eq!(short_cpu_name("Intel(R) Core(TM) i7-8700K CPU @ 3.70GHz"), "Intel Core i7-8700K");
        assert_eq!(short_cpu_name("AMD Ryzen 7 7840HS w/ Radeon 780M Graphics"), "AMD Ryzen 7 7840HS w/ Radeon 780M Graphics");
    }

    #[test]
    fn missing_temperature_is_not_zero() {
        let f = Frame::default();
        let info = SystemInfo::default();
        let h = test_hist();
        let d = cpu_detail(&f, &info, &h, false, 1.0);
        assert!(!d.live.iter().any(|s| s.label == "Temperature"));
        assert!(!d.note.is_empty());
    }

    fn test_hist() -> Histories {
        Histories {
            cap: 4,
            cpu: History::new(4),
            cpu_kernel: History::new(4),
            cores: Vec::new(),
            mem_pct: History::new(4),
            mem_used: History::new(4),
            commit: History::new(4),
            disks: Vec::new(),
            nets: Vec::new(),
            gpus: Vec::new(),
            fps: History::new(4),
        }
    }
}

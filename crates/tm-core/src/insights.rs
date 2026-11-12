//! Deterministic system insights.
//!
//! Each rule looks at averages over a window and states what was measured. It doesn't guess at
//! causes. Every rule has a higher threshold to appear than to disappear (hysteresis), so an insight
//! doesn't flicker on and off while a value hovers near the line.

use std::collections::{HashMap, HashSet};

use crate::format;
use crate::history::History;
use crate::model::*;
use crate::process::Growth;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
}

/// Where "Show" takes the user.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Cpu,
    Memory,
    Disk(u32),
    Gpu(u64),
    Process(ProcKey),
    Startup,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Insight {
    /// Stable while the condition lasts.
    pub id: String,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    pub target: Target,
}

/// Values the rules read, besides the current frame.
pub struct Context<'a> {
    pub frame: &'a Frame,
    pub info: &'a SystemInfo,
    pub interval_secs: f32,
    pub mem_used_pct: &'a History,
    pub disk_active: &'a [(u32, &'a History)],
    pub gpu_util: &'a [(u64, &'a History)],
    pub growth: &'a HashMap<ProcKey, Growth>,
    pub startup_enabled: Option<u32>,
}

/// Thresholds, kept together so they're easy to review.
pub mod limits {
    pub const MEM_RAISE: f32 = 90.0;
    pub const MEM_CLEAR: f32 = 85.0;
    pub const MEM_WINDOW_SECS: f32 = 15.0;
    pub const COMMIT_RAISE: f32 = 90.0;
    pub const COMMIT_CLEAR: f32 = 85.0;
    pub const PROC_CPU_RAISE: f32 = 25.0;
    pub const PROC_CPU_CLEAR: f32 = 18.0;
    pub const PROC_CPU_WINDOW_SECS: f32 = 30.0;
    pub const DISK_RAISE: f32 = 90.0;
    pub const DISK_CLEAR: f32 = 75.0;
    pub const DISK_WINDOW_SECS: f32 = 15.0;
    pub const GPU_RAISE: f32 = 90.0;
    pub const GPU_CLEAR: f32 = 80.0;
    pub const GPU_WINDOW_SECS: f32 = 30.0;
    pub const STARTUP_RAISE: u32 = 10;
    pub const GROWTH_MIN_SECS: f64 = 600.0;
    pub const GROWTH_MIN_BYTES: u64 = 300 * 1024 * 1024;
    pub const GROWTH_MIN_RATIO: f64 = 1.5;
    pub const SPACE_FREE_PCT: f64 = 10.0;
    pub const SPACE_FREE_BYTES: u64 = 20 * 1024 * 1024 * 1024;
    pub const SPACE_CRITICAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
}

#[derive(Default)]
pub struct InsightEngine {
    active: HashSet<String>,
    proc_cpu: HashMap<ProcKey, History>,
}

fn window(secs: f32, interval: f32) -> usize {
    ((secs / interval.max(0.1)).round() as usize).max(3)
}

impl InsightEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hysteresis: a condition that is already showing stays until it drops below `clear`.
    fn gate(&self, id: &str, value: f32, raise: f32, clear: f32) -> bool {
        if self.active.contains(id) { value >= clear } else { value >= raise }
    }

    pub fn evaluate(&mut self, cx: &Context) -> Vec<Insight> {
        use limits::*;
        let mut out = Vec::new();
        let f = cx.frame;

        // Memory pressure.
        let n = window(MEM_WINDOW_SECS, cx.interval_secs);
        if let (Some(mem), Some(avg)) = (f.mem, cx.mem_used_pct.mean_tail(n, n * 2 / 3)) {
            if self.gate("mem", avg, MEM_RAISE, MEM_CLEAR) {
                let top = f.processes.iter().filter(|p| p.key.pid > 4).max_by_key(|p| p.private_ws);
                let mut detail = format!(
                    "{} of {} ({}) has been in use on average over the last {}.",
                    format::bytes(mem.used()),
                    format::bytes(mem.total),
                    format::pct(avg),
                    format::span(MEM_WINDOW_SECS as u64)
                );
                if let Some(p) = top {
                    detail.push_str(&format!(" The largest is {} at {}.", p.name, format::bytes(p.private_ws)));
                }
                out.push(Insight { id: "mem".into(), severity: Severity::Warning, title: "Memory is nearly full".into(), detail, target: Target::Memory });
            }
            let commit = mem.commit_pct();
            if mem.commit_limit > 0 && self.gate("commit", commit, COMMIT_RAISE, COMMIT_CLEAR) {
                out.push(Insight {
                    id: "commit".into(),
                    severity: Severity::Warning,
                    title: "Committed memory is near its limit".into(),
                    detail: format!(
                        "{} of the {} limit ({}) is committed. When it's reached, programs can fail to get more memory.",
                        format::bytes(mem.commit),
                        format::bytes(mem.commit_limit),
                        format::pct(commit)
                    ),
                    target: Target::Memory,
                });
            }
        }

        // A process sustaining high CPU.
        if f.has_processes {
            let n = window(PROC_CPU_WINDOW_SECS, cx.interval_secs);
            let mut seen = HashSet::with_capacity(f.processes.len());
            for p in f.processes.iter() {
                if p.key.pid <= 4 {
                    continue;
                }
                seen.insert(p.key);
                let h = self.proc_cpu.entry(p.key).or_insert_with(|| History::new(n));
                if h.capacity() != n {
                    h.resize(n);
                }
                h.push_opt(p.cpu);
            }
            self.proc_cpu.retain(|k, _| seen.contains(k));
            let mut busy: Vec<(f32, &ProcNow)> = f
                .processes
                .iter()
                .filter_map(|p| {
                    let avg = self.proc_cpu.get(&p.key)?.mean_tail(n, n * 2 / 3)?;
                    let id = format!("cpu:{}:{}", p.key.pid, p.key.created);
                    self.gate(&id, avg, PROC_CPU_RAISE, PROC_CPU_CLEAR).then_some((avg, p))
                })
                .collect();
            busy.sort_by(|a, b| b.0.total_cmp(&a.0));
            for (avg, p) in busy.into_iter().take(3) {
                out.push(Insight {
                    id: format!("cpu:{}:{}", p.key.pid, p.key.created),
                    severity: if avg >= 50.0 { Severity::Warning } else { Severity::Info },
                    title: format!("{} is using a lot of CPU", p.name),
                    detail: format!("It has averaged {} of total CPU over the last {}.", format::pct(avg), format::span(PROC_CPU_WINDOW_SECS as u64)),
                    target: Target::Process(p.key),
                });
            }
        }

        // Disks near saturation.
        let n = window(DISK_WINDOW_SECS, cx.interval_secs);
        for (id, hist) in cx.disk_active {
            let Some(avg) = hist.mean_tail(n, n * 2 / 3) else { continue };
            let key = format!("disk:{id}");
            if self.gate(&key, avg, DISK_RAISE, DISK_CLEAR) {
                let name = cx.info.disks.iter().find(|d| d.id == *id).map(|d| d.title()).unwrap_or_else(|| format!("Disk {id}"));
                let now = f.disks.iter().find(|d| d.id == *id);
                let mut detail = format!("Its active time has averaged {} over the last {}.", format::pct(avg), format::span(DISK_WINDOW_SECS as u64));
                if let Some(d) = now {
                    detail.push_str(&format!(" Right now it reads {} and writes {}.", format::rate(d.read.unwrap_or(0.0)), format::rate(d.write.unwrap_or(0.0))));
                }
                out.push(Insight { id: key, severity: Severity::Warning, title: format!("{name} is very busy"), detail, target: Target::Disk(*id) });
            }
        }

        // GPU busy.
        let n = window(GPU_WINDOW_SECS, cx.interval_secs);
        for (id, hist) in cx.gpu_util {
            let Some(avg) = hist.mean_tail(n, n * 2 / 3) else { continue };
            let key = format!("gpu:{id}");
            if self.gate(&key, avg, GPU_RAISE, GPU_CLEAR) {
                let name = cx.info.gpus.iter().find(|g| g.id == *id).map(|g| g.name.clone()).unwrap_or_else(|| "The GPU".into());
                let mut detail = format!("It has averaged {} use over the last {}.", format::pct(avg), format::span(GPU_WINDOW_SECS as u64));
                if let Some(p) = f.processes.iter().filter(|p| p.gpu.unwrap_or(0.0) >= 1.0).max_by(|a, b| a.gpu.unwrap_or(0.0).total_cmp(&b.gpu.unwrap_or(0.0))) {
                    detail.push_str(&format!(" The busiest program is {} at {}.", p.name, format::pct(p.gpu.unwrap_or(0.0))));
                }
                out.push(Insight { id: key, severity: Severity::Info, title: format!("{name} is busy"), detail, target: Target::Gpu(*id) });
            }
        }

        // Many startup apps.
        if let Some(n) = cx.startup_enabled {
            if n >= STARTUP_RAISE {
                out.push(Insight {
                    id: "startup".into(),
                    severity: Severity::Info,
                    title: format!("{n} apps start when you sign in"),
                    detail: "Each one starts with Windows. You can turn off the ones you don't need on the Startup page.".into(),
                    target: Target::Startup,
                });
            }
        }

        // Memory that keeps growing.
        for p in f.processes.iter() {
            let Some(g) = cx.growth.get(&p.key) else { continue };
            let (Some(&(t0, first)), Some(&(t1, last))) = (g.samples.front(), g.samples.back()) else { continue };
            let elapsed = t1 - t0;
            if elapsed < GROWTH_MIN_SECS {
                continue;
            }
            let peak = g.samples.iter().map(|s| s.1).max().unwrap_or(last);
            let grew = last.saturating_sub(first);
            // Still near its peak: it grew and stayed grown rather than spiking once.
            let sustained = last as f64 >= peak as f64 * 0.9;
            let ratio = if first == 0 { f64::INFINITY } else { last as f64 / first as f64 };
            if grew >= GROWTH_MIN_BYTES && ratio >= GROWTH_MIN_RATIO && sustained {
                out.push(Insight {
                    id: format!("grow:{}:{}", p.key.pid, p.key.created),
                    severity: Severity::Info,
                    title: format!("{}'s memory keeps growing", p.name),
                    detail: format!(
                        "Its committed memory went from {} to {} over {}.",
                        format::bytes(first),
                        format::bytes(last),
                        format::span(elapsed as u64)
                    ),
                    target: Target::Process(p.key),
                });
            }
        }

        // Volumes running out of space.
        for d in &cx.info.disks {
            for v in &d.volumes {
                let (Some(total), Some(free)) = (v.total, v.free) else { continue };
                if total == 0 {
                    continue;
                }
                let pct = free as f64 / total as f64 * 100.0;
                let low = (pct < SPACE_FREE_PCT && free < SPACE_FREE_BYTES) || free < SPACE_CRITICAL_BYTES;
                if low && total > 4 * SPACE_CRITICAL_BYTES {
                    out.push(Insight {
                        id: format!("space:{}", v.mount),
                        severity: Severity::Warning,
                        title: format!("{} is almost full", if v.mount.is_empty() { "A volume" } else { &v.mount }),
                        detail: format!("{} free of {} ({:.0}%).", format::bytes(free), format::bytes(total), pct),
                        target: Target::Disk(d.id),
                    });
                }
            }
        }

        out.sort_by(|a, b| b.severity.cmp(&a.severity));
        self.active = out.iter().map(|i| i.id.clone()).collect();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn mem(used_pct: f64) -> MemNow {
        let total = 16u64 << 30;
        MemNow { total, available: (total as f64 * (1.0 - used_pct / 100.0)) as u64, commit: 10 << 30, commit_limit: 32 << 30, ..Default::default() }
    }

    struct Fixture {
        info: SystemInfo,
        mem_hist: History,
        disk_hist: History,
        gpu_hist: History,
        growth: HashMap<ProcKey, Growth>,
    }

    impl Fixture {
        fn new() -> Self {
            Self { info: SystemInfo::default(), mem_hist: History::new(60), disk_hist: History::new(60), gpu_hist: History::new(60), growth: HashMap::new() }
        }
        fn run(&self, engine: &mut InsightEngine, frame: &Frame, startup: Option<u32>) -> Vec<Insight> {
            let disks = [(0u32, &self.disk_hist)];
            let gpus = [(7u64, &self.gpu_hist)];
            engine.evaluate(&Context {
                frame,
                info: &self.info,
                interval_secs: 1.0,
                mem_used_pct: &self.mem_hist,
                disk_active: &disks,
                gpu_util: &gpus,
                growth: &self.growth,
                startup_enabled: startup,
            })
        }
    }

    #[test]
    fn calm_system_has_no_insights() {
        let mut fx = Fixture::new();
        for _ in 0..30 {
            fx.mem_hist.push(40.0);
            fx.disk_hist.push(3.0);
            fx.gpu_hist.push(5.0);
        }
        let frame = Frame { mem: Some(mem(40.0)), ..Default::default() };
        assert!(fx.run(&mut InsightEngine::new(), &frame, Some(4)).is_empty());
    }

    #[test]
    fn memory_pressure_has_hysteresis() {
        let mut fx = Fixture::new();
        let mut e = InsightEngine::new();
        for _ in 0..20 {
            fx.mem_hist.push(92.0);
        }
        let frame = Frame { mem: Some(mem(92.0)), ..Default::default() };
        let out = fx.run(&mut e, &frame, None);
        assert!(out.iter().any(|i| i.id == "mem"));
        // Drops to 87 %: below the raise line but above the clear line, so it stays.
        for _ in 0..20 {
            fx.mem_hist.push(87.0);
        }
        assert!(fx.run(&mut e, &frame, None).iter().any(|i| i.id == "mem"));
        // A fresh engine would not raise it at 87 %.
        assert!(!fx.run(&mut InsightEngine::new(), &frame, None).iter().any(|i| i.id == "mem"));
        for _ in 0..20 {
            fx.mem_hist.push(70.0);
        }
        assert!(!fx.run(&mut e, &frame, None).iter().any(|i| i.id == "mem"));
    }

    #[test]
    fn a_short_spike_is_not_reported() {
        let mut fx = Fixture::new();
        for _ in 0..20 {
            fx.disk_hist.push(5.0);
        }
        fx.disk_hist.push(100.0);
        fx.disk_hist.push(100.0);
        let frame = Frame::default();
        assert!(fx.run(&mut InsightEngine::new(), &frame, None).is_empty());
    }

    #[test]
    fn sustained_disk_activity_is_reported_with_facts() {
        let mut fx = Fixture::new();
        for _ in 0..20 {
            fx.disk_hist.push(97.0);
        }
        let frame = Frame { disks: vec![DiskNow { id: 0, active: Some(97.0), read: Some(1024.0 * 1024.0), write: Some(0.0), ..Default::default() }], ..Default::default() };
        let out = fx.run(&mut InsightEngine::new(), &frame, None);
        let disk = out.iter().find(|i| i.id == "disk:0").expect("disk insight");
        assert!(disk.detail.contains("97%"), "{}", disk.detail);
        assert_eq!(disk.target, Target::Disk(0));
    }

    #[test]
    fn process_cpu_needs_a_sustained_average() {
        let fx = Fixture::new();
        let mut e = InsightEngine::new();
        let key = ProcKey { pid: 1234, created: 9 };
        let make = |cpu: f32| Frame {
            has_processes: true,
            processes: Arc::new(vec![ProcNow { key, name: "busy.exe".into(), cpu: Some(cpu), ..Default::default() }]),
            ..Default::default()
        };
        for _ in 0..10 {
            assert!(fx.run(&mut e, &make(60.0), None).is_empty(), "not enough samples yet");
        }
        let mut out = Vec::new();
        for _ in 0..25 {
            out = fx.run(&mut e, &make(60.0), None);
        }
        let i = out.iter().find(|i| i.target == Target::Process(key)).expect("cpu insight");
        assert!(i.title.contains("busy.exe"));
        assert_eq!(i.severity, Severity::Warning);
    }

    #[test]
    fn startup_count() {
        let fx = Fixture::new();
        let frame = Frame::default();
        assert!(fx.run(&mut InsightEngine::new(), &frame, Some(9)).is_empty());
        let out = fx.run(&mut InsightEngine::new(), &frame, Some(12));
        assert_eq!(out[0].title, "12 apps start when you sign in");
    }

    #[test]
    fn memory_growth_must_be_large_long_and_sustained() {
        let mut fx = Fixture::new();
        let key = ProcKey { pid: 50, created: 1 };
        let frame = Frame { processes: Arc::new(vec![ProcNow { key, name: "leaky.exe".into(), ..Default::default() }]), ..Default::default() };
        let mb = 1024 * 1024;
        // Grew from 400 MB to 1.2 GB over 20 minutes and stayed there.
        let samples = (0..=40).map(|i| (i as f64 * 30.0, (400 + i * 20) as u64 * mb)).collect();
        fx.growth.insert(key, Growth { samples });
        let out = fx.run(&mut InsightEngine::new(), &frame, None);
        assert!(out.iter().any(|i| i.id.starts_with("grow:")), "{out:?}");
        // The same growth over only 5 minutes is not enough.
        let samples = (0..=10).map(|i| (i as f64 * 30.0, (400 + i * 80) as u64 * mb)).collect();
        fx.growth.insert(key, Growth { samples });
        assert!(fx.run(&mut InsightEngine::new(), &frame, None).is_empty());
        // Grew, then dropped back: not sustained.
        let mut samples: std::collections::VecDeque<(f64, u64)> = (0..=40).map(|i| (i as f64 * 30.0, (400 + i * 20) as u64 * mb)).collect();
        samples.push_back((1300.0, 500 * mb));
        fx.growth.insert(key, Growth { samples });
        assert!(fx.run(&mut InsightEngine::new(), &frame, None).is_empty());
    }

    #[test]
    fn nearly_full_volume() {
        let mut fx = Fixture::new();
        fx.info.disks.push(DiskInfo {
            id: 0,
            volumes: vec![VolumeInfo { mount: "C:".into(), total: Some(476 << 30), free: Some(4 << 30), ..Default::default() }],
            ..Default::default()
        });
        let out = fx.run(&mut InsightEngine::new(), &Frame::default(), None);
        assert_eq!(out[0].title, "C: is almost full");
    }
}

//! The collector: a background thread that samples the platform on a fixed interval, turns raw
//! counters into rates, keeps bounded history and evaluates insights.
//!
//! The UI thread never calls the OS for monitoring data. It reads the latest [`Frame`] and the
//! histories from [`Shared`] when the collector says something changed.
//!
//! A second, low-priority thread resolves process paths, owners, descriptions and icons, so a slow
//! disk or a big icon never delays a sample.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::counter::{busy_percent, delta_u64, rate};
use crate::cpu::{self, CpuTimes};
use crate::history::History;
use crate::insights::{Context, InsightEngine};
use crate::model::*;
use crate::platform::{IconPixels, MetaResolver, SampleRequest, Sampler};
use crate::process::{ProcessTracker, TrackInput};

/// What the UI is showing, which decides what is worth sampling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Demand {
    pub processes: bool,
    pub windows: bool,
    pub services: bool,
    pub process_gpu: bool,
    pub memory_detail: bool,
    pub sensors: bool,
    /// Stop sampling altogether (hidden with background monitoring off).
    pub paused: bool,
}

impl Default for Demand {
    fn default() -> Self {
        Self { processes: true, windows: true, services: true, process_gpu: false, memory_detail: true, sensors: true, paused: false }
    }
}

pub enum Command {
    Demand(Demand),
    Interval(Duration),
    HistoryLen(usize),
    RefreshNow,
    /// Number of enabled startup entries, for the startup insight.
    StartupCount(Option<u32>),
    /// Re-read hardware details now (after a device change, or when a page opens).
    Describe,
}

pub struct DiskHist {
    pub id: u32,
    pub active: History,
    pub read: History,
    pub write: History,
}

pub struct NetHist {
    pub id: u64,
    pub recv: History,
    pub send: History,
}

pub struct GpuHist {
    pub id: u64,
    pub util: History,
    pub engines: Vec<History>,
    pub dedicated: History,
    pub shared: History,
}

pub struct Histories {
    pub cap: usize,
    pub cpu: History,
    pub cpu_kernel: History,
    pub cores: Vec<History>,
    pub mem_pct: History,
    pub mem_used: History,
    pub commit: History,
    pub disks: Vec<DiskHist>,
    pub nets: Vec<NetHist>,
    pub gpus: Vec<GpuHist>,
}

impl Histories {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            cpu: History::new(cap),
            cpu_kernel: History::new(cap),
            cores: Vec::new(),
            mem_pct: History::new(cap),
            mem_used: History::new(cap),
            commit: History::new(cap),
            disks: Vec::new(),
            nets: Vec::new(),
            gpus: Vec::new(),
        }
    }

    fn resize(&mut self, cap: usize) {
        self.cap = cap;
        for h in [&mut self.cpu, &mut self.cpu_kernel, &mut self.mem_pct, &mut self.mem_used, &mut self.commit] {
            h.resize(cap);
        }
        self.cores.iter_mut().for_each(|h| h.resize(cap));
        for d in &mut self.disks {
            d.active.resize(cap);
            d.read.resize(cap);
            d.write.resize(cap);
        }
        for n in &mut self.nets {
            n.recv.resize(cap);
            n.send.resize(cap);
        }
        for g in &mut self.gpus {
            g.util.resize(cap);
            g.dedicated.resize(cap);
            g.shared.resize(cap);
            g.engines.iter_mut().for_each(|h| h.resize(cap));
        }
    }

    pub fn disk(&self, id: u32) -> Option<&DiskHist> {
        self.disks.iter().find(|d| d.id == id)
    }
    pub fn net(&self, id: u64) -> Option<&NetHist> {
        self.nets.iter().find(|n| n.id == id)
    }
    pub fn gpu(&self, id: u64) -> Option<&GpuHist> {
        self.gpus.iter().find(|g| g.id == id)
    }
}

/// State the collector publishes and the UI reads.
pub struct Shared {
    pub info: Arc<SystemInfo>,
    /// Bumped whenever `info` changes.
    pub info_seq: u64,
    pub frame: Arc<Frame>,
    pub hist: Histories,
    /// Icons resolved since the UI last looked; the UI takes them.
    pub icons: Vec<(u32, IconPixels)>,
    /// How long the last sample took, for the diagnostics line in Settings.
    pub sample_cost: Duration,
    /// The last provider failure, if any, for diagnostics.
    pub last_error: Option<String>,
}

pub struct Collector {
    tx: Option<Sender<Command>>,
    pub shared: Arc<Mutex<Shared>>,
    thread: Option<JoinHandle<()>>,
    meta_thread: Option<JoinHandle<()>>,
}

impl Collector {
    pub fn spawn(
        sampler: Box<dyn Sampler>,
        resolver: Box<dyn MetaResolver>,
        notify: Box<dyn Fn() + Send>,
        interval: Duration,
        history_len: usize,
        demand: Demand,
    ) -> Collector {
        let shared = Arc::new(Mutex::new(Shared {
            info: Arc::new(SystemInfo::default()),
            info_seq: 0,
            frame: Arc::new(Frame::default()),
            hist: Histories::new(history_len),
            icons: Vec::new(),
            sample_cost: Duration::ZERO,
            last_error: None,
        }));
        let (tx, rx) = mpsc::channel();
        let (meta_tx, meta_rx_worker) = mpsc::channel::<(ProcKey, Arc<str>)>();
        let (meta_done_tx, meta_done_rx) = mpsc::channel::<(ProcKey, ProcMeta)>();
        let meta_shared = shared.clone();
        let meta_thread = std::thread::Builder::new()
            .name("metadata".into())
            .spawn(move || metadata_worker(resolver, meta_rx_worker, meta_done_tx, meta_shared))
            .ok();
        let worker_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("collector".into())
            .spawn(move || {
                let mut w = Worker {
                    sampler,
                    shared: worker_shared,
                    notify,
                    interval,
                    demand,
                    prev: None,
                    prev_at: None,
                    started: Instant::now(),
                    tracker: ProcessTracker::new(),
                    insights: InsightEngine::new(),
                    meta_tx,
                    meta_rx: meta_done_rx,
                    startup_count: None,
                    ticks: 0,
                    last_describe: Instant::now(),
                    seq: 0,
                };
                w.run(rx);
            })
            .expect("spawn collector thread");
        Collector { tx: Some(tx), shared, thread: Some(thread), meta_thread }
    }

    pub fn send(&self, cmd: Command) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(cmd);
        }
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        // Closing the channel ends the loop; the metadata thread ends when the collector's sender drops.
        self.tx.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        if let Some(t) = self.meta_thread.take() {
            let _ = t.join();
        }
    }
}

fn metadata_worker(mut resolver: Box<dyn MetaResolver>, rx: Receiver<(ProcKey, Arc<str>)>, done: Sender<(ProcKey, ProcMeta)>, shared: Arc<Mutex<Shared>>) {
    struct FileEntry {
        description: Option<Arc<str>>,
        company: Option<Arc<str>>,
        icon: Option<u32>,
    }
    let mut files: HashMap<String, FileEntry> = HashMap::new();
    let mut next_icon = 1u32;
    while let Ok((key, name)) = rx.recv() {
        let result = catch_unwind(AssertUnwindSafe(|| {
            let id = resolver.identify(key, &name);
            let mut meta = ProcMeta { path: id.path.clone(), user: id.user, system: id.system, started: id.started, ..Default::default() };
            if let Some(path) = &id.path {
                let lower = path.to_lowercase();
                let entry = files.entry(lower).or_insert_with(|| {
                    let info = resolver.file_info(path);
                    let icon = info.icon.map(|px| {
                        let n = next_icon;
                        next_icon += 1;
                        if let Ok(mut s) = shared.lock() {
                            s.icons.push((n, px));
                        }
                        n
                    });
                    FileEntry { description: info.description, company: info.company, icon }
                });
                meta.description = entry.description.clone();
                meta.company = entry.company.clone();
                meta.icon = entry.icon;
            }
            meta
        }));
        let meta = result.unwrap_or_default();
        if done.send((key, meta)).is_err() {
            break;
        }
    }
}

struct Worker {
    sampler: Box<dyn Sampler>,
    shared: Arc<Mutex<Shared>>,
    notify: Box<dyn Fn() + Send>,
    interval: Duration,
    demand: Demand,
    prev: Option<RawSample>,
    prev_at: Option<Instant>,
    started: Instant,
    tracker: ProcessTracker,
    insights: InsightEngine,
    meta_tx: Sender<(ProcKey, Arc<str>)>,
    meta_rx: Receiver<(ProcKey, ProcMeta)>,
    startup_count: Option<u32>,
    ticks: u64,
    last_describe: Instant,
    seq: u64,
}

/// Hardware details are re-read this often to pick up new disks, adapters and free space.
const DESCRIBE_EVERY: Duration = Duration::from_secs(30);

impl Worker {
    fn run(&mut self, rx: Receiver<Command>) {
        self.describe(true);
        // A first sample right away and a second one shortly after, so rates show within a moment
        // of starting instead of after a whole interval.
        self.tick();
        let mut next = Instant::now() + Duration::from_millis(300);
        loop {
            let wait = next.saturating_duration_since(Instant::now());
            match rx.recv_timeout(wait) {
                Ok(cmd) => {
                    match cmd {
                        Command::Demand(d) => {
                            let woke = self.demand.paused && !d.paused;
                            let more = (d.processes && !self.demand.processes) || (d.process_gpu && !self.demand.process_gpu);
                            self.demand = d;
                            if woke {
                                // Rates across a long pause would be averages over the pause: start fresh.
                                self.prev = None;
                            }
                            if woke || more {
                                next = Instant::now();
                            }
                        }
                        Command::Interval(i) => {
                            self.interval = i.max(Duration::from_millis(250));
                            next = next.min(Instant::now() + self.interval);
                        }
                        Command::HistoryLen(n) => {
                            if let Ok(mut s) = self.shared.lock() {
                                s.hist.resize(n);
                            }
                        }
                        Command::RefreshNow => next = Instant::now(),
                        Command::StartupCount(n) => self.startup_count = n,
                        Command::Describe => {
                            self.describe(false);
                            (self.notify)();
                        }
                    }
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            let started = Instant::now();
            if !self.demand.paused {
                if self.last_describe.elapsed() >= DESCRIBE_EVERY {
                    self.describe(false);
                }
                self.tick();
            }
            next += self.interval;
            if next <= started {
                // Fell behind (the machine slept, or a sample was slow): don't burst to catch up.
                next = started + self.interval;
            }
        }
    }

    fn describe(&mut self, full: bool) {
        self.last_describe = Instant::now();
        match catch_unwind(AssertUnwindSafe(|| self.sampler.describe(full))) {
            Ok(info) => {
                if let Ok(mut s) = self.shared.lock() {
                    s.info = Arc::new(info);
                    s.info_seq += 1;
                }
            }
            Err(e) => self.record_error("describing hardware", e),
        }
    }

    fn record_error(&self, what: &str, e: Box<dyn std::any::Any + Send>) {
        let msg = e.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| e.downcast_ref::<String>().cloned()).unwrap_or_else(|| "unknown error".into());
        if let Ok(mut s) = self.shared.lock() {
            s.last_error = Some(format!("Failed {what}: {msg}"));
        }
    }

    fn request(&self) -> SampleRequest {
        let d = self.demand;
        // Services rarely change owner: refresh them every fifth tick.
        let services_due = self.ticks % 5 == 0;
        SampleRequest {
            processes: d.processes,
            windows: d.processes && d.windows,
            services: d.processes && d.services && services_due,
            process_gpu: d.processes && d.process_gpu,
            memory_detail: d.memory_detail,
            sensors: d.sensors || self.ticks % 5 == 0,
        }
    }

    fn tick(&mut self) {
        while let Ok((key, meta)) = self.meta_rx.try_recv() {
            self.tracker.set_meta(key, meta);
        }
        let req = self.request();
        let at = Instant::now();
        let raw = match catch_unwind(AssertUnwindSafe(|| self.sampler.sample(&req))) {
            Ok(raw) => raw,
            Err(e) => {
                self.record_error("reading counters", e);
                return;
            }
        };
        let cost = at.elapsed();
        self.ticks += 1;
        let secs = self.prev_at.map(|p| at.duration_since(p).as_secs_f64());
        let info = self.shared.lock().map(|s| s.info.clone()).unwrap_or_default();
        let frame = self.compute(&raw, secs, &info, req);
        self.prev = Some(raw);
        self.prev_at = Some(at);

        let Ok(mut s) = self.shared.lock() else { return };
        push_history(&mut s.hist, &frame, &info);
        let mut frame = frame;
        {
            let disk_active: Vec<(u32, &History)> = s.hist.disks.iter().map(|d| (d.id, &d.active)).collect();
            let gpu_util: Vec<(u64, &History)> = s.hist.gpus.iter().map(|g| (g.id, &g.util)).collect();
            frame.insights = self.insights.evaluate(&Context {
                frame: &frame,
                info: &info,
                interval_secs: self.interval.as_secs_f32(),
                mem_used_pct: &s.hist.mem_pct,
                disk_active: &disk_active,
                gpu_util: &gpu_util,
                growth: self.tracker.growth(),
                startup_enabled: self.startup_count,
            });
        }
        s.frame = Arc::new(frame);
        s.sample_cost = cost;
        drop(s);
        (self.notify)();
    }

    fn compute(&mut self, raw: &RawSample, secs: Option<f64>, info: &SystemInfo, req: SampleRequest) -> Frame {
        self.seq += 1;
        let prev = self.prev.as_ref();
        let mut frame = Frame { seq: self.seq, interval_secs: self.interval.as_secs_f32(), ..Default::default() };

        // CPU.
        let total_pair = prev.and_then(|p| p.cpu.total).zip(raw.cpu.total);
        if let Some((a, b)) = total_pair {
            if let Some(u) = cpu::usage(a, b) {
                frame.cpu.total = Some(u.busy);
                frame.cpu.kernel = Some(u.kernel);
            }
        }
        frame.cpu.cores = match prev {
            Some(p) if p.cpu.cores.len() == raw.cpu.cores.len() => {
                p.cpu.cores.iter().zip(&raw.cpu.cores).map(|(a, b)| cpu::usage(*a, *b).map(|u| u.busy)).collect()
            }
            _ => vec![None; raw.cpu.cores.len()],
        };
        let system_delta = total_pair.and_then(|(a, b): (CpuTimes, CpuTimes)| delta_u64(a.total(), b.total()));
        frame.cpu.speed_mhz = raw.cpu.performance_pct.zip(info.cpu.base_mhz).map(|(pct, base)| (base as f32 * pct / 100.0).round() as u32);
        frame.cpu.temperature_c = raw.thermal.iter().find(|t| t.kind == ThermalKind::Cpu).map(|t| t.celsius);
        frame.thermal = raw.thermal.clone();

        frame.mem = raw.mem;

        // Disks: the disk's own timestamps give the interval.
        for d in &raw.disks {
            let p = prev.and_then(|p| p.disks.iter().find(|x| x.id == d.id));
            let mut now = DiskNow { id: d.id, queue_depth: d.queue_depth, ..Default::default() };
            if let Some(p) = p {
                if let Some(dt) = delta_u64(p.query_time, d.query_time).filter(|dt| *dt > 0) {
                    let idle = delta_u64(p.idle_time, d.idle_time);
                    now.active = idle.and_then(|idle| busy_percent(dt.saturating_sub(idle), dt));
                    let s = dt as f64 / 1e7;
                    now.read = delta_u64(p.bytes_read, d.bytes_read).and_then(|b| rate(b, s));
                    now.write = delta_u64(p.bytes_written, d.bytes_written).and_then(|b| rate(b, s));
                    let ops = delta_u64(p.read_count, d.read_count).zip(delta_u64(p.write_count, d.write_count)).map(|(r, w)| r + w);
                    let time = delta_u64(p.read_time, d.read_time).zip(delta_u64(p.write_time, d.write_time)).map(|(r, w)| r + w);
                    now.response_ms = match (ops, time) {
                        (Some(0), Some(_)) => Some(0.0),
                        (Some(o), Some(t)) => Some((t as f64 / o as f64 / 10_000.0) as f32),
                        _ => None,
                    };
                }
            }
            frame.disks.push(now);
        }

        // Network.
        for n in &raw.nets {
            let p = prev.and_then(|p| p.nets.iter().find(|x| x.id == n.id));
            let mut now = NetNow { id: n.id, link_bps: n.link_bps, ..Default::default() };
            if let (Some(p), Some(s)) = (p, secs) {
                now.recv = delta_u64(p.in_octets, n.in_octets).and_then(|b| rate(b, s));
                now.send = delta_u64(p.out_octets, n.out_octets).and_then(|b| rate(b, s));
            }
            frame.nets.push(now);
        }

        // GPUs: engine running time over the GPU's own timestamps.
        for g in &raw.gpus {
            let p = prev.and_then(|p| p.gpus.iter().find(|x| x.id == g.id));
            let mut now = GpuNow {
                id: g.id,
                dedicated_used: g.dedicated_used,
                shared_used: g.shared_used,
                dedicated_limit: g.dedicated_limit,
                shared_limit: g.shared_limit,
                temperature_c: g.temperature_c,
                clock_mhz: g.clock_mhz,
                mem_clock_mhz: g.mem_clock_mhz,
                fan_rpm: g.fan_rpm,
                power_pct: g.power_pct,
                ..Default::default()
            };
            now.engines = vec![None; g.engine_time.len()];
            if let Some(p) = p {
                if let Some(dt) = delta_u64(p.timestamp, g.timestamp).filter(|dt| *dt > 0) {
                    for (i, t) in g.engine_time.iter().enumerate() {
                        let before = p.engine_time.get(i).copied().flatten();
                        now.engines[i] = match (before, t) {
                            (Some(b), Some(t)) => delta_u64(b, *t).and_then(|d| busy_percent(d, dt)),
                            _ => None,
                        };
                    }
                    now.util = now.engines.iter().flatten().copied().fold(None, |m: Option<f32>, v| Some(m.map_or(v, |m| m.max(v))));
                }
            }
            frame.gpus.push(now);
        }

        // Processes.
        if let Some(procs) = &raw.processes {
            let out = self.tracker.update(TrackInput {
                procs,
                system_delta,
                seconds: secs.unwrap_or(0.0),
                clock: self.started.elapsed().as_secs_f64(),
                windows: raw.windows.as_deref(),
                services: raw.services.as_deref(),
                gpu: raw.process_gpu.as_deref(),
            });
            for (key, name) in out.needs_meta {
                let _ = self.meta_tx.send((key, name));
            }
            frame.sys.processes = out.procs.iter().filter(|p| p.key.pid != 0).count() as u32;
            frame.sys.threads = out.procs.iter().map(|p| p.threads).sum();
            frame.processes = Arc::new(out.procs);
            frame.has_processes = true;
            frame.process_io_is_disk = raw.process_io_is_disk;
            frame.process_gpu = req.process_gpu && raw.process_gpu.is_some();
        } else if let Some(p) = prev.and_then(|p| p.processes.as_ref()) {
            // Not sampled this tick: keep the counts honest from the last list.
            frame.sys.processes = p.iter().filter(|p| p.pid != 0).count() as u32;
            frame.sys.threads = p.iter().map(|p| p.threads).sum();
        }
        frame.sys.uptime_secs = raw.uptime_secs;
        frame.sys.handles = raw.handles;
        frame
    }
}

fn push_history(h: &mut Histories, f: &Frame, info: &SystemInfo) {
    let cap = h.cap;
    h.cpu.push_opt(f.cpu.total);
    h.cpu_kernel.push_opt(f.cpu.kernel);
    if h.cores.len() != f.cpu.cores.len() {
        h.cores = (0..f.cpu.cores.len()).map(|_| History::new(cap)).collect();
    }
    for (hist, v) in h.cores.iter_mut().zip(&f.cpu.cores) {
        hist.push_opt(*v);
    }
    match f.mem {
        Some(m) => {
            h.mem_pct.push(m.used_pct());
            h.mem_used.push(m.used() as f32);
            h.commit.push(m.commit as f32);
        }
        None => {
            h.mem_pct.push_opt(None);
            h.mem_used.push_opt(None);
            h.commit.push_opt(None);
        }
    }
    h.disks.retain(|d| f.disks.iter().any(|x| x.id == d.id));
    for d in &f.disks {
        let idx = match h.disks.iter().position(|x| x.id == d.id) {
            Some(i) => i,
            None => {
                h.disks.push(DiskHist { id: d.id, active: History::new(cap), read: History::new(cap), write: History::new(cap) });
                h.disks.len() - 1
            }
        };
        let hist = &mut h.disks[idx];
        hist.active.push_opt(d.active);
        hist.read.push_opt(d.read.map(|v| v as f32));
        hist.write.push_opt(d.write.map(|v| v as f32));
    }
    h.nets.retain(|n| f.nets.iter().any(|x| x.id == n.id));
    for n in &f.nets {
        let idx = match h.nets.iter().position(|x| x.id == n.id) {
            Some(i) => i,
            None => {
                h.nets.push(NetHist { id: n.id, recv: History::new(cap), send: History::new(cap) });
                h.nets.len() - 1
            }
        };
        h.nets[idx].recv.push_opt(n.recv.map(|v| v as f32));
        h.nets[idx].send.push_opt(n.send.map(|v| v as f32));
    }
    h.gpus.retain(|g| f.gpus.iter().any(|x| x.id == g.id));
    for g in &f.gpus {
        let idx = match h.gpus.iter().position(|x| x.id == g.id) {
            Some(i) => i,
            None => {
                h.gpus.push(GpuHist { id: g.id, util: History::new(cap), engines: Vec::new(), dedicated: History::new(cap), shared: History::new(cap) });
                h.gpus.len() - 1
            }
        };
        let hist = &mut h.gpus[idx];
        let engines = info.gpus.iter().find(|x| x.id == g.id).map_or(g.engines.len(), |x| x.engines.len().max(g.engines.len()));
        if hist.engines.len() != engines {
            hist.engines = (0..engines).map(|_| History::new(cap)).collect();
        }
        hist.util.push_opt(g.util);
        for (i, e) in hist.engines.iter_mut().enumerate() {
            e.push_opt(g.engines.get(i).copied().flatten());
        }
        hist.dedicated.push_opt(g.dedicated_used.map(|v| v as f32));
        hist.shared.push_opt(g.shared_used.map(|v| v as f32));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{FileInfo, Identity};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A sampler that advances its counters by fixed amounts, and can be told to fail.
    struct Fake {
        n: u64,
        panic_on: Option<u64>,
    }

    impl Sampler for Fake {
        fn describe(&mut self, _full: bool) -> SystemInfo {
            SystemInfo { cpu: CpuInfo { base_mhz: Some(3000), logical: 2, ..Default::default() }, ..Default::default() }
        }
        fn sample(&mut self, req: &SampleRequest) -> RawSample {
            self.n += 1;
            if Some(self.n) == self.panic_on {
                panic!("sensor driver went away");
            }
            let n = self.n;
            // Two logical CPUs, 1 s apart: 2e7 units of total time, a quarter of it busy.
            let total = CpuTimes { idle: n * 15_000_000, kernel: n * 15_000_000 + n * 1_000_000, user: n * 4_000_000 };
            RawSample {
                cpu: RawCpu { total: Some(total), cores: vec![total, total], performance_pct: Some(110.0) },
                mem: Some(MemNow { total: 1000, available: 400, commit: 10, commit_limit: 100, ..Default::default() }),
                disks: vec![RawDisk { id: 0, bytes_read: n * 1_000_000, query_time: n * 10_000_000, idle_time: n * 5_000_000, ..Default::default() }],
                nets: vec![RawNet { id: 9, in_octets: n * 2000, out_octets: n * 1000, link_bps: None }],
                processes: req.processes.then(|| vec![RawProcess { pid: 42, created: 1, name: "a.exe".into(), cpu_time: n * 5_000_000, ..Default::default() }]),
                ..Default::default()
            }
        }
    }

    struct Resolver;
    impl MetaResolver for Resolver {
        fn identify(&mut self, _key: ProcKey, _name: &str) -> Identity {
            Identity { path: Some("C:\\a.exe".into()), ..Default::default() }
        }
        fn file_info(&mut self, _path: &str) -> FileInfo {
            FileInfo { description: Some("Thing A".into()), ..Default::default() }
        }
    }

    fn wait_for(c: &Collector, f: impl Fn(&Shared) -> bool) -> bool {
        for _ in 0..200 {
            if f(&c.shared.lock().unwrap()) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn computes_rates_after_two_samples() {
        let notified = Arc::new(AtomicU32::new(0));
        let n2 = notified.clone();
        let c = Collector::spawn(
            Box::new(Fake { n: 0, panic_on: None }),
            Box::new(Resolver),
            Box::new(move || {
                n2.fetch_add(1, Ordering::SeqCst);
            }),
            Duration::from_millis(50),
            10,
            Demand::default(),
        );
        assert!(wait_for(&c, |s| s.frame.cpu.total.is_some()));
        let s = c.shared.lock().unwrap();
        let f = &s.frame;
        assert!((f.cpu.total.unwrap() - 25.0).abs() < 0.01, "{:?}", f.cpu.total);
        assert_eq!(f.cpu.speed_mhz, Some(3300));
        assert_eq!(f.disks[0].active, Some(50.0));
        assert_eq!(f.disks[0].read, Some(1_000_000.0));
        assert!(f.nets[0].recv.is_some());
        let p = &f.processes[0];
        assert!((p.cpu.unwrap() - 25.0).abs() < 0.01, "5e6 of 2e7 units: {:?}", p.cpu);
        assert!(notified.load(Ordering::SeqCst) >= 2);
        drop(s);
        assert!(wait_for(&c, |s| s.frame.processes.first().is_some_and(|p| p.description() == Some("Thing A"))), "metadata arrives");
    }

    #[test]
    fn a_panicking_provider_does_not_stop_sampling() {
        let c = Collector::spawn(Box::new(Fake { n: 0, panic_on: Some(2) }), Box::new(Resolver), Box::new(|| {}), Duration::from_millis(30), 10, Demand::default());
        assert!(wait_for(&c, |s| s.last_error.is_some()));
        assert!(wait_for(&c, |s| s.frame.seq >= 3), "sampling continues after the failure");
    }

    #[test]
    fn processes_are_skipped_when_not_needed() {
        let demand = Demand { processes: false, ..Demand::default() };
        let c = Collector::spawn(Box::new(Fake { n: 0, panic_on: None }), Box::new(Resolver), Box::new(|| {}), Duration::from_millis(30), 10, demand);
        assert!(wait_for(&c, |s| s.frame.seq >= 2));
        assert!(!c.shared.lock().unwrap().frame.has_processes);
    }

    #[test]
    fn history_is_bounded() {
        let c = Collector::spawn(Box::new(Fake { n: 0, panic_on: None }), Box::new(Resolver), Box::new(|| {}), Duration::from_millis(5), 4, Demand::default());
        assert!(wait_for(&c, |s| s.frame.seq >= 12));
        let s = c.shared.lock().unwrap();
        assert_eq!(s.hist.cpu.len(), 4);
        assert_eq!(s.hist.disks[0].read.len(), 4);
    }
}

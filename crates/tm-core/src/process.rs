//! Turns raw process counters into per-process rates, and tracks process identities.
//!
//! Processes are keyed by PID *and* creation time, so a recycled PID is a new process with a fresh
//! history rather than inheriting the old one's counters (which would produce nonsense rates).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::counter::{delta_u64, rate};
use crate::cpu::process_share;
use crate::model::*;

#[derive(Clone, Copy, Debug)]
struct Prev {
    cpu_time: u64,
    read: u64,
    write: u64,
}

/// Committed-memory samples for spotting a process that keeps growing.
#[derive(Clone, Debug, Default)]
pub struct Growth {
    /// (seconds since tracking started, commit bytes), one every [`GROWTH_STEP_SECS`].
    pub samples: VecDeque<(f64, u64)>,
}

pub const GROWTH_STEP_SECS: f64 = 30.0;
/// One hour of growth samples.
const GROWTH_KEEP: usize = 120;

pub struct TrackInput<'a> {
    pub procs: &'a [RawProcess],
    /// Change in total CPU time of all logical processors since the last sample (100 ns).
    pub system_delta: Option<u64>,
    pub seconds: f64,
    /// Seconds since the tracker started; used to space out growth samples.
    pub clock: f64,
    pub windows: Option<&'a [WindowOwner]>,
    pub services: Option<&'a [ServiceOwner]>,
    pub gpu: Option<&'a [RawProcessGpu]>,
}

#[derive(Default)]
pub struct ProcessTracker {
    prev: HashMap<ProcKey, Prev>,
    meta: HashMap<ProcKey, Arc<ProcMeta>>,
    requested: HashMap<ProcKey, ()>,
    gpu_prev: HashMap<ProcKey, (u64, Vec<(Arc<str>, u64)>)>,
    growth: HashMap<ProcKey, Growth>,
    last_growth_clock: Option<f64>,
    windows: HashMap<u32, bool>,
    services: HashMap<u32, Vec<(Arc<str>, Arc<str>)>>,
}

pub struct TrackOutput {
    pub procs: Vec<ProcNow>,
    /// Processes seen for the first time that still need path, owner and icon.
    pub needs_meta: Vec<(ProcKey, Arc<str>)>,
}

impl ProcessTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_meta(&mut self, key: ProcKey, meta: ProcMeta) {
        // A late answer for a process that already exited is dropped.
        if self.prev.contains_key(&key) {
            self.meta.insert(key, Arc::new(meta));
        }
    }

    pub fn meta(&self, key: &ProcKey) -> Option<&Arc<ProcMeta>> {
        self.meta.get(key)
    }

    pub fn growth(&self) -> &HashMap<ProcKey, Growth> {
        &self.growth
    }

    pub fn update(&mut self, input: TrackInput) -> TrackOutput {
        if let Some(w) = input.windows {
            self.windows.clear();
            for owner in w {
                let hung = self.windows.entry(owner.pid).or_insert(false);
                *hung |= owner.hung;
            }
        }
        if let Some(s) = input.services {
            self.services.clear();
            for svc in s {
                self.services.entry(svc.pid).or_default().push((svc.name.clone(), svc.display.clone()));
            }
        }
        let mut gpu_now: HashMap<ProcKey, (u64, &[(Arc<str>, u64)])> = HashMap::new();
        if let Some(g) = input.gpu {
            for p in g {
                gpu_now.insert(ProcKey { pid: p.pid, created: p.created }, (p.timestamp, &p.engines));
            }
        }
        let take_growth = match self.last_growth_clock {
            None => true,
            Some(t) => input.clock - t >= GROWTH_STEP_SECS,
        };
        if take_growth {
            self.last_growth_clock = Some(input.clock);
        }

        let mut out = Vec::with_capacity(input.procs.len());
        let mut needs_meta = Vec::new();
        let mut next_prev = HashMap::with_capacity(input.procs.len());
        let mut next_gpu = HashMap::new();
        for raw in input.procs {
            let key = ProcKey { pid: raw.pid, created: raw.created };
            let prev = self.prev.get(&key);
            let cpu = match (prev, input.system_delta) {
                (Some(p), Some(sys)) => delta_u64(p.cpu_time, raw.cpu_time).and_then(|d| process_share(d, sys)),
                _ => None,
            };
            let read = prev.and_then(|p| delta_u64(p.read, raw.read_bytes)).and_then(|d| rate(d, input.seconds));
            let write = prev.and_then(|p| delta_u64(p.write, raw.write_bytes)).and_then(|d| rate(d, input.seconds));
            next_prev.insert(key, Prev { cpu_time: raw.cpu_time, read: raw.read_bytes, write: raw.write_bytes });

            let (gpu, gpu_engine) = match gpu_now.get(&key) {
                Some(&(ts, engines)) => {
                    let result = self.gpu_prev.get(&key).and_then(|(pts, pengines)| {
                        let dt = ts.checked_sub(*pts)?;
                        if dt == 0 {
                            return None;
                        }
                        let mut best: Option<(f32, &Arc<str>)> = None;
                        for (name, t) in engines {
                            let before = pengines.iter().find(|(n, _)| n == name).map(|(_, v)| *v).unwrap_or(*t);
                            let Some(d) = t.checked_sub(before) else { continue };
                            let pct = ((d as f64 / dt as f64) * 100.0).clamp(0.0, 100.0) as f32;
                            if best.is_none_or(|(b, _)| pct > b) {
                                best = Some((pct, name));
                            }
                        }
                        Some(best.map_or((0.0, None), |(p, n)| (p, (p >= 0.05).then(|| n.clone()))))
                    });
                    next_gpu.insert(key, (ts, engines.to_vec()));
                    match result {
                        Some((p, n)) => (Some(p), n),
                        None => (None, None),
                    }
                }
                // Not sampled, or the platform couldn't query this process: unknown, not idle.
                None => (None, None),
            };

            let meta = self.meta.get(&key).cloned();
            if meta.is_none() && !self.requested.contains_key(&key) {
                self.requested.insert(key, ());
                needs_meta.push((key, raw.name.clone()));
            }
            let meta = match (meta, &raw.user) {
                // The sampler sometimes knows the owner before the resolver does.
                (None, Some(user)) => Some(Arc::new(ProcMeta { user: Some(user.clone()), ..Default::default() })),
                (m, _) => m,
            };

            let status = if raw.suspended {
                ProcStatus::Suspended
            } else if self.windows.get(&raw.pid).copied().unwrap_or(false) {
                ProcStatus::NotResponding
            } else {
                ProcStatus::Running
            };

            if take_growth && raw.pid > 4 {
                let g = self.growth.entry(key).or_default();
                g.samples.push_back((input.clock, raw.commit));
                while g.samples.len() > GROWTH_KEEP {
                    g.samples.pop_front();
                }
            }

            out.push(ProcNow {
                key,
                ppid: raw.ppid,
                name: raw.name.clone(),
                cpu,
                private_ws: raw.private_ws,
                working_set: raw.working_set,
                commit: raw.commit,
                read,
                write,
                gpu,
                gpu_engine,
                threads: raw.threads,
                handles: raw.handles,
                base_priority: raw.base_priority,
                session: raw.session,
                status,
                has_window: self.windows.contains_key(&raw.pid),
                cpu_time: raw.cpu_time,
                services: self.services.get(&raw.pid).cloned().unwrap_or_default(),
                meta,
            });
        }
        // Forget everything about processes that exited.
        self.prev = next_prev;
        self.meta.retain(|k, _| self.prev.contains_key(k));
        self.requested.retain(|k, _| self.prev.contains_key(k));
        self.growth.retain(|k, _| self.prev.contains_key(k));
        if input.gpu.is_some() {
            self.gpu_prev = next_gpu;
        } else {
            self.gpu_prev.clear();
        }
        TrackOutput { procs: out, needs_meta }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(pid: u32, created: u64, cpu_time: u64, read: u64) -> RawProcess {
        RawProcess { pid, created, name: format!("p{pid}.exe").into(), cpu_time, read_bytes: read, commit: 100, ..Default::default() }
    }

    fn input<'a>(procs: &'a [RawProcess], system_delta: u64) -> TrackInput<'a> {
        TrackInput { procs, system_delta: Some(system_delta), seconds: 1.0, clock: 0.0, windows: None, services: None, gpu: None }
    }

    #[test]
    fn first_sight_has_no_rate_then_rates_follow() {
        let mut t = ProcessTracker::new();
        let out = t.update(input(&[raw(10, 1, 0, 0)], 12_000));
        assert_eq!(out.procs[0].cpu, None, "no previous sample yet");
        assert_eq!(out.needs_meta.len(), 1);
        let out = t.update(input(&[raw(10, 1, 3_000, 2048)], 12_000));
        assert_eq!(out.procs[0].cpu, Some(25.0));
        assert_eq!(out.procs[0].read, Some(2048.0));
        assert!(out.needs_meta.is_empty(), "metadata is asked for once");
    }

    #[test]
    fn reused_pid_is_a_new_process() {
        let mut t = ProcessTracker::new();
        t.update(input(&[raw(10, 1, 9_000_000, 0)], 12_000));
        // Same PID, different creation time, smaller counters: must not produce a rate.
        let out = t.update(input(&[raw(10, 2, 100, 0)], 12_000));
        assert_eq!(out.procs[0].cpu, None);
        assert_eq!(out.needs_meta.len(), 1, "the new process needs its own metadata");
    }

    #[test]
    fn exited_processes_are_forgotten() {
        let mut t = ProcessTracker::new();
        t.update(input(&[raw(10, 1, 0, 0), raw(11, 1, 0, 0)], 1));
        t.set_meta(ProcKey { pid: 11, created: 1 }, ProcMeta::default());
        t.update(input(&[raw(10, 1, 0, 0)], 1));
        assert!(t.meta(&ProcKey { pid: 11, created: 1 }).is_none());
        // A late metadata answer for an exited process is ignored.
        t.set_meta(ProcKey { pid: 11, created: 1 }, ProcMeta::default());
        assert!(t.meta(&ProcKey { pid: 11, created: 1 }).is_none());
    }

    #[test]
    fn counter_reset_gives_unknown_rate() {
        let mut t = ProcessTracker::new();
        t.update(input(&[raw(10, 1, 0, 5_000)], 1));
        let out = t.update(input(&[raw(10, 1, 0, 10)], 1));
        assert_eq!(out.procs[0].read, None);
    }

    #[test]
    fn windows_and_status() {
        let mut t = ProcessTracker::new();
        let procs = [raw(10, 1, 0, 0), raw(11, 1, 0, 0), RawProcess { suspended: true, ..raw(12, 1, 0, 0) }];
        let windows = [WindowOwner { pid: 10, hung: false }, WindowOwner { pid: 11, hung: true }];
        let out = t.update(TrackInput { windows: Some(&windows), ..input(&procs, 1) });
        assert!(out.procs[0].has_window);
        assert_eq!(out.procs[0].status, ProcStatus::Running);
        assert_eq!(out.procs[1].status, ProcStatus::NotResponding);
        assert_eq!(out.procs[2].status, ProcStatus::Suspended);
        assert!(!out.procs[2].has_window);
    }

    #[test]
    fn gpu_share_is_the_busiest_engine() {
        let mut t = ProcessTracker::new();
        let procs = [raw(10, 1, 0, 0)];
        let a = [RawProcessGpu { pid: 10, created: 1, timestamp: 0, engines: vec![("3D".into(), 0), ("Copy".into(), 0)] }];
        let out = t.update(TrackInput { gpu: Some(&a), ..input(&procs, 1) });
        assert_eq!(out.procs[0].gpu, None);
        let b = [RawProcessGpu { pid: 10, created: 1, timestamp: 10_000_000, engines: vec![("3D".into(), 4_000_000), ("Copy".into(), 1_000_000)] }];
        let out = t.update(TrackInput { gpu: Some(&b), ..input(&procs, 1) });
        assert_eq!(out.procs[0].gpu, Some(40.0));
        assert_eq!(out.procs[0].gpu_engine.as_deref(), Some("3D"));
    }

    #[test]
    fn growth_is_sampled_every_thirty_seconds() {
        let mut t = ProcessTracker::new();
        for clock in [0.0, 10.0, 20.0, 30.0, 45.0, 61.0] {
            t.update(TrackInput { clock, ..input(&[raw(10, 1, 0, 0)], 1) });
        }
        assert_eq!(t.growth()[&ProcKey { pid: 10, created: 1 }].samples.len(), 3);
    }
}

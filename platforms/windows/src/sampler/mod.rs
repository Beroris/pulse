//! Windows counters behind [`tm_core::platform::Sampler`].
//!
//! Sources, chosen for cost and correctness:
//! - CPU: `NtQuerySystemInformation(SystemProcessorPerformanceInformation)` per processor group.
//! - Processes: one `NtQuerySystemInformation` call returns every process with its counters.
//! - Memory: `GlobalMemoryStatusEx`, `GetPerformanceInfo`, plus PDH for the standby/modified lists.
//! - Disks: `IOCTL_DISK_PERFORMANCE` on each physical disk.
//! - Network: `GetIfEntry2` per adapter.
//! - GPU: the D3DKMT statistics interface (what Task Manager uses), names and sizes from DXGI.
//! - Temperatures: GPU from D3DKMT adapter performance data; ACPI thermal zones from PDH.
//! No WMI anywhere: WMI queries cost tens of milliseconds and wake a service process.

mod disks;
mod gpu;
mod info;
mod net;
mod procs;

use std::sync::Arc;

use tm_core::cpu::CpuTimes;
use tm_core::model::*;
use tm_core::platform::{SampleRequest, Sampler};
use windows::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
use windows::Win32::System::SystemInformation::{GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetActiveProcessorGroupCount;

use crate::pdh;

pub use procs::{Snapshot, account_name, enum_service_owners, enum_services, top_windows};

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn NtQuerySystemInformationEx(class: i32, input: *const core::ffi::c_void, input_len: u32, info: *mut core::ffi::c_void, len: u32, ret: *mut u32) -> i32;
}

/// SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ProcessorPerf {
    idle: i64,
    kernel: i64,
    user: i64,
    dpc: i64,
    interrupt: i64,
    interrupt_count: u32,
}

struct PdhSet {
    q: pdh::Query,
    perf: usize,
    standby: [usize; 3],
    modified: usize,
    free: usize,
    paged: usize,
    nonpaged: usize,
    thermal_hp: usize,
    thermal: usize,
}

pub struct WinSampler {
    pdh: Option<PdhSet>,
    procs: procs::Snapshot,
    disks: disks::Disks,
    nets: net::Nets,
    gpus: Option<gpu::Gpus>,
    /// Size of the memory compression store, from the last process list.
    compressed: Option<u64>,
    cpu_buf: Vec<ProcessorPerf>,
    static_cache: Option<(CpuInfo, MemInfo, String, String)>,
    scm: Option<procs::Scm>,
}

impl WinSampler {
    pub fn new() -> WinSampler {
        WinSampler {
            pdh: None,
            procs: procs::Snapshot::new(),
            disks: disks::Disks::default(),
            nets: net::Nets::default(),
            gpus: None,
            compressed: None,
            cpu_buf: Vec::new(),
            static_cache: None,
            scm: procs::Scm::open(),
        }
    }

    fn pdh(&mut self) -> Option<&mut PdhSet> {
        if self.pdh.is_none() {
            let mut q = pdh::Query::new()?;
            let perf = q.add(r"\Processor Information(_Total)\% Processor Performance");
            let standby = [
                q.add(r"\Memory\Standby Cache Reserve Bytes"),
                q.add(r"\Memory\Standby Cache Normal Priority Bytes"),
                q.add(r"\Memory\Standby Cache Core Bytes"),
            ];
            let modified = q.add(r"\Memory\Modified Page List Bytes");
            let free = q.add(r"\Memory\Free & Zero Page List Bytes");
            let paged = q.add(r"\Memory\Pool Paged Bytes");
            let nonpaged = q.add(r"\Memory\Pool Nonpaged Bytes");
            let thermal_hp = q.add(r"\Thermal Zone Information(*)\High Precision Temperature");
            let thermal = q.add(r"\Thermal Zone Information(*)\Temperature");
            q.collect();
            self.pdh = Some(PdhSet { q, perf, standby, modified, free, paged, nonpaged, thermal_hp, thermal });
        }
        self.pdh.as_mut()
    }

    fn cpu(&mut self) -> RawCpu {
        let groups = unsafe { GetActiveProcessorGroupCount() }.max(1);
        let mut cores = Vec::new();
        for g in 0..groups {
            if self.cpu_buf.len() < 64 {
                self.cpu_buf.resize(64, ProcessorPerf::default());
            }
            let mut ret = 0u32;
            let status = unsafe {
                NtQuerySystemInformationEx(
                    8, // SystemProcessorPerformanceInformation
                    &g as *const u16 as *const _,
                    2,
                    self.cpu_buf.as_mut_ptr() as *mut _,
                    (self.cpu_buf.len() * std::mem::size_of::<ProcessorPerf>()) as u32,
                    &mut ret,
                )
            };
            if status < 0 {
                continue;
            }
            let n = ret as usize / std::mem::size_of::<ProcessorPerf>();
            for p in &self.cpu_buf[..n.min(self.cpu_buf.len())] {
                cores.push(CpuTimes { idle: p.idle as u64, kernel: p.kernel as u64, user: p.user as u64 });
            }
        }
        let total = (!cores.is_empty()).then(|| {
            cores.iter().fold(CpuTimes::default(), |a, c| CpuTimes { idle: a.idle + c.idle, kernel: a.kernel + c.kernel, user: a.user + c.user })
        });
        RawCpu { total, cores, performance_pct: None }
    }

    fn memory(&mut self, detail: bool) -> (Option<MemNow>, Option<u64>) {
        let mut ms = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        let mut pi = PERFORMANCE_INFORMATION { cb: std::mem::size_of::<PERFORMANCE_INFORMATION>() as u32, ..Default::default() };
        let ok_ms = unsafe { GlobalMemoryStatusEx(&mut ms) }.is_ok();
        let ok_pi = unsafe { GetPerformanceInfo(&mut pi, pi.cb) }.is_ok();
        if !ok_ms {
            return (None, None);
        }
        let page = if ok_pi { pi.PageSize as u64 } else { 4096 };
        let mut m = MemNow {
            total: ms.ullTotalPhys,
            available: ms.ullAvailPhys,
            commit: if ok_pi { pi.CommitTotal as u64 * page } else { ms.ullTotalPageFile - ms.ullAvailPageFile },
            commit_limit: if ok_pi { pi.CommitLimit as u64 * page } else { ms.ullTotalPageFile },
            compressed: self.compressed,
            ..Default::default()
        };
        if ok_pi {
            m.paged_pool = Some(pi.KernelPaged as u64 * page);
            m.nonpaged_pool = Some(pi.KernelNonpaged as u64 * page);
        }
        if detail {
            if let Some(p) = self.pdh() {
                let sum: Option<f64> = p.standby.iter().map(|&i| p.q.value(i)).sum();
                m.standby = sum.map(|v| v as u64);
                m.modified = p.q.value(p.modified).map(|v| v as u64);
                m.free = p.q.value(p.free).map(|v| v as u64);
                m.cached = m.standby.zip(m.modified).map(|(s, md)| s + md);
                if let Some(v) = p.q.value(p.paged) {
                    m.paged_pool = Some(v as u64);
                }
                if let Some(v) = p.q.value(p.nonpaged) {
                    m.nonpaged_pool = Some(v as u64);
                }
            }
        }
        (Some(m), ok_pi.then_some(pi.HandleCount as u64))
    }

    fn thermal(&mut self) -> Vec<ThermalReading> {
        let Some(p) = self.pdh() else { return Vec::new() };
        // "High Precision Temperature" is in tenths of a kelvin; the older counter in kelvin.
        let mut zones: Vec<(String, f64)> = p.q.array(p.thermal_hp).into_iter().map(|(n, v)| (n, v / 10.0)).collect();
        if zones.is_empty() {
            zones = p.q.array(p.thermal);
        }
        zones
            .into_iter()
            .filter_map(|(name, kelvin)| {
                let c = (kelvin - 273.15) as f32;
                // A zone reporting 0 K or an impossible value isn't a working sensor.
                (c > 1.0 && c < 150.0).then(|| ThermalReading { name: zone_name(&name).into(), celsius: c, kind: ThermalKind::Zone })
            })
            .collect()
    }
}

/// "\_TZ.THRM" → "THRM".
fn zone_name(instance: &str) -> String {
    instance.rsplit(['.', '\\']).next().unwrap_or(instance).trim().to_string()
}

impl Default for WinSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler for WinSampler {
    fn describe(&mut self, full: bool) -> SystemInfo {
        if full || self.static_cache.is_none() {
            self.static_cache = Some((info::cpu_info(), info::mem_info(), info::os_name(), info::host_name()));
        }
        if self.gpus.is_none() || full {
            self.gpus = gpu::Gpus::new();
        }
        let (cpu, mut mem, os, host) = self.static_cache.clone().unwrap_or_default();
        let mut ms = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        if unsafe { GlobalMemoryStatusEx(&mut ms) }.is_ok() {
            mem.total = ms.ullTotalPhys;
        }
        SystemInfo {
            os,
            host,
            cpu,
            mem,
            disks: self.disks.describe(),
            nets: self.nets.describe(),
            gpus: self.gpus.as_ref().map(|g| g.info()).unwrap_or_default(),
            elevated: crate::util::is_elevated(),
        }
    }

    fn sample(&mut self, req: &SampleRequest) -> RawSample {
        if let Some(p) = self.pdh() {
            p.q.collect();
        }
        let mut cpu = self.cpu();
        cpu.performance_pct = self.pdh().and_then(|p| p.q.value(p.perf)).map(|v| v as f32);
        let (mem, handles) = self.memory(req.memory_detail);
        let mut raw = RawSample {
            cpu,
            mem,
            disks: self.disks.sample(),
            nets: self.nets.sample(),
            uptime_secs: Some(unsafe { GetTickCount64() } / 1000),
            handles,
            ..Default::default()
        };
        if let Some(g) = self.gpus.as_mut() {
            raw.gpus = g.sample(req.sensors, req.gpu_engines);
        }
        if req.sensors {
            raw.thermal = self.thermal();
        }
        if req.processes {
            if let Some((list, disk_io)) = self.procs.sample() {
                self.compressed = list.iter().find(|p| p.pid > 4 && &*p.name == "Memory Compression").map(|p| p.working_set);
                if let Some(m) = raw.mem.as_mut() {
                    m.compressed = self.compressed;
                }
                if req.process_gpu {
                    if let Some(g) = self.gpus.as_mut() {
                        raw.process_gpu = Some(g.sample_processes(&list));
                    }
                }
                raw.process_io_is_disk = disk_io;
                raw.processes = Some(list);
            }
            if req.windows {
                raw.windows = Some(top_windows());
            }
            if req.services {
                raw.services = self.scm.as_ref().map(|s| enum_service_owners(s));
            }
        } else if let Some(g) = self.gpus.as_mut() {
            g.forget_processes();
        }
        raw
    }
}

/// Shared names: one allocation per distinct string instead of one per sample.
#[derive(Default)]
pub struct Interner(std::collections::HashMap<String, Arc<str>>);

impl Interner {
    pub fn get(&mut self, s: &str) -> Arc<str> {
        if let Some(a) = self.0.get(s) {
            return a.clone();
        }
        let a: Arc<str> = s.into();
        // Process names come and go; keep the table from growing without bound.
        if self.0.len() > 4096 {
            self.0.clear();
        }
        self.0.insert(s.to_string(), a.clone());
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_names() {
        assert_eq!(zone_name(r"\_TZ.THRM"), "THRM");
        assert_eq!(zone_name("TZ00"), "TZ00");
    }

    /// What each source costs per tick on this machine. Not a pass/fail test:
    /// `cargo test --release -p tm-windows -- --ignored --nocapture source_costs`
    #[test]
    #[ignore]
    fn source_costs() {
        fn time(label: &str, mut f: impl FnMut()) {
            const N: u32 = 200;
            f();
            let t = std::time::Instant::now();
            for _ in 0..N {
                f();
            }
            println!("{label:<34}{:>8.3} ms", t.elapsed().as_secs_f64() * 1000.0 / N as f64);
        }
        let mut s = WinSampler::new();
        s.describe(true);
        let all = SampleRequest { processes: true, windows: true, services: true, process_gpu: true, memory_detail: true, sensors: true, gpu_engines: true };
        let hidden = SampleRequest { processes: true, windows: false, services: false, process_gpu: false, memory_detail: false, sensors: false, gpu_engines: false };
        s.sample(&all);
        time("pdh collect", || {
            s.pdh().map(|p| p.q.collect());
        });
        time("cpu", || {
            s.cpu();
        });
        time("memory", || {
            s.memory(false);
        });
        time("memory, detail", || {
            s.memory(true);
        });
        time("disks", || {
            s.disks.sample();
        });
        time("network", || {
            s.nets.sample();
        });
        time("gpu", || {
            s.gpus.as_mut().map(|g| g.sample(false, true));
        });
        time("gpu, sensors", || {
            s.gpus.as_mut().map(|g| g.sample(true, true));
        });
        time("thermal zones", || {
            s.thermal();
        });
        time("processes", || {
            s.procs.sample();
        });
        let list = s.procs.sample().map(|(l, _)| l).unwrap_or_default();
        time("per-process gpu", || {
            s.gpus.as_mut().map(|g| g.sample_processes(&list));
        });
        time("windows", || {
            top_windows();
        });
        time("service owners", || {
            s.scm.as_ref().map(enum_service_owners);
        });
        time("whole sample: hidden", || {
            s.sample(&hidden);
        });
        time("whole sample: everything", || {
            s.sample(&all);
        });
    }

    #[test]
    fn samples_real_counters() {
        let mut s = WinSampler::new();
        let info = s.describe(true);
        assert!(info.cpu.logical > 0, "{:?}", info.cpu);
        assert!(info.mem.total > 0);
        let req = SampleRequest { processes: true, windows: true, services: true, process_gpu: true, memory_detail: true, sensors: true, gpu_engines: true };
        let raw = s.sample(&req);
        assert_eq!(raw.cpu.cores.len() as u32, info.cpu.logical);
        let procs = raw.processes.expect("process list");
        assert!(procs.len() > 10);
        let me = std::process::id();
        assert!(procs.iter().any(|p| p.pid == me), "finds its own process");
        assert!(raw.mem.unwrap().total > 0);
    }
}

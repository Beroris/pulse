//! Data shared between a platform backend, the engine and the UI.
//!
//! `Raw*` types are what a backend reports: cumulative counters exactly as the OS keeps them.
//! `*Now` types are what the engine computes from two raw samples: rates and percentages.
//! `*Info` types describe hardware that rarely changes.
//!
//! Anything a platform can't measure is `None`. The UI shows that as unavailable, never as zero.

use std::sync::Arc;

use crate::cpu::CpuTimes;

/// A process identity that survives PID reuse: Windows recycles PIDs quickly, so every action
/// checks the creation time too before touching a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct ProcKey {
    pub pid: u32,
    /// Creation time in the platform's native unit (100 ns since 1601 on Windows). 0 if unknown.
    pub created: u64,
}

// ─── Static hardware description ───────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct SystemInfo {
    pub os: String,
    pub host: String,
    pub cpu: CpuInfo,
    pub mem: MemInfo,
    pub disks: Vec<DiskInfo>,
    pub nets: Vec<NetInfo>,
    pub gpus: Vec<GpuInfo>,
    /// Whether this process runs with administrator rights.
    pub elevated: bool,
}

#[derive(Clone, Debug, Default)]
pub struct CpuInfo {
    pub name: String,
    pub vendor: String,
    pub base_mhz: Option<u32>,
    pub sockets: u32,
    pub cores: u32,
    pub logical: u32,
    pub l1_bytes: Option<u64>,
    pub l2_bytes: Option<u64>,
    pub l3_bytes: Option<u64>,
    pub virtualization: Option<bool>,
}

#[derive(Clone, Debug, Default)]
pub struct MemInfo {
    /// Physical memory usable by the OS.
    pub total: u64,
    /// Physically installed, including what firmware and hardware reserve.
    pub installed: Option<u64>,
    pub speed_mts: Option<u32>,
    pub slots_used: Option<u32>,
    pub slots_total: Option<u32>,
    pub form_factor: Option<String>,
    pub kind: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DiskKind {
    Ssd,
    Hdd,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default)]
pub struct VolumeInfo {
    /// "C:" and so on.
    pub mount: String,
    pub label: String,
    pub filesystem: String,
    pub total: Option<u64>,
    pub free: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct DiskInfo {
    /// Stable id: the physical disk number on Windows.
    pub id: u32,
    pub model: String,
    pub kind: DiskKind,
    pub bus: Option<String>,
    pub size: Option<u64>,
    pub volumes: Vec<VolumeInfo>,
    pub system: bool,
    pub pagefile: bool,
    pub removable: bool,
}

impl DiskInfo {
    /// "Disk 0 (C: D:)".
    pub fn title(&self) -> String {
        let mounts: Vec<&str> = self.volumes.iter().map(|v| v.mount.as_str()).filter(|m| !m.is_empty()).collect();
        if mounts.is_empty() { format!("Disk {}", self.id) } else { format!("Disk {} ({})", self.id, mounts.join(" ")) }
    }
}

#[derive(Clone, Debug, Default)]
pub struct NetInfo {
    pub id: u64,
    pub name: String,
    pub description: String,
    pub kind: String,
    pub mac: Option<String>,
    pub link_bps: Option<u64>,
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    pub dns_suffix: Option<String>,
    pub connected: bool,
}

#[derive(Clone, Debug, Default)]
pub struct GpuInfo {
    pub id: u64,
    pub name: String,
    pub vendor: String,
    pub dedicated: Option<u64>,
    pub shared: Option<u64>,
    pub driver_version: Option<String>,
    pub driver_date: Option<String>,
    pub location: Option<String>,
    /// Engine names, indexed like [`RawGpu::engine_time`].
    pub engines: Vec<Arc<str>>,
}

// ─── Raw counters from a backend ───────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct RawSample {
    pub cpu: RawCpu,
    pub mem: Option<MemNow>,
    pub disks: Vec<RawDisk>,
    pub nets: Vec<RawNet>,
    pub gpus: Vec<RawGpu>,
    /// `None` when the request didn't ask for processes.
    pub processes: Option<Vec<RawProcess>>,
    /// Processes that own a visible, titled top-level window.
    pub windows: Option<Vec<WindowOwner>>,
    /// Services hosted per process, when requested.
    pub services: Option<Vec<ServiceOwner>>,
    /// Per-process GPU running time (100 ns) summed per engine type, when requested.
    pub process_gpu: Option<Vec<RawProcessGpu>>,
    /// Whether per-process read/write counts only physical disk I/O (true) or all I/O (false).
    pub process_io_is_disk: bool,
    pub thermal: Vec<ThermalReading>,
    pub uptime_secs: Option<u64>,
    pub handles: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct RawCpu {
    pub total: Option<CpuTimes>,
    pub cores: Vec<CpuTimes>,
    /// Current speed as a percentage of base speed, when the OS reports it.
    pub performance_pct: Option<f32>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RawDisk {
    pub id: u32,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub read_count: u64,
    pub write_count: u64,
    /// Cumulative time spent on reads and writes (100 ns).
    pub read_time: u64,
    pub write_time: u64,
    /// Cumulative idle time (100 ns) and the timestamp of the reading in the same unit.
    pub idle_time: u64,
    pub query_time: u64,
    pub queue_depth: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RawNet {
    pub id: u64,
    pub in_octets: u64,
    pub out_octets: u64,
    pub link_bps: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct RawGpu {
    pub id: u64,
    /// Timestamp of the reading (100 ns), so engine time can be turned into a percentage.
    pub timestamp: u64,
    /// Cumulative running time per engine (100 ns), indexed like [`GpuInfo::engines`].
    pub engine_time: Vec<Option<u64>>,
    pub dedicated_used: Option<u64>,
    pub shared_used: Option<u64>,
    pub dedicated_limit: Option<u64>,
    pub shared_limit: Option<u64>,
    pub temperature_c: Option<f32>,
    pub clock_mhz: Option<u32>,
    pub mem_clock_mhz: Option<u32>,
    pub fan_rpm: Option<u32>,
    pub power_pct: Option<f32>,
}

#[derive(Clone, Debug, Default)]
pub struct RawProcess {
    pub pid: u32,
    pub ppid: u32,
    pub created: u64,
    pub name: Arc<str>,
    pub threads: u32,
    pub handles: u32,
    /// Kernel + user time (100 ns).
    pub cpu_time: u64,
    /// Private working set: memory only this process uses that is in RAM. What Task Manager shows.
    pub private_ws: u64,
    pub working_set: u64,
    /// Private bytes committed (the commit size).
    pub commit: u64,
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub base_priority: i32,
    pub session: u32,
    pub suspended: bool,
    /// Owner, when the OS reported it with the process list (saves opening the process).
    pub user: Option<Arc<str>>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WindowOwner {
    pub pid: u32,
    pub hung: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ServiceOwner {
    pub pid: u32,
    pub name: Arc<str>,
    pub display: Arc<str>,
}

#[derive(Clone, Debug, Default)]
pub struct RawProcessGpu {
    pub pid: u32,
    pub created: u64,
    pub timestamp: u64,
    /// (engine type, cumulative running time) summed over the process's engines of that type.
    pub engines: Vec<(Arc<str>, u64)>,
}

#[derive(Clone, Debug)]
pub struct ThermalReading {
    pub name: Arc<str>,
    pub celsius: f32,
    pub kind: ThermalKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThermalKind {
    /// A real CPU package/die sensor.
    Cpu,
    /// An ACPI thermal zone reported by firmware. Often a motherboard sensor, not the CPU itself.
    Zone,
}

// ─── Computed values ───────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct CpuNow {
    pub total: Option<f32>,
    pub kernel: Option<f32>,
    pub cores: Vec<Option<f32>>,
    pub speed_mhz: Option<u32>,
    pub temperature_c: Option<f32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MemNow {
    pub total: u64,
    pub available: u64,
    pub commit: u64,
    pub commit_limit: u64,
    /// Standby + modified lists: cached data the OS can drop or write out.
    pub cached: Option<u64>,
    pub standby: Option<u64>,
    pub modified: Option<u64>,
    pub free: Option<u64>,
    pub paged_pool: Option<u64>,
    pub nonpaged_pool: Option<u64>,
    /// Memory held by the compression store.
    pub compressed: Option<u64>,
}

impl MemNow {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }
    pub fn used_pct(&self) -> f32 {
        if self.total == 0 { 0.0 } else { (self.used() as f64 / self.total as f64 * 100.0) as f32 }
    }
    pub fn commit_pct(&self) -> f32 {
        if self.commit_limit == 0 { 0.0 } else { (self.commit as f64 / self.commit_limit as f64 * 100.0) as f32 }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DiskNow {
    pub id: u32,
    pub active: Option<f32>,
    pub read: Option<f64>,
    pub write: Option<f64>,
    pub response_ms: Option<f32>,
    pub queue_depth: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NetNow {
    pub id: u64,
    pub recv: Option<f64>,
    pub send: Option<f64>,
    pub link_bps: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct GpuNow {
    pub id: u64,
    /// Busiest engine, the same summary Windows Task Manager shows.
    pub util: Option<f32>,
    pub engines: Vec<Option<f32>>,
    pub dedicated_used: Option<u64>,
    pub shared_used: Option<u64>,
    pub dedicated_limit: Option<u64>,
    pub shared_limit: Option<u64>,
    pub temperature_c: Option<f32>,
    pub clock_mhz: Option<u32>,
    pub mem_clock_mhz: Option<u32>,
    pub fan_rpm: Option<u32>,
    pub power_pct: Option<f32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProcStatus {
    #[default]
    Running,
    Suspended,
    NotResponding,
}

/// Metadata that is expensive to find and never changes for a process's lifetime.
#[derive(Clone, Debug, Default)]
pub struct ProcMeta {
    pub path: Option<Arc<str>>,
    pub user: Option<Arc<str>>,
    pub description: Option<Arc<str>>,
    pub company: Option<Arc<str>>,
    /// Key into the UI's icon cache.
    pub icon: Option<u32>,
    /// Start time, already formatted in local time.
    pub started: Option<Arc<str>>,
    /// Part of the operating system (lives in the Windows folder, or is a kernel process).
    pub system: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ProcNow {
    pub key: ProcKey,
    pub ppid: u32,
    pub name: Arc<str>,
    pub cpu: Option<f32>,
    pub private_ws: u64,
    pub working_set: u64,
    pub commit: u64,
    pub read: Option<f64>,
    pub write: Option<f64>,
    pub gpu: Option<f32>,
    pub gpu_engine: Option<Arc<str>>,
    pub threads: u32,
    pub handles: u32,
    pub base_priority: i32,
    pub session: u32,
    pub status: ProcStatus,
    pub has_window: bool,
    pub cpu_time: u64,
    pub services: Vec<(Arc<str>, Arc<str>)>,
    pub meta: Option<Arc<ProcMeta>>,
}

impl ProcNow {
    pub fn disk(&self) -> Option<f64> {
        match (self.read, self.write) {
            (None, None) => None,
            (r, w) => Some(r.unwrap_or(0.0) + w.unwrap_or(0.0)),
        }
    }
    pub fn path(&self) -> Option<&str> {
        self.meta.as_ref().and_then(|m| m.path.as_deref())
    }
    pub fn user(&self) -> Option<&str> {
        self.meta.as_ref().and_then(|m| m.user.as_deref())
    }
    pub fn description(&self) -> Option<&str> {
        self.meta.as_ref().and_then(|m| m.description.as_deref())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SysNow {
    pub uptime_secs: Option<u64>,
    pub processes: u32,
    pub threads: u32,
    pub handles: Option<u64>,
}

/// One computed tick, handed to the UI.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub seq: u64,
    pub interval_secs: f32,
    pub cpu: CpuNow,
    pub mem: Option<MemNow>,
    pub disks: Vec<DiskNow>,
    pub nets: Vec<NetNow>,
    pub gpus: Vec<GpuNow>,
    pub sys: SysNow,
    /// Empty and `has_processes == false` when processes weren't sampled this tick.
    pub processes: Arc<Vec<ProcNow>>,
    pub has_processes: bool,
    pub process_io_is_disk: bool,
    pub process_gpu: bool,
    pub thermal: Vec<ThermalReading>,
    pub insights: Vec<crate::insights::Insight>,
}

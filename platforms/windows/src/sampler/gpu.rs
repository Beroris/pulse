//! GPUs, through the same kernel statistics interface (D3DKMT) that Windows Task Manager reads.
//!
//! - Engine use: each node's cumulative running time.
//! - Memory: bytes resident in each segment, split into dedicated and shared (aperture) segments.
//! - Temperature, fan, power and memory clock: the adapter performance data WDDM 2.4+ drivers
//!   report. Drivers that don't report a value leave it at zero, which is shown as unavailable.
//! - Per process: each process's running time per node, for processes this app may query.

use std::collections::HashMap;
use std::sync::Arc;

use tm_core::model::{GpuInfo, RawGpu, RawProcess, RawProcessGpu};
use windows::Wdk::Graphics::Direct3D::*;
use windows::Win32::Foundation::{HANDLE, LUID};
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, KEY_READ};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_INFORMATION};

use crate::util::{Owned, RegKey, from_wide};

struct Adapter {
    luid: LUID,
    handle: u32,
    nodes: u32,
    segments: u32,
    info: GpuInfo,
    /// Per node: the name used for per-process engine totals ("GPU 0 - 3D").
    process_engine: Vec<Arc<str>>,
    /// Nodes that have ever run anything; the rest are skipped per process.
    used_nodes: Vec<bool>,
    three_d_node: Option<u32>,
}

impl Drop for Adapter {
    fn drop(&mut self) {
        if self.handle != 0 {
            unsafe {
                let _ = D3DKMTCloseAdapter(&D3DKMT_CLOSEADAPTER { hAdapter: self.handle });
            }
        }
    }
}

pub struct Gpus {
    adapters: Vec<Adapter>,
    qpc_freq: i64,
    procs: HashMap<(u32, u64), Option<Owned>>,
}

fn luid_id(l: LUID) -> u64 {
    ((l.HighPart as u32 as u64) << 32) | l.LowPart as u64
}

fn vendor_name(id: u32) -> &'static str {
    match id {
        0x10DE => "NVIDIA",
        0x1002 | 0x1022 => "AMD",
        0x8086 => "Intel",
        0x1414 => "Microsoft",
        0x5143 => "Qualcomm",
        0x13B5 => "Arm",
        _ => "",
    }
}

fn engine_type_label(t: i32) -> &'static str {
    match t {
        1 => "3D",
        2 => "Video Decode",
        3 => "Video Encode",
        4 => "Video Processing",
        5 => "Scene Assembly",
        6 => "Copy",
        7 => "Overlay",
        8 => "Crypto",
        _ => "Other",
    }
}

fn query_info<T>(handle: u32, kind: KMTQUERYADAPTERINFOTYPE, data: &mut T) -> bool {
    let mut q = D3DKMT_QUERYADAPTERINFO {
        hAdapter: handle,
        Type: kind,
        pPrivateDriverData: data as *mut T as *mut _,
        PrivateDriverDataSize: std::mem::size_of::<T>() as u32,
    };
    unsafe { D3DKMTQueryAdapterInfo(&mut q) }.is_ok()
}

fn stats(luid: LUID, kind: D3DKMT_QUERYSTATISTICS_TYPE, process: HANDLE, id: u32) -> Option<D3DKMT_QUERYSTATISTICS> {
    let mut q = D3DKMT_QUERYSTATISTICS { Type: kind, AdapterLuid: luid, hProcess: process, ..Default::default() };
    q.Anonymous.QueryNode = D3DKMT_QUERYSTATISTICS_QUERY_NODE { NodeId: id };
    unsafe { D3DKMTQueryStatistics(&q) }.is_ok().then_some(q)
}

/// Driver version and date from the display adapter's device class key.
fn driver_details(vendor: u32, device: u32) -> (Option<String>, Option<String>) {
    const CLASS: &str = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    let Some(class) = RegKey::open(HKEY_LOCAL_MACHINE, CLASS, KEY_READ) else { return (None, None) };
    let want = format!("ven_{vendor:04x}&dev_{device:04x}");
    for sub in class.subkeys() {
        let Some(k) = RegKey::open(HKEY_LOCAL_MACHINE, &format!(r"{CLASS}\{sub}"), KEY_READ) else { continue };
        if k.string("MatchingDeviceId").is_some_and(|m| m.to_lowercase().contains(&want)) {
            return (k.string("DriverVersion"), k.string("DriverDate"));
        }
    }
    (None, None)
}

impl Gpus {
    pub fn new() -> Option<Gpus> {
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
        let mut adapters: Vec<Adapter> = Vec::new();
        let mut i = 0;
        while let Ok(a) = unsafe { factory.EnumAdapters1(i) } {
            i += 1;
            let Ok(desc) = (unsafe { a.GetDesc1() }) else { continue };
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 || adapters.iter().any(|x| luid_id(x.luid) == luid_id(desc.AdapterLuid)) {
                continue;
            }
            let index = adapters.len();
            let mut open = D3DKMT_OPENADAPTERFROMLUID { AdapterLuid: desc.AdapterLuid, hAdapter: 0 };
            let handle = if unsafe { D3DKMTOpenAdapterFromLuid(&mut open) }.is_ok() { open.hAdapter } else { 0 };
            let (nodes, segments) = match stats(desc.AdapterLuid, D3DKMT_QUERYSTATISTICS_ADAPTER, HANDLE::default(), 0) {
                Some(q) => unsafe { (q.QueryResult.AdapterInformation.NodeCount, q.QueryResult.AdapterInformation.NbSegments) },
                None => (0, 0),
            };
            let mut names: Vec<Arc<str>> = Vec::new();
            let mut process_engine = Vec::new();
            let mut three_d_node = None;
            for n in 0..nodes {
                let mut meta = D3DKMT_NODEMETADATA { NodeOrdinalAndAdapterIndex: n, ..Default::default() };
                let (base, ty) = if handle != 0 && query_info(handle, KMTQAITYPE_NODEMETADATA, &mut meta) {
                    let raw_name = unsafe { std::ptr::addr_of!(meta.NodeData.FriendlyName).read_unaligned() };
                    let friendly = from_wide(&raw_name);
                    let ty = meta.NodeData.EngineType.0;
                    (if friendly.trim().is_empty() { engine_type_label(ty).to_string() } else { friendly.trim().to_string() }, ty)
                } else {
                    (format!("Engine {n}"), 0)
                };
                if ty == 1 && three_d_node.is_none() {
                    three_d_node = Some(n);
                }
                process_engine.push(Arc::<str>::from(format!("GPU {index} - {base}")));
                // Several nodes can share a name (copy engines); number the repeats.
                let mut name = base.clone();
                let mut k = 2;
                while names.iter().any(|x| **x == *name) {
                    name = format!("{base} {k}");
                    k += 1;
                }
                names.push(name.into());
            }
            let mut location = None;
            if handle != 0 {
                let mut addr = D3DKMT_ADAPTERADDRESS::default();
                if query_info(handle, KMTQAITYPE_ADAPTERADDRESS, &mut addr) {
                    location = Some(format!("PCI bus {}, device {}, function {}", addr.BusNumber, addr.DeviceNumber, addr.FunctionNumber));
                }
            }
            let (driver_version, driver_date) = driver_details(desc.VendorId, desc.DeviceId);
            let info = GpuInfo {
                id: luid_id(desc.AdapterLuid),
                name: from_wide(&desc.Description).trim().to_string(),
                vendor: vendor_name(desc.VendorId).to_string(),
                dedicated: (desc.DedicatedVideoMemory > 0).then_some(desc.DedicatedVideoMemory as u64),
                shared: (desc.SharedSystemMemory > 0).then_some(desc.SharedSystemMemory as u64),
                driver_version,
                driver_date,
                location,
                engines: names,
            };
            adapters.push(Adapter {
                luid: desc.AdapterLuid,
                handle,
                nodes,
                segments,
                info,
                process_engine,
                used_nodes: vec![true; nodes as usize],
                three_d_node,
            });
        }
        let mut freq = 0i64;
        let _ = unsafe { QueryPerformanceFrequency(&mut freq) };
        Some(Gpus { adapters, qpc_freq: freq.max(1), procs: HashMap::new() })
    }

    pub fn info(&self) -> Vec<GpuInfo> {
        self.adapters.iter().map(|a| a.info.clone()).collect()
    }

    pub fn sample(&mut self, sensors: bool) -> Vec<RawGpu> {
        let mut out = Vec::with_capacity(self.adapters.len());
        for a in &mut self.adapters {
            let ts = c_now(self.qpc_freq);
            let mut g = RawGpu { id: a.info.id, timestamp: ts, ..Default::default() };
            g.engine_time = Vec::with_capacity(a.nodes as usize);
            let mut freq_3d = None;
            for n in 0..a.nodes {
                let r = stats(a.luid, D3DKMT_QUERYSTATISTICS_NODE, HANDLE::default(), n);
                g.engine_time.push(r.map(|q| {
                    let ni = unsafe { q.QueryResult.NodeInformation };
                    if Some(n) == a.three_d_node && ni.NodePerfData.Frequency > 0 {
                        freq_3d = Some((ni.NodePerfData.Frequency / 1_000_000) as u32);
                    }
                    let t = ni.GlobalInformation.RunningTime.max(0) as u64;
                    a.used_nodes[n as usize] = t > 0;
                    t
                }));
            }
            let (mut dedicated, mut shared, mut any) = (0u64, 0u64, false);
            for s in 0..a.segments {
                let Some(q) = stats(a.luid, D3DKMT_QUERYSTATISTICS_SEGMENT, HANDLE::default(), s) else { continue };
                let si = unsafe { q.QueryResult.SegmentInformation };
                any = true;
                if si.Aperture != 0 {
                    shared += si.BytesResident;
                } else {
                    dedicated += si.BytesResident;
                }
            }
            if any {
                g.dedicated_used = a.info.dedicated.map(|_| dedicated);
                g.shared_used = Some(shared);
            }
            g.dedicated_limit = a.info.dedicated;
            g.shared_limit = a.info.shared;
            if sensors && a.handle != 0 {
                if freq_3d.is_none() {
                    if let Some(n) = a.three_d_node {
                        let mut np = D3DKMT_NODE_PERFDATA { NodeOrdinal: n, ..Default::default() };
                        if query_info(a.handle, KMTQAITYPE_NODEPERFDATA, &mut np) && np.Frequency > 0 {
                            freq_3d = Some((np.Frequency / 1_000_000) as u32);
                        }
                    }
                }
                g.clock_mhz = freq_3d;
                let mut p = D3DKMT_ADAPTER_PERFDATA::default();
                if query_info(a.handle, KMTQAITYPE_ADAPTERPERFDATA, &mut p) {
                    // Tenths of a degree; zero means the driver doesn't report it.
                    g.temperature_c = (p.Temperature > 0 && p.Temperature < 1500).then_some(p.Temperature as f32 / 10.0);
                    g.fan_rpm = (p.FanRPM > 0).then_some(p.FanRPM);
                    g.power_pct = (p.Power > 0).then_some(p.Power as f32 / 10.0);
                    g.mem_clock_mhz = (p.MemoryFrequency > 0).then_some((p.MemoryFrequency / 1_000_000) as u32);
                }
            }
            out.push(g);
        }
        out
    }

    /// GPU running time per engine for every process this app may query. Processes it may not
    /// query (other users' processes without admin rights) are left out, so they show as unknown
    /// rather than idle.
    ///
    /// The statistics need `PROCESS_QUERY_INFORMATION`; the limited right is always refused. With
    /// it, a process that has never used an adapter gets `STATUS_INVALID_PARAMETER` for that
    /// adapter's engines, which means no GPU time, not an unknown amount.
    pub fn sample_processes(&mut self, list: &[RawProcess]) -> Vec<RawProcessGpu> {
        const NO_DEVICE: i32 = 0xC000_000Du32 as i32;
        let mut out = Vec::new();
        let mut alive = std::collections::HashSet::with_capacity(list.len());
        for p in list {
            if p.pid <= 4 {
                continue;
            }
            let key = (p.pid, p.created);
            alive.insert(key);
            let h = self.procs.entry(key).or_insert_with(|| unsafe { OpenProcess(PROCESS_QUERY_INFORMATION, false, p.pid) }.ok().map(Owned));
            let Some(h) = h else { continue };
            let ts = c_now(self.qpc_freq);
            let mut engines: Vec<(Arc<str>, u64)> = Vec::new();
            let mut ok = true;
            'adapters: for a in &self.adapters {
                for n in 0..a.nodes {
                    if !a.used_nodes[n as usize] {
                        continue;
                    }
                    let mut q = D3DKMT_QUERYSTATISTICS { Type: D3DKMT_QUERYSTATISTICS_PROCESS_NODE, AdapterLuid: a.luid, hProcess: h.get(), ..Default::default() };
                    q.Anonymous.QueryNode = D3DKMT_QUERYSTATISTICS_QUERY_NODE { NodeId: n };
                    let status = unsafe { D3DKMTQueryStatistics(&q) };
                    if status.0 == NO_DEVICE {
                        continue 'adapters;
                    }
                    if status.is_err() {
                        ok = false;
                        break 'adapters;
                    }
                    let t = unsafe { q.QueryResult.ProcessNodeInformation.RunningTime }.max(0) as u64;
                    let name = &a.process_engine[n as usize];
                    match engines.iter_mut().find(|(e, _)| e == name) {
                        Some(e) => e.1 += t,
                        None => engines.push((name.clone(), t)),
                    }
                }
            }
            if ok {
                out.push(RawProcessGpu { pid: p.pid, created: p.created, timestamp: ts, engines });
            }
        }
        self.procs.retain(|k, _| alive.contains(k));
        out
    }

    /// Releases the cached process handles when per-process GPU isn't shown.
    pub fn forget_processes(&mut self) {
        self.procs.clear();
    }
}

/// Now, in 100 ns units.
fn c_now(freq: i64) -> u64 {
    let mut c = 0i64;
    let _ = unsafe { QueryPerformanceCounter(&mut c) };
    (c as u128 * 10_000_000 / freq.max(1) as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_adapters() {
        let Some(mut g) = Gpus::new() else { return };
        let info = g.info();
        let a = g.sample(true);
        let b = g.sample(true);
        assert_eq!(a.len(), info.len());
        for (x, y) in a.iter().zip(&b) {
            assert!(y.timestamp > x.timestamp);
        }
        for i in &info {
            eprintln!("{} [{}] engines={:?} dedicated={:?} driver={:?} {:?}", i.name, i.vendor, i.engines, i.dedicated, i.driver_version, i.location);
        }
        eprintln!("{:?}", b);
    }
}

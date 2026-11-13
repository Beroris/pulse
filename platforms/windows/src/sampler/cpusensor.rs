//! The CPU's own temperature and power, read through PawnIO (https://pawnio.eu).
//!
//! Windows only reports a CPU temperature when the firmware has an ACPI thermal zone, which most
//! desktop boards don't. The sensor itself sits behind registers that only a kernel driver can
//! read. PawnIO is a signed driver that runs small signed modules, each allowing a fixed list of
//! reads. Hardware tools such as LibreHardwareMonitor and OpenRGB install it, and so can the user.
//! This app never installs it.
//!
//! PawnIO's device is open to administrators only, so the sensors are read when the app runs as
//! administrator. The modules are the official signed builds, built into the exe (see
//! `assets/pawnio`). Only read functions are ever called.

use std::time::{Duration, Instant};

use tm_core::model::SensorAccess;
use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, FreeLibrary, HANDLE, HMODULE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, KEY_READ};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::{HRESULT, PCSTR, PCWSTR, s};

use crate::util::{RegKey, wide};

static AMD_MODULE: &[u8] = include_bytes!("../../../../assets/pawnio/AMDFamily17.bin");
static INTEL_MODULE: &[u8] = include_bytes!("../../../../assets/pawnio/IntelMSR.bin");

type OpenFn = unsafe extern "system" fn(*mut HANDLE) -> HRESULT;
type LoadFn = unsafe extern "system" fn(HANDLE, *const u8, usize) -> HRESULT;
type ExecFn = unsafe extern "system" fn(HANDLE, PCSTR, *const u64, usize, *mut u64, usize, *mut usize) -> HRESULT;
type CloseFn = unsafe extern "system" fn(HANDLE) -> HRESULT;

/// SMN address of THM_TCON_CUR_TMP, the reported temperature control value (Tctl) on Zen.
const AMD_TCTL: u32 = 0x0005_9800;
const AMD_PWR_UNIT: u32 = 0xC001_0299;
const AMD_PKG_ENERGY: u32 = 0xC001_029B;
const INTEL_THERM_STATUS: u32 = 0x19C;
const INTEL_TEMPERATURE_TARGET: u32 = 0x1A2;
const INTEL_PKG_THERM_STATUS: u32 = 0x1B1;
const INTEL_RAPL_POWER_UNIT: u32 = 0x606;
const INTEL_PKG_ENERGY: u32 = 0x611;

/// After "nothing installed", look again this often, in case it gets installed.
const RETRY: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CpuReading {
    pub celsius: Option<f32>,
    pub watts: Option<f32>,
    pub access: SensorAccess,
}

pub struct CpuSensors {
    pawn: Option<Pawn>,
    /// Why there's no reader, and when that was decided.
    off: Option<(SensorAccess, Instant)>,
}

impl Default for CpuSensors {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuSensors {
    pub fn new() -> CpuSensors {
        CpuSensors { pawn: None, off: None }
    }

    pub fn read(&mut self) -> CpuReading {
        if self.pawn.is_none() {
            let retry = match self.off {
                None => true,
                Some((SensorAccess::NoDriver, at)) => at.elapsed() >= RETRY,
                // Elevation can't change while the app runs, and the CPU never does.
                Some(_) => false,
            };
            if retry {
                match Pawn::open() {
                    Ok(p) => {
                        self.pawn = Some(p);
                        self.off = None;
                    }
                    Err(a) => self.off = Some((a, Instant::now())),
                }
            }
        }
        match self.pawn.as_mut() {
            Some(p) => {
                let celsius = p.temperature();
                let watts = p.power();
                CpuReading { celsius, watts, access: SensorAccess::Ok }
            }
            None => CpuReading { access: self.off.map_or(SensorAccess::Unknown, |o| o.0), ..Default::default() },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    /// `offset`: how far Tctl reads above the die temperature on a few early Ryzen models.
    Amd { offset: f32 },
    /// `tjmax`: the temperature the readout counts down from. `package`: the whole-package sensor
    /// exists; otherwise the core the call runs on is read.
    Intel { tjmax: f32, package: bool },
}

struct Pawn {
    lib: HMODULE,
    handle: HANDLE,
    exec: ExecFn,
    close: CloseFn,
    kind: Kind,
    /// Shared with other hardware tools (HWiNFO, LibreHardwareMonitor, OpenRGB) so that two of them
    /// never interleave the index and data writes of a PCI configuration read.
    pci: Option<HANDLE>,
    /// Joules per count of the package energy counter.
    energy_unit: Option<f64>,
    last_energy: Option<(u32, Instant)>,
    last_celsius: Option<f32>,
}

// The driver handle and module may be used from any thread; only the collector thread does.
unsafe impl Send for Pawn {}

impl Pawn {
    fn open() -> Result<Pawn, SensorAccess> {
        let cpu = cpu_id();
        let module = match cpu.vendor {
            Vendor::Amd if (0x17..=0x1A).contains(&cpu.family) => AMD_MODULE,
            Vendor::Intel => INTEL_MODULE,
            _ => return Err(SensorAccess::Unsupported),
        };
        let dll = dll_path().ok_or(SensorAccess::NoDriver)?;
        // Don't load it just to be told no: its device only opens for administrators.
        if !crate::util::is_elevated() {
            return Err(SensorAccess::NeedsAdmin);
        }
        let w = wide(&dll);
        let lib = unsafe { LoadLibraryExW(PCWSTR(w.as_ptr()), None, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32) }.map_err(|_| SensorAccess::NoDriver)?;
        let free = |e| {
            unsafe {
                let _ = FreeLibrary(lib);
            }
            e
        };
        let procs = unsafe {
            (
                GetProcAddress(lib, s!("pawnio_open")),
                GetProcAddress(lib, s!("pawnio_load")),
                GetProcAddress(lib, s!("pawnio_execute")),
                GetProcAddress(lib, s!("pawnio_close")),
            )
        };
        let (Some(open), Some(load), Some(exec), Some(close)) = procs else {
            return Err(free(SensorAccess::NoDriver));
        };
        let (open, load, exec, close): (OpenFn, LoadFn, ExecFn, CloseFn) = unsafe {
            (std::mem::transmute(open), std::mem::transmute(load), std::mem::transmute(exec), std::mem::transmute(close))
        };
        let mut handle = HANDLE::default();
        let hr = unsafe { open(&mut handle) };
        if hr.is_err() {
            return Err(free(if hr == E_ACCESSDENIED { SensorAccess::NeedsAdmin } else { SensorAccess::NoDriver }));
        }
        // The module checks the CPU itself and refuses one it doesn't know.
        if unsafe { load(handle, module.as_ptr(), module.len()) }.is_err() {
            unsafe {
                let _ = close(handle);
            }
            return Err(free(SensorAccess::Unsupported));
        }
        let mut p = Pawn {
            lib,
            handle,
            exec,
            close,
            kind: Kind::Amd { offset: 0.0 },
            pci: None,
            energy_unit: None,
            last_energy: None,
            last_celsius: None,
        };
        match cpu.vendor {
            Vendor::Amd => {
                p.kind = Kind::Amd { offset: tctl_offset(cpu.family, &cpu.brand) };
                let name = wide(r"Global\Access_PCI");
                p.pci = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }.ok();
                p.energy_unit = p.msr(AMD_PWR_UNIT).map(energy_unit);
            }
            _ => {
                let tjmax = p.msr(INTEL_TEMPERATURE_TARGET).map(|v| ((v >> 16) & 0xFF) as f32).filter(|t| *t > 0.0).unwrap_or(100.0);
                p.kind = Kind::Intel { tjmax, package: cpu.package_thermal };
                p.energy_unit = p.msr(INTEL_RAPL_POWER_UNIT).map(energy_unit);
            }
        }
        Ok(p)
    }

    fn call(&self, name: PCSTR, arg: u64) -> Option<u64> {
        let mut out = 0u64;
        let mut written = 0usize;
        let hr = unsafe { (self.exec)(self.handle, name, &arg, 1, &mut out, 1, &mut written) };
        (hr.is_ok() && written >= 1).then_some(out)
    }

    fn msr(&self, msr: u32) -> Option<u64> {
        self.call(s!("ioctl_read_msr"), msr as u64)
    }

    fn smn(&self, addr: u32) -> Option<u32> {
        let locked = match self.pci {
            Some(m) => {
                let r = unsafe { WaitForSingleObject(m, 20) };
                if r != WAIT_OBJECT_0 && r != WAIT_ABANDONED {
                    return None;
                }
                Some(m)
            }
            None => None,
        };
        let v = self.call(s!("ioctl_read_smn"), addr as u64);
        if let Some(m) = locked {
            unsafe {
                let _ = ReleaseMutex(m);
            }
        }
        v.map(|v| v as u32)
    }

    fn temperature(&mut self) -> Option<f32> {
        let t = match self.kind {
            Kind::Amd { offset } => match self.smn(AMD_TCTL) {
                Some(reg) => Some(amd_tctl(reg) - offset),
                // Another tool held the PCI lock for a moment: keep the last reading.
                None => return self.last_celsius,
            },
            Kind::Intel { tjmax, package } => {
                let msr = if package { INTEL_PKG_THERM_STATUS } else { INTEL_THERM_STATUS };
                self.msr(msr).and_then(|v| intel_temp(v, tjmax, package))
            }
        };
        let t = t.filter(|c| *c > 0.0 && *c < 125.0);
        self.last_celsius = t;
        t
    }

    fn power(&mut self) -> Option<f32> {
        let unit = self.energy_unit?;
        let msr = match self.kind {
            Kind::Amd { .. } => AMD_PKG_ENERGY,
            Kind::Intel { .. } => INTEL_PKG_ENERGY,
        };
        let now = Instant::now();
        let count = self.msr(msr)? as u32;
        let last = self.last_energy.replace((count, now));
        let (before, at) = last?;
        package_watts(before, count, unit, now.duration_since(at).as_secs_f64())
    }
}

impl Drop for Pawn {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.close)(self.handle);
            if let Some(m) = self.pci {
                let _ = CloseHandle(m);
            }
            let _ = FreeLibrary(self.lib);
        }
    }
}

/// Where PawnIO's user-mode library is, if PawnIO is installed.
fn dll_path() -> Option<String> {
    let dir = RegKey::open(HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\PawnIO", KEY_READ)
        .and_then(|k| k.string("InstallLocation"))
        .filter(|d| !d.trim().is_empty())
        .or_else(|| std::env::var("ProgramW6432").or_else(|_| std::env::var("ProgramFiles")).ok().map(|p| format!(r"{p}\PawnIO")))?;
    let path = format!(r"{}\PawnIOLib.dll", dir.trim_end_matches(['\\', '/']));
    std::path::Path::new(&path).is_file().then_some(path)
}

/// Tctl in °C from THM_TCON_CUR_TMP, as Linux's k10temp decodes it: 11 bits in eighths of a
/// degree, and a 49° lower range when the range-select bit or both Tj-select bits are set.
pub fn amd_tctl(reg: u32) -> f32 {
    let mut c = (reg >> 21) as f32 * 0.125;
    if reg & (1 << 19) != 0 || reg & 0x3_0000 == 0x3_0000 {
        c -= 49.0;
    }
    c
}

/// Early Ryzen models whose Tctl reads above the die temperature (k10temp's table).
fn tctl_offset(family: u32, brand: &str) -> f32 {
    const TABLE: [(&str, f32); 6] = [
        ("AMD Ryzen 5 1600X", 20.0),
        ("AMD Ryzen 7 1700X", 20.0),
        ("AMD Ryzen 7 1800X", 20.0),
        ("AMD Ryzen 7 2700X", 10.0),
        ("AMD Ryzen Threadripper 19", 27.0),
        ("AMD Ryzen Threadripper 29", 27.0),
    ];
    if family != 0x17 {
        return 0.0;
    }
    TABLE.iter().find(|(name, _)| brand.contains(name)).map_or(0.0, |t| t.1)
}

/// An Intel thermal status register: degrees below TjMax in bits 22:16. The per-core register
/// also has a "reading valid" bit; the package one doesn't.
pub fn intel_temp(reg: u64, tjmax: f32, package: bool) -> Option<f32> {
    if !package && reg & (1 << 31) == 0 {
        return None;
    }
    Some(tjmax - ((reg >> 16) & 0x7F) as f32)
}

/// Joules per count, from a power unit register's energy status units (bits 12:8).
fn energy_unit(reg: u64) -> f64 {
    1.0 / (1u64 << ((reg >> 8) & 0x1F)) as f64
}

/// Average power between two readings of a 32-bit energy counter, which wraps.
pub fn package_watts(before: u32, after: u32, unit: f64, secs: f64) -> Option<f32> {
    if secs < 0.05 {
        return None;
    }
    let w = after.wrapping_sub(before) as f64 * unit / secs;
    (w.is_finite() && (0.0..2000.0).contains(&w)).then_some(w as f32)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Vendor {
    Amd,
    Intel,
    Other,
}

struct CpuId {
    vendor: Vendor,
    family: u32,
    brand: String,
    /// Intel's package thermal status register exists (CPUID 6, EAX bit 6).
    package_thermal: bool,
}

#[cfg(target_arch = "x86_64")]
#[allow(unused_unsafe)] // `__cpuid` is safe in newer Rust.
fn cpu_id() -> CpuId {
    use core::arch::x86_64::__cpuid;
    // SAFETY: CPUID is available on every x86-64 processor.
    unsafe {
        let r = __cpuid(0);
        let mut v = Vec::with_capacity(12);
        for x in [r.ebx, r.edx, r.ecx] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        let vendor = match &v[..] {
            b"AuthenticAMD" => Vendor::Amd,
            b"GenuineIntel" => Vendor::Intel,
            _ => Vendor::Other,
        };
        let max = r.eax;
        let eax = __cpuid(1).eax;
        let base = (eax >> 8) & 0xF;
        let family = if base == 0xF { base + ((eax >> 20) & 0xFF) } else { base };
        let package_thermal = max >= 6 && __cpuid(6).eax & (1 << 6) != 0;
        let mut b = Vec::with_capacity(48);
        if __cpuid(0x8000_0000).eax >= 0x8000_0004 {
            for leaf in 0x8000_0002..=0x8000_0004u32 {
                let r = __cpuid(leaf);
                for x in [r.eax, r.ebx, r.ecx, r.edx] {
                    b.extend_from_slice(&x.to_le_bytes());
                }
            }
        }
        let brand = String::from_utf8_lossy(&b).trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_string();
        CpuId { vendor, family, brand, package_thermal }
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn cpu_id() -> CpuId {
    CpuId { vendor: Vendor::Other, family: 0, brand: String::new(), package_thermal: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amd_tctl_is_decoded_like_k10temp() {
        // 45.5 °C: 364 eighths in bits 31:21.
        assert_eq!(amd_tctl(364 << 21), 45.5);
        // The same raw value in the lower range (range-select bit 19).
        assert_eq!(amd_tctl((364 << 21) | (1 << 19)), -3.5);
        // Both Tj-select bits also mean the lower range.
        assert_eq!(amd_tctl((800 << 21) | 0x3_0000), 51.0);
        // One Tj-select bit alone doesn't.
        assert_eq!(amd_tctl((800 << 21) | 0x1_0000), 100.0);
    }

    #[test]
    fn early_ryzen_offsets() {
        assert_eq!(tctl_offset(0x17, "AMD Ryzen 7 1700X Eight-Core Processor"), 20.0);
        assert_eq!(tctl_offset(0x17, "AMD Ryzen Threadripper 2950X 16-Core Processor"), 27.0);
        assert_eq!(tctl_offset(0x19, "AMD Ryzen 5 5600X 6-Core Processor"), 0.0);
        assert_eq!(tctl_offset(0x17, "AMD Ryzen 7 1700 Eight-Core Processor"), 0.0);
    }

    #[test]
    fn intel_readout_counts_down_from_tjmax() {
        let valid = 1u64 << 31;
        assert_eq!(intel_temp(valid | (35 << 16), 100.0, false), Some(65.0));
        assert_eq!(intel_temp(35 << 16, 100.0, false), None, "core reading not marked valid");
        assert_eq!(intel_temp(35 << 16, 105.0, true), Some(70.0), "package register has no valid bit");
    }

    #[test]
    fn energy_counter_wraps() {
        let unit = energy_unit(16 << 8); // 1/65536 J, typical on Zen
        assert_eq!(unit, 1.0 / 65536.0);
        // 65 W for one second.
        let w = package_watts(1_000, 1_000 + 65 * 65536, unit, 1.0).unwrap();
        assert!((w - 65.0).abs() < 0.01, "{w}");
        // Across the wrap.
        let w = package_watts(u32::MAX - 100, 65 * 65536 - 101, unit, 1.0).unwrap();
        assert!((w - 65.0).abs() < 0.01, "{w}");
        assert_eq!(package_watts(0, 10, unit, 0.0), None, "no time passed");
    }

    /// Reads the sensors once and prints what came back. Needs PawnIO and an elevated prompt to
    /// show a temperature; otherwise prints why not:
    /// `cargo test -p tm-windows -- --ignored --nocapture reads_cpu_sensors`
    #[test]
    #[ignore]
    fn reads_cpu_sensors() {
        let mut s = CpuSensors::new();
        let first = s.read();
        std::thread::sleep(Duration::from_millis(500));
        let second = s.read();
        println!("{first:?}\n{second:?}");
    }
}

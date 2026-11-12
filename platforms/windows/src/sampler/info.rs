//! Hardware that doesn't change while running: the processor, memory modules, the OS.

use tm_core::model::{CpuInfo, MemInfo};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, KEY_READ};
use windows::Win32::System::SystemInformation::*;
use windows::Win32::System::Threading::{GetActiveProcessorCount, IsProcessorFeaturePresent, PF_VIRT_FIRMWARE_ENABLED};
use windows::core::PWSTR;

use crate::util::{RegKey, from_wide};

pub fn cpu_info() -> CpuInfo {
    let mut info = CpuInfo::default();
    if let Some(k) = RegKey::open(HKEY_LOCAL_MACHINE, r"HARDWARE\DESCRIPTION\System\CentralProcessor\0", KEY_READ) {
        info.name = k.string("ProcessorNameString").map(|s| s.split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default();
        info.vendor = k.string("VendorIdentifier").unwrap_or_default();
        info.base_mhz = k.dword("~MHz").filter(|m| *m > 0);
    }
    // 0xFFFF: every processor group.
    info.logical = unsafe { GetActiveProcessorCount(0xFFFF) };

    let mut len = 0u32;
    let _ = unsafe { GetLogicalProcessorInformationEx(RelationAll, None, &mut len) };
    if len > 0 {
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        if unsafe { GetLogicalProcessorInformationEx(RelationAll, Some(buf.as_mut_ptr() as *mut _), &mut len) }.is_ok() {
            let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, len as usize) };
            let (mut l1, mut l2, mut l3) = (0u64, 0u64, 0u64);
            let mut off = 0usize;
            while off + 8 <= bytes.len() {
                let relation = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
                let size = u32::from_le_bytes(bytes[off + 4..off + 8].try_into().unwrap()) as usize;
                if size == 0 || off + size > bytes.len() {
                    break;
                }
                match relation {
                    0 => info.cores += 1,   // RelationProcessorCore
                    3 => info.sockets += 1, // RelationProcessorPackage
                    2 if size >= 20 => {
                        // CACHE_RELATIONSHIP: Level u8, Associativity u8, LineSize u16, CacheSize u32, Type.
                        let level = bytes[off + 8];
                        let cache = u32::from_le_bytes(bytes[off + 12..off + 16].try_into().unwrap()) as u64;
                        match level {
                            1 => l1 += cache,
                            2 => l2 += cache,
                            3 => l3 += cache,
                            _ => {}
                        }
                    }
                    _ => {}
                }
                off += size;
            }
            info.l1_bytes = (l1 > 0).then_some(l1);
            info.l2_bytes = (l2 > 0).then_some(l2);
            info.l3_bytes = (l3 > 0).then_some(l3);
        }
    }
    info.virtualization = Some(unsafe { IsProcessorFeaturePresent(PF_VIRT_FIRMWARE_ENABLED) }.as_bool());
    info
}

pub fn mem_info() -> MemInfo {
    let mut info = MemInfo::default();
    let mut kb = 0u64;
    if unsafe { GetPhysicallyInstalledSystemMemory(&mut kb) }.is_ok() && kb > 0 {
        info.installed = Some(kb * 1024);
    }
    let size = unsafe { GetSystemFirmwareTable(RSMB, 0, None) };
    if size == 0 {
        return info;
    }
    let mut buf = vec![0u8; size as usize];
    if unsafe { GetSystemFirmwareTable(RSMB, 0, Some(&mut buf)) } != size || buf.len() < 8 {
        return info;
    }
    let modules = smbios_memory(&buf[8..]);
    if !modules.is_empty() {
        info.slots_total = Some(modules.len() as u32);
        let used: Vec<&Module> = modules.iter().filter(|m| m.size > 0).collect();
        info.slots_used = Some(used.len() as u32);
        info.speed_mts = used.iter().filter_map(|m| m.speed).max();
        info.form_factor = used.iter().find_map(|m| m.form_factor.map(String::from));
        info.kind = used.iter().find_map(|m| m.kind.map(String::from));
    }
    info
}

struct Module {
    size: u64,
    speed: Option<u32>,
    form_factor: Option<&'static str>,
    kind: Option<&'static str>,
}

/// Memory Device (type 17) structures from a raw SMBIOS table.
fn smbios_memory(table: &[u8]) -> Vec<Module> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 4 <= table.len() {
        let kind = table[off];
        let len = table[off + 1] as usize;
        if len < 4 || off + len > table.len() {
            break;
        }
        let s = &table[off..off + len];
        if kind == 17 && len >= 0x15 {
            let word = |o: usize| (o + 2 <= s.len()).then(|| u16::from_le_bytes([s[o], s[o + 1]]));
            let raw = word(0x0C).unwrap_or(0);
            let size = match raw {
                0 | 0xFFFF => 0,
                0x7FFF if len >= 0x20 => u32::from_le_bytes([s[0x1C], s[0x1D], s[0x1E], s[0x1F]]) as u64 & 0x7FFF_FFFF,
                v if v & 0x8000 != 0 => (v & 0x7FFF) as u64 / 1024,
                v => v as u64,
            } * 1024
                * 1024;
            let configured = if len >= 0x22 { word(0x20).filter(|v| *v != 0 && *v != 0xFFFF) } else { None };
            let speed = configured.or(word(0x15).filter(|v| *v != 0 && *v != 0xFFFF)).map(|v| v as u32);
            out.push(Module {
                size,
                speed,
                form_factor: match s[0x0E] {
                    0x09 => Some("DIMM"),
                    0x0B => Some("Soldered"),
                    0x0D => Some("SODIMM"),
                    0x0F => Some("FB-DIMM"),
                    0x10 => Some("Die"),
                    _ => None,
                },
                kind: match s[0x12] {
                    0x12 => Some("DDR"),
                    0x13 => Some("DDR2"),
                    0x18 => Some("DDR3"),
                    0x1A => Some("DDR4"),
                    0x1B => Some("LPDDR"),
                    0x1C => Some("LPDDR2"),
                    0x1D => Some("LPDDR3"),
                    0x1E => Some("LPDDR4"),
                    0x22 => Some("DDR5"),
                    0x23 => Some("LPDDR5"),
                    _ => None,
                },
            });
        }
        if kind == 127 {
            break; // end of table
        }
        // Skip the formatted area, then the string set, which ends with two nulls.
        let mut p = off + len;
        while p + 1 < table.len() && !(table[p] == 0 && table[p + 1] == 0) {
            p += 1;
        }
        off = p + 2;
    }
    out
}

pub fn os_name() -> String {
    let Some(k) = RegKey::open(HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", KEY_READ) else {
        return "Windows".into();
    };
    let build: u32 = k.string("CurrentBuildNumber").and_then(|b| b.parse().ok()).unwrap_or(0);
    let mut name = k.string("ProductName").unwrap_or_else(|| "Windows".into());
    // Windows 11 still says "Windows 10" here.
    if build >= 22000 {
        name = name.replace("Windows 10", "Windows 11");
    }
    if let Some(v) = k.string("DisplayVersion") {
        name = format!("{name} {v}");
    }
    if build > 0 {
        match k.dword("UBR") {
            Some(ubr) => name = format!("{name} (build {build}.{ubr})"),
            None => name = format!("{name} (build {build})"),
        }
    }
    name
}

pub fn host_name() -> String {
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    if unsafe { GetComputerNameExW(ComputerNameDnsHostname, Some(PWSTR(buf.as_mut_ptr())), &mut len) }.is_ok() {
        return from_wide(&buf[..len as usize]);
    }
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_memory_device() {
        // One type 17 structure (0x28 bytes) with no strings, then the end marker.
        let mut t17 = vec![0u8; 0x28];
        t17[0] = 17;
        t17[1] = 0x28;
        t17[0x0C..0x0E].copy_from_slice(&16384u16.to_le_bytes());
        t17[0x0E] = 0x0D;
        t17[0x12] = 0x22;
        t17[0x15..0x17].copy_from_slice(&5600u16.to_le_bytes());
        t17[0x20..0x22].copy_from_slice(&5200u16.to_le_bytes());
        let mut table = t17;
        table.extend_from_slice(&[0, 0, 127, 4, 0, 0, 0, 0]);
        let m = smbios_memory(&table);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].size, 16 * 1024 * 1024 * 1024);
        assert_eq!(m[0].speed, Some(5200));
        assert_eq!(m[0].form_factor, Some("SODIMM"));
        assert_eq!(m[0].kind, Some("DDR5"));
    }

    #[test]
    fn describes_this_machine() {
        let c = cpu_info();
        assert!(c.cores > 0 && c.logical >= c.cores && c.sockets > 0, "{c:?}");
        let m = mem_info();
        eprintln!("{c:?}\n{m:?}\n{}\n{}", os_name(), host_name());
        assert!(os_name().starts_with("Windows"));
    }
}

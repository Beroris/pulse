//! Physical disks: what they are (storage property queries) and how busy (`IOCTL_DISK_PERFORMANCE`).
//!
//! Disks are opened with no access rights, which every user may do and which needs no admin. The
//! handle is closed again straight after each query, so a USB drive can always be ejected.

use tm_core::model::{DiskInfo, DiskKind, RawDisk, VolumeInfo};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::*;
use windows::Win32::System::WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE};
use windows::core::PCWSTR;

use crate::util::{Owned, from_wide, wide};

const MAX_DISKS: u32 = 32;

#[derive(Default)]
pub struct Disks {
    ids: Vec<u32>,
}

fn open(path: &str) -> Option<Owned> {
    let p = wide(path);
    let h = unsafe {
        CreateFileW(PCWSTR(p.as_ptr()), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None)
    }
    .ok()?;
    Some(Owned(h))
}

/// Runs an IOCTL into a u64-aligned buffer of `len` bytes. Returns the bytes written.
fn ioctl(h: HANDLE, code: u32, input: Option<&[u8]>, out: &mut [u64]) -> Option<usize> {
    let mut ret = 0u32;
    unsafe {
        DeviceIoControl(
            h,
            code,
            input.map(|i| i.as_ptr() as *const _),
            input.map_or(0, |i| i.len() as u32),
            Some(out.as_mut_ptr() as *mut _),
            (out.len() * 8) as u32,
            Some(&mut ret),
            None,
        )
    }
    .ok()?;
    Some(ret as usize)
}

fn property_query(id: STORAGE_PROPERTY_ID) -> [u8; std::mem::size_of::<STORAGE_PROPERTY_QUERY>()] {
    let q = STORAGE_PROPERTY_QUERY { PropertyId: id, QueryType: PropertyStandardQuery, AdditionalParameters: [0] };
    unsafe { std::mem::transmute(q) }
}

fn ascii_at(buf: &[u8], offset: u32) -> String {
    let o = offset as usize;
    if o == 0 || o >= buf.len() {
        return String::new();
    }
    let end = buf[o..].iter().position(|b| *b == 0).map_or(buf.len(), |e| o + e);
    String::from_utf8_lossy(&buf[o..end]).trim().to_string()
}

fn bus_name(bus: i32) -> Option<&'static str> {
    Some(match bus {
        1 => "SCSI",
        2 => "ATAPI",
        3 => "ATA",
        4 => "IEEE 1394",
        6 => "Fibre Channel",
        7 => "USB",
        8 => "RAID",
        9 => "iSCSI",
        10 => "SAS",
        11 => "SATA",
        12 => "SD",
        13 => "MMC",
        14 | 15 => "Virtual",
        16 => "Storage Spaces",
        17 => "NVMe",
        18 => "SCM",
        19 => "UFS",
        _ => return None,
    })
}

fn describe_disk(id: u32) -> Option<DiskInfo> {
    let h = open(&format!(r"\\.\PhysicalDrive{id}"))?;
    let mut info = DiskInfo { id, ..Default::default() };
    let mut buf = vec![0u64; 128];

    let q = property_query(StorageDeviceProperty);
    if let Some(n) = ioctl(h.get(), IOCTL_STORAGE_QUERY_PROPERTY, Some(&q), &mut buf) {
        let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, n) };
        if n >= std::mem::size_of::<STORAGE_DEVICE_DESCRIPTOR>() {
            let d = unsafe { &*(buf.as_ptr() as *const STORAGE_DEVICE_DESCRIPTOR) };
            let vendor = ascii_at(bytes, d.VendorIdOffset);
            let product = ascii_at(bytes, d.ProductIdOffset);
            info.model = if vendor.is_empty() || product.to_lowercase().starts_with(&vendor.to_lowercase()) {
                product
            } else {
                format!("{vendor} {product}")
            };
            info.bus = bus_name(d.BusType.0).map(String::from);
            info.removable = d.RemovableMedia || d.BusType.0 == 7 || d.BusType.0 == 12;
            if d.BusType.0 == 17 {
                info.kind = DiskKind::Ssd;
            }
        }
    }
    if info.kind == DiskKind::Unknown {
        let q = property_query(StorageDeviceSeekPenaltyProperty);
        if let Some(n) = ioctl(h.get(), IOCTL_STORAGE_QUERY_PROPERTY, Some(&q), &mut buf) {
            if n >= std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() {
                let d = unsafe { &*(buf.as_ptr() as *const DEVICE_SEEK_PENALTY_DESCRIPTOR) };
                info.kind = if d.IncursSeekPenalty { DiskKind::Hdd } else { DiskKind::Ssd };
            }
        }
    }
    if let Some(n) = ioctl(h.get(), IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, None, &mut buf) {
        if n >= 32 {
            let g = unsafe { &*(buf.as_ptr() as *const DISK_GEOMETRY_EX) };
            info.size = (g.DiskSize > 0).then_some(g.DiskSize as u64);
        }
    }
    if info.model.is_empty() {
        info.model = format!("Disk {id}");
    }
    Some(info)
}

/// Fixed and removable volumes with a drive letter, and the disks each spans.
fn volumes() -> Vec<(Vec<u32>, VolumeInfo)> {
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = wide(&format!("{letter}:\\"));
        let kind = unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) };
        if kind != DRIVE_FIXED && kind != DRIVE_REMOVABLE {
            continue;
        }
        let Some(h) = open(&format!(r"\\.\{letter}:")) else { continue };
        let mut buf = vec![0u64; 64];
        let mut disks = Vec::new();
        if ioctl(h.get(), IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, None, &mut buf).is_some() {
            let v = unsafe { &*(buf.as_ptr() as *const VOLUME_DISK_EXTENTS) };
            let extents = unsafe { std::slice::from_raw_parts(v.Extents.as_ptr(), (v.NumberOfDiskExtents as usize).min(16)) };
            disks = extents.iter().map(|e| e.DiskNumber).collect();
            disks.dedup();
        }
        drop(h);
        let mut label = [0u16; 128];
        let mut fs = [0u16; 32];
        let named = unsafe { GetVolumeInformationW(PCWSTR(root.as_ptr()), Some(&mut label), None, None, None, Some(&mut fs)) }.is_ok();
        let (mut total, mut free) = (0u64, 0u64);
        let sized = unsafe { GetDiskFreeSpaceExW(PCWSTR(root.as_ptr()), None, Some(&mut total), Some(&mut free)) }.is_ok();
        out.push((
            disks,
            VolumeInfo {
                mount: format!("{letter}:"),
                label: if named { from_wide(&label) } else { String::new() },
                filesystem: if named { from_wide(&fs) } else { String::new() },
                total: sized.then_some(total),
                free: sized.then_some(free),
            },
        ));
    }
    out
}

impl Disks {
    pub fn describe(&mut self) -> Vec<DiskInfo> {
        let mut disks: Vec<DiskInfo> = (0..MAX_DISKS).filter_map(describe_disk).collect();
        let system_drive = crate::util::windows_dir().get(..2).map(|s| s.to_uppercase()).unwrap_or_default();
        for (ids, vol) in volumes() {
            for id in ids {
                if let Some(d) = disks.iter_mut().find(|d| d.id == id) {
                    if vol.mount.eq_ignore_ascii_case(&system_drive) {
                        d.system = true;
                    }
                    let pf = wide(&format!("{}\\pagefile.sys", vol.mount));
                    if unsafe { GetFileAttributesW(PCWSTR(pf.as_ptr())) } != INVALID_FILE_ATTRIBUTES {
                        d.pagefile = true;
                    }
                    d.volumes.push(vol.clone());
                }
            }
        }
        // Card readers and empty bays report a disk with no medium and no size: leave them out.
        disks.retain(|d| d.size.is_some());
        self.ids = disks.iter().map(|d| d.id).collect();
        disks
    }

    pub fn sample(&mut self) -> Vec<RawDisk> {
        let mut out = Vec::with_capacity(self.ids.len());
        let mut buf = vec![0u64; std::mem::size_of::<DISK_PERFORMANCE>().div_ceil(8)];
        for &id in &self.ids {
            let Some(h) = open(&format!(r"\\.\PhysicalDrive{id}")) else { continue };
            if ioctl(h.get(), IOCTL_DISK_PERFORMANCE, None, &mut buf).is_none() {
                continue;
            }
            let p = unsafe { &*(buf.as_ptr() as *const DISK_PERFORMANCE) };
            out.push(RawDisk {
                id,
                bytes_read: p.BytesRead.max(0) as u64,
                bytes_written: p.BytesWritten.max(0) as u64,
                read_count: p.ReadCount as u64,
                write_count: p.WriteCount as u64,
                read_time: p.ReadTime.max(0) as u64,
                write_time: p.WriteTime.max(0) as u64,
                idle_time: p.IdleTime.max(0) as u64,
                query_time: p.QueryTime.max(0) as u64,
                queue_depth: p.QueueDepth,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_system_disk() {
        let mut d = Disks::default();
        let info = d.describe();
        assert!(info.iter().any(|d| d.system), "{info:#?}");
        let raw = d.sample();
        assert_eq!(raw.len(), info.len());
        assert!(raw.iter().all(|r| r.query_time > 0));
    }
}

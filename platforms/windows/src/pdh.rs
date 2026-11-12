//! A small wrapper over the Performance Data Helper (PDH) API.
//!
//! Used only for the few counters Windows offers no cheaper way to read: processor performance
//! (for the live clock speed), the standby/modified memory lists and pool sizes, and ACPI thermal
//! zones. Everything else comes from direct calls. PDH is not WMI: a collect is a direct call into
//! the counter provider and costs well under a millisecond for these counters.

use windows::Win32::System::Performance::*;
use windows::core::PCWSTR;

use crate::util::{from_pwstr, wide};

const ERROR_SUCCESS: u32 = 0;
const PDH_MORE_DATA: u32 = 0x8000_07D2;
/// Don't clamp percentages at 100 (processor performance goes above it when boosting).
const FORMAT: PDH_FMT = PDH_FMT(PDH_FMT_DOUBLE.0 | 0x0000_8000);

pub struct Query {
    query: PDH_HQUERY,
    counters: Vec<Option<PDH_HCOUNTER>>,
}

unsafe impl Send for Query {}

impl Query {
    pub fn new() -> Option<Query> {
        let mut q = PDH_HQUERY::default();
        let r = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut q) };
        (r == ERROR_SUCCESS).then_some(Query { query: q, counters: Vec::new() })
    }

    /// Adds a counter by its English path. Returns its index even if it doesn't exist on this
    /// machine, in which case reading it gives `None`.
    pub fn add(&mut self, path: &str) -> usize {
        let p = wide(path);
        let mut c = PDH_HCOUNTER::default();
        let r = unsafe { PdhAddEnglishCounterW(self.query, PCWSTR(p.as_ptr()), 0, &mut c) };
        self.counters.push((r == ERROR_SUCCESS).then_some(c));
        self.counters.len() - 1
    }

    pub fn collect(&self) -> bool {
        unsafe { PdhCollectQueryData(self.query) == ERROR_SUCCESS }
    }

    /// The counter's value. `None` if it doesn't exist or has no valid value yet (rate counters
    /// need two collects).
    pub fn value(&self, i: usize) -> Option<f64> {
        let c = (*self.counters.get(i)?)?;
        let mut v = PDH_FMT_COUNTERVALUE::default();
        let r = unsafe { PdhGetFormattedCounterValue(c, FORMAT, None, &mut v) };
        if r != ERROR_SUCCESS || (v.CStatus != 0 && v.CStatus != 1) {
            return None;
        }
        let d = unsafe { v.Anonymous.doubleValue };
        d.is_finite().then_some(d)
    }

    /// Every instance of a wildcard counter as (instance name, value).
    pub fn array(&self, i: usize) -> Vec<(String, f64)> {
        let Some(Some(c)) = self.counters.get(i).copied() else { return Vec::new() };
        unsafe {
            let mut size = 0u32;
            let mut count = 0u32;
            let r = PdhGetFormattedCounterArrayW(c, FORMAT, &mut size, &mut count, None);
            if r != PDH_MORE_DATA || size == 0 {
                return Vec::new();
            }
            // u64 storage keeps the items aligned.
            let mut buf = vec![0u64; (size as usize).div_ceil(8) + 1];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(c, FORMAT, &mut size, &mut count, Some(items)) != ERROR_SUCCESS {
                return Vec::new();
            }
            (0..count as usize)
                .filter_map(|k| {
                    let item = &*items.add(k);
                    if item.FmtValue.CStatus != 0 && item.FmtValue.CStatus != 1 {
                        return None;
                    }
                    let v = item.FmtValue.Anonymous.doubleValue;
                    v.is_finite().then(|| (from_pwstr(item.szName.0), v))
                })
                .collect()
        }
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        unsafe {
            let _ = PdhCloseQuery(self.query);
        }
    }
}

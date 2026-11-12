//! CPU usage maths.
//!
//! Windows (and most kernels) report, per logical processor, cumulative idle, kernel and user time.
//! On Windows the kernel time *includes* idle time. Usage over an interval is
//! `1 - Δidle / Δ(kernel + user)`, which is the "% Processor Time" definition.
//!
//! A process's share is its CPU time delta over the *total* CPU time delta of all logical
//! processors in the same interval. That normalises to the whole machine: one thread spinning on a
//! 12-thread CPU shows as about 8.3 %, the same convention Windows Task Manager uses.

use crate::counter::delta_u64;

/// Cumulative times in 100 ns units. `kernel` includes `idle`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuTimes {
    pub idle: u64,
    pub kernel: u64,
    pub user: u64,
}

impl CpuTimes {
    pub fn total(&self) -> u64 {
        self.kernel.saturating_add(self.user)
    }
}

/// Busy and kernel-only percentages between two readings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuUsage {
    pub busy: f32,
    pub kernel: f32,
}

/// Usage between two readings. `None` across a counter reset or an interval with no time.
pub fn usage(prev: CpuTimes, now: CpuTimes) -> Option<CpuUsage> {
    let d_idle = delta_u64(prev.idle, now.idle)?;
    let d_kernel = delta_u64(prev.kernel, now.kernel)?;
    let d_user = delta_u64(prev.user, now.user)?;
    let d_total = d_kernel.checked_add(d_user)?;
    if d_total == 0 {
        return None;
    }
    let busy = d_total.saturating_sub(d_idle) as f64 / d_total as f64;
    let kernel = d_kernel.saturating_sub(d_idle) as f64 / d_total as f64;
    Some(CpuUsage { busy: (busy * 100.0).clamp(0.0, 100.0) as f32, kernel: (kernel * 100.0).clamp(0.0, 100.0) as f32 })
}

/// A process's share of the whole machine in percent.
///
/// `process_delta` is the process's kernel + user time delta; `system_total_delta` is the summed
/// kernel + user delta of every logical processor over the same interval (both 100 ns units).
pub fn process_share(process_delta: u64, system_total_delta: u64) -> Option<f32> {
    if system_total_delta == 0 {
        return None;
    }
    Some(((process_delta as f64 / system_total_delta as f64) * 100.0).clamp(0.0, 100.0) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(idle: u64, kernel_excl_idle: u64, user: u64) -> CpuTimes {
        CpuTimes { idle, kernel: idle + kernel_excl_idle, user }
    }

    #[test]
    fn half_busy() {
        let a = t(0, 0, 0);
        let b = t(500, 100, 400);
        let u = usage(a, b).unwrap();
        assert!((u.busy - 50.0).abs() < 1e-4);
        assert!((u.kernel - 10.0).abs() < 1e-4);
    }

    #[test]
    fn fully_idle_and_fully_busy() {
        assert_eq!(usage(t(0, 0, 0), t(1000, 0, 0)).unwrap().busy, 0.0);
        assert_eq!(usage(t(0, 0, 0), t(0, 300, 700)).unwrap().busy, 100.0);
    }

    #[test]
    fn no_elapsed_time_is_unknown_not_zero() {
        let a = t(10, 10, 10);
        assert_eq!(usage(a, a), None);
    }

    #[test]
    fn counter_going_backwards_is_unknown() {
        assert_eq!(usage(t(1000, 10, 10), t(5, 10, 10)), None);
    }

    #[test]
    fn process_share_normalises_to_all_cores() {
        // 12 logical CPUs, 1 s interval: 12 s of CPU time in total (100 ns units).
        let system = 12 * 10_000_000;
        // One thread busy for the whole second.
        let one_thread = 10_000_000;
        let share = process_share(one_thread, system).unwrap();
        assert!((share - 100.0 / 12.0).abs() < 1e-3);
    }

    #[test]
    fn process_share_is_clamped() {
        // Sampling jitter can make a process's delta exceed the system delta slightly.
        assert_eq!(process_share(110, 100), Some(100.0));
        assert_eq!(process_share(5, 0), None);
    }
}

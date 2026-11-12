//! Turning cumulative OS counters into deltas and rates.
//!
//! Operating systems report most activity as ever-growing totals (bytes read, 100 ns units of CPU
//! time). A rate is the difference between two readings over the time between them. Two things go
//! wrong in practice and are handled here:
//!
//! - **Resets.** A counter starts again from zero, for example when a network adapter is disabled
//!   and enabled again, or a disk is re-enumerated. The new reading is then smaller than the old one.
//!   That interval has no meaningful rate, so the result is `None`, never a huge negative or wrapped
//!   number.
//! - **Wrap-around.** Counters kept in 32 bits roll over at 2^32. A small forward step across the
//!   boundary is a real delta; a step that would be implausibly large is treated as a reset.

/// Difference between two readings of a 64-bit monotonic counter, or `None` after a reset.
pub fn delta_u64(prev: u64, now: u64) -> Option<u64> {
    now.checked_sub(prev)
}

/// Difference between two readings of a counter that wraps at 2^32.
///
/// `max_plausible` is the largest delta that could really happen in one interval. A wrapped
/// difference above it means the counter was reset rather than wrapped, and gives `None`.
pub fn delta_u32_wrapping(prev: u32, now: u32, max_plausible: u32) -> Option<u32> {
    let d = now.wrapping_sub(prev);
    (d <= max_plausible).then_some(d)
}

/// Per-second rate from a delta over `seconds`. `None` for intervals too short to be meaningful.
pub fn rate(delta: u64, seconds: f64) -> Option<f64> {
    (seconds >= 0.001).then(|| delta as f64 / seconds)
}

/// Rate between two 64-bit readings, `None` across a reset or a too-short interval.
pub fn rate_u64(prev: u64, now: u64, seconds: f64) -> Option<f64> {
    delta_u64(prev, now).and_then(|d| rate(d, seconds))
}

/// A busy fraction from a busy-time delta over an elapsed-time delta, clamped to 0..=100 %.
///
/// Both deltas use the same unit. Sampling jitter can make busy time slightly exceed elapsed time;
/// the result is clamped rather than shown as 103 %.
pub fn busy_percent(busy: u64, elapsed: u64) -> Option<f32> {
    if elapsed == 0 {
        return None;
    }
    Some(((busy as f64 / elapsed as f64) * 100.0).clamp(0.0, 100.0) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_delta_and_rate() {
        assert_eq!(delta_u64(100, 350), Some(250));
        assert_eq!(rate_u64(1_000, 3_000, 2.0), Some(1_000.0));
    }

    #[test]
    fn reset_gives_no_rate_instead_of_a_spike() {
        // A network adapter re-enabled: its byte counter starts again near zero.
        assert_eq!(delta_u64(9_000_000, 1_200), None);
        assert_eq!(rate_u64(9_000_000, 1_200, 1.0), None);
    }

    #[test]
    fn wrap_around_of_a_32_bit_counter() {
        let prev = u32::MAX - 10;
        let now = 20;
        assert_eq!(delta_u32_wrapping(prev, now, 1_000), Some(31));
    }

    #[test]
    fn implausible_wrap_is_treated_as_reset() {
        // Going from 5 000 to 100 would be a wrap of almost 2^32: really a reset.
        assert_eq!(delta_u32_wrapping(5_000, 100, 1_000_000), None);
    }

    #[test]
    fn too_short_interval_has_no_rate() {
        assert_eq!(rate(500, 0.0), None);
        assert_eq!(rate(500, 0.0005), None);
    }

    #[test]
    fn busy_percent_is_clamped() {
        assert_eq!(busy_percent(50, 100), Some(50.0));
        assert_eq!(busy_percent(103, 100), Some(100.0));
        assert_eq!(busy_percent(0, 0), None);
    }
}

//! SVG path data for history graphs.
//!
//! A graph is drawn by the UI with a `Path` stretched over its area. The path coordinates are in a
//! view box `width` units wide and 100 tall, newest value at the right edge. Unknown values leave a
//! gap. Curves use monotone cubic interpolation, so a smoothed line never overshoots past 0 % or
//! 100 % the way a plain spline would.

use std::fmt::Write;

use crate::history::History;

/// Path data for one series.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphPath {
    pub line: String,
    pub fill: String,
    /// The view box width the paths were made for.
    pub width: f32,
}

/// Builds the line and area for `history` scaled so `max` reaches the top. Long histories are
/// reduced to at most `max_points` buckets, each keeping its peak so short spikes stay visible.
pub fn build(history: &History, max: f32, max_points: usize, smooth: bool) -> GraphPath {
    let cap = history.capacity();
    let bucket = cap.div_ceil(max_points.max(2)).max(1);
    let slots = cap.div_ceil(bucket);
    // Values right-aligned in `cap` slots, then reduced into `slots` buckets.
    let mut reduced: Vec<f32> = vec![f32::NAN; slots];
    let offset = cap - history.len();
    for (i, v) in history.iter().enumerate() {
        if v.is_nan() {
            continue;
        }
        let pos = offset + i;
        // Align buckets to the right edge so the newest value always sits in the last bucket.
        let from_right = cap - 1 - pos;
        let b = slots - 1 - from_right / bucket;
        let slot = &mut reduced[b];
        *slot = if slot.is_nan() { v } else { slot.max(v) };
    }
    let width = (slots - 1).max(1) as f32;
    let max = if max > 0.0 { max } else { 1.0 };
    let points: Vec<Option<(f32, f32)>> = reduced
        .iter()
        .enumerate()
        .map(|(i, v)| (!v.is_nan()).then(|| (i as f32, 100.0 - (v / max).clamp(0.0, 1.0) * 100.0)))
        .collect();
    let (line, fill) = paths(&points, smooth);
    GraphPath { line, fill, width }
}

fn paths(points: &[Option<(f32, f32)>], smooth: bool) -> (String, String) {
    let mut line = String::with_capacity(points.len() * 14);
    let mut fill = String::with_capacity(points.len() * 14 + 64);
    let mut run: Vec<(f32, f32)> = Vec::with_capacity(points.len());
    let flush = |run: &mut Vec<(f32, f32)>, line: &mut String, fill: &mut String| {
        if run.is_empty() {
            return;
        }
        if run.len() == 1 {
            // A lone point: draw a short flat dash so it is visible.
            let (x, y) = run[0];
            let _ = write!(line, "M{} {:.1}L{} {:.1}", fmt_x(x - 0.4), y, fmt_x(x + 0.4), y);
            let _ = write!(fill, "M{} 100L{} {:.1}L{} {:.1}L{} 100Z", fmt_x(x - 0.4), fmt_x(x - 0.4), y, fmt_x(x + 0.4), y, fmt_x(x + 0.4));
            run.clear();
            return;
        }
        let mut seg = String::with_capacity(run.len() * 14);
        segments(run, smooth, &mut seg);
        let (x0, y0) = run[0];
        let (xn, _) = run[run.len() - 1];
        let _ = write!(line, "M{} {:.1}{}", fmt_x(x0), y0, seg);
        let _ = write!(fill, "M{} 100L{} {:.1}{}L{} 100Z", fmt_x(x0), fmt_x(x0), y0, seg, fmt_x(xn));
        run.clear();
    };
    for p in points {
        match p {
            Some(pt) => run.push(*pt),
            None => flush(&mut run, &mut line, &mut fill),
        }
    }
    flush(&mut run, &mut line, &mut fill);
    (line, fill)
}

fn fmt_x(x: f32) -> String {
    if (x - x.round()).abs() < 1e-3 { format!("{}", x.round() as i64) } else { format!("{x:.2}") }
}

/// Path segments from the first point of `run` to its last.
fn segments(run: &[(f32, f32)], smooth: bool, out: &mut String) {
    if !smooth || run.len() < 3 {
        for &(x, y) in &run[1..] {
            let _ = write!(out, "L{} {:.1}", fmt_x(x), y);
        }
        return;
    }
    // Fritsch–Carlson monotone tangents.
    let n = run.len();
    let mut d = vec![0.0f32; n - 1];
    for i in 0..n - 1 {
        let dx = run[i + 1].0 - run[i].0;
        d[i] = if dx.abs() < f32::EPSILON { 0.0 } else { (run[i + 1].1 - run[i].1) / dx };
    }
    let mut m = vec![0.0f32; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        m[i] = if d[i - 1] * d[i] <= 0.0 { 0.0 } else { (d[i - 1] + d[i]) / 2.0 };
    }
    for i in 0..n - 1 {
        if d[i] == 0.0 {
            m[i] = 0.0;
            m[i + 1] = 0.0;
            continue;
        }
        let a = m[i] / d[i];
        let b = m[i + 1] / d[i];
        let s = a * a + b * b;
        if s > 9.0 {
            let t = 3.0 / s.sqrt();
            m[i] = t * a * d[i];
            m[i + 1] = t * b * d[i];
        }
    }
    for i in 0..n - 1 {
        let (x0, y0) = run[i];
        let (x1, y1) = run[i + 1];
        let h = (x1 - x0) / 3.0;
        let c1y = (y0 + m[i] * h).clamp(0.0, 100.0);
        let c2y = (y1 - m[i + 1] * h).clamp(0.0, 100.0);
        let _ = write!(out, "C{:.2} {:.1} {:.2} {:.1} {} {:.1}", x0 + h, c1y, x1 - h, c2y, fmt_x(x1), y1);
    }
}

/// A round ceiling for an auto-scaled graph: the next step of 1, 2, 5 × 10ⁿ at or above `v`,
/// never below `floor`.
pub fn nice_ceiling(v: f64, floor: f64) -> f64 {
    let v = v.max(floor).max(f64::MIN_POSITIVE);
    let exp = v.log10().floor();
    let base = 10f64.powf(exp);
    for step in [1.0, 2.0, 5.0, 10.0] {
        let c = step * base;
        if c >= v * 0.999_999 {
            return c;
        }
    }
    10.0 * base
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hist(values: &[f32], cap: usize) -> History {
        let mut h = History::new(cap);
        for v in values {
            h.push(*v);
        }
        h
    }

    #[test]
    fn newest_value_is_at_the_right_edge() {
        let h = hist(&[0.0, 50.0, 100.0], 5);
        let g = build(&h, 100.0, 100, false);
        assert_eq!(g.width, 4.0);
        assert_eq!(g.line, "M2 100.0L3 50.0L4 0.0");
        assert!(g.fill.starts_with("M2 100L2 100.0"));
        assert!(g.fill.ends_with("L4 100Z"));
    }

    #[test]
    fn unknown_values_leave_a_gap() {
        let h = hist(&[10.0, 20.0, f32::NAN, 30.0, 40.0], 5);
        let g = build(&h, 100.0, 100, false);
        assert_eq!(g.line.matches('M').count(), 2, "{}", g.line);
    }

    #[test]
    fn values_above_max_are_clamped() {
        let h = hist(&[250.0, 250.0], 2);
        let g = build(&h, 100.0, 100, false);
        assert_eq!(g.line, "M0 0.0L1 0.0");
    }

    #[test]
    fn long_histories_keep_their_peaks() {
        let mut values = vec![0.0f32; 600];
        values[301] = 90.0;
        let h = hist(&values, 600);
        let g = build(&h, 100.0, 100, false);
        assert!(g.width <= 100.0);
        assert!(g.line.contains(" 10.0"), "the 90 % spike survives reduction: {}", g.line);
    }

    #[test]
    fn smoothing_never_overshoots() {
        let h = hist(&[0.0, 100.0, 100.0, 0.0, 100.0], 5);
        let g = build(&h, 100.0, 100, true);
        for token in g.line.split(|c: char| c.is_ascii_alphabetic() || c == ' ').filter(|t| !t.is_empty()) {
            let v: f32 = token.parse().unwrap();
            assert!((-0.01..=100.01).contains(&v), "{v} in {}", g.line);
        }
    }

    #[test]
    fn empty_history_draws_nothing() {
        let g = build(&History::new(10), 100.0, 100, true);
        assert!(g.line.is_empty() && g.fill.is_empty());
    }

    #[test]
    fn nice_ceilings() {
        assert_eq!(nice_ceiling(0.0, 100.0), 100.0);
        assert_eq!(nice_ceiling(130.0, 1.0), 200.0);
        assert_eq!(nice_ceiling(4.2e6, 1.0), 5e6);
        assert_eq!(nice_ceiling(5e6, 1.0), 5e6);
        assert_eq!(nice_ceiling(7e6, 1.0), 1e7);
    }
}

//! Conversions between tm-core data and what the Slint views take.

use std::rc::Rc;

use slint::{Color, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use tm_core::history::History;
use tm_core::platform::IconPixels;

use crate::{GraphData, Rgb, StatData};

/// Graph paths for `h`, scaled so `max` reaches the top.
pub fn graph(h: &History, max: f32, points: usize) -> GraphData {
    let g = tm_core::graph::build(h, max, points, true);
    if g.line.is_empty() {
        return GraphData::default();
    }
    GraphData { line: g.line.into(), fill: g.fill.into(), width: g.width }
}

/// Combines histories of the same length sample by sample, newest values aligned.
pub fn combine(hs: &[&History], f: fn(f32, f32) -> f32) -> History {
    let cap = hs.iter().map(|h| h.capacity()).max().unwrap_or(2);
    let len = hs.iter().map(|h| h.len()).max().unwrap_or(0);
    let mut out = History::new(cap);
    let series: Vec<Vec<f32>> = hs.iter().map(|h| h.iter().collect()).collect();
    for i in 0..len {
        let mut acc = f32::NAN;
        for s in &series {
            // Right-align: index i of the longest series.
            let offset = len - s.len();
            if i < offset {
                continue;
            }
            let v = s[i - offset];
            if v.is_nan() {
                continue;
            }
            acc = if acc.is_nan() { v } else { f(acc, v) };
        }
        out.push(acc);
    }
    out
}

pub fn image(px: &IconPixels) -> Option<Image> {
    if px.width == 0 || px.height == 0 || px.rgba.len() != (px.width * px.height * 4) as usize {
        return None;
    }
    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&px.rgba, px.width, px.height);
    Some(Image::from_rgba8(buf))
}

pub fn color(c: Rgb) -> Color {
    Color::from_rgb_u8(c.0, c.1, c.2)
}

fn linear(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

pub fn luminance(c: Rgb) -> f64 {
    0.2126 * linear(c.0) + 0.7152 * linear(c.1) + 0.0722 * linear(c.2)
}

/// WCAG contrast ratio between two colours.
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let m = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round().clamp(0.0, 255.0) as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// The accent moved toward white (dark theme) or black (light theme) until it stands out from the
/// background enough to be read as text and seen as a line.
pub fn readable_accent(accent: Rgb, background: Rgb, dark: bool) -> Rgb {
    let target = if dark { (255, 255, 255) } else { (0, 0, 0) };
    let mut c = accent;
    for _ in 0..24 {
        if contrast(c, background) >= 3.6 {
            break;
        }
        c = mix(c, target, 0.08);
    }
    c
}

/// Black or white, whichever reads better on the accent.
pub fn on_accent(accent: Rgb) -> Rgb {
    if contrast(accent, (0, 0, 0)) >= contrast(accent, (255, 255, 255)) { (0, 0, 0) } else { (255, 255, 255) }
}

pub fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

pub fn stat(label: &str, value: impl Into<SharedString>) -> StatData {
    StatData { label: label.into(), value: value.into(), tip: SharedString::new() }
}

pub fn stat_tip(label: &str, value: impl Into<SharedString>, tip: &str) -> StatData {
    StatData { label: label.into(), value: value.into(), tip: tip.into() }
}

/// Updates a model in place: rows that changed are replaced, so the views keep their state and
/// nothing is rebuilt when the count stays the same.
pub fn sync<T: Clone + PartialEq + 'static>(model: &Rc<VecModel<T>>, items: Vec<T>) {
    if model.row_count() != items.len() {
        model.set_vec(items);
        return;
    }
    for (i, item) in items.into_iter().enumerate() {
        if model.row_data(i).as_ref() != Some(&item) {
            model.set_row_data(i, item);
        }
    }
}

pub fn model<T: Clone + 'static>(m: &Rc<VecModel<T>>) -> ModelRc<T> {
    ModelRc::from(m.clone())
}

pub fn strings(items: &[&str]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(items.iter().map(|s| SharedString::from(*s)).collect::<Vec<_>>()))
}

/// Joins names for a sentence: "a", "a and b", "a, b and 3 others".
pub fn names(list: &[String]) -> String {
    match list.len() {
        0 => String::new(),
        1 => list[0].clone(),
        2 => format!("{} and {}", list[0], list[1]),
        3 => format!("{}, {} and {}", list[0], list[1], list[2]),
        n => format!("{}, {} and {} others", list[0], list[1], n - 2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combine_aligns_newest_values() {
        let mut a = History::new(4);
        let mut b = History::new(4);
        for v in [1.0, 2.0, 3.0] {
            a.push(v);
        }
        b.push(10.0);
        b.push_opt(None);
        let s = combine(&[&a, &b], |x, y| x + y);
        assert_eq!(s.iter().collect::<Vec<_>>(), vec![1.0, 12.0, 3.0]);
    }

    #[test]
    fn accents_are_made_readable() {
        let bg = (0x13, 0x13, 0x15);
        let dim = (0x20, 0x30, 0x80);
        let out = readable_accent(dim, bg, true);
        assert!(contrast(out, bg) >= 3.6, "{out:?}");
        // A colour that already stands out is left alone.
        assert_eq!(readable_accent((0x60, 0xcd, 0xff), bg, true), (0x60, 0xcd, 0xff));
        assert_eq!(on_accent((0x60, 0xcd, 0xff)), (0, 0, 0));
        assert_eq!(on_accent((0x00, 0x5f, 0xb8)), (255, 255, 255));
    }

    #[test]
    fn name_lists() {
        let v: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        assert_eq!(names(&v[..1]), "a");
        assert_eq!(names(&v[..2]), "a and b");
        assert_eq!(names(&v), "a, b and 2 others");
    }
}

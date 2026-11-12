//! Human-readable numbers. Sizes use 1024-based units labelled KB/MB/GB, as Windows does; network
//! speeds use bits per second with 1000-based units, as networking does.

const KB: f64 = 1024.0;
const MB: f64 = KB * 1024.0;
const GB: f64 = MB * 1024.0;
const TB: f64 = GB * 1024.0;

/// "512 B", "12.3 KB", "456 MB", "1.2 GB": three significant figures or so.
pub fn bytes(b: u64) -> String {
    let b = b as f64;
    if b < KB {
        format!("{} B", b as u64)
    } else if b < MB {
        scaled(b / KB, "KB")
    } else if b < GB {
        scaled(b / MB, "MB")
    } else if b < TB {
        scaled(b / GB, "GB")
    } else {
        scaled(b / TB, "TB")
    }
}

/// Same as [`bytes`] but for a size known to be large, like a capacity: "15.9 GB", "954 GB".
pub fn capacity(b: u64) -> String {
    bytes(b)
}

fn scaled(v: f64, unit: &str) -> String {
    if v >= 100.0 {
        format!("{v:.0} {unit}")
    } else if v >= 10.0 {
        format!("{v:.1} {unit}")
    } else {
        format!("{v:.2} {unit}")
    }
}

/// Process memory for a table column: always MB with one decimal until it reaches 1 GB, so the
/// column lines up and compares at a glance. "0.8 MB", "412.6 MB", "1.24 GB".
pub fn mem_column(b: u64) -> String {
    let v = b as f64;
    if v < GB { format!("{:.1} MB", v / MB) } else { format!("{:.2} GB", v / GB) }
}

/// Disk throughput for a table column: "0 MB/s", "0.1 MB/s", "35.2 MB/s".
pub fn rate_column(bytes_per_sec: f64) -> String {
    let mb = bytes_per_sec / MB;
    if mb < 0.05 { "0 MB/s".into() } else if mb < 100.0 { format!("{mb:.1} MB/s") } else { format!("{mb:.0} MB/s") }
}

/// Throughput with automatic units: "0 KB/s", "12.3 KB/s", "45.6 MB/s".
pub fn rate(bytes_per_sec: f64) -> String {
    let v = bytes_per_sec.max(0.0);
    if v < KB {
        if v < 1.0 { "0 KB/s".into() } else { format!("{:.1} KB/s", v / KB) }
    } else if v < MB {
        format!("{}/s", scaled(v / KB, "KB"))
    } else if v < GB {
        format!("{}/s", scaled(v / MB, "MB"))
    } else {
        format!("{}/s", scaled(v / GB, "GB"))
    }
}

/// Network speed in bits per second: "0 Kbps", "840 Kbps", "12.4 Mbps", "1.2 Gbps".
pub fn bits(bytes_per_sec: f64) -> String {
    let b = bytes_per_sec.max(0.0) * 8.0;
    if b < 1_000_000.0 {
        let k = b / 1000.0;
        if k < 0.05 { "0 Kbps".into() } else if k < 10.0 { format!("{k:.1} Kbps") } else { format!("{k:.0} Kbps") }
    } else if b < 1_000_000_000.0 {
        let m = b / 1_000_000.0;
        if m < 10.0 { format!("{m:.2} Mbps") } else if m < 100.0 { format!("{m:.1} Mbps") } else { format!("{m:.0} Mbps") }
    } else {
        format!("{:.2} Gbps", b / 1_000_000_000.0)
    }
}

/// A link speed: "1 Gbps", "866.7 Mbps", "2.5 Gbps".
pub fn link_speed(bps: u64) -> String {
    let b = bps as f64;
    if b >= 1e9 {
        let g = b / 1e9;
        if (g - g.round()).abs() < 0.01 { format!("{g:.0} Gbps") } else { format!("{g:.1} Gbps") }
    } else if b >= 1e6 {
        let m = b / 1e6;
        if (m - m.round()).abs() < 0.01 { format!("{m:.0} Mbps") } else { format!("{m:.1} Mbps") }
    } else {
        format!("{:.0} Kbps", b / 1e3)
    }
}

/// Percentage for a table: one decimal, so small values still compare. "0%", "0.4%", "37.2%".
pub fn pct_column(p: f32) -> String {
    if p < 0.05 { "0%".into() } else { format!("{p:.1}%") }
}

/// Percentage for a headline: whole numbers. "7%", "100%".
pub fn pct(p: f32) -> String {
    format!("{:.0}%", p.clamp(0.0, 100.0))
}

/// "3.70 GHz", "800 MHz".
pub fn mhz(m: u32) -> String {
    if m >= 1000 { format!("{:.2} GHz", m as f64 / 1000.0) } else { format!("{m} MHz") }
}

pub fn celsius(c: f32) -> String {
    format!("{c:.0} °C")
}

/// Uptime: "4:12:55" under a day, "3d 04:12:55" after.
pub fn uptime(secs: u64) -> String {
    let d = secs / 86_400;
    let h = secs / 3600 % 24;
    let m = secs / 60 % 60;
    let s = secs % 60;
    if d > 0 { format!("{d}d {h:02}:{m:02}:{s:02}") } else { format!("{h}:{m:02}:{s:02}") }
}

/// A short span for insights: "45 seconds", "3 minutes", "1 hour 20 minutes".
pub fn span(secs: u64) -> String {
    if secs < 90 {
        format!("{secs} seconds")
    } else if secs < 3600 {
        let m = (secs + 30) / 60;
        format!("{m} minute{}", if m == 1 { "" } else { "s" })
    } else {
        let h = secs / 3600;
        let m = secs % 3600 / 60;
        let hs = format!("{h} hour{}", if h == 1 { "" } else { "s" });
        if m == 0 { hs } else { format!("{hs} {m} minute{}", if m == 1 { "" } else { "s" }) }
    }
}

/// CPU time from 100 ns units: "0:01:23" (h:mm:ss).
pub fn cpu_time(units_100ns: u64) -> String {
    let secs = units_100ns / 10_000_000;
    format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// Thousands separators: 12345 -> "12,345".
pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(12 * 1024 + 300), "12.3 KB");
        assert_eq!(bytes(456 * 1024 * 1024), "456 MB");
        assert_eq!(bytes(16 * 1024 * 1024 * 1024 - 100 * 1024 * 1024), "15.9 GB");
    }

    #[test]
    fn table_columns_line_up() {
        assert_eq!(mem_column(800 * 1024), "0.8 MB");
        assert_eq!(mem_column(412 * 1024 * 1024 + 600 * 1024), "412.6 MB");
        assert_eq!(mem_column(1_331_439_861), "1.24 GB");
        assert_eq!(rate_column(0.0), "0 MB/s");
        assert_eq!(rate_column(0.1 * MB), "0.1 MB/s");
        assert_eq!(pct_column(0.0), "0%");
        assert_eq!(pct_column(0.44), "0.4%");
        assert_eq!(pct_column(37.25), "37.2%");
    }

    #[test]
    fn throughput() {
        assert_eq!(rate(0.0), "0 KB/s");
        assert_eq!(rate(12.3 * KB), "12.3 KB/s");
        assert_eq!(rate(45.6 * MB), "45.6 MB/s");
        assert_eq!(bits(0.0), "0 Kbps");
        assert_eq!(bits(105_000.0), "840 Kbps");
        assert_eq!(bits(1_550_000.0), "12.4 Mbps");
        assert_eq!(link_speed(1_000_000_000), "1 Gbps");
        assert_eq!(link_speed(2_500_000_000), "2.5 Gbps");
        assert_eq!(link_speed(866_700_000), "866.7 Mbps");
    }

    #[test]
    fn times() {
        assert_eq!(uptime(59), "0:00:59");
        assert_eq!(uptime(4 * 3600 + 12 * 60 + 55), "4:12:55");
        assert_eq!(uptime(3 * 86_400 + 4 * 3600 + 12 * 60 + 55), "3d 04:12:55");
        assert_eq!(span(45), "45 seconds");
        assert_eq!(span(180), "3 minutes");
        assert_eq!(span(4800), "1 hour 20 minutes");
        assert_eq!(cpu_time(83 * 10_000_000), "0:01:23");
    }

    #[test]
    fn misc() {
        assert_eq!(mhz(3701), "3.70 GHz");
        assert_eq!(mhz(800), "800 MHz");
        assert_eq!(count(12345), "12,345");
        assert_eq!(count(999), "999");
        assert_eq!(count(1_000_000), "1,000,000");
    }
}

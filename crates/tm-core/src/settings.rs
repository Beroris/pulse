//! Preferences, stored as a small, human-readable JSON file.
//!
//! Every field has a default, unknown fields are ignored and out-of-range values are clamped, so a
//! hand-edited or partly broken file never stops the app from starting.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Motion {
    /// Follow Windows' "Animation effects" setting.
    #[default]
    System,
    On,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Background {
    /// Keep sampling everything while hidden, so insights and history stay complete.
    #[default]
    Full,
    /// Keep system-wide history only. Processes aren't sampled while hidden.
    History,
    /// Stop sampling while hidden.
    Pause,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Renderer {
    /// Drawn on the CPU. The lightest choice: no graphics driver loaded.
    #[default]
    Software,
    /// Drawn with OpenGL. Smoother on very large windows; costs graphics driver memory.
    Gpu,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Page {
    #[default]
    Overview,
    Processes,
    Performance,
    Startup,
    Services,
    Settings,
}

impl Page {
    pub const ALL: [Page; 6] = [Page::Overview, Page::Processes, Page::Performance, Page::Startup, Page::Services, Page::Settings];
    pub fn index(self) -> i32 {
        Page::ALL.iter().position(|p| *p == self).unwrap_or(0) as i32
    }
    pub fn from_index(i: i32) -> Page {
        Page::ALL.get(i.max(0) as usize).copied().unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ViewMode {
    /// Apps, background and Windows processes, each grouped by program.
    #[default]
    Grouped,
    /// Parent and child processes.
    Tree,
    /// Every process on its own.
    List,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Hash, Default, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum Column {
    #[default]
    Name,
    Pid,
    Status,
    User,
    Cpu,
    Memory,
    Disk,
    Gpu,
    GpuEngine,
    Path,
    CommandLine,
    Started,
    Priority,
    Threads,
    Handles,
    Commit,
    WorkingSet,
    CpuTime,
    Description,
    Publisher,
}

impl Column {
    /// Columns that can be turned on and off, in display order.
    pub const OPTIONAL: [Column; 18] = [
        Column::Pid,
        Column::Status,
        Column::User,
        Column::Cpu,
        Column::Memory,
        Column::Disk,
        Column::Gpu,
        Column::GpuEngine,
        Column::Description,
        Column::Publisher,
        Column::Path,
        Column::Started,
        Column::Priority,
        Column::Threads,
        Column::Handles,
        Column::Commit,
        Column::WorkingSet,
        Column::CpuTime,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Column::Name => "Name",
            Column::Pid => "PID",
            Column::Status => "Status",
            Column::User => "User",
            Column::Cpu => "CPU",
            Column::Memory => "Memory",
            Column::Disk => "Disk",
            Column::Gpu => "GPU",
            Column::GpuEngine => "GPU engine",
            Column::Path => "Path",
            Column::CommandLine => "Command line",
            Column::Started => "Started",
            Column::Priority => "Priority",
            Column::Threads => "Threads",
            Column::Handles => "Handles",
            Column::Commit => "Commit size",
            Column::WorkingSet => "Working set",
            Column::CpuTime => "CPU time",
            Column::Description => "Description",
            Column::Publisher => "Publisher",
        }
    }
    /// Text columns sort A→Z first; number columns biggest first.
    pub fn numeric(self) -> bool {
        matches!(
            self,
            Column::Pid
                | Column::Cpu
                | Column::Memory
                | Column::Disk
                | Column::Gpu
                | Column::Threads
                | Column::Handles
                | Column::Commit
                | Column::WorkingSet
                | Column::CpuTime
                | Column::Started
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Metric {
    #[default]
    Cpu,
    Memory,
    Gpu,
    Disk,
    Network,
    /// Frames per second of the app in front.
    Fps,
    /// The GPU's dedicated memory in use.
    #[serde(rename = "gpu_memory")]
    GpuMemory,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactSettings {
    pub metrics: Vec<Metric>,
    /// Clicks pass through to whatever is underneath.
    pub click_through: bool,
    /// 0.3 to 1.
    pub opacity: f32,
    pub x: Option<i32>,
    pub y: Option<i32>,
    /// Was showing when the app last quit.
    pub open: bool,
}

impl Default for CompactSettings {
    fn default() -> Self {
        Self { metrics: vec![Metric::Cpu, Metric::Memory, Metric::Gpu], click_through: false, opacity: 0.94, x: None, y: None, open: false }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WindowPlacement {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

/// How a simple table (startup apps, services) is sorted and how wide its columns are.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TableLayout {
    /// Key of the sort column; empty for the table’s default.
    pub sort: String,
    pub desc: bool,
    /// Widths the user set, in logical pixels, by column key. A column that isn’t here takes its
    /// default width, or the room left over if it’s the table’s stretching column.
    pub widths: std::collections::BTreeMap<String, u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // Appearance
    pub theme: ThemeChoice,
    /// "system" for the Windows accent colour, or "#rrggbb".
    pub accent: String,
    /// Interface scale on top of the display's own scaling, 0.8 to 1.5.
    pub scale: f32,
    pub motion: Motion,
    pub sidebar_compact: bool,
    // Monitoring
    pub refresh_ms: u32,
    pub history_secs: u32,
    pub background: Background,
    pub reduced: bool,
    pub renderer: Renderer,
    // General
    pub start_page: Page,
    pub remember_window: bool,
    pub window: Option<WindowPlacement>,
    pub always_on_top: bool,
    pub tray_icon: bool,
    /// Closing the window keeps the app in the tray instead of quitting.
    pub close_to_tray: bool,
    pub minimize_to_tray: bool,
    // Processes
    pub view: ViewMode,
    pub sort: Column,
    pub sort_desc: bool,
    pub columns: Vec<Column>,
    /// Column widths the user set, in logical pixels.
    pub widths: std::collections::BTreeMap<Column, u32>,
    /// Show programs' descriptions ("Google Chrome") rather than file names ("chrome.exe").
    pub friendly_names: bool,
    /// Process memory always in MB (“2,080.4 MB”) instead of switching to GB past 1 GB.
    pub memory_in_mb: bool,
    // Startup and services
    pub startup_table: TableLayout,
    pub services_table: TableLayout,
    // Performance
    pub cpu_per_core: bool,
    pub compact: CompactSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::System,
            accent: "system".into(),
            scale: 1.0,
            motion: Motion::System,
            sidebar_compact: false,
            refresh_ms: 1000,
            history_secs: 60,
            background: Background::Full,
            reduced: false,
            renderer: Renderer::Software,
            start_page: Page::Overview,
            remember_window: true,
            window: None,
            always_on_top: false,
            tray_icon: true,
            close_to_tray: false,
            minimize_to_tray: false,
            view: ViewMode::Grouped,
            sort: Column::Cpu,
            sort_desc: true,
            columns: vec![Column::Status, Column::Cpu, Column::Memory, Column::Disk, Column::Gpu],
            widths: Default::default(),
            friendly_names: true,
            memory_in_mb: false,
            startup_table: TableLayout::default(),
            services_table: TableLayout::default(),
            cpu_per_core: false,
            compact: CompactSettings::default(),
        }
    }
}

pub const REFRESH_CHOICES: [u32; 4] = [500, 1000, 2000, 5000];
pub const HISTORY_CHOICES: [u32; 4] = [60, 120, 300, 600];

impl Settings {
    /// Reads settings from JSON text. Anything unreadable falls back to defaults.
    pub fn from_json(text: &str) -> Settings {
        let mut s: Settings = serde_json::from_str(text).unwrap_or_default();
        s.sanitize();
        s
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    /// Clamps every value into its allowed range.
    pub fn sanitize(&mut self) {
        self.refresh_ms = *REFRESH_CHOICES.iter().min_by_key(|c| (**c as i64 - self.refresh_ms as i64).abs()).unwrap_or(&1000);
        self.history_secs = *HISTORY_CHOICES.iter().min_by_key(|c| (**c as i64 - self.history_secs as i64).abs()).unwrap_or(&60);
        if !self.scale.is_finite() {
            self.scale = 1.0;
        }
        self.scale = (self.scale * 20.0).round() / 20.0;
        self.scale = self.scale.clamp(0.8, 1.5);
        if !self.compact.opacity.is_finite() {
            self.compact.opacity = 0.94;
        }
        self.compact.opacity = self.compact.opacity.clamp(0.3, 1.0);
        if self.compact.metrics.is_empty() {
            self.compact.metrics = CompactSettings::default().metrics;
        }
        dedup(&mut self.compact.metrics);
        self.columns.retain(|c| *c != Column::Name && *c != Column::CommandLine);
        dedup(&mut self.columns);
        self.widths.retain(|_, w| (40..=2000).contains(w));
        for t in [&mut self.startup_table, &mut self.services_table] {
            t.widths.retain(|_, w| (40..=2000).contains(w));
        }
        if self.accent != "system" && parse_hex(&self.accent).is_none() {
            self.accent = "system".into();
        }
        if let Some(w) = &self.window {
            if w.width < 400 || w.height < 300 || w.width > 20_000 || w.height > 20_000 {
                self.window = None;
            }
        }
    }

    /// Effective refresh interval: reduced mode never samples faster than every 2 s.
    pub fn effective_refresh_ms(&self) -> u32 {
        if self.reduced { self.refresh_ms.max(2000) } else { self.refresh_ms }
    }

    /// Number of samples a graph keeps.
    pub fn history_len(&self) -> usize {
        ((self.history_secs as u64 * 1000) / self.effective_refresh_ms() as u64).max(2) as usize + 1
    }
}

fn dedup<T: PartialEq + Copy>(v: &mut Vec<T>) {
    let mut seen: Vec<T> = Vec::with_capacity(v.len());
    v.retain(|x| {
        if seen.contains(x) {
            false
        } else {
            seen.push(*x);
            true
        }
    });
}

/// Parses "#rrggbb" or "rrggbb".
pub fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut s = Settings::default();
        s.theme = ThemeChoice::Light;
        s.columns.push(Column::Pid);
        s.compact.metrics = vec![Metric::Network];
        let back = Settings::from_json(&s.to_json());
        assert_eq!(back, s);
    }

    #[test]
    fn broken_or_partial_files_fall_back_to_defaults() {
        assert_eq!(Settings::from_json("not json"), Settings::default());
        let s = Settings::from_json(r#"{"theme":"dark","something_new":42}"#);
        assert_eq!(s.theme, ThemeChoice::Dark);
        assert_eq!(s.refresh_ms, 1000);
    }

    #[test]
    fn values_are_clamped_to_choices() {
        let s = Settings::from_json(r#"{"refresh_ms":1,"history_secs":100000,"scale":9,"accent":"bogus","compact":{"opacity":0}}"#);
        assert_eq!(s.refresh_ms, 500);
        assert_eq!(s.history_secs, 600);
        assert_eq!(s.scale, 1.5);
        assert_eq!(s.accent, "system");
        assert_eq!(s.compact.opacity, 0.3);
    }

    #[test]
    fn duplicates_and_fixed_columns_are_removed() {
        let s = Settings::from_json(r#"{"columns":["cpu","cpu","name","memory"]}"#);
        assert_eq!(s.columns, vec![Column::Cpu, Column::Memory]);
    }

    #[test]
    fn reduced_mode_slows_sampling() {
        let mut s = Settings::default();
        s.refresh_ms = 500;
        s.reduced = true;
        assert_eq!(s.effective_refresh_ms(), 2000);
        assert_eq!(s.history_len(), 31);
    }

    #[test]
    fn hex_colours() {
        assert_eq!(parse_hex("#4c8dff"), Some((0x4c, 0x8d, 0xff)));
        assert_eq!(parse_hex("4C8DFF"), Some((0x4c, 0x8d, 0xff)));
        assert_eq!(parse_hex("#4c8df"), None);
    }
}

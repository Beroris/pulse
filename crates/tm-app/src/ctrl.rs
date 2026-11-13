//! The controller: owns the windows, turns collector frames into view data, and carries out what
//! the user asks for. Runs on the UI thread; slow work goes to short-lived worker threads whose
//! results come back through the event loop.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{CloseRequestResponse, ComponentHandle, Image, PhysicalPosition, PhysicalSize, SharedString, Timer, TimerMode, VecModel};
use tm_core::engine::{Collector, Command, Demand, Histories};
use tm_core::insights::Target;
use tm_core::platform::*;
use tm_core::safety::Protection;
use tm_core::settings::*;
use tm_core::table::RowId;
use tm_core::{Frame, ProcKey, SystemInfo, format};

use crate::lists::{self, Services, Startup};
use crate::pages::{self, DevKey, IconSet};
use crate::procs::{self, CellContext, Table};
use crate::util::{self, model, stat};
use crate::*;

const SCALES: [f32; 6] = [0.8, 0.9, 1.0, 1.1, 1.25, 1.5];
const SWATCHES: [Rgb; 8] = [
    (0x3b, 0x82, 0xf6),
    (0x38, 0xbd, 0xf8),
    (0x14, 0xb8, 0xa6),
    (0x22, 0xc5, 0x5e),
    (0xf9, 0x73, 0x16),
    (0xec, 0x48, 0x99),
    (0x8b, 0x5c, 0xf6),
    (0x94, 0xa3, 0xb8),
];
/// In the order the compact monitor shows them, and Settings lists them.
const METRICS: [Metric; 7] = [Metric::Fps, Metric::Cpu, Metric::Memory, Metric::Gpu, Metric::GpuMemory, Metric::Disk, Metric::Network];
const DARK_BG: Rgb = (0x13, 0x13, 0x15);
const LIGHT_BG: Rgb = (0xfa, 0xfa, 0xfb);

thread_local! {
    static CTRL: RefCell<Option<Rc<Ctrl>>> = const { RefCell::new(None) };
}

/// Runs `f` with the controller, if the app is running.
pub fn with(f: impl FnOnce(&Rc<Ctrl>)) {
    let c = CTRL.with(|x| x.borrow().clone());
    if let Some(c) = c {
        f(&c);
    }
}

/// Runs `f` on the UI thread later. For worker threads reporting back.
fn post(f: impl FnOnce(&Rc<Ctrl>) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || with(f));
}

/// A process an action applies to, captured when the user asked.
#[derive(Clone)]
struct Target1 {
    key: ProcKey,
    name: String,
    has_window: bool,
}

struct Models {
    cards: Rc<VecModel<CardData>>,
    insights: Rc<VecModel<InsightData>>,
    rows: procs::RowsModel,
    columns: Rc<VecModel<ColumnData>>,
    column_on: Rc<VecModel<bool>>,
    details: Rc<VecModel<StatData>>,
    devices: Rc<VecModel<DeviceData>>,
    graphs: Rc<VecModel<PerfGraph>>,
    cores: Rc<VecModel<GraphData>>,
    live: Rc<VecModel<StatData>>,
    fixed: Rc<VecModel<StatData>>,
    segments: Rc<VecModel<Segment>>,
    startup: Rc<VecModel<StartupRow>>,
    startup_cols: Rc<VecModel<ColumnData>>,
    services: Rc<VecModel<ServiceRow>>,
    service_cols: Rc<VecModel<ColumnData>>,
    service_facts: Rc<VecModel<StatData>>,
    compact_on: Rc<VecModel<bool>>,
    swatches: Rc<VecModel<Swatch>>,
    compact: Rc<VecModel<CompactMetric>>,
    dialog_lines: Rc<VecModel<SharedString>>,
}

impl Models {
    fn new() -> Self {
        Self {
            cards: Rc::default(),
            insights: Rc::default(),
            rows: Default::default(),
            columns: Rc::default(),
            column_on: Rc::default(),
            details: Rc::default(),
            devices: Rc::default(),
            graphs: Rc::default(),
            cores: Rc::default(),
            live: Rc::default(),
            fixed: Rc::default(),
            segments: Rc::default(),
            startup: Rc::default(),
            startup_cols: Rc::default(),
            services: Rc::default(),
            service_cols: Rc::default(),
            service_facts: Rc::default(),
            compact_on: Rc::default(),
            swatches: Rc::default(),
            compact: Rc::default(),
            dialog_lines: Rc::default(),
        }
    }
}

struct State {
    s: Settings,
    path: PathBuf,
    frame: Arc<Frame>,
    info: Arc<SystemInfo>,
    info_seq: u64,
    icons: HashMap<u32, Image>,
    cost: Duration,
    error: Option<String>,
    main_shown: bool,
    minimized: bool,
    compact_shown: bool,
    page: Page,
    device: DevKey,
    dev_keys: Vec<DevKey>,
    table: Table,
    startup: Startup,
    services: Services,
    dialog: Option<Box<dyn FnOnce(&Rc<Ctrl>)>>,
    /// The renderer this run uses (the setting may differ until restart).
    renderer: Renderer,
    elevated: bool,
    demand: Option<Demand>,
    /// The theme as last applied: dark, accent, motion, high contrast.
    theme: Option<(bool, Rgb, bool, bool)>,
    /// Last size and position of the main window while neither minimized nor maximized.
    normal: Option<(i32, i32, u32, u32)>,
    quitting: bool,
    /// A window was shown before Windows created it: style and scale it on its first event.
    setup_pending: bool,
    /// The compact monitor has no saved position yet and still needs its default one.
    compact_unplaced: bool,
}

pub struct Ctrl {
    main: MainWindow,
    compact: RefCell<Option<CompactWindow>>,
    tray: RefCell<Option<Tray>>,
    collector: Collector,
    control: Arc<dyn SystemControl>,
    shell: Rc<dyn Shell>,
    st: RefCell<State>,
    m: Models,
    icons: IconSet,
    save_timer: Timer,
    notice_timer: Timer,
    services_timer: Timer,
    theme_timer: Timer,
    trim_timer: Timer,
}

macro_rules! cb {
    ($c:ident, $target:expr, $on:ident, |$($arg:ident),*| $body:expr) => {{
        let w: Weak<Ctrl> = Rc::downgrade($c);
        $target.$on(move |$($arg),*| {
            if let Some($c) = w.upgrade() {
                let $c = &$c;
                $body;
            }
        });
    }};
}

pub fn run(p: Platform) -> Result<(), slint::PlatformError> {
    let text = std::fs::read_to_string(&p.settings_path).unwrap_or_default();
    let mut s = Settings::from_json(&text);
    let wanted = s.renderer;
    let name = match wanted {
        Renderer::Software => "software",
        Renderer::Gpu => "femtovg",
    };
    let mut renderer = wanted;
    if slint::BackendSelector::new().backend_name("winit".into()).renderer_name(name.into()).select().is_err() {
        renderer = Renderer::Software;
        slint::BackendSelector::new().backend_name("winit".into()).renderer_name("software".into()).select()?;
    }
    let main = MainWindow::new()?;
    let elevated = p.control.is_elevated();
    // The registry is the truth for "start when I sign in".
    s.compact.open = s.compact.open && !s.compact.metrics.is_empty();

    let pending = Arc::new(AtomicBool::new(false));
    let notify = {
        let pending = pending.clone();
        Box::new(move || {
            if !pending.swap(true, Ordering::AcqRel) {
                let pending = pending.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    pending.store(false, Ordering::Release);
                    with(|c| c.on_frame());
                });
            }
        })
    };
    let start_page = match s.start_page {
        Page::Settings => Page::Overview,
        p => p,
    };
    let state = State {
        path: p.settings_path.clone(),
        frame: Arc::new(Frame::default()),
        info: Arc::new(SystemInfo::default()),
        info_seq: 0,
        icons: HashMap::new(),
        cost: Duration::ZERO,
        error: None,
        main_shown: false,
        minimized: false,
        compact_shown: false,
        page: start_page,
        device: DevKey::Cpu,
        dev_keys: vec![DevKey::Cpu, DevKey::Memory],
        table: Table::default(),
        startup: Startup::default(),
        services: Services::default(),
        dialog: None,
        renderer,
        elevated,
        demand: None,
        theme: None,
        normal: s.window.map(|w| (w.x, w.y, w.width, w.height)),
        quitting: false,
        setup_pending: false,
        compact_unplaced: false,
        s,
    };
    let demand = initial_demand(&state, !p.start_hidden);
    let collector = Collector::spawn(
        p.sampler,
        p.resolver,
        notify,
        Duration::from_millis(state.s.effective_refresh_ms() as u64),
        state.s.history_len(),
        demand,
    );
    let ic = main.global::<Icons>();
    let icons = IconSet {
        chip: ic.get_chip(),
        thermometer: ic.get_thermometer(),
        memory: ic.get_memory(),
        gpu: ic.get_gpu(),
        disk: ic.get_disk(),
        network: ic.get_network(),
        clock: ic.get_clock(),
        processes: ic.get_processes(),
    };
    let c = Rc::new(Ctrl {
        main,
        compact: RefCell::new(None),
        tray: RefCell::new(None),
        collector,
        control: p.control,
        shell: p.shell,
        st: RefCell::new(state),
        m: Models::new(),
        icons,
        save_timer: Timer::default(),
        notice_timer: Timer::default(),
        services_timer: Timer::default(),
        theme_timer: Timer::default(),
        trim_timer: Timer::default(),
    });
    {
        let mut st = c.st.borrow_mut();
        st.demand = Some(demand);
    }
    CTRL.with(|x| *x.borrow_mut() = Some(c.clone()));
    c.init(p.start_hidden);
    let r = slint::run_event_loop_until_quit();
    c.save_now();
    r
}

fn initial_demand(st: &State, main: bool) -> Demand {
    compute_demand(&st.s, st.page, main, st.s.compact.open, matches!(st.device, DevKey::Gpu(_)))
}

/// What to sample for what is on screen. `gpu_open`: the Performance page is showing a GPU.
fn compute_demand(s: &Settings, page: Page, main: bool, compact: bool, gpu_open: bool) -> Demand {
    if !main && !compact && s.background == Background::Pause {
        return Demand { processes: false, windows: false, services: false, process_gpu: false, memory_detail: false, sensors: false, gpu_engines: false, fps: false, paused: true };
    }
    let on_procs = main && page == Page::Processes;
    Demand {
        processes: main || s.background == Background::Full,
        windows: on_procs,
        services: on_procs,
        process_gpu: on_procs && !s.reduced && (s.columns.contains(&Column::Gpu) || s.columns.contains(&Column::GpuEngine)),
        memory_detail: main && page == Page::Performance,
        sensors: !s.reduced && ((main && matches!(page, Page::Overview | Page::Performance)) || compact),
        // Each engine has its own graph there. Elsewhere only the busiest engine is shown.
        gpu_engines: main && page == Page::Performance && gpu_open,
        fps: compact && s.compact.metrics.contains(&Metric::Fps),
        paused: false,
    }
}

impl Ctrl {
    fn app(&self) -> App<'_> {
        self.main.global::<App>()
    }

    // ─── Start-up ──────────────────────────────────────────────────────────

    fn init(self: &Rc<Self>, start_hidden: bool) {
        let app = self.app();
        app.set_version(env!("CARGO_PKG_VERSION").into());
        {
            let st = self.st.borrow();
            app.set_elevated(st.elevated);
            app.set_settings_path(st.path.to_string_lossy().as_ref().into());
        }
        app.set_cards(model(&self.m.cards));
        app.set_insights(model(&self.m.insights));
        app.set_rows(self.m.rows.model());
        app.set_columns(model(&self.m.columns));
        app.set_column_on(model(&self.m.column_on));
        app.set_details(model(&self.m.details));
        app.set_devices(model(&self.m.devices));
        app.set_perf_graphs(model(&self.m.graphs));
        app.set_perf_cores(model(&self.m.cores));
        app.set_perf_live(model(&self.m.live));
        app.set_perf_static(model(&self.m.fixed));
        app.set_perf_segments(model(&self.m.segments));
        app.set_startup_rows(model(&self.m.startup));
        app.set_startup_columns(model(&self.m.startup_cols));
        app.set_service_rows(model(&self.m.services));
        app.set_service_columns(model(&self.m.service_cols));
        {
            let mut st = self.st.borrow_mut();
            st.startup.layout = st.s.startup_table.clone();
            st.services.layout = st.s.services_table.clone();
        }
        self.push_table_columns();
        app.set_service_facts(model(&self.m.service_facts));
        app.set_compact_metrics(model(&self.m.compact_on));
        app.set_swatches(model(&self.m.swatches));
        app.set_dialog_lines(model(&self.m.dialog_lines));
        let labels: Vec<&str> = Column::OPTIONAL.iter().map(|c| c.label()).collect();
        app.set_column_choices(util::strings(&labels));
        app.set_overview_note("Nothing needs attention. Insights appear here when something stays unusual for a while, such as memory staying nearly full or one app keeping the CPU busy.".into());
        app.set_autostart(self.control.autostart());
        self.push_settings();
        self.apply_theme(true);
        self.wire();
        self.wire_window();

        let page = self.st.borrow().page;
        self.navigate(page);
        self.restore_placement();

        let (tray, compact) = {
            let st = self.st.borrow();
            (st.s.tray_icon, st.s.compact.open)
        };
        if tray {
            self.set_tray(true);
        }
        if !start_hidden || (!tray && !compact) {
            self.show_main();
            if start_hidden {
                self.main.window().set_minimized(true);
            }
        }
        if compact {
            self.show_compact();
        }
        // Read startup entries early, so the "many startup apps" insight can show without a visit.
        Timer::single_shot(Duration::from_secs(4), || {
            with(|c| {
                let need = { let st = c.st.borrow(); !st.startup.loaded && !st.startup.loading };
                if need {
                    c.load_startup(false);
                }
            })
        });
        let w = Rc::downgrade(self);
        self.theme_timer.start(TimerMode::Repeated, Duration::from_secs(2), move || {
            if let Some(c) = w.upgrade() {
                // Only while something shows: a hidden app picks the theme up when it next appears.
                let shown = { let st = c.st.borrow(); (st.main_shown && !st.minimized) || st.compact_shown };
                if shown {
                    c.apply_theme(false);
                }
            }
        });
    }

    /// Copies settings into the view's properties.
    fn push_settings(&self) {
        let app = self.app();
        let st = self.st.borrow();
        let s = &st.s;
        app.set_theme_index(match s.theme {
            ThemeChoice::System => 0,
            ThemeChoice::Dark => 1,
            ThemeChoice::Light => 2,
        });
        app.set_motion_index(match s.motion {
            Motion::System => 0,
            Motion::On => 1,
            Motion::Off => 2,
        });
        app.set_scale_index(SCALES.iter().position(|x| (x - s.scale).abs() < 0.01).unwrap_or(2) as i32);
        app.set_refresh_index(REFRESH_CHOICES.iter().position(|x| *x == s.refresh_ms).unwrap_or(1) as i32);
        app.set_history_index(HISTORY_CHOICES.iter().position(|x| *x == s.history_secs).unwrap_or(0) as i32);
        app.set_background_index(match s.background {
            Background::Full => 0,
            Background::History => 1,
            Background::Pause => 2,
        });
        app.set_renderer_index(if s.renderer == Renderer::Gpu { 1 } else { 0 });
        app.set_renderer_restart(s.renderer != st.renderer);
        app.set_start_page_index(s.start_page.index().min(4));
        app.set_reduced(s.reduced);
        app.set_remember_window(s.remember_window);
        app.set_always_on_top(s.always_on_top);
        app.set_tray_icon(s.tray_icon);
        app.set_close_to_tray(s.close_to_tray);
        app.set_minimize_to_tray(s.minimize_to_tray);
        app.set_compact_click_through(s.compact.click_through);
        app.set_sidebar_compact(s.sidebar_compact);
        app.set_view_mode(match s.view {
            ViewMode::Grouped => 0,
            ViewMode::Tree => 1,
            ViewMode::List => 2,
        });
        app.set_friendly_names(s.friendly_names);
        app.set_memory_in_mb(s.memory_in_mb);
        app.set_perf_per_core(s.cpu_per_core);
        util::sync(&self.m.compact_on, METRICS.iter().map(|m| s.compact.metrics.contains(m)).collect());
        util::sync(&self.m.column_on, Column::OPTIONAL.iter().map(|c| s.columns.contains(c)).collect());
    }

    // ─── Wiring ────────────────────────────────────────────────────────────

    fn wire(self: &Rc<Self>) {
        let c = self;
        let app = self.app();
        cb!(c, app, on_navigate, |i| c.navigate(Page::from_index(i)));
        cb!(c, app, on_toggle_sidebar, | | {
            let v = !c.st.borrow().s.sidebar_compact;
            c.setting_bool("sidebar", v)
        });
        cb!(c, app, on_open_compact, | | c.toggle_compact());
        cb!(c, app, on_restart_elevated, | | c.restart_elevated());
        cb!(c, app, on_card_clicked, |k| c.card_clicked(&k));
        cb!(c, app, on_insight_action, |id| c.insight_action(&id));

        // Processes
        cb!(c, app, on_search_edited, |t| {
            c.st.borrow_mut().table.search = t.to_string();
            c.app().set_table_scroll(0.0);
            c.update_table()
        });
        cb!(c, app, on_view_changed, |i| {
            c.st.borrow_mut().s.view = match i {
                1 => ViewMode::Tree,
                2 => ViewMode::List,
                _ => ViewMode::Grouped,
            };
            c.save_soon();
            c.update_table()
        });
        cb!(c, app, on_friendly_changed, |v| {
            c.st.borrow_mut().s.friendly_names = v;
            c.save_soon();
            c.update_table()
        });
        cb!(c, app, on_sort_by, |i| c.sort_by(i));
        cb!(c, app, on_column_toggled, |i| c.toggle_column(i));
        cb!(c, app, on_column_resized, |i, w| c.resize_column(i, w));
        cb!(c, app, on_column_resize_started, |name_w| c.pin_name_column(name_w));
        cb!(c, app, on_columns_reset, | | {
            c.st.borrow_mut().s.widths.clear();
            c.save_soon();
            c.update_columns()
        });
        cb!(c, app, on_row_pressed, |i, ctrl, shift| {
            c.st.borrow_mut().table.press(i.max(0) as usize, ctrl, shift);
            c.update_table();
            c.follow_details()
        });
        cb!(c, app, on_row_context, |i| {
            {
                let mut st = c.st.borrow_mut();
                let id = st.table.rows.get(i.max(0) as usize).map(|r| r.id.clone());
                if let Some(id) = id {
                    if !st.table.selected.contains(&id) {
                        st.table.press(i as usize, false, false);
                    }
                }
            }
            c.update_table()
        });
        cb!(c, app, on_row_activated, |i| {
            // A double-click opens the details of a process or an app; on a section heading
            // (Apps, Background processes…) it opens or closes the section.
            let is_section = matches!(c.st.borrow().table.rows.get(i.max(0) as usize).map(|r| &r.id), Some(RowId::Section(_)));
            if !is_section {
                c.show_details();
            } else {
                c.st.borrow_mut().table.toggle(i.max(0) as usize);
                c.update_table();
            }
        });
        cb!(c, app, on_row_toggle, |i| {
            c.st.borrow_mut().table.toggle(i.max(0) as usize);
            c.update_table()
        });
        cb!(c, app, on_move_selection, |d, shift| {
            let idx = c.st.borrow_mut().table.move_cursor(d, shift);
            c.update_table();
            if let Some(i) = idx {
                c.scroll_to(i);
            }
            c.follow_details()
        });
        cb!(c, app, on_expand_selected, |open| {
            c.st.borrow_mut().table.expand_cursor(open);
            c.update_table()
        });
        cb!(c, app, on_table_hover, |on| {
            c.st.borrow_mut().table.hold = on;
            // Leaving: show the current order straight away rather than at the next refresh.
            if !on {
                c.update_table();
            }
        });
        cb!(c, app, on_select_all, | | {
            c.st.borrow_mut().table.select_all();
            c.update_table()
        });
        cb!(c, app, on_end_selected, | | c.end_selected(false));
        cb!(c, app, on_force_end_selected, | | c.end_selected(true));
        cb!(c, app, on_set_priority, |i| c.set_priority(i));
        cb!(c, app, on_copy, |i| c.copy(i));
        cb!(c, app, on_open_location, | | {
            let path = c.st.borrow().table.single().and_then(|p| p.path().map(str::to_string));
            if let Some(p) = path {
                c.control.open_location(&p);
            }
        });
        cb!(c, app, on_file_properties, | | {
            let path = c.st.borrow().table.single().and_then(|p| p.path().map(str::to_string));
            if let Some(p) = path {
                c.control.open_properties(&p);
            }
        });
        cb!(c, app, on_go_to_services, | | c.go_to_services());
        cb!(c, app, on_show_details, | | c.show_details());
        cb!(c, app, on_close_details, | | {
            c.st.borrow_mut().table.details = None;
            c.app().set_details_open(false)
        });

        // Performance
        cb!(c, app, on_device_selected, |i| {
            {
                let mut st = c.st.borrow_mut();
                if let Some(k) = st.dev_keys.get(i.max(0) as usize).cloned() {
                    st.device = k;
                }
            }
            c.update_demand();
            c.render_page()
        });
        cb!(c, app, on_toggle_per_core, | | {
            let v = {
                let mut st = c.st.borrow_mut();
                st.s.cpu_per_core = !st.s.cpu_per_core;
                st.s.cpu_per_core
            };
            c.app().set_perf_per_core(v);
            c.save_soon();
            c.render_page()
        });

        // Startup
        cb!(c, app, on_startup_search_edited, |t| {
            {
                let mut st = c.st.borrow_mut();
                st.startup.search = t.to_string();
                st.startup.filter();
            }
            c.render_startup(true)
        });
        cb!(c, app, on_startup_select, |i| {
            {
                let mut st = c.st.borrow_mut();
                st.startup.selected = if i < 0 { None } else { st.startup.entry_at(i as usize).map(|e| e.id.clone()) };
            }
            c.render_startup(false)
        });
        cb!(c, app, on_startup_toggle, |i, on| c.startup_toggle(i, on));
        cb!(c, app, on_startup_open_location, | | {
            let p = c.st.borrow().startup.selected_entry().and_then(|e| e.path.clone());
            if let Some(p) = p {
                c.control.open_location(&p);
            }
        });
        cb!(c, app, on_startup_properties, | | {
            let p = c.st.borrow().startup.selected_entry().and_then(|e| e.path.clone());
            if let Some(p) = p {
                c.control.open_properties(&p);
            }
        });
        cb!(c, app, on_startup_refresh, | | c.load_startup(true));
        cb!(c, app, on_startup_filter_changed, | | {
            {
                let app = c.app();
                let mut st = c.st.borrow_mut();
                st.startup.show = app.get_startup_filter();
                st.startup.location = app.get_startup_location();
                st.startup.filter();
            }
            c.render_startup(true)
        });
        cb!(c, app, on_startup_sort, |i| {
            {
                let mut st = c.st.borrow_mut();
                lists::sort_clicked(&lists::STARTUP_COLS, &mut st.startup.layout, lists::STARTUP_SORT, i.max(0) as usize);
                st.startup.filter();
            }
            c.table_layout_changed();
            c.render_startup(true)
        });
        cb!(c, app, on_startup_resize_started, |w| {
            let changed = lists::pin_stretch(&lists::STARTUP_COLS, &mut c.st.borrow_mut().startup.layout, w);
            if changed {
                c.table_layout_changed();
            }
        });
        cb!(c, app, on_startup_resized, |i, w| {
            lists::resized(&lists::STARTUP_COLS, &mut c.st.borrow_mut().startup.layout, i.max(0) as usize, w);
            c.table_layout_changed()
        });
        cb!(c, app, on_startup_columns_reset, | | {
            c.st.borrow_mut().startup.layout.widths.clear();
            c.table_layout_changed()
        });

        // Services
        cb!(c, app, on_services_search_edited, |t| {
            {
                let mut st = c.st.borrow_mut();
                st.services.search = t.to_string();
                st.services.apply_filter();
            }
            c.render_services()
        });
        cb!(c, app, on_services_filter_changed, |i| {
            {
                let mut st = c.st.borrow_mut();
                st.services.filter = i;
                st.services.apply_filter();
            }
            c.render_services()
        });
        cb!(c, app, on_service_select, |i| {
            {
                let mut st = c.st.borrow_mut();
                st.services.selected = if i < 0 { None } else { st.services.at(i as usize).map(|s| s.name.clone()) };
            }
            c.render_services()
        });
        cb!(c, app, on_service_start, | | c.service_command(ServiceAction::Start));
        cb!(c, app, on_service_stop, | | c.service_command(ServiceAction::Stop));
        cb!(c, app, on_service_restart, | | c.service_command(ServiceAction::Restart));
        cb!(c, app, on_service_set_start, |i| {
            if let Some(t) = StartType::CHOOSABLE.get(i.max(0) as usize) {
                c.service_command(ServiceAction::SetStart(*t));
            }
        });
        cb!(c, app, on_service_go_to_process, | | c.service_go_to_process());
        cb!(c, app, on_services_refresh, | | c.load_services(true));
        cb!(c, app, on_services_start_filter_changed, |i| {
            {
                let mut st = c.st.borrow_mut();
                st.services.start_filter = i;
                st.services.apply_filter();
            }
            c.render_services()
        });
        cb!(c, app, on_service_sort, |i| {
            {
                let mut st = c.st.borrow_mut();
                lists::sort_clicked(&lists::SERVICE_COLS, &mut st.services.layout, lists::SERVICE_SORT, i.max(0) as usize);
                st.services.apply_filter();
            }
            c.table_layout_changed();
            c.render_services()
        });
        cb!(c, app, on_service_resize_started, |w| {
            let changed = lists::pin_stretch(&lists::SERVICE_COLS, &mut c.st.borrow_mut().services.layout, w);
            if changed {
                c.table_layout_changed();
            }
        });
        cb!(c, app, on_service_resized, |i, w| {
            lists::resized(&lists::SERVICE_COLS, &mut c.st.borrow_mut().services.layout, i.max(0) as usize, w);
            c.table_layout_changed()
        });
        cb!(c, app, on_service_columns_reset, | | {
            c.st.borrow_mut().services.layout.widths.clear();
            c.table_layout_changed()
        });
        cb!(c, app, on_clear_standby, | | c.clear_standby());

        // Settings
        cb!(c, app, on_setting_int, |k, v| c.setting_int(&k, v));
        cb!(c, app, on_setting_bool, |k, v| c.setting_bool(&k, v));
        cb!(c, app, on_accent_picked, |i| {
            if let Some(sw) = SWATCHES.get(i.max(0) as usize) {
                c.st.borrow_mut().s.accent = util::hex(*sw);
                c.apply_theme(true);
                c.save_soon();
            }
        });
        cb!(c, app, on_accent_system_picked, | | {
            c.st.borrow_mut().s.accent = "system".into();
            c.apply_theme(true);
            c.save_soon()
        });
        cb!(c, app, on_open_settings_file, | | {
            c.save_now();
            let p = c.st.borrow().path.to_string_lossy().into_owned();
            c.control.open_path(&p)
        });
        cb!(c, app, on_open_url, |u| c.control.open_url(&u));

        // Dialog and notice
        cb!(c, app, on_dialog_confirmed, | | {
            let f = c.st.borrow_mut().dialog.take();
            c.app().set_dialog_open(false);
            if let Some(f) = f {
                f(c);
            }
        });
        cb!(c, app, on_dialog_cancelled, | | {
            c.st.borrow_mut().dialog = None;
            c.app().set_dialog_open(false)
        });
        cb!(c, app, on_notice_dismissed, | | {
            c.notice_timer.stop();
            c.app().set_notice_visible(false)
        });
        cb!(c, app, on_shortcut, |k| {
            if k.as_str() == "refresh" {
                c.refresh_current();
            }
        });
    }

    fn wire_window(self: &Rc<Self>) {
        let w = Rc::downgrade(self);
        self.main.window().on_close_requested(move || {
            let Some(c) = w.upgrade() else { return CloseRequestResponse::HideWindow };
            // The compact monitor keeps the app open on its own; otherwise only the tray can.
            let to_tray = { let st = c.st.borrow(); (st.s.tray_icon && st.s.close_to_tray) || st.compact_shown };
            if to_tray {
                // Hidden right after, the same way as from the tray, which keeps its placement.
                Timer::single_shot(Duration::ZERO, || with(|c| c.hide_main()));
                CloseRequestResponse::KeepWindowShown
            } else {
                c.quit();
                CloseRequestResponse::KeepWindowShown
            }
        });
        let w = Rc::downgrade(self);
        self.main.window().on_winit_window_event(move |_window, event| {
            let Some(c) = w.upgrade() else { return EventResult::Propagate };
            c.setup_if_pending();
            match event {
                winit::event::WindowEvent::Resized(size) => {
                    let minimized = size.width == 0 && size.height == 0;
                    let was = c.st.borrow().minimized;
                    if minimized != was {
                        c.st.borrow_mut().minimized = minimized;
                        // Changing windows inside the event filter isn't safe: do it right after.
                        Timer::single_shot(Duration::ZERO, move || with(|c| c.minimized_changed(minimized)));
                    }
                    Timer::single_shot(Duration::ZERO, || with(|c| c.note_normal_placement()));
                }
                winit::event::WindowEvent::Moved(_) => {
                    Timer::single_shot(Duration::ZERO, || with(|c| c.note_normal_placement()));
                }
                winit::event::WindowEvent::ScaleFactorChanged { .. } => {
                    Timer::single_shot(Duration::ZERO, || with(|c| c.apply_scale()));
                }
                _ => {}
            }
            EventResult::Propagate
        });
    }

    // ─── Frames ────────────────────────────────────────────────────────────

    fn on_frame(self: &Rc<Self>) {
        {
            let shared = self.collector.shared.clone();
            let Ok(mut sh) = shared.lock() else { return };
            let icons = std::mem::take(&mut sh.icons);
            let mut st = self.st.borrow_mut();
            for (id, px) in icons {
                if let Some(img) = util::image(&px) {
                    st.icons.insert(id, img);
                }
            }
            st.frame = sh.frame.clone();
            st.cost = sh.sample_cost;
            st.error = sh.last_error.clone();
            if sh.info_seq != st.info_seq {
                st.info_seq = sh.info_seq;
                st.info = sh.info.clone();
                st.dev_keys = pages::device_keys(&st.info);
                if !st.dev_keys.contains(&st.device) {
                    st.device = DevKey::Cpu;
                }
                let line = pages::machine_line(&st.info);
                drop(st);
                self.app().set_machine(line.into());
            } else {
                drop(st);
            }
            let st = &mut *self.st.borrow_mut();
            let frame = st.frame.clone();
            st.table.set_frame(&frame);
        }
        let (main, compact) = {
            let st = self.st.borrow();
            (st.main_shown && !st.minimized, st.compact_shown)
        };
        if main {
            self.render_page();
        }
        if compact {
            self.render_compact();
        }
    }

    /// Fills the current page from the latest frame.
    fn render_page(self: &Rc<Self>) {
        let page = self.st.borrow().page;
        match page {
            Page::Overview => {
                let (cards, insights) = {
                    let st = self.st.borrow();
                    let Ok(sh) = self.collector.shared.lock() else { return };
                    (pages::cards(&st.frame, &st.info, &sh.hist, &self.icons), pages::insights(&st.frame.insights))
                };
                util::sync(&self.m.cards, cards);
                util::sync(&self.m.insights, insights);
            }
            Page::Processes => {
                self.update_table();
                self.render_details();
            }
            Page::Performance => self.render_perf(),
            Page::Settings => self.render_diagnostics(),
            Page::Startup | Page::Services => {}
        }
    }

    fn render_perf(self: &Rc<Self>) {
        let (devs, detail, index) = {
            let st = self.st.borrow();
            let Ok(sh) = self.collector.shared.lock() else { return };
            let h: &Histories = &sh.hist;
            let devs = pages::devices(&st.dev_keys, &st.frame, &st.info, h);
            let detail = pages::detail(&st.device, &st.frame, &st.info, h, st.s.cpu_per_core);
            let index = st.dev_keys.iter().position(|k| *k == st.device).unwrap_or(0);
            (devs, detail, index)
        };
        let app = self.app();
        util::sync(&self.m.devices, devs);
        app.set_device(index as i32);
        app.set_perf_title(detail.title.into());
        app.set_perf_subtitle(detail.subtitle.into());
        app.set_perf_is_cpu(detail.is_cpu);
        app.set_perf_is_memory(matches!(self.st.borrow().device, DevKey::Memory));
        util::sync(&self.m.graphs, detail.graphs);
        util::sync(&self.m.cores, detail.cores);
        util::sync(&self.m.live, detail.live);
        util::sync(&self.m.fixed, detail.fixed);
        util::sync(&self.m.segments, detail.segments);
        app.set_perf_note(detail.note.into());
    }

    fn render_diagnostics(&self) {
        let st = self.st.borrow();
        let own = st.frame.processes.iter().find(|p| p.key.pid == std::process::id()).map(|p| p.private_ws);
        let mut parts = vec![format!("Reading counters takes {:.1} ms per refresh", st.cost.as_secs_f64() * 1000.0)];
        if let Some(m) = own {
            parts.push(format!("this app uses {} of memory", format::bytes(m)));
        }
        parts.push(format!(
            "drawing in {}",
            match st.renderer {
                Renderer::Software => "software",
                Renderer::Gpu => "OpenGL",
            }
        ));
        parts.push(if st.elevated { "running as administrator".into() } else { "running without administrator rights".into() });
        let mut text = parts.join(", ");
        text.push('.');
        if let Some(e) = &st.error {
            text.push_str(&format!(" Last problem: {e}"));
        }
        drop(st);
        self.app().set_diagnostics(text.into());
    }

    fn render_compact(&self) {
        let metrics = {
            let st = self.st.borrow();
            let Ok(sh) = self.collector.shared.lock() else { return };
            pages::compact(&st.s.compact.metrics, &st.frame, &st.info, &sh.hist)
        };
        util::sync(&self.m.compact, metrics);
    }

    // ─── Process table ─────────────────────────────────────────────────────

    fn update_table(&self) {
        let app = self.app();
        let mut guard = self.st.borrow_mut();
        let st = &mut *guard;
        st.table.rebuild(&st.s);
        let cx = CellContext { frame: &st.frame, mem_total: st.frame.mem.map_or(st.info.mem.total, |m| m.total), icons: &st.icons, mem_mb: st.s.memory_in_mb };
        let rows = st.table.view_rows(&st.s, &cx);
        let summary = st.table.summary(&st.frame);
        let procs = st.table.selected_procs();
        let count = procs.len() as i32;
        let can_end = procs.iter().any(|p| p.key.pid > 4);
        let has_services = procs.iter().any(|p| !p.services.is_empty());
        let single = st.table.single();
        let has_path = single.is_some_and(|p| p.path().is_some());
        let is_group = st.table.single_is_group();
        let priority = if is_group {
            -1
        } else {
            single.and_then(|p| Priority::from_base(p.base_priority)).and_then(|p| Priority::ALL.iter().position(|x| *x == p)).map_or(-1, |i| i as i32)
        };
        drop(guard);
        self.m.rows.push(rows);
        self.update_columns();
        app.set_process_summary(summary.into());
        app.set_selection_count(count);
        app.set_selection_can_end(can_end);
        app.set_selection_has_services(has_services);
        app.set_selection_has_path(has_path);
        app.set_selection_is_group(is_group);
        app.set_selection_priority(priority);
    }

    /// Pushes column titles, totals and widths, without rebuilding the rows.
    fn update_columns(&self) {
        let app = self.app();
        let (name_col, cols, total) = {
            let st = self.st.borrow();
            st.table.columns(&st.s, &st.frame)
        };
        util::sync(&self.m.columns, cols);
        app.set_name_column(name_col);
        app.set_columns_width(total);
    }

    /// Before any column is resized, Name stops filling the leftover room and keeps the width it
    /// has, so dragging one edge moves only that edge.
    fn pin_name_column(&self, name_w: f32) {
        let pinned = {
            let mut st = self.st.borrow_mut();
            let s = &mut st.s;
            if s.widths.contains_key(&Column::Name) {
                false
            } else {
                s.widths.insert(Column::Name, name_w.clamp(procs::NAME_MIN, 2000.0).round() as u32);
                true
            }
        };
        if pinned {
            self.update_columns();
        }
    }

    fn scroll_to(&self, i: usize) {
        let app = self.app();
        let row = app.get_table_row_height().max(1.0);
        let visible = app.get_table_visible_height();
        let mut top = -app.get_table_scroll();
        let y = i as f32 * row;
        if y < top {
            top = y;
        } else if y + row > top + visible {
            top = y + row - visible;
        }
        app.set_table_scroll(-top.max(0.0));
    }

    fn sort_by(&self, i: i32) {
        {
            let mut st = self.st.borrow_mut();
            let col = if i <= 0 { Column::Name } else { st.s.columns.get(i as usize - 1).copied().unwrap_or(Column::Name) };
            if st.s.sort == col {
                st.s.sort_desc = !st.s.sort_desc;
            } else {
                st.s.sort = col;
                st.s.sort_desc = col.numeric() && col != Column::Pid && col != Column::Started;
            }
        }
        self.save_soon();
        self.update_table();
    }

    fn toggle_column(&self, i: i32) {
        let Some(&col) = Column::OPTIONAL.get(i.max(0) as usize) else { return };
        {
            let mut st = self.st.borrow_mut();
            let s = &mut st.s;
            if s.columns.contains(&col) {
                s.columns.retain(|c| *c != col);
                if s.sort == col {
                    s.sort = if s.columns.contains(&Column::Cpu) { Column::Cpu } else { Column::Name };
                    s.sort_desc = s.sort == Column::Cpu;
                }
            } else {
                s.columns.push(col);
                let order = |c: &Column| Column::OPTIONAL.iter().position(|x| x == c).unwrap_or(usize::MAX);
                s.columns.sort_by_key(order);
            }
        }
        self.push_settings();
        self.update_demand();
        self.save_soon();
        self.update_table();
    }

    fn resize_column(&self, i: i32, w: f32) {
        {
            let mut st = self.st.borrow_mut();
            let col = if i <= 0 { Some(Column::Name) } else { st.s.columns.get(i as usize - 1).copied() };
            if let Some(col) = col {
                let min = if col == Column::Name { procs::NAME_MIN } else { 48.0 };
                st.s.widths.insert(col, w.clamp(min, 1600.0).round() as u32);
            }
        }
        self.save_soon();
        self.update_columns();
    }

    // ─── Details panel ─────────────────────────────────────────────────────

    fn show_details(self: &Rc<Self>) {
        let key = {
            let st = self.st.borrow();
            st.table.single().or_else(|| st.table.selected_procs().into_iter().next()).map(|p| p.key)
        };
        let Some(key) = key else { return };
        self.st.borrow_mut().table.details = Some(key);
        self.app().set_details_open(true);
        self.load_details(key);
        self.render_details();
    }

    /// While the panel is open, it follows the selection.
    fn follow_details(self: &Rc<Self>) {
        let (open, key) = {
            let st = self.st.borrow();
            (st.table.details.is_some(), st.table.single().map(|p| p.key))
        };
        if open {
            if let Some(k) = key {
                let same = self.st.borrow().table.details == Some(k);
                if !same {
                    self.st.borrow_mut().table.details = Some(k);
                    self.load_details(k);
                    self.render_details();
                }
            }
        }
    }

    fn load_details(&self, key: ProcKey) {
        let control = self.control.clone();
        std::thread::spawn(move || {
            let d = control.process_details(key);
            post(move |c| {
                if let Some(d) = d {
                    c.st.borrow_mut().table.details_static = Some((key, d));
                }
                c.render_details();
            });
        });
    }

    fn render_details(&self) {
        let data = {
            let st = self.st.borrow();
            let Some(key) = st.table.details else { return };
            let d = st.table.details_static.as_ref().filter(|(k, _)| *k == key).map(|(_, d)| d);
            match st.table.procs.iter().find(|p| p.key == key) {
                Some(p) => {
                    let list = procs::describe(p, d, st.s.friendly_names);
                    let title = tm_core::table::display_name(p, st.s.friendly_names).into_owned();
                    let sub = format!("PID {}{}", p.key.pid, p.user().map(|u| format!(" · {u}")).unwrap_or_default());
                    let icon = p.meta.as_ref().and_then(|m| m.icon).and_then(|i| st.icons.get(&i)).cloned();
                    Some((title, sub, icon, list))
                }
                None => Some(("Process exited".into(), format!("PID {} is no longer running", key.pid), None, Vec::new())),
            }
        };
        let Some((title, sub, icon, list)) = data else { return };
        let app = self.app();
        app.set_details_title(title.into());
        app.set_details_subtitle(sub.into());
        app.set_details_has_icon(icon.is_some());
        app.set_details_icon(icon.unwrap_or_default());
        util::sync(&self.m.details, list.into_iter().map(|(l, v)| stat(l, v)).collect());
    }

    // ─── Ending processes ──────────────────────────────────────────────────

    fn targets(&self) -> Vec<Target1> {
        let st = self.st.borrow();
        st.table
            .selected_procs()
            .into_iter()
            .filter(|p| p.key.pid != 0)
            .map(|p| Target1 { key: p.key, name: tm_core::table::display_name(p, st.s.friendly_names).into_owned(), has_window: p.has_window })
            .collect()
    }

    fn end_selected(self: &Rc<Self>, force: bool) {
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        let mut allowed = Vec::new();
        let mut refused: Vec<(String, String)> = Vec::new();
        let mut cautions: Vec<String> = Vec::new();
        for t in targets {
            match self.control.protection(t.key) {
                Protection::Refused(why) => refused.push((t.name.clone(), why)),
                Protection::Caution(why) => {
                    if !cautions.contains(&why) {
                        cautions.push(why);
                    }
                    allowed.push(t);
                }
                Protection::Normal => allowed.push(t),
            }
        }
        if allowed.is_empty() {
            let (name, why) = &refused[0];
            self.notice(&format!("{name} can't be ended. {why}"), 2);
            return;
        }
        let names: Vec<String> = allowed.iter().map(|t| t.name.clone()).collect();
        let mut unique = names.clone();
        unique.dedup();
        let one = if unique.len() == 1 { Some(unique[0].clone()) } else { None };
        let windowed = allowed.iter().any(|t| t.has_window);
        let graceful = !force && windowed;
        let (title, mut body, confirm, danger) = if graceful {
            let mut b = if one.is_some() {
                "It's asked to close, as if you clicked its close button. If it has unsaved work, it may ask you about it first.".to_string()
            } else {
                "Each app is asked to close, as if you clicked its close button. An app with unsaved work may ask you about it first.".to_string()
            };
            if allowed.iter().any(|t| !t.has_window) {
                b.push_str(" Its background processes usually close with it; any that don't keep running.");
            }
            (
                match &one {
                    Some(n) => format!("End {n}?"),
                    None => format!("End {} tasks?", allowed.len()),
                },
                b,
                "End task",
                false,
            )
        } else if !force {
            (
                match &one {
                    Some(n) => format!("End {n}?"),
                    None => format!("End {} processes?", allowed.len()),
                },
                "There's no window to close, so it's stopped right away. Anything it hasn't saved is lost.".to_string(),
                "End process",
                true,
            )
        } else {
            (
                match &one {
                    Some(n) => format!("Force end {n}?"),
                    None => format!("Force end {} processes?", allowed.len()),
                },
                "It's stopped right away, with no chance to save. Anything it hasn't saved is lost.".to_string(),
                "Force end",
                true,
            )
        };
        for c in &cautions {
            body.push_str("\n\n");
            body.push_str(c);
        }
        if !refused.is_empty() {
            body.push_str(&format!("\n\nSkipped, because Windows needs them: {}.", util::names(&refused.iter().map(|r| r.0.clone()).collect::<Vec<_>>())));
        }
        let mut lines: Vec<String> = allowed.iter().take(8).map(|t| format!("{}  ·  PID {}", t.name, t.key.pid)).collect();
        if allowed.len() > 8 {
            lines.push(format!("and {} more", allowed.len() - 8));
        }
        self.ask(&title, &body, lines, confirm, danger, false, move |c| c.do_end(allowed, graceful, false));
    }

    fn do_end(self: &Rc<Self>, targets: Vec<Target1>, graceful: bool, elevate: bool) {
        let control = self.control.clone();
        std::thread::spawn(move || {
            let mut asked = Vec::new();
            let mut ended = Vec::new();
            let mut no_windows = Vec::new();
            let mut needs_admin = Vec::new();
            let mut failed = Vec::new();
            for t in targets {
                if graceful {
                    if !t.has_window {
                        continue;
                    }
                    match control.end_task(t.key) {
                        Ok(EndOutcome::CloseRequested) => asked.push(t),
                        Ok(EndOutcome::NoWindows) => no_windows.push(t),
                        Err(ActionError::Gone) => ended.push(t),
                        Err(e) => failed.push((t, e.message())),
                    }
                } else {
                    match control.terminate(t.key, elevate) {
                        Ok(()) | Err(ActionError::Gone) => ended.push(t),
                        Err(ActionError::NeedsElevation(_)) if !elevate => needs_admin.push(t),
                        Err(ActionError::Cancelled) => {}
                        Err(e) => failed.push((t, e.message())),
                    }
                }
            }
            post(move |c| c.end_done(asked, ended, no_windows, needs_admin, failed));
        });
    }

    fn end_done(self: &Rc<Self>, asked: Vec<Target1>, ended: Vec<Target1>, no_windows: Vec<Target1>, needs_admin: Vec<Target1>, failed: Vec<(Target1, String)>) {
        let name_list = |v: &[Target1]| {
            let mut n: Vec<String> = v.iter().map(|t| t.name.clone()).collect();
            n.dedup();
            util::names(&n)
        };
        if !failed.is_empty() {
            let (t, msg) = &failed[0];
            self.notice(&format!("Couldn't end {}. {msg}", t.name), 2);
        } else if !ended.is_empty() {
            self.notice(&format!("Ended {}.", name_list(&ended)), 0);
        } else if !asked.is_empty() {
            self.notice(&format!("Asked {} to close.", name_list(&asked)), 0);
        }
        if !asked.is_empty() {
            // If it's still there a little later, it's probably waiting on the user.
            let keys: Vec<(ProcKey, String)> = asked.iter().map(|t| (t.key, t.name.clone())).collect();
            Timer::single_shot(Duration::from_secs(6), move || {
                with(|c| {
                    let still: Vec<String> = {
                        let st = c.st.borrow();
                        keys.iter().filter(|(k, _)| st.table.procs.iter().any(|p| p.key == *k)).map(|(_, n)| n.clone()).collect()
                    };
                    if let Some(n) = still.first() {
                        c.notice(&format!("{n} is still open. It may be waiting for you to save or confirm something. Force end stops it right away."), 1);
                    }
                })
            });
        }
        if !needs_admin.is_empty() {
            let names = name_list(&needs_admin);
            self.ask(
                "Administrator rights needed",
                &format!("Windows only lets an administrator end {names}. Continue, and Windows will ask you to allow it."),
                Vec::new(),
                "Continue",
                true,
                true,
                move |c| c.do_end(needs_admin, false, true),
            );
        } else if !no_windows.is_empty() {
            let names = name_list(&no_windows);
            self.ask(
                &format!("{names} has no window to close"),
                "It can only be stopped right away, with no chance to save. Anything it hasn't saved is lost.",
                Vec::new(),
                "Force end",
                true,
                false,
                move |c| c.do_end(no_windows, false, false),
            );
        }
        self.collector.send(Command::RefreshNow);
    }

    fn set_priority(self: &Rc<Self>, i: i32) {
        let Some(&p) = Priority::ALL.get(i.max(0) as usize) else { return };
        let target = {
            let st = self.st.borrow();
            if st.table.single_is_group() {
                None
            } else {
                st.table.single().map(|x| Target1 { key: x.key, name: tm_core::table::display_name(x, st.s.friendly_names).into_owned(), has_window: x.has_window })
            }
        };
        let Some(t) = target else { return };
        if p == Priority::Realtime {
            let body = "A realtime process runs ahead of nearly everything else, including mouse and keyboard input. If it gets busy, the PC can stop responding. Windows only allows this with administrator rights.";
            self.ask(&format!("Set {} to Realtime?", t.name), body, Vec::new(), "Set Realtime", true, !self.st.borrow().elevated, move |c| {
                let elevate = !c.st.borrow().elevated;
                c.do_priority(t, p, elevate)
            });
        } else {
            self.do_priority(t, p, false);
        }
    }

    fn do_priority(self: &Rc<Self>, t: Target1, p: Priority, elevate: bool) {
        let control = self.control.clone();
        std::thread::spawn(move || {
            let r = control.set_priority(t.key, p, elevate);
            post(move |c| match r {
                Ok(()) => {
                    c.notice(&format!("{} now runs at {} priority.", t.name, p.label().to_lowercase()), 0);
                    c.collector.send(Command::RefreshNow);
                }
                Err(ActionError::NeedsElevation(_)) if !elevate => {
                    c.ask(
                        "Administrator rights needed",
                        &format!("Windows only lets an administrator change the priority of {}. Continue, and Windows will ask you to allow it.", t.name),
                        Vec::new(),
                        "Continue",
                        false,
                        true,
                        move |c| c.do_priority(t, p, true),
                    );
                }
                Err(ActionError::Cancelled) => {}
                Err(e) => c.notice(&format!("Couldn't change the priority of {}. {}", t.name, e.message()), 2),
            });
        });
    }

    fn copy(self: &Rc<Self>, what: i32) {
        let (text, keys) = {
            let st = self.st.borrow();
            let procs = st.table.selected_procs();
            if procs.is_empty() {
                return;
            }
            let friendly = st.s.friendly_names;
            let text: String = match what {
                0 => procs.iter().map(|p| tm_core::table::display_name(p, friendly).into_owned()).collect::<Vec<_>>().join("\n"),
                1 => procs.iter().map(|p| p.key.pid.to_string()).collect::<Vec<_>>().join("\n"),
                2 => procs.iter().filter_map(|p| p.path().map(str::to_string)).collect::<Vec<_>>().join("\n"),
                3 => String::new(),
                _ => procs
                    .iter()
                    .map(|p| {
                        let d = st.table.details_static.as_ref().filter(|(k, _)| *k == p.key).map(|(_, d)| d);
                        procs::describe(p, d, friendly).into_iter().map(|(l, v)| format!("{l}: {v}")).collect::<Vec<_>>().join("\n")
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            };
            (text, procs.iter().take(50).map(|p| p.key).collect::<Vec<_>>())
        };
        if what == 3 {
            let control = self.control.clone();
            std::thread::spawn(move || {
                let lines: Vec<String> = keys.iter().filter_map(|k| control.process_details(*k).and_then(|d| d.command_line)).collect();
                post(move |c| {
                    if lines.is_empty() {
                        c.notice("The command line couldn't be read. Windows may need administrator rights for this process.", 1);
                    } else {
                        c.control.copy_text(&lines.join("\n"));
                        c.notice("Copied.", 0);
                    }
                });
            });
            return;
        }
        if !text.is_empty() {
            self.control.copy_text(&text);
            self.notice("Copied.", 0);
        }
    }

    fn go_to_services(self: &Rc<Self>) {
        let pid = self.st.borrow().table.selected_procs().into_iter().find(|p| !p.services.is_empty()).map(|p| p.key.pid);
        let Some(pid) = pid else { return };
        {
            let mut st = self.st.borrow_mut();
            st.services.search = format!("pid:{pid}");
            st.services.filter = 0;
            st.services.apply_filter();
        }
        self.app().set_services_search(format!("pid:{pid}").into());
        self.app().set_services_filter(0);
        self.navigate(Page::Services);
    }

    // ─── Startup page ──────────────────────────────────────────────────────

    fn load_startup(self: &Rc<Self>, visible: bool) {
        {
            let mut st = self.st.borrow_mut();
            if st.startup.loading {
                return;
            }
            st.startup.loading = true;
        }
        if visible {
            self.app().set_startup_loading(self.st.borrow().startup.entries.is_empty());
        }
        let control = self.control.clone();
        std::thread::spawn(move || {
            let list = control.startup_entries();
            post(move |c| {
                let list: Vec<(StartupEntry, Option<Image>)> = list
                    .into_iter()
                    .map(|mut e| {
                        let img = e.icon.take().and_then(|px| util::image(&px));
                        (e, img)
                    })
                    .collect();
                let count = {
                    let mut st = c.st.borrow_mut();
                    st.startup.set(list);
                    st.startup.enabled_count()
                };
                c.collector.send(Command::StartupCount(Some(count)));
                c.app().set_startup_loading(false);
                c.render_startup(true);
                // The first read runs a few seconds after launch, which may be at sign-in with no
                // window open: hand back what it took once it's done.
                c.release_when_idle();
            });
        });
    }

    /// Column titles, sort arrows and widths of the startup and services tables.
    fn push_table_columns(&self) {
        let app = self.app();
        let st = self.st.borrow();
        let (cols, fixed) = lists::columns(&lists::STARTUP_COLS, &st.startup.layout, lists::STARTUP_SORT);
        util::sync(&self.m.startup_cols, cols);
        app.set_startup_fixed_width(fixed);
        let (cols, fixed) = lists::columns(&lists::SERVICE_COLS, &st.services.layout, lists::SERVICE_SORT);
        util::sync(&self.m.service_cols, cols);
        app.set_service_fixed_width(fixed);
    }

    /// A table was sorted or a column resized: show it and keep it for next time.
    fn table_layout_changed(&self) {
        {
            let mut st = self.st.borrow_mut();
            st.s.startup_table = st.startup.layout.clone();
            st.s.services_table = st.services.layout.clone();
        }
        self.push_table_columns();
        self.save_soon();
    }

    fn render_startup(&self, rebuild: bool) {
        let app = self.app();
        let st = self.st.borrow();
        let rows = st.startup.rows();
        if rebuild {
            // Rebuilt from scratch so every switch shows the stored state, not the last click.
            self.m.startup.set_vec(rows);
        } else {
            util::sync(&self.m.startup, rows);
        }
        app.set_startup_summary(st.startup.summary().into());
        app.set_startup_selected(st.startup.selected_row());
        if let Some(e) = st.startup.selected_entry() {
            app.set_startup_name(e.name.as_str().into());
            app.set_startup_command(e.command.as_str().into());
            app.set_startup_path(e.path.as_deref().unwrap_or("").into());
            app.set_startup_how(e.how.as_str().into());
            app.set_startup_publisher(e.publisher.as_deref().unwrap_or("Unknown publisher").into());
            app.set_startup_source(e.source.as_str().into());
            app.set_startup_enabled(e.enabled);
            app.set_startup_missing(e.missing);
            app.set_startup_needs_admin(e.needs_admin && !st.elevated);
        }
    }

    fn startup_toggle(self: &Rc<Self>, row: i32, on: bool) {
        let entry = {
            let st = self.st.borrow();
            st.startup.entry_at(row.max(0) as usize).map(|e| (e.id.clone(), e.name.clone(), e.needs_admin && !st.elevated))
        };
        let Some((id, name, admin)) = entry else { return };
        self.do_startup(id, name, on, admin);
    }

    fn do_startup(self: &Rc<Self>, id: String, name: String, on: bool, elevate: bool) {
        let control = self.control.clone();
        std::thread::spawn(move || {
            let r = control.set_startup_enabled(&id, on, elevate);
            post(move |c| {
                match r {
                    Ok(()) => {
                        let count = {
                            let mut st = c.st.borrow_mut();
                            st.startup.set_enabled(&id, on);
                            st.startup.enabled_count()
                        };
                        c.collector.send(Command::StartupCount(Some(count)));
                        c.notice(&if on { format!("{name} will start when you sign in.") } else { format!("{name} won't start when you sign in. Turn it back on any time.") }, 0);
                    }
                    Err(ActionError::NeedsElevation(_)) if !elevate => {
                        let (id2, name2) = (id.clone(), name.clone());
                        c.ask(
                            "Administrator rights needed",
                            &format!("{name} starts for everyone who uses this PC, so only an administrator can change it. Continue, and Windows will ask you to allow it."),
                            Vec::new(),
                            "Continue",
                            false,
                            true,
                            move |c| c.do_startup(id2, name2, on, true),
                        );
                    }
                    Err(ActionError::Cancelled) => {}
                    Err(e) => c.notice(&format!("Couldn't change {name}. {}", e.message()), 2),
                }
                c.render_startup(true);
            });
        });
    }

    // ─── Services page ─────────────────────────────────────────────────────

    fn load_services(self: &Rc<Self>, full: bool) {
        {
            let mut st = self.st.borrow_mut();
            if st.services.loading {
                return;
            }
            st.services.loading = true;
        }
        self.app().set_services_loading(true);
        let control = self.control.clone();
        std::thread::spawn(move || {
            let r = control.services(full);
            post(move |c| {
                {
                    let mut st = c.st.borrow_mut();
                    st.services.loading = false;
                    match r {
                        Ok(list) if full => st.services.set(list),
                        Ok(list) => st.services.merge_states(list),
                        Err(_) => {}
                    }
                }
                c.app().set_services_loading(false);
                c.render_services();
            });
        });
    }

    fn render_services(&self) {
        let app = self.app();
        let st = self.st.borrow();
        util::sync(&self.m.services, st.services.rows());
        app.set_services_summary(st.services.summary().into());
        app.set_service_selected(st.services.selected_row());
        app.set_service_busy(st.services.busy.is_some());
        if let Some(s) = st.services.selected_info() {
            app.set_service_name(s.name.as_str().into());
            app.set_service_display(s.display.as_str().into());
            app.set_service_description(s.description.as_deref().unwrap_or("").into());
            app.set_service_status(s.state.label().into());
            app.set_service_running(s.state.is_running());
            app.set_service_can_stop(s.can_stop);
            app.set_service_start_choosable(StartType::CHOOSABLE.contains(&s.start));
            app.set_service_start_index(StartType::CHOOSABLE.iter().position(|t| *t == s.start).map_or(-1, |i| i as i32));
            app.set_service_warning(lists::warning(s).into());
            util::sync(&self.m.service_facts, lists::facts(s));
        }
    }

    fn service_command(self: &Rc<Self>, action: ServiceAction) {
        let info = self.st.borrow().services.selected_info().cloned();
        let Some(s) = info else { return };
        let important = lists::is_important(&s.name);
        match action {
            ServiceAction::Start => self.do_service(s.name, s.display, action, false),
            ServiceAction::Stop | ServiceAction::Restart => {
                let deps = self.control.service_dependents(&s.name);
                let mut body = if action == ServiceAction::Stop {
                    "Anything that uses it stops working until it's started again.".to_string()
                } else {
                    "It stops and starts again. Anything that uses it is interrupted for a moment.".to_string()
                };
                if !deps.is_empty() {
                    let names: Vec<String> = deps.iter().map(|(_, d)| d.clone()).collect();
                    body.push_str(&format!(
                        " These running services need it and {} too: {}.",
                        if action == ServiceAction::Stop { "stop" } else { "restart" },
                        util::names(&names)
                    ));
                }
                if important {
                    body.push_str("\n\nWindows relies on this service. Parts of Windows may stop working until it runs again.");
                }
                let title = format!("{} {}?", if action == ServiceAction::Stop { "Stop" } else { "Restart" }, s.display);
                let confirm = if action == ServiceAction::Stop { "Stop" } else { "Restart" };
                let (name, display) = (s.name.clone(), s.display.clone());
                self.ask(&title, &body, Vec::new(), confirm, action == ServiceAction::Stop, false, move |c| c.do_service(name, display, action, false));
            }
            ServiceAction::SetStart(t) => {
                if t == StartType::Disabled {
                    let (name, display) = (s.name.clone(), s.display.clone());
                    let mut body = "It won't run, even when Windows or an app asks for it, until you change this back.".to_string();
                    if important {
                        body.push_str("\n\nWindows relies on this service. Parts of Windows may stop working.");
                    }
                    self.ask(&format!("Disable {}?", s.display), &body, Vec::new(), "Disable", true, false, move |c| c.do_service(name, display, action, false));
                } else {
                    self.do_service(s.name, s.display, action, false);
                }
            }
        }
    }

    fn do_service(self: &Rc<Self>, name: String, display: String, action: ServiceAction, elevate: bool) {
        self.st.borrow_mut().services.busy = Some(name.clone());
        self.app().set_service_busy(true);
        let control = self.control.clone();
        std::thread::spawn(move || {
            let r = control.service_action(&name, action, elevate);
            post(move |c| {
                c.st.borrow_mut().services.busy = None;
                c.app().set_service_busy(false);
                match r {
                    Ok(()) => {
                        let what = match action {
                            ServiceAction::Start => "started",
                            ServiceAction::Stop => "stopped",
                            ServiceAction::Restart => "restarted",
                            ServiceAction::SetStart(_) => "updated",
                        };
                        c.notice(&format!("{display} {what}."), 0);
                    }
                    Err(ActionError::NeedsElevation(_)) if !elevate => {
                        let (n2, d2) = (name.clone(), display.clone());
                        c.ask(
                            "Administrator rights needed",
                            &format!("Windows only lets an administrator change {display}. Continue, and Windows will ask you to allow it."),
                            Vec::new(),
                            "Continue",
                            false,
                            true,
                            move |c| c.do_service(n2, d2, action, true),
                        );
                    }
                    Err(ActionError::Cancelled) => {}
                    Err(e) => c.notice(&format!("Couldn't change {display}. {}", e.message()), 2),
                }
                // Configuration changes need the full read; state changes only the quick one.
                c.load_services(matches!(action, ServiceAction::SetStart(_)));
                c.render_services();
            });
        });
    }

    // ─── Memory ────────────────────────────────────────────────────────────

    fn clear_standby(self: &Rc<Self>) {
        let elevated = self.st.borrow().elevated;
        let admin = if elevated { "" } else { " Windows will ask for administrator rights." };
        self.ask(
            "Clear standby memory?",
            &format!(
                "Standby memory holds files and programs you used recently, so they open quickly next time. Windows gives it to any program that needs memory, so it isn't wasted and doesn't slow anything down.\n\nClearing it shows that memory as free straight away. The cost: what you open next loads from disk again, so things can feel slower for a while until Windows fills it back up. It rarely helps, except to measure something from a cold start.{admin}"
            ),
            Vec::new(),
            "Clear standby",
            false,
            !elevated,
            |c| {
                let control = c.control.clone();
                std::thread::spawn(move || {
                    let r = control.clear_standby(true);
                    post(move |c| match r {
                        Ok(()) => c.notice("Standby memory cleared.", 0),
                        Err(ActionError::Cancelled) => {}
                        Err(e) => c.notice(&format!("Couldn't clear standby memory. {}", e.message()), 2),
                    });
                });
            },
        );
    }

    fn service_go_to_process(self: &Rc<Self>) {
        let pid = self.st.borrow().services.selected_info().map(|s| s.pid).filter(|p| *p != 0);
        let Some(pid) = pid else { return };
        let key = self.st.borrow().table.procs.iter().find(|p| p.key.pid == pid).map(|p| p.key);
        self.navigate(Page::Processes);
        if let Some(key) = key {
            self.app().set_search(SharedString::new());
            {
                let mut st = self.st.borrow_mut();
                let st = &mut *st;
                st.table.reveal(key, &st.s);
            }
            self.update_table();
            let i = self.st.borrow().table.index_of(&RowId::Proc(key));
            if let Some(i) = i {
                self.scroll_to(i);
            }
        }
    }

    // ─── Navigation ────────────────────────────────────────────────────────

    fn navigate(self: &Rc<Self>, page: Page) {
        let prev = {
            let mut st = self.st.borrow_mut();
            std::mem::replace(&mut st.page, page)
        };
        self.app().set_page(page.index());
        self.update_demand();
        if page == Page::Processes && prev != Page::Processes {
            // Windows and services are only read for this page: get them now rather than next tick.
            self.collector.send(Command::RefreshNow);
        }
        match page {
            Page::Startup => {
                let need = { let st = self.st.borrow(); !st.startup.loaded };
                if need {
                    self.load_startup(true);
                } else {
                    self.render_startup(true);
                }
            }
            Page::Services => {
                let loaded = self.st.borrow().services.loaded;
                self.load_services(!loaded);
                self.render_services();
                let w = Rc::downgrade(self);
                self.services_timer.start(TimerMode::Repeated, Duration::from_secs(3), move || {
                    if let Some(c) = w.upgrade() {
                        let ok = { let st = c.st.borrow(); st.page == Page::Services && st.main_shown && !st.minimized && st.services.loaded };
                        if ok {
                            c.load_services(false);
                        }
                    }
                });
            }
            _ => {}
        }
        if page != Page::Services {
            self.services_timer.stop();
        }
        self.render_page();
    }

    fn refresh_current(self: &Rc<Self>) {
        let page = self.st.borrow().page;
        match page {
            Page::Startup => self.load_startup(true),
            Page::Services => self.load_services(true),
            _ => self.collector.send(Command::RefreshNow),
        }
    }

    fn card_clicked(self: &Rc<Self>, key: &str) {
        let dev = {
            let st = self.st.borrow();
            match key {
                "cpu" | "cpu-temp" => Some(DevKey::Cpu),
                "mem" => Some(DevKey::Memory),
                "gpu" | "gpu-temp" => st.info.gpus.first().map(|g| DevKey::Gpu(g.id)),
                "disk" => st
                    .frame
                    .disks
                    .iter()
                    .filter(|d| d.active.is_some())
                    .max_by(|a, b| a.active.unwrap_or(0.0).total_cmp(&b.active.unwrap_or(0.0)))
                    .map(|d| DevKey::Disk(d.id))
                    .or_else(|| st.info.disks.first().map(|d| DevKey::Disk(d.id))),
                "net" => st
                    .frame
                    .nets
                    .iter()
                    .max_by(|a, b| a.recv.unwrap_or(0.0).total_cmp(&b.recv.unwrap_or(0.0)))
                    .map(|n| DevKey::Net(n.id))
                    .or_else(|| st.info.nets.first().map(|n| DevKey::Net(n.id))),
                _ => None,
            }
        };
        if let Some(d) = dev {
            self.st.borrow_mut().device = d;
            self.navigate(Page::Performance);
        }
    }

    fn insight_action(self: &Rc<Self>, id: &str) {
        let target = self.st.borrow().frame.insights.iter().find(|n| n.id == id).map(|n| n.target.clone());
        let Some(t) = target else { return };
        match t {
            Target::Cpu => self.go_device(DevKey::Cpu),
            Target::Memory => self.go_device(DevKey::Memory),
            Target::Disk(d) => self.go_device(DevKey::Disk(d)),
            Target::Gpu(g) => self.go_device(DevKey::Gpu(g)),
            Target::Startup => self.navigate(Page::Startup),
            Target::Process(key) => {
                self.navigate(Page::Processes);
                self.app().set_search(SharedString::new());
                {
                    let mut st = self.st.borrow_mut();
                    let st = &mut *st;
                    st.table.reveal(key, &st.s);
                }
                self.update_table();
                let i = self.st.borrow().table.index_of(&RowId::Proc(key));
                if let Some(i) = i {
                    self.scroll_to(i);
                }
            }
        }
    }

    fn go_device(self: &Rc<Self>, d: DevKey) {
        self.st.borrow_mut().device = d;
        self.navigate(Page::Performance);
    }

    // ─── Settings ──────────────────────────────────────────────────────────

    fn setting_int(self: &Rc<Self>, key: &str, v: i32) {
        let i = v.max(0) as usize;
        match key {
            "theme" => {
                self.st.borrow_mut().s.theme = [ThemeChoice::System, ThemeChoice::Dark, ThemeChoice::Light][i.min(2)];
                self.apply_theme(true);
            }
            "motion" => {
                self.st.borrow_mut().s.motion = [Motion::System, Motion::On, Motion::Off][i.min(2)];
                self.apply_theme(true);
            }
            "scale" => {
                self.st.borrow_mut().s.scale = SCALES[i.min(SCALES.len() - 1)];
                self.apply_scale();
            }
            "refresh" => {
                self.st.borrow_mut().s.refresh_ms = REFRESH_CHOICES[i.min(3)];
                self.send_timing();
            }
            "history" => {
                self.st.borrow_mut().s.history_secs = HISTORY_CHOICES[i.min(3)];
                self.send_timing();
            }
            "background" => {
                self.st.borrow_mut().s.background = [Background::Full, Background::History, Background::Pause][i.min(2)];
                self.update_demand();
            }
            "renderer" => {
                let restart = {
                    let mut st = self.st.borrow_mut();
                    st.s.renderer = if i == 1 { Renderer::Gpu } else { Renderer::Software };
                    st.s.renderer != st.renderer
                };
                self.app().set_renderer_restart(restart);
            }
            "start-page" => self.st.borrow_mut().s.start_page = Page::from_index(v.min(4)),
            "compact-metric" => {
                {
                    let mut st = self.st.borrow_mut();
                    let m = METRICS[i.min(METRICS.len() - 1)];
                    let list = &mut st.s.compact.metrics;
                    if list.contains(&m) {
                        if list.len() > 1 {
                            list.retain(|x| *x != m);
                        }
                    } else {
                        list.push(m);
                        list.sort_by_key(|x| METRICS.iter().position(|y| y == x));
                    }
                }
                self.push_settings();
                self.update_demand();
                self.render_compact();
            }
            _ => {}
        }
        self.save_soon();
    }

    fn setting_bool(self: &Rc<Self>, key: &str, v: bool) {
        match key {
            "sidebar" => {
                self.st.borrow_mut().s.sidebar_compact = v;
                self.app().set_sidebar_compact(v);
            }
            "reduced" => {
                self.st.borrow_mut().s.reduced = v;
                self.send_timing();
                self.update_demand();
            }
            "autostart" => {
                if let Err(e) = self.control.set_autostart(v) {
                    self.notice(&format!("Couldn't change that. {}", e.message()), 2);
                }
                self.app().set_autostart(self.control.autostart());
            }
            "remember-window" => self.st.borrow_mut().s.remember_window = v,
            "always-on-top" => {
                self.st.borrow_mut().s.always_on_top = v;
                self.app().set_always_on_top(v);
            }
            "tray" => {
                self.st.borrow_mut().s.tray_icon = v;
                self.set_tray(v);
                let hidden = !self.st.borrow().main_shown;
                if !v && hidden {
                    self.show_main();
                }
            }
            "close-to-tray" => self.st.borrow_mut().s.close_to_tray = v,
            "memory-in-mb" => {
                self.st.borrow_mut().s.memory_in_mb = v;
                self.app().set_memory_in_mb(v);
                self.update_table();
            }
            "minimize-to-tray" => self.st.borrow_mut().s.minimize_to_tray = v,
            "click-through" => {
                self.st.borrow_mut().s.compact.click_through = v;
                self.apply_click_through();
            }
            _ => {}
        }
        self.save_soon();
    }

    fn send_timing(&self) {
        let (ms, len) = {
            let st = self.st.borrow();
            (st.s.effective_refresh_ms(), st.s.history_len())
        };
        self.collector.send(Command::Interval(Duration::from_millis(ms as u64)));
        self.collector.send(Command::HistoryLen(len));
    }

    fn update_demand(&self) {
        let d = {
            let st = self.st.borrow();
            compute_demand(&st.s, st.page, st.main_shown && !st.minimized, st.compact_shown, matches!(st.device, DevKey::Gpu(_)))
        };
        let changed = {
            let mut st = self.st.borrow_mut();
            let changed = st.demand != Some(d);
            st.demand = Some(d);
            changed
        };
        if changed {
            self.collector.send(Command::Demand(d));
        }
    }

    /// Applies the theme and accent to every window. With `force` false, only when something changed
    /// (the system theme or accent, polled).
    fn apply_theme(&self, force: bool) {
        let (dark, accent, motion, hc, system_accent) = {
            let st = self.st.borrow();
            let s = &st.s;
            let dark = match s.theme {
                ThemeChoice::System => self.shell.system_dark(),
                ThemeChoice::Dark => true,
                ThemeChoice::Light => false,
            };
            let bg = if dark { DARK_BG } else { LIGHT_BG };
            let base = if s.accent == "system" { self.shell.system_accent().map(|(d, l)| if dark { d } else { l }) } else { parse_hex(&s.accent) };
            let base = base.unwrap_or(if dark { (0x60, 0xcd, 0xff) } else { (0x00, 0x5f, 0xb8) });
            let accent = util::readable_accent(base, bg, dark);
            let motion = match s.motion {
                Motion::System => self.shell.animations(),
                Motion::On => true,
                Motion::Off => false,
            };
            (dark, accent, motion, self.shell.high_contrast(), s.accent == "system")
        };
        let now = (dark, accent, motion, hc);
        let changed = self.st.borrow().theme != Some(now);
        if !changed && !force {
            return;
        }
        self.st.borrow_mut().theme = Some(now);
        self.shell.set_menu_theme(dark);
        let on = util::on_accent(accent);
        let set = |t: Theme| {
            t.set_dark(dark);
            t.set_accent(util::color(accent));
            t.set_on_accent(util::color(on));
            t.set_motion(motion);
            t.set_high_contrast(hc);
        };
        set(self.main.global::<Theme>());
        if let Some(c) = self.compact.borrow().as_ref() {
            set(c.global::<Theme>());
        }
        let picked = self.st.borrow().s.accent.clone();
        util::sync(&self.m.swatches, SWATCHES.iter().map(|sw| Swatch { color: util::color(*sw), selected: !system_accent && util::hex(*sw) == picked }).collect());
        self.app().set_accent_system(system_accent);
        if changed {
            self.style_windows();
        }
    }

    fn style_windows(&self) {
        let (dark, main, compact) = {
            let st = self.st.borrow();
            (st.theme.map_or(true, |t| t.0), st.main_shown, st.compact_shown)
        };
        if main {
            self.shell.style_main(self.main.window(), dark, if dark { DARK_BG } else { LIGHT_BG });
        }
        if compact {
            if let Some(c) = self.compact.borrow().as_ref() {
                self.shell.style_compact(c.window(), dark);
            }
        }
    }

    /// The interface size: Windows' display scaling times the user's choice.
    fn apply_scale(&self) {
        let user = self.st.borrow().s.scale;
        let windows: Vec<slint::Window> = Vec::new();
        drop(windows);
        let apply = |w: &slint::Window| {
            if let Some(os) = w.with_winit_window(|ww| ww.scale_factor() as f32) {
                let want = os * user;
                if (w.scale_factor() - want).abs() > 0.001 {
                    w.dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: want });
                }
            }
        };
        apply(self.main.window());
        if let Some(c) = self.compact.borrow().as_ref() {
            apply(c.window());
        }
    }

    // ─── Windows ───────────────────────────────────────────────────────────

    fn restore_placement(&self) {
        let p = {
            let st = self.st.borrow();
            if !st.s.remember_window { None } else { st.s.window }
        };
        if let Some(p) = p {
            if self.shell.on_screen(p.x, p.y, p.width, p.height) {
                let w = self.main.window();
                w.set_position(PhysicalPosition::new(p.x, p.y));
                w.set_size(PhysicalSize::new(p.width, p.height));
            }
        }
    }

    /// Remembers the main window's size and position while it's neither maximized nor minimized.
    /// Runs after the events that moved or resized it: when a window is maximized, Windows reports
    /// the move before the resize, and only after both does the window say it's maximized.
    fn note_normal_placement(&self) {
        let w = self.main.window();
        let Some((maximized, minimized)) = w.with_winit_window(|ww| (ww.is_maximized(), ww.is_minimized().unwrap_or(false))) else { return };
        let size = w.size();
        if maximized || minimized || size.width == 0 || size.height == 0 {
            return;
        }
        let pos = w.position();
        self.st.borrow_mut().normal = Some((pos.x, pos.y, size.width, size.height));
    }

    fn capture_placement(&self) {
        let w = self.main.window();
        // Slint's flag, which minimizing leaves alone (it ignores the zero-size resize), so a
        // maximized window closed while minimized still comes back maximized.
        let maximized = w.is_maximized();
        let mut st = self.st.borrow_mut();
        if !st.s.remember_window {
            return;
        }
        if let Some((x, y, width, height)) = st.normal {
            if width >= 400 && height >= 300 {
                st.s.window = Some(WindowPlacement { x, y, width, height, maximized });
            }
        }
    }

    fn show_main(self: &Rc<Self>) {
        let first = !self.st.borrow().main_shown;
        let w = self.main.window();
        if w.is_minimized() {
            w.set_minimized(false);
        }
        let _ = self.main.show();
        {
            let mut st = self.st.borrow_mut();
            st.main_shown = true;
            st.minimized = false;
            st.setup_pending = true;
        }
        self.window_setup();
        if first {
            let maximized = { let st = self.st.borrow(); st.s.remember_window && st.s.window.is_some_and(|p| p.maximized) };
            if maximized {
                w.set_maximized(true);
            }
            if self.st.borrow().normal.is_none() {
                let (pos, size) = (w.position(), w.size());
                self.st.borrow_mut().normal = Some((pos.x, pos.y, size.width, size.height));
            }
        }
        w.with_winit_window(|ww| ww.focus_window());
        self.update_demand();
        self.render_page();
    }

    /// Scale, frame colours and click-through, for windows that exist. Windows shown before the
    /// event loop runs only exist once it starts, so this runs again on their first event.
    fn window_setup(&self) {
        self.apply_theme(false);
        self.apply_scale();
        self.style_windows();
        self.apply_click_through();
        if self.st.borrow().compact_unplaced {
            self.place_compact();
        }
    }

    /// Puts the compact monitor at the top right of the screen it opened on.
    fn place_compact(&self) {
        let compact = self.compact.borrow();
        let Some(c) = compact.as_ref() else { return };
        let Some(Some(m)) = c.window().with_winit_window(|w| w.current_monitor().or_else(|| w.primary_monitor())) else { return };
        let size = c.window().size();
        let margin = (24.0 * c.window().scale_factor()) as i32;
        let x = m.position().x + m.size().width as i32 - size.width as i32 - margin;
        let y = m.position().y + margin;
        c.window().set_position(PhysicalPosition::new(x, y));
        self.st.borrow_mut().compact_unplaced = false;
    }

    fn setup_if_pending(&self) {
        let pending = self.st.try_borrow_mut().is_ok_and(|mut st| std::mem::replace(&mut st.setup_pending, false));
        if pending {
            Timer::single_shot(Duration::ZERO, || with(|c| c.window_setup()));
        }
    }

    fn hide_main(&self) {
        self.capture_placement();
        let _ = self.main.hide();
        self.st.borrow_mut().main_shown = false;
        self.update_demand();
        self.save_soon();
        self.release_when_idle();
    }

    /// Once no window is left open, hands memory the app isn't using back to Windows. Asking
    /// again before it ran pushes it back, so it runs once.
    fn release_when_idle(&self) {
        // After the hidden windows' own clean-up has run.
        self.trim_timer.start(TimerMode::SingleShot, Duration::from_millis(1500), || {
            with(|c| {
                let idle = { let st = c.st.borrow(); !st.main_shown && !st.compact_shown && !st.quitting };
                if idle {
                    c.shell.release_memory();
                }
            })
        });
    }

    fn minimized_changed(self: &Rc<Self>, minimized: bool) {
        let to_tray = { let st = self.st.borrow(); st.s.tray_icon && st.s.minimize_to_tray };
        if minimized && to_tray {
            self.main.window().set_minimized(false);
            self.hide_main();
            return;
        }
        self.update_demand();
        if !minimized {
            self.render_page();
        }
    }

    fn ensure_compact(self: &Rc<Self>) -> Option<CompactWindow> {
        if let Some(c) = self.compact.borrow().as_ref() {
            return Some(c.clone_strong());
        }
        let c = CompactWindow::new().ok()?;
        c.set_metrics(model(&self.m.compact));
        let s = self;
        cb!(s, c, on_open_main, | | s.show_main());
        cb!(s, c, on_dismiss, | | s.hide_compact());
        cb!(s, c, on_drag, | | {
            if let Some(cw) = s.compact.borrow().as_ref() {
                cw.window().with_winit_window(|w| {
                    let _ = w.drag_window();
                });
            }
        });
        let w = Rc::downgrade(self);
        c.window().on_winit_window_event(move |_win, event| {
            let Some(c) = w.upgrade() else { return EventResult::Propagate };
            c.setup_if_pending();
            if let winit::event::WindowEvent::Moved(pos) = event {
                {
                    let mut st = c.st.borrow_mut();
                    st.s.compact.x = Some(pos.x);
                    st.s.compact.y = Some(pos.y);
                }
                c.save_soon();
            }
            EventResult::Propagate
        });
        let w = Rc::downgrade(self);
        c.window().on_close_requested(move || {
            if let Some(c) = w.upgrade() {
                Timer::single_shot(Duration::ZERO, move || with(|c| c.hide_compact()));
                drop(c);
            }
            CloseRequestResponse::KeepWindowShown
        });
        *self.compact.borrow_mut() = Some(c.clone_strong());
        self.apply_theme(true);
        Some(c)
    }

    fn show_compact(self: &Rc<Self>) {
        let Some(c) = self.ensure_compact() else { return };
        let saved = {
            let st = self.st.borrow();
            st.s.compact.x.zip(st.s.compact.y)
        };
        let placed = saved.is_some_and(|(x, y)| self.shell.on_screen(x, y, 120, 60));
        if let (true, Some((x, y))) = (placed, saved) {
            c.window().set_position(PhysicalPosition::new(x, y));
        }
        c.set_click_through(self.st.borrow().s.compact.click_through);
        self.render_compact();
        let _ = c.show();
        {
            let mut st = self.st.borrow_mut();
            st.compact_shown = true;
            st.s.compact.open = true;
            st.compact_unplaced = !placed;
            st.setup_pending = true;
        }
        self.window_setup();
        self.update_demand();
        self.update_tray();
        self.save_soon();
    }

    fn hide_compact(self: &Rc<Self>) {
        if let Some(c) = self.compact.borrow().as_ref() {
            let _ = c.hide();
        }
        let nothing_left = {
            let mut st = self.st.borrow_mut();
            st.compact_shown = false;
            st.s.compact.open = false;
            !st.main_shown
        };
        self.update_demand();
        self.update_tray();
        self.save_soon();
        if nothing_left && self.tray.borrow().is_none() {
            // No window and no tray icon: nothing left to come back to.
            self.quit();
        } else if nothing_left {
            self.release_when_idle();
        }
    }

    fn toggle_compact(self: &Rc<Self>) {
        if self.st.borrow().compact_shown {
            self.hide_compact();
        } else {
            self.show_compact();
        }
    }

    fn apply_click_through(&self) {
        let on = self.st.borrow().s.compact.click_through;
        if let Some(c) = self.compact.borrow().as_ref() {
            c.set_click_through(on);
            c.window().with_winit_window(|w| {
                let _ = w.set_cursor_hittest(!on);
            });
        }
        self.app().set_compact_click_through(on);
        self.update_tray();
    }

    fn set_tray(self: &Rc<Self>, on: bool) {
        if !on {
            if let Some(t) = self.tray.borrow_mut().take() {
                let _ = t.hide();
            }
            return;
        }
        if self.tray.borrow().is_some() {
            return;
        }
        let Ok(t) = Tray::new() else { return };
        let c = self;
        cb!(c, t, on_show_main, | | c.show_main());
        cb!(c, t, on_toggle_compact, | | c.toggle_compact());
        cb!(c, t, on_toggle_click_through, | | {
            let v = !c.st.borrow().s.compact.click_through;
            c.setting_bool("click-through", v)
        });
        cb!(c, t, on_quit, | | c.quit());
        let _ = t.show();
        *self.tray.borrow_mut() = Some(t);
        self.update_tray();
    }

    fn update_tray(&self) {
        let (open, ct) = {
            let st = self.st.borrow();
            (st.compact_shown, st.s.compact.click_through)
        };
        if let Some(t) = self.tray.borrow().as_ref() {
            t.set_compact_open(open);
            t.set_click_through(ct);
        }
    }

    pub fn external(self: &Rc<Self>, cmd: &str) {
        match cmd {
            "quit" => self.quit(),
            _ => self.show_main(),
        }
    }

    fn restart_elevated(self: &Rc<Self>) {
        self.capture_placement();
        self.save_now();
        match self.control.restart_elevated() {
            Ok(()) => self.quit(),
            Err(ActionError::Cancelled) => {}
            Err(e) => self.notice(&format!("Couldn't restart as administrator. {}", e.message()), 2),
        }
    }

    fn quit(&self) {
        if std::mem::replace(&mut self.st.borrow_mut().quitting, true) {
            return;
        }
        if self.st.borrow().main_shown {
            self.capture_placement();
        }
        self.save_now();
        if let Some(t) = self.tray.borrow_mut().take() {
            let _ = t.hide();
        }
        if let Some(c) = self.compact.borrow().as_ref() {
            let _ = c.hide();
        }
        let _ = self.main.hide();
        let _ = slint::quit_event_loop();
    }

    // ─── Dialog, notice, saving ────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn ask(&self, title: &str, body: &str, lines: Vec<String>, confirm: &str, danger: bool, shield: bool, then: impl FnOnce(&Rc<Ctrl>) + 'static) {
        self.st.borrow_mut().dialog = Some(Box::new(then));
        let app = self.app();
        app.set_dialog_title(title.into());
        app.set_dialog_body(body.into());
        self.m.dialog_lines.set_vec(lines.into_iter().map(SharedString::from).collect::<Vec<_>>());
        app.set_dialog_confirm(confirm.into());
        app.set_dialog_danger(danger);
        app.set_dialog_shield(shield);
        app.set_dialog_open(true);
    }

    /// kind: 0 done, 1 heads-up, 2 problem.
    fn notice(&self, text: &str, kind: i32) {
        let app = self.app();
        app.set_notice(text.into());
        app.set_notice_kind(kind);
        app.set_notice_visible(true);
        let secs = if kind == 0 { 4 } else { 9 };
        self.notice_timer.start(TimerMode::SingleShot, Duration::from_secs(secs), || {
            with(|c| c.app().set_notice_visible(false));
        });
    }

    fn save_soon(&self) {
        self.save_timer.start(TimerMode::SingleShot, Duration::from_millis(700), || with(|c| c.save_now()));
    }

    fn save_now(&self) {
        let (path, text) = {
            let st = self.st.borrow();
            (st.path.clone(), st.s.to_json())
        };
        if let Err(e) = write_atomic(&path, &text) {
            self.st.borrow_mut().error = Some(format!("Couldn't save settings: {e}"));
        }
    }
}

/// Writes a file so a crash mid-write never leaves it half written.
fn write_atomic(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_and_paused_samples_nothing() {
        let mut s = Settings::default();
        s.background = Background::Pause;
        let d = compute_demand(&s, Page::Overview, false, false, false);
        assert!(d.paused);
        // The compact monitor keeps sampling going, without processes.
        let d = compute_demand(&s, Page::Overview, false, true, false);
        assert!(!d.paused && !d.processes);
    }

    #[test]
    fn process_extras_only_for_the_processes_page() {
        let s = Settings::default();
        let d = compute_demand(&s, Page::Overview, true, false, false);
        assert!(d.processes && !d.windows && !d.services && !d.process_gpu);
        let d = compute_demand(&s, Page::Processes, true, false, false);
        assert!(d.windows && d.services && d.process_gpu);
        let mut r = s.clone();
        r.reduced = true;
        let d = compute_demand(&r, Page::Processes, true, false, false);
        assert!(!d.process_gpu && !d.sensors);
    }

    #[test]
    fn every_gpu_engine_is_read_only_while_a_gpu_is_open() {
        let s = Settings::default();
        assert!(compute_demand(&s, Page::Performance, true, false, true).gpu_engines);
        assert!(!compute_demand(&s, Page::Performance, true, false, false).gpu_engines, "the CPU or memory view");
        assert!(!compute_demand(&s, Page::Overview, true, false, true).gpu_engines);
        assert!(!compute_demand(&s, Page::Performance, false, true, true).gpu_engines, "hidden behind the compact monitor");
    }

    #[test]
    fn history_mode_drops_processes_while_hidden() {
        let mut s = Settings::default();
        s.background = Background::History;
        let d = compute_demand(&s, Page::Processes, false, false, false);
        assert!(!d.processes && !d.paused);
    }

    #[test]
    fn atomic_write_replaces() {
        let dir = std::env::temp_dir().join(format!("tm-app-test-{}", std::process::id()));
        let p = dir.join("settings.json");
        write_atomic(&p, "{\"a\":1}").unwrap();
        write_atomic(&p, "{\"a\":2}").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"a\":2}");
        let _ = std::fs::remove_dir_all(dir);
    }
}

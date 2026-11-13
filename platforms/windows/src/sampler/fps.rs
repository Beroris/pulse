//! Frames per second of the app in front, counted from Windows' own graphics events, the way
//! PresentMon and Xbox Game Bar count them.
//!
//! One real-time ETW session listens for presents from three providers: DXGI (Direct3D 10 to 12),
//! Direct3D 9, and the graphics kernel, which also sees Vulkan and OpenGL. Each provider is
//! filtered to its present events by ID, so nothing else is delivered.
//!
//! Windows lets administrators and members of the Performance Log Users group start a session.
//! Game Bar's FPS counter adds the user to that group when it asks for access.
//!
//! A session outlives the process that started it, so it is stopped when the frame rate leaves
//! the screen, when sampling pauses and when the app exits, and a leftover one from a crash is
//! replaced on the next start.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use tm_core::model::{FpsAccess, FpsNow};
use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Diagnostics::Etw::*;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
use windows::Win32::System::Threading::PROCESS_BASIC_INFORMATION;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
use windows::core::{GUID, PCWSTR, PWSTR};

use crate::util::wide;

#[cfg(not(debug_assertions))]
const SESSION: &str = "Pulse frame rate";
#[cfg(debug_assertions)]
const SESSION: &str = "Pulse frame rate (development)";

const DXGI: GUID = GUID::from_u128(0xCA11C036_0102_4A2D_A6AD_F03CFED5D3C9);
const D3D9: GUID = GUID::from_u128(0x783ACA0A_790E_4D7F_8451_AA850511C6B9);
const DXGKRNL: GUID = GUID::from_u128(0x802EC45A_1E99_4B83_9920_87C98277BA9D);

/// Event IDs and keywords, from the providers' manifests (as PresentMon lists them).
const DXGI_PRESENT: u16 = 42;
const DXGI_PRESENT_MPO: u16 = 55;
const D3D9_PRESENT: u16 = 1;
const DXGK_PRESENT_HISTORY: u16 = 171;
const DXGK_PRESENT_HISTORY_DETAILED: u16 = 215;
const KEYWORD_API_PRESENT: u64 = 0x8000_0000_0000_0002;
const KEYWORD_DXGK_PRESENT: u64 = 0x0800_0000;

/// FlushTimer is in milliseconds (EVENT_TRACE_USE_MS_FLUSH_TIMER).
const USE_MS_FLUSH_TIMER: u32 = 0x10;
/// How often buffers are handed over, which is how late the newest frames can arrive.
const FLUSH_MS: u32 = 250;

/// This process has a session running, so exiting must stop it.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Present times of one process, by where they came from.
#[derive(Default)]
struct Track {
    /// DXGI and Direct3D 9: one per frame an app presents.
    api: VecDeque<i64>,
    /// Graphics kernel, for apps that present some other way. Windows logs one or the other kind
    /// of present history event per frame, depending on its version, so they're kept apart.
    history: VecDeque<i64>,
    detailed: VecDeque<i64>,
}

impl Track {
    fn newest(&self) -> i64 {
        [&self.api, &self.history, &self.detailed].iter().filter_map(|q| q.back().copied()).max().unwrap_or(0)
    }
}

struct Shared {
    tracks: Mutex<HashMap<u32, Track>>,
    /// Two seconds in timestamp ticks: how much of each track is kept.
    keep: i64,
}

pub struct FpsTracer {
    control: CONTROLTRACE_HANDLE,
    consumer: PROCESSTRACE_HANDLE,
    thread: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
    freq: i64,
    me: u32,
    /// The process last shown, kept while the app in front draws nothing (alt-tabbed to a chat app).
    target: Option<u32>,
    name: Option<(u32, Arc<str>)>,
    /// Parents of processes that draw, looked up once each.
    parents: HashMap<u32, Option<u32>>,
}

impl FpsTracer {
    pub fn start() -> Result<FpsTracer, FpsAccess> {
        let mut freq = 0i64;
        unsafe {
            let _ = QueryPerformanceFrequency(&mut freq);
        }
        if freq <= 0 {
            return Err(FpsAccess::Failed);
        }
        let name = wide(SESSION);
        let mut control = CONTROLTRACE_HANDLE::default();
        let mut props = properties();
        let mut r = unsafe { StartTraceW(&mut control, PCWSTR(name.as_ptr()), props.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES) };
        if r == ERROR_ALREADY_EXISTS {
            // Left over from a copy that crashed.
            stop_by_name();
            props = properties();
            r = unsafe { StartTraceW(&mut control, PCWSTR(name.as_ptr()), props.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES) };
        }
        if r == ERROR_ACCESS_DENIED {
            return Err(FpsAccess::NeedsPermission);
        }
        if r != ERROR_SUCCESS {
            return Err(FpsAccess::Failed);
        }
        RUNNING.store(true, Ordering::SeqCst);
        let fail = |a: FpsAccess| {
            stop(control);
            a
        };
        let r = enable(control, &DXGI, KEYWORD_API_PRESENT, &[DXGI_PRESENT, DXGI_PRESENT_MPO]);
        if r == ERROR_ACCESS_DENIED {
            return Err(fail(FpsAccess::NeedsPermission));
        }
        if r != ERROR_SUCCESS {
            return Err(fail(FpsAccess::Failed));
        }
        // Optional: without these, Direct3D 9, Vulkan and OpenGL games show no frame rate.
        let _ = enable(control, &D3D9, KEYWORD_API_PRESENT, &[D3D9_PRESENT]);
        let _ = enable(control, &DXGKRNL, KEYWORD_DXGK_PRESENT, &[DXGK_PRESENT_HISTORY, DXGK_PRESENT_HISTORY_DETAILED]);

        let shared = Arc::new(Shared { tracks: Mutex::new(HashMap::new()), keep: freq * 2 });
        let mut logger = name.clone();
        let mut log: EVENT_TRACE_LOGFILEW = unsafe { std::mem::zeroed() };
        log.LoggerName = PWSTR(logger.as_mut_ptr());
        log.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD | PROCESS_TRACE_MODE_RAW_TIMESTAMP;
        log.Anonymous2.EventRecordCallback = Some(on_event);
        log.Context = Arc::as_ptr(&shared) as *mut _;
        let consumer = unsafe { OpenTraceW(&mut log) };
        if consumer.Value == u64::MAX {
            return Err(fail(FpsAccess::Failed));
        }
        // The callback reads `shared` through a raw pointer: this clone keeps it alive until
        // ProcessTrace has returned.
        let keep = shared.clone();
        let thread = std::thread::Builder::new()
            .name("frame rate".into())
            .spawn(move || {
                unsafe {
                    let _ = ProcessTrace(&[consumer], None, None);
                }
                drop(keep);
            })
            .ok();
        if thread.is_none() {
            unsafe {
                let _ = CloseTrace(consumer);
            }
            return Err(fail(FpsAccess::Failed));
        }
        Ok(FpsTracer { control, consumer, thread, shared, freq, me: std::process::id(), target: None, name: None, parents: HashMap::new() })
    }

    /// The frame rate of the app in front, or of the one last shown if the app in front isn't
    /// drawing. `None` when neither is.
    pub fn read(&mut self) -> Option<FpsNow> {
        let mut now = 0i64;
        unsafe {
            let _ = QueryPerformanceCounter(&mut now);
        }
        let front = foreground_pid().filter(|&p| p != self.me);
        let drawing: Vec<(u32, f32)> = {
            let mut tracks = self.shared.tracks.lock().ok()?;
            // Forget processes that stopped drawing a while ago.
            tracks.retain(|_, t| now - t.newest() < self.freq * 5);
            self.parents.retain(|pid, _| tracks.contains_key(pid));
            tracks.iter().filter(|(p, _)| **p != self.me).filter_map(|(&p, t)| track_rate(t, self.freq, now).filter(|f| *f >= 1.0).map(|f| (p, f))).collect()
        };
        let mut pick = None;
        for app in [front, self.target].into_iter().flatten() {
            if let Some(&(_, f)) = drawing.iter().find(|(p, _)| *p == app) {
                pick = Some((app, f));
                break;
            }
            // Browsers, and apps built on them, draw from a child process.
            let mut child: Option<f32> = None;
            for &(p, f) in &drawing {
                if self.parent(p) == Some(app) {
                    child = Some(child.map_or(f, |c| c.max(f)));
                }
            }
            if let Some(f) = child {
                pick = Some((app, f));
                break;
            }
        }
        let (pid, fps) = pick?;
        self.target = Some(pid);
        let name = match &self.name {
            Some((p, n)) if *p == pid => n.clone(),
            _ => {
                let n: Arc<str> = process_name(pid).unwrap_or_else(|| format!("PID {pid}")).into();
                self.name = Some((pid, n.clone()));
                n
            }
        };
        Some(FpsNow { pid, name, fps })
    }
}

impl FpsTracer {
    fn parent(&mut self, pid: u32) -> Option<u32> {
        *self.parents.entry(pid).or_insert_with(|| parent_pid(pid))
    }
}

impl Drop for FpsTracer {
    fn drop(&mut self) {
        unsafe {
            // Makes ProcessTrace return once it has delivered what it has.
            let _ = CloseTrace(self.consumer);
        }
        stop(self.control);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Stops this process's session, if it has one. For exit, which skips destructors.
pub fn stop_on_exit() {
    if RUNNING.load(Ordering::SeqCst) {
        stop_by_name();
    }
}

fn stop(control: CONTROLTRACE_HANDLE) {
    let mut props = properties();
    unsafe {
        let _ = ControlTraceW(control, PCWSTR::null(), props.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES, EVENT_TRACE_CONTROL_STOP);
    }
    RUNNING.store(false, Ordering::SeqCst);
}

fn stop_by_name() {
    let name = wide(SESSION);
    let mut props = properties();
    unsafe {
        let _ = ControlTraceW(CONTROLTRACE_HANDLE::default(), PCWSTR(name.as_ptr()), props.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES, EVENT_TRACE_CONTROL_STOP);
    }
    RUNNING.store(false, Ordering::SeqCst);
}

/// EVENT_TRACE_PROPERTIES followed by room for the session name, 8-byte aligned.
fn properties() -> Vec<u64> {
    let head = std::mem::size_of::<EVENT_TRACE_PROPERTIES>();
    // The name, and room for Windows to write back a log file name on stop.
    let size = head + (SESSION.len() + 1) * 2 + 1024;
    let mut buf = vec![0u64; size.div_ceil(8)];
    let p = buf.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES;
    unsafe {
        (*p).Wnode.BufferSize = (buf.len() * 8) as u32;
        (*p).Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        // Timestamps from QueryPerformanceCounter, to compare with it directly.
        (*p).Wnode.ClientContext = 1;
        (*p).BufferSize = 16; // KB
        (*p).MinimumBuffers = 4;
        (*p).MaximumBuffers = 32;
        (*p).LogFileMode = EVENT_TRACE_REAL_TIME_MODE | USE_MS_FLUSH_TIMER;
        (*p).FlushTimer = FLUSH_MS;
        (*p).LoggerNameOffset = head as u32;
    }
    buf
}

/// Turns a provider on for the session, delivering only the given event IDs.
fn enable(control: CONTROLTRACE_HANDLE, provider: &GUID, keywords: u64, ids: &[u16]) -> WIN32_ERROR {
    // EVENT_FILTER_EVENT_ID: FilterIn (1 byte) and Reserved (1 byte), Count, then the IDs.
    let mut filter: Vec<u16> = Vec::with_capacity(2 + ids.len());
    filter.push(1);
    filter.push(ids.len() as u16);
    filter.extend_from_slice(ids);
    let mut desc = EVENT_FILTER_DESCRIPTOR { Ptr: filter.as_ptr() as u64, Size: (filter.len() * 2) as u32, Type: EVENT_FILTER_TYPE_EVENT_ID };
    let mut params: ENABLE_TRACE_PARAMETERS = unsafe { std::mem::zeroed() };
    params.Version = ENABLE_TRACE_PARAMETERS_VERSION_2;
    params.EnableFilterDesc = &mut desc;
    params.FilterDescCount = 1;
    unsafe { EnableTraceEx2(control, provider, EVENT_CONTROL_CODE_ENABLE_PROVIDER.0, TRACE_LEVEL_VERBOSE as u8, keywords, 0, 0, Some(&params as *const _)) }
}

/// Called on the trace thread for each event. Must not panic: it is called from Windows.
unsafe extern "system" fn on_event(rec: *mut EVENT_RECORD) {
    let Some(r) = (unsafe { rec.as_ref() }) else { return };
    let h = &r.EventHeader;
    let id = h.EventDescriptor.Id;
    let kind = if (h.ProviderId == DXGI && (id == DXGI_PRESENT || id == DXGI_PRESENT_MPO)) || (h.ProviderId == D3D9 && id == D3D9_PRESENT) {
        0
    } else if h.ProviderId == DXGKRNL && id == DXGK_PRESENT_HISTORY {
        1
    } else if h.ProviderId == DXGKRNL && id == DXGK_PRESENT_HISTORY_DETAILED {
        2
    } else {
        return;
    };
    let Some(shared) = (unsafe { (r.UserContext as *const Shared).as_ref() }) else { return };
    let Ok(mut tracks) = shared.tracks.lock() else { return };
    let t = tracks.entry(h.ProcessId).or_default();
    let q = match kind {
        0 => &mut t.api,
        1 => &mut t.history,
        _ => &mut t.detailed,
    };
    q.push_back(h.TimeStamp);
    while q.len() > 2048 || q.front().is_some_and(|&f| h.TimeStamp - f > shared.keep) {
        q.pop_front();
    }
}

/// Frames in the second up to a process's newest present. Prefers the API's count, which is one
/// per frame the app made, and falls back to the graphics kernel's. `None` once it hasn't
/// presented for a second and a half, allowing for frames still on their way.
fn track_rate(t: &Track, freq: i64, now: i64) -> Option<f32> {
    let source = if !t.api.is_empty() { &t.api } else if t.detailed.len() >= t.history.len() { &t.detailed } else { &t.history };
    count_rate(source, freq, now)
}

pub(crate) fn count_rate(stamps: &VecDeque<i64>, freq: i64, now: i64) -> Option<f32> {
    let newest = *stamps.iter().max()?;
    if now - newest > freq * 3 / 2 + freq * FLUSH_MS as i64 / 1000 {
        return None;
    }
    let from = newest - freq;
    Some(stamps.iter().filter(|&&t| t > from).count() as f32)
}

fn foreground_pid() -> Option<u32> {
    let w = unsafe { GetForegroundWindow() };
    if w.is_invalid() {
        return None;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(w, Some(&mut pid)) };
    (pid > 4).then_some(pid)
}

fn parent_pid(pid: u32) -> Option<u32> {
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut info = PROCESS_BASIC_INFORMATION::default();
    let mut len = 0u32;
    let status = unsafe { NtQueryInformationProcess(h, ProcessBasicInformation, &mut info as *mut _ as *mut _, std::mem::size_of::<PROCESS_BASIC_INFORMATION>() as u32, &mut len) };
    unsafe {
        let _ = CloseHandle(h);
    }
    (status.is_ok() && info.InheritedFromUniqueProcessId != 0).then_some(info.InheritedFromUniqueProcessId as u32)
}

/// "Overwatch" for "C:\…\Overwatch.exe".
fn process_name(pid: u32) -> Option<String> {
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 512];
    let mut len = buf.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.is_ok();
    unsafe {
        let _ = CloseHandle(h);
    }
    if !ok {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    let file = path.rsplit(['\\', '/']).next().unwrap_or(&path);
    let stem = file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(file);
    (!stem.is_empty()).then(|| stem.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_frames_in_the_last_second() {
        let freq = 1_000_000i64;
        // 144 frames a second for two seconds.
        let q: VecDeque<i64> = (0..288).map(|i| i * freq / 144).collect();
        let newest = *q.back().unwrap();
        let fps = count_rate(&q, freq, newest + freq / 10).unwrap();
        assert!((143.0..=145.0).contains(&fps), "{fps}");
        // Stopped presenting two seconds ago.
        assert_eq!(count_rate(&q, freq, newest + 2 * freq), None);
        assert_eq!(count_rate(&VecDeque::new(), freq, 0), None);
    }

    #[test]
    fn api_presents_win_over_kernel_ones() {
        let freq = 1000i64;
        let mut t = Track::default();
        t.api = (0..60).map(|i| i * 1000 / 60).collect();
        // The kernel sees the same frames, and more for other reasons.
        t.history = (0..120).map(|i| i * 1000 / 120).collect();
        assert_eq!(track_rate(&t, freq, 1000), Some(60.0));
        t.api.clear();
        assert_eq!(track_rate(&t, freq, 1000), Some(120.0));
    }

    /// Counts frames for three seconds and prints each process that presented. Needs to be run by
    /// an administrator or a member of Performance Log Users:
    /// `cargo test -p tm-windows -- --ignored --nocapture traces_frames`
    #[test]
    #[ignore]
    fn traces_frames() {
        let mut t = match FpsTracer::start() {
            Ok(t) => t,
            Err(a) => panic!("couldn't start: {a:?}"),
        };
        std::thread::sleep(std::time::Duration::from_secs(3));
        let mut now = 0i64;
        unsafe {
            let _ = QueryPerformanceCounter(&mut now);
        }
        {
            let tracks = t.shared.tracks.lock().unwrap();
            let mut rows: Vec<(u32, f32, usize, usize, usize)> = tracks
                .iter()
                .map(|(pid, tr)| (*pid, track_rate(tr, t.freq, now).unwrap_or(0.0), tr.api.len(), tr.history.len(), tr.detailed.len()))
                .collect();
            rows.sort_by(|a, b| b.1.total_cmp(&a.1));
            for (pid, fps, a, h, d) in rows {
                println!("{pid:>7} {:<28} {fps:>6.1} fps   api {a:>4}  history {h:>4}  detailed {d:>4}", process_name(pid).unwrap_or_default());
            }
        }
        println!("in front: {:?}", t.read());
        drop(t);
        assert!(!RUNNING.load(Ordering::SeqCst));
    }
}

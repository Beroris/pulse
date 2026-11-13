# Performance

These figures come from runs on 2026-11-13, on the release build as packaged (`opt-level = "s"` with the drawing and sampling crates at 3, `lto = "fat"`, one codegen unit, static CRT, software renderer, 1 s refresh). The machine was an AMD Ryzen 5 5600X (12 logical processors) with 16 GB of RAM and a Radeon RX 6800 XT, running Windows 11 Pro 10.0.26200, with 188 to 241 processes. They describe this machine only, not a guarantee for others.

## Targets

| Target | Result |
|---|---|
| Idle memory under 80 MB | 12–14 MB with a window open, 2–4 MB in the tray (Task Manager's Memory column); 13–15 MB committed |
| Idle CPU under 0.5% | 0.06–0.09% with the window open, 0.03% in the tray |
| Warm start under 500 ms | 46 ms to a visible window |
| 1 s refresh | default; 0.5, 2 and 5 s can be chosen |

## Idle, by state

Each state was left to settle for 17 s, then measured over 40 s, twice. The CPU figure is milliseconds of CPU used per second of wall time.

| State | UI thread | Total | Of the whole CPU | Of one core | Memory | Private bytes | Working set |
|---|---:|---:|---:|---:|---:|---:|---:|
| Overview, visible | 3.1 ms | 7.1–7.9 ms | 0.06–0.07% | 0.7–0.8% | 12.1 MB | 13.4–13.5 MB | 39.5 MB |
| Processes, visible | 5.9–6.2 ms | 10.3–10.8 ms | 0.09% | 1.0–1.1% | 14.0 MB | 15.3 MB | 41.8 MB |
| Performance (CPU view), visible | 4.0–4.1 ms | 8.6–8.7 ms | 0.07% | 0.9% | 13.0–13.1 MB | 14.4 MB | 40.8 MB |
| Compact monitor only | 1.0 ms | 5.2–5.8 ms | 0.04–0.05% | 0.5–0.6% | 12.4–12.5 MB | 13.7–13.8 MB | 39.9 MB |
| In the tray after closing the window, still monitoring | 0.1 ms | 3.6–4.1 ms | 0.03% | 0.4% | 2.7–3.6 MB | 13.4–13.9 MB | 6.0–7.5 MB |
| In the tray since sign-in (`--background`), still monitoring | 0.0 ms | 3.9–4.2 ms | 0.03% | 0.4% | 2.2–2.3 MB | 9.0–9.1 MB | 5.0 MB |
| In the tray, paused | 0.0 ms | 0.03 ms | 0.00% | 0.00% | 0.7–0.8 MB | 12.8 MB | 1.2–1.4 MB |
| In the tray since sign-in, paused | 0.0 ms | 0.03 ms | 0.00% | 0.00% | 0.6–0.8 MB | 7.9–8.0 MB | 1.1–1.3 MB |

Run to run, the visible states move by about 1 ms with what the rest of the machine is doing.

The Performance page's GPU view reads every GPU engine each second, which on this card adds about 6 ms: about 15.5 ms in total, measured on 2026-11-12.

### What the memory columns mean

- **Memory** is the private working set: the app's own pages that are in RAM right now. It's what Task Manager's Memory column shows.
- **Private bytes** is everything the app has committed, whether it's in RAM or not.
- **Working set** adds shared pages: the exe itself, system DLLs and the font. Windows counts those once for the whole system.

When the last window closes, Pulse hands its pages back to Windows, once, 1.5 s later. It does the same once after its first read of the startup entries, which happens a few seconds after it starts in the tray. It never does this on a timer. The pages the collector still uses come back as it touches them, which is why the tray figures settle at 2–4 MB rather than under 1 MB. The committed figure doesn't change. A window that has been opened is kept, so it opens again at once. That is the gap between 13.4 MB (closed to the tray) and 9 MB (never opened).

With a window open, the interface's state and drawing keep 12–14 MB in use. Getting that under 10 MB would mean a different UI toolkit, not a setting.

### How this is measured

CPU time comes from cycle counts, which are exact:
- `QueryProcessCycleTime` for the whole process, which includes threads that have already exited;
- `QueryThreadCycleTime` for the UI thread.

Both are divided by the counter's rate, measured with a short spin (3.6–3.7 GHz here). The usual `TotalProcessorTime` isn't used. Windows adds to it in 15.6 ms steps at clock interrupts, so a thread that wakes on a timer, works for a few milliseconds and sleeps again is charged a whole step or nothing. Over 30 s that gave figures up to twice too high or too low for the same build.

Memory comes from `Win32_PerfRawData_PerfProc_Process` (`WorkingSetPrivate`) and the process's private bytes and working set.

Every run uses a portable copy with its own settings.

## Start

| Measurement | Result | How |
|---|---:|---|
| Warm start to a visible window | 45–47 ms in 12 of 14 runs, 61 ms in the other 2 (median 46) | `Start-Process`, then polling for the window, which moves in 15 ms steps; one run first to warm the disk cache |
| Started with `--background` | 9.0 MB private bytes, 2.3 MB memory, 5.0 MB working set once settled | no window is created |

## Size

| File | Size |
|---|---:|
| `pulse.exe` (also the portable download) | 17.6 MB |
| Installer | 7.6 MB |

Inside the exe, code is 10.2 MB and read-only data 5.9 MB. What is known about where it goes:

- **The interface itself.** Slint turns the `.slint` files into Rust code. That was the largest single crate (3.7 MB) before it was built for size.
- **Parts of Slint this app doesn't use but can't leave out.** The window backend always includes SVG, JPEG, WebP, PNG and GIF decoding (about 1.5 MB of code in the first build, before it was built for size). The text layout always includes word-break dictionaries for Chinese, Japanese, Thai, Khmer, Lao and Burmese; their size in the exe hasn't been measured separately, but they are a large part of the 5.9 MB of data. Neither has a switch. Removing them would mean keeping a patched copy of a Slint crate.
- **The optional GPU renderer** was about 0.35 MB in the first build, so dropping it wouldn't help much.
- **Geist**, the font: 0.17 MB.

## What was changed after measuring

### Second round (2026-11-13)

The build from before these changes and the build after them were measured on the same day, alternating, twice each. CPU is total ms per second; memory is Task Manager's Memory column, with private bytes after the slash.

| State | CPU before | CPU after | Memory before | Memory after |
|---|---:|---:|---:|---:|
| In the tray since sign-in | 4.4–4.8 ms | 3.9–4.2 ms | 7.3–8.6 / 10.1 MB | 2.2–2.3 / 9.0–9.1 MB |
| In the tray after closing the window | 4.3–4.5 ms | 3.6–4.1 ms | 12.8–13.3 / 14.2–15.0 MB | 2.7–3.6 / 13.4–13.9 MB |
| In the tray, paused | 0.03 ms | 0.03 ms | 12.8–12.9 / 14.4–15.4 MB | 0.7–0.8 / 12.8 MB |
| In the tray since sign-in, paused | 0.03 ms | 0.03 ms | 7.0–7.1 / 8.5–8.7 MB | 0.6–0.8 / 7.9–8.0 MB |
| Compact monitor only | 5.0–5.9 ms | 5.2–5.8 ms | 13.4–13.6 / 14.9–15.0 MB | 12.4–12.5 / 13.7–13.8 MB |
| Overview | 9.5–12.4 ms | 7.1–7.9 ms | 13.4–13.5 / 14.8–15.2 MB | 12.1 / 13.4–13.5 MB |
| Processes | 9.4–10.4 ms | 10.3–10.8 ms | 15.5–17.0 / 17.3–18.4 MB | 14.0 / 15.3 MB |
| Performance (CPU view) | 8.3–8.9 ms | 8.6–8.7 ms | 13.7–13.8 / 15.3–15.4 MB | 13.0–13.1 / 14.4 MB |

1. **Memory handed back once no window is open.** This is described above. It's a single call, `SetProcessWorkingSetSizeEx` with both limits at −1, and it doesn't change what the app has committed.
2. **Segment heap.** The manifest opts into Windows' newer heap (Windows 10 2004 and later), which gives freed memory back more readily. It saved 1–2.5 MB of private bytes in every state, with no change in CPU.
3. **Bars no longer glide to new values.** Each reading change animated the bar for 0.2 s, so with a 1 s refresh it was drawing for a fifth of every second. That was about half the Overview's drawing cost: its UI thread went from 4.9–8.1 ms to 3.1 ms.
4. **Counters held only while needed.**
   - The memory-list counters are opened only while memory detail is on screen.
   - The temperature counters are let go while paused.
   - The process-list buffer is freed while no list is wanted.
   - While paused, the collector sleeps until it's told to resume, instead of waking each second to check.

   This saved a little memory. Its effect on CPU was within the noise.

Tried and dropped: destroying the window when it closes, which Slint can do. The window came back without its icon, always-on-top, minimum size or maximized state, because Slint's window backend keeps its own copy of those and doesn't apply them again to a new window.

### First round (2026-11-12)

The first build of 1.0.0 was measured the same way, with 139 processes running, and three things stood out.

| | First build | After |
|---|---:|---:|
| Hidden in the tray | 12.0 ms | 4.0 ms |
| Overview | 17.5 ms | 10.9 ms |
| Processes | 24.2 ms | 14.5 ms |
| Performance (CPU view) | 15.8 ms | 9.6 ms |
| Exe | 22.6 MB | 17.4 MB |
| Installer | 8.2 MB | 7.5 MB |

1. **GPU engines.** Reading one engine's statistics takes the AMD driver about 0.46 ms, and this card has 16 engines, of which 3 had run since Windows started. That was 7.4 ms of the collector's 10.7 ms per sample. Engines whose running time hasn't moved for 10 s are now read every 5 s (every 15 s if they have never run since Windows started), and every engine is read each second only while the GPU view is open.

   The cost: outside the GPU view, an idle engine that starts work is noticed up to 5 or 15 s late. When it is noticed, only the latest second's share of that work is shown. The seconds before it stay at zero in the history.
2. **Table rows.** With the table sorted by CPU, rows change places on most refreshes, and every change of order rebuilt all visible rows. Rows now keep their place in the view and only their values change.
3. **Size.** Almost all of the code runs once a second at most, so it's now built for size. The crates that draw, shape text and do the per-tick maths keep full speed.

## Why it's cheap at idle

- The collector runs on its own thread and makes one `NtQuerySystemInformation` call per tick for every process, without opening them. Paths, descriptions and icons are looked up once per process on a low-priority thread.
- Pages are built when you open them and dropped when you leave, except the process table, which is kept (hidden) so it opens at once. It draws only the rows in view, and each tick it hands the view only the values that changed.
- What is sampled depends on what is showing:
  - per-process GPU use only on the Processes page with a GPU column on;
  - window titles and services only on that page;
  - memory detail only on the Performance page;
  - temperatures each second only where they're shown, and every 5 s otherwise.

  With "pause while hidden", nothing is sampled while the window is hidden, and the collector doesn't wake at all.
- Once no window is open, the app hands its memory back to Windows, so in the tray it holds 2–4 MB of RAM.
- The software renderer avoids loading a graphics driver, which can cost 100 MB or more.

What each data source costs per sample can be printed with `cargo test -p tm-windows -- --ignored --nocapture source_costs`.

## Not measured yet

- Windows 10. The installer allows version 1809 and newer, but all testing so far was on Windows 11. The segment heap needs version 2004; older versions ignore the setting and use the usual heap.
- NVIDIA and Intel graphics. The cost of reading an engine comes from the driver, so it may differ a lot.
- Low-end hardware, and machines with several hundred processes or more than 64 logical processors.
- The GPU renderer setting, and very large windows.
- Two or more monitors with different scale factors.
- A side-by-side run against Windows Task Manager on the same machine.
- Long runs (days) for memory growth. History buffers have a fixed size, so it shouldn't grow, but that hasn't been watched over days.

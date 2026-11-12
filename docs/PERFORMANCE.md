# Performance

These figures come from one run on 2026-11-12, on the release build as packaged (`opt-level = "s"` with the drawing and sampling crates at 3, `lto = "fat"`, one codegen unit, static CRT, software renderer, 1 s refresh). The machine was an AMD Ryzen 5 5600X (12 logical processors) with 16 GB of RAM and a Radeon RX 6800 XT, running Windows 11 Pro 10.0.26200, with 139 processes. They describe this machine only, not a guarantee for others.

## Targets

| Target | Result |
|---|---|
| Idle memory under 80 MB | 44–48 MB working set, 14–18 MB private |
| Idle CPU under 0.5% | 0.08–0.12% with the window open, 0.03% in the tray |
| Warm start under 500 ms | 62 ms to a visible window |
| 1 s refresh | default; 0.5, 2 and 5 s can be chosen |

## Idle, by state

Each state was left to settle for 17 s, then measured over 40 s. The figure is milliseconds of CPU used per second of wall time, per thread.

| State | UI thread | Collector thread | Total | Of the whole CPU | Of one core | Private bytes | Working set |
|---|---:|---:|---:|---:|---:|---:|---:|
| Overview, visible | 6.2 ms | 4.7 ms | 10.9 ms | 0.09% | 1.1% | 14.4 MB | 44.5 MB |
| Processes, visible | 10.0 ms | 4.4 ms | 14.5 ms | 0.12% | 1.4% | 17.4 MB | 47.5 MB |
| Performance (CPU view), visible | 5.0 ms | 4.7 ms | 9.6 ms | 0.08% | 1.0% | 15.5 MB | 45.5 MB |
| Compact monitor only | 1.0 ms | 4.8 ms | 5.8 ms | 0.05% | 0.6% | 15.1 MB | 45.1 MB |
| Hidden in the tray, still monitoring | 0.1 ms | 3.9 ms | 4.0 ms | 0.03% | 0.4% | 14.6 MB | 44.6 MB |
| Hidden in the tray, paused | 0.0 ms | 0.0 ms | 0.03 ms | 0.00% | 0.00% | 14.7 MB | 44.4 MB |

Run to run, the visible states move by about 1 ms with what the rest of the machine is doing. Overview measured 9.6 ms in an earlier run of the same build.

The Performance page's GPU view reads every GPU engine each second, which on this card adds about 6 ms: about 15.5 ms in total, measured on an earlier build from the same day.

The working set is larger than the private figure mostly because of shared pages: the exe itself, system DLLs and the font.

### How this is measured

CPU time comes from `QueryThreadCycleTime`, which counts processor cycles exactly, divided by the counter's rate (measured with a short spin, 3.6–3.7 GHz here). The usual `TotalProcessorTime` isn't used. Windows adds to it in 15.6 ms steps at clock interrupts, and a thread that wakes on a timer, works for a few milliseconds and sleeps again is charged a whole step or nothing. Over 30 s that gave figures up to twice too high or too low for the same build.

## Start

| Measurement | Result | How |
|---|---:|---|
| Warm start to a visible window | 61, 62, 62, 62, 62, 77, 77 ms (median 62) | `Start-Process`, then polling for the window; one run first to warm the disk cache |
| Started with `--background` | 9.1 MB private bytes, 33 MB working set after 3 s | no window is created |

## Size

| File | Size |
|---|---:|
| `bero-task-manager.exe` (also the portable download) | 17.4 MB |
| Installer | 7.5 MB |

Inside the exe, code is 10.2 MB and read-only data 5.9 MB. What is known about where it goes:

- **The interface itself.** Slint turns the `.slint` files into Rust code. That was the largest single crate (3.7 MB) before it was built for size.
- **Parts of Slint this app doesn't use but can't leave out.** The window backend always includes SVG, JPEG, WebP, PNG and GIF decoding (about 1.5 MB of code in the first build, before it was built for size). The text layout always includes word-break dictionaries for Chinese, Japanese, Thai, Khmer, Lao and Burmese; their size in the exe hasn't been measured separately, but they are a large part of the 5.9 MB of data. Neither has a switch. Removing them would mean keeping a patched copy of a Slint crate.
- **The optional GPU renderer** was about 0.35 MB in the first build, so dropping it wouldn't help much.
- **Geist**, the font: 0.17 MB.

## What was changed after measuring

The first build of 1.0.0 was measured the same way, and three things stood out.

| | First build | Now |
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
- What is sampled depends on what is showing. Per-process GPU use is read only on the Processes page with a GPU column on; window titles and services only on that page; memory detail only on the Performance page; temperatures only where they're shown. With "pause while hidden", nothing is sampled while the window is hidden.
- The software renderer avoids loading a graphics driver, which can cost 100 MB or more.

What each data source costs per sample can be printed with `cargo test -p tm-windows -- --ignored --nocapture source_costs`.

## Not measured yet

- Windows 10. The installer allows version 1809 and newer, but all testing so far was on Windows 11.
- NVIDIA and Intel graphics. The cost of reading an engine comes from the driver, so it may differ a lot.
- Low-end hardware, and machines with several hundred processes or more than 64 logical processors.
- The GPU renderer setting, and very large windows.
- Two or more monitors with different scale factors.
- A side-by-side run against Windows Task Manager on the same machine.
- Long runs (days) for memory growth. History buffers have a fixed size, so it shouldn't grow, but that hasn't been watched over days.

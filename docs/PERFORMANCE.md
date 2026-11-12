# Performance

These figures come from one run on 2026-11-12, on the release build (`lto = "fat"`, one codegen unit, static CRT, software renderer, 1 s refresh). The machine was an AMD Ryzen 5 5600X (12 logical processors) with 16 GB of RAM, running Windows 11 Pro 10.0.26200. They describe this machine only, not a guarantee for others.

## Targets

| Target | Result |
|---|---|
| Idle memory under 80 MB | 45–50 MB working set, 14–17 MB of it private |
| Idle CPU under 0.5% | 0.13–0.16% with the window open, 0.09% in the tray |
| Warm start under 500 ms | 62 ms to a visible window |
| 1 s refresh | default; 0.5, 2 and 5 s can be chosen |

## Idle, by state

Each state was left to settle for 15 s, then measured over 60 s. CPU is `TotalProcessorTime` over wall time; "of one core" is the same figure times 12.

| State | CPU, whole machine | CPU, of one core | Private working set | Working set | Private bytes |
|---|---:|---:|---:|---:|---:|
| Overview, visible | 0.15% | 1.8% | 13.6 MB | 45.4 MB | 15.1 MB |
| Processes, visible | 0.16% | 1.9% | 16.1 MB | 48.6 MB | 17.9 MB |
| Performance (CPU), visible | 0.13% | 1.6% | 16.5 MB | 49.4 MB | 18.2 MB |
| Hidden in the tray, still monitoring | 0.09% | 1.1% | 16.6 MB | 49.8 MB | 18.2 MB |
| Compact monitor only | 0.10% | 1.2% | 13.6 MB | | |
| Hidden in the tray, paused | 0.00% | 0.03% | 13.1 MB | | |
| Started with `--background` (3 s after start) | | | | | 9.5 MB |

Threads: 7–9. Handles: about 400–490.

The working set is larger than the private figure mostly because of shared pages: the exe itself, system DLLs and the font. Those are counted once for the whole system, not per app.

## Start

| Measurement | Result | How |
|---|---:|---|
| Warm start to a visible window | 62, 62, 62, 77, 77 ms (median 62) | `Start-Process`, then polling for the window; one run first to warm the disk cache |
| Second launch handing over to the running copy | under 1 s, returns code 0 | started the exe again while one was hidden in the tray |

## Size

| File | Size |
|---|---:|
| `bero-task-manager.exe` (also the portable download) | 22.6 MB |
| Installer | 8.2 MB |

The exe is the biggest cost here and the obvious thing to work on. It carries two renderers (the software one, used by default, and the optional OpenGL one), Slint's text shaping, the accessibility bridge and the Geist font. How much each part takes hasn't been broken down yet.

## Why it's cheap at idle

- The collector runs on its own thread and makes one `NtQuerySystemInformation` call per tick for every process, without opening them. Paths, descriptions and icons are looked up once per process on a low-priority thread.
- Pages are built when you open them and dropped when you leave, except the process table, which is kept (hidden) so it opens at once. It draws only the rows in view, and each tick it hands the view only the rows whose text changed.
- What is sampled depends on what is showing. Per-process GPU use is read only on the Processes page with a GPU column on; window titles and services only on that page; memory detail only on the Performance page; temperatures only where they're shown. With "pause while hidden", nothing is sampled while the window is hidden.
- The software renderer avoids loading a graphics driver, which can cost 100 MB or more.

## Not measured yet

- Windows 10. The installer allows version 1809 and newer, but all testing so far was on Windows 11.
- Low-end hardware, and machines with several hundred processes or more than 64 logical processors.
- The GPU renderer setting, and very large windows.
- Two or more monitors with different scale factors.
- A side-by-side run against Windows Task Manager on the same machine.
- Long runs (days) for memory growth. History buffers have a fixed size, so it shouldn't grow, but that hasn't been watched over days.

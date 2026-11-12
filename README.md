<img src="assets/icon.png" width="64" height="64" alt="">

# Bero Task Manager

A fast, quiet task manager for Windows. See what's using your CPU, memory, disk, GPU and network, end what's stuck, and tidy what starts with Windows, in a window that opens in about 60 ms and sits at around 0.1% CPU while you watch it.

It's part of the Bero suite, next to Prism, Orbit and Spotlight: same font, same dark-first look.

## What it does

- **Overview.** Live cards for CPU, memory, disks, GPU and network, plus a short list of plain facts worth knowing: memory nearly full, one program taking a big share of the CPU, a disk that has been flat out for a while, an app whose memory keeps growing, a drive running out of space. Every insight is computed from real readings; if there's nothing to say, it says nothing.
- **Processes.** Grouped into apps, background processes and Windows processes, or shown as a parent/child tree, or as a flat list. Sort by any column, pick from 18 columns (GPU engine, command line, publisher, commit size and more), resize them, search as you type. Rows hold still while your pointer is over the table, so the thing you're about to click doesn't jump away.
- **End task, gently.** End task asks a program to close the way its own close button would, so it can offer to save your work. If it doesn't close, you're told, and **Force end** is a separate, clearly marked step. Processes that would crash or sign you out of Windows (csrss, wininit, lsass and friends) are refused outright, and the check is repeated right before acting.
- **Performance.** Graphs and the numbers behind them for each CPU (overall or per core), memory (in use, committed, cached, pools, speed, slots), every GPU and its engines, every disk (active time, response time, read and write speed) and every network adapter.
- **Startup.** Everything that starts when you sign in: both Run keys, both Startup folders and Store apps' startup tasks. Disabling uses the same switch Windows Task Manager and Settings use, so nothing is ever deleted and you can turn it back on from any of the three.
- **Services.** Start, stop and restart services and change their startup type, with a clear warning on the ones Windows relies on.
- **Compact monitor.** A small, rounded, always-on-top readout of the numbers you pick. Make it see-through, or let clicks pass straight through it to whatever is underneath. While only the monitor is showing, the main window isn't drawn at all.
- **Tray.** Optional. Close to the tray, minimise to the tray, or let the window close the app like any other.
- **No admin prompt to start.** It runs with your normal rights. When something needs admin rights (ending a process another user owns, stopping a service), it asks Windows for them for that one action only, through the usual UAC prompt.
- **Honest numbers.** If Windows doesn't report something on your PC, such as a temperature or a GPU's memory, it says so instead of showing a zero.
- **Settings.** System, dark or light theme; your Windows accent colour or one of several others; interface scale; refresh rate (0.5 to 5 s) and history length (1 to 10 minutes); whether to keep monitoring, keep only history, or pause while hidden.

## Keys

| Key | What it does |
|---|---|
| Ctrl+1 … Ctrl+5 | Overview, Processes, Performance, Startup, Services |
| Ctrl+, | Settings |
| Ctrl+F | Search processes |
| Ctrl+M | Compact monitor |
| Ctrl+B | Narrow or widen the sidebar |
| F5 | Refresh now |

In the process table:

| Key | What it does |
|---|---|
| ↑ ↓, Page Up, Page Down, Home, End | Move; add Shift to select a range |
| → / ← | Expand / collapse a group |
| Enter | Details |
| Delete | End task |
| Shift+Delete | Force end |
| Ctrl+A / Ctrl+C | Select all / copy details |
| Esc | Back to the search box |

Right-click a process for priority, its services, its file location and properties, and copying its name, PID, path or command line.

## Install

Download **BeroTaskManager-&lt;version&gt;-Setup.exe** from [Releases](https://github.com/Beroris/task-manager/releases/latest) and run it. It installs for you only, so it doesn't ask for admin rights, and adds Bero Task Manager to the Start menu. Uninstall it from Settings → Apps like any other app. Startup entries you disabled with it stay disabled after uninstalling, because those were your choices; turn them back on in Windows Settings → Apps → Startup if you like.

To run it without installing, download **BeroTaskManager-&lt;version&gt;-Portable.exe** instead. Run it from anywhere, a USB stick included. It keeps its settings in a "Bero Task Manager settings" folder beside itself rather than in your profile.

It needs 64-bit Windows 10 version 1809 or newer, or Windows 11. So far it has been tested on Windows 11.

## Running it from other tools

Starting the exe again brings the copy that's already running to the front, so a shortcut or launcher never opens a second one.

```
bero-task-manager.exe               # open it, or bring it to the front
bero-task-manager.exe --background  # start in the tray without opening the window
bero-task-manager.exe --quit        # close the running copy
```

## Resource use

Measured on the release build on Windows 11 (Ryzen 5 5600X, 16 GB). See [docs/PERFORMANCE.md](docs/PERFORMANCE.md).

| | |
|---|---|
| Download | 8.2 MB installer, 22.6 MB portable exe, no runtime needed |
| Start to window on screen | 62 ms (warm) |
| Idle CPU, window open | 0.13–0.16% of the whole CPU |
| Idle CPU, paused in the tray | 0.00% |
| Memory | 14–17 MB private working set, 45–50 MB working set |

## Build

You need Rust 1.88 or newer.

```sh
cargo build --release -p tm-windows      # target/release/bero-task-manager.exe
cargo test --workspace
pwsh scripts/package-windows.ps1        # dist/: the installer and the portable exe (needs Inno Setup 6)
```

## Layout

| Path | What |
|---|---|
| `crates/tm-core` | Platform-neutral: the data model, sampling engine, history, insights, table building (grouping, tree, sorting, search), termination safety rules, settings and number formatting. No OS calls. |
| `crates/tm-app` | The interface, in Slint, and the controller that ties it to the engine. Talks to the OS only through traits. |
| `platforms/windows` | The Windows app: sampling (NtQuerySystemInformation, PDH, D3DKMT for the GPU), actions (ending processes, priorities, services, startup entries), elevation per action, tray, single instance. |
| `installer` | The setup wizard (Inno Setup), built by `scripts/package-windows.ps1`. |
| `assets/fonts` | Geist, the interface font, built into the exe (SIL Open Font License; see `Geist-LICENSE.txt`). |

A macOS version would reuse `tm-core` and `tm-app` and add a `platforms/macos`. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Privacy

Bero Task Manager never connects to the network. There's no account, no telemetry and no update check. Settings are in `%APPDATA%\Bero Task Manager\settings.json` (or beside the portable exe), and that's all it writes, apart from the Windows changes you ask it to make.

## Support

Bero Task Manager is free. If it helps, you can [support me on Ko-fi](https://ko-fi.com/beroris).

## License

GPL-3.0. See [LICENSE](LICENSE). The Geist font in `assets/fonts` keeps its own SIL Open Font License.

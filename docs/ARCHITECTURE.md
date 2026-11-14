# Architecture

Pulse has three layers. Only the bottom one knows it's running on Windows.

```
platforms/windows   pulse.exe: Windows counters, actions, tray, single instance
        │ implements Sampler, MetaResolver, SystemControl (tm-core) and Shell (tm-app)
crates/tm-app       the Slint interface and the controller that drives it
        │ uses
crates/tm-core      data model, engine, history, insights, table, safety rules, settings
```

## tm-core: no OS calls

Everything here is plain data and maths, unit tested without real hardware.

- `platform.rs` holds the traits a backend implements:
  - `Sampler` reports raw, cumulative counters: CPU times per processor, every process's counters, memory, disks, network and GPU.
  - `MetaResolver` looks up what doesn't change for a process's life: path, owner, description, publisher, icon.
  - `SystemControl` carries out actions: ending processes, priorities, services, startup entries, elevation.
- `engine.rs` is the collector. It samples on its own thread at the refresh interval and turns pairs of raw samples into rates (`counter.rs`, `cpu.rs`, `process.rs`). It keeps fixed-size history (`history.rs`), evaluates insights (`insights.rs`) and publishes a `Frame`. A second, low-priority thread runs the `MetaResolver`, so a slow disk never delays a sample. What gets sampled follows a `Demand` the UI sets, depending on what is on screen.
- `model.rs` keeps three kinds of data:
  - `Raw*` is what the OS reports;
  - `*Now` is what the engine computed;
  - `*Info` describes hardware.

  Anything a platform can't measure is `None`, and the UI shows that as unavailable, never as zero.
- `process.rs` keys processes by PID **and** creation time, so a recycled PID never inherits another process's counters.
- `table.rs` builds the process table: the grouped, tree and list views, sorting, search, and `keep_order`, which holds rows still while the pointer is over the table.
- `safety.rs` classifies a process before it's ended:
  - refused: kernel pseudo-processes, and processes whose loss stops Windows or signs you out;
  - allowed with a warning (caution);
  - normal.

  The backend repeats the check right before acting, including in the elevated helper.
- `update.rs` reads GitHub's answer about the latest release: whether it's newer than this copy, and which file this kind of copy installs from (Setup for an installed copy, the portable exe for a portable one, nothing for a build from source). Only plain semver tags count, and only files on github.com. The `UpdateHost` trait is what a platform does about it: ask, download, install, clean up.
- `settings.rs` stores preferences as JSON. Every field has a default, and bad values are clamped, so a damaged file never stops the app.

## tm-app: the interface

- `ui/*.slint` holds the views, one file per page plus shared widgets, theme and the global `App` state that the controller fills. `table.slint` has the scroll view, its thin overlay scrollbars and the resizable header cell that every table and page shares.
- Column widths: dragging an edge changes only that column. The column that fills the spare width (Name, or a table's main text column) gets a fixed width as soon as any drag starts, so it stops absorbing the change. A table wider than the view scrolls sideways. Widths and sort order are saved per table.
- Text whose width changes every tick (summaries, counts, button labels) is set to elide or given a minimum width. Otherwise the layout's minimum width would follow the text and Windows would widen the window to fit it.
- `ctrl.rs` is the controller. It runs on the UI thread, owns the main window, the compact monitor and the tray, and turns each new `Frame` into view data. It also carries out what the user asks for. Slow actions run on short-lived worker threads and post their results back through the event loop.
- `procs.rs`, `pages.rs` and `lists.rs` turn data into rows and cards. They hold no windows, so they're tested directly.
- The `Shell` trait (`lib.rs`) covers the few window details only a platform can provide: title bar colours, the accent colour, the animation and high-contrast settings, native menu theming, and handing memory back to the system once no window is open.

The UI never calls the OS for monitoring data. It only reads the collector's latest frame and histories.

## platforms/windows: the Windows backend

- `sampler/` reads the counters:
  - CPU and processes: `NtQuerySystemInformation`. One call returns every process, so no process has to be opened and no admin rights are needed.
  - Memory: `GlobalMemoryStatusEx` and `GetPerformanceInfo`, plus PDH for the standby and modified lists and the pools.
  - Disks: `IOCTL_DISK_PERFORMANCE`, through a handle opened with no access rights and closed straight away.
  - Network: `GetIfEntry2`. Only adapters that are hardware or have a usable address (not link-local) are shown, so WAN Miniports, filter drivers and idle virtual switches stay out of the list.
  - CPU temperature and power (`cpusensor.rs`): through PawnIO, if it's installed and the app is elevated, since only administrators can open its device. It loads the signed `AMDFamily17` or `IntelMSR` module built into the exe. On AMD it reads Tctl from SMN `0x59800`, decoded as Linux's k10temp does, while holding the `Global\Access_PCI` mutex other hardware tools use. On Intel it reads TjMax minus the thermal status readout. Power comes from the package energy counter on both. Otherwise the reading says why there is none (no driver, needs admin, unsupported CPU), and the UI shows that.
  - Frame rate (`fps.rs`): a real-time ETW session on the DXGI, Direct3D 9 and DxgKrnl providers, filtered by event ID to presents only. It counts presents per process over the last second. The app shown is the one in front, or the child process drawing for it (browsers and Electron apps), or the one shown last while the app in front draws nothing. The session only runs while FPS is on screen. A session outlives its process, so it's stopped on pause and on exit, and a leftover one is replaced. It needs administrator rights. Windows' rules also allow Performance Log Users, but Windows 11 has refused them a real-time session unless elevated.
  - GPU: D3DKMT statistics, the same source Windows Task Manager reads, plus DXGI for names and sizes. Reading an engine is slow on some drivers, so engines that have been idle for a while are read every few seconds instead of every tick, unless the GPU view is open.
- `meta.rs` reads paths, owners, version info and icons.
- `control.rs` carries out actions:
  - End task posts `WM_CLOSE` to the program's windows. Force end is `TerminateProcess`.
  - When Windows answers "access denied", the action comes back as `NeedsElevation`. If you agree, the same action runs again in a short-lived elevated copy of the exe (`--admin-action`). That copy repeats the safety checks, then exits. The app itself stays unelevated.
  - Clear standby enables `SeProfileSingleProcessPrivilege`, which only an elevated token holds, and asks `NtSetSystemInformation` to purge the standby list. Unelevated, it goes through the same `--admin-action` copy.
- `taskmgr.rs` makes Pulse open in place of Task Manager: the `Debugger` value of taskmgr.exe's Image File Execution Options, which Windows starts instead, with Task Manager's command line after it. That covers Ctrl+Shift+Esc, the taskbar menu and Ctrl+Alt+Del. The key is under HKLM, so on and off go through the `--admin-action` copy, and off only removes a value that points at this exe. `main.rs` treats `/startup` in that command line as a request for the Startup page. The uninstaller turns it off first, since the value would otherwise point at nothing.
- `update.rs` is the `UpdateHost`: WinHTTP for the API and the download, which must match the size and SHA-256 GitHub lists, and BCrypt for the hash. An installed copy runs the new Setup with `/UPDATE`, which keeps the user's choices, then starts Pulse again with `--updated`. A portable copy renames its running exe aside, moves the new one into its place and starts it with `--after-update <pid>`, which waits for the old one to exit before claiming the single-copy mutex. When to look is up to `ctrl.rs`: 8 s after starting, then once a day by the wall clock, on its own worker thread. A failed look the user didn't ask for stays quiet and is tried again an hour later.
- `startup.rs` covers the Run keys, the Startup folders and Store apps' startup tasks. Disabling writes the same `StartupApproved` flag Windows uses, so nothing is ever deleted.
- `services.rs` talks to the Service Control Manager.
- `instance.rs` keeps one copy running (development builds use their own names, so one can run beside an installed copy): a named mutex, plus a message-only window that receives `show`, `startup` and `quit` from later launches. It accepts them from unelevated senders too, for a copy running as administrator (restarted that way, or started elevated in place of Task Manager). If a `quit` hasn't closed the app within 5 s, the app ends itself anyway, so an installer never waits on a copy that has stopped responding.
- `main.rs` ends the process with `TerminateProcess` once the event loop returns and settings are saved. A normal exit tears down thread-locals after Windows has already stopped the worker threads. The collector's handle then waits on threads that are gone and panics, which made every exit a crash.
- `shell.rs` sets the DWM title bar and border colours, reads the theme and accent, and themes native menus.

## Porting to macOS

A macOS version would add `platforms/macos`, implementing the same four traits:

- `Sampler`: `host_processor_info`, `proc_pidinfo`, `vm_statistics64`, IOKit for disks and GPUs, `getifaddrs` for network.
- `MetaResolver`: bundle names and icons.
- `SystemControl`: ending processes, launchd jobs instead of services, login items instead of Run keys.
- `Shell`: appearance and accent colour.

tm-core and tm-app would be reused as they are. Some Windows ideas have no direct macOS match, so these parts of the UI would need a macOS version or would be hidden there:

- per-process GPU engines;
- the startup sources;
- services.

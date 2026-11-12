# Architecture

Bero Task Manager has three layers. Only the bottom one knows it's running on Windows.

```
platforms/windows   bero-task-manager.exe: Windows counters, actions, tray, single instance
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
- `settings.rs` stores preferences as JSON. Every field has a default, and bad values are clamped, so a damaged file never stops the app.

## tm-app: the interface

- `ui/*.slint` holds the views, one file per page plus shared widgets, theme and the global `App` state that the controller fills.
- `ctrl.rs` is the controller. It runs on the UI thread, owns the main window, the compact monitor and the tray, and turns each new `Frame` into view data. It also carries out what the user asks for. Slow actions run on short-lived worker threads and post their results back through the event loop.
- `procs.rs`, `pages.rs` and `lists.rs` turn data into rows and cards. They hold no windows, so they're tested directly.
- The `Shell` trait (`lib.rs`) covers the few window details only a platform can provide: title bar colours, the accent colour, the animation and high-contrast settings, and native menu theming.

The UI never calls the OS for monitoring data. It only reads the collector's latest frame and histories.

## platforms/windows: the Windows backend

- `sampler/` reads the counters:
  - CPU and processes: `NtQuerySystemInformation`. One call returns every process, so no process has to be opened and no admin rights are needed.
  - Memory: `GlobalMemoryStatusEx` and `GetPerformanceInfo`, plus PDH for the standby and modified lists and the pools.
  - Disks: `IOCTL_DISK_PERFORMANCE`, through a handle opened with no access rights and closed straight away.
  - Network: `GetIfEntry2`.
  - GPU: D3DKMT statistics, the same source Windows Task Manager reads, plus DXGI for names and sizes. Reading an engine is slow on some drivers, so engines that have been idle for a while are read every few seconds instead of every tick, unless the GPU view is open.
- `meta.rs` reads paths, owners, version info and icons.
- `control.rs` carries out actions:
  - End task posts `WM_CLOSE` to the program's windows. Force end is `TerminateProcess`.
  - When Windows answers "access denied", the action comes back as `NeedsElevation`. If you agree, the same action runs again in a short-lived elevated copy of the exe (`--admin-action`). That copy repeats the safety checks, then exits. The app itself stays unelevated.
- `startup.rs` covers the Run keys, the Startup folders and Store apps' startup tasks. Disabling writes the same `StartupApproved` flag Windows uses, so nothing is ever deleted.
- `services.rs` talks to the Service Control Manager.
- `instance.rs` keeps one copy running: a named mutex, plus a message-only window that receives `show` and `quit` from later launches.
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

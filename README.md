<img src="assets/icon.png" width="64" height="64" alt="">

# Pulse

A fast, quiet task manager for Windows. See what's using your CPU, memory, disk, GPU and network, end what's stuck, and tidy what starts with Windows, in a window that opens in under 50 ms, uses less than 0.1% of your CPU while you watch it, and shrinks to a few MB of memory when you close it to the tray.

It's part of the Bero suite, next to Prism, Orbit and Spotlight: same font, same dark-first look.

## What it does

- **Overview.** Live cards for CPU, memory, disks, GPU and network, plus a short list of plain facts worth knowing: memory nearly full, one program taking a big share of the CPU, a disk that has been flat out for a while, an app whose memory keeps growing, a drive running out of space. Every insight is computed from real readings; if there's nothing to say, it says nothing.
- **Processes.** Grouped into apps, background processes and Windows processes, or shown as a parent/child tree, or as a flat list. Sort by any column, pick from 18 columns (GPU engine, command line, publisher, commit size and more), resize them, search as you type. Double-click a row for its details in any view. Memory reads as 2.03 GB, or as 2,080.4 MB if you'd rather every row compared in one unit. Rows hold still while your pointer is over the table, so the thing you're about to click doesn't jump away.
- **End task, gently.** End task asks a program to close the way its own close button would, so it can offer to save your work. If it doesn't close, you're told, and **Force end** is a separate, clearly marked step. Processes that would crash or sign you out of Windows (csrss, wininit, lsass and friends) are refused outright, and the check is repeated right before acting.
- **Performance.** Graphs and the numbers behind them for each CPU (overall or per core), memory (in use, committed, cached, pools, speed, slots), every GPU and its engines, every disk (active time, response time, read and write speed) and every network adapter. The memory view can also clear the standby list, with an honest note about what that costs.
- **Startup.** Everything that starts when you sign in: both Run keys, both Startup folders and Store apps' startup tasks. Sort and resize the columns, and filter by enabled or disabled and by where each one is registered. Disabling uses the same switch Windows Task Manager and Settings use, so nothing is ever deleted and you can turn it back on from any of the three.
- **Services.** Start, stop and restart services and change their startup type, with a clear warning on the ones Windows relies on. Sort and resize the columns, and filter by running or stopped and by startup type.
- **Compact monitor.** A small, rounded, always-on-top readout of the numbers you pick: FPS, CPU (with its temperature, when it can be read), memory, GPU and its temperature, GPU memory, disk and network. Make it see-through, or let clicks pass straight through it to whatever is underneath. While only the monitor is showing, the main window isn't drawn at all.
- **FPS.** The frame rate of the app in front, counted from the same Windows graphics events Xbox Game Bar and PresentMon use. It works for DirectX 9 to 12, Vulkan and OpenGL games, and for browsers. Windows only allows it for programs running as administrator, so use **Run as administrator** in the sidebar, or start Pulse that way: it takes over from a copy that's running without it. The counting only runs while FPS is on screen.
- **Tray.** Optional. Close to the tray, minimise to the tray, or let the window close the app like any other.
- **Ctrl+Shift+Esc.** Optional. Turn on **Open with Ctrl+Shift+Esc** in Settings and Pulse opens wherever Task Manager would: Ctrl+Shift+Esc, the taskbar's menu, Ctrl+Alt+Del and `taskmgr`. Windows' links to Task Manager's startup apps open Pulse's Startup page. See [Opening with Ctrl+Shift+Esc](#opening-with-ctrlshiftesc) for how it works and how to undo it by hand.
- **No admin prompt to start.** It runs with your normal rights. When something needs admin rights (ending a process another user owns, stopping a service), it asks Windows for them for that one action only, through the usual UAC prompt.
- **Honest numbers.** If Windows doesn't report something on your PC, such as a temperature or a GPU's memory, it says so instead of showing a zero. CPU temperature is the usual one: Windows only reports it on PCs whose firmware has an ACPI thermal zone, which many desktop boards don't.
- **CPU temperature and power, through PawnIO.** Reading the CPU's own sensor takes a kernel driver. This app won't install one, but if [PawnIO](https://pawnio.eu) is already there (LibreHardwareMonitor and OpenRGB install it, for example), Pulse uses it when you run it as administrator. That gives the CPU's temperature (Tctl on Ryzen) and its package power. PawnIO is signed, and it runs only signed modules that each allow a fixed list of reads. The two modules used here are built in, and only their read functions are called.
- **Updates.** Shortly after it starts and once a day, Pulse asks GitHub whether there's a newer version. If there is, it downloads in the background and **Update to** the new version shows in the sidebar. One click installs it, and Pulse opens again a few seconds later. You can turn the check off in Settings.
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
| Enter or double-click | Details (on a group: its main process) |
| Delete | End task |
| Shift+Delete | Force end |
| Ctrl+A / Ctrl+C | Select all / copy details |
| Esc | Back to the search box |

Right-click a process for priority, its services, its file location and properties, and copying its name, PID, path or command line. Drag a column's edge to resize it. Only that column changes, and the table scrolls sideways if it gets wider than the window. **Reset column widths** is in the ⋯ menu here, and in the header's right-click menu on Startup and Services.

## Install

Download **Pulse-&lt;version&gt;-Setup.exe** from [Releases](https://github.com/Beroris/pulse/releases/latest) and run it. It installs for you only, so it doesn't ask for admin rights, and adds Pulse to the Start menu. Uninstall it from Settings → Apps like any other app. Startup entries you disabled with it stay disabled after uninstalling, because those were your choices; turn them back on in Windows Settings → Apps → Startup if you like.

To run it without installing, download **Pulse-&lt;version&gt;-Portable.exe** instead. Run it from anywhere, a USB stick included. It keeps its settings in a "Pulse settings" folder beside itself rather than in your profile.

It needs 64-bit Windows 10 version 1809 or newer, or Windows 11. So far it has been tested on Windows 11.

## Opening with Ctrl+Shift+Esc

Windows keeps Ctrl+Shift+Esc for Task Manager, but it lets you choose a program to start in place of Task Manager. Process Explorer's "Replace Task Manager" uses the same setting: a `Debugger` value under

```
HKEY_LOCAL_MACHINESOFTWAREMicrosoftWindows NTCurrentVersionImage File Execution Options	askmgr.exe
```

that holds the path of Pulse's exe. A few things follow from that:

- Turning it on or off asks for administrator rights, because the setting is for the whole PC.
- It applies to every account on the PC. Pulse installs into your own profile, so another account that can't open that folder gets no Task Manager at all while it's on.
- Uninstalling Pulse turns it off first. A portable copy can't do that for you: turn it off before you move or delete the copy, or Ctrl+Shift+Esc will open nothing.
- If Ctrl+Shift+Esc ever opens nothing, the setting points at a copy that has gone. Turn the switch on in any copy of Pulse to point it there, or delete the value yourself from an administrator terminal:

  ```
  reg delete "HKLMSOFTWAREMicrosoftWindows NTCurrentVersionImage File Execution Options	askmgr.exe" /v Debugger /f
  ```

## Running it from other tools

Starting the exe again brings the copy that's already running to the front, so a shortcut or launcher never opens a second one.

```
pulse.exe               # open it, or bring it to the front
pulse.exe --background  # start in the tray without opening the window
pulse.exe --quit        # close the running copy
```

## Resource use

Measured on the release build on Windows 11 (Ryzen 5 5600X, 16 GB). See [docs/PERFORMANCE.md](docs/PERFORMANCE.md).

| | |
|---|---|
| Download | 7.6 MB installer, 17.6 MB portable exe, no runtime needed |
| Start to window on screen | 46 ms (warm) |
| Idle CPU, window open | 0.06–0.09% of the whole CPU |
| Idle CPU, in the tray and still monitoring | 0.03% |
| Idle CPU, paused in the tray | 0.00% |
| Memory, window open | 12–14 MB in Task Manager's Memory column, 13–15 MB committed |
| Memory, in the tray | 2–4 MB in the Memory column, 9–14 MB committed |
| Memory, paused in the tray | under 1 MB in the Memory column |

## Build

You need Rust 1.88 or newer.

```sh
cargo build --release -p tm-windows      # target/release/pulse.exe
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
| `assets/pawnio` | The two signed PawnIO modules that read CPU sensors, built into the exe (LGPL-2.1; see its README). |

A macOS version would reuse `tm-core` and `tm-app` and add a `platforms/macos`. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Privacy

The only network access is the update check. Pulse asks GitHub for its latest release, and if it's newer, downloads it from GitHub and checks it against the size and SHA-256 GitHub lists. Nothing about you or your PC is sent. You can turn the check off in Settings, under About. There's no account and no telemetry. Settings are in `%APPDATA%\Pulse\settings.json` (or beside the portable exe), and that's all it writes, apart from a downloaded update until it's installed, and the Windows changes you ask it to make.

## Support

Pulse is free. If it helps, you can [support me on Ko-fi](https://ko-fi.com/beroris).

## License

GPL-3.0. See [LICENSE](LICENSE). The Geist font in `assets/fonts` keeps its own SIL Open Font License, and the PawnIO modules in `assets/pawnio` their LGPL-2.1.

# Changelog

All notable changes to rtop are in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and rtop uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

The first public release will be 0.2.0. Versions 0.1.0 and 0.1.1 were private builds.

## 0.2.0

Feature round 1. A user wrote: "none of them report ZFS ARC stats, so I gave up and went back to top." This release adds ZFS ARC stats, and more.

### Added

- **ZFS ARC panel.** ARC size, target and maximum, MRU and MFU split, data and metadata, compression ratio, hit, I/O-hit, miss and demand hit ratios over a 5 s window (since boot on the first reading, or when there were no reads or a counter reset), and L2ARC size and hit ratio when an L2ARC is present. Reads `/proc/spl/kstat/zfs/arcstats` on Linux and `kstat.zfs.misc.arcstats.*` by sysctl on FreeBSD and macOS. The panel shows only when ZFS is loaded.
- **Storage panel.** Used and total space for each filesystem, and read and write rates for each disk. Pseudo filesystems, snap mounts and mount points that are files (such as the `/etc/hosts` that Docker bind-mounts) are hidden, and ZFS child datasets fold into their pool.
- **Sensors panel.** Temperatures from the hardware sensors, in Celsius or Fahrenheit. Battery level and state on Linux, macOS and Windows.
- **Process tree view** (`t`, or `--tree`), with branch lines. Siblings follow the current sort.
- **Kill with a confirm prompt** (`x` or `F9`, then `y` for TERM or `f` for KILL). rtop checks the PID and start time again before it sends the signal, and refuses PID 0, PID 1, itself and (on Windows) PID 4.
- **Process columns:** user, state, thread count (Linux and macOS), per-process disk read and write rates, and the full command line (`c`).
- **Reverse sort** (`r`) and an I/O sort column.
- **Per-interface network view** (`n`): receive and transmit rates for each interface.
- **Adaptive layout.** Panels collapse in a fixed order when the terminal is small, and the footer shows how many are hidden. The CPU panel adds core columns when it is short. The process table drops columns on narrow terminals.
- **Panel toggles:** `1` to `8` show or hide each panel, and `0` shows them all.
- **Help overlay** (`?` or `F1`). It and `--help` read the same key table.
- **Config file** at `$XDG_CONFIG_HOME/rtop/config.toml`, `~/.config/rtop/config.toml` or `%APPDATA%\rtop\config.toml`. Keys: `interval_ms`, `sort`, `reverse`, `tree`, `command`, `panels`, `temp_unit`, `network`. Unknown keys give a warning; a bad value stops rtop before the TUI starts.
- **CLI flags:** `-c`/`--config <path>`, `--no-config`, `-t`/`--tree` and `-V`/`--version`.
- On Linux with ZFS, the memory gauge counts the shrinkable part of the ARC as free memory, and the memory panel shows the ARC size.
- A debug-only `RTOP_ZFS_ARCSTATS` variable reads ARC counters from a file, for tests.

### Changed

- The release profile builds with `opt-level = "s"`. The binary is smaller, with no measured CPU change.
- One shared copy of the stable sort code serves every sorted list, to keep the binary small.
- On Linux, network byte counters come from `/sys/class/net/<name>/statistics`, not from `sysinfo`.
- On Windows, kill opens the process, checks its creation time and calls `TerminateProcess` on the same handle. TERM is not supported on Windows.

### Fixed

- A crash (SIGSEGV) in the sampler on Linux musl builds, in `sysinfo`'s interface address code. rtop no longer calls it on Linux.
- On Windows, kill no longer runs `taskkill.exe` by bare name, which could run a planted `taskkill.exe` from the rtop directory or kill a reused PID.
- Typed or pasted text could open and confirm the kill prompt before it was drawn. Now any other key or a paste cancels the prompt, and `y` and `f` act only after the prompt is on screen.
- Deep process trees made the tree prefix grow without limit. The prefix now stops at 32 levels.
- On Linux, rtop no longer reads the status of device batteries (for example a Bluetooth mouse), which could stall the sampler for seconds.
- On Linux, disk I/O reads `/proc/diskstats` once for each device on each tick, not once for each mount.
- Panels in a split row keep their minimum width.
- Memory, read and write columns are wide enough for four-digit values.

## 0.1.1 - 2026-10-16 (private)

### Fixed

- The Windows binary links the C runtime statically, so it runs without the Visual C++ Redistributable. The release workflow fails if the exe needs `VCRUNTIME140.dll` again.

## 0.1.0 - 2026-10-16 (private)

### Added

- First release: a terminal system monitor in Rust with ratatui and sysinfo.
- CPU use for each core, a CPU and memory history chart, RAM and swap gauges, network rates with sparklines, and a process table that you can sort and filter.
- Pause, and a refresh interval from 250 to 8000 ms (`-i`, `+` and `-`).
- Release binaries for macOS (universal), Linux x86_64 and arm64 (static musl), and Windows x86_64.

[Unreleased]: https://github.com/DarkWaveLabs/rtop/commits/main

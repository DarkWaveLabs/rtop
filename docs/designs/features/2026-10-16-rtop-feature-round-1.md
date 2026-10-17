# rtop feature round 1: design

Date: 2026-10-16. Status: ready for implementation (revision 2), with one decision open for the maintainer (§7.6). Plan: `docs/plans/features/2026-10-16-rtop-feature-round-1.md`.

This document uses ASD-STE100 Simplified Technical English and en-ZA spelling.

### Revision 2 (after review, 2026-10-16)

- Layout: `PROC_MIN_H` is 9 (6 process rows, as today). Cpu `min_h` is 4. The height pass removes in strict `COLLAPSE_ORDER`, then puts back panels that cost no height. Spare height goes in reverse `COLLAPSE_ORDER`. The CPU panel adds core columns when it is short (§6.10, §7.5).
- Storage I/O: fast ticks refresh each kept disk, not the disk list. Rates are summed once for each `io_group` (macOS: the whole disk). ZFS volumes show `n/a`. The first-sample set is keyed by (device, mount) (§6.3).
- Per-target code: a `cfg` table replaces "compiled on every OS" (§5.5).
- `sysctl_u64` uses `std::ptr::null_mut()` (§6.1).
- S0 commits `docs/` and the ZFS fixtures, which are now in Appendix A.
- Linux thread count adds the main thread (§6.7.1). `D` state maps to `DiskSleep` (§6.7).
- ARC hit ratios use a 5 s window, with a since-boot fallback that the panel labels (§6.1, §6.2).
- Kill target comes from the row drawn on screen, not from the selection index at key time (§7.3, §8).
- Command lines refresh on slow ticks only. `exe` is not refreshed (§6.7).
- Smaller fixes: FreeBSD `/` is never dropped as a ZFS child; `config::load` takes the environment getter; CRLF test; IOKit keys are `&CStr`; `crate::process::visible_rows` in `ui.rs`; the ZFS hit line drops `dd` and `dm` first.
- Open for the maintainer: on Linux, count shrinkable ARC as free memory (§7.6).

## 1. Purpose

A user wrote: "none of them report ZFS ARC stats, so I gave up and went back to top". This round adds four feature groups to rtop:

- **A. Storage.** A ZFS ARC panel. Disk I/O rates. Filesystem usage.
- **B. Process power.** A tree view. Kill with a confirm prompt. User, state and thread columns. The full command line. Disk I/O for each process.
- **C. Hardware sensors.** Temperatures. Battery level and state.
- **D. Layout and config.** Keys that show and hide panels. A help overlay. A view for each network interface. An optional config file.

Several agents build the groups at the same time. Thus each group lives in new modules with exact interfaces. One integration step then connects the modules.

## 2. Decisions

| Topic | Decision | Reason |
|---|---|---|
| Module shape | One data module and one UI file for each feature. `ui.rs` gets child modules (`src/ui/<x>.rs`). | Disjoint file ownership. Child modules can use the private palette and `panel()` in `ui.rs`. |
| Shared types | Only `src/panel.rs` (written in Stage 0, given in full below) is shared. | Stage 1 tasks do not depend on each other. |
| Kill path | The UI sends a `KillRequest` to the sampler on a second channel. The sampler checks the target and calls `sysinfo` `Process::kill_with`. The result comes back as `Event::KillDone`. | The sampler owns `System`. The sampler can check PID and start time against fresh data. No locks. |
| Slow refresh | The sampler sets `slow = true` on the first tick and then once each 5 s. The disk list, disk space, sensors, battery, user list reload, command lines and macOS thread counts refresh only on a slow tick. | Sampling stays cheap at a 250 ms interval. |
| Command line, user | `ProcessRefreshKind` uses `UpdateKind::OnlyIfNotSet` for `user` on every tick, and for `cmd` on slow ticks only. The collector caches the joined command as `Arc<str>` by (pid, start time). | `OnlyIfNotSet` re-reads an empty command line (kernel threads, zombies) on each call (`sysinfo` `linux/process.rs:520`). On slow ticks only, that cost is bounded. |
| Disk I/O | Fast ticks call `Disk::refresh_specifics` with I/O only, on each kept disk. Slow ticks re-list. The total counts each `io_group` once. | `Disks::refresh_specifics` re-reads `/proc/mounts` (Linux) or runs `getfsstat` plus CFURL fetches (macOS) on each call. macOS volumes of one APFS container share one I/O counter. |
| ZFS on Linux | Read `/proc/spl/kstat/zfs/arcstats` on each tick. Parse with a pure function. | One small procfs read. Hit ratio needs deltas over a window. |
| ZFS on FreeBSD, macOS | `libc::sysctlbyname` on `kstat.zfs.misc.arcstats.<field>`. | No new crate. `libc` 0.2.190 is already in the lock file. |
| ZFS on Windows | Not supported. The panel never shows. | No known stats interface. |
| Battery | Hand-written readers: Linux sysfs, macOS IOKit power sources (`objc2-io-kit` feature `ps`), Windows `GetSystemPowerStatus` (`windows` 0.62). FreeBSD: none. | No new crates. Only new features of crates in the lock file. |
| Config | Hand-written parser for a flat TOML subset. | About 150 lines. `toml` would add about five crates and `serde`. `serde` is not compiled today. |
| CLI | Keep the hand-written parser. Add `--config`, `--no-config`, `--tree`, `--version`. | Four flags do not justify a crate. |
| Layout | A pure function `layout::plan` decides panel areas and auto-collapse. | Testable without a terminal. Satisfies 80×24. |
| Threads column | Linux: `Process::tasks()` length + 1 (the list leaves out the main thread). macOS: `libc::proc_pidinfo(PROC_PIDTASKINFO)` on slow ticks. FreeBSD and Windows: no value (`-`). | `sysinfo` gives tasks on Linux only. |
| State column | The collector maps the state on every OS. Windows hides the column. | `sysinfo` on Windows always reports `Run`. One code path for every OS gives no per-OS dead code. |
| Memory gauge with ZFS on Linux | Recommended, pending the maintainer (§7.6): subtract the shrinkable ARC (`size − c_min`) from used memory. | `sysinfo` reports Linux used memory as `total − MemAvailable`. FreeBSD already subtracts ARC in `sysinfo`. |

### Not built in this round

- Fold and unfold of tree branches.
- Selection that follows a PID across re-sorts. The kill key takes the row that was drawn last (§7.3), and the prompt freezes that target, so this is safe.
- I/O rates for ZFS datasets (OpenZFS `objset-*` kstats). ZFS volumes show `n/a`.
- History charts for each network interface, disk or temperature.
- Battery time remaining and power draw. The Linux units are not verified.
- Thread counts on Windows and FreeBSD.
- ZFS pool and dataset stats (only ARC and L2ARC).
- A ZFS reader for Windows.
- Columns that the user can choose. Columns follow the terminal width.
- Key remapping in the config file.
- README changes. A later task updates the README.
- CI changes for the cross-target checks. A later task can add them.

## 3. Architecture after the change

```text
 sampler thread  ──Event::Metrics(Snapshot)──┐
   owns System, Networks,                    │
   ProcCollector, StorageCollector,          ├──► mpsc<Event> ──► UI thread (owns App, draws)
   ZfsCollector, SensorCollector             │                      │
   ──Event::KillDone(KillReport)─────────────┤                      │
 input thread    ──Event::Key / Resize───────┘                      │
                                                                    │
 sampler thread  ◄──────── mpsc<KillRequest> ───────────────────────┘
```

- The sampler owns all `sysinfo` handles and all collectors. It moves one `Snapshot` into the channel on each tick.
- The UI thread never reads `/proc`, `sysctl`, IOKit or WMI. It only reads the config file once, before the TUI starts.
- The sampler checks the kill channel with `try_recv` on each 50 ms sleep step. Thus a kill starts within 50 ms.

### Module map

| File | Owner (Stage) | Contents |
|---|---|---|
| `src/panel.rs` | Stage 0 | `PanelId`, `PanelSet`. |
| `src/zfs.rs` | T1 | `ArcRaw`, `ZfsArc`, `L2Arc`, `parse_kstat`, `from_pairs`, `derive`, `arc_adjusted_used`, `HIT_WINDOW`, `ZfsCollector`. |
| `src/ui/zfs.rs` | T1 | `draw_zfs`, size constants. |
| `testdata/arcstats-2.2.txt`, `testdata/arcstats-2.2-next.txt` | Stage 0 (from Appendix A) | Fixtures. T1 tests load them with `include_str!`. Frozen after Stage 0. |
| `src/storage.rs` | T2 | `Volume`, `Storage`, `StorageCollector`, `keep_volume`, `drop_zfs_children`, `dedupe_rates`, `apple_whole_disk`. |
| `src/ui/storage.rs` | T2 | `draw_storage`, size constants. |
| `src/sensors.rs` (may add `src/sensors/*.rs`) | T3 | `Temp`, `Battery`, `BatteryState`, `Sensors`, `SensorCollector`, battery parsers. |
| `src/ui/sensors.rs` | T3 | `draw_sensors`, size constants. |
| `src/process.rs` | T4 | `ProcInfo`, `ProcState`, `SortKey`, `ProcView`, `ProcRow`, `visible_rows`, tree, `ProcCollector`, kill types and `send_signal`. |
| `src/ui/process.rs` | T4 | `ProcTable`, `Column`, `columns_for_width`, `draw_processes`, `draw_kill_prompt`. |
| `src/net.rs` | T5 | `NetIface`, `collect`, `keep_iface`, `totals`. |
| `src/ui/net.rs` | T5 | `draw_interfaces`, size constants. |
| `src/layout.rs` | T6 | `PanelReq`, `LayoutPlan`, `plan`, `COLLAPSE_ORDER`, `PROC_MIN_H`. |
| `src/config.rs` | T7 | `Config`, `ParseError`, `ConfigSource`, `parse`, `default_path`, `load`. |
| `src/help.rs` | T8 | `KeyHelp`, `KeySection`, `KEYMAP`, `usage_keys`. |
| `src/ui/help.rs` | T8 | `draw_help`. |
| `src/main.rs`, `src/metrics.rs`, `src/app.rs`, `src/ui.rs` | Stage 2 | Integration only. |
| `Cargo.toml`, `Cargo.lock` | Stage 0 | Frozen after Stage 0. |
| `docs/designs/features/…`, `docs/plans/features/…` | Stage 0 commits them | Untracked before Stage 0. Worktrees need them. |

## 4. Dependencies

Stage 0 adds these lines to `Cargo.toml`. They add **no new crates** to `Cargo.lock`. They add direct edges and features to crates that `sysinfo` already pulls in.

```toml
[target.'cfg(any(target_os = "macos", target_os = "freebsd"))'.dependencies]
libc = "0.2.190"

[target.'cfg(target_os = "macos")'.dependencies]
objc2-core-foundation = { version = "0.3.2", default-features = false, features = ["std", "CFArray", "CFBase", "CFDictionary", "CFNumber", "CFString"] }
objc2-io-kit = { version = "0.3.2", default-features = false, features = ["std", "libc", "ps"] }

[target.'cfg(windows)'.dependencies]
windows = { version = "0.62", features = ["Win32_System_Power"] }
```

Justification:

- `libc`: `sysctlbyname` (ZFS on macOS and FreeBSD) and `proc_pidinfo` (macOS threads). Already compiled for `sysinfo`.
- `objc2-io-kit` feature `ps`: `IOPSCopyPowerSourcesInfo`, `IOPSCopyPowerSourcesList`, `IOPSGetPowerSourceDescription`. The feature adds only more `objc2-core-foundation` features. `CFBoolean` is in the `CFNumber` feature.
- `windows` feature `Win32_System_Power`: `GetSystemPowerStatus`. `sysinfo`'s `system` feature already enables it. The direct entry makes the dependency explicit.

Size budget: the stripped release binary on `aarch64-apple-darwin` must not grow by more than 150 KiB against the Stage 0 baseline. Stage 0 records the baseline. Stage 3 measures it again.

## 5. Shared code written in Stage 0

### 5.1 `src/panel.rs` (complete file)

```rust
//! Panel identities and the set of visible panels. Layout, config and app share these types.

/// One panel on the screen. The order here is the key order (1 to 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PanelId {
    Cpu,
    History,
    Memory,
    Network,
    Storage,
    Zfs,
    Sensors,
    Processes,
}

impl PanelId {
    pub const ALL: [PanelId; 8] = [
        PanelId::Cpu,
        PanelId::History,
        PanelId::Memory,
        PanelId::Network,
        PanelId::Storage,
        PanelId::Zfs,
        PanelId::Sensors,
        PanelId::Processes,
    ];

    /// The name in the config file and in messages.
    pub fn name(self) -> &'static str {
        match self {
            PanelId::Cpu => "cpu",
            PanelId::History => "history",
            PanelId::Memory => "memory",
            PanelId::Network => "network",
            PanelId::Storage => "storage",
            PanelId::Zfs => "zfs",
            PanelId::Sensors => "sensors",
            PanelId::Processes => "processes",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    /// The digit key that shows or hides the panel: '1' to '8'.
    pub fn key(self) -> char {
        char::from(b'1' + self as u8)
    }

    pub fn from_key(c: char) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.key() == c)
    }

    fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// A set of panels, stored as a bit mask. `Default` is every panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelSet(u16);

impl PanelSet {
    pub const NONE: PanelSet = PanelSet(0);
    pub const ALL: PanelSet = PanelSet((1 << PanelId::ALL.len()) - 1);

    pub fn contains(self, id: PanelId) -> bool {
        self.0 & id.bit() != 0
    }

    pub fn insert(&mut self, id: PanelId) {
        self.0 |= id.bit();
    }

    pub fn remove(&mut self, id: PanelId) {
        self.0 &= !id.bit();
    }

    pub fn toggle(&mut self, id: PanelId) {
        self.0 ^= id.bit();
    }

    pub fn iter(self) -> impl Iterator<Item = PanelId> {
        PanelId::ALL.into_iter().filter(move |&p| self.contains(p))
    }
}

impl Default for PanelSet {
    fn default() -> Self {
        Self::ALL
    }
}

impl FromIterator<PanelId> for PanelSet {
    fn from_iter<I: IntoIterator<Item = PanelId>>(iter: I) -> Self {
        let mut set = PanelSet::NONE;
        for id in iter {
            set.insert(id);
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_names_round_trip() {
        for id in PanelId::ALL {
            assert_eq!(PanelId::from_key(id.key()), Some(id));
            assert_eq!(PanelId::from_name(id.name()), Some(id));
        }
        assert_eq!(PanelId::Cpu.key(), '1');
        assert_eq!(PanelId::Processes.key(), '8');
        assert_eq!(PanelId::from_key('9'), None);
    }

    #[test]
    fn set_operations() {
        let mut set = PanelSet::ALL;
        set.toggle(PanelId::Zfs);
        assert!(!set.contains(PanelId::Zfs));
        set.insert(PanelId::Zfs);
        assert_eq!(set, PanelSet::ALL);
        let two: PanelSet = [PanelId::Cpu, PanelId::Memory].into_iter().collect();
        assert_eq!(two.iter().collect::<Vec<_>>(), [PanelId::Cpu, PanelId::Memory]);
    }
}
```

### 5.2 Module declarations

In `src/main.rs`, after `mod ui;`:

```rust
// Round 1 modules. Stage 2 removes each `allow` when it connects the module.
#[allow(dead_code)]
mod config;
#[allow(dead_code)]
mod help;
#[allow(dead_code)]
mod layout;
#[allow(dead_code)]
mod net;
#[allow(dead_code)]
mod panel;
#[allow(dead_code)]
mod process;
#[allow(dead_code)]
mod sensors;
#[allow(dead_code)]
mod storage;
#[allow(dead_code)]
mod zfs;
```

In `src/ui.rs`, after the `use` lines:

```rust
// Round 1 panels. Stage 2 removes each `allow` when it connects the panel.
#[allow(dead_code)]
mod help;
#[allow(dead_code)]
mod net;
#[allow(dead_code)]
mod process;
#[allow(dead_code)]
mod sensors;
#[allow(dead_code)]
mod storage;
#[allow(dead_code)]
mod zfs;
```

A lint attribute on a `mod` item applies to all items in the module. Each stub file holds one `//!` line only.

### 5.3 Test helper in `src/ui.rs`

Stage 0 adds this function at module level in `src/ui.rs`. Every `src/ui/*.rs` test uses it as `super::render_to_string`.

```rust
/// Renders `draw` into a test terminal and returns the screen as text, one line for each row.
#[cfg(test)]
pub(crate) fn render_to_string(width: u16, height: u16, draw: impl FnOnce(&mut Frame)) -> String {
    use ratatui::{Terminal, backend::TestBackend};
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(draw).unwrap();
    let buffer = terminal.backend().buffer();
    buffer
        .content()
        .chunks(width as usize)
        .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}
```

Stage 0 also changes the body of the existing test helper `render` in the `ui.rs` tests to `render_to_string(width, height, |frame| draw(frame, app))`. Without a caller, `render_to_string` gives a dead-code warning in the test build, and `clippy -D warnings` fails.

### 5.4 Rules for `src/ui/*.rs` files

- Use the parent's private items: `use super::{panel, heat, PURPLE, VIOLET, PINK, BLUE, DIM, FAINT, SELECTED, INK};`. Import only what you use.
- Use `crate::format::{bytes, rate, percent, meter}` for numbers. Put new formatters in your own file.
- Do not edit `src/ui.rs`. If you need a helper that is not in `ui.rs`, write it in your file.
- Each panel draws its own bordered block with `panel(title)`.
- Each draw function must not panic for any `Rect`, including zero width or zero height.

### 5.5 Per-target code

The S0 `#[allow(dead_code)]` on each `mod` line hides dead code in Stage 1. S2 removes those lines, and then `clippy -D warnings` fails for each item that no caller uses on a target (CI runs Linux clippy, `.github/workflows/ci.yml:25`). Thus each item that only one OS calls has a `cfg` that keeps it out of the other targets' non-test builds. Use exactly these attributes:

| Item | Attribute |
|---|---|
| `zfs::FIELDS` | `#[cfg(any(test, target_os = "macos", target_os = "freebsd"))]` |
| `zfs::from_pairs` | `#[cfg(any(test, debug_assertions, target_os = "linux", target_os = "macos", target_os = "freebsd"))]` |
| `zfs::parse_kstat` | `#[cfg(any(test, debug_assertions, target_os = "linux"))]` |
| `zfs::sysctl_u64` (private) | `#[cfg(any(target_os = "macos", target_os = "freebsd"))]` |
| `zfs::arc_adjusted_used` | `#[cfg(any(test, target_os = "linux"))]` |
| `zfs::ArcRaw`, `zfs::derive`, `ZfsCollector` | None. `collect` is the same on every OS. Only the private `read_raw` has a body for each OS (§6.1), so the compiler sees a call to `derive` on every target. |
| `sensors::parse_linux_supply` | `#[cfg(any(test, target_os = "linux"))]` |
| `sensors::combine` | `#[cfg(any(test, target_os = "linux", target_os = "macos"))]` |
| `sensors::from_macos` | `#[cfg(any(test, target_os = "macos"))]` |
| `sensors::from_power_status` | `#[cfg(any(test, windows))]` |
| `sensors::Battery`, `sensors::BatteryState` | `#[cfg_attr(target_os = "freebsd", allow(dead_code, reason = "no FreeBSD battery reader"))]` |
| `storage::apple_whole_disk` | `#[cfg(any(test, target_os = "macos"))]` |
| `process::ProcState::from_sysinfo` | None. The collector calls it on every OS (§6.7.1). |
| `process::linux_threads` (private) | `#[cfg(any(test, target_os = "linux"))]` |
| macOS thread reader (private, `proc_pidinfo`) | `#[cfg(target_os = "macos")]` |

Each enum variant must have a constructor on every target that builds the enum. For `BatteryState`, §6.5 gives mappings that make all five variants on Linux, macOS and Windows.

## 6. Interfaces for each module

Implementers copy these signatures exactly. Private helpers are free. Field order and names are fixed.

### 6.1 T1: `src/zfs.rs`

```rust
//! ZFS ARC statistics: a pure parser, delta logic and a reader for each OS.

/// Raw ARC counters as the kernel gives them. A field that a release does not have is `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArcRaw {
    pub size: u64,
    pub c: u64,
    pub c_min: u64,
    pub c_max: u64,
    pub hits: u64,
    /// OpenZFS 2.2 and later.
    pub iohits: Option<u64>,
    pub misses: u64,
    pub demand_data_hits: u64,
    pub demand_data_iohits: Option<u64>,
    pub demand_data_misses: u64,
    pub demand_metadata_hits: u64,
    pub demand_metadata_iohits: Option<u64>,
    pub demand_metadata_misses: u64,
    pub mru_size: u64,
    pub mfu_size: u64,
    /// Present since compressed ARC (ZoL 0.7, from memory). `None` on older releases.
    pub data_size: Option<u64>,
    pub metadata_size: Option<u64>,
    /// Removed in OpenZFS 2.2 (from memory). Fallback for `metadata` only.
    pub arc_meta_used: Option<u64>,
    pub compressed_size: Option<u64>,
    pub uncompressed_size: Option<u64>,
    pub l2_size: u64,
    pub l2_asize: u64,
    pub l2_hits: u64,
    pub l2_misses: u64,
}

/// The ARC values that the panel shows. Percentages are 0.0 to 100.0.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ZfsArc {
    pub size: u64,
    pub target: u64,
    pub min: u64,
    pub max: u64,
    pub mru: u64,
    pub mfu: u64,
    pub data: u64,
    pub metadata: u64,
    /// uncompressed_size / compressed_size. `None` if a value is missing or compressed_size is 0.
    pub compress_ratio: Option<f64>,
    /// Over the window (see `derive`), or since boot when `hit_since_boot` is true.
    /// `None` only when there were no reads at all.
    pub hit_pct: Option<f64>,
    pub iohit_pct: Option<f64>,
    pub demand_data_pct: Option<f64>,
    pub demand_meta_pct: Option<f64>,
    /// True when the four values above are since-boot ratios, not window ratios.
    pub hit_since_boot: bool,
    /// `Some` only when l2_size > 0.
    pub l2: Option<L2Arc>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct L2Arc {
    pub size: u64,
    pub asize: u64,
    /// Δl2_hits / (Δl2_hits + Δl2_misses) over the window, or since boot when `since_boot` is true.
    pub hit_pct: Option<f64>,
    pub since_boot: bool,
}

/// The hit-ratio window. The collector derives against the newest kept sample that is at least this old.
pub const HIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

/// The kstat field names that `from_pairs` reads. FreeBSD and macOS read these names by sysctl.
/// `from_pairs` matches names itself and does not use this constant (see the cfg table in §5.5).
#[cfg(any(test, target_os = "macos", target_os = "freebsd"))]
pub const FIELDS: [&str; 24] = [
    "size", "c", "c_min", "c_max", "hits", "iohits", "misses",
    "demand_data_hits", "demand_data_iohits", "demand_data_misses",
    "demand_metadata_hits", "demand_metadata_iohits", "demand_metadata_misses",
    "mru_size", "mfu_size", "data_size", "metadata_size", "arc_meta_used",
    "compressed_size", "uncompressed_size",
    "l2_size", "l2_asize", "l2_hits", "l2_misses",
];

/// Builds `ArcRaw` from name and value pairs. Unknown names are ignored.
/// Returns `None` if `size`, `c` or `c_max` is missing.
#[cfg(any(test, debug_assertions, target_os = "linux", target_os = "macos", target_os = "freebsd"))]
pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, u64)>) -> Option<ArcRaw>;

/// Parses the text of `/proc/spl/kstat/zfs/arcstats`. Skips the first two lines.
/// Each row is `name type value`; the value is the third token. Rows that do not parse are skipped.
#[cfg(any(test, debug_assertions, target_os = "linux"))]
pub fn parse_kstat(text: &str) -> Option<ArcRaw>;

/// Pure: makes the panel values from the current raw counters and the window's base counters.
pub fn derive(prev: Option<&ArcRaw>, cur: &ArcRaw) -> ZfsArc;

/// Pure. Linux used memory with the shrinkable ARC (`size − min`) counted as free. See §7.6.
#[cfg(any(test, target_os = "linux"))]
pub fn arc_adjusted_used(used: u64, arc: &ZfsArc) -> u64;

/// Owned by the sampler. Keeps a short ring of earlier counters for the hit-ratio window.
pub struct ZfsCollector { /* private: ring: VecDeque<(Instant, ArcRaw)>, present: bool */ }

impl ZfsCollector {
    pub fn new() -> Self;
    /// Reads the counters. Returns `None` when ZFS is absent.
    /// When ZFS is absent, it tries again only on a slow tick.
    pub fn collect(&mut self, slow: bool) -> Option<ZfsArc>;
}
```

`FIELDS` holds exactly the names that `from_pairs` reads. A test checks this: `from_pairs` over `FIELDS` with distinct values sets each field. The MRU target (`p`, `pd`, `pm`) and the metadata target (`meta`, `arc_meta_limit`) are not shown in this round.

Rules for `derive`:

- `data` = `data_size`, else `size.saturating_sub(metadata)`.
- `metadata` = `metadata_size`, else `arc_meta_used`, else 0.
- Window ratios, with `d(x) = cur.x.checked_sub(prev.x)`:
  - `reads = d(hits) + d(iohits or 0) + d(misses)`.
  - Use the window when `prev` is `Some`, every `checked_sub` of `hits`, `iohits` and `misses` is `Some`, and `reads > 0`. Then `hit_since_boot = false`:
    - `hit_pct = 100 × d(hits) / reads`. `iohit_pct = 100 × d(iohits) / reads`, or `None` if `iohits` is absent.
    - Demand data: `100 × d(demand_data_hits) / (d(demand_data_hits) + d(demand_data_iohits or 0) + d(demand_data_misses))`. `None` if that sum is 0 or a `checked_sub` is `None`. Demand metadata uses the same pattern.
  - Otherwise use the counters of `cur` alone (since boot), with the same formulas and `x` in place of `d(x)`, and set `hit_since_boot = true`. Each value is `None` when its denominator is 0.
- L2: the same rule with `l2_hits` and `l2_misses`, and its own `since_boot` flag.
- `compress_ratio = uncompressed_size / compressed_size` as `f64`.
- `l2` is `Some` only when `cur.l2_size > 0`.

`arc_adjusted_used(used, arc)` = `used.saturating_sub(arc.size.saturating_sub(arc.min))`.

`ZfsCollector::collect`:

1. When `present` is false and `slow` is false, return `None` without a read.
2. `read_raw()`. On `None`: clear the ring, set `present = false`, return `None`. It must not return a stale `ZfsArc`. On `Some(cur)`: set `present = true`.
3. Drop ring entries that are older than the newest entry whose age is at least `HIT_WINDOW`. The base is that entry, else the oldest entry, else `None`.
4. `derive(base, &cur)`. If `cur.hits < base.hits` (counter reset), clear the ring before step 5.
5. Push `(now, cur)`. Keep at most 64 entries. Return `Some`.

Put the choice of base in a private pure helper, `fn window_base(ring: &VecDeque<(Instant, ArcRaw)>, now: Instant) -> Option<usize>`, and test it with `Instant` values built by adding durations to one base `Instant`.

Reader for each OS: one private `fn read_raw() -> Option<ArcRaw>` with a body for each target. `collect` has no `cfg`.

| Target | `read_raw` body | Present when |
|---|---|---|
| Any, debug builds only | First: if env `RTOP_ZFS_ARCSTATS` is set, read that file with `parse_kstat`. | The file parses. |
| `target_os = "linux"` | `std::fs::read_to_string("/proc/spl/kstat/zfs/arcstats")`, then `parse_kstat`. | The file reads and parses. |
| `target_os = "freebsd"` and `"macos"` | `sysctl_u64("kstat.zfs.misc.arcstats.<name>")` for each name in `FIELDS`, then `from_pairs`. | `size` reads. |
| Other | `None`. | Never. |

```rust
#[cfg(any(target_os = "freebsd", target_os = "macos"))]
fn sysctl_u64(name: &str) -> Option<u64> {
    let c = std::ffi::CString::new(name).ok()?;
    let mut v: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    // SAFETY: `c` is NUL-terminated. `v` and `len` live until the call returns.
    // `newp` is null and `newlen` is 0, so nothing is written to the kernel.
    let rc = unsafe {
        libc::sysctlbyname(
            c.as_ptr(),
            (&raw mut v).cast::<libc::c_void>(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && len == std::mem::size_of::<u64>()).then_some(v)
}
```

`std::ptr::null_mut()` is required: the Apple `sysctlbyname` takes `newp: *mut c_void` (`libc-0.2.190/src/unix/bsd/apple/mod.rs:4746`), FreeBSD takes `*const c_void`, and `*mut` coerces to `*const` but not the reverse.

### 6.2 T1: `src/ui/zfs.rs`

```rust
use ratatui::Frame;
use ratatui::layout::Rect;
use crate::zfs::ZfsArc;

pub const MIN_HEIGHT: u16 = 4;
pub const MIN_WIDTH: u16 = 36;

/// Borders plus one line for each row of content that `arc` has.
pub fn want_height(arc: &ZfsArc) -> u16;

pub fn draw_zfs(frame: &mut Frame, area: Rect, arc: &ZfsArc);
```

Title: ` zfs arc {size} `. Content lines, in priority order. If the area is short, draw the first lines and drop the rest:

1. A gauge of `size / max` with the label `size {size} / target {target}` and `min {min} max {max}` when the width permits.
2. `hit {hit_pct}  io {iohit_pct}  miss {100 - hit - io}  dd {demand_data_pct}  dm {demand_meta_pct}`. Show `—` for `None`. Drop `io` when `iohit_pct` is `None`. When `hit_since_boot` is true, the line starts with `boot ` (`boot hit 85.0% …`), so the user knows it is not a live value. The full line is about 50 columns; the panel at 80 columns has 38 inner columns. When the line does not fit the inner width, drop parts in this order: `dm`, `dd`, `miss`, `io`. Never cut a part in the middle.
3. `mfu {mfu}  mru {mru}` and a two-colour split bar (MFU in `VIOLET`, MRU in `BLUE`).
4. `data {data}  meta {metadata}  comp {ratio:.2}×`. Drop `comp` when the ratio is `None`.
5. Only when `l2` is `Some`: `l2 {size} ({asize})  hit {hit_pct}`, with `boot ` before `hit` when `l2.since_boot` is true.

`want_height` is `2 + 4`, plus 1 when `l2` is `Some`.

### 6.3 T2: `src/storage.rs`

```rust
//! Filesystem usage and disk I/O rates from `sysinfo::Disks`.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Volume {
    pub mount: String,
    /// The device or dataset name from `Disk::name` (lossy UTF-8). On macOS this is the volume name.
    pub device: String,
    /// From `Disk::file_system` (lossy UTF-8).
    pub file_system: String,
    pub total: u64,
    pub available: u64,
    /// Volumes with the same group share one I/O counter. The total counts each group once.
    /// macOS: the whole-disk BSD name (`apple_whole_disk`), else `device`. Other targets: `device`.
    pub io_group: String,
    /// False when the OS gives no I/O counter for the volume: file system `zfs`, or both
    /// since-boot totals are 0. The UI shows `n/a`.
    pub io_known: bool,
    /// Bytes per second over the last interval. 0 on the first sample of a (device, mount), and when `io_known` is false.
    pub read_rate: u64,
    pub write_rate: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Storage {
    /// Sorted: "/" first, then by mount point.
    pub volumes: Vec<Volume>,
    /// Sum over distinct `io_group` values of volumes with `io_known`.
    pub read_rate: u64,
    pub write_rate: u64,
    /// True when at least one volume has `io_known`.
    pub io_known: bool,
}

/// Owned by the sampler.
pub struct StorageCollector {
    /* private: disks: sysinfo::Disks, seen: HashSet<(String, String)> keyed by (device, mount),
       groups: HashMap<String, String> mount → io_group (macOS, filled on slow ticks), last: Storage */
}

impl StorageCollector {
    pub fn new() -> Self;
    /// I/O counters of kept disks refresh on every call. Space and the disk list refresh only when `slow` is true.
    pub fn collect(&mut self, secs: f64, slow: bool) -> Storage;
}

/// Pure filter. Drops pseudo and system volumes.
pub fn keep_volume(mount: &str, file_system: &str, total: u64) -> bool;

/// Pure. Drops a ZFS dataset `pool/a/b` when its pool root `pool` is also in the list,
/// except the volume mounted at "/", which is always kept.
/// Input items are (device, file_system, mount). Returns the indices to keep, in input order.
pub fn drop_zfs_children(items: &[(&str, &str, &str)]) -> Vec<usize>;

/// Pure. Sums `read_rate` and `write_rate` one time for each distinct `io_group`,
/// over volumes with `io_known` only. For a group, it takes the first volume's rates.
pub fn dedupe_rates(volumes: &[Volume]) -> (u64, u64);

/// Pure. The whole-disk BSD name from a macOS `statfs` `f_mntfromname`:
/// "/dev/disk3s1s1" → Some("disk3"); "/dev/disk10s5" → Some("disk10"); "map auto_home" → None.
#[cfg(any(test, target_os = "macos"))]
pub fn apple_whole_disk(mntfrom: &str) -> Option<&str>;
```

Refresh calls:

- `StorageCollector::new()` uses `Disks::new_with_refreshed_list_specifics(DiskRefreshKind::nothing().with_storage().with_io_usage())`.
- Slow call: `disks.refresh_specifics(true, DiskRefreshKind::nothing().with_storage().with_io_usage())`. This re-lists, so it also refreshes the I/O counters. Do not also do the fast-call loop on a slow call.
- Fast call: `for disk in disks.list_mut()`, only for a disk that `keep_volume` keeps and whose file system is not `zfs`: `disk.refresh_specifics(DiskRefreshKind::nothing().with_io_usage())`. `Disks::refresh_specifics` is not used on fast calls: on Linux it re-reads `/proc/mounts` and `/dev/disk/by-id` (`sysinfo` `linux/disk.rs:172-195,360-450`); on macOS it runs `getfsstat` and CFURL fetches for each volume (`apple/disk.rs:243-357`).
- macOS only, on slow calls: for each kept mount, `libc::statfs` the mount point, read `f_mntfromname`, and store `apple_whole_disk(..)` in `groups`. `/` and `/System/Volumes/Data` are both in one APFS container, and `sysinfo` reads one `IOBlockStorageDriver` counter for both (`macos/disk.rs:106-131`). Without the group, every Mac shows double the real I/O.

`keep_volume` rules: drop `total == 0`. Drop file systems `autofs`, `devfs`, `devtmpfs`, `overlay`, `squashfs`, `nullfs`. Drop mounts that start with `/System/Volumes/`, except `/System/Volumes/Data`. Drop mounts that start with `/snap/`. Keep all other mounts. The function is pure and does not use `cfg`, so tests run on every host.

`drop_zfs_children` keeps "/" because on the FreeBSD installer layout `zroot` is mounted at `/zroot` and `zroot/ROOT/default` at `/` (from memory, not checked).

Rate rule: a (device, mount) that is new to `seen` reports rate 0 for this tick. `sysinfo` sets the previous counter to 0 for a new `Disk`, so the first delta is the total since boot. A second mount of a known device is a new `Disk` too, so the key must include the mount. After each call, remove from `seen` each key that is not in this call's list, so a later remount is new again.

ZFS datasets have no `/proc/diskstats` key (`linux/disk.rs:118-134`), so their counters stay 0: `io_known` is false and the rate is `n/a`, not 0.

### 6.4 T2: `src/ui/storage.rs`

```rust
use ratatui::Frame;
use ratatui::layout::Rect;
use crate::storage::Storage;

pub const MIN_HEIGHT: u16 = 4;
pub const MIN_WIDTH: u16 = 30;
/// The most volume rows the panel asks for.
pub const MAX_ROWS: u16 = 6;

/// 2 borders + 1 I/O line + min(volumes, MAX_ROWS).
pub fn want_height(storage: &Storage) -> u16;

pub fn draw_storage(frame: &mut Frame, area: Rect, storage: &Storage);
```

Title: ` storage {shown}/{total} `. Line 1: `io ↓ {read}/s ↑ {write}/s`. When `storage.io_known` is false: `io n/a (zfs)` if any volume is `zfs`, else `io n/a`. When some volumes have `io_known` and some `zfs` volumes do not, add ` · zfs n/a`. Each next line is one volume: the mount point (truncated from the left with `…`), a meter of used space with `heat(pct)`, `{pct}%` and `{used}/{total}`. When the inner width is 70 or more, add the volume's read and write rates, or `n/a` when `io_known` is false.

### 6.5 T3: `src/sensors.rs`

```rust
//! Temperatures from `sysinfo::Components` and battery state from each OS.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Temp {
    pub label: String,
    pub celsius: f32,
    pub critical: Option<f32>,
}

#[cfg_attr(target_os = "freebsd", allow(dead_code, reason = "no FreeBSD battery reader"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BatteryState {
    Charging,
    Discharging,
    Full,
    NotCharging,
    #[default]
    Unknown,
}

#[cfg_attr(target_os = "freebsd", allow(dead_code, reason = "no FreeBSD battery reader"))]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Battery {
    /// 0.0 to 100.0.
    pub percent: f32,
    pub state: BatteryState,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sensors {
    /// Hottest first; ties by label. Never holds NaN.
    pub temps: Vec<Temp>,
    pub battery: Option<Battery>,
}

impl Sensors {
    pub fn is_empty(&self) -> bool; // temps empty and battery None
}

/// Owned by the sampler.
pub struct SensorCollector { /* private: components: sysinfo::Components, last: Sensors */ }

impl SensorCollector {
    pub fn new() -> Self;
    /// Refreshes only when `slow` is true. Otherwise returns a clone of the last value.
    pub fn collect(&mut self, slow: bool) -> Sensors;
}

/// Pure. Drops NaN and non-finite values, rounds to 0.1 °C, sorts hottest first, ties by label.
pub fn tidy_temps(raw: Vec<Temp>) -> Vec<Temp>;

/// Pure. One Linux `/sys/class/power_supply/<name>/` entry.
/// `kind` is the `type` file, `scope` the `scope` file, `present` the `present` file.
/// Returns `None` unless kind is "Battery", scope is not "Device", and present is not "0".
#[cfg(any(test, target_os = "linux"))]
pub fn parse_linux_supply(
    kind: &str,
    status: &str,
    capacity: &str,
    present: Option<&str>,
    scope: Option<&str>,
) -> Option<Battery>;

/// Pure. Joins several system batteries: mean percent; Charging if any charges,
/// else Discharging if any discharges, else Full if all are full, else NotCharging if any is
/// NotCharging, else Unknown.
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
pub fn combine(batteries: &[Battery]) -> Option<Battery>;

/// Pure. Windows `SYSTEM_POWER_STATUS` fields.
/// flag & 128 → None. percent 255 → None. flag & 8 → Charging.
/// ac 1 → Full if percent is 100, else NotCharging. ac 0 → Discharging. Else (ac 255) Unknown.
#[cfg(any(test, windows))]
pub fn from_power_status(ac_line: u8, battery_flag: u8, life_percent: u8) -> Option<Battery>;

/// Pure. macOS power source values.
/// `max <= 0` → None. percent = 100 × current / max, clamped to 0..=100.
/// `is_charging` → Charging. Else `state` "Battery Power" → Discharging.
/// Else `state` "AC Power": Full if current >= max, else NotCharging. Else Unknown.
#[cfg(any(test, target_os = "macos"))]
pub fn from_macos(current: i64, max: i64, is_charging: bool, state: &str) -> Option<Battery>;
```

These mappings make all five `BatteryState` variants on Linux, macOS and Windows non-test builds, so no variant is dead there (§5.5).

Linux status strings: `Charging`, `Discharging`, `Full`, `Not charging`, `Unknown`. Trim whitespace and newlines before matching.

Platform readers (private, behind `cfg`):

| Target | Temperatures | Battery |
|---|---|---|
| Linux | `Components` | Read each `/sys/class/power_supply/*/{type,status,capacity,present,scope}` with std. Pass to `parse_linux_supply`, then `combine`. |
| macOS | `Components` | `IOPSCopyPowerSourcesInfo`, `IOPSCopyPowerSourcesList`, then `IOPSGetPowerSourceDescription` for each source. Keep sources whose "Type" is "InternalBattery". Read "Current Capacity", "Max Capacity", "Is Charging", "Power Source State". The `kIOPS*Key` constants in `objc2-io-kit` are `&'static CStr`, not `CFString` (`objc2-io-kit-0.3.2/src/generated/ps.rs:69-108`), and `CFString::from_static_str` takes `&'static str`. Make each key with `CFString::from_static_str(key.to_str().ok()?)`, or with the string literal. Pass to `from_macos`, then `combine`. |
| Windows | `Components` (WMI; often empty without admin rights) | `GetSystemPowerStatus`, then `from_power_status`. |
| FreeBSD | `Components` | `None`. |

Temperature mapping: `Component::temperature()` returns `Option<f32>`. Linux can return NaN. `tidy_temps` drops those. `critical` comes from `Component::critical()`.

`Components::refresh(true)` runs on slow ticks only. `SensorCollector::new()` calls `Components::new_with_refreshed_list()`.

If the macOS IOKit code does not compile or does not work within the time box, the macOS battery reader returns `None`. The task then records a follow-up. Do not add `starship-battery` without a decision from the maintainer.

### 6.6 T3: `src/ui/sensors.rs`

```rust
use ratatui::Frame;
use ratatui::layout::Rect;
use crate::sensors::Sensors;

pub const MIN_HEIGHT: u16 = 3;
pub const MIN_WIDTH: u16 = 22;
pub const MAX_ROWS: u16 = 6;

/// 2 borders + 1 if a battery + min(temps, MAX_ROWS).
pub fn want_height(sensors: &Sensors) -> u16;

/// `fahrenheit` changes only the displayed unit.
pub fn draw_sensors(frame: &mut Frame, area: Rect, sensors: &Sensors, fahrenheit: bool);
```

Title: ` sensors · max {hottest} ` (or ` sensors ` with no temperatures). The battery line comes first: `bat {meter} {pct}% {state}`. Then one line for each temperature: the label, then the value right-aligned, coloured by `heat(c / critical × 100)` when `critical` is `Some`, else `heat(c)`.

### 6.7 T4: `src/process.rs`

```rust
//! Process data, sorting, filtering, the tree view and kill. The sampler owns the collector.

use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProcState {
    Run,
    Sleep,
    DiskSleep,
    Stop,
    Zombie,
    Idle,
    Dead,
    #[default]
    Unknown,
}

impl ProcState {
    /// Maps the `sysinfo` status. Exact table below. Called on every OS.
    pub fn from_sysinfo(status: sysinfo::ProcessStatus) -> Self;
    /// One letter, like `ps`: R S D T Z I X, and '?' for Unknown.
    pub fn code(self) -> char;
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcInfo {
    pub pid: u32,
    pub parent: Option<u32>,
    /// Seconds since the epoch. Kill uses (pid, start_time) to find the same process.
    pub start_time: u64,
    pub name: String,
    /// The joined command line, at most 4096 chars. Empty when unknown.
    pub cmd: Arc<str>,
    pub user: Option<Arc<str>>,
    pub state: ProcState,
    pub threads: Option<u32>,
    /// Percent of one core.
    pub cpu: f32,
    pub memory: u64,
    /// Bytes per second over the last interval. 0 on the first sample of a process.
    pub read_rate: u64,
    pub write_rate: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Pid,
    Name,
    Cpu,
    Memory,
    Io,
}

impl SortKey {
    /// The `s` key order: Cpu → Memory → Io → Pid → Name → Cpu.
    pub fn next(self) -> Self;
    /// Header text: "PID", "NAME", "CPU%", "MEM", "IO".
    pub fn title(self) -> &'static str;
    /// True for Cpu, Memory and Io.
    pub fn descending(self) -> bool;
    /// Config name: "pid", "name", "cpu", "mem", "io".
    pub fn name(self) -> &'static str;
    pub fn from_name(name: &str) -> Option<Self>;
}

/// How the UI wants the rows.
#[derive(Clone, Copy, Debug)]
pub struct ProcView<'a> {
    /// Case-insensitive substring of the name, or an exact PID. Empty matches all.
    pub filter: &'a str,
    pub sort: SortKey,
    /// Flips the default direction of `sort`.
    pub reverse: bool,
    pub tree: bool,
    /// When true, the filter also matches the command line.
    pub match_cmd: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProcRow<'a> {
    pub proc: &'a ProcInfo,
    /// Tree glyphs such as "│ ├─". Empty in the flat view. At most 32 levels: a deeper
    /// row shows the first 31 levels, then `…`, then its branch, so a long parent chain
    /// cannot make the prefixes cost memory that grows with the square of the depth.
    pub prefix: String,
}

/// Pure. Filters, sorts (ties by PID) and, in tree mode, orders the rows depth-first.
/// In tree mode, the sort orders siblings. A row whose parent is not visible is a root.
/// A parent loop (from PID reuse) must not hang or repeat rows.
pub fn visible_rows<'a>(procs: &'a [ProcInfo], view: &ProcView<'_>) -> Vec<ProcRow<'a>>;

/// Pure. Joins argv with spaces (lossy UTF-8) and cuts it to 4096 chars.
pub fn join_cmd(parts: &[std::ffi::OsString]) -> String;

/// The refresh kind the sampler must use for `refresh_processes_specifics`.
/// Every tick: memory, cpu, disk_usage, tasks, user(OnlyIfNotSet).
/// Slow ticks add cmd(OnlyIfNotSet). `exe` is never refreshed: rtop does not read it.
pub fn refresh_kind(slow: bool) -> sysinfo::ProcessRefreshKind;

/// Owned by the sampler. Caches command lines, user names and thread counts.
pub struct ProcCollector { /* private */ }

impl ProcCollector {
    pub fn new() -> Self;
    /// Builds the process list. Skips Linux threads (`thread_kind().is_some()`).
    pub fn collect(&mut self, sys: &sysinfo::System, secs: f64, slow: bool) -> Vec<ProcInfo>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KillTarget {
    pub pid: u32,
    pub start_time: u64,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillSignal {
    /// SIGTERM. Not supported on Windows.
    Term,
    /// SIGKILL. On Windows `TerminateProcess` on a handle whose creation time was checked.
    Kill,
}

impl KillSignal {
    /// "TERM" or "KILL".
    pub fn label(self) -> &'static str;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KillRequest {
    pub target: KillTarget,
    pub signal: KillSignal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillError {
    /// PID 0, PID 1, rtop itself, or PID 4 on Windows.
    Protected,
    /// The PID is gone, or a new process has the PID now.
    Gone,
    /// `kill_with` returned `None`.
    Unsupported,
    /// `kill_with` returned `Some(false)`.
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KillReport {
    pub request: KillRequest,
    pub result: Result<(), KillError>,
}

impl KillReport {
    /// The footer text. Exact strings are in section 6.7.2.
    pub fn message(&self) -> String;
}

/// Pure. Refuses protected PIDs.
pub fn check_target(pid: u32, self_pid: u32) -> Result<(), KillError>;

/// Runs on the sampler thread. Checks the target, refreshes that one PID,
/// compares start_time, then calls `kill_with`.
pub fn send_signal(sys: &mut sysinfo::System, req: &KillRequest, self_pid: u32) -> KillReport;
```

`from_sysinfo` table (`sysinfo` Linux maps `'D'` to `UninterruptibleDiskSleep`, `linux/process.rs:34`):

| `sysinfo::ProcessStatus` | `ProcState` | `code` |
|---|---|---|
| `Run` | `Run` | `R` |
| `Sleep` | `Sleep` | `S` |
| `UninterruptibleDiskSleep` | `DiskSleep` | `D` |
| `Stop`, `Tracing`, `Suspended` | `Stop` | `T` |
| `Zombie` | `Zombie` | `Z` |
| `Idle` | `Idle` | `I` |
| `Dead` | `Dead` | `X` |
| any other (`Wakekill`, `Waking`, `Parked`, `LockBlocked`, `Unknown(_)`) | `Unknown` | `?` |

#### 6.7.1 Collector rules

- Cache: `HashMap<u32, Known>` with `Known { start_time: u64, cmd: Arc<str>, threads: Option<u32> }`. A pid whose start time changed is a new process.
- A process new to the cache reports `read_rate = 0` and `write_rate = 0` for this tick. Otherwise rate = `disk_usage().read_bytes / secs` (the value is already a delta since the last refresh).
- After each `collect`, remove cache entries for PIDs that were not in this tick's list.
- `cmd`: build `join_cmd(process.cmd())` when the entry is created or while the cached value is empty. `sysinfo` reads the command line only on slow ticks (`refresh_kind(true)`), so a new process can show `[name]` for up to 5 s.
- User names: `HashMap<sysinfo::Uid, Arc<str>>` built from `sysinfo::Users`. Use `Process::user_id()`. On a miss, reload `Users` only on a slow tick and at most once each 60 s. Until then, `user` is `None`.
- Threads: Linux uses `linux_threads(process.tasks().map(|t| t.len()))`, a private pure helper that returns `n + 1`, because `sysinfo` leaves the main thread out of the task list (`linux/process.rs:975`). A single-threaded process shows 1. macOS uses `libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, …)` and `pti_threadnum`, on slow ticks only; the value stays in the cache between slow ticks. Other targets: `None`.
- State: `ProcState::from_sysinfo(process.status())` on every OS. On Windows the value is always `Run`, and the UI hides the column (`state_and_threads: false`).

#### 6.7.2 Kill rules

`check_target` refuses `pid == 0`, `pid == 1`, `pid == self_pid`, and on Windows `pid == 4`.

`send_signal` steps:

1. `check_target(req.target.pid, self_pid)`. On error, return the report.
2. `sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, ProcessRefreshKind::nothing())`.
3. If `sys.process(pid)` is `None`, or its `start_time()` is not `req.target.start_time`, return `Gone`.
4. `kill_with(signal)`: `None` → `Unsupported`; `Some(false)` → `Failed`; `Some(true)` → `Ok(())`.

On Windows, steps 2 to 4 are different, because `sysinfo`'s `kill_with` runs `taskkill.exe` by bare name (Windows searches the directory of rtop.exe before System32, so a planted `taskkill.exe` would run) and `taskkill /F` then acts on a bare PID that may have been reused. Instead: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION`, plus `PROCESS_TERMINATE` for KILL`)`; access denied → `Failed`, any other error → `Gone`. Compare the creation time from `GetProcessTimes` on that handle, in the same seconds as `sysinfo`'s `start_time`; different → `Gone`. TERM → `Unsupported`. KILL → `TerminateProcess` on the same handle; error → `Failed`. Needs the `Win32_System_Threading` feature of the `windows` crate.

`KillReport::message` strings (`{sig}` is `TERM` or `KILL`):

| Result | Text |
|---|---|
| `Ok` | `sent {sig} to {pid} {name}` |
| `Protected` | `refused: pid {pid} is protected` |
| `Gone` | `{pid} {name} is not running now` |
| `Unsupported` | `{sig} is not supported here; press x then f to force` |
| `Failed` | `could not signal {pid} {name}: permission denied or process gone` |

### 6.8 T4: `src/ui/process.rs`

```rust
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::TableState;
use crate::process::{KillTarget, ProcRow, SortKey};

pub struct ProcTable<'a> {
    pub rows: &'a [ProcRow<'a>],
    /// All processes before the filter.
    pub total: usize,
    pub sort: SortKey,
    pub reverse: bool,
    pub tree: bool,
    /// Show the command line in the last column, not the name.
    pub show_cmd: bool,
    pub filter: &'a str,
    /// False on Windows: hide the State and Threads columns.
    pub state_and_threads: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Pid,
    User,
    State,
    Threads,
    Cpu,
    Memory,
    Read,
    Write,
    Name,
}

/// Pure. Columns for the inner table width (area width minus 2 borders and 1 highlight cell).
/// >= 96: all. 60..96: no Read, Write. < 60: Pid, Cpu, Memory, Name.
/// When `state_and_threads` is false, drop State and Threads.
pub fn columns_for_width(inner_width: u16, state_and_threads: bool) -> Vec<Column>;

/// The caller clamps the selection before the call.
pub fn draw_processes(frame: &mut Frame, area: Rect, table: &ProcTable<'_>, state: &mut TableState);

/// Draws a centred dialog over `area` (the full frame). Clears the cells under it first.
pub fn draw_kill_prompt(frame: &mut Frame, area: Rect, target: &KillTarget);
```

Column widths (column spacing 1): Pid 7, User 8, State 1, Threads 4, Cpu 6, Memory 10, Read 10, Write 10 (`format::bytes` prints up to 10 chars, for example `1010.0 MiB`), Name `Fill(1)`. Headers: `PID USER S THR CPU% MEM READ/s WRITE/s NAME` (or `COMMAND` when `show_cmd`). The sort column is `PINK` and bold with `▼` or `▲` for the effective direction. For `SortKey::Io`, both Read and Write headers are marked; if those columns are not visible, the title still says `sort io`.

Name cell: `prefix` + name. With `show_cmd`: `prefix` + cmd, or `[name]` when cmd is empty (kernel threads).

Title: ` processes {shown}/{total} · sort {key}{ rev} · tree · filter "{f}" `. Show the parts that apply.

Kill prompt: a box 52 wide (or the frame width) and 5 high, titled ` kill `. Text: `Send a signal to {pid} {name}?` then `y TERM · f KILL (force) · n cancel`.

### 6.9 T5: `src/net.rs` and `src/ui/net.rs`

```rust
//! src/net.rs: per-interface network rates.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetIface {
    pub name: String,
    /// Bytes per second over the last interval.
    pub rx_rate: u64,
    pub tx_rate: u64,
    /// Bytes since boot.
    pub total_rx: u64,
    pub total_tx: u64,
}

/// Pure. Drops names that start with "lo" and interfaces with no traffic since boot.
pub fn keep_iface(name: &str, total_rx: u64, total_tx: u64) -> bool;

/// Builds the list from refreshed `Networks`, sorted by name.
pub fn collect(networks: &sysinfo::Networks, secs: f64) -> Vec<NetIface>;

/// Pure. Sum of rx_rate and tx_rate.
pub fn totals(ifaces: &[NetIface]) -> (u64, u64);
```

```rust
//! src/ui/net.rs
use ratatui::Frame;
use ratatui::layout::Rect;
use crate::net::NetIface;

pub const MIN_HEIGHT: u16 = 4;
pub const MAX_ROWS: u16 = 6;

/// 2 borders + 1 header + min(ifaces, MAX_ROWS).
pub fn want_height(ifaces: &[NetIface]) -> u16;

pub fn draw_interfaces(frame: &mut Frame, area: Rect, ifaces: &[NetIface]);
```

Title: ` network · {n} interfaces `. Header: `IFACE  ↓ RX/s  ↑ TX/s  TOTAL ↓/↑`. Drop the total column below 48 inner columns. The busiest interface (largest rx + tx rate) is bold.

### 6.10 T6: `src/layout.rs`

```rust
//! Pure layout: which panels fit, and where.

use ratatui::layout::Rect;
use crate::panel::PanelId;

/// What one visible, available panel needs. The caller adds a request only for panels
/// that the user shows and that have data (no ZFS → no Zfs request).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelReq {
    pub id: PanelId,
    pub min_h: u16,
    pub want_h: u16,
    pub min_w: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayoutPlan {
    /// Panels to draw, with their areas, in PanelId order.
    pub areas: Vec<(PanelId, Rect)>,
    /// Panels that `plan` removed to fit the space, in removal order.
    pub collapsed: Vec<PanelId>,
}

impl LayoutPlan {
    pub fn area(&self, id: PanelId) -> Option<Rect>;
}

/// The process table keeps at least this many lines (borders + header + 6 rows),
/// the same 6 process rows that rtop shows today at 80×24 with 8 cores.
pub const PROC_MIN_H: u16 = 9;

/// Auto-collapse removes panels in this order. Processes never collapse.
pub const COLLAPSE_ORDER: [PanelId; 7] = [
    PanelId::History,
    PanelId::Sensors,
    PanelId::Storage,
    PanelId::Network,
    PanelId::Zfs,
    PanelId::Memory,
    PanelId::Cpu,
];

pub fn plan(body: Rect, reqs: &[PanelReq]) -> LayoutPlan;
```

Rows, top to bottom:

| Row | Panels, left to right | Width weights |
|---|---|---|
| 0 | Cpu, History | 45, 55 |
| 1 | Memory, Network | 1, 1 |
| 2 | Storage, Zfs, Sensors | 3, 3, 2 |
| fill | Processes | takes the rest |

Algorithm of `plan` (all arithmetic saturating):

1. Start with the panels in `reqs`. A `Processes` request means the fill row is present. Its `min_h` is `PROC_MIN_H`.
2. **Width pass.** For each row: while the row has 2 or more panels and the sum of their `min_w` is more than `body.width`, remove the row's panel that comes first in `COLLAPSE_ORDER`. Add it to `collapsed`.
3. **Height pass.** `budget = body.height.saturating_sub(PROC_MIN_H if Processes is present, else 0)`. `row_min(r)` is the largest `min_h` in row r (0 if empty). `total` is the sum of `row_min`. While `total > budget` and a row panel is left: remove the first present panel in `COLLAPSE_ORDER`, even if its removal does not make `total` smaller. Add it to `collapsed`.
4. **Put-back pass.** For each panel in `collapsed` that the height pass removed, last removed first: put it back if its row is not empty, its `min_h` is not more than `row_min` of that row, and the row's `min_w` sum with it is not more than `body.width`. Remove it from `collapsed`. (Panels that the width pass removed stay removed.) This keeps a panel whose removal saved no height, for example Network next to a taller Memory.
5. **Heights.** Each row gets `row_min(r)`. `spare = budget - total`. For each panel in reverse `COLLAPSE_ORDER` (Cpu, Memory, Zfs, Network, Storage, Sensors, History) that is present: its row grows by `min(spare, want_h - current row height)` (saturating), and `spare` shrinks by the same amount.
6. If Processes is present, it gets `body.height - sum(row heights)`. Otherwise, the last non-empty row gets the remaining `spare`.
7. Split each row horizontally with `Constraint::Fill(weight)`. In a row of two or more panels, a panel whose share is under its `min_w` gets `Constraint::Length(min_w)` instead, and the split repeats until every share fits; the width pass keeps the row's `min_w` sum within the body width, so this always ends.

Why strict order: the old rule ("remove the first panel whose removal makes `total` smaller") removed Memory (5, next to Network 4) before Storage (4, next to Zfs 4), because removing Storage alone saved nothing. Strict order plus step 4 gives this property, which T6 tests: **Memory never collapses while History, Storage or Sensors is shown**, given the §7.5 requests (History `min_h` 6 > Cpu `min_h` 4).

Worked example 1, 80×24 (body 80×22), 8 cores, ZFS without L2ARC, 3 volumes, 10 temperatures, no battery:

| Request | min_h | want_h | min_w |
|---|---|---|---|
| Cpu | 4 | 6 | 20 |
| History | 6 | 8 | 20 |
| Memory | 5 | 5 | 30 |
| Network | 4 | 5 | 24 |
| Storage | 4 | 6 | 30 |
| Zfs | 4 | 6 | 36 |
| Sensors | 3 | 8 | 22 |
| Processes | 9 | — | — |

- Width pass: row 2 needs 30 + 36 + 22 = 88 > 80. Sensors goes. Row 2 then needs 66.
- Height pass: budget 22 − 9 = 13. Row minimums 6 + 5 + 4 = 15 > 13. History goes. Row minimums 4 + 5 + 4 = 13. It fits.
- Put-back: History needs 6 > row 0 minimum 4: stays out. Sensors was a width removal: stays out.
- Heights: spare 0. Rows are 4, 5, 4. Processes gets 22 − 13 = 9 (6 process rows).
- Result: cpu at full width (4; 8 cores in 4 columns of 2 lines, §7.5), memory and network (5), storage and zfs (4; zfs shows its first two lines), processes (9). `collapsed = [Sensors, History]`.

Worked example 2, 100×30 (body 100×28), the same requests:

- Width pass: row 2 needs 88 ≤ 100. Nothing goes.
- Height pass: budget 28 − 9 = 19. Row minimums 6 + 5 + 4 = 15. It fits.
- Heights: spare 4. Cpu: row 0 is 6, want 6, +0. Memory: row 1 is 5, +0. Zfs: row 2 4 → 6, spare 2. Network +0. Storage: row 2 is 6, +0. Sensors: row 2 6 → 8, spare 0. History +0.
- Result: rows 6, 5, 8. Processes gets 28 − 19 = 9. `collapsed` is empty.

80×24 matrix (T6 and S2 test it): for Cpu `want_h` 6, 6 and 10 (4, 8 and 16 cores, `core_rows + 2` with today's `core_columns`), each with and without the Zfs request (Storage with 3 volumes, Sensors with 10 temperatures, Network totals): Processes gets at least 9 lines, and Memory is not in `collapsed` while History, Storage or Sensors has an area.

### 6.11 T7: `src/config.rs`

```rust
//! Optional config file: a flat TOML subset. Pure parser plus path rules.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use crate::panel::PanelSet;

/// Every field is `None` when the file does not set it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
    pub interval_ms: Option<u64>,
    /// One of `SORT_NAMES`.
    pub sort: Option<String>,
    pub reverse: Option<bool>,
    pub tree: Option<bool>,
    /// Show the full command line in the process table.
    pub command: Option<bool>,
    /// Panels shown at start.
    pub panels: Option<PanelSet>,
    /// From `temp_unit = "f"`.
    pub fahrenheit: Option<bool>,
    /// From `network = "interfaces"`.
    pub net_detail: Option<bool>,
    /// "line N: unknown key `x`, ignored", one for each unknown key.
    pub warnings: Vec<String>,
}

pub const SORT_NAMES: [&str; 5] = ["cpu", "mem", "io", "pid", "name"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// 1-based.
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError { /* "line {line}: {message}" */ }
impl std::error::Error for ParseError {}

pub fn parse(text: &str) -> Result<Config, ParseError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigSource {
    /// The default path. A missing file is not an error.
    Default,
    /// From `--config`. A missing file is an error.
    Path(PathBuf),
    /// From `--no-config`.
    Disabled,
}

/// Pure. `get` reads an environment variable.
/// Windows: %APPDATA%\rtop\config.toml.
/// Other: $XDG_CONFIG_HOME/rtop/config.toml if it is set and absolute, else $HOME/.config/rtop/config.toml.
pub fn default_path(get: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf>;

/// Reads and parses. Returns the config and the path it read (None if no file was read).
/// Error text names the path, and the line for a parse error.
/// `get` reads an environment variable; `ConfigSource::Default` passes it to `default_path`.
/// `main` passes `|k| std::env::var_os(k)`. Tests pass a closure, so they never read the real
/// home directory and never call `set_var` (which is `unsafe` in edition 2024).
pub fn load(
    source: &ConfigSource,
    get: impl Fn(&str) -> Option<OsString>,
) -> anyhow::Result<(Config, Option<PathBuf>)>;

/// Reads at most 64 KiB. A larger file is an error.
fn read_limited(path: &Path) -> std::io::Result<String>;
```

Grammar (any valid file is also valid TOML):

```text
file    = { line }
line    = ws [ pair ] ws [ comment ] newline
pair    = key ws "=" ws value
key     = 1*( ALPHA / DIGIT / "_" / "-" )
value   = integer / boolean / string / array
integer = 1*DIGIT                      ; no sign, no underscores
boolean = "true" / "false"
string  = DQUOTE *( char / "\\\"" / "\\\\" ) DQUOTE
array   = "[" ws [ string *( ws "," ws string ) [ ws "," ] ] ws "]"   ; one line only
comment = "#" *any
```

Keys:

| Key | Type | Values | Config field |
|---|---|---|---|
| `interval_ms` | integer | any; Stage 2 clamps to 250..=8000 | `interval_ms` |
| `sort` | string | `cpu`, `mem`, `io`, `pid`, `name` | `sort` |
| `reverse` | boolean | | `reverse` |
| `tree` | boolean | | `tree` |
| `command` | boolean | | `command` |
| `panels` | array of strings | names from `PanelId::name` | `panels` |
| `temp_unit` | string | `c`, `f` | `fahrenheit` |
| `network` | string | `total`, `interfaces` | `net_detail` |

Errors (return `ParseError`): a `[table]` line, a duplicate key, a wrong type, an unknown value, an unterminated string or array, text after a value that is not a comment. A leading UTF-8 BOM is allowed. A `\r` before each newline is allowed (a file saved on Windows); use `str::lines`, which removes it. An unknown key is a warning, not an error.

Example file (also a test fixture):

```toml
# ~/.config/rtop/config.toml
interval_ms = 500
sort = "mem"
reverse = false
tree = true
command = false
panels = ["cpu", "memory", "network", "zfs", "processes"]
temp_unit = "c"
network = "interfaces"
```

### 6.12 T8: `src/help.rs` and `src/ui/help.rs`

```rust
//! src/help.rs: the one key table. The help overlay and `--help` read it.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyHelp {
    pub keys: &'static str,
    pub action: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeySection {
    pub title: &'static str,
    pub keys: &'static [KeyHelp],
}

/// The content of the key table in section 8, in that order.
pub const KEYMAP: &[KeySection];

/// Pure. Plain text for `--help`: each section title, then "    {keys:<16}{action}" lines.
pub fn usage_keys() -> String;
```

```rust
//! src/ui/help.rs
use ratatui::Frame;
use ratatui::layout::Rect;

/// Draws a centred overlay over `area` (the full frame). Clears the cells under it.
/// Two columns of sections when the overlay is 76 or more wide, else one column.
/// Title: " keys · esc closes ". Cuts off what does not fit; never panics.
pub fn draw_help(frame: &mut Frame, area: Rect);
```

## 7. Integration interfaces (Stage 2)

### 7.1 `Snapshot` and `Event`

```rust
// src/metrics.rs
pub use crate::process::ProcInfo; // replaces the old struct

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub cpus: Vec<f32>,
    pub cpu_total: f32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub rx_rate: u64,
    pub tx_rate: u64,
    pub load: [f64; 3],
    pub uptime: u64,
    pub processes: Vec<ProcInfo>,
    pub interfaces: Vec<crate::net::NetIface>,
    pub storage: crate::storage::Storage,
    pub zfs: Option<crate::zfs::ZfsArc>,
    pub sensors: crate::sensors::Sensors,
}

pub fn spawn_sampler(
    tx: Sender<Event>,
    interval_ms: Arc<AtomicU64>,
    kills: Receiver<crate::process::KillRequest>,
) -> io::Result<()>;
```

```rust
// src/app.rs
#[derive(Debug)]
pub enum Event {
    Key(KeyEvent),
    /// Text pasted while the terminal has bracketed paste on (main.rs turns it on).
    Paste(String),
    Resize,
    Metrics(Snapshot),
    KillDone(crate::process::KillReport),
}
```

### 7.2 Sampler

```rust
const SLOW_PERIOD: Duration = Duration::from_secs(5);

struct Sampler {
    sys: System,
    networks: Networks,
    last: Instant,
    last_slow: Option<Instant>,
    procs: ProcCollector,
    storage: StorageCollector,
    zfs: ZfsCollector,
    sensors: SensorCollector,
}
```

`sample()` order: compute `slow` first (true when `last_slow` is `None` or at least `SLOW_PERIOD` old; then set `last_slow`). Refresh CPU, memory, `refresh_processes_specifics(ProcessesToUpdate::All, true, process::refresh_kind(slow))`, networks. Compute `secs`. Then `net::collect`, `net::totals` (for `rx_rate` and `tx_rate`), `procs.collect`, `storage.collect`, `zfs.collect`, `sensors.collect`. Then, on Linux only and only if the maintainer accepts §7.6, `mem_used = zfs::arc_adjusted_used(mem_used, arc)` when `zfs` is `Some`. `Sampler::new` uses `process::refresh_kind(true)`.

The sleep loop checks `kills.try_recv()` on each step. For each request it sends `Event::KillDone(process::send_signal(&mut self.sys, &req, std::process::id()))`.

### 7.3 `App`

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Filter,
    Help,
    ConfirmKill(crate::process::KillTarget),
}

pub use crate::process::SortKey; // replaces the old enum

pub struct App {
    // existing fields, except `mode: Mode` (now not Copy)
    pub reverse: bool,
    pub tree: bool,
    pub show_cmd: bool,
    pub net_detail: bool,
    pub fahrenheit: bool,
    /// Panels the user wants. Layout can still collapse some.
    pub visible: PanelSet,
    /// One footer message. The next key press clears it.
    pub status: Option<String>,
    /// The process row that the last frame highlighted. `ui::draw` sets it on each frame.
    /// The `x` key builds the kill target from this, not from `table.selected()`.
    pub highlighted: Option<crate::process::KillTarget>,
    /// True once a frame has shown the kill prompt. `ui::draw` sets it; `x` clears it.
    pub kill_prompt_drawn: bool,
    kill_tx: Option<Sender<crate::process::KillRequest>>,
}

impl App {
    pub fn new(host: HostInfo, interval_ms: Arc<AtomicU64>) -> Self; // unchanged signature
    pub fn with_kill_sender(self, tx: Sender<crate::process::KillRequest>) -> Self;
    /// Applies config values, then the caller applies CLI overrides.
    pub fn apply_config(&mut self, config: &crate::config::Config);
    /// Replaces `visible_processes`.
    pub fn visible_rows(&self) -> Vec<crate::process::ProcRow<'_>>;
}
```

### 7.4 CLI

```rust
// src/main.rs
struct Cli {
    /// Some when -i was given.
    interval_ms: Option<u64>,
    config: config::ConfigSource,
    tree: bool,
}

enum Parsed {
    Run(Cli),
    Help,
    Version,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Parsed>;
```

Flags: `-i/--interval <ms>`, `-c/--config <path>`, `--no-config`, `-t/--tree`, `-h/--help`, `-V/--version`. `--config` with `--no-config` is an error. Precedence: CLI, then config file, then built-in default. A config error stops rtop before the TUI starts, with exit status 1. The first config warning goes to `app.status`, with `(+N more)` when there are more.

`USAGE` becomes a function that adds `help::usage_keys()` under a `KEYS:` heading.

### 7.5 `ui::draw`

1. Split header, body, footer as today. Keep the "sampling…" path.
2. Build `Vec<PanelReq>` for each panel that is in `app.visible` and has data: Zfs needs `snapshot.zfs.is_some()`, Storage needs volumes, Sensors needs `!sensors.is_empty()`, Network uses `ui::net::want_height` when `net_detail` is on. Cpu: `min_h 4`, `want_h = core_rows + 2` (`core_rows` from today's `core_columns`), `min_w 20`. History: `min_h 6`, `want_h 8`, `min_w 20`. Memory: 5, 5, `min_w 30`. Network (totals): 4, 5, `min_w 24`. Cap every `want_h` at 14.
3. `let plan = layout::plan(body, &reqs);` Draw each `(id, area)`.
4. Footer: `app.status` when set. Else the mode line. In Normal mode, add `· {n} hidden` when `plan.collapsed` is not empty.
5. Overlays last: `Mode::Help` → `help::draw_help(frame, frame.area())`. `Mode::ConfirmKill(t)` → `process::draw_kill_prompt(frame, frame.area(), t)`.
6. Memory panel info line: add `· arc {size}` when `snapshot.zfs` is `Some`.
7. After the selection is clamped, set `app.highlighted` to the `KillTarget { pid, start_time, name }` of the selected visible row, or `None` when no row is selected or the Processes panel is not drawn.

CPU panel when short: `draw_cores` takes the column count from its area, not only from the core count. Write a pure helper in `ui.rs` and test it:

```rust
/// Columns for the core list: today's choice, more when the area is short,
/// at most one column for each 14 inner cells of width.
fn core_grid(cores: usize, inner: Rect) -> usize {
    let max_cols = ((usize::from(inner.width) + 2) / 14).max(1);
    let by_height = cores.div_ceil(usize::from(inner.height).max(1));
    core_columns(cores).max(by_height).min(max_cols)
}
```

At 80×24 (worked example 1) the Cpu panel has inner 78×2, so 8 cores show in 4 columns of 2 lines. When even `max_cols` columns do not hold every core, the panel shows the first cores and the title keeps the total.

Name note: `ui.rs` declares a child module `process`, so inside `ui.rs` the path `process::visible_rows` names `ui::process`. Write `crate::process::visible_rows`.

Borrow note: to draw the table while `app.table` is borrowed mutably, destructure the fields: `let App { snapshot, table, highlighted, .. } = app;` and call `crate::process::visible_rows` on `snapshot`.

### 7.6 Decision for the maintainer: memory gauge on a Linux ZFS host

`sysinfo` reports Linux used memory as `total − MemAvailable` (`linux/system.rs:332-333`, checked). The ARC is not in `MemAvailable` (from memory, not checked against the kernel), so on a ZFS host the RAM gauge counts the ARC as used, often 80–90%. This is the first value that the ZFS user sees. FreeBSD does not have the problem: `sysinfo` already subtracts the ARC from wired memory (`bsd/freebsd/system.rs:593-597`, checked).

| Option | Effect |
|---|---|
| A (recommended) | On Linux, when `zfs` is `Some`, `mem_used = arc_adjusted_used(mem_used, arc)`: the shrinkable ARC (`size − c_min`) counts as free. This is what htop does (from memory). The info line `· arc {size}` tells the user that ARC exists. |
| B | Keep `total − MemAvailable`. Only the info line shows the ARC size. |

T1 writes `arc_adjusted_used` in both cases (cfg `any(test, target_os = "linux")`). With option B, S2 deletes it and its test in step 4, and records the change.

## 8. Key bindings (final)

Normal mode ignores a key that has the Ctrl or Alt modifier, except Ctrl-C. Shift is allowed.

| Mode | Key | Action |
|---|---|---|
| Any | `Ctrl-C` | Quit |
| Normal | `q` | Quit |
| Normal | `?`, `F1` | Open the help overlay |
| Normal | `Esc` | Clear the filter and the footer message |
| Normal | `/` | Start the filter |
| Normal | `s` | Next sort column (CPU, MEM, IO, PID, NAME) |
| Normal | `r` | Reverse the sort direction |
| Normal | `t` | Tree view on or off |
| Normal | `c` | Name or full command line |
| Normal | `x`, `F9` | Kill the selected process (asks first) |
| Normal | `n` | Network: totals or each interface |
| Normal | `1`–`8` | Show or hide a panel: 1 cpu, 2 history, 3 memory, 4 network, 5 storage, 6 zfs, 7 sensors, 8 processes |
| Normal | `0` | Show all panels |
| Normal | `space` | Pause or resume |
| Normal | `+`, `=` / `-` | Interval ×2 / ×0.5 |
| Normal | `↑` `↓` `j` `k` | Move the selection |
| Normal | `PgUp` `PgDn` | Move by 10 rows |
| Normal | `Home` `g` / `End` `G` | First / last row |
| Filter | text, `Backspace` | Edit the filter |
| Filter | `Enter` / `Esc` | Keep / clear the filter |
| Help | `Esc`, `?`, `q`, `F1` | Close the overlay |
| Confirm kill | `y` | Send TERM |
| Confirm kill | `f` | Send KILL (force) |
| Confirm kill | `n`, `Esc` | Cancel |

Other keys in Help mode do nothing. In Confirm kill mode every other key, `Enter` included, cancels the prompt with the status `kill cancelled`; so do `y` or `f` with Ctrl or Alt, a paste, and `y` or `f` that arrive before a frame has drawn the prompt (`app.kill_prompt_drawn`). `main.rs` handles a batch of keys before it redraws, so without that check a pasted or blind-typed `x…y` would send a signal for a prompt nobody saw. `main.rs` also turns on bracketed paste, so a paste arrives as one `Event::Paste`: the filter takes its text (control characters dropped), Confirm kill cancels, and other modes ignore it.

Conflict check: each key has one action in each mode. `x` is not next to `j` or `k`. `K` stays free. A digit for a panel with no data on this host puts `{name}: not available here` in the footer.

Kill flow in `App`:

1. `x` when `app.highlighted` is `None`: status `nothing selected`.
2. `x` on a protected PID (`check_target` with `std::process::id()`): status from `KillError::Protected`. Mode stays Normal.
3. Otherwise: `kill_prompt_drawn = false`, `mode = ConfirmKill(app.highlighted.clone())`. The target is the row that the last frame showed. `main.rs` drains a batch of events before it redraws (`src/main.rs:69-72`), so a `Metrics` event before `x` in one batch re-sorts the rows; the selection index then points at another process. `highlighted` does not change until the next draw.
4. `y` or `f` (no Ctrl or Alt) after a frame has drawn the prompt: send `KillRequest`, set status `sending {sig} to {pid} {name}…`, mode Normal. If no sender, or the send fails: status `kill is not available`.
5. `Event::KillDone(report)`: status = `report.message()`. This event is handled also when paused.

## 9. Invariants

These must hold before, during and after the change.

1. **No locks.** The sampler owns every `sysinfo` handle and collector. A `Snapshot` is moved, never shared. The only shared value is the interval atomic.
2. **No I/O on the UI thread** after the TUI starts. Signals go out on the sampler thread.
3. **A panel with no data takes no space.** No ZFS, no battery and no temperatures, or no volumes, means no request to `layout::plan`.
4. **Kill safety.** rtop never signals PID 0, PID 1, its own PID, or PID 4 on Windows. A signal goes out only after `y` or `f` in Confirm kill mode. The target is the (pid, start_time) of the row on screen when `x` was pressed, checked against a fresh refresh on the sampler thread.
5. **No panic** on any input: any parser returns `Option` or `Result`; rate and ratio arithmetic is checked or saturating; NaN temperatures are dropped; every draw function accepts any `Rect`, including 1×1.
6. **No since-boot spikes.** The first reading of a rate counter (disk for each (device, mount), process I/O) gives rate 0. ARC ratios with no window are since-boot ratios, and the panel labels them `boot`. No rate divides a since-boot total by one interval.
7. **Cheap ticks.** No process spawn on a tick. The disk list, disk space, command lines, sensors, battery, user list and macOS thread counts refresh at most once each 5 s. A fast tick reads I/O counters only for kept, non-ZFS disks.
8. **Ownership.** Stage 1 tasks change only their own files. `Cargo.toml` and `Cargo.lock` do not change after Stage 0.
9. **Size.** No new crate in `Cargo.lock`. The release binary grows by 150 KiB or less.
10. **Targets.** Each stage compiles for `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-pc-windows-msvc` and `x86_64-unknown-freebsd`.
11. **Existing behaviour stays.** Current keys keep their actions. The existing tests pass, changed only where a struct gets new fields or the sort cycle gets `Io`.
12. **Small screens.** When Processes is shown and the body is at least 9 high, the process table has at least 6 rows (as today at 80×24). Memory never collapses while History, Storage or Sensors is shown.
13. **Per-target builds.** No item is dead in a non-test build on any target in invariant 10, without the S0 `allow` lines (§5.5).

## 10. Risks

| Risk | Detect | Response |
|---|---|---|
| macOS O3X does not register `kstat.zfs.misc.arcstats` (not verified). | A Mac with O3X shows no ZFS panel. `sysctl kstat.zfs.misc.arcstats.size` fails. | The panel stays hidden. That is correct behaviour. Record it in the README later. |
| An OpenZFS release drops a field that `from_pairs` requires (`size`, `c`, `c_max`). | The ZFS panel is missing on a ZFS host. | Test with both fixtures. Make only those three fields required. |
| macOS IOKit battery FFI is wrong or needs more CF features. | `cargo check --target aarch64-apple-darwin` fails, or no battery line on a MacBook. | Time box. Fall back to `None` on macOS. Ask the maintainer about `starship-battery`. |
| PID reuse between the frame and the kill. | Resolved in review: a one-PID refresh replaces the entry when the start time changes (`linux/process.rs:645-668`, `macos/process.rs:725`). T4 test still covers `Gone`. | None needed. |
| macOS `f_mntfromname` of a volume is not `/dev/diskN…` (for example a network or disk-image mount), so `apple_whole_disk` returns `None`. | The volume groups by its own `device`. | Correct: such a volume has its own counter. |
| Two APFS containers on one physical disk count their shared driver twice (assumed rare, not checked). | Storage total higher than `iostat -d` on such a Mac. | Accept for this round. |
| The ARC is in fact counted in `MemAvailable` on some kernel (not checked). | On a Linux ZFS host, rtop's used memory is lower than `free`'s by more than the ARC. | Option B in §7.6. |
| A dataset mount on Linux ZFS shows `n/a` I/O, which the ZFS user may read as a defect. | User report. | Round 2: `objset-*` kstats. |
| macOS `PROC_PIDTASKINFO` fails for processes of other users without root. | The Threads column shows `-` for root processes. | Accept. It is a display gap only. |
| The first command-line read on macOS allocates `arg_max` for each process. | The first sample takes longer (estimate 10–50 ms, not measured). | Accept. It happens once for each process. |
| Windows `Components` (WMI) is slow at start. | The first frame is late on Windows. | It runs on the sampler thread once. Accept. |
| A panel's `want_height` and the draw function disagree. | TestBackend tests show cut lines at `want_height`. | Each UI task tests that all lines show at `want_height`. |
| At 80×24, History collapses whenever row 2 has a panel (every host with a volume), and Sensors collapses next to Storage and Zfs. | Worked example 1 in 6.10. The footer says `{n} hidden`. | By design: the process table keeps today's 6 rows. When the user hides every row-2 panel (keys `5`, `6`, `7`), History comes back at 80×24 (6 + 5 ≤ 13). |
| Binary growth from `objc2-io-kit` `ps`. | Stage 3 size check. | Over 150 KiB: look at `cargo bloat` in a scratch copy. |
| Cross targets are not installed (`rustup target list --installed` shows only the two Apple targets today). | `cargo check --target …` fails with "target may not be installed". | Stage 0 adds them with `rustup target add`, after the lead confirms with the maintainer. |

## 11. Rejected alternatives

- **One shared `Snapshot` edit in Stage 1**: every task would edit `metrics.rs` and conflict.
- **UI thread sends signals with `libc::kill` or `TerminateProcess`**: no fresh start time to stop a PID-reuse kill; needs more FFI.
- **A generic `Command` enum on the sampler channel**: only kill needs it now.
- **`toml` crate**: about five new crates plus `serde` for eight flat keys.
- **`lexopt` or `clap`**: four new flags do not need a crate.
- **`starship-battery`**: adds `uom`, `lazycell`, `mach2` for three fields.
- **`battery` crate**: no release since 2020.
- **A panel registry with trait objects**: more code than a fixed row table, and no gain for eight panels.
- **Per-frame cache of tree rows**: a redraw happens once for each event; 500 processes sort in microseconds.
- **Bind-mount a fixture over `/proc/spl` in Docker**: runc refuses mounts under `/proc`. A debug-only env path is used instead.
- **Hide the ZFS panel last in the collapse order**: then Memory goes first on small screens, and Memory is the more general panel. A ZFS user can hide History with `2` or in the config.
- **Height pass that removes only panels whose removal saves height**: on a tie it skips Storage and removes Memory; strict order plus a put-back pass keeps Memory.
- **Dedupe disk rates by an equal `(total_read_bytes, total_written_bytes)` pair**: each `Disk` reads its counter in a separate IOKit call, so a busy disk gives two different pairs and the total doubles on that tick; the whole-disk group is stable.
- **One `Disks::refresh_specifics` call on each tick**: it re-lists mounts on each call.
- **ARC hit ratio over one tick**: at 250 ms an idle host has no reads on most ticks, so the value flickers `—`.
- **Ship group A alone as a first release, with ZFS in place of History and today's fixed layout**: less regression risk per release, but Stage 1 runs all groups in parallel, so A is not done sooner, and the fixes in revision 2 remove the layout and I/O defects that made one round risky.

## Appendix A. ZFS fixtures (Stage 0 writes these files)

These are synthetic files in the OpenZFS 2.2 kstat format. The values are chosen for exact test arithmetic. They are not a capture from a real host (plan §3.4 asks for one). Rows that rtop does not read (`overhead_size`, `hdr_size`, `dbuf_size`, `memory_*` and others) test that unknown names are skipped. `arc_meta_used` is absent, as in 2.2 (from memory). Each row is the name padded to 32 columns, the type padded to 5 columns, then the value. Each file ends with one newline.

Sums that the tests depend on: `hits + iohits + misses` = 10 000 000. Demand data = 5 000 000 reads, demand metadata = 4 000 000, prefetch = 1 000 000. `l2_hits + l2_misses` = `misses`.

### A.1 `testdata/arcstats-2.2.txt`

```text
13 1 0x01 37 10656 4625134567890123 1049387654321098
name                            type data
hits                            4    8500000
iohits                          4    37116
misses                          4    1462884
demand_data_hits                4    4000000
demand_data_iohits              4    10000
demand_data_misses              4    990000
demand_metadata_hits            4    3800000
demand_metadata_iohits          4    5000
demand_metadata_misses          4    195000
prefetch_data_hits              4    600000
prefetch_data_iohits            4    20000
prefetch_data_misses            4    250000
prefetch_metadata_hits          4    100000
prefetch_metadata_iohits        4    2116
prefetch_metadata_misses        4    27884
c                               4    10737418240
c_min                           4    1073741824
c_max                           4    17179869184
size                            4    9663676416
compressed_size                 4    3000000000
uncompressed_size               4    7260000000
overhead_size                   4    123456789
hdr_size                        4    52428800
data_size                       4    6552476672
metadata_size                   4    2147483648
dbuf_size                       4    104857600
dnode_size                      4    209715200
bonus_size                      4    31457280
mru_size                        4    5368709120
mfu_size                        4    3221225472
l2_hits                         4    365721
l2_misses                       4    1097163
l2_size                         4    214748364800
l2_asize                        4    171798691840
l2_hdr_size                     4    83886080
memory_all_bytes                4    34359738368
memory_free_bytes               4    4294967296
```

### A.2 `testdata/arcstats-2.2-next.txt`

The same file as A.1, with exactly these eight rows changed. All other rows are byte-for-byte the same.

| Row | A.1 value | A.2 value | Change |
|---|---|---|---|
| `hits` | 8500000 | 8509000 | +9000 |
| `misses` | 1462884 | 1463884 | +1000 |
| `demand_data_hits` | 4000000 | 4004500 | +4500 |
| `demand_data_misses` | 990000 | 990500 | +500 |
| `demand_metadata_hits` | 3800000 | 3804500 | +4500 |
| `demand_metadata_misses` | 195000 | 195500 | +500 |
| `l2_hits` | 365721 | 365821 | +100 |
| `l2_misses` | 1097163 | 1098063 | +900 |

Expected values (T1 tests):

| Call | Result |
|---|---|
| `derive(None, &a1)` | `hit_since_boot` true; `hit_pct` 85.0; `iohit_pct` 0.37116; `demand_data_pct` 80.0; `demand_meta_pct` 95.0; `l2` `Some { size: 214748364800, asize: 171798691840, hit_pct: Some(25.0), since_boot: true }`; `compress_ratio` 2.42; `data` 6552476672; `metadata` 2147483648 |
| `derive(Some(&a1), &a2)` | `hit_since_boot` false; `hit_pct` 90.0; `iohit_pct` `Some(0.0)`; `demand_data_pct` 90.0; `demand_meta_pct` 90.0; `l2.hit_pct` 10.0, `l2.since_boot` false |
| `derive(Some(&a2), &a1)` (counter reset) | the same as `derive(None, &a1)` |
| `derive(Some(&a1), &a1)` (no reads) | the same as `derive(None, &a1)` |

Compare each `f64` with `(x - want).abs() < 1e-9`, except `compress_ratio`, which uses `< 1e-6`.

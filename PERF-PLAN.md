# Performance plan: CPU round 1

Status: done on macOS, 2026-10-21. Accepted 2026-10-20 and revised after a critic review. Branch: `perf/cpu-round-1`. Results are in section 11.

A user reported that htop uses about 0.2% CPU and rtop about 1.5%. This plan finds where rtop spends its CPU and removes the cost, without a change to what rtop shows, except where section 7 says so.

This file is at the repo root because Chad asked for `PERF-PLAN.md` by name. The project keeps other plans in `docs/plans/<topic>/`.

## 1. Goal and targets

Measure steady-state CPU over 60 s, two runs, at 120×40, with `--no-config -i 1000`, on the reference Mac (section 2). Compare with `htop -d 10` on the same host at the same time.

| Stage | Target (steady CPU, % of one core) |
|---|---|
| Baseline | 2.85% (measured) |
| After wave A (sampler, cadence, macOS processes) | 0.9% or less |
| After wave B (Linux processes, UI) | 0.7% or less |
| Stretch | 0.5% or less, the same as htop (0.47%) |

Other limits:

- No change to the values on the screen, except the deliberate differences in section 7.
- The stripped release binary on `aarch64-apple-darwin` grows by 40 KiB or less. Baseline: 926,192 bytes.
- No new crate in `Cargo.lock`. `libc` is already in the lock file; Linux gets it as a direct dependency (W4).

## 2. Baseline

Reference host: Apple M5, 10 cores, macOS 27.0.1, 609 processes (369 owned by the user, 240 by other users). rtop 0.2.0 at `20a6b96`. htop 3.5.1.

Method: `scripts/cpubench.py` runs the program in a pseudo-terminal, lets it warm up for 5 s, then reads its CPU time with `ps` and, at exit, its `rusage` from `wait4`.

| Program | Interval | Steady CPU | User (60 s) | Sys (60 s) | Context switches/s |
|---|---|---|---|---|---|
| rtop | 1.0 s | 2.80%, 2.90% | 0.26 s | 1.50 s | 47-61 |
| rtop | 250 ms | 9.13% | 0.79 s | 4.72 s | 102 |
| htop | 1.0 s | 0.46%, 0.47% | 0.08 s | 0.23 s | 5 |
| htop | 1.5 s (default) | 0.47%, 0.47% | 0.08 s | 0.23 s | 3-10 |

rtop costs about 6 times as much as htop. 85% of the cost is kernel time, so the cost is in system calls, not in Rust code.

`sample_cost` (W0) on the baseline: a fast tick takes 16.3 ms on average, a slow tick 106 ms.

## 3. Where the time goes

`sample` (macOS, 1 ms interval, 30 s) on a release build with symbols. Idle waits are left out. One sample is about 1 ms of CPU. The shares come from stacks, not from separate measurements, so each work item is measured again when it lands.

| Cost | Samples | Share | Where |
|---|---|---|---|
| `sysctl(KERN_PROCARGS2)`, 2 for each process on each tick | 243 | 28% | `sysinfo` macOS `update_process` → `get_process_infos` |
| Sensors: IOHID IPC for each sensor, and a re-list | 189 | 22% | `Components::refresh`, each 5 s, about 31 ms each time |
| Other `proc_pidinfo` and `proc_pid_rusage` calls | about 140 | 16% | 4 calls for each process on each tick |
| The 50 ms poll loop: `thread::sleep` → mach `clock_get_time` | about 130 | 15% | `Sampler::wait_until_due` |
| Disk space: `CFURLCopyResourcePropertiesForKeys` → CacheDelete → `os_log` | 55 | 6% | `Disks::refresh_specifics` with storage, each 5 s |
| UI thread: draw and terminal writes | 48 | 5.5% | `ui::draw`, mostly `draw_processes` |
| Disk I/O: IOKit registry walk for each kept volume | 25 | 3% | `Disk::refresh_specifics(io_usage)` on each tick |
| Network: `getifaddrs`, and `if_indextoname` for each interface | 23 | 3% | `Networks::refresh` |

### 3.1 Process refresh on macOS

`sysinfo` 0.39.6 calls `get_process_infos` for every known process on every refresh (`unix/apple/macos/process.rs:750`). No `ProcessRefreshKind` turns it off. That function makes two `sysctl(KERN_PROCARGS2)` calls, which copy the argument area out of the other process. Calls on each tick, today:

| Processes | Calls for each process | Result |
|---|---|---|
| Own (369) | `TBSDINFO`, 2 × `KERN_PROCARGS2`, `THREADINFO`, `TASKINFO`, `proc_pid_rusage` | 6 calls |
| Other users' (240) | the same 6 calls, plus `proc_pidpath` | `TBSDINFO`, `TASKINFO`, `THREADINFO` and `rusage` fail with `EPERM` on every tick |

That is about 3,650 calls on each tick. As a normal user, on another user's process, `PROC_PIDT_SHORTBSDINFO`, `PROC_PIDUNIQIDENTIFIERINFO` and `proc_pidpath` work; `TASKALLINFO`, `TBSDINFO`, `TASKINFO` and `proc_pid_rusage` fail with `EPERM`. So today these processes show no user, no parent, start time 0 and state `?`. For a zombie, every call fails with `ESRCH`, so `sysinfo` drops it. `kernel_task` (PID 0) has no path, so `sysinfo` drops it too.

htop reads the process list once, then makes one `TASKINFO` call for each process that it can read. It reads the arguments only once for each process.

### 3.2 Process refresh on Linux (from the source; no Linux host in this round)

rtop asks for `with_tasks()`, so `sysinfo` lists `/proc/<pid>/task` and treats each thread as a process. For each process it also reads `/proc/<pid>/status` to find the thread group. For each process and each thread it reads `stat`, `statm` and `io`. rtop then drops the threads, and uses the task list only to count them. On a desktop with 400 processes and 2,000 threads, that is about 8,000 file reads on each tick. The thread count is already in field 20 of `/proc/<pid>/stat`.

## 4. Decisions

Chad made these decisions on 2026-10-20:

| Question | Decision |
|---|---|
| Scope of the process change | Native collectors on macOS and Linux. Windows and FreeBSD keep `sysinfo`. |
| Cadence of slow collectors | Smarter refresh: sensors only while the panel is drawn, a subset each 5 s and a full scan each 30 s; disk space each 30 s. |
| Linux measurement | None in this round. Linux code gets unit tests on fixtures, live tests that run in CI, and clippy for two Linux targets. |
| Git | Branch `perf/cpu-round-1`, one commit for each work item, no push. |

Decision from the critic review: the kill check on macOS uses `p_uniqueid` (unique for each process since boot), not the start time. It works on every live process, including other users' and setuid processes. Today's start time is 0 for those, so the check compares nothing, and a setuid process such as `login` can still receive a signal from the user.

## 5. Work items

Each item lists the change, the files, the expected saving on the reference Mac, and the tests. Each worker measures its item with `sample_cost` and `scripts/cpubench.py` when it lands.

### W0. Measurement tools (lead, done: `8a1205b`)

- `scripts/cpubench.py` (stdlib only) and `scripts/README.md`.
- An ignored test `metrics::tests::sample_cost` that times fast and slow ticks. Run: `cargo test --release sample_cost -- --ignored --nocapture`.

### W0b. Shared interfaces (lead, before wave A)

No behaviour change. It fixes the interfaces that more than one worker uses, so wave A workers do not edit the same lines.

- `ProcInfo::start_time` and `KillTarget::start_time` become `ident: u64`, the process identity for the kill check. The `sysinfo` collector fills it with the start time in seconds, as now. Nothing on the screen shows it.
- `ProcInfo::name` and `KillTarget::name` become `Arc<str>`, so a tick does not allocate a name for each process.
- `ProcCollector::collect` takes `&mut sysinfo::System` and refreshes the processes itself. `ProcCollector::signal` does the kill check and the signal. `metrics.rs` no longer calls `refresh_processes_specifics`.
- `Sampler::new` calls `collect` once as a baseline before the first 200 ms sleep, so the first frame has CPU percent.
- `process.rs` is split: `process/sysinfo_procs.rs` holds today's `sysinfo` collector and kill code (Windows included). `process.rs` keeps the public types, the pure functions and the module wiring.

### W1. Sampler control channel (worker S)

- Replace the kill channel with a control channel: `enum Control { Kill(KillRequest), Wake, Pause(bool) }`.
- `wait_until_due` blocks in `recv_timeout(due - elapsed)`. A `Kill` is handled at once through `ProcCollector::signal`, as now. `Wake` makes the sampler read the interval again, so `+` and `-` still take effect at once. The UI stores the new interval before it sends `Wake`, so no wake-up is lost. One wake-up for each tick, not 20.
- The UI sends `Pause(true|false)` on space.
- While paused, the sampler does not sample. It still handles kills and sends `KillDone`. On `Pause(false)` it takes one sample, does not send it (it sets the baseline for every rate), and sends the next one after the interval. The screen keeps the frozen snapshot until then, as it does now.
- If the control channel closes, the sampler goes on sampling with a plain sleep, as now.
- Files: `metrics.rs`, `app.rs`, `main.rs`.
- Saving: about 130 samples (15%).
- Tests: `wait_until_due` with short intervals: a kill is handled at once, `Wake` shortens the wait, `Pause` stops samples and `KillDone` still arrives, the sampler goes on when the channel closes. `app.rs`: `+`, `-` and space send `Wake` and `Pause`.

### W2. Panel-aware, per-collector cadence (worker S)

- Each slow collector gets its own period, from a pure helper `due(last, now, period) -> bool` (a general form of `is_slow`).
- The UI publishes the set of panels it drew in the last frame (`layout::plan` areas) in an `Arc<AtomicU16>` bit set. It starts as all panels, so the first ticks discover everything. This is the second shared atomic, after the interval.
- Sensors:
  - The first slow tick does a full `Components::refresh(true)`, so `has_data` knows if the host has sensors.
  - While the sensors panel is drawn: each 5 s refresh only the 12 hottest components of the last reading, chosen by index in `Components::list_mut` (labels can repeat), with `Component::refresh` on each. The panel shows 6. Each 30 s, a full re-scan.
  - While the panel is not drawn: no sensor refresh, and the snapshot keeps the last value. But while the last reading has no temperatures, a full scan runs each 30 s whatever is drawn. Without this, an empty reading hides the panel (`has_data` is false), so it is never drawn and never read again.
  - When the panel is drawn again and the reading is more than 5 s old, refresh on the next tick.
  - The battery stays on the 5 s cadence (cheap).
- Disk space:
  - Each 5 s, re-list the disks without space (`DiskRefreshKind::nothing().with_io_usage()`); existing disks keep their space values. For mounts that are new since the last list, read the space in the same tick, before `keep_volume` (a new disk has total 0 until then). A new USB disk still shows within 5 s.
  - Each 30 s, and on the first tick, while the storage panel is drawn: read the space of all disks.
- Disk I/O:
  - The counters keep the current cadence (each tick) whether or not the panel is drawn, so a rate never spans a gap.
  - macOS: on a fast tick, refresh one disk for each `io_group` (whole disk), not each volume. Key the shared usage by `io_group` and give it to every volume of the group on every tick, the re-list tick too. Otherwise a volume that was refreshed only by the re-list carries a 5 s delta that is divided by one tick.
- Files: `metrics.rs`, `sensors.rs`, `storage.rs`, `ui.rs` (publish the drawn set only), `panel.rs` (bit set helpers), `app.rs`.
- Saving: sensors about 150 of 189 samples; disk space about 45 of 55; disk I/O about 12 of 25. Total about 200 samples (23%).
- Tests: pure tests for `due`, for the choice of the 12 hottest by index (with repeated labels), for the empty-reading rescan rule, for the new-mount set, and for the group-shared usage on a re-list tick; a UI test that the drawn set leaves out a collapsed panel.

### W3. Native macOS collector (worker P1)

`process/macos.rs` implements the interface of section 6.1.

On each tick:

1. `proc_listallpids` into a reused buffer.
2. For each PID:
   - A process seen before as readable: `PROC_PIDTASKALLINFO` (BSD info and task info in one call) and `proc_pid_rusage`.
   - A process seen before as denied: nothing on a fast tick. On a slow tick, `PROC_PIDT_SHORTBSDINFO` (parent, user, state) and `PROC_PIDUNIQIDENTIFIERINFO`. A new `p_uniqueid` means PID reuse: treat it as a new process.
   - A new PID: `PROC_PIDUNIQIDENTIFIERINFO` for `ident`, then `TASKALLINFO`.

Error rules:

- `TASKALLINFO` fails with `EPERM`: the denied path. `PROC_PIDT_SHORTBSDINFO` must work, and the name comes from `proc_pidpath`. If either fails, drop the PID, as `sysinfo` does (this drops `kernel_task`).
- `ESRCH`, or `PROC_PIDUNIQIDENTIFIERINFO` fails: drop the PID (this drops zombies and processes that just exited, as today).
- A readable process whose start time (`pbi_start_tvsec`, `pbi_start_tvusec`) changed is a new process: read its identity again. So a readable process costs no identity call on a fast tick.

Fields:

- `ident`: `p_uniqueid`. libc has neither `PROC_PIDUNIQIDENTIFIERINFO` (17) nor `proc_uniqidentifierinfo` (56 bytes), so write both by hand, and check the returned size.
- Name, readable process: on first sight, one `KERN_PROCARGS2` read. The name is the file name of the exec path, as `sysinfo` gives it. The command line comes from the same buffer, with empty arguments skipped, as `sysinfo` does. If that read fails, use `proc_pidpath`, then `pbi_name`, then `pbi_comm`.
- A command line that is still empty is read again on a slow tick, as now.
- User: `pbi_ruid`, or `pbsi_ruid` for a denied process (the real UID, as `sysinfo` gives it).
- Threads: `pti_threadnum` on each tick.
- CPU: `pti_total_user + pti_total_system` in mach time units, converted with `mach_timebase_info` (125/3 on Apple silicon, 1/1 on Intel), as a percent of one core over the measured interval. 0 on the first sample. Capped at `cpus × 100`.
- Memory: `pti_resident_size`.
- I/O: cumulative `ri_diskio_bytesread` and `ri_diskio_byteswritten`; rates from the change, 0 on the first sample.
- State, readable process: `SIDL` → Idle; `SRUN` → Run if `pti_numrunning > 0`, else Sleep; `SSLEEP` → Sleep; `SSTOP` → Stop; `SZOMB` → Zombie; other → Unknown.
- State, denied process: `SSTOP` → Stop, `SZOMB` → Zombie, `SIDL` → Idle, other (also `SRUN`, which macOS reports for most sleeping processes) → Unknown, as today.

Kill (`signal`):

- Check the PID as now (`check_target`). Read `PROC_PIDUNIQIDENTIFIERINFO` for the PID again. If it fails, or `p_uniqueid` differs from `ident`, report Gone. Else `libc::kill`. Any `kill` error gives the Failed message, as now.
- `signal` must not change the CPU or I/O baselines in the cache.

Also:

- Remove `macos_threads` and the macOS branch of the `sysinfo` collector once it is not used.
- Shared helpers in `process.rs` (section 6.1): `UserNames` and `cpu_percent`. P2 uses them in wave B.
- Files: `process.rs` (helpers and wiring), `process/macos.rs` (new).
- Saving: about 330 of 411 process samples (38%). Calls on each fast tick go from about 3,650 to about 740, with no `KERN_PROCARGS2`.
- Tests:
  - Pure: the CPU maths with 125/3 and 1/1, the state maps, the name choice, the argument parsing on a fixture buffer (empty arguments, no NUL at the end), the error rules on fake call results.
  - Live, macOS only: the native list and the `sysinfo` list have the same PID set, allowing for processes that start or exit in between; for every readable process they agree on name, user, parent and threads (1 or more).
  - Live: kill a `sleep` child through `ProcCollector::signal` with TERM; a wrong `ident` gives Gone and the child lives.

### W4. Native Linux collector (worker P2, after W3)

- `process/linux.rs`, with the pure parsers behind `cfg(any(test, target_os = "linux"))`, so their tests run on every host (the pattern of `net::SysfsNet`).
- On each tick, for each numeric entry of `/proc`:
  - Read `/proc/<pid>/stat` into a reused buffer. Parse the name (between the first `(` and the last `)`), state, parent, flags, `utime`, `stime`, `num_threads`, `starttime` and `rss`.
  - Skip kernel threads (`PF_KTHREAD`), as today. Remember them, and read their `stat` again only on a slow tick, to find PID reuse.
  - Read `/proc/<pid>/io` (`read_bytes`, `write_bytes`). After `EACCES` or `EPERM`, do not read it again for that process.
- On the first sight of a process: the real UID from the `Uid:` line of `/proc/<pid>/status`, and `cmdline`. Split `cmdline` on NUL, trim ASCII white space from each argument and drop empty ones, as `sysinfo` does. A process whose command line is empty is read again on a slow tick.
- No `with_tasks()`, no `statm` (`rss` from `stat` is the same value), no `status` on each tick.
- `ident`: `starttime` in clock ticks since boot. A changed `starttime` means PID reuse.
- CPU: `utime + stime` in clock ticks, converted to nanoseconds, then `cpu_percent`.
- Memory: `rss × page size`. Threads: `num_threads`.
- State: `R` Run, `S` Sleep, `D` DiskSleep, `T` and `t` Stop, `Z` Zombie, `X` and `x` Dead, `I` Idle, other Unknown (the map `sysinfo` and `ProcState::from_sysinfo` give today).
- Kill (`signal`): read `/proc/<pid>/stat` again; if it fails or `starttime` differs from `ident`, report Gone; else `libc::kill`. Any `kill` error gives Failed.
- `CLK_TCK` and the page size come from `libc::sysconf`. Add `target_os = "linux"` to the `libc` target in `Cargo.toml`.
- Remove `linux_threads` and the Linux branch of the `sysinfo` collector.
- Files: `process/linux.rs` (new), `process.rs` (wiring only), `Cargo.toml`, `testdata/proc/` (new fixtures).
- Saving on Linux: from about `4 × (processes + threads) + processes` file reads on each tick to about `2 × processes`. Not measured in this round.
- Tests:
  - Pure, on fixtures: a name with spaces and `)`, a kernel thread, a zombie, a missing `io`, a `cmdline` with empty arguments and white space, the `Uid:` line, the state map, the I/O-denied cache.
  - Live, `cfg(target_os = "linux")` (they run in CI): native against `sysinfo` for the test process; kill a `sleep` child; a wrong `ident` gives Gone.
  - Clippy for `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`.

### W5. Draw only the visible process rows (worker U, after W3)

- `draw_processes` builds `Row`s only for the rows that fit in the table, not for all processes. It computes the window with the same rules as ratatui's `Table` (rows are 1 line high and the column widths are fixed) and keeps `app.table` as it is now.
- Files: `ui.rs` (`draw_processes`), `ui/process.rs`.
- Saving: about 20 of 48 UI samples on 609 processes; more on hosts with thousands of processes.
- Tests: an equivalence test that renders the windowed table and the full table into `TestBackend` for many cases (rows, height, selection, offset, and key sequences with `j`, `k`, `PageDown`, `PageUp`, `g`, `G`), and checks that the screens and the resulting `TableState` are the same. An ignored `draw_cost` bench with 2,000 processes.

### W6. macOS storage and network (worker N, wave B)

Added after wave A, from a probe that timed the thread CPU time of each collector on the reference Mac (wall time includes waits on other processes, such as `hidd` for sensors):

| Work | rtop CPU each time | How often | CPU per second |
|---|---|---|---|
| Disk I/O: `IOServiceGetMatchingServices` and a registry walk for one whole disk | 2.20 ms | each tick | 2.2 ms |
| Processes (native, W3) | 2.04 ms | each tick | 2.0 ms |
| Network: `if_indextoname` (a whole `getifaddrs`) for each of 19 interfaces | 1.78 ms | each tick | 1.8 ms |
| UI draw (W5) | - | each tick | about 1.4 ms |
| Disk space: reads every listed disk, kept or not | 29.1 ms | 30 s | 1.0 ms |
| Disk re-list: refreshes the I/O of every listed disk | 3.98 ms | 5 s | 0.8 ms |
| Sensors: 12 hottest; full scan | 0.46 ms; 2.55 ms | 5 s; 30 s | 0.2 ms |

Changes, macOS unless said:

- Disk I/O: find the `IOBlockStorageDriver` registry entry of each whole disk once (when the disk list changes), keep it, and read only its `Statistics` property on each tick (`IORegistryEntryCreateCFProperty`). Release the entries when a disk goes. The counters and rates stay the same.
- Disk re-list (every OS): re-list without the I/O kind; the I/O loop of each tick then runs on re-list ticks too, for the group leaders only.
- Disk space (every OS): read the space only of disks that `keep_volume` keeps by mount and file system, on the 30 s read and for new mounts.
- Network: a native reader like `SysfsNet`. Take the names from the `sockaddr_dl` after each `RTM_IFINFO2` message of one `sysctl(NET_RT_IFLIST2)` call, not from `if_indextoname`. Keep 64-bit counters: `sysinfo` issue #1378 says the counters in that buffer can be 32 bits, so read `IFMIB_IFDATA` for each interface if that is still true. Same interfaces and totals as `sysinfo` gives today.
- Files: `net.rs`, `storage.rs`, `metrics.rs` (the network wiring), `Cargo.toml` (`objc2-io-kit` features only, no new crate).
- Saving: about 4.5 ms of CPU each second (about 0.45%).
- Tests: a pure parser for the routing-message buffer on a synthetic buffer; the `Statistics` dictionary reader on a built `CFDictionary`; live macOS tests that the native network list and the `sysinfo` list agree on names and totals, and that the disk counters equal the `sysinfo` counters.

Still optional, only if the stretch target is not met: skip `proc_pid_rusage` while the I/O columns are not drawn and the sort is not I/O (the first sample after they show again gives rate 0).

### W7. Documents (lead)

- `docs/architecture.md`: the control channel, the drawn-panel set, the per-collector cadence table, the native process collectors, the kill identity.
- `docs/usage.md` and the kill flow: "start time" becomes "identity" where the text means the check.
- `CHANGELOG.md` (Unreleased): lower CPU use; the cadence changes; macOS now shows the user and parent of other users' processes; the kill check on macOS uses the unique process ID.
- `docs/platforms.md`: the macOS and Linux process sources.
- `CONTRIBUTING.md`: how to run `scripts/cpubench.py` and `sample_cost`.
- This file: the results in section 11.

## 6. Interfaces

### 6.1 Process collector

Every OS has a type `process::ProcCollector` with:

```rust
pub fn new() -> Self;
/// Reads all processes. `sys` is used only by the sysinfo collector.
pub fn collect(&mut self, sys: &mut sysinfo::System, secs: f64, slow: bool) -> Vec<ProcInfo>;
/// Checks the target again with the same identity as `collect`, then sends the signal.
pub fn signal(&mut self, sys: &mut sysinfo::System, req: &KillRequest, self_pid: u32) -> KillReport;
```

`ProcInfo::ident` and `KillTarget::ident`: macOS `p_uniqueid`; Linux `starttime` in clock ticks; other OSes the start time in seconds since the epoch.

Shared helpers in `process.rs` for the native collectors (`cfg(any(test, target_os = "linux", target_os = "macos"))`):

- `UserNames`: UID (`u32`) to name, loaded from `sysinfo::Users`, reloaded on a miss at most once each 60 s (today's rule).
- `cpu_percent(delta_ns: u64, secs: f64, cpus: usize) -> f32`: percent of one core, capped at `cpus × 100`, 0 for a bad `secs`.
- `io_rate` (exists): 0 on the first sample of a process.

### 6.2 Control channel

```rust
pub enum Control {
    Kill(KillRequest),
    /// The interval changed: read it again now.
    Wake,
    /// Stop or start sampling. Kills still work while paused.
    Pause(bool),
}
```

### 6.3 Drawn-panel set

`Arc<AtomicU16>`, one bit for each `PanelId`. The UI stores it after each `layout::plan`. The sampler reads it with `Relaxed` ordering at the start of a tick. A stale value costs at most one extra or one skipped refresh.

### 6.4 Cadence

| Work | Period | Condition |
|---|---|---|
| CPU, memory, processes, network, disk I/O counters, ZFS counters | each tick | always |
| Disk list (space only for new mounts), battery, command lines that are still empty, other users' macOS processes, ZFS probe when absent | 5 s | always |
| User list | on a miss, at most each 60 s | slow tick |
| Sensors: the 12 hottest | 5 s | sensors panel drawn |
| Sensors: full re-scan | 30 s, and the first tick | sensors panel drawn, or the last reading has no temperatures (first tick: always) |
| Disk space of all disks | 30 s, and the first tick | storage panel drawn (first tick: always) |

## 7. Parity rules

These stay exactly the same:

- The process list: the same processes. Linux: no threads, no kernel threads. macOS: no zombies, no `kernel_task`.
- PID, parent, name, command line, user, memory and thread count for processes the user can read.
- CPU percent and I/O rates for a busy process, within the error of the sample timing.
- The kill flow, its protected PIDs and its messages. A kill still checks the process identity again before it sends the signal.

Deliberate differences, to list in the changelog where users see them:

- macOS, other users' processes: the user and the parent now show. The state stays `?` for most of them, as now (macOS reports `SRUN` for a sleeping process). CPU, memory and I/O stay unknown, as now.
- macOS: the kill check uses `p_uniqueid`, so it works for other users' and setuid processes too (today it compares start time 0 with 0).
- macOS: the thread count updates on each tick, not each 5 s.
- macOS: state `R` comes from `pti_numrunning > 0`, not from the state of the first thread. The `D` state (uninterruptible wait) and a stop of only the first thread no longer show; macOS shows them rarely.
- macOS: a process that goes idle shows 0% CPU. `sysinfo` keeps the last non-zero value when the CPU time does not change. CPU percent is capped at `cpus × 100`.
- The first frame shows I/O rates for processes seen at start-up (the start-up baseline), not 0.
- Sensors: a sensor that is not among the 12 hottest can be up to 30 s old. While the panel is hidden, readings stop.
- Disk space can be up to 30 s old.
- While paused, rtop does no sampling.

## 8. Execution

Each worker gets one batch, in its own git worktree, and commits there. The lead merges each batch into `perf/cpu-round-1`.

| Wave | Worker | Items | Owns | Model, effort |
|---|---|---|---|---|
| 0 | lead | W0, W0b, this plan | `scripts/`, `PERF-PLAN.md`, the interface change | - |
| A | S | W1, W2 | `metrics.rs`, `app.rs`, `main.rs`, `sensors.rs`, `storage.rs`, `panel.rs`, `ui.rs` (drawn set only) | Opus, medium |
| A | P1 | W3 | `process.rs`, `process/macos.rs`, `process/sysinfo_procs.rs` | Opus, high (FFI, kill identity) |
| B | P2 | W4 | `process/linux.rs`, `process.rs` (wiring), `process/sysinfo_procs.rs` (remove the Linux branch), `testdata/proc/`, `Cargo.toml` | Opus, medium |
| B | U | W5 | `ui.rs` (`draw_processes`), `ui/process.rs` | Opus, medium |
| B | N | W6 | `net.rs`, `storage.rs`, `metrics.rs` (network wiring), `Cargo.toml` (features) | Opus, high (IOKit and CF ownership) |
| C | reviewer | review the combined diff against this plan | read-only | Opus, high |
| C | fixers | high and medium findings, one batch for each file set | as the findings need | Opus, medium |
| C | verifier | section 9 checks | read-only | Sonnet, medium |
| C | lead | benchmark, W6 decision, W7, results | `PERF-PLAN.md`, `docs/` | - |

After W0b, S and P1 share no file. Wave B starts after wave A is merged and green. Agents: 8 (critic, S, P1, P2, U, N, reviewer, verifier), plus fixers if the review finds high or medium issues.

## 9. Verification

Each worker runs these before it reports, and the verifier runs them on the merged branch:

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
for t in x86_64-apple-darwin x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-pc-windows-msvc x86_64-unknown-freebsd; do
  cargo clippy --all-targets --locked --target "$t" -- -D warnings || break
done
cargo build --release && ls -l target/release/rtop
```

Acceptance:

1. All the checks above pass.
2. The benchmark in section 1 meets the target for the wave.
3. `sample_cost` reports each kind of tick. Limits on the reference Mac: fast tick 4 ms or less; 5 s tick 15 ms or less with the sensors panel hidden; 30 s tick 45 ms or less of thread CPU (`collector_cost`). Changed on 2026-10-21 from 60 ms of wall time: most of that tick's wall time is spent waiting on macOS services (the disk space service and `hidd`), not on rtop's own CPU.
4. A manual run on this Mac: the process table, tree, filter, sort, kill (TERM and KILL on a `sleep` process), pause, `+` and `-` work as before.
5. The binary grows by 40 KiB or less.

Before a merge to `main` (Chad's call, not part of this round): push the branch and get CI green on Linux, or run the tests in Docker as `CONTRIBUTING.md` shows. Then run `scripts/cpubench.py` against htop on a Linux host.

## 10. Risks

| Risk | Mitigation |
|---|---|
| The kill check and the collector identify a process in different ways, so a kill reports "not running now", or hits a new process after PID reuse | One function for each OS gives `ident` to both. macOS uses `p_uniqueid`, which every live process has. Live kill tests on macOS here and on Linux in CI. |
| Mach time units on Apple silicon | Convert with `mach_timebase_info`; test the maths with 125/3 and 1/1; clippy for `x86_64-apple-darwin`. |
| `unsafe` FFI mistakes (sizes, buffer lengths, hand-written structs) | Check each returned size against `size_of`; a `SAFETY:` comment on each block; the reviewer checks each block. |
| The process list changes (zombies, `kernel_task`, exited processes) | The error rules in W3; the live parity test over the whole list. |
| Linux code does not run on Linux in this round | Fixture tests here; live tests that run in CI; clippy for two Linux targets; CI or Docker before a merge to `main`. |
| A sensor outside the 12 hottest heats up | The full re-scan each 30 s finds it. |
| The sensors panel disappears for good after one empty reading | A full scan each 30 s while the reading is empty, whatever is drawn. |
| A disk I/O rate spans a gap | Counters refresh on each tick; the macOS group usage is shared on every tick. |

## 11. Results

Filled in as each wave lands.

| Stage | Steady CPU (1 s) | Steady CPU (250 ms) | Fast tick | Slow tick | Binary size | Notes |
|---|---|---|---|---|---|---|
| Baseline `20a6b96` | 2.80%, 2.90% | 9.13% | 16.3 ms | 106 ms | 926,192 | htop at 1 s: 0.46%, 0.47% |
| W1, W2 `4bb5bd8` | 2.09% (one run) | - | 19.3 ms | 35.0 ms (5 s), 89.5 ms (30 s) | 942,752 | Another worker was building during the run. |
| Wave A `4bfeca4` | 1.00%, 1.09% | 4.04% | 8.6 ms | 23.5 ms (5 s), 81.0 ms (30 s) | 942,704 | htop at 1 s: 0.44%. Target 0.9% not met; see W6. |
| Wave B `d85f0a1` | 0.33%, 0.33% | 1.13% | 1.95 ms | 14.1 ms (5 s), 67 ms (30 s) | 942,720 | htop at 1 s: 0.26% in the same window. |
| Final `ba38d42` (review fixes) | 0.56%, 0.60% | 2.13% | 3.9-4.4 ms | 17-20 ms (5 s), 82-108 ms (30 s, wall) | 942,800 | Next morning, a busier host: htop 0.55% in the same window. |

How to read these numbers: the host's load moves both programs. Compare rtop with htop in the same window, not across windows.

| Window | rtop | htop | rtop ÷ htop |
|---|---|---|---|
| Baseline | 2.85% | 0.47% | 6.1× |
| Wave A | 1.05% | 0.44% | 2.4× |
| Wave B | 0.33% | 0.26% | 1.3× |
| Final | 0.58% | 0.55% | 1.06× |
| A/B check (final) | 0.62%-0.73% | 0.46% | 1.4×-1.6× |

The A/B check ran the wave B and final binaries in turn in one window: 0.71% and 0.60% against 0.73% and 0.62%. The review fixes added no measurable cost.

Acceptance (section 9), final branch:

| Item | Result |
|---|---|
| fmt, clippy on the host and five targets, tests | Pass. 385 tests on macOS (twice, no flaky failures). F1 also ran the Linux tests in Docker (`rust:1.99-alpine`): 349 passed. |
| CPU targets | Stretch (0.5%) met in the wave B window; about htop's level in every window. |
| `sample_cost` | Fast tick 3.9-4.4 ms (limit 4 ms, at the edge on a busy host); 5 s tick with sensors hidden 4.5-6.0 ms (limit 15 ms); 30 s tick 24-28 ms thread CPU for the space read (limit 45 ms). |
| Manual run | A scripted run in a pseudo-terminal, checked through a terminal emulator: TERM and KILL reach the right `sleep` process, PID 1 is refused, pause uses 0 ms of CPU in 6 s, `+` and `-` work, the view keys work, `q` exits cleanly. |
| Binary size | 942,800 bytes, +16,608 (one 16 KiB page; the limit is 40 KiB). |
| `unsafe` | Every block and declaration has a `SAFETY` comment. |

Known issues after the round:

- Disk I/O counters are read after the re-list and the space reads on those ticks, so on a 30 s tick at 250 ms an I/O rate can be off by about 10% for one tick. This was so before the round.
- After `exec`, a process keeps the name and command line it had when rtop first saw it, as with `sysinfo`.
- After two failed counter reads in a row, a volume shows `n/a` until a read works.
- The process table still builds a `ProcRow` for each process on each frame (`App::visible_rows`); in tree view each row has a prefix string.
- Linux has not been benchmarked against htop.

Binary size note: on `aarch64-apple-darwin` the file grows in 16 KiB pages. W0b added 1,444 bytes of code but 16,528 bytes of file, because the code crossed a page boundary. Compare code size from a linker map (`-C link-arg=-Wl,-map,<file>`) as well as the file size.

Security finding during W3: `sysinfo` 0.39.6 on macOS parses `KERN_PROCARGS2` past the argument list when a process has empty arguments (processes that rewrite their title, such as Node and Electron helpers). rtop 0.2.0 then shows environment variables, secrets included, in the command column. The native parser (W3) counts empty arguments, so it stops at `argc`. Windows and FreeBSD do not use this parser.

## 12. Not in this round

- `sysctl(KERN_PROC_ALL)` on macOS, as htop uses: `libc` has no `kinfo_proc`, and a hand-written layout of that large struct is a risk. It would give other users' processes a real start time, but `p_uniqueid` already gives a better identity.
- A patched `sysinfo` (a fork or `[patch]`): a fork to maintain. An upstream issue about `get_process_infos` on each refresh is worth filing.
- `statfs` for disk space on macOS: it does not count purgeable space, so the numbers would change.
- A `BufWriter` around the terminal: about 7 samples, and it touches the terminal restore path.
- Skipping a redraw while paused: after W1 only one `Metrics` event can arrive while paused, and a skip could hide the kill prompt.
- A longer default interval: htop defaults to 1.5 s, but the interval is the user's choice.

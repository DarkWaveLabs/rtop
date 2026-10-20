# Handover: CPU round 1

Status on 2026-10-20: waves 0 and A are merged; wave B is in progress. Branch `perf/cpu-round-1`, not pushed. The full plan is in [PERF-PLAN.md](PERF-PLAN.md).

## The problem

A user reported that htop uses about 0.2% CPU and rtop about 1.5%. On the reference Mac (Apple M5, 10 cores, about 600 processes, 1 s interval, 120×40), rtop used 2.85% and htop 0.47%. 85% of rtop's cost was kernel time, so system calls were the cause, not Rust code.

## Results so far

| Stage | Steady CPU at 1 s | At 250 ms | Fast tick | Commit |
|---|---|---|---|---|
| Baseline | 2.80%, 2.90% | 9.13% | 16.3 ms | `20a6b96` |
| Sampler and cadence (W1, W2) | 2.09% | - | 19.3 ms | `4bb5bd8` |
| Native macOS processes (wave A) | 1.00%, 1.09% | 4.04% | 8.6 ms | `4bfeca4` |
| htop, same host | 0.44%-0.47% | - | - | - |

Wave B aims at about 0.5% to 0.6%, the same as htop.

## What we found

- **Processes on macOS.** `sysinfo` 0.39.6 copies the argument area of every process on every refresh (two `KERN_PROCARGS2` calls), and it cannot be turned off. With the four other calls, that was about 3,650 system calls on each tick. For other users' processes, most of these calls fail with `EPERM` on every tick.
- **Processes on Linux** (from the source, not measured). `with_tasks()` makes `sysinfo` read `stat`, `statm` and `io` for every thread as well as every process, and `status` for every PID. rtop then drops the threads.
- **The sampler loop.** It woke 20 times a second to check for kills. On macOS each `thread::sleep` costs a mach call.
- **Slow collectors.** The sensors re-list, the disk space read (CacheDelete and `os_log`) and the disk re-list ran every 5 s for every disk, also for disks rtop does not show.
- **What is left after wave A** (thread CPU per call, so not waiting time): disk I/O IOKit matching 2.2 ms per tick, processes 2.0 ms, network 1.8 ms (`getifaddrs` once per interface), UI about 1.4 ms per second, disk space 29 ms each 30 s, disk re-list 4 ms each 5 s. The sensors are cheap for rtop; their wall time is spent waiting on `hidd`.

## What landed

| Commit | Change |
|---|---|
| `8a1205b` | `scripts/cpubench.py` (stdlib Python, runs a TUI in a pseudo-terminal and prints its CPU use) and the ignored `sample_cost` test. |
| `c982fd7`, `dd486f9` | `PERF-PLAN.md`, revised after a critic review and after wave A. |
| `f3f2e16` | One process-collector interface for every OS (`collect`, `signal`). `start_time` became `ident`, the identity that the kill check compares. Names became `Arc<str>`. |
| `7961f3c` | A control channel (`Kill`, `Wake`, `Pause`). The sampler blocks until the next tick and does no sampling while paused. |
| `4bb5bd8` | Each slow collector has its own cadence. The UI publishes the panels it drew. Sensors: the 12 hottest each 5 s and a full scan each 30 s, only while the panel is drawn. Disk space each 30 s. macOS disk I/O: one refresh for each whole disk. |
| `4bfeca4` | A native macOS process collector: about 2 calls for each readable process on each tick, and none for other users' processes on a fast tick. The kill check uses `p_uniqueid`. |

## In progress (wave B)

- **P2:** a native Linux process collector, with the same design as the macOS one.
- **U:** the process table builds rows only for the visible window, not for every process.
- **N:** macOS disk I/O through a cached IOKit entry, a native macOS network reader, and space and re-list reads only for disks that rtop shows.

Next: merge wave B, one review of the combined diff, fixes for high and medium findings, a verifier pass, a benchmark, the document updates (W7 in the plan), and a manual run of the TUI.

## Testing done

- **Benchmark:** `scripts/cpubench.py`, 60 s runs, two runs for each main build, rtop against htop at the same interval. Numbers are above and in `PERF-PLAN.md` section 11.
- **Profiles:** `sample` on a release build with symbols, before and after wave A.
- **Per-collector probe:** thread CPU time (`CLOCK_THREAD_CPUTIME_ID`) against wall time for each collector, in a scratch copy. It showed that `sample` counts waiting time too, so the sensors looked costlier than they are.
- **Checks on each merge:** `cargo fmt --check`; `cargo clippy --all-targets -- -D warnings` for the host and for `x86_64-apple-darwin`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-pc-windows-msvc` and `x86_64-unknown-freebsd`; `cargo test` (334 passed, 1 ignored, after wave A).
- **New tests:** the cadence rules, the control channel with a fake sampler, the group-shared disk I/O, the sensor choice by index, and the macOS collector's decisions with fake system calls. Live macOS tests compare the whole process list with the `sysinfo` collector, and kill a `sleep` child through the identity check.
- **Size:** the binary changes in 16 KiB pages on arm64 macOS, so a linker map gives the real code growth. After wave A the file is 942,704 bytes, against 926,192 at the baseline.

## Not tested yet

- Linux: no Linux host or Docker in this round. The Linux code compiles for two Linux targets and its parsers have tests here. Its live tests run only in CI or in Docker.
- A manual TUI run of kill, pause, `+` and `-` on the merged branch.
- An Intel Mac, and running as root.

## Things worth knowing

- **Environment variables in the command column (fixed on this branch).** `sysinfo`'s macOS argument parser skips empty arguments without counting them, so it reads past `argc` into the environment. rtop 0.2.0 then shows environment variables of processes that rewrite their title (Node, Electron helpers). The native parser counts empty arguments. An upstream `sysinfo` issue is worth filing.
- **Kill identity.** For other users' processes, start time 0 was compared with 0, so the old check matched anything. The native collectors use `p_uniqueid` on macOS and the start time in clock ticks on Linux.
- **Process list parity.** Like `sysinfo`, the macOS collector drops zombies and `kernel_task`. Other users' processes now show their user and parent.

## Decisions for Chad

- Push the branch and let CI run the Linux tests, or run them in Docker, before a merge to `main`.
- A 0.2.1 release, or an advisory, for the command-line fix.
- Whether `PERF-PLAN.md` and this file move to `docs/plans/performance/` after the round.

## Housekeeping

- Agent worktrees live in `.claude/worktrees/`. Remove them after the merges with `git worktree remove`.
- Benchmark outputs and profiles are in the session scratchpad, not in the repo.

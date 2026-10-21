# Handover: CPU round 1

Status on 2026-10-21: done on macOS. Branch `perf/cpu-round-1`, 31 commits on `20a6b96`, not pushed. The full plan, the measurements and the acceptance results are in [PERF-PLAN.md](PERF-PLAN.md).

## The problem

A user reported that htop uses about 0.2% CPU and rtop about 1.5%. On the reference Mac (Apple M5, 10 cores, about 600 processes, 1 s interval, 120×40), rtop used 2.85% and htop 0.47%. 85% of rtop's cost was kernel time, so system calls were the cause, not Rust code.

## Result

rtop now costs about as much CPU as htop. The host's load moves both programs, so compare them in the same window:

| Window | rtop | htop | rtop ÷ htop |
|---|---|---|---|
| Before (0.2.0) | 2.85% | 0.47% | 6.1× |
| After wave A | 1.05% | 0.44% | 2.4× |
| After wave B | 0.33% | 0.26% | 1.3× |
| Final branch | 0.58% | 0.55% | 1.06× |
| A/B check, final binary | 0.62%-0.73% | 0.46% | 1.4×-1.6× |

At 250 ms, rtop went from 9.1% to 1.1%-2.1%. A fast tick went from 16.3 ms to about 2-4 ms.

## What we found

- **Processes on macOS.** `sysinfo` 0.39.6 copies the argument area of every process on every refresh (two `KERN_PROCARGS2` calls), with no way to turn it off. That was about 3,650 system calls on each tick. For other users' processes, most of these fail with `EPERM` on every tick.
- **Processes on Linux.** `sysinfo` reads `stat`, `statm` and `io` for every thread, and `status` for every PID. `ProcessRefreshKind::nothing()` already turns tasks on.
- **The sampler loop.** It woke 20 times a second; on macOS each `thread::sleep` costs a mach call.
- **Slow work.** Sensor re-lists, disk space (CacheDelete), and disk re-lists ran every 5 s for every disk.
- **After wave A,** a probe of thread CPU per collector found the rest: IOKit matching for disk I/O (2.2 ms per tick), `getifaddrs` once per network interface (1.8 ms), and space reads of disks rtop does not show.
- **`sample` counts waiting time.** Sensors looked costly in the profile, but most of their wall time is waiting on `hidd`. Thread CPU time (`CLOCK_THREAD_CPUTIME_ID`) gives the real cost.

## What landed

| Area | Change |
|---|---|
| Tools | `scripts/cpubench.py`; the ignored tests `sample_cost`, `collector_cost` and `draw_cost`. |
| Sampler | A control channel (`Kill`, `Wake`, `Pause`): it blocks until the next tick and does no sampling while paused. |
| Cadence | Each slow collector has its own period, and some skip work when their panel is not drawn (sensors, disk space). |
| Processes | Native collectors on macOS (`libproc`) and Linux (`/proc`). Windows and FreeBSD keep `sysinfo`. |
| Kill identity | `p_uniqueid` on macOS, the start time in clock ticks on Linux, the start time in seconds elsewhere. |
| macOS I/O and network | A cached IOKit driver entry for each whole disk; one `NET_RT_IFLIST2` call plus `IFMIB_IFDATA`. |
| UI | Table rows only for the visible window: 2.9 ms to 0.43 ms per frame with 2,000 processes. |
| Review fixes | 8 process findings and 6 storage and sampler findings (see the commits from `9450548` to `ba38d42`). |
| Docs | `architecture.md`, `usage.md`, `platforms.md`, `CONTRIBUTING.md`, `SECURITY.md`, `README.md`, `src/README.md`, `CHANGELOG.md`. |

## Testing done

- **Benchmarks:** `scripts/cpubench.py`, 60 s runs against htop at the same interval, at each stage, and a final A/B run of the wave B and final binaries.
- **Profiles and probes:** `sample` before and after wave A; thread CPU time per collector.
- **Checks:** fmt; clippy with `-D warnings` for the host and five targets (`x86_64-apple-darwin`, both Linux musl targets, Windows, FreeBSD); 385 tests on macOS, run twice. The Linux tests also ran in Docker (`rust:1.99-alpine`): 349 passed.
- **Live tests:** the whole macOS process list against `sysinfo`; kills of `sleep` children through the identity check; the macOS environment leak with an empty `argv[0]`; that `exec` keeps `p_uniqueid`; the macOS network and disk counters against `sysinfo`.
- **Review:** a critic review of the plan, two code reviews (process and kill; sampler, storage, network and UI), and a verifier pass.
- **TUI run:** a scripted run in a pseudo-terminal, checked through a terminal emulator: TERM and KILL, PID 1 refused, pause at 0 ms CPU, `+` and `-`, the view keys, and a clean quit.

## Not tested

- Linux CPU against htop, and a live Linux TUI run.
- An Intel Mac, Rosetta, and running as root.
- Swapping a USB disk for another with the same name while rtop runs.

## Things worth knowing

- **Environment in the command column (fixed on this branch).** `sysinfo`'s macOS parser skips empty arguments without counting them, so it reads past `argc` into the environment. The macOS kernel, Apple's `ps` and psutil skip NULs the same way. rtop now skips only the measured padding after the exec path, and stops at `argc`.
- **Kill identity on macOS.** The old check compared start time 0 with 0 for processes rtop cannot read, including setuid ones. `p_uniqueid` works for every process.
- **Binary size.** On arm64 macOS the file grows in 16 KiB pages. Use a linker map for the real code growth.
- **CPU count.** `available_parallelism()` and musl's `sysconf` follow rtop's own affinity. Linux reads `/sys/devices/system/cpu/online`.
- **Docker.** Fixer F1 started Docker Desktop to run the Linux tests, against the "macOS only" decision; the brief did not repeat it. Docker Desktop is stopped again, and nothing was removed.

## Known issues

These are listed in `PERF-PLAN.md` section 11: the disk I/O read order on re-list ticks (as before the round), the name after `exec` (as with `sysinfo`), `n/a` after two failed disk counter reads, and `App::visible_rows` building a row for each process on each frame.

## Decisions for Chad

- Push the branch and let CI run the Linux tests before a merge to `main`.
- A release with the macOS command-line fix, and whether it needs an advisory.
- An upstream `sysinfo` issue about the argument parser and the per-refresh `KERN_PROCARGS2` reads.
- Whether `PERF-PLAN.md` and this file move to `docs/plans/performance/`.

## Housekeeping

- The agent worktrees and their branches are removed; every commit is on `perf/cpu-round-1`.
- Benchmark outputs, profiles and the TUI check script are in the session scratchpad, not in the repo.

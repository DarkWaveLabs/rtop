# Platforms

Which features work on which OS, and how much of each has been tested. For ZFS detail, see [ZFS ARC](zfs.md).

rtop is one code base. Each OS has its own readers behind `cfg` attributes, so a build for one OS does not contain the code for the others.

| Feature | Linux | macOS | Windows | FreeBSD |
|---|:-:|:-:|:-:|:-:|
| CPU, memory, swap, history, processes | ✅ | ✅ | ✅ | ✅ |
| Load average | ✅ | ✅ | ➖ ¹ | ✅ |
| Network rates, total and per interface | ✅ | ✅ | ✅ | ✅ |
| Storage: filesystems and space | ✅ | ✅ | ✅ | ✅ |
| Storage: disk I/O rates | ✅ | ✅ | ✅ | ✅ |
| **ZFS ARC panel** | ✅ | ✅ | ❌ | ✅ |
| ARC counted as free memory | ✅ | ❌ | ❌ | ✅ ² |
| Process tree, sort, filter, command line | ✅ | ✅ | ✅ | ✅ |
| Process state column (S) | ✅ | ✅ | ❌ ³ | ✅ |
| Process threads column (THR) | ✅ | ✅ | ❌ ³ | ➖ ⁴ |
| Process disk I/O (READ/s, WRITE/s) | ✅ | ✅ | ✅ | ➖ ⁵ |
| Kill with `TERM` | ✅ | ✅ | ❌ | ✅ |
| Kill with `KILL` | ✅ | ✅ | ✅ | ✅ |
| Temperatures | ✅ | ✅ | ➖ ⁶ | ➖ ⁶ |
| Battery | ✅ | ✅ | ✅ | ❌ |
| Config file | ✅ | ✅ | ✅ | ✅ |
| Release binary | ✅ | ✅ | ✅ | ❌ build from source |

✅ implemented · ➖ partial or limited · ❌ not available. See [What is tested](#what-is-tested).

1. `sysinfo` documents the load average as not working on Windows.
2. On FreeBSD, `sysinfo` subtracts the ARC from used memory. On Linux, rtop does it. On macOS, rtop does not adjust the memory gauge.
3. Windows reports every process as running and gives no thread counts, so rtop hides both columns.
4. FreeBSD shows `-`. `sysinfo` gives no thread count there.
5. On FreeBSD, `sysinfo` fills these values from the kernel's block counters, not byte counts. Treat them as a rough guide.
6. rtop shows what the `sysinfo` crate finds. On Windows it reads one thermal-zone value. On FreeBSD it reads `dev.cpu.<n>.temperature`, which needs a CPU temperature driver. The panel is hidden when the host reports nothing.

## What is tested

| Platform | Status |
|---|---|
| Linux | Unit tests, `clippy` and `rustfmt` run in CI on every pull request. The screenshots come from a Linux container (Alpine, arm64). |
| macOS | Developed on macOS. The release workflow builds the universal binary. CI does not run macOS tests. |
| Windows | The release workflow builds the binary and runs `--help`. `clippy` passes for `x86_64-pc-windows-msvc`. **Not yet run-tested** on a live system. |
| FreeBSD | `clippy` passes for `x86_64-unknown-freebsd`. It **builds, but it is not yet run-tested**. No release binary. |

The ZFS ARC reader has only been tested with synthetic fixtures (`testdata/`), not on a real ZFS pool. If you run ZFS and the numbers look wrong, please open an issue and include the output of `cat /proc/spl/kstat/zfs/arcstats` (Linux) or `sysctl kstat.zfs.misc.arcstats` (FreeBSD and macOS).

# Security policy

## Supported versions

Security fixes go into the `main` branch and the next release. Only the latest release gets fixes. We do not back-port fixes to older releases.

| Version | Supported |
|---|---|
| Latest release | Yes |
| `main` | Yes |
| Older releases | No |

## Report a vulnerability

Do not open a public issue for a security problem.

Use GitHub private vulnerability reporting:

**<https://github.com/DarkWaveLabs/rtop/security/advisories/new>**

Only the maintainers can see the report. We discuss the fix with you in the advisory, and we credit you when we publish it, unless you ask us not to.

### What to include

- The rtop version (`rtop --version`) or the commit hash.
- The operating system and version, and the CPU architecture.
- How you ran rtop: as a normal user, with `sudo`, or as root.
- The steps to reproduce the problem. Include a config file or an input file if one is necessary.
- What happens, and what you expected to happen.
- The impact as you see it, for example "a local user can make rtop signal a process that it should not".
- A proof of concept or a suggested fix, if you have one.

### Response targets

These are best-effort targets. rtop is maintained by a small team.

| Step | Target |
|---|---|
| Acknowledge the report | Within 5 working days |
| First assessment (valid or not, and severity) | Within 10 working days |
| Fix or mitigation | Depends on severity. We tell you the plan in the advisory. |

We ask you to keep the problem private until a fixed release is available, or until 90 days after the report, whichever is first.

## Scope

rtop is a local terminal program. It opens no network ports and sends no data off the machine. These areas are in scope:

- **Process signals.** The `x` (or `F9`) key opens a kill prompt. In the prompt, `y` sends `SIGTERM` and `f` sends `SIGKILL`. On Windows, `f` calls `TerminateProcess`, and `y` is not supported. rtop checks the PID and the identity of the target again before it sends the signal (the unique process ID on macOS, the start time on other OSes), and it refuses PID 0, PID 1, its own PID and (on Windows) the System process, PID 4. A way to make rtop signal a different process from the one on the confirm prompt is in scope.
- **Running with elevated rights.** Some people run rtop with `sudo` or as root to see every process. Any way that a less-privileged user can use a root rtop to signal processes, read files or run code is in scope.
- **Config file parsing.** rtop reads `config.toml` from `$XDG_CONFIG_HOME/rtop/`, `~/.config/rtop/` or `%APPDATA%\rtop\`, or the file given with `--config`. The parser is hand-written and must never panic or hang. It accepts at most 64 KiB. A crash, hang or memory problem from a crafted config file is in scope.
- **FFI code.** rtop calls OS APIs through `unsafe` code: `sysctlbyname` on macOS and FreeBSD; `sysconf` and `kill` on Linux; `proc_pidinfo`, `proc_listallpids`, `proc_pid_rusage`, `proc_pidpath`, `sysctl` (`KERN_PROCARGS2`, `NET_RT_IFLIST2`, `IFMIB_IFDATA`), `statfs`, `kill`, IOKit registry and power sources, and Core Foundation on macOS; and `OpenProcess`, `GetProcessTimes`, `TerminateProcess` and `GetSystemPowerStatus` on Windows. Memory-safety problems in this code are in scope.
- **Process data on screen.** rtop shows other processes' command lines. A way to make it show another process's environment, or other data that the OS does not show in `ps`, is in scope.
- **Untrusted text on screen.** Process names, command lines, user names, interface names and mount points come from other users. A way to inject terminal escape sequences through them is in scope.
- **Release binaries.** A problem in how the release workflow builds or publishes binaries and `SHA256SUMS`.

### The `RTOP_ZFS_ARCSTATS` variable

A debug build (`cargo build` without `--release`) reads ZFS ARC counters from the file that `RTOP_ZFS_ARCSTATS` names, in place of the OS source. This is for tests. Release builds do not include this code. A problem that needs this variable in a release build is in scope. A problem in a debug build only is not.

## Out of scope

- Problems that need an attacker who already controls the account that runs rtop, its environment variables or its config file.
- Signals that you send to your own processes, or that root sends to any process. This is how `kill` works.
- Information that rtop shows and that the OS already gives to the same user, for example other users' process names or command lines through `ps`.
- Denial of service from a very large number of processes, disks or interfaces, unless it is far out of proportion to the input.
- Debug builds, and builds from a fork or with changed code.
- Vulnerabilities in dependencies that rtop does not reach. Report those to the dependency. Dependabot keeps the lock file current.
- Bugs in the terminal emulator.

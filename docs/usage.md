# Usage

How to run rtop, which keys it has, what each panel shows, how the process table works, and the rules that keep a kill safe. For the config file, see [Configuration](configuration.md). For the ZFS ARC panel, see [ZFS ARC](zfs.md).

## Command line

```sh
rtop                      # start with the defaults
rtop -i 500               # refresh every 500 ms
rtop --tree               # start in the tree view
rtop --config ./my.toml   # read this config file
rtop --no-config          # ignore the config file
```

rtop uses 24-bit colour, rounded box characters and Braille glyphs. Use a terminal that supports them.

| Option | Meaning |
|---|---|
| `-i`, `--interval <ms>` | Refresh interval, 250 to 8000 ms. Default 1000. A value outside the range is moved to the nearest limit. |
| `-c`, `--config <path>` | Read this config file, not the default one. A missing file is an error. |
| `--no-config` | Read no config file. You cannot use it with `--config`. |
| `-t`, `--tree` | Start in the tree view. |
| `-h`, `--help` | Show the help text, with the key list. |
| `-V`, `--version` | Show the version. |

The refresh interval sets how often rtop samples. Slow data refreshes less often: the disk list and space, sensors, the battery, the user list and command lines refresh every 5 seconds, and so do macOS thread counts.

## Keys

This table follows the help overlay and `--help`. Both read one key table in the code (`KEYMAP` in `src/help.rs`).

**General**

| Key | Action |
|---|---|
| `q`, `Ctrl-C` | Quit |
| `?`, `F1` | Show the help |
| `space` | Pause or resume |
| `+` `=` / `-` | Interval ×2 / ×0.5 (250 to 8000 ms) |
| `Esc` | Clear the filter or the footer message |

**Processes**

| Key | Action |
|---|---|
| `↑` `↓` `j` `k` | Move the selection |
| `PgUp` `PgDn` | Move by 10 rows |
| `Home` `g` / `End` `G` | First / last row |
| `s` | Next sort column |
| `r` | Reverse the sort |
| `t` | Tree view on or off |
| `c` | Show the name or the command line |
| `x`, `F9` | Kill the selected process |

**Panels**

| Key | Action |
|---|---|
| `1` | Show or hide cpu |
| `2` | Show or hide history |
| `3` | Show or hide memory |
| `4` | Show or hide network |
| `5` | Show or hide storage |
| `6` | Show or hide zfs |
| `7` | Show or hide sensors |
| `8` | Show or hide processes |
| `0` | Show all panels |
| `n` | Network: total or per interface |

A panel that has no data on your host takes no space. Pressing its key shows `<panel>: not available here`.

**Filter, help and kill prompt**

| Key | Action |
|---|---|
| `/` | Start the filter |
| `Backspace` | Filter: edit |
| `Enter` / `Esc` | Filter: keep / clear |
| `Esc` `?` `q` `F1` | Close the help |
| `y` | Kill: send `TERM` |
| `f` | Kill: send `KILL` |
| `n`, `Esc` | Kill: cancel |

## What each panel shows

The ZFS ARC panel has its own page: [ZFS ARC](zfs.md).

### CPU and memory

- A bar for each CPU core. When the panel is short, it adds columns.
- A history graph of CPU and memory use for the last 120 samples, drawn with Braille glyphs.
- RAM and swap gauges, the process count and the core count.
- A header line with the host name, OS, CPU, uptime and load average.

### Storage

- Used and total space for each filesystem, with a meter. Pseudo filesystems and snap mounts are hidden.
- A ZFS child dataset folds into its pool when the pool root is also listed. The `/` mount is always kept.
- Read and write rates for each disk. Volumes on one disk count once, for example the APFS volumes of one container on macOS.
- Volumes with no disk counter, such as ZFS datasets, `tmpfs` and network filesystems, show `n/a`. Mount points that are files, such as the `/etc/hosts` that Docker bind-mounts, are hidden.

### Sensors

- Temperatures from the hardware sensors, hottest first, in °C or °F. The unit is set with `temp_unit` in the [config file](configuration.md); no key changes it. The colour shows how close each reading is to its critical value.
- The battery level and state: charging, discharging, full, not charging or unknown.
- The panel appears only when the host reports a sensor.

### Network

- Total receive and transmit rates, with sparklines.
- A view for each interface (`n`): receive and transmit rates and the totals since boot. Loopback interfaces and interfaces with no traffic since boot are hidden.

### Processes

- Columns for PID, user, state, threads, CPU, memory, disk read and disk write rates, and name.
- Sort by CPU, memory, I/O, PID or name. Reverse the sort with `r`.
- A filter by name or exact PID.
- A tree view with branch lines. Siblings follow the sort order.
- The full command line, in place of the name (`c`).
- Kill a process with a confirm prompt. See [Kill safety](#kill-safety).

### Layout and config

- An adaptive layout. Panels collapse in a fixed order when the terminal is small, and the footer shows how many are hidden. It works at 80×24.
- Keys `1` to `8` show or hide each panel. `0` shows them all.
- A help overlay (`?`) that reads the same key table as `--help`.
- An optional [config file](configuration.md) and a few command-line flags.
- No telemetry. rtop opens no network ports and sends no data off the machine.

## The process table

The table adds columns as the terminal gets wider.

| Terminal width | Columns |
|---|---|
| 99 or more | PID, USER, S (state), THR (threads), CPU%, MEM, READ/s, WRITE/s, NAME |
| 63 to 98 | The same, without READ/s and WRITE/s |
| Under 63 | PID, CPU%, MEM, NAME |

On Windows, the S and THR columns are hidden. On FreeBSD, THR shows `-`.

Without root, the OS hides some data about other users' processes. Their READ/s and WRITE/s show `0`, not `-`, because rtop cannot tell a hidden counter from an idle one. On macOS, their command lines stay `[name]`.

The `s` key goes to the next sort column in this order: CPU, memory, I/O, PID, name. CPU, memory and I/O sort from high to low. PID and name sort from low to high. The `r` key reverses the direction. A command line is read on a slow tick, so a new process shows as `[name]` for up to 5 seconds when you show command lines.

The filter (`/`) matches part of a process name, ignoring case, or an exact PID. When you show command lines (`c`), it also matches the command line.

## Kill safety

Select a process and press `x` or `F9`. rtop then asks before it sends a signal.

- **Confirm prompt.** The prompt names the PID and the process. `y` sends `TERM`. `f` sends `KILL`. `n` or `Esc` cancels.
- **The target is fixed.** rtop takes the target from the row that the last frame highlighted, and the prompt keeps it. A re-sort cannot change which process gets the signal.
- **Protected PIDs.** rtop refuses PID 0, PID 1, its own PID and (on Windows) the System process, PID 4. It refuses before it opens the prompt, and again when it sends the signal.
- **PID reuse.** Before it signals, the sampler reads the process again and compares the start time. If the PID now belongs to a new process, rtop does not send the signal and says so.
- **Paste cannot trigger it.** rtop turns on bracketed paste, so pasted text arrives as one event, not as key presses. A paste cancels the prompt. Any key other than `y`, `f`, `n` and `Esc` also cancels it, and `y` or `f` pressed before a frame has shown the prompt cancels it.
- **Windows.** `f` opens the process, checks its creation time and calls `TerminateProcess` on that same handle. `y` (`TERM`) is not supported on Windows, and rtop tells you so.
- **Permissions.** If the OS refuses the signal, the footer says `permission denied or process gone`. rtop does not escalate.

The result of every kill shows in the footer.

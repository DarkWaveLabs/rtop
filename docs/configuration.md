# Configuration

rtop works with no config file. Use one to change the defaults. For the command-line flags, see [Usage](usage.md#command-line).

A config file is optional. rtop reads it once, before it starts the display.

## Path

| OS | Default path |
|---|---|
| Linux, macOS, FreeBSD | `$XDG_CONFIG_HOME/rtop/config.toml` when that variable is set to an absolute path. Otherwise `~/.config/rtop/config.toml`. |
| Windows | `%APPDATA%\rtop\config.toml` |

A missing file at the default path is fine. Use `--config <path>` to read another file (a missing file is then an error), or `--no-config` to read none.

## Precedence

Command line, then config file, then built-in default. The flags that overlap are `-i` and `-t`. The `--tree` flag can only turn the tree on.

## Keys

| Key | Type | Default | Values | Example |
|---|---|---|---|---|
| `interval_ms` | integer | `1000` | 250 to 8000. A value outside the range is moved to the nearest limit. | `interval_ms = 500` |
| `sort` | string | `"cpu"` | `"cpu"`, `"mem"`, `"io"`, `"pid"`, `"name"` | `sort = "mem"` |
| `reverse` | boolean | `false` | `true`, `false` | `reverse = true` |
| `tree` | boolean | `false` | `true`, `false` | `tree = true` |
| `command` | boolean | `false` | Show the command line, as the `c` key does. | `command = true` |
| `panels` | array of strings | all panels | `"cpu"`, `"history"`, `"memory"`, `"network"`, `"storage"`, `"zfs"`, `"sensors"`, `"processes"` | `panels = ["cpu", "memory", "processes"]` |
| `temp_unit` | string | `"c"` | `"c"`, `"f"` | `temp_unit = "f"` |
| `network` | string | `"total"` | `"total"`, `"interfaces"` | `network = "interfaces"` |

## Example

Example `config.toml` (every key, with its default value, except where the comment says otherwise):

```toml
# ~/.config/rtop/config.toml

interval_ms = 1000          # 250 to 8000
sort        = "cpu"         # "cpu", "mem", "io", "pid" or "name"
reverse     = false
tree        = false
command     = false         # show command lines, not names

# Panels to show at start. Leave this key out to show all of them.
panels = ["cpu", "history", "memory", "network", "storage", "zfs", "sensors", "processes"]

temp_unit = "c"             # "c" or "f"
network   = "total"         # "total" or "interfaces"
```

## Format

The parser reads a flat subset of TOML: `key = value` lines and `#` comments.

- Values are integers, `true` or `false`, quoted strings, and one-line arrays of quoted strings.
- A string supports two escapes only: `\"` and `\\`.
- Tables (`[section]`) are not supported.
- The file must be UTF-8 and at most 64 KiB.

## Errors

- A syntax error, a duplicate key, a wrong type or an unknown value stops rtop before the display starts. rtop prints the path and the line to stderr and exits with code 1. For example: ``rtop: config <path>: line 1: unknown value `x` for `sort`; expected one of: cpu, mem, io, pid, name``.
- An unknown key is not an error. rtop ignores it and shows a warning in the footer (``line 3: unknown key `colour`, ignored``) until you press a key.
- A panel name that is not valid in `panels` is an error. A valid panel that your host cannot show, such as `zfs` without ZFS, takes no space.

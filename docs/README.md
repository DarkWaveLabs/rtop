# docs

Project documents. The pages at the top of this folder are for people who use rtop. The folders group the design documents by topic, and each file name in a folder starts with its date (`YYYY-MM-DD-<name>.md`).

## Pages

| Page | Contents |
|---|---|
| [`usage.md`](usage.md) | Command-line options, every key, what each panel shows, the process table and kill safety. |
| [`configuration.md`](configuration.md) | The config file: path for each OS, every key, precedence, errors and a full example. |
| [`zfs.md`](zfs.md) | The ZFS ARC panel: what it shows, how the hit ratio works, the ARC as free memory, data sources and the `RTOP_ZFS_ARCSTATS` debug fixture. |
| [`install.md`](install.md) | Release binaries and how to check them, and how to build from source. |
| [`platforms.md`](platforms.md) | The feature matrix for Linux, macOS, Windows and FreeBSD, and what is tested. |
| [`architecture.md`](architecture.md) | Threads, the kill flow, the layout planner, design principles and the file map. |

## Folders

| Folder | Contents |
|---|---|
| `designs/` | Designs that the code implements: decisions, interfaces, invariants and risks. The newest file in a topic is the current design. |
| `plans/` | Implementation plans for the designs, under the same topic name: tasks, file ownership, checks and acceptance. |
| `assets/` | Images for the README and the docs, for example screenshots. |

Current topic: `features/` holds the feature round 1 design and plan (ZFS ARC, storage, sensors, process tree and kill, per-interface network, layout, help and config). The code in `src/` is the final truth where it differs from a document.

The [README](../README.md) is the short overview. For the changes in each version, see the [changelog](../CHANGELOG.md).

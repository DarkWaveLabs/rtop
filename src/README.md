# src

Module map. Data modules collect and compute with no drawing code; each has a matching file in `ui/` that draws it.

| File | Contents |
|---|---|
| `main.rs` | CLI parsing, `--help` text, config loading, thread setup and the event loop. |
| `metrics.rs` | The sampler thread: owns the `sysinfo` handles and the collectors, and sends one `Snapshot` per tick. |
| `app.rs` | Application state, history ring buffer, sort and filter, and key handling. No terminal code. |
| `ui.rs` | Panel drawing and the overall frame; child modules in `ui/` draw each feature panel. |
| `layout.rs` | Pure layout planner: which panels fit, where they go, and which collapse. |
| `panel.rs` | Panel identities and the set of visible panels, shared by layout, config and app. |
| `help.rs` | The one key table (`KEYMAP`), read by the help overlay and `--help`. |
| `config.rs` | Config file: a flat TOML subset parser and the default path rules. |
| `process.rs` | Process data, sorting, filtering, the tree view and kill. |
| `storage.rs` | Filesystem usage and disk I/O rates. |
| `zfs.rs` | ZFS ARC statistics: kstat parser, hit-ratio window and a reader for each OS. |
| `sensors.rs` | Temperatures, and battery state for each OS. |
| `net.rs` | Per-interface network rates and totals. |
| `format.rs` | Formatters for bytes, rates and durations, and the bar meter. |
| `order.rs` | One shared stable sort, to keep the binary small. |
| `ui/` | `zfs.rs`, `storage.rs`, `sensors.rs`, `process.rs`, `net.rs` and `help.rs`: one drawing file for each feature panel and the help overlay. |

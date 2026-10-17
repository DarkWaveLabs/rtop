# rtop feature round 1: implementation plan

Date: 2026-10-16. Design: `docs/designs/features/2026-10-16-rtop-feature-round-1.md` (called "the design" below). Section numbers such as "§6.1" point to the design.

This document uses ASD-STE100 Simplified Technical English and en-ZA spelling.

## Overview

| Stage | Tasks | Parallel | Result |
|---|---|---|---|
| 0 | S0 scaffold | No | Stub modules, `panel.rs`, dependencies, baseline size. rtop runs as before. |
| 1 | T1 to T8 | Yes, all eight | Each module complete and tested. Not connected. rtop runs as before. |
| 2 | S2 integration | No | All features connected. |
| 3 | S3 verification | No | Checks on all targets, size, manual runs. |

rtop compiles, passes its tests and runs at the end of each stage.

## Working rules for all tasks

1. Use the `export PATH="/opt/homebrew/opt/rustup/bin:$PATH"` line before each `cargo` command.
2. Each Stage 1 task works in its own git worktree and branch, made from the S0 commit. Name the branch `r1/<task>`, for example `r1/t1-zfs`. Do not work in the main worktree: another task's unfinished file would break your build. Read the design from your worktree (`docs/designs/features/…`); S0 commits it.
2a. Per-target code: put exactly the `cfg` attributes of design §5.5 on your items. The S0 `#[allow(dead_code)]` hides dead code in your build, so the clippy commands in rule 5 do not find a missing `cfg`. Check by hand that each item in §5.5 has its attribute.
3. Change only the files that your task owns. Do not change `Cargo.toml`, `Cargo.lock`, `src/main.rs`, `src/metrics.rs`, `src/app.rs`, `src/ui.rs`, `src/format.rs`, `src/panel.rs`, `testdata/` or `docs/` in Stage 1. If you need a change there, stop and report it.
4. Copy the interfaces from the design exactly: names, fields, field order, signatures. Private helpers are your choice.
5. Before you report done, run these commands in your worktree. All must pass:

   ```sh
   cargo fmt --check
   cargo clippy --all-targets --locked -- -D warnings
   cargo test --locked
   cargo clippy --all-targets --locked --target x86_64-unknown-linux-musl -- -D warnings
   cargo clippy --all-targets --locked --target x86_64-pc-windows-msvc -- -D warnings
   cargo clippy --all-targets --locked --target x86_64-unknown-freebsd -- -D warnings
   ```

6. Fixtures are `const` values in the test module, or `include_str!` of a file that the task owns.
7. Use `unsafe` only for FFI. Put a `// SAFETY:` comment on each `unsafe` block.
8. Commit subject: imperative, under 72 characters. Do not add attribution lines.

## Stage 0: scaffold (task S0)

**Owner:** one agent, or the lead. **Owns:** `Cargo.toml`, `Cargo.lock`, `src/panel.rs`, `src/main.rs` (module lines only), `src/ui.rs` (module lines and test helper only), the stub files, `testdata/arcstats-2.2.txt`, `testdata/arcstats-2.2-next.txt`, and the first commit of `docs/designs/features/` and `docs/plans/features/`.

`docs/` is untracked before S0 (`git status` shows `?? docs/`). A worktree made from the S0 commit has only tracked files, so S0 must commit the design, this plan and the fixtures, or the Stage 1 agents cannot read them.

Steps:

1. Ask the maintainer to approve the cross targets. They are user-level `rustup` components, not Homebrew packages. The lead decides if this needs approval. Then run:

   ```sh
   rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-pc-windows-msvc x86_64-unknown-freebsd
   ```

2. Record the baseline release size on the Mac host:

   ```sh
   cargo build --release --locked && stat -f %z target/release/rtop
   ```

   Write the number in the S0 commit body.
3. Add the dependency lines from design §4 to `Cargo.toml`. Run `cargo check`. Then confirm that no new crate came in:

   ```sh
   git diff Cargo.lock    # only the dependency list of the `rtop` package may change
   ```

4. Write `src/panel.rs` exactly as in design §5.1.
5. Add the module lines to `src/main.rs` and `src/ui.rs` exactly as in design §5.2.
6. Add `render_to_string` to `src/ui.rs` exactly as in design §5.3. Change the existing test helper `render` to call it.
7. Make each stub file with one line. Files: `src/config.rs`, `src/help.rs`, `src/layout.rs`, `src/net.rs`, `src/process.rs`, `src/sensors.rs`, `src/storage.rs`, `src/zfs.rs`, `src/ui/help.rs`, `src/ui/net.rs`, `src/ui/process.rs`, `src/ui/sensors.rs`, `src/ui/storage.rs`, `src/ui/zfs.rs`. Example: `//! ZFS ARC statistics.`
8. Write `testdata/arcstats-2.2.txt` exactly as design Appendix A.1, and `testdata/arcstats-2.2-next.txt` as A.1 with the eight rows in A.2 changed. Check: `diff testdata/arcstats-2.2.txt testdata/arcstats-2.2-next.txt | grep -c '^>'` prints `8`.
9. Run the commands from rule 5, plus `cargo check --locked --target aarch64-unknown-linux-musl` and `cargo check --locked --target x86_64-apple-darwin`.
10. Commit the code, `testdata/` and both `docs/` files: `Scaffold modules for feature round 1`. Do not commit `docs/sdd/`.

**Acceptance:**

- All commands in step 9 pass.
- `cargo test` runs the two new `panel` tests.
- `git diff Cargo.lock` adds no `[[package]]` entry.
- `git ls-files docs testdata` lists the design, the plan and both fixtures.
- `cargo run --release` shows the same screen as before.

**Rollback:** revert the S0 commit.

## Stage 1: parallel module tasks

All eight tasks start from the S0 commit. No task depends on another task. Each task lists the files it owns. No two tasks own the same file.

### T1: ZFS ARC

**Owns:** `src/zfs.rs`, `src/ui/zfs.rs`. **Reads:** `testdata/arcstats-2.2.txt`, `testdata/arcstats-2.2-next.txt` (S0 wrote them from design Appendix A; do not change them).

**Interface:** design §5.5 (cfg table), §6.1, §6.2 and Appendix A.

**Work:**

1. Write `parse_kstat`, `from_pairs`, `derive`, `arc_adjusted_used`, `read_raw` for each target, `sysctl_u64` (with `std::ptr::null_mut()`), `window_base` and `ZfsCollector`.
2. Write `draw_zfs` and `want_height`.

**Tests (required):**

- `parse_kstat` on the 2.2 fixture: `size` 9663676416, `c` 10737418240, `c_min` 1073741824, `c_max` 17179869184, `iohits` `Some(37116)`, `data_size` `Some(6552476672)`, `metadata_size` `Some(2147483648)`, `arc_meta_used` `None`, `l2_size` 214748364800.
- The four `derive` rows in design Appendix A give exactly the values there (A.1 = `a1`, A.2 = `a2`).
- `parse_kstat` on this 2.1-style fixture (a `const` in the test; 2.1 has no `iohits` rows):

  ```text
  13 1 0x01 23 6624 4625134567890123 1049387654321098
  name                            type data
  hits                            4    1000000
  misses                          4    250000
  demand_data_hits                4    600000
  demand_data_misses              4    100000
  demand_metadata_hits            4    300000
  demand_metadata_misses          4    50000
  p                               4    2147483648
  c                               4    4294967296
  c_min                           4    536870912
  c_max                           4    8589934592
  size                            4    4294967296
  compressed_size                 4    2147483648
  uncompressed_size               4    3221225472
  data_size                       4    3006477107
  metadata_size                   4    1073741824
  mru_size                        4    1610612736
  mfu_size                        4    2147483648
  l2_hits                         4    0
  l2_misses                       4    0
  l2_size                         4    0
  l2_asize                        4    0
  arc_meta_used                   4    1342177280
  arc_meta_limit                  4    3221225472
  ```

  Expect `iohits` `None`, `data_size` `Some(3006477107)`. `derive(None, &raw)`: `data` 3006477107, `metadata` 1073741824 (`metadata_size` wins over `arc_meta_used`), `compress_ratio` `Some(1.5)`, `l2` `None`, `iohit_pct` `None`, `hit_pct` `Some(80.0)` with `hit_since_boot` true, `demand_data_pct` `Some(600000/700000 × 100)`.
- Fallback (synthetic, through `from_pairs`): `[("size", 1000), ("c", 1000), ("c_max", 2000), ("arc_meta_used", 300)]` gives `metadata` 300 and `data` 700. The same without `arc_meta_used` gives `metadata` 0 and `data` 1000.
- Window with no demand reads: `a1` and a copy with only `hits` +10 gives `hit_pct` 100.0, `hit_since_boot` false, `demand_data_pct` `None`.
- `parse_kstat` returns `None` for `""`, for two header lines only, and for a file with no `size` row. It skips a row such as `hits 4 notanumber`.
- `from_pairs` with only `size`, `c`, `c_max` returns `Some`.
- `FIELDS` agrees with `from_pairs`: feeding `FIELDS` with value `i + 1` for index `i` gives an `ArcRaw` where no field is 0 or `None`.
- `arc_adjusted_used(10_000, &ZfsArc { size: 6_000, min: 1_000, ..Default::default() })` gives 5 000. With `size` < `min`: `used` unchanged. With `used` < shrinkable: 0.
- `window_base`: with entries at base+0 s, +2 s, +4 s, +6 s and `now` = base+10 s, the base index is the entry at +4 s (newest at least 5 s old). With entries only at +8 s and +9 s, it is the oldest (+8 s). Empty ring: `None`.
- `ZfsCollector` counter reset: a second `collect` whose `hits` are lower than the base gives `hit_since_boot` true and clears the ring (test through a private helper that takes the raw value, so no file or sysctl is read).
- Render at 60×7 with `derive(Some(&a1), &a2)`: the screen has `zfs arc`, `9.0 GiB`, `90.0%`, `l2`, `2.42`, and no `boot`.
- Render at 60×7 with `derive(None, &a1)`: the screen has `boot hit 85.0%`.
- Render at 40×`want_height` (38 inner columns, the 80×24 width): the hit line shows `hit` and `io`, and has no part cut in the middle; `dm` and `dd` are gone.
- Render at `MIN_WIDTH`×`MIN_HEIGHT`: the screen has the size line and the hit line.
- Render at 60×`want_height(&arc)`: every line shows (the `l2` text is present).
- Render at 1×1, 10×3 and 200×2: no panic.
- `ZfsCollector` (debug builds): do not set environment variables in tests (`set_var` is `unsafe` in edition 2024, and rule 7 allows `unsafe` only for FFI). Test the private file reader with a path argument instead.

**Acceptance:** the tests pass. `want_height` is 6 without L2ARC and 7 with L2ARC. On a host without ZFS, `ZfsCollector::new().collect(true)` returns `None` and does not panic. Each item in design §5.5 for `zfs` has its `cfg`.

### T2: storage

**Owns:** `src/storage.rs`, `src/ui/storage.rs`.

**Interface:** design §5.5 (cfg table), §6.3 and §6.4.

**Work notes:** fast calls refresh each kept, non-`zfs` disk with `Disk::refresh_specifics`; only slow calls use `Disks::refresh_specifics` (design §6.3). On macOS, the slow call fills `groups` with `libc::statfs` and `apple_whole_disk`, inside one `unsafe` block with a `// SAFETY:` comment.

**Tests (required):**

- `keep_volume`: keeps `("/", "apfs", 1)`, `("/System/Volumes/Data", "apfs", 1)`, `("/home", "ext4", 1)`, `("/boot/efi", "vfat", 1)`. Drops `("/System/Volumes/VM", "apfs", 1)`, `("/System/Volumes/Preboot", "apfs", 1)`, `("/snap/core/1", "squashfs", 1)`, `("/dev", "devfs", 1)`, `("/x", "ext4", 0)`.
- `drop_zfs_children(&[("tank", "zfs", "/tank"), ("tank/home", "zfs", "/tank/home"), ("tank/home/alice", "zfs", "/tank/home/alice"), ("rpool/ROOT/ubuntu", "zfs", "/"), ("/dev/sda1", "ext4", "/boot")])` returns `[0, 3, 4]`. `rpool/ROOT/ubuntu` stays, because `rpool` is not in the list.
- FreeBSD layout: `drop_zfs_children(&[("zroot", "zfs", "/zroot"), ("zroot/ROOT/default", "zfs", "/"), ("zroot/usr/home", "zfs", "/usr/home")])` returns `[0, 1]`: "/" is never dropped.
- `dedupe_rates`: two volumes with `io_group` `sda1` and read 100 each, and one with `sdb1` and read 50, give read 150. macOS case: `/` and `/System/Volumes/Data` with `io_group` `disk3` and read 100 each give read 100. A volume with `io_known` false and read 999 adds nothing.
- `apple_whole_disk`: `"/dev/disk3s1s1"` → `Some("disk3")`, `"/dev/disk3s5"` → `Some("disk3")`, `"/dev/disk10s2"` → `Some("disk10")`, `"/dev/disk4"` → `Some("disk4")`, `"map auto_home"` → `None`, `"//user@host/share"` → `None`, `""` → `None`.
- `io_known`: a private pure helper `io_known(file_system: &str, total_read: u64, total_written: u64) -> bool` gives false for `("zfs", 5, 5)` and `("ext4", 0, 0)`, and true for `("ext4", 5, 0)`.
- First-sample rule: a private pure helper `rate(delta: u64, secs: f64, first: bool) -> u64` returns 0 when `first` is true. Test it.
- `seen` key: a private pure helper `fn first_samples(seen: &mut HashSet<(String, String)>, now: &[(String, String)]) -> Vec<bool>` returns, for each (device, mount) in `now`, true when it was not in `seen`, then sets `seen` to exactly `now`. Test: `sda1` at `/a` known, then `sda1` at `/b` added → `/b` is first, `/a` is not. Then a call without `/a`, then a call with `/a` again → `/a` is first.
- Sort: "/" first, then by mount point. Test with a private pure sort helper.
- Render at 60×`want_height` with three volumes: the screen has `storage 3/3`, `io`, each mount and `%`.
- Render at 80×`want_height` (inner width ≥ 70) with one `zfs` volume (`io_known` false) and one ext4 volume: the zfs row shows `n/a`, and the I/O line has `zfs n/a`.
- Render with only `zfs` volumes and `storage.io_known` false: the I/O line is `io n/a (zfs)`.
- Render at 40×5 with eight volumes: the title shows `storage 2/8` (the shown rows that fit) and no panic.
- Render at 1×1 and 200×2: no panic.

**Acceptance:** the tests pass. `apple_whole_disk` has its `cfg` from design §5.5. A manual check on the Mac host is not part of this task; S3 does it.

### T3: sensors

**Owns:** `src/sensors.rs`, and `src/sensors/*.rs` if the task splits the platform readers, `src/ui/sensors.rs`.

**Interface:** design §5.5 (cfg table), §6.5 and §6.6.

**Tests (required):**

- `tidy_temps`: drops `f32::NAN` and `f32::INFINITY`; sorts `[("b", 40.0), ("a", 70.0), ("c", 70.0)]` to `a, c, b`; rounds 41.26 to 41.3.
- `parse_linux_supply("Battery", "Charging\n", "87\n", None, None)` gives 87.0, `Charging`.
- `parse_linux_supply("Battery", "Not charging", "80", Some("1"), Some("System"))` gives `NotCharging`.
- `parse_linux_supply("Mains", …)` gives `None`. `scope` `Some("Device")` gives `None`. `present` `Some("0")` gives `None`. Capacity `"abc"` gives `None`.
- `parse_linux_supply` status strings: `"Full"` → `Full`, `"Discharging"` → `Discharging`, `"Unknown"` → `Unknown`, `"Not charging\r\n"` → `NotCharging`.
- `combine`: `[]` gives `None`. `[(80, Discharging), (60, Charging)]` gives 70.0, `Charging`. `[(100, Full), (100, Full)]` gives `Full`. `[(90, NotCharging), (100, Full)]` gives `NotCharging`. `[(50, Unknown)]` gives `Unknown`.
- `from_power_status`: `(0, 1, 55)` gives 55, `Discharging`. `(1, 8, 40)` gives `Charging`. `(1, 1, 100)` gives `Full`. `(1, 1, 90)` gives `NotCharging`. `(255, 1, 50)` gives `Unknown`. `(1, 128, 255)` gives `None`. `(0, 1, 255)` gives `None`.
- `from_macos(4000, 5000, false, "Battery Power")` gives 80.0, `Discharging`. `(5000, 5000, false, "AC Power")` gives `Full`. `(4500, 5000, false, "AC Power")` gives 90.0, `NotCharging`. `(4500, 5000, true, "AC Power")` gives `Charging`. `(4500, 5000, false, "UPS Power")` gives `Unknown`. `(10, 0, false, "AC Power")` gives `None`. `(6000, 5000, false, "AC Power")` gives 100.0, `Full`.
- `Sensors::is_empty` on `Sensors::default()` is true.
- Render at 40×`want_height` with a battery and three temps: the screen has `sensors`, `bat`, `%`, each label, `°C`. With `fahrenheit` true: `°F`, and 100 °C shows as `212`.
- Render at 1×1 and 200×2: no panic.

**Platform work:**

- macOS IOKit reader: time box it to the effort of the rest of the task. If it does not work, return `None` on macOS and write the problem in the task report. The `kIOPS*Key` constants are `&CStr`; make `CFString` keys as design §6.5 says.
- Windows reader: `windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS}`. It must pass `clippy --target x86_64-pc-windows-msvc`.

**Acceptance:** the tests pass on the Mac host. All three clippy cross-checks pass. Each item in design §5.5 for `sensors` has its attribute. The task report says whether the macOS battery reader works on the host (a MacBook shows a value; a desktop Mac returns `None`).

### T4: process power

**Owns:** `src/process.rs`, `src/ui/process.rs`.

**Interface:** design §6.7 and §6.8.

**Tests (required):**

- Port the behaviour of the current `app.rs` tests to `visible_rows`: CPU sort with PID ties, memory sort, PID sort, case-insensitive name sort, the filter (substring and exact PID).
- `SortKey::next` cycle: Cpu, Memory, Io, Pid, Name, Cpu. `from_name(name())` round-trips for all keys.
- `reverse` flips each key's direction.
- `match_cmd`: filter `"--port"` matches a process whose `cmd` has `--port` only when `match_cmd` is true.
- Tree: processes 1 (no parent), 10 (parent 1), 11 (parent 1), 20 (parent 10), 30 (parent 999, not present). Expected order with PID sort: 1, 10, 20, 11, 30. Expected prefixes: `""`, `"├─"`, `"│ └─"`, `"└─"`, `""`. Glyph shapes may differ, but they must be tested exactly as implemented.
- Tree with a loop: 5 (parent 6), 6 (parent 5). Each shows once. No hang.
- Tree with the filter: a matched child whose parent does not match becomes a root.
- `join_cmd`: `["/bin/sh", "-c", "echo hi"]` gives `/bin/sh -c echo hi`. A 10 000-char argument is cut to 4096 chars. An empty list gives `""`.
- `ProcState::code` for each variant.
- `ProcState::from_sysinfo` for each row of the design §6.7 table, including `UninterruptibleDiskSleep` → `DiskSleep` (`D`), `Tracing` → `Stop`, and `Unknown(7)` → `Unknown` (`?`).
- `linux_threads` (private, `fn linux_threads(tasks: Option<usize>) -> Option<u32>`, `cfg` from design §5.5): `Some(0)` → `Some(1)`, `Some(3)` → `Some(4)`, `None` → `None`.
- `refresh_kind(false)`: `cmd()` is `UpdateKind::Never`, `exe()` is `UpdateKind::Never`, `user()` is `OnlyIfNotSet`, and memory, cpu, disk usage and tasks are on. `refresh_kind(true)`: the same, but `cmd()` is `OnlyIfNotSet`.
- `check_target(0, 99)`, `(1, 99)`, `(99, 99)` give `Err(Protected)`. `(500, 99)` gives `Ok`.
- `KillReport::message` gives the exact strings in design §6.7.2 for each result.
- `send_signal` (only `#[cfg(unix)]`): spawn `std::process::Command::new("sleep").arg("30")`. Make a `System`, refresh with `refresh_kind(true)`. Send `Term` with the right `start_time`: result `Ok`. Wait for the child. Send again: result `Gone`. Send with a wrong `start_time` to a second live child: result `Gone`, and the child is still alive (`try_wait` gives `None`). Kill the child at the end.
- `ProcCollector`: a pure private helper for the first-sample rate rule (as in T2). Test it.
- `columns_for_width`: 120 → all 9; 80 → no Read and Write; 50 → Pid, Cpu, Memory, Name; `state_and_threads` false removes State and Threads.
- Render `draw_processes` at 110×10 with two rows: the header has `USER`, `THR`, `READ/s`, the title has `processes 2/2 · sort cpu`. With `show_cmd`: the header has `COMMAND` and the row shows the cmd text. With `tree` and a prefix: the prefix shows before the name.
- Render `draw_kill_prompt` at 80×24: the screen has the pid, the name, `y TERM`, `f KILL`, `n cancel`. At 10×3: no panic.
- Render both at 1×1: no panic.

**Acceptance:** the tests pass. `refresh_kind(slow)` matches design §6.7 (the getters test above). The collector calls `ProcState::from_sysinfo` on every OS; no `cfg` on it.

### T5: per-interface network

**Owns:** `src/net.rs`, `src/ui/net.rs`.

**Interface:** design §6.9.

**Tests (required):**

- `keep_iface`: drops `lo`, `lo0`, and `en5` with zero totals. Keeps `en0` with totals (1, 0).
- `totals` on three interfaces gives the sums.
- A private pure helper that builds `NetIface` from (name, received, transmitted, total_rx, total_tx, secs): rates use `secs` with a floor of 0.001. Test it. Test the sort by name.
- Render at 60×`want_height` with three interfaces: the screen has `network · 3 interfaces`, each name, `/s`.
- Render at 40×5 (inner width under 48): no total column, no panic.
- Render at 1×1: no panic.

**Acceptance:** the tests pass.

### T6: layout

**Owns:** `src/layout.rs`.

**Interface:** design §6.10.

**Tests (required):**

- Worked example 1 in design §6.10 (body 80×22) gives exactly: Cpu at row 0 with full width and height 4, Memory and Network height 5, Storage and Zfs height 4, Processes height 9. `collapsed == [Sensors, History]`. Check each `Rect`.
- Worked example 2 (body 100×28) gives rows 6, 5, 8 and Processes 9. `collapsed` is empty. Check each `Rect`.
- The 80×24 matrix in design §6.10 (body 80×22; Cpu `want_h` 6, 6, 10; with and without Zfs): Processes height ≥ 9 each time, and Memory is not in `collapsed` while History, Storage or Sensors has an area.
- Put-back: body 80×20 (budget 11) with Cpu (4, 6, 20), History (6, 8, 20), Memory (5, 5, 30), Network (4, 5, 24), Storage (4, 6, 30), Zfs (4, 6, 36), Processes. The height pass removes History, Storage, Network, Zfs in that order; the put-back pass returns Network. Result: `collapsed == [History, Storage, Zfs]`, Memory and Network share row 1, Cpu row 0 is 6 high (spare 2), Processes 9.
- Spare order: with a large body, the row of Cpu, Memory and Zfs reaches its `want_h` before History gets any spare (design §6.10 step 5).
- 110×30 body with all eight requests (Sensors `min_w` 22): nothing collapses. The sum of all heights equals the body height. No areas overlap.
- No Zfs, Storage or Sensors request: row 2 takes no height.
- Processes request absent: the last row gets the spare height.
- Body height 3: every row panel collapses. Processes gets 3.
- Body 0×0 and 1×1: no panic. Areas have zero size or are absent.
- Width pass: a row with one panel never removes that panel for width.
- Property test without a crate: loop over widths 0..=200 step 7 and heights 0..=60 step 3, with the full request set. Each time: no panic, all areas inside `body`, no overlap between areas, and Memory is not in `collapsed` while History, Storage or Sensors has an area.

**Acceptance:** the tests pass.

### T7: config

**Owns:** `src/config.rs`.

**Interface:** design §6.11.

**Tests (required):**

- The example file in design §6.11 parses to the expected `Config`. `panels` holds exactly the five named panels.
- An empty file and a comments-only file give `Config::default()`.
- A leading BOM is accepted.
- CRLF: the example file with each `\n` replaced by `\r\n` parses to the same `Config`, with no "text after value" error.
- Inline comments: `tree = true # yes` works. `sort = "mem#1"` is an error (unknown value), so `#` in a string is kept.
- Errors with the right line number: `[ui]` (tables not supported), duplicate key, `tree = "yes"` (wrong type), `sort = "fast"`, `panels = ["cpu", "gpu"]`, `temp_unit = "k"`, `name = "unterminated`, `panels = ["cpu"`, `interval_ms = -5`, `interval_ms = 5 7`.
- Unknown key `colour = "red"` on line 3: no error, and `warnings == ["line 3: unknown key `colour`, ignored"]`.
- Escapes: `sort = "\"cpu\""` is an error (unknown value `"cpu"` with quotes), and it does not panic.
- `default_path`: Windows rules are tested with `cfg(windows)` only. On other targets: `XDG_CONFIG_HOME=/x` gives `/x/rtop/config.toml`; `XDG_CONFIG_HOME=rel` with `HOME=/h` gives `/h/.config/rtop/config.toml`; empty `XDG_CONFIG_HOME` uses `HOME`; no `HOME` gives `None`.
- `load(&ConfigSource::Disabled, get)` gives a default config and `None`. `load(&ConfigSource::Path(missing), get)` is an error that names the path. `load(&ConfigSource::Default, get)`, where `get` returns `HOME` = an empty temporary directory and nothing else, gives a default config and `None`. With `HOME/.config/rtop/config.toml` written in that directory, it gives the parsed config and that path. Never read the real environment in a test. Use a temporary directory under `std::env::temp_dir()` with a unique name; remove it at the end.
- `read_limited` refuses a file over 64 KiB.
- `SORT_NAMES` has 5 unique entries.

**Acceptance:** the tests pass. The parser has no `unwrap` or index that can panic on any input. Add a test that feeds every line of the module's own test inputs with one byte removed at each position, and checks for no panic.

### T8: help and keymap

**Owns:** `src/help.rs`, `src/ui/help.rs`.

**Interface:** design §6.12. `KEYMAP` content: design §8, grouped into four sections: "General", "Processes", "Panels", "Filter, help and kill prompt".

**Tests (required):**

- `KEYMAP` has four sections. No section is empty. The keys text has `?`, `x`, `t`, `r`, `c`, `n`, `1`, `0`.
- `usage_keys()` has each section title and each action text.
- Render `draw_help` at 100×40: the screen has `keys`, `esc closes`, `kill`, `tree`, `zfs`.
- Render at 80×24 and 40×12: no panic. At 80×24 the title shows.
- Render at 1×1: no panic.

**Acceptance:** the tests pass.

## Stage 2: integration (task S2)

**Owner:** one agent. **Owns:** `src/main.rs`, `src/metrics.rs`, `src/app.rs`, `src/ui.rs`. Reads all other files. Changes another file only to fix a defect, and records each such change in the report.

**Interface:** design §7 and §8.

Steps, in order. Commit after each step that compiles and passes tests.

1. Make branch `r1/integrate` from the S0 commit. Merge `r1/t1-zfs` through `r1/t8-help` one at a time. File ownership is disjoint, so there must be no conflicts. If a conflict occurs, stop and report it.
2. Run the rule 5 commands. All must pass before you change code.
3. **Process types.** In `metrics.rs`, replace `ProcInfo` with `pub use crate::process::ProcInfo;`. In `app.rs`, replace `SortKey` with `pub use crate::process::SortKey;`. Replace `visible_processes` with `visible_rows`. Fix the struct literals in `app.rs` and `ui.rs` tests with `..ProcInfo::default()`. Update `sorts_by_each_column` for the new `Io` step. Replace the body of `draw_processes` in `ui.rs` with a call to `ui::process::draw_processes`. Remove `#[allow(dead_code)]` from `mod process` in both files.
4. **Sampler.** Add the collectors, `SLOW_PERIOD`, the kill channel and the new `Snapshot` fields (design §7.1, §7.2). Compute `slow` before the process refresh and pass it to `process::refresh_kind(slow)`. In `main.rs`, make the channel `let (kill_tx, kill_rx) = mpsc::channel();` and pass `kill_rx` to `spawn_sampler`. Apply the §7.6 decision: option A (default unless the maintainer says B) calls `zfs::arc_adjusted_used` under `#[cfg(target_os = "linux")]`; option B deletes `arc_adjusted_used` and its test from `src/zfs.rs` and records that change. Remove the `allow` from `zfs`, `storage`, `sensors`, `net`.
5. **App state and keys.** Add the fields (including `highlighted`), `Mode::Help`, `Mode::ConfirmKill`, `with_kill_sender`, `apply_config`, the key table (design §8) and `Event::KillDone`. `x` builds the target from `app.highlighted`. Ignore Ctrl and Alt keys in Normal mode. Clear `status` on each key press, before the key action.
6. **Layout and panels in `ui::draw`.** Follow design §7.5: Cpu `min_h` 4, `core_grid`, `crate::process::visible_rows`, and set `app.highlighted` after the selection is clamped. Use `layout::plan`. Draw each panel. Draw the overlays last. Remove the `allow` from `layout`, `panel` and the `ui` child modules.
7. **CLI and config.** Write `Cli`, `Parsed`, the new `parse_args` and the `usage()` function (design §7.4). Load the config with `config::load(&cli.config, |k| std::env::var_os(k))` before `ratatui::run`. Apply CLI overrides after `apply_config`. Remove the `allow` from `config` and `help`.
8. Confirm no `#[allow(dead_code)]` from S0 remains: `rg -n 'allow\(dead_code' src` must print only the two `sensors` lines from design §5.5 (with `reason = "no FreeBSD battery reader"`).
9. Run all Stage 3 automated commands. If a per-target clippy run finds dead code in a Stage 1 file, the §5.5 table missed an item: fix it with a `cfg` in the same style and record the change in the report.

**Tests to add or change:**

- `app.rs`:
  - `x` on a selected row sets `Mode::ConfirmKill` with that row's pid, start time and name.
  - Batch race: draw a frame (so `highlighted` is row A), then handle `Event::Metrics` with a snapshot that sorts row B first, then `x`, with no draw between. The prompt target is row A.
  - `x` with `highlighted` `None`: status `nothing selected`.
  - `y` sends a `KillRequest` with `Term` on the channel (use a real `mpsc::channel` in the test). `f` sends `Kill`. `n` and `Esc` cancel and send nothing. `Enter` does nothing.
  - `x` on pid 1 sets status `refused: pid 1 is protected`, and the mode stays Normal.
  - `x` without a sender, then `y`: status `kill is not available`.
  - `Event::KillDone` sets `status` while paused.
  - `?` opens Help. `Esc`, `q`, `?` close it. `q` in Help does not quit.
  - Digits toggle `visible`. `0` restores all. A digit for Zfs with `snapshot.zfs == None` sets `zfs: not available here`.
  - `Ctrl-x` and `Alt-q` do nothing in Normal mode. `Ctrl-C` still quits.
  - `t`, `r`, `c`, `n` toggle their fields.
  - `apply_config` sets sort, reverse, tree, command, panels, fahrenheit, net_detail, and clamps `interval_ms`.
  - Any key clears `status`.
- `ui.rs`:
  - `full_frame_shows_every_panel` at 110×40 with ZFS, storage, sensors and a battery in the snapshot: each panel title shows.
  - 80×24 with the worked-example 1 data: `zfs arc`, `storage`, `memory`, `network`, `processes` show; `sensors` and `history` do not; the footer has `2 hidden`; the process table shows 6 rows; the CPU panel shows all 8 core labels (`0` to `7`).
  - 80×24 for 4, 8 and 16 cores, each with and without ZFS (3 volumes, 10 temperatures): the process table shows at least 6 rows, and `memory` shows while `storage` shows.
  - `core_grid`: 8 cores in 78×2 gives 4; 8 cores in 33×4 gives 2; 16 cores in 78×2 gives 5 (the width limit); 0 cores gives 1.
  - No ZFS, no sensors, no volumes: the old panels fill the space; no `zfs`, `storage` or `sensors` text.
  - `2` (History hidden): no `history` text.
  - Help mode: `esc closes` shows. Confirm kill mode: `y TERM` shows.
  - `tiny_terminals_do_not_panic` covers all modes.
- `main.rs`:
  - `--config x` gives `ConfigSource::Path`. `--no-config` gives `Disabled`. Both together: error. `-t` sets `tree`. `-V` gives `Version`. `-i 500` gives `Some(500)`. No `-i` gives `None`.
  - `usage()` has `KEYS:` and `kill`.
- `metrics.rs` (option A only, `#[cfg(target_os = "linux")]` test): the sampler's memory step with `mem_used` 10 000 and an ARC of size 6 000, min 1 000 gives 5 000. Put the step in a small pure function so it is testable.

**Acceptance:**

- Stage 3 automated commands pass.
- Every key in design §8 works as stated (tests above).
- `cargo run` on the Mac host shows the new panels that have data, and the help overlay.

**Rollback:** the merge to `main` is one merge commit, or a short series of commits on `r1/integrate`. Revert them. No data migration exists. The config file is new and optional, so an old binary ignores it.

## Stage 3: verification (task S3)

### 3.1 Automated

Run from the repo root on the Mac host:

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
for t in aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-pc-windows-msvc x86_64-unknown-freebsd; do
  echo "== $t"
  cargo clippy --all-targets --locked --target "$t" -- -D warnings || break
done
git diff --stat main -- Cargo.lock      # only the rtop dependency list changed in S0
cargo build --release --locked && stat -f %z target/release/rtop   # compare with the S0 baseline; growth <= 150 KiB
```

`cargo clippy` does the work of `cargo check`, and it also finds warnings in `cfg` code for each target. No target needs a linker for these commands.

### 3.2 Manual: Linux under Docker with a ZFS fixture

The debug-only `RTOP_ZFS_ARCSTATS` path makes rtop read a file instead of `/proc/spl/kstat/zfs/arcstats`. A Docker bind mount under `/proc` is not possible. A loop makes the counters grow, so the hit ratio shows a value.

```sh
docker run --rm -it -v "$PWD":/src:ro -w /src \
  -e CARGO_TARGET_DIR=/tmp/target -e RTOP_ZFS_ARCSTATS=/tmp/arcstats \
  rust:1.99-alpine sh -c '
    cargo build --locked &&
    cp testdata/arcstats-2.2.txt /tmp/arcstats &&
    ( i=0; while :; do i=$((i+1));
        awk -v i="$i" "\$1==\"hits\"{\$3+=i*9000} \$1==\"misses\"{\$3+=i*1000} {print}" \
          testdata/arcstats-2.2.txt > /tmp/arcstats.new && mv /tmp/arcstats.new /tmp/arcstats;
        sleep 1; done ) &
    stty cols 80 rows 24 && /tmp/target/debug/rtop'
```

Check on screen:

1. The ZFS panel shows `zfs arc 9.0 GiB`. The first frame shows `boot hit 85.0%`; within about six seconds it shows `hit 90.0%` without `boot`, and the value does not flicker to `—`.
2. At 80×24, Sensors is hidden (a container has no sensors), History is hidden, and the footer does not claim hidden panels that have no data. The process table shows 6 rows. With option A, the memory gauge reads lower than `free` in the container by up to 8 GiB (the fixture's `size − c_min`), and never below 0%. With option B, it matches `free`.
3. `6` hides and shows the ZFS panel.
4. `?` opens help. `Esc` closes it.
5. `t` shows the tree. `c` shows the command lines.
6. `x` on the `sh` loop process, then `n`: nothing happens. `x` then `y`: the footer says `sent TERM to …`. On PID 1 (`sh` is PID 1 in the container): the footer says `refused: pid 1 is protected`.
7. Without `stty`, resize the terminal to 60×15 and 200×60: no panic.
8. `q` quits. The terminal is clean.

### 3.3 Manual: Mac host

```sh
RTOP_ZFS_ARCSTATS=testdata/arcstats-2.2.txt cargo run
cargo run --release -- --no-config -t
printf 'sort = "mem"\npanels = ["cpu", "memory", "processes"]\n' > /tmp/rtop-test.toml && cargo run --release -- --config /tmp/rtop-test.toml
printf '[ui]\n' > /tmp/rtop-bad.toml && cargo run --release -- --config /tmp/rtop-bad.toml   # must exit with "line 1: …"
```

Check: the sensors panel shows temperatures; a MacBook shows the battery line; storage shows `/` and `/System/Volumes/Data`; the threads column shows values for your own processes; `n` shows per-interface rows.

Disk I/O on the Mac: run `iostat -d -w 1 disk0` in a second terminal and copy a large file (`dd if=/dev/zero of=/tmp/rtop-io bs=1m count=2000`, then `rm /tmp/rtop-io`). The storage `io` line is within about 20% of `iostat`, not twice it. In a terminal of 80×24, the process table shows 6 rows.

Option A memory note: the fixture ARC does not change the macOS memory gauge (the adjustment is Linux only).

### 3.4 Not covered

- A real ZFS host (Linux, FreeBSD, macOS O3X). The Appendix A fixtures are synthetic. Ask a user with ZFS to run `cat /proc/spl/kstat/zfs/arcstats` (Linux) or `sysctl kstat.zfs.misc.arcstats` (FreeBSD) and add the capture as a third fixture. Ask the same user to compare rtop's memory gauge with `free -b` and `arc_summary`.
- FreeBSD `/` on `zroot/ROOT/default`: covered only by the T2 unit test.
- Windows at run time. Only clippy covers Windows. The release workflow builds it and runs `--help`.
- FreeBSD at run time.

## Order and parallel work

```text
S0 ──► T1 ┐
      T2 │
      T3 │
      T4 ├──► S2 ──► S3
      T5 │
      T6 │
      T7 │
      T8 ┘
```

- T1 to T8 run at the same time. Their owned files do not overlap.
- T4 is the largest task. T6 and T8 are the smallest.
- S2 can merge each Stage 1 branch when it is done, but it starts step 3 only after all eight are merged.

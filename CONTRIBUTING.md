# Contributing to rtop

Thank you for your help. This guide tells you how to set up, which checks must pass, and how the code is written.

To report a security problem, do not open an issue. Read [SECURITY.md](SECURITY.md).

## Before you start

- For a bug, open an issue with the bug report form. Include `rtop --version`, the OS and the terminal.
- For a new feature, open an issue first, so we can agree on the shape before you write the code. rtop stays small on purpose, so we decline some good ideas.
- Small fixes (typos, a clear bug with a test) can go straight to a pull request.

## Set up

You need Rust 1.95 or later (the `rust-version` in `Cargo.toml`). Install it with [rustup](https://rustup.rs).

```sh
git clone https://github.com/DarkWaveLabs/rtop.git
cd rtop
cargo run                     # debug build
cargo run --release -- -i 500 # release build, 500 ms refresh
```

To read how the code fits together before you change it, see the [architecture page](docs/architecture.md). To build a static Linux binary with Docker, see [Install](docs/install.md#build-from-source).

On macOS with the Homebrew `rustup` formula, `cargo` is keg-only. Add `/opt/homebrew/opt/rustup/bin` to your `PATH` if your shell cannot find it.

### Test the ZFS panel without ZFS

A debug build reads ZFS ARC counters from a file when `RTOP_ZFS_ARCSTATS` is set. Release builds ignore this variable.

```sh
RTOP_ZFS_ARCSTATS=testdata/arcstats-2.2.txt cargo run
```

The file does not change, so there are no reads in the window. The hit values then show the since-boot ratios with the `boot` label. The fixtures in `testdata/` are synthetic. See [ZFS ARC](docs/zfs.md#try-the-panel-without-zfs).

## Checks

CI (`.github/workflows/ci.yml`) runs these on Linux for each pull request. All must pass:

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

rtop has code for each OS behind `cfg` attributes, so a Linux build does not compile the macOS, Windows or FreeBSD code. Run clippy for the other targets too. These checks need no linker, only the targets:

```sh
rustup target add x86_64-unknown-linux-musl x86_64-pc-windows-msvc x86_64-unknown-freebsd aarch64-apple-darwin
for t in x86_64-unknown-linux-musl x86_64-pc-windows-msvc x86_64-unknown-freebsd aarch64-apple-darwin; do
  cargo clippy --all-targets --locked --target "$t" -- -D warnings || break
done
```

To run the tests on Linux from another OS, use Docker:

```sh
docker run --rm -v "$PWD":/src:ro -w /src -e CARGO_TARGET_DIR=/tmp/target \
  rust:1.99-alpine cargo test --locked
```

If you change code for one OS, say in the pull request whether you ran it on that OS. The [platforms page](docs/platforms.md) lists what each OS supports and how it is tested.

## Code rules

These rules come from the existing code. Follow them in new code.

- **Logic without I/O.** Put decisions in pure functions that take plain values: parsing, sorting, layout, formatting, rate maths. `app.rs` has no terminal code, `layout.rs` is pure, and `config::parse` and `zfs::parse_kstat` take text, not files. Test them with plain unit tests.
- **Test the drawing with `TestBackend`.** UI code renders into ratatui's `TestBackend`. Tests check the text on the screen. See the tests in `src/ui/*.rs`.
- **Tests do not read the real machine.** Use fixtures (`testdata/`) or `const` values. To read an environment variable, take a getter closure, as `config::load` does. Do not call `std::env::set_var` in tests.
- **`unsafe` is for FFI only.** Put a `// SAFETY:` comment on each `unsafe` block that says why the call is sound: pointer validity, sizes, lifetimes, and who owns the result.
- **`cfg` for each target.** Put OS-specific code behind `#[cfg(target_os = "...")]` (or `cfg(windows)`), and give every other OS a fallback that returns `None` or nothing. A function that only one OS uses must not be dead code on the others. Clippy for each target finds this.
- **No new dependencies without a reason.** rtop has three direct crates on every OS, plus `libc`, `objc2-*` and `windows` for some targets. Before you add a crate, check if a small hand-written function does the job (the config parser and the CLI parser are hand-written for this reason). If you add a crate, say in the pull request what it adds to `Cargo.lock` and to the binary size.
- **Keep the binary small.** The release profile optimises for size. Compare the stripped release binary before and after your change (`cargo build --release`, then `ls -l target/release/rtop`). Explain any growth of more than a few KiB.
- **Keep sampling cheap.** The sampler runs as often as every 250 ms. Read only what a tick needs, read a value once when it does not change (names, command lines), and give slow reads (disk lists, sensors, disk space) their own period in `Clocks::plan`. Measure a change that touches the sampler (see [Measure CPU use](#measure-cpu-use)).
- **One key table.** Keys live in `src/help.rs` (`KEYMAP`). The help overlay and `--help` both read it. Add a new key there, and handle it in `app.rs`.
- **No locks.** The sampler owns the `sysinfo` handles and sends snapshots over a channel. Keep shared state out of the UI thread.
- **Spelling.** Docs and comments use South African English (colour, organise, licence).

## Measure CPU use

rtop should cost about as much CPU as htop. Three tools measure it:

- `cargo test --release sample_cost -- --ignored --nocapture` times each kind of sampler tick on this host.
- `cargo test --release collector_cost -- --ignored --nocapture` prints the thread CPU time and wall time of each collector. Wall time also counts waits on other processes, such as the sensor service on macOS.
- `scripts/cpubench.py` runs a build in a pseudo-terminal for a fixed time and prints its CPU use. Compare two builds, or rtop and htop, at the same interval and terminal size. See [`scripts/README.md`](scripts/README.md).

Say in the pull request what you measured, before and after.

## Commits

- Subject line: imperative mood, under 72 characters, no full stop. For example: `Show L2ARC size in the ZFS panel`.
- Put the reason and the detail in the body, wrapped at about 72 characters.
- One logical change for each commit. Keep formatting changes apart from behaviour changes.
- Do not add generated attribution lines.

## Pull requests

1. Fork the repository and make a branch from `main`.
2. Make the change, with tests. A bug fix starts with a test that fails without the fix.
3. Run the checks above, for each target.
4. Open the pull request against `main` and complete the template.
5. CI must pass. A maintainer reviews the change. We may ask for changes, or squash small fix-up commits when we merge.
6. Add a line to `CHANGELOG.md` under the unreleased version for a change that users can see.

By sending a pull request, you agree that your contribution is licensed under the [MIT licence](LICENSE).

# Install

You can download a binary or build rtop from source. rtop has no runtime dependencies.

## Release binaries

Each release (`v*`) has one binary for each platform and a `SHA256SUMS` file. Get them from the [latest release](https://github.com/DarkWaveLabs/rtop/releases/latest). The [release workflow](../.github/workflows/release.yml) builds them.

| File | Platform |
|---|---|
| `rtop-macos-universal` | macOS, Apple Silicon and Intel (two builds joined with `lipo`) |
| `rtop-linux-x86_64-musl` | Linux x86_64, statically linked |
| `rtop-linux-aarch64-musl` | Linux arm64, statically linked |
| `rtop-windows-x86_64.exe` | Windows x86_64. The C runtime is linked statically, so the Visual C++ Redistributable is not necessary. |

To check a download, run the check on the one line for your file. On Linux, use `sha256sum -c`. On macOS, use `shasum -a 256 -c`.

```sh
grep rtop-linux-x86_64-musl SHA256SUMS | sha256sum -c
```

Then make the file executable and move it to a directory on your `PATH`, for example:

```sh
chmod +x rtop-linux-x86_64-musl
mv rtop-linux-x86_64-musl ~/.local/bin/rtop
```

- **macOS.** The binary is not signed. If you downloaded it with a browser, Gatekeeper blocks the first run. Remove the quarantine flag first: `xattr -d com.apple.quarantine rtop-macos-universal`.
- **Windows.** SmartScreen may warn about an unknown publisher. Choose **More info**, then **Run anyway**.

## Build from source

You need Rust 1.95 or later ([rustup](https://rustup.rs)). The Docker build at the end of this page needs no Rust toolchain.

```sh
cargo install --git https://github.com/DarkWaveLabs/rtop --locked
```

Or build it from a clone:

```sh
git clone https://github.com/DarkWaveLabs/rtop.git
cd rtop
cargo build --release --locked
./target/release/rtop
```

The release profile builds for size (`opt-level = "s"`, LTO, stripped). The result for version 0.2.0 is 904 KiB on macOS arm64 and 1.07 MiB for the Docker build below (Linux arm64, musl).

To build a static Linux binary with Docker, so you need no Rust toolchain on the host:

```sh
mkdir -p dist
docker run --rm -v "$PWD":/src:ro -v "$PWD/dist":/out -w /src \
  -e CARGO_TARGET_DIR=/tmp/target rust:1.99-alpine \
  sh -c 'cargo build --release --locked && cp /tmp/target/release/rtop /out/rtop-linux-musl'
```

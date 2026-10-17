## What and why

<!-- What does this change do, and why? Link the issue: "Fixes #123". -->

## How you tested it

<!-- The OS (and terminal) you ran it on, and what you checked by hand. -->

## Checklist

- [ ] `cargo fmt --check` passes.
- [ ] `cargo clippy --all-targets --locked -- -D warnings` passes.
- [ ] `cargo test --locked` passes.
- [ ] Clippy passes for the other targets: `x86_64-unknown-linux-musl`, `x86_64-pc-windows-msvc`, `x86_64-unknown-freebsd`, `aarch64-apple-darwin` (see CONTRIBUTING.md).
- [ ] New logic is in pure functions with unit tests; new drawing has a `TestBackend` test.
- [ ] Each new `unsafe` block has a `// SAFETY:` comment.
- [ ] OS-specific code is behind `cfg`, with a fallback for the other targets.
- [ ] No new dependency, or the reason and the size change are below.
- [ ] Release binary size change: <!-- e.g. +2 KiB on aarch64-apple-darwin, or "not measured" -->
- [ ] New keys are in `src/help.rs` (`KEYMAP`).
- [ ] `CHANGELOG.md` has a line for a change that users can see.

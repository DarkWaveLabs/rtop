# scripts

Helper scripts for people who work on rtop. rtop does not need them to build or run.

| Script | Use |
|---|---|
| [`cpubench.py`](cpubench.py) | Runs a terminal program in a pseudo-terminal for a fixed time and prints its CPU use as one JSON line. Use it to compare rtop with htop, or one rtop build with another. |

## cpubench.py

It needs only the system `python3` (standard library) on macOS or Linux.

```sh
cargo build --release
scripts/cpubench.py --secs 60 --label rtop -- target/release/rtop --no-config -i 1000
HTOPRC=/dev/null scripts/cpubench.py --secs 60 --label htop -- htop -d 10
```

- `cpu_pct_steady` is the CPU use after the warm-up (5 s by default), in percent of one core. Compare this value.
- `cpu_pct_total` includes start-up.
- `user_s` and `sys_s` split the total into user and kernel time.
- `tty_bytes_per_s` is how much the program writes to the terminal.

Run each case at least twice, at the same terminal size (`--cols`, `--rows`, default 120×40), with nothing else busy on the host. For the cost of one sampler tick without a terminal, run `cargo test --release sample_cost -- --ignored --nocapture`.

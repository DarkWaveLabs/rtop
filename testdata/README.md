# testdata

Test fixtures. Tests load them with `include_str!`, so no test reads the real machine.

| File | Contents |
|---|---|
| `arcstats-2.2.txt` | A ZFS ARC kstat file in the OpenZFS 2.2 format (`/proc/spl/kstat/zfs/arcstats`). |
| `arcstats-2.2-next.txt` | The same file one sample later, with higher hit and miss counters. Tests use the pair to check the rate and hit-ratio maths. |

The two files are **synthetic**. They are not a capture from a real host. The values are chosen so the test arithmetic is exact, and rows that rtop does not read are kept to test that unknown names are skipped. Design Appendix A (`docs/designs/features/2026-10-16-rtop-feature-round-1.md`) describes them.

Users: `src/zfs.rs` (parser and delta tests) and `src/ui/zfs.rs` (panel tests). A debug build can also show them on screen:

```sh
RTOP_ZFS_ARCSTATS=testdata/arcstats-2.2.txt cargo run
```

# docs

Project documents. Each folder groups files by topic, and each file name starts with its date (`YYYY-MM-DD-<name>.md`).

| Folder | Contents |
|---|---|
| `designs/` | Designs that the code implements: decisions, interfaces, invariants and risks. The newest file in a topic is the current design. |
| `plans/` | Implementation plans for the designs, under the same topic name: tasks, file ownership, checks and acceptance. |
| `assets/` | Images for the README and the docs, for example screenshots. |

Current topic: `features/` holds the feature round 1 design and plan (ZFS ARC, storage, sensors, process tree and kill, per-interface network, layout, help and config). The code in `src/` is the final truth where it differs from a document.

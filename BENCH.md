# Benchmark suite

`revisa bench` (dev-tools only) runs the domain-layer pipeline stage by stage over a deterministic generated corpus and reports median wall time plus machine-independent counters. GUI/frame-path costs are out of scope.

```
mise run bench                            # scale 1, 3 iterations, table output
mise run bench -- --scale 5 --json        # bigger corpus, JSON output
mise run bench -- --filter walk           # only stages matching a substring
mise run bench -- --left DIR --right DIR  # external corpus (no rename scoring)
```

## Stages

| Stage | Measures |
|---|---|
| `walk` | Directory scan + pairing + rename detection. `candidates` = deleted × added before detection; `precision`/`recall` score detected renames against the generated ground truth. |
| `read-diff` | Read contents + Myers line diff, sequential (per-file phase-1 cost). |
| `highlight-init` | Syntax dump + theme load. |
| `highlight` | Raw syntect throughput, broken out per extension. |
| `compose` | Full `FileDiffData` construction: diff + highlight + inline diff + styled spans (the cost of opening one file). `styled_mb` = resident span memory. |
| `search-snapshot` | The per-dispatch `SearchableFileData` corpus snapshot. |
| `search[q]` | `compute_file_matches` over all files per query. |
| `fold` | `FoldState` construction + unified prefix-sum, ×8 reps. |
| `fuzzy[q]` | Quick-picker matching over all paths, ×64 reps. |

The corpus (seeded, reproducible) mixes languages (rs/go/md/yaml/json), sizes (40–1200 lines), and change kinds: modifications, exact renames, near renames (10/30% edits), adds/deletes (some with identical line counts, adversarial for size-based rename pre-filters), a binary pair, and an oversized file. Counts multiply with `--scale`; wall times are machine-dependent, counters are not.

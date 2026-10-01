**revisa** is a directory diff review tool, mostly meant to be used as a git difftool. It targets Linux only and runs as a native GUI app using egui/eframe.

## Core Tenets

- Separation of UI vs domain concerns
- Avoid unnecessary dependency bloat — use dependencies for core concerns, implement trivial functions instead of pulling in crates
- Test coverage: domain logic must be well tested
- Code quality: run "mise run lint" after changes and fix issues to improve quality
- When doing performance work, read BENCH.md to review the current setup.

## Build System

This project uses **mise**. Prefer `mise run build` / `mise run test` over direct `cargo` commands.

- Build logs are clean (no warnings) — no need to trim output
- `cargo build` gives the exact same output; don't switch for no benefit
- `CARGO_NET_GIT_FETCH_WITH_CLI=true` is configured in mise for cargo commands

## Key Technical Details

### egui/eframe (v0.34)
- `DiffViewCtx` is the central rendering config struct, constructed from `Settings` + `FontVariants`
- Fields use `pub(super)` visibility for submodule access within `diff_view/`
- egui persists widget state (scroll offsets, area sizes) in `Memory` keyed by `Id`
- To reset persisted state (e.g., when reopening a widget), use `Area::sizing_pass(true)` on the first frame — this clears the cached `AreaState.size`
- `ScrollArea::State` only persists offset and bar visibility, NOT visual size — `auto_shrink` evaluates every frame
- Font variants loaded via `fc-match` shell queries at startup; synthetic italic fallback when no real italic font.
- egui 0.34's default behavior with eframe is to repaint only when there's input or request_repaint() is called.

### Diff Engine
- `similar` v3 with `Algorithm::Histogram`
- Inline diff uses custom tokenizer with `min_ratio` guard (0.4) to avoid noisy highlights
- `FoldState` manages fold segments; `unified_view_offsets` is a lazy prefix-sum for O(1) unified view-row mapping
- Unified offsets are cleared on fold mutations and recomputed lazily

### Settings
- See `SETTINGS.md` for full reference


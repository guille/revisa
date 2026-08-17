**revisa** is a directory diff review tool, mostly meant to be used as a git difftool. It targets Linux only and runs as a native GUI app using egui/eframe.

## Current State

All core features are implemented and working:

- **Side-by-side diff view** with scroll-synced old/new panels, syntax highlighting, and inline word-level diff highlighting
- **Unified (stacked) diff view** toggled with Ctrl+M, with dual gutter (old/new line numbers)
- **Sidebar** with file tree view, change kind badges, reviewed checkboxes, and progress bar
- **Quick picker** (Ctrl+P) with fuzzy search for fast file switching
- **Folding** of unchanged regions with clickable expand-up/expand-down bars
- **Rename detection** with heuristic content similarity scoring (parallel via rayon)
- **Syntax highlighting** via syntect with embedded bundled syntaxes and custom .tmTheme support
- **Configurable settings** via TOML (`~/.config/revisa/config.toml`) — see SETTINGS.md
- **All keybinds** configurable; help overlay shows current bindings
- **Exclude directory** feature (right-click context menu in sidebar)
- **Open in editor** feature (configurable editor command or $VISUAL/$EDITOR)
- **Copy path** to clipboard (Ctrl+Y or click icon in header)
- **Search** across all files in the diff
- **Bold/italic font support** via fontconfig
- **All tests passing**, 0 warnings

## Architecture

```
src/
├── main.rs                — CLI (clap subcommands: diff, build-cache), app setup, status bar
├── app.rs                 — AppState, FileDiffData, FontVariants, diff computation
├── domain/
│   ├── mod.rs
│   ├── diff.rs            — Diff computation
│   ├── editor.rs          — "Open in editor" argv building (placeholders + per-editor line syntax)
│   ├── file_pair.rs       — File pairing, walk_and_pair, rename detection
│   ├── file_tree.rs       — Manage the file tree
│   ├── fold.rs            — FoldState, DiffMode, UnifiedSubRow, unified offset mapping
│   ├── hunk.rs            — AlignedRow alignment, hunk navigation
│   ├── review_state.rs    — Reviewed file tracking
│   ├── search.rs          — Search in files functionality
│   └── settings.rs        — TOML settings parsing, validation, keybind system
├── highlight/
│   ├── mod.rs             — StyledSpan, compose_line, Highlighter
│   └── cache.rs           — Syntax cache (bundled + user custom)
└── ui/
    ├── mod.rs
    ├── common.rs           — Shared constants, icon helpers, collapse_path
    ├── diff_view/
    │   ├── mod.rs          — DiffViewCtx, show/show_inner, rendering (SBS + unified)
    │   ├── input.rs        — Keyboard/scroll input handling, momentum scrolling
    │   └── header.rs       — Unified file header bar with copy button
    ├── file_list.rs        — Sidebar mode: file tree with context menu
    ├── help_overlay.rs     — F1 help overlay
    ├── review_complete.rs  — Modal that pops when the user has reviewed all files
    ├── search_panel.rs     — Sidebar mode: search across all files
    └── quick_picker.rs     — Ctrl+P fuzzy file picker
```

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
- TOML config at `$XDG_CONFIG_HOME/revisa/config.toml` (default `~/.config/revisa/config.toml`)
- All colors are `#RRGGBB` or `#RRGGBBAA` hex strings; keybinds are `"key"` or `"ctrl+key"` format
- See `SETTINGS.md` for full reference

### Performance
- Background parallel diff via rayon (file 0 computed eagerly, rest in background)
- Files >4k lines (configurable threshold) get a placeholder message instead of rendering
- Glyph calibration done once; fold labels cached; sidebar defaults cached
- Layout warmup until PPI stable (`PPI_STABLE_THRESHOLD`) to let egui panel layout stabilize

### CLI
- `revisa diff --left /path/left --right /path/right [--config /path/config.toml]`
- `revisa build-cache` — prebuild syntax cache for faster startup
- `revisa bench` (dev-tools feature) — domain-layer benchmark suite over a generated corpus; see BENCH.md

## Out of Scope
- Accessibility
- Line wrapping (horizontal scroll instead)

# Settings

Revisa loads settings from a TOML file at startup. The default location is
`~/.config/revisa/config.toml`. A different path can be passed with the
`--config` CLI flag.

Any key that is omitted uses its default value. An empty file (or missing file)
is perfectly valid — everything falls back to built-in defaults.

---

## `[font]`

| Key            | Type   | Default                       | Description |
|----------------|--------|-------------------------------|-------------|
| `face`         | string | `"monospace"`    | Font family name. Must be installed on the system. |
| `size`         | float  | `14.0`                        | Font size in points for diff panel content. |
| `gutter_size`  | float  | `13.0`                        | Font size for line-number gutters. Set smaller than `size` if you prefer a compact gutter. |

## `[colors]`

All colors are hex strings in `"#RRGGBB"` or `"#RRGGBBAA"` format. The alpha channel is optional and defaults to `FF` (fully opaque).

| Key                  | Type   | Default     | Description |
|----------------------|--------|-------------|-------------|
| `bg_app`             | string | `"#2C2C2C"` | Application background color. |
| `bg_added`           | string | `"#1C4D2A"` | Row background for added lines. |
| `bg_removed`         | string | `"#3A1E1E"` | Row background for removed lines. |
| `bg_inline_added`    | string | `"#2E7A42"` | Inline (word-level) highlight for added text. |
| `bg_inline_removed`  | string | `"#7A2E2E"` | Inline (word-level) highlight for removed text. |
| `bg_padding`         | string | `"#232328"` | Background for padding rows (blank lines opposite an add/delete). |
| `bg_fold`            | string | `"#1A2A3A"` | Background for fold separator rows. |
| `bg_header`          | string | `"#1E1E1E"` | Background for the diff panel filename headers. |
| `fg_fold_text`       | string | `"#6090C0"` | Text color of fold separator labels. |
| `fg_fold_line`       | string | `"#406080"` | Color of the horizontal lines bordering fold separators. |
| `fg_gutter`          | string | `"#84786A"` | Line number text color in the gutter. |
| `fg_gutter_added`    | string | `"#2E7A42"` | Gutter line number color for added/new lines. |
| `fg_gutter_removed`  | string | `"#7A2E2E"` | Gutter line number color for removed/old lines. |
| `fg_gutter_separator`| string | `"#3C3C3C"` | Color of the vertical line between gutter and content. |

## `[behavior]`

| Key                    | Type   | Default | Description |
|------------------------|--------|---------|-------------|
| `use_nerdfont_icons`   | bool   | `true`  | Use Nerd Font glyphs for folder, fold, and sidebar icons. Set to `false` to use plain ASCII fallbacks. Requires a Nerd Font to be set in `font.face`. |
| `fold_context`         | int    | `5`     | Number of unchanged lines to keep visible around each diff hunk before folding the rest. |
| `fold_expand_step`     | int    | `20`    | Number of hidden lines revealed per click on a fold expand button. |
| `sidebar_width`        | float    | `25.0`   | Initial sidebar width as a percentage of window width. Set to `0` to start with the sidebar hidden. The sidebar can still be toggled with the `toggle_sidebar` keybind. |
| `theme`                | string | `""`    | Path to a `.tmTheme` file for syntax highlighting. When empty, the built-in theme is used. |
| `line_height`          | float  | `1.5`  | Line height as a multiplier of `font.size`. The actual pixel height is `font.size × line_height`. Must be at least `1.0`. |
| `fold_row_height`      | int    | `2`     | Number of `line_height` units each fold separator occupies. `1` = compact, `2` = standard, `3` = spacious. Must be at least `1`. |
| `editor`               | string | `""`    | Editor command for "open in editor" (Ctrl+O). When empty, falls back to `$VISUAL` then `$EDITOR`. May include arguments. |
| `default_diff_mode`    | string | `"side-by-side"` | Default diff view mode. Accepts `"side-by-side"`, `"unified"` or `"auto"`. With `"auto"` the mode is picked once at startup based on window width: side-by-side when two 80-column panels fit, unified otherwise. |
| `max_diff_lines`       | int    | `4000`  | Maximum lines per file before showing a "too large" placeholder. Files above this limit can still be computed on demand via the "Calculate anyway" button. Set to `0` to disable the limit (all files computed regardless of size). |

## `[keybinds]`

Key names follow a `Modifier+Key` format. Supported modifiers: `Ctrl`, `Shift`, `Alt`.
Keys are case-insensitive. You can press "?" or F1 in-app to display the available keybinds.

| Key                  | Type   | Default           | Description |
|----------------------|--------|-------------------|-------------|
| `next_hunk`          | string | `"."`             | Jump to the next diff hunk. |
| `prev_hunk`          | string | `","`             | Jump to the previous diff hunk. |
| `mark_reviewed_next` | string | `"Enter"`         | Mark the current file as reviewed and open the next unreviewed file. |
| `mark_reviewed`      | string | `"Space"`         | Toggle the reviewed state of the current file without advancing. |
| `next_file`          | string | `"Ctrl+Down"`     | Open the next file without changing the reviewed state. |
| `prev_file`          | string | `"Ctrl+Up"`       | Open the previous file without changing the reviewed state. |
| `quick_picker`       | string | `"Ctrl+P"`        | Open the quick file picker overlay. |
| `toggle_sidebar`     | string | `"Ctrl+B"`        | Show or hide the sidebar. |
| `fold_all`           | string | `"Ctrl+Shift+F"`  | Collapse all unchanged regions to folds. |
| `unfold_all`         | string | `"Ctrl+Shift+E"`  | Expand all folds, showing the full file. |
| `open_in_editor`     | string | `"Ctrl+O"`        | Open the current file in an external editor. |
| `copy_path`          | string | `"Ctrl+Y"`        | Copy the open file's path. |
| `toggle_diff_mode`   | string | `"Ctrl+M"`        | Toggle between side-by-side and unified diff view. |
| `goto_line`          | string | `"Ctrl+G"`        | Open picker in go-to-line mode (type `:N` to jump to line N). |

---

## Example `settings.toml`

```toml
[font]
face = "JetBrainsMono Nerd Font"
size = 14.0

[colors]
bg_app = "#1E1E2E"

[behavior]
fold_context = 3
sidebar_width = 25
theme = "~/.config/revisa/dracula.tmTheme"

[keybinds]
next_hunk = "n"
prev_hunk = "N"
```

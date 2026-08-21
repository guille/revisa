use std::ops::Range;
use std::path::Path;
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color as SyntectColor, FontStyle as SyntectFontStyle, Style as SyntectStyle, Theme, ThemeSet,
};
use syntect::parsing::{SyntaxReference, SyntaxSet};

pub mod cache;

/// A styled span for a single segment of a line.
/// Combines syntax highlighting (foreground) with diff annotation (background).
#[derive(Debug, Clone, PartialEq)]
pub struct StyledSpan {
    /// Byte range into the line string.
    pub range: Range<usize>,
    /// Foreground color (from syntax highlighting).
    pub fg: [u8; 4], // RGBA
    /// Background color (from diff annotation).
    pub bg: [u8; 4], // RGBA
    /// Whether text is bold.
    pub bold: bool,
    /// Whether text is italic.
    pub italic: bool,
}

/// A span style, de-duplicated per file into a small table. A theme yields a
/// few dozen distinct (fg, bg, flags) combinations, shared by thousands of spans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpanStyle {
    pub fg: [u8; 4],
    pub bg: [u8; 4],
    pub bold: bool,
    pub italic: bool,
}

/// A packed styled span: byte range into the line plus a style-table index.
/// 8 bytes vs 40 for `StyledSpan`; spans longer than `u16::MAX` bytes are
/// split into same-style chunks at build time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackedSpan {
    pub start: u32,
    pub len: u16,
    pub style: u16,
}

impl PackedSpan {
    pub fn range(self) -> Range<usize> {
        self.start as usize..self.start as usize + self.len as usize
    }
}

/// Interns `SpanStyle`s into a compact table during composition.
#[derive(Default)]
pub struct StyleInterner {
    styles: Vec<SpanStyle>,
    lookup: std::collections::HashMap<SpanStyle, u16>,
}

impl StyleInterner {
    pub fn intern(&mut self, style: SpanStyle) -> u16 {
        if let Some(&id) = self.lookup.get(&style) {
            return id;
        }
        // A full table is practically impossible (styles are theme scopes ×
        // diff backgrounds); degrade to style 0 rather than grow past u16.
        if self.styles.len() > usize::from(u16::MAX) {
            return 0;
        }
        let id = self.styles.len() as u16;
        self.styles.push(style);
        self.lookup.insert(style, id);
        id
    }

    /// Consume the interner, keeping only the table.
    pub fn finish(self) -> Vec<SpanStyle> {
        self.styles
    }
}

/// Flat per-row styled spans: one contiguous span buffer plus a row-offset
/// index. Replaces a `Vec<Vec<StyledSpan>>` — no per-row heap allocation,
/// and styles live in a shared table referenced by index.
#[derive(Default)]
pub struct StyledRows {
    spans: Vec<PackedSpan>,
    /// Row `r`'s spans live at `spans[row_offsets[r]..row_offsets[r + 1]]`.
    row_offsets: Vec<u32>,
}

impl StyledRows {
    pub fn with_row_capacity(rows: usize) -> Self {
        let mut row_offsets = Vec::with_capacity(rows + 1);
        row_offsets.push(0);
        Self {
            spans: Vec::new(),
            row_offsets,
        }
    }

    /// Pack one row of composed spans, interning their styles.
    pub fn push_row(&mut self, row: &[StyledSpan], interner: &mut StyleInterner) {
        for span in row {
            let style = interner.intern(SpanStyle {
                fg: span.fg,
                bg: span.bg,
                bold: span.bold,
                italic: span.italic,
            });
            let mut start = span.range.start;
            while start < span.range.end {
                let len = (span.range.end - start).min(usize::from(u16::MAX));
                self.spans.push(PackedSpan {
                    start: start as u32,
                    len: len as u16,
                    style,
                });
                start += len;
            }
        }
        self.row_offsets.push(self.spans.len() as u32);
    }

    /// Spans for a row; empty for out-of-range rows (e.g. placeholders).
    pub fn row(&self, idx: usize) -> &[PackedSpan] {
        match (self.row_offsets.get(idx), self.row_offsets.get(idx + 1)) {
            (Some(&start), Some(&end)) => &self.spans[start as usize..end as usize],
            _ => &[],
        }
    }

    #[cfg(any(test, feature = "dev-tools"))]
    pub fn span_count(&self) -> usize {
        self.spans.len()
    }

    #[cfg(any(test, feature = "dev-tools"))]
    pub fn row_count(&self) -> usize {
        self.row_offsets.len().saturating_sub(1)
    }
}

/// Cached syntax highlighting data for a file: per-line spans.
pub struct HighlightedFile {
    /// Per-line syntax spans: Vec of (style, byte_range) for each line.
    pub lines: Vec<Vec<SyntaxSpan>>,
}

/// A single syntax-highlighted span within a line.
#[derive(Debug, Clone)]
pub struct SyntaxSpan {
    pub range: Range<usize>,
    pub style: SyntectStyle,
}

/// Lines longer than this are not syntax-highlighted (rendered with the
/// default foreground instead).
const MAX_HIGHLIGHT_LINE_BYTES: usize = 1_000;

/// Global highlighting resources (loaded once).
pub struct Highlighter {
    syntax_set: SyntaxSet,
    theme: Theme,
}

impl Highlighter {
    pub fn new(custom_theme_path: Option<&Path>) -> Self {
        let syntax_set = Self::load_syntax_set();
        let theme = match custom_theme_path {
            Some(path) => ThemeSet::get_theme(path).unwrap_or_else(|e| {
                eprintln!("Warning: failed to load theme from {}: {e}", path.display());
                eprintln!("Falling back to default theme.");
                Self::default_theme()
            }),
            None => Self::default_theme(),
        };
        Self { syntax_set, theme }
    }

    /// Load syntax set. Priority: user cache > embedded bundle.
    fn load_syntax_set() -> SyntaxSet {
        // 1. Try user's custom cache (built with `revisa build-cache`,
        //    which also incorporates bat's syntaxes if available).
        if let Some(ss) = cache::load_syntax_cache() {
            return ss;
        }

        // 2. Embedded bundle (always available).
        cache::load_bundled_syntaxes()
    }

    /// Load the default theme embedded from assets/themes/default.tmTheme.
    fn default_theme() -> Theme {
        static BUNDLED_THEME: &[u8] = include_bytes!("../../assets/themes/default.tmTheme");
        let cursor = std::io::Cursor::new(BUNDLED_THEME);
        ThemeSet::load_from_reader(&mut std::io::BufReader::new(cursor))
            .expect("bundled default.tmTheme should be valid")
    }

    /// Return an empty highlighted file (no lines, no syntax work).
    pub fn empty_file() -> HighlightedFile {
        HighlightedFile { lines: Vec::new() }
    }

    /// Get the default foreground color from the theme.
    pub fn default_fg(&self) -> [u8; 4] {
        let fg = self.theme.settings.foreground.unwrap_or(SyntectColor {
            r: 200,
            g: 200,
            b: 200,
            a: 255,
        });
        [fg.r, fg.g, fg.b, fg.a]
    }

    /// Highlight an entire file, returning per-line syntax spans.
    pub fn highlight_file(&self, content: &str, filename: &str) -> HighlightedFile {
        let syntax = self.detect_syntax(filename, content);
        let mut highlighter = HighlightLines::new(syntax, &self.theme);
        let mut buf = String::with_capacity(256);
        let mut lines = Vec::new();

        for line in content.lines() {
            // Very long lines (minified content) render with the default
            // style: regex cost grows with line length — pathologically so
            // under fancy-regex — and highlighting adds nothing there.
            // The parse state carries over the skipped line unchanged.
            if line.len() > MAX_HIGHLIGHT_LINE_BYTES {
                lines.push(Vec::new());
                continue;
            }
            // syntect expects the line with its newline for state tracking.
            // Reuse a buffer to avoid per-line allocation.
            buf.clear();
            buf.push_str(line);
            buf.push('\n');
            let ranges = highlighter
                .highlight_line(&buf, &self.syntax_set)
                .unwrap_or_default();

            let mut offset = 0;
            let spans: Vec<SyntaxSpan> = ranges
                .iter()
                .filter_map(|(style, text)| {
                    // Trim the trailing newline we added.
                    let trimmed = if offset + text.len() > line.len() {
                        &text[..text.len().saturating_sub(1)]
                    } else {
                        text
                    };
                    let len = trimmed.len();
                    if len == 0 {
                        offset += text.len();
                        return None;
                    }
                    let span = SyntaxSpan {
                        range: offset..offset + len,
                        style: *style,
                    };
                    offset += text.len();
                    Some(span)
                })
                .collect();
            lines.push(spans);
        }

        HighlightedFile { lines }
    }

    /// Pick a syntax for `filename`, falling back to `content`'s first line
    /// when the name is unrecognized.
    ///
    /// Deliberately avoids `SyntaxSet::find_syntax_for_file`, which opens
    /// `filename` from disk: the name here is relative to the diff root, so
    /// that resolves against the process CWD and matches an unrelated file.
    fn detect_syntax(&self, filename: &str, content: &str) -> &SyntaxReference {
        let path = Path::new(filename);
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let extension = path.extension().and_then(|x| x.to_str()).unwrap_or("");
        self.syntax_set
            .find_syntax_by_extension(name)
            .or_else(|| self.syntax_set.find_syntax_by_extension(extension))
            .or_else(|| {
                content
                    .lines()
                    .next()
                    .filter(|l| l.len() <= MAX_HIGHLIGHT_LINE_BYTES)
                    .and_then(|l| self.syntax_set.find_syntax_by_first_line(l))
            })
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text())
    }
}

/// Diff background kind for compositing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffBg {
    /// Unchanged line — use theme default background.
    None,
    /// Added line (right side).
    Added,
    /// Removed line (left side).
    Removed,
    /// Modified line (old side).
    ModifiedOld,
    /// Modified line (new side).
    ModifiedNew,
}

/// Configurable colors for diff backgrounds.
#[derive(Debug, Clone, Copy)]
pub struct DiffBgColors {
    pub added: [u8; 4],
    pub removed: [u8; 4],
}

impl DiffBgColors {
    pub fn resolve(self, bg: DiffBg) -> [u8; 4] {
        match bg {
            DiffBg::None => [0, 0, 0, 0],
            DiffBg::Added | DiffBg::ModifiedNew => self.added,
            DiffBg::Removed | DiffBg::ModifiedOld => self.removed,
        }
    }
}

/// Compose syntax highlighting spans with a diff background into final styled spans.
pub fn compose_line(
    syntax_spans: &[SyntaxSpan],
    diff_bg: DiffBg,
    line_len: usize,
    default_fg: [u8; 4],
    colors: DiffBgColors,
) -> Vec<StyledSpan> {
    let bg = colors.resolve(diff_bg);

    if syntax_spans.is_empty() {
        // No syntax highlighting — single span for the whole line.
        if line_len == 0 {
            return vec![];
        }
        return vec![StyledSpan {
            range: 0..line_len,
            fg: default_fg,
            bg,
            bold: false,
            italic: false,
        }];
    }

    syntax_spans
        .iter()
        .map(|syn| StyledSpan {
            range: syn.range.clone(),
            fg: [
                syn.style.foreground.r,
                syn.style.foreground.g,
                syn.style.foreground.b,
                syn.style.foreground.a,
            ],
            bg,
            bold: syn.style.font_style.contains(SyntectFontStyle::BOLD),
            italic: syn.style.font_style.contains(SyntectFontStyle::ITALIC),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use syntect::highlighting::{Color, FontStyle, Style};

    fn make_syntax_span(range: Range<usize>, r: u8, g: u8, b: u8) -> SyntaxSpan {
        SyntaxSpan {
            range,
            style: Style {
                foreground: Color { r, g, b, a: 255 },
                background: Color {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 0,
                },
                font_style: FontStyle::empty(),
            },
        }
    }

    fn test_colors() -> DiffBgColors {
        DiffBgColors {
            added: [0x1C, 0x4D, 0x2A, 255],
            removed: [0x3A, 0x1E, 0x1E, 255],
        }
    }

    #[test]
    fn test_compose_empty_line() {
        let result = compose_line(&[], DiffBg::None, 0, [200, 200, 200, 255], test_colors());
        assert!(result.is_empty());
    }

    #[test]
    fn test_compose_no_syntax_spans() {
        let result = compose_line(&[], DiffBg::Added, 10, [200, 200, 200, 255], test_colors());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].range, 0..10);
        assert_eq!(result[0].bg, [0x1C, 0x4D, 0x2A, 255]);
        assert_eq!(result[0].fg, [200, 200, 200, 255]);
    }

    #[test]
    fn test_compose_with_syntax_spans() {
        let spans = vec![
            make_syntax_span(0..5, 255, 0, 0),  // red keyword
            make_syntax_span(5..10, 0, 255, 0), // green string
        ];
        let result = compose_line(
            &spans,
            DiffBg::ModifiedNew,
            10,
            [200, 200, 200, 255],
            test_colors(),
        );
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].fg, [255, 0, 0, 255]);
        assert_eq!(result[0].bg, [0x1C, 0x4D, 0x2A, 255]); // modified new bg (same as added)
        assert_eq!(result[1].fg, [0, 255, 0, 255]);
        assert_eq!(result[1].bg, [0x1C, 0x4D, 0x2A, 255]);
    }

    #[test]
    fn test_compose_preserves_bold_italic() {
        let spans = vec![SyntaxSpan {
            range: 0..5,
            style: Style {
                foreground: Color {
                    r: 100,
                    g: 100,
                    b: 100,
                    a: 255,
                },
                background: Color {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 0,
                },
                font_style: FontStyle::BOLD | FontStyle::ITALIC,
            },
        }];
        let result = compose_line(&spans, DiffBg::None, 5, [200, 200, 200, 255], test_colors());
        assert!(result[0].bold);
        assert!(result[0].italic);
    }

    #[test]
    fn test_compose_unchanged_has_transparent_bg() {
        let spans = vec![make_syntax_span(0..5, 100, 100, 100)];
        let result = compose_line(&spans, DiffBg::None, 5, [200, 200, 200, 255], test_colors());
        assert_eq!(result[0].bg, [0, 0, 0, 0]); // transparent
    }

    /// Syntax detection reads only its two arguments — never the filesystem.
    ///
    /// Every case below uses a path that does not exist relative to the test
    /// CWD, so any implementation that opens `filename` (as
    /// `SyntaxSet::find_syntax_for_file` does) fails these.
    #[test]
    fn test_detect_syntax_never_touches_disk() {
        let h = Highlighter::new(None);
        let name_of = |file: &str, content: &str| h.detect_syntax(file, content).name.clone();

        // Extensionless script: detected from the shebang in `content`.
        assert_eq!(name_of("scripts/deploy", "#!/bin/bash\necho hi\n"), "Bash");
        assert_eq!(
            name_of("bin/run", "#!/usr/bin/env python3\nprint(1)\n"),
            "Python"
        );
        // A known extension still wins over a contradicting first line.
        assert_eq!(name_of("src/lib.rs", "#!/bin/bash\nfn main() {}\n"), "Rust");
        // Whole-filename match (the syntax lists "Makefile" as an extension).
        assert_eq!(name_of("build/Makefile", "all:\n\techo hi\n"), "Makefile");
        // Nothing to go on — plain text, not a guess.
        assert_eq!(name_of("notes/scratch", "just some prose\n"), "Plain Text");
    }

    /// Detection follows the content, not a same-named file that happens to
    /// exist near the process CWD. `git-revisa` is an extensionless bash
    /// script in the crate root; tests run with CWD there.
    #[test]
    fn test_detect_syntax_ignores_cwd_namesake() {
        assert!(
            std::path::Path::new("git-revisa").exists(),
            "this test needs an extensionless script in the crate root to act \
             as the decoy; if git-revisa was renamed, point it at the new one",
        );
        let h = Highlighter::new(None);
        // Rust content under the decoy's name must not come back as Bash.
        assert_eq!(
            h.detect_syntax("git-revisa", "fn main() {}\n").name,
            "Plain Text"
        );
    }

    #[test]
    fn test_highlighter_creates_successfully() {
        let h = Highlighter::new(None);
        assert_ne!(h.default_fg(), [0, 0, 0, 0]);
    }

    #[test]
    fn test_highlight_file_produces_lines() {
        let h = Highlighter::new(None);
        let content = "fn main() {\n    println!(\"hello\");\n}\n";
        let result = h.highlight_file(content, "test.rs");
        assert_eq!(result.lines.len(), 3);
        // Each line should have at least one span.
        assert!(!result.lines[0].is_empty());
    }

    #[test]
    fn test_highlight_file_skips_very_long_lines() {
        let h = Highlighter::new(None);
        let long = "let x = 1; ".repeat(200); // > MAX_HIGHLIGHT_LINE_BYTES
        let content = format!("fn main() {{}}\n{long}\nfn other() {{}}\n");
        let result = h.highlight_file(&content, "test.rs");
        assert_eq!(result.lines.len(), 3);
        assert!(!result.lines[0].is_empty());
        // Long line renders plain (empty spans → default fg in compose_line).
        assert!(result.lines[1].is_empty());
        // Following lines are still highlighted.
        assert!(!result.lines[2].is_empty());
    }

    #[test]
    fn test_compose_removed_bg() {
        let spans = vec![make_syntax_span(0..3, 100, 100, 100)];
        let result = compose_line(
            &spans,
            DiffBg::Removed,
            3,
            [200, 200, 200, 255],
            test_colors(),
        );
        assert_eq!(result[0].bg, [0x3A, 0x1E, 0x1E, 255]);
    }

    #[test]
    fn test_compose_modified_old_uses_removed_bg() {
        let spans = vec![make_syntax_span(0..3, 100, 100, 100)];
        let result = compose_line(
            &spans,
            DiffBg::ModifiedOld,
            3,
            [200, 200, 200, 255],
            test_colors(),
        );
        assert_eq!(result[0].bg, [0x3A, 0x1E, 0x1E, 255]); // same as removed
    }

    #[test]
    fn test_compose_multiple_spans_preserve_ranges() {
        let spans = vec![
            make_syntax_span(0..3, 255, 0, 0),
            make_syntax_span(3..7, 0, 255, 0),
            make_syntax_span(7..10, 0, 0, 255),
        ];
        let result = compose_line(
            &spans,
            DiffBg::Added,
            10,
            [200, 200, 200, 255],
            test_colors(),
        );
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].range, 0..3);
        assert_eq!(result[1].range, 3..7);
        assert_eq!(result[2].range, 7..10);
    }

    #[test]
    fn test_compose_no_syntax_no_diff_uses_defaults() {
        let result = compose_line(&[], DiffBg::None, 5, [200, 200, 200, 255], test_colors());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].fg, [200, 200, 200, 255]);
        assert_eq!(result[0].bg, [0, 0, 0, 0]);
        assert!(!result[0].bold);
        assert!(!result[0].italic);
    }

    #[test]
    fn test_diff_bg_colors_resolve_all_variants() {
        let c = test_colors();
        assert_eq!(c.resolve(DiffBg::None), [0, 0, 0, 0]);
        assert_eq!(c.resolve(DiffBg::Added), c.added);
        assert_eq!(c.resolve(DiffBg::Removed), c.removed);
        assert_eq!(c.resolve(DiffBg::ModifiedNew), c.added);
        assert_eq!(c.resolve(DiffBg::ModifiedOld), c.removed);
    }

    fn styled(range: Range<usize>, fg: [u8; 4], bold: bool) -> StyledSpan {
        StyledSpan {
            range,
            fg,
            bg: [0, 0, 0, 0],
            bold,
            italic: false,
        }
    }

    #[test]
    fn test_styled_rows_pack_and_lookup() {
        let mut interner = StyleInterner::default();
        let mut rows = StyledRows::with_row_capacity(3);
        let red = [255, 0, 0, 255];
        let blue = [0, 0, 255, 255];
        rows.push_row(
            &[styled(0..4, red, false), styled(4..9, blue, true)],
            &mut interner,
        );
        rows.push_row(&[], &mut interner);
        rows.push_row(&[styled(2..6, red, false)], &mut interner);
        let styles = interner.finish();

        // red/plain is shared between rows 0 and 2 — three spans, two styles.
        assert_eq!(styles.len(), 2);
        assert_eq!(rows.span_count(), 3);
        assert_eq!(rows.row_count(), 3);

        let r0 = rows.row(0);
        assert_eq!(r0.len(), 2);
        assert_eq!(r0[0].range(), 0..4);
        assert_eq!(styles[usize::from(r0[0].style)].fg, red);
        assert_eq!(r0[1].range(), 4..9);
        assert!(styles[usize::from(r0[1].style)].bold);

        assert!(rows.row(1).is_empty());
        assert_eq!(rows.row(2)[0].style, r0[0].style);
        // Out of range (incl. default-constructed placeholders) → empty.
        assert!(rows.row(3).is_empty());
        assert!(StyledRows::default().row(0).is_empty());
    }

    #[test]
    fn test_styled_rows_splits_oversized_spans() {
        let mut interner = StyleInterner::default();
        let mut rows = StyledRows::with_row_capacity(1);
        let huge = usize::from(u16::MAX) * 2 + 10;
        rows.push_row(&[styled(0..huge, [1, 2, 3, 4], false)], &mut interner);

        let spans = rows.row(0);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].range(), 0..usize::from(u16::MAX));
        assert_eq!(spans[2].range().end, huge);
        // Chunks are contiguous and same-style.
        assert_eq!(spans[1].range().start, spans[0].range().end);
        assert!(spans.iter().all(|s| s.style == spans[0].style));
    }
}

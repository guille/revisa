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
        // 1. Try user's custom cache (built with `revisa build-cache`).
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
        let syntax = self.detect_syntax(filename);
        let mut highlighter = HighlightLines::new(syntax, &self.theme);
        let mut buf = String::with_capacity(256);
        let mut lines = Vec::new();

        for line in content.lines() {
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

    fn detect_syntax(&self, filename: &str) -> &SyntaxReference {
        self.syntax_set
            .find_syntax_for_file(filename)
            .ok()
            .flatten()
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
}

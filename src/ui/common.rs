use crate::domain::file_pair::FileChangeKind;
use eframe::egui;

pub const COLOR_ADDED: egui::Color32 = egui::Color32::from_rgb(0x3F, 0xB9, 0x50);
pub const COLOR_DELETED: egui::Color32 = egui::Color32::from_rgb(0xF8, 0x51, 0x49);

/// Non-selectable label shorthand.
pub fn ns_label(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) {
    ui.add(egui::Label::new(text).selectable(false));
}

/// Ratio of secondary UI font sizes (status bar, sidebar) to the main font size.
pub const UI_FONT_RATIO: f32 = 0.85;
pub const COLOR_MODIFIED: egui::Color32 = egui::Color32::from_rgb(0xD2, 0x99, 0x22);
pub const COLOR_RENAMED: egui::Color32 = egui::Color32::from_rgb(0x6E, 0x9E, 0xCF);
pub const COLOR_PERMISSION: egui::Color32 = egui::Color32::from_rgb(0xB0, 0x80, 0xD0);

/// Horizontal inner margin for header/status bar panels.
pub const PANEL_H_MARGIN: f32 = 8.0;
/// Same margin as an `i8` for `egui::Margin::symmetric`.
pub const PANEL_H_MARGIN_I8: i8 = PANEL_H_MARGIN as i8;

/// Named font family keys for style variants (used in font loading and rendering).
pub const FONT_BOLD: &str = "mono-bold";
pub const FONT_ITALIC: &str = "mono-italic";
pub const FONT_BOLD_ITALIC: &str = "mono-bold-italic";

/// Returns the symbol and color for a given file change kind.
pub fn kind_symbol_colored(kind: FileChangeKind) -> (&'static str, egui::Color32) {
    match kind {
        FileChangeKind::Modified => ("M", COLOR_MODIFIED),
        FileChangeKind::Deleted => ("D", COLOR_DELETED),
        FileChangeKind::Added => ("A", COLOR_ADDED),
        FileChangeKind::Renamed { .. } => ("R", COLOR_RENAMED),
    }
}

// --- Nerd Font icons with ASCII fallbacks ---

/// Directory arrow when expanded.
pub fn icon_dir_expanded(nerdfont: bool) -> &'static str {
    if nerdfont { "\u{f07c}" } else { "v" }
}

/// Directory arrow when collapsed.
pub fn icon_dir_collapsed(nerdfont: bool) -> &'static str {
    if nerdfont { "\u{f07b}" } else { ">" }
}

/// Fold expand-down indicator.
pub fn icon_fold_down(nerdfont: bool) -> &'static str {
    if nerdfont { "\u{f4d9}" } else { "v" }
}

/// Fold expand-up indicator.
pub fn icon_fold_up(nerdfont: bool) -> &'static str {
    if nerdfont { "\u{f4da}" } else { "^" }
}

/// Chevron for expanded/open group.
pub fn icon_chevron_down(nerdfont: bool) -> &'static str {
    if nerdfont { "\u{f078}" } else { "\u{25be}" }
}

/// Chevron for collapsed group.
pub fn icon_chevron_right(nerdfont: bool) -> &'static str {
    if nerdfont { "\u{f054}" } else { "\u{25b8}" }
}

/// Fold single-region indicator (small hidden region).
pub fn icon_fold_single(nerdfont: bool) -> &'static str {
    if nerdfont { "\u{f48c}" } else { "---" }
}

/// Arrow for rename display (old → new).
pub fn icon_rename_arrow(nerdfont: bool) -> &'static str {
    if nerdfont { " \u{ea9c} " } else { " -> " }
}

/// Sidebar toggle when sidebar is shown.
pub fn icon_sidebar_shown(nerdfont: bool) -> &'static str {
    if nerdfont { "\u{f0c9}" } else { "[=]" }
}

/// Sidebar toggle when sidebar is hidden.
pub fn icon_sidebar_hidden(nerdfont: bool) -> &'static str {
    if nerdfont { "\u{f054}" } else { "[>]" }
}

/// Checkmark icon (e.g. review complete).
pub fn icon_check(nerdfont: bool) -> &'static str {
    if nerdfont { "\u{f00c}" } else { "✓" }
}

/// Search / magnifying glass icon.
pub fn icon_search(nerdfont: bool) -> &'static str {
    if nerdfont { "\u{f002}" } else { "\u{1f50d}" }
}

/// Quick picker icon (file switcher).
pub fn icon_picker(nerdfont: bool) -> &'static str {
    if nerdfont { "\u{ea94}" } else { "\u{25b7}" }
}

/// External link icon (open in editor).
pub fn icon_external(nerdfont: bool) -> &'static str {
    if nerdfont { "\u{f08e}" } else { "↗" }
}

/// Columns icon (side-by-side view).
pub fn icon_columns(nerdfont: bool) -> &'static str {
    if nerdfont { "\u{f0db}" } else { "||" }
}

/// Unified view icon (stacked lines).
pub fn icon_unified(nerdfont: bool) -> &'static str {
    if nerdfont { "\u{f039}" } else { "≡" }
}

/// Collapse a file path to fit within `max_width` pixels.
///
/// Returns a list of `(text, dimmed)` segments. Directory segments that were
/// shortened to their first character are marked as dimmed.
/// Segments are shortened left-to-right until the path fits.
pub fn collapse_path(path: &str, max_width: f32, char_width: f32) -> Vec<(String, bool)> {
    let total_chars = (max_width / char_width) as usize;

    if path.chars().count() <= total_chars {
        return vec![(path.to_string(), false)];
    }

    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 1 {
        // Single component (just a filename) — truncate if needed.
        if path.chars().count() > total_chars && total_chars > 1 {
            let truncated: String = path.chars().take(total_chars.saturating_sub(1)).collect();
            return vec![(format!("{truncated}…"), true)];
        }
        return vec![(path.to_string(), false)];
    }

    let mut shortened: Vec<(String, bool)> = parts.iter().map(|p| (p.to_string(), false)).collect();

    // Collapse directories left-to-right to first char.
    for i in 0..shortened.len() - 1 {
        let current_len: usize = shortened
            .iter()
            .map(|(s, _)| s.chars().count())
            .sum::<usize>()
            + shortened.len()
            - 1;
        if current_len <= total_chars {
            break;
        }
        if let Some(first_char) = shortened[i].0.chars().next() {
            shortened[i] = (first_char.to_string(), true);
        }
    }

    // If still too long after collapsing all dirs, truncate the filename.
    let current_len: usize = shortened
        .iter()
        .map(|(s, _)| s.chars().count())
        .sum::<usize>()
        + shortened.len()
        - 1;
    if current_len > total_chars {
        let last = shortened.len() - 1;
        // Budget for filename = total_chars minus (prefix dirs + separators).
        let prefix_len: usize = shortened[..last]
            .iter()
            .map(|(s, _)| s.chars().count())
            .sum::<usize>()
            + last; // +last for '/' separators
        let name_budget = total_chars.saturating_sub(prefix_len);
        if name_budget > 1 {
            let name = &shortened[last].0;
            let truncated: String = name.chars().take(name_budget.saturating_sub(1)).collect();
            shortened[last] = (format!("{truncated}…"), true);
        }
    }

    let mut result = Vec::new();
    for (idx, (seg, dimmed)) in shortened.into_iter().enumerate() {
        if idx > 0 {
            result.push(("/".to_string(), dimmed));
        }
        result.push((seg, dimmed));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collapse_path_fits() {
        let result = collapse_path("src/main.rs", 100.0, 1.0);
        assert_eq!(result, vec![("src/main.rs".to_string(), false)]);
    }

    #[test]
    fn test_collapse_path_shortens_dirs() {
        let result = collapse_path("some/long/path/to/file.rs", 15.0, 1.0);
        let text: String = result.iter().map(|(s, _)| s.as_str()).collect();
        assert!(
            text.chars().count() <= 15,
            "collapsed path too long: {text}"
        );
        assert!(text.ends_with("file.rs"));
        assert!(result[0].1, "first segment should be dimmed");
    }

    #[test]
    fn test_collapse_path_no_dirs() {
        // Single component, budget 3 chars — truncated with ellipsis.
        let result = collapse_path("file.rs", 3.0, 1.0);
        let text: String = result.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(text, "fi…");
        assert!(result[0].1, "truncated filename should be dimmed");
    }

    #[test]
    fn test_collapse_path_no_dirs_fits() {
        let result = collapse_path("file.rs", 10.0, 1.0);
        assert_eq!(result, vec![("file.rs".to_string(), false)]);
    }

    #[test]
    fn test_collapse_path_truncates_filename_after_dirs() {
        let result = collapse_path("a/b/c/very_long_filename.rs", 10.0, 1.0);
        let text: String = result.iter().map(|(s, _)| s.as_str()).collect();
        assert!(
            text.chars().count() <= 10,
            "collapsed path too long: {text} (chars={})",
            text.chars().count()
        );
        assert!(text.contains("…"), "should contain ellipsis: {text}");
    }

    #[test]
    fn test_collapse_path_progressive() {
        let result = collapse_path("ab/cd/ef/file.rs", 14.0, 1.0);
        let text: String = result.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(text, "a/c/ef/file.rs");
        assert!(result[0].1); // "a" is dimmed
        assert!(result[2].1); // "c" is dimmed
        assert!(!result[4].1); // "ef" is not dimmed
    }

    #[test]
    fn test_collapse_path_multibyte() {
        // "données/résumé.txt" has multi-byte chars (é is 2 bytes in UTF-8).
        let result = collapse_path("données/résumé.txt", 18.0, 1.0);
        assert_eq!(result, vec![("données/résumé.txt".to_string(), false)]);

        // With budget 12, dirs should be collapsed: "d/résumé.txt" = 12 chars.
        let result = collapse_path("données/résumé.txt", 12.0, 1.0);
        let text: String = result.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(text, "d/résumé.txt");
        assert!(result[0].1); // "d" is dimmed
    }
}

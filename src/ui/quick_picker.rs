use crate::app::AppState;
use crate::domain::file_pair::FileChangeKind;
use eframe::egui;

use super::common::kind_symbol_colored;

const SCRIM_COLOR: egui::Color32 = egui::Color32::from_black_alpha(128);

/// State for the quick picker overlay.
pub struct QuickPicker {
    open: bool,
    query: String,
    /// Index into the filtered results that is currently highlighted.
    selected_idx: usize,
    /// Cached file name strings (computed once when picker opens).
    cached_names: Vec<(usize, String, FileChangeKind)>,
    /// Set to true when selection changes; consumed after one scroll.
    scroll_to_selected: bool,
    /// Set to true on the first frame after opening; triggers a sizing pass on the Area
    /// so that egui discards the stale (shrunken) cached size.
    needs_sizing_pass: bool,
    /// Actual scroll area content height from the previous frame.
    prev_content_height: f32,
    /// The file that was selected before the picker opened (for restore on Escape).
    original_file: Option<usize>,
    /// Saved scroll state for restore on cancel.
    original_scroll_y: f32,
    original_scroll_x: f32,
    /// Cached filtered results: (index into cached_names, score, matched positions).
    cached_filtered: Vec<(usize, i64, Vec<usize>)>,
    /// Query string from when cached_filtered was last computed.
    cached_filter_query: Option<String>,
}

impl QuickPicker {
    pub fn new() -> Self {
        Self {
            open: false,
            query: String::new(),
            selected_idx: 0,
            cached_names: Vec::new(),
            scroll_to_selected: false,
            needs_sizing_pass: false,
            prev_content_height: 0.0,
            original_file: None,
            original_scroll_y: 0.0,
            original_scroll_x: 0.0,
            cached_filtered: Vec::new(),
            cached_filter_query: None,
        }
    }

    pub fn toggle(&mut self, current_file: usize, scroll_y: f32, scroll_x: f32) -> Option<usize> {
        self.open = !self.open;
        if self.open {
            self.query.clear();
            self.selected_idx = 0;
            self.cached_names.clear();
            self.needs_sizing_pass = true;
            self.original_file = Some(current_file);
            self.original_scroll_y = scroll_y;
            self.original_scroll_x = scroll_x;
            None
        } else {
            // Closing — return original file for restore.
            let orig = self.original_file.take();
            self.query.clear();
            self.selected_idx = 0;
            self.cached_names.clear();
            orig
        }
    }

    /// Open the picker with ":" pre-filled for goto-line mode.
    pub fn open_goto_line(&mut self, current_file: usize, scroll_y: f32, scroll_x: f32) {
        if !self.open {
            self.open = true;
            self.query = ":".to_string();
            self.selected_idx = 0;
            self.cached_names.clear();
            self.needs_sizing_pass = true;
            self.original_file = Some(current_file);
            self.original_scroll_y = scroll_y;
            self.original_scroll_x = scroll_x;
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.selected_idx = 0;
        self.cached_names.clear();
        self.cached_filtered.clear();
        self.cached_filter_query = None;
        self.original_file = None;
    }

    /// Saved scroll Y from before the picker opened.
    pub fn saved_scroll_y(&self) -> f32 {
        self.original_scroll_y
    }

    /// Saved scroll X from before the picker opened.
    pub fn saved_scroll_x(&self) -> f32 {
        self.original_scroll_x
    }
}

/// Render the quick picker overlay. Returns true if the picker consumed input this frame.
pub fn show(ctx: &egui::Context, state: &mut AppState, picker: &mut QuickPicker) -> bool {
    if !picker.open {
        return false;
    }

    // Build file name cache on first frame after opening.
    if picker.cached_names.is_empty() && !state.file_pairs.is_empty() {
        picker.cached_names = state
            .file_pairs
            .iter()
            .enumerate()
            .filter(|(i, _)| !state.is_file_excluded(*i))
            .map(|(i, fp)| (i, fp.relative_path.to_string_lossy().into_owned(), fp.kind))
            .collect();
        // Start with the current file highlighted.
        if let Some(orig) = picker.original_file
            && let Some(pos) = picker.cached_names.iter().position(|(i, _, _)| *i == orig)
        {
            picker.selected_idx = pos;
            picker.scroll_to_selected = true;
        }
    }

    // Take ownership of cached_names temporarily to avoid borrow conflicts.
    let file_names = std::mem::take(&mut picker.cached_names);

    // Parse query: split into fuzzy part and optional line number on last ':'.
    let (fuzzy_query, goto_line) = parse_goto_line(&picker.query);
    let fuzzy_query = fuzzy_query.to_string();
    let ends_with_colon = picker.query.ends_with(':');

    // Recompute filtered results only when the query changes.
    if picker.cached_filter_query.as_deref() != Some(&picker.query) {
        picker.cached_filter_query = Some(picker.query.clone());
        picker.cached_filtered =
            if fuzzy_query.is_empty() && (goto_line.is_some() || ends_with_colon) {
                // Goto-line mode (":N" or bare ":") — don't show file list.
                Vec::new()
            } else if fuzzy_query.is_empty() {
                file_names
                    .iter()
                    .enumerate()
                    .map(|(idx, _)| (idx, 0i64, Vec::new()))
                    .collect()
            } else {
                let mut scored: Vec<_> = file_names
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, (_, name, _))| {
                        fuzzy_match(&fuzzy_query, name).map(|m| (idx, m.score, m.positions))
                    })
                    .collect();
                scored.sort_by_key(|a| std::cmp::Reverse(a.1));
                scored
            };
    }

    let filtered = std::mem::take(&mut picker.cached_filtered);

    // Clamp selected index.
    if !filtered.is_empty() {
        picker.selected_idx = picker.selected_idx.min(filtered.len() - 1);
    }

    // Handle keyboard input for the picker (non-text keys only).
    let mut cancelled = false;
    let mut confirmed = false;
    let mut chosen_file: Option<usize> = None;
    let mut chosen_line: Option<usize> = goto_line;

    ctx.input(|input| {
        for event in &input.events {
            match event {
                egui::Event::Key {
                    key: egui::Key::Escape,
                    pressed: true,
                    ..
                } => {
                    cancelled = true;
                }
                egui::Event::Key {
                    key: egui::Key::Enter,
                    pressed: true,
                    ..
                } => {
                    if fuzzy_query.is_empty() && goto_line.is_some() {
                        // ":N" — goto line in current file, no file switch.
                        chosen_file = None;
                    } else if let Some((names_idx, _, _)) = filtered.get(picker.selected_idx) {
                        chosen_file = Some(file_names[*names_idx].0);
                    } else {
                        // No results — clear line target too.
                        chosen_line = None;
                    }
                    confirmed = true;
                }
                egui::Event::Key {
                    key: egui::Key::ArrowUp,
                    pressed: true,
                    ..
                } => {
                    picker.selected_idx = picker.selected_idx.saturating_sub(1);
                    picker.scroll_to_selected = true;
                }
                egui::Event::Key {
                    key: egui::Key::ArrowDown,
                    pressed: true,
                    ..
                } if !filtered.is_empty() => {
                    picker.selected_idx = (picker.selected_idx + 1).min(filtered.len() - 1);
                    picker.scroll_to_selected = true;
                }
                _ => {}
            }
        }
    });

    if cancelled {
        // Restore original file selection and scroll position.
        if let Some(orig) = picker.original_file {
            state.selected_file = orig;
            state.scroll.y = picker.original_scroll_y;
            state.scroll.x = picker.original_scroll_x;
            state.scroll.vy = 0.0;
        }
        picker.close();
        return true;
    }
    if confirmed {
        if let Some(idx) = chosen_file {
            state.select_file(idx);
        }
        // Goto line: navigate to the target line number.
        if let Some(line) = chosen_line {
            goto_line_in_current_file(state, line);
        }
        picker.close();
        return true;
    }

    // Live preview: switch to highlighted file without resetting scroll.
    // Don't force diff computation — let the background thread handle it.
    // The diff view will show a loading indicator if not cached yet.
    if let Some((names_idx, _, _)) = filtered.get(picker.selected_idx) {
        let file_idx = file_names[*names_idx].0;
        if state.selected_file != file_idx {
            state.selected_file = file_idx;
        }
    }

    // Draw semi-transparent scrim backdrop.
    let screen_rect = ctx.content_rect();
    egui::Area::new(egui::Id::new("picker_scrim"))
        .anchor(egui::Align2::LEFT_TOP, [0.0, 0.0])
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            ui.painter().rect_filled(screen_rect, 0.0, SCRIM_COLOR);
        });

    // Render the overlay.
    let screen_rect = ctx.content_rect();
    let max_picker_height = (screen_rect.height() * 0.7).max(200.0);

    let sizing_pass = picker.needs_sizing_pass;
    picker.needs_sizing_pass = false;
    let prev_query = picker.query.clone();

    egui::Area::new(egui::Id::new("quick_picker"))
        .anchor(egui::Align2::CENTER_TOP, [0.0, 80.0])
        .order(egui::Order::Foreground)
        .sizing_pass(sizing_pass)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .fill(egui::Color32::from_rgb(0x25, 0x25, 0x26))
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_min_width(550.0);
                    ui.set_max_width(700.0);
                    ui.spacing_mut().item_spacing.y = 10.0;

                    // Search input with styled container.
                    egui::Frame::NONE
                        .fill(egui::Color32::from_rgb(0x3C, 0x3C, 0x3C))
                        .stroke(egui::Stroke::new(
                            1.0,
                            egui::Color32::from_rgb(0x55, 0x55, 0x55),
                        ))
                        .corner_radius(4.0)
                        .inner_margin(egui::Margin::symmetric(8, 4))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.horizontal(|ui| {
                                let is_goto_line_mode = picker.query.starts_with(':');
                                let label = if is_goto_line_mode {
                                    "Go to line:"
                                } else {
                                    "Go to file:"
                                };
                                ui.label(
                                    egui::RichText::new(label)
                                        .color(egui::Color32::from_gray(0x8B)),
                                );
                                let text_edit = egui::TextEdit::singleline(&mut picker.query)
                                    .frame(egui::Frame::NONE)
                                    .text_color(egui::Color32::from_gray(0xE0))
                                    .font(egui::TextStyle::Monospace)
                                    .hint_text(
                                        egui::RichText::new("type to filter...")
                                            .color(egui::Color32::from_gray(0x6E)),
                                    )
                                    .desired_width(ui.available_width())
                                    .cursor_at_end(true);
                                let response = ui.add(text_edit);
                                // Auto-focus on first frame.
                                if sizing_pass {
                                    response.request_focus();
                                }
                            });
                        });

                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(4.0);

                    // File list inside a scroll area.
                    let selection_bg = egui::Color32::from_rgb(0x26, 0x4F, 0x78);
                    let hover_bg = egui::Color32::from_rgb(0x2A, 0x2D, 0x2E);
                    let row_margin = egui::Margin::symmetric(10, 3);

                    // Reserve height for the header (search + separators ~ 60px).
                    let list_max_height = (max_picker_height - 60.0).max(100.0);
                    // Use the content height measured from the previous frame to set
                    // a min_height, preventing the Area from caching a stale smaller size.
                    let min_h = picker.prev_content_height.min(list_max_height);

                    let scroll_id = ui.id().with("picker_scroll");
                    let scroll_output = egui::ScrollArea::vertical()
                        .id_salt(scroll_id)
                        .max_height(list_max_height)
                        .min_scrolled_height(min_h)
                        .show(ui, |ui| {
                            for (vis_idx, (names_idx, _score, match_positions)) in
                                filtered.iter().enumerate()
                            {
                                let (file_idx, ref name, ref kind) = file_names[*names_idx];
                                let is_selected = vis_idx == picker.selected_idx;

                                // Split into filename and parent directory.
                                let path = std::path::Path::new(name);
                                let filename = path
                                    .file_name()
                                    .map(|f| f.to_string_lossy())
                                    .unwrap_or_default();
                                let parent = path
                                    .parent()
                                    .filter(|p| *p != std::path::Path::new(""))
                                    .map(|p| format!("{}/", p.display()));

                                let row_id = ui.id().with(("picker_row", vis_idx));

                                // Check if this row was hovered last frame using egui memory.
                                let was_hovered = ui.ctx().memory(|mem| {
                                    mem.data.get_temp::<bool>(row_id).unwrap_or(false)
                                });

                                let bg = if is_selected {
                                    selection_bg
                                } else if was_hovered {
                                    hover_bg
                                } else {
                                    egui::Color32::TRANSPARENT
                                };

                                // Use Frame::fill to paint background BEFORE content.
                                let available_width = ui.available_width();
                                let row_response = egui::Frame::NONE
                                    .fill(bg)
                                    .corner_radius(4.0)
                                    .inner_margin(row_margin)
                                    .show(ui, |ui| {
                                        ui.set_min_width(available_width - row_margin.sum().x);
                                        ui.horizontal(|ui| {
                                            let (symbol, color) = kind_symbol_colored(*kind);
                                            ui.label(
                                                egui::RichText::new(symbol)
                                                    .color(color)
                                                    .strong()
                                                    .monospace(),
                                            );

                                            ui.vertical(|ui| {
                                                let text_color = if is_selected {
                                                    egui::Color32::from_gray(0xFF)
                                                } else {
                                                    egui::Color32::from_gray(0xCC)
                                                };
                                                let highlight_color =
                                                    egui::Color32::from_rgb(0xFF, 0xD7, 0x00); // gold
                                                // Filename byte offset within the full path.
                                                let fname_offset = name.len() - filename.len();
                                                let job = highlighted_layout(
                                                    filename.as_ref(),
                                                    match_positions,
                                                    fname_offset,
                                                    14.0,
                                                    text_color,
                                                    highlight_color,
                                                );
                                                ui.label(job);
                                                if let Some(parent_str) = &parent {
                                                    let dir_color = if is_selected {
                                                        egui::Color32::from_gray(0xBB)
                                                    } else {
                                                        egui::Color32::from_gray(0x78)
                                                    };
                                                    let dir_highlight =
                                                        egui::Color32::from_rgb(0xCC, 0xAA, 0x00);
                                                    let job = highlighted_layout(
                                                        parent_str,
                                                        match_positions,
                                                        0, // parent starts at byte 0 of the full path
                                                        10.0,
                                                        dir_color,
                                                        dir_highlight,
                                                    );
                                                    ui.label(job);
                                                } else {
                                                    // Empty spacer to keep row height uniform.
                                                    ui.label(
                                                        egui::RichText::new(" ")
                                                            .monospace()
                                                            .size(10.0),
                                                    );
                                                }
                                            });
                                        });
                                    });

                                // Store hover state for next frame.
                                let row_rect = row_response.response.rect;

                                // Auto-scroll to keep selected row visible (only when selection changed).
                                if is_selected && picker.scroll_to_selected {
                                    ui.scroll_to_rect(row_rect, Some(egui::Align::Center));
                                }

                                let interact = ui.interact(row_rect, row_id, egui::Sense::click());
                                let is_hovered = interact.hovered();
                                ui.ctx().memory_mut(|mem| {
                                    mem.data.insert_temp(row_id, is_hovered);
                                });

                                if interact.clicked() {
                                    state.select_file(file_idx);
                                    picker.close();
                                }
                            }

                            if filtered.is_empty() {
                                let hint = if goto_line.is_some() {
                                    "Press Enter to go to line"
                                } else if ends_with_colon {
                                    "Type a line number..."
                                } else {
                                    "No matches"
                                };
                                ui.label(
                                    egui::RichText::new(hint).color(egui::Color32::from_gray(0x8B)),
                                );
                            }
                        }); // end ScrollArea

                    // Store actual content height for next frame.
                    picker.prev_content_height = scroll_output.content_size.y;

                    // Consume the scroll flag after rendering.
                    picker.scroll_to_selected = false;
                });
        });

    // Restore cached data for next frame.
    picker.cached_names = file_names;
    picker.cached_filtered = filtered;

    // If the query changed (via TextEdit), reset selection to top.
    if picker.query != prev_query {
        picker.selected_idx = 0;
        picker.scroll_to_selected = true;
    }

    true
}

/// Result of a fuzzy match: score and matched character positions in the target.
pub struct FuzzyMatch {
    pub score: i64,
    /// Byte-offset positions of matched characters in the original target string.
    pub positions: Vec<usize>,
}

/// Fuzzy matching: all characters in `query` must appear in `target` in order.
/// Returns a `FuzzyMatch` with score and matched positions, or None if no match.
///
/// Scoring heuristics:
/// - +10 for consecutive matched characters
/// - +8 for matching at a word boundary (after `/`, `_`, `.`, or camelCase transition)
/// - +1 per matched character
/// - -1 per gap character between matches (gap penalty)
/// - -target.len()/4 length penalty (shorter targets preferred)
/// - +15 bonus for matches in the basename (filename portion after last `/`)
pub fn fuzzy_match(query: &str, target: &str) -> Option<FuzzyMatch> {
    if query.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            positions: Vec::new(),
        });
    }

    let query_lower = query.to_lowercase();
    let target_lower = target.to_lowercase();
    let target_bytes = target.as_bytes();

    let mut query_chars = query_lower.chars().peekable();
    let mut score: i64 = 0;
    let mut last_match_pos: Option<usize> = None;
    let mut positions = Vec::with_capacity(query.len());

    // Find basename start (after last '/').
    let basename_start = target.rfind('/').map_or(0, |p| p + 1);

    // Track byte offset alongside char position.
    let mut byte_offset = 0usize;
    for (pos, ch) in target_lower.chars().enumerate() {
        let ch_len = ch.len_utf8();
        if query_chars.peek() == Some(&ch) {
            query_chars.next();

            // Gap penalty: penalize distance between matched characters.
            if let Some(prev) = last_match_pos {
                let gap = pos - prev - 1;
                if gap > 0 {
                    #[allow(clippy::cast_possible_wrap)]
                    {
                        score -= gap as i64;
                    }
                }
                // Consecutive match bonus.
                if gap == 0 {
                    score += 10;
                }
            }

            // Word-boundary bonus: start of target, after '/', '_', '.', or camelCase.
            let is_boundary = pos == 0
                || matches!(
                    target_bytes.get(pos.wrapping_sub(1)),
                    Some(b'/' | b'_' | b'.')
                )
                || (pos > 0
                    && target_bytes[pos].is_ascii_uppercase()
                    && target_bytes[pos - 1].is_ascii_lowercase());
            if is_boundary {
                score += 8;
            }

            // Basename bonus: matches in the filename are more relevant.
            if pos >= basename_start {
                score += 15;
            }

            positions.push(byte_offset);
            last_match_pos = Some(pos);
            score += 1;
        }
        byte_offset += ch_len;
    }

    // All query chars must be consumed.
    if query_chars.peek().is_some() {
        return None;
    }

    // Target-length penalty: prefer shorter, more precise matches.
    #[allow(clippy::cast_possible_wrap)]
    {
        score -= (target.len() as i64) / 4;
    }

    Some(FuzzyMatch { score, positions })
}

/// Convenience wrapper that returns just the score (used in tests).
#[cfg(test)]
pub fn fuzzy_score(query: &str, target: &str) -> Option<i64> {
    fuzzy_match(query, target).map(|m| m.score)
}

/// Build an egui `LayoutJob` for a string with certain byte positions highlighted.
fn highlighted_layout(
    text: &str,
    positions: &[usize],
    base_offset: usize,
    font_size: f32,
    normal_color: egui::Color32,
    highlight_color: egui::Color32,
) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, LayoutSection};
    use egui::{FontId, TextFormat};

    let mut job = LayoutJob {
        text: text.to_string(),
        ..Default::default()
    };

    let normal_fmt = TextFormat {
        font_id: FontId::monospace(font_size),
        color: normal_color,
        ..Default::default()
    };
    let highlight_fmt = TextFormat {
        font_id: FontId::monospace(font_size),
        color: highlight_color,
        ..Default::default()
    };

    // Build a set of byte positions within this text that should be highlighted.
    let text_end = base_offset + text.len();
    let matched: std::collections::HashSet<usize> = positions
        .iter()
        .filter(|&&p| p >= base_offset && p < text_end)
        .map(|&p| p - base_offset)
        .collect();

    // Walk through the text, grouping consecutive chars with same highlight state.
    let mut i = 0;
    while i < text.len() {
        let is_match = matched.contains(&i);
        let start = i;
        // Find the extent of this char (UTF-8).
        let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
        i += ch_len;
        // Extend while the next char has the same state.
        while i < text.len() {
            let next_match = matched.contains(&i);
            if next_match != is_match {
                break;
            }
            let next_len = text[i..].chars().next().map_or(1, char::len_utf8);
            i += next_len;
        }
        job.sections.push(LayoutSection {
            leading_space: 0.0,
            byte_range: start..i,
            format: if is_match {
                highlight_fmt.clone()
            } else {
                normal_fmt.clone()
            },
        });
    }

    if job.sections.is_empty() {
        // Empty text fallback.
        job.sections.push(LayoutSection {
            leading_space: 0.0,
            byte_range: 0..0,
            format: normal_fmt,
        });
    }

    job
}

/// Parse a picker query into a fuzzy search part and an optional goto-line number.
/// Splits on the last ':' — e.g., "foo:22" → ("foo", Some(22)), ":22" → ("", Some(22)).
fn parse_goto_line(query: &str) -> (&str, Option<usize>) {
    if let Some(colon_pos) = query.rfind(':') {
        let after = &query[colon_pos + 1..];
        if after.is_empty() {
            // Trailing ':' with nothing after — treat as fuzzy query without the colon.
            // Return a special marker: fuzzy part is before ':', line is None.
            return (&query[..colon_pos], None);
        }
        if let Ok(line) = after.parse::<usize>()
            && line > 0
        {
            return (&query[..colon_pos], Some(line));
        }
        // After ':' is not a valid line number — treat whole query as fuzzy.
    }
    (query, None)
}

/// Navigate to a specific 1-based line number in the current file.
/// Expands folds if needed and scrolls to the target line.
fn goto_line_in_current_file(state: &mut AppState, line: usize) {
    let selected = state.selected_file;
    let Some(diff_data) = state.diff_cache.get_mut(&selected) else {
        // Diff not ready yet — defer the goto until the background thread delivers it.
        state.scroll.pending_goto_line = Some(line);
        return;
    };

    let data_idx = if let Some(idx) = diff_data.line_to_data_row(line) {
        idx
    } else {
        // Line out of range — clamp to last data row.
        if diff_data.aligned_rows.is_empty() {
            return;
        }
        diff_data.aligned_rows.len() - 1
    };

    // Expose the row if it's inside a fold.
    diff_data.fold_state.expose_data_row(data_idx);
    diff_data.ensure_unified_offsets_if_needed(state.diff_mode);

    // Map to view row and scroll (centered).
    if let Some(view_row) = diff_data
        .fold_state
        .data_to_view_row_for_mode(data_idx, state.diff_mode)
    {
        state.scroll.pending_center_row = Some(view_row);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_fuzzy_exact_match() {
        assert!(fuzzy_score("main.rs", "src/main.rs").is_some());
    }

    #[test]
    fn test_fuzzy_partial_match() {
        assert!(fuzzy_score("mr", "src/main.rs").is_some());
    }

    #[test]
    fn test_fuzzy_no_match() {
        assert!(fuzzy_score("xyz", "src/main.rs").is_none());
    }

    #[test]
    fn test_fuzzy_case_insensitive() {
        assert!(fuzzy_score("MAIN", "src/main.rs").is_some());
    }

    #[test]
    fn test_fuzzy_consecutive_bonus() {
        let score_consecutive = fuzzy_score("main", "main.rs").unwrap();
        let score_spread = fuzzy_score("main", "m_a_i_n.rs").unwrap();
        assert!(score_consecutive > score_spread);
    }

    #[test]
    fn test_fuzzy_path_segment_bonus() {
        let score_start = fuzzy_score("m", "main.rs").unwrap();
        let score_mid = fuzzy_score("m", "aam.rs").unwrap();
        assert!(score_start > score_mid);
    }

    #[test]
    fn test_fuzzy_empty_query_matches_all() {
        assert!(fuzzy_score("", "anything.rs").is_some());
    }

    #[test]
    fn test_fuzzy_unicode_chars() {
        // Unicode filenames should match correctly.
        assert!(fuzzy_score("café", "café.rs").is_some());
        assert!(fuzzy_score("日本", "日本語.txt").is_some());
        // Partial match across Unicode chars.
        assert!(fuzzy_score("über", "überall.rs").is_some());
        // Non-matching Unicode.
        assert!(fuzzy_score("αβ", "gamma.rs").is_none());
    }

    #[test]
    fn test_parse_goto_line_plain_query() {
        assert_eq!(parse_goto_line("foo"), ("foo", None));
    }

    #[test]
    fn test_parse_goto_line_with_line() {
        assert_eq!(parse_goto_line("foo:22"), ("foo", Some(22)));
    }

    #[test]
    fn test_parse_goto_line_only_line() {
        assert_eq!(parse_goto_line(":42"), ("", Some(42)));
    }

    #[test]
    fn test_parse_goto_line_zero_ignored() {
        assert_eq!(parse_goto_line(":0"), (":0", None));
    }

    #[test]
    fn test_parse_goto_line_invalid_number() {
        assert_eq!(parse_goto_line("foo:bar"), ("foo:bar", None));
    }

    #[test]
    fn test_parse_goto_line_path_with_colon() {
        // "some/path:with:colons:10" → fuzzy = "some/path:with:colons", line = 10
        assert_eq!(
            parse_goto_line("some/path:with:colons:10"),
            ("some/path:with:colons", Some(10))
        );
    }

    #[test]
    fn test_parse_goto_line_empty() {
        assert_eq!(parse_goto_line(""), ("", None));
    }

    #[test]
    fn test_parse_goto_line_trailing_colon() {
        // "foo:" → fuzzy = "foo", no line number.
        assert_eq!(parse_goto_line("foo:"), ("foo", None));
    }

    #[test]
    fn test_parse_goto_line_just_colon() {
        // ":" → fuzzy = "", no line number.
        assert_eq!(parse_goto_line(":"), ("", None));
    }

    // --- Fuzzy scoring heuristic tests ---

    #[test]
    fn test_fuzzy_gap_penalty() {
        // Tight match should beat scattered match.
        let tight = fuzzy_score("ab", "ab.rs").unwrap();
        let scattered = fuzzy_score("ab", "a_____b.rs").unwrap();
        assert!(tight > scattered, "tight={tight} scattered={scattered}");
    }

    #[test]
    fn test_fuzzy_word_boundary_bonus() {
        // 'p' at word boundary (after '_') should score better.
        let boundary = fuzzy_score("fp", "file_pair.rs").unwrap();
        let mid_word = fuzzy_score("fp", "fxpair.rs").unwrap();
        assert!(
            boundary > mid_word,
            "boundary={boundary} mid_word={mid_word}"
        );
    }

    #[test]
    fn test_fuzzy_shorter_target_preferred() {
        // Same match quality, shorter target wins.
        let short = fuzzy_score("mod", "mod.rs").unwrap();
        let long = fuzzy_score("mod", "src/domain/mod.rs").unwrap();
        assert!(short > long, "short={short} long={long}");
    }

    #[test]
    fn test_fuzzy_basename_bonus() {
        // Match in filename should beat match in directory name.
        let basename = fuzzy_score("app", "src/app.rs").unwrap();
        let dirname = fuzzy_score("app", "app/src/index.rs").unwrap();
        assert!(basename > dirname, "basename={basename} dirname={dirname}");
    }

    #[test]
    fn test_fuzzy_dot_boundary() {
        // 'r' at dot boundary (.rs) should get bonus.
        let dot = fuzzy_score("r", "file.rs").unwrap();
        let mid = fuzzy_score("r", "filer").unwrap();
        assert!(dot > mid, "dot={dot} mid={mid}");
    }

    #[test]
    fn test_fuzzy_camel_case_boundary() {
        // Match at camelCase transition.
        let camel = fuzzy_score("fb", "fooBar.rs").unwrap();
        let no_camel = fuzzy_score("fb", "foobr.rs").unwrap();
        assert!(camel > no_camel, "camel={camel} no_camel={no_camel}");
    }
}

use crate::app::AppState;
use eframe::egui;

use super::common::{UI_FONT_RATIO, icon_chevron_down, icon_chevron_right, ns_label};

/// Color for the match count label.
const MATCH_COUNT_COLOR: egui::Color32 = egui::Color32::from_rgb(0x8B, 0x94, 0x9E);
/// Color for line numbers in search results.
const LINE_NUM_COLOR: egui::Color32 = egui::Color32::from_rgb(0x68, 0x68, 0x68);
/// Color for matched text in previews.
const MATCH_TEXT_COLOR: egui::Color32 = egui::Color32::from_rgb(0xFF, 0xD7, 0x00);
/// Background color for the selected result row.
const SELECTED_BG: egui::Color32 = egui::Color32::from_rgb(0x26, 0x4F, 0x78);
/// Maximum characters to show in a preview line.
const MAX_PREVIEW_CHARS: usize = 120;

/// Render the search sidebar panel.
pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    let sidebar_font_size = (state.settings.font.size * UI_FONT_RATIO).round();
    let row_height = sidebar_font_size + 7.0;

    ui.add_space(8.0);

    // Search input + match count.
    let mut query_changed = false;
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        let response = ui.add(
            egui::TextEdit::singleline(&mut state.search.query)
                .desired_width(ui.available_width() - 60.0)
                .hint_text("Search...")
                .font(egui::FontId::monospace(sidebar_font_size)),
        );

        // Auto-focus when search panel is first opened.
        if state.search.needs_focus {
            response.request_focus();
            state.search.needs_focus = false;
        }

        // Escape closes search. Re-request focus if Enter surrendered it,
        // but not if the picker just opened (it should steal focus).
        if (response.has_focus() || response.lost_focus()) && !state.picker_open {
            let escape_pressed = ui.input(|i| {
                i.events.iter().any(|e| {
                    matches!(
                        e,
                        egui::Event::Key {
                            key: egui::Key::Escape,
                            pressed: true,
                            ..
                        }
                    )
                })
            });

            if escape_pressed {
                state.search.open = false;
                if state.search.sidebar_was_hidden {
                    state.sidebar_visible = false;
                    state.search.sidebar_was_hidden = false;
                }
                return;
            }

            // Singleline TextEdit surrenders focus on Enter; reclaim it
            // so the user can keep typing.
            if response.lost_focus() {
                response.request_focus();
            }
        }

        if response.changed() {
            query_changed = true;
        }

        // Match count.
        let current = if state.search.has_matches() {
            state.search.current_match + 1
        } else {
            0
        };
        let total = state.search.total_matches();
        ns_label(
            ui,
            egui::RichText::new(format!("{current}/{total}"))
                .size(sidebar_font_size - 1.0)
                .color(MATCH_COUNT_COLOR),
        );
    });

    // Start debounce timer if query changed.
    if query_changed {
        state.search.mark_query_changed();
    }

    ui.add_space(4.0);
    ui.separator();
    ui.add_space(4.0);

    // Results list — read from cached display model.
    if state.search.query.is_empty() {
        ns_label(
            ui,
            egui::RichText::new("Type to search across all files")
                .size(sidebar_font_size)
                .color(MATCH_COUNT_COLOR),
        );
        return;
    }
    if state.search.cached_groups().is_empty() {
        let all_files_loaded = state.files_computed >= state.file_pairs.len();
        let msg = if state.search.is_searching() || !all_files_loaded {
            "Searching\u{2026}"
        } else {
            "No results found"
        };
        ns_label(
            ui,
            egui::RichText::new(msg)
                .size(sidebar_font_size)
                .color(MATCH_COUNT_COLOR),
        );
        return;
    }

    let current_match_idx = state.search.current_match;
    let has_matches = state.search.has_matches();
    let scroll_to_current = state.search.scroll_to_current;
    // Consume the flag so we only scroll once.
    state.search.scroll_to_current = false;

    // Now render without borrowing state.search immutably.
    let mut clicked_match: Option<usize> = None; // index into all_matches

    // Pre-compute auto-expand: when navigating to a match, expand its collapsed group.
    if scroll_to_current {
        let groups_to_expand: Vec<usize> = state
            .search
            .cached_groups()
            .iter()
            .filter(|g| {
                state.search.collapsed_groups.contains(&g.file_idx)
                    && g.rows.iter().any(|r| r.match_idx == current_match_idx)
            })
            .map(|g| g.file_idx)
            .collect();
        for idx in groups_to_expand {
            state.search.collapsed_groups.remove(&idx);
        }
    }

    let nf = state.settings.behavior.use_nerdfont_icons;
    let mut toggle_group: Option<usize> = None;

    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            // Prevent content from expanding the sidebar panel width.
            ui.set_max_width(ui.available_width());
            for group in state.search.cached_groups() {
                let is_collapsed = state.search.collapsed_groups.contains(&group.file_idx);

                // File header — full-width clickable row.
                let header_height = sidebar_font_size + 8.0;
                let (header_rect, header_response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), header_height),
                    egui::Sense::click(),
                );

                if ui.is_rect_visible(header_rect) {
                    // Hover highlight.
                    if header_response.hovered() {
                        ui.painter().rect_filled(
                            header_rect,
                            0.0,
                            egui::Color32::from_white_alpha(8),
                        );
                    }

                    let is_selected = group.file_idx == state.selected_file;
                    let file_color = if is_selected {
                        egui::Color32::WHITE
                    } else {
                        egui::Color32::from_rgb(0xCC, 0xCC, 0xCC)
                    };

                    // Collapse/expand arrow.
                    let arrow = if is_collapsed {
                        icon_chevron_right(nf)
                    } else {
                        icon_chevron_down(nf)
                    };
                    let arrow_galley = ui.painter().layout_no_wrap(
                        arrow.to_string(),
                        egui::FontId::monospace(sidebar_font_size - 1.0),
                        MATCH_COUNT_COLOR,
                    );
                    ui.painter().galley(
                        egui::pos2(
                            header_rect.min.x + 4.0,
                            header_rect.center().y - arrow_galley.size().y / 2.0,
                        ),
                        arrow_galley,
                        MATCH_COUNT_COLOR,
                    );

                    // File name.
                    let name_galley = ui.painter().layout_no_wrap(
                        group.rel_path.clone(),
                        egui::FontId::monospace(sidebar_font_size),
                        file_color,
                    );
                    ui.painter().galley(
                        egui::pos2(
                            header_rect.min.x + 18.0,
                            header_rect.center().y - name_galley.size().y / 2.0,
                        ),
                        name_galley,
                        file_color,
                    );

                    // Match count (right-aligned).
                    let count_str = format!("{}", group.match_count);
                    let count_galley = ui.painter().layout_no_wrap(
                        count_str,
                        egui::FontId::monospace(sidebar_font_size - 1.0),
                        MATCH_COUNT_COLOR,
                    );
                    ui.painter().galley(
                        egui::pos2(
                            header_rect.max.x - count_galley.size().x - 8.0,
                            header_rect.center().y - count_galley.size().y / 2.0,
                        ),
                        count_galley,
                        MATCH_COUNT_COLOR,
                    );
                }

                if header_response.clicked() {
                    toggle_group = Some(group.file_idx);
                    // Also navigate to first match if expanding or already expanded.
                    if is_collapsed {
                        // Will expand — navigate after toggle.
                    } else if let Some(idx) = group.first_match_idx {
                        clicked_match = Some(idx);
                    }
                }

                // Preview lines (only when expanded).
                if !is_collapsed {
                    for row in &group.rows {
                        let (rect, response) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), row_height),
                            egui::Sense::click(),
                        );

                        let is_current = has_matches && row.match_idx == current_match_idx;

                        // Scroll the current match into view when navigating.
                        if is_current && scroll_to_current {
                            ui.scroll_to_rect(rect, Some(egui::Align::Center));
                        }

                        if ui.is_rect_visible(rect) {
                            if is_current {
                                ui.painter().rect_filled(rect, 0.0, SELECTED_BG);
                            } else if response.hovered() {
                                ui.painter().rect_filled(
                                    rect,
                                    0.0,
                                    egui::Color32::from_rgb(0x2A, 0x2D, 0x2E),
                                );
                            }

                            // Line number.
                            let num_str = format!("{:>5}", row.line_num);
                            let num_galley = ui.painter().layout_no_wrap(
                                num_str,
                                egui::FontId::monospace(sidebar_font_size - 1.0),
                                LINE_NUM_COLOR,
                            );
                            ui.painter().galley(
                                egui::pos2(
                                    rect.min.x + 8.0,
                                    rect.center().y - num_galley.size().y / 2.0,
                                ),
                                num_galley,
                                LINE_NUM_COLOR,
                            );

                            // Line content with match highlighted.
                            let text_x = rect.min.x + 52.0;
                            let text_y = rect.center().y - sidebar_font_size / 2.0;

                            let trimmed_start =
                                row.line_text.len() - row.line_text.trim_start().len();
                            let display_byte_end =
                                byte_offset_of_nth_char(&row.line_text, MAX_PREVIEW_CHARS)
                                    .min(row.line_text.len());

                            let match_start =
                                row.byte_range.start.clamp(trimmed_start, display_byte_end);
                            let match_end =
                                row.byte_range.end.clamp(trimmed_start, display_byte_end);

                            let default_format = egui::TextFormat {
                                font_id: egui::FontId::monospace(sidebar_font_size - 1.0),
                                color: egui::Color32::from_rgb(0xCC, 0xCC, 0xCC),
                                ..Default::default()
                            };
                            let highlight_format = egui::TextFormat {
                                font_id: egui::FontId::monospace(sidebar_font_size - 1.0),
                                color: MATCH_TEXT_COLOR,
                                ..Default::default()
                            };

                            let mut job = egui::text::LayoutJob::default();
                            if match_start > trimmed_start {
                                job.append(
                                    &row.line_text[trimmed_start..match_start],
                                    0.0,
                                    default_format.clone(),
                                );
                            }
                            if match_end > match_start {
                                job.append(
                                    &row.line_text[match_start..match_end],
                                    0.0,
                                    highlight_format,
                                );
                            }
                            if match_end < display_byte_end {
                                job.append(
                                    &row.line_text[match_end..display_byte_end],
                                    0.0,
                                    default_format,
                                );
                            }

                            let galley = ui.painter().layout_job(job);
                            ui.painter().galley(
                                egui::pos2(text_x, text_y),
                                galley,
                                egui::Color32::PLACEHOLDER,
                            );
                        }

                        if response.clicked() {
                            clicked_match = Some(row.match_idx);
                        }
                    }
                }

                ui.add_space(2.0);
            }
        });

    // Process deferred toggle.
    if let Some(file_idx) = toggle_group
        && !state.search.collapsed_groups.remove(&file_idx)
    {
        state.search.collapsed_groups.insert(file_idx);
    }

    // Process deferred click.
    if let Some(match_idx) = clicked_match {
        state.search.current_match = match_idx;
        if let Some(m) = state.search.match_at(match_idx).cloned() {
            navigate_to_match(state, &m);
        }
    }
}

/// Navigate to a search match: switch file if needed, unfold, scroll.
pub fn navigate_to_match(state: &mut AppState, m: &crate::domain::search::SearchMatch) {
    if m.file_index != state.selected_file {
        state.select_file(m.file_index);
    }

    // Ensure diff data is available.
    let idx = state.selected_file;
    if !state.diff_cache.contains_key(&idx) {
        let pair = &state.file_pairs[idx];
        let (_, old, new, diff, is_binary) = crate::app::read_and_diff(pair);
        let data = if is_binary {
            crate::app::FileDiffData::binary_placeholder(
                state.settings.behavior.fold_context,
                state.settings.behavior.fold_expand_step,
                state.settings.behavior.fold_row_height,
            )
        } else {
            let filename = pair.relative_path.to_string_lossy();
            let old_filename = pair
                .old_relative_path
                .as_ref()
                .map_or_else(|| filename.clone(), |p| p.to_string_lossy());
            crate::app::compute_diff_from_contents_with_diff(
                old,
                new,
                Some(diff),
                &filename,
                &old_filename,
                &state.highlighter,
                &state.settings,
                false,
            )
        };
        state.diff_cache.insert(idx, data);
        state.galley_cache.borrow_mut().invalidate_file(idx);
        state.files_computed += 1;
    }

    // Auto-unfold if match is in a folded region.
    if let Some(diff_data) = state.diff_cache.get_mut(&idx) {
        diff_data.fold_state.expose_data_row(m.data_row);
        diff_data.ensure_unified_offsets_if_needed(state.diff_mode);

        // Convert data_row to view_row and set pending_center_row.
        if let Some(view_row) = diff_data
            .fold_state
            .data_to_view_row_for_mode(m.data_row, state.diff_mode)
        {
            state.scroll.pending_center_row = Some(view_row);
        }
    }
}

/// Get the byte offset of the nth char in a string.
fn byte_offset_of_nth_char(s: &str, n: usize) -> usize {
    s.char_indices()
        .nth(n)
        .map_or(s.len(), |(byte_idx, _)| byte_idx)
}

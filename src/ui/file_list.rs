use crate::app::AppState;
use crate::domain::diff::DiffStat;
use crate::domain::file_pair::FileChangeKind;
use crate::domain::file_tree::{FlatEntryKind, toggle_dir};
use eframe::egui;
use std::fmt::Write;

use super::common::{
    COLOR_ADDED, COLOR_DELETED, COLOR_PERMISSION, icon_dir_collapsed, icon_dir_expanded,
    kind_symbol_colored, ns_label,
};

const DIR_COLOR: egui::Color32 = egui::Color32::from_rgb(0x8B, 0x94, 0x9E);
const INDENT_PX: f32 = 14.0;
const ACCENT_COLOR: egui::Color32 = egui::Color32::from_rgb(0x00, 0x78, 0xD4);
/// Label text for binary files (used in stats area measurement and rendering).
const BIN_LABEL: &str = "BIN";
/// Minimum width for the sidebar panel.
pub const SIDEBAR_MIN_WIDTH: f32 = 150.0;

/// Render the file list sidebar panel.
pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    // Derive sidebar font sizes from the main font size.
    let sidebar_font_size = (state.settings.font.size * super::common::UI_FONT_RATIO).round();
    let file_row_height = sidebar_font_size + 7.0;

    // App name + version — subtle, dimmed label.
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        let brand_color = egui::Color32::from_rgb(0xA3, 0x84, 0x5B);
        ns_label(
            ui,
            egui::RichText::new(env!("CARGO_PKG_NAME"))
                .size(sidebar_font_size + 1.0)
                .color(brand_color),
        );
        ns_label(
            ui,
            egui::RichText::new(concat!(
                env!("CARGO_PKG_VERSION_MAJOR"),
                ".",
                env!("CARGO_PKG_VERSION_MINOR")
            ))
            .size(sidebar_font_size - 1.0)
            .color(brand_color.gamma_multiply(0.6)),
        );
    });
    ui.add_space(6.0);
    ui.separator();
    ui.add_space(4.0);

    let reviewed_count = state.cached_reviewed_count;
    let total_count = state.cached_visible_count;
    let total_added = state.cached_total_added;
    let total_deleted = state.cached_total_deleted;

    // Pre-measure widths using a single galley call for the monospace font.
    let reviewed_text = format!("{reviewed_count}/{total_count} reviewed");
    let mono_font = egui::FontId::monospace(sidebar_font_size);
    let char_width = ui
        .painter()
        .layout_no_wrap("0".to_string(), mono_font.clone(), egui::Color32::WHITE)
        .size()
        .x;
    let reviewed_width = reviewed_text.len() as f32 * char_width;

    // Build stat text: "+N −M" with char-count based width estimate.
    let added_text = if total_added > 0 {
        Some(format!("+{total_added}"))
    } else {
        None
    };
    let deleted_text = if total_deleted > 0 {
        Some(format!("−{total_deleted}"))
    } else {
        None
    };
    let stat_char_count = added_text.as_ref().map_or(0, String::len)
        + deleted_text.as_ref().map_or(0, |t| t.chars().count()) // "−" is multi-byte
        + usize::from(added_text.is_some() && deleted_text.is_some()); // space separator
    let stat_width = stat_char_count as f32 * char_width;

    let available = ui.available_width();
    let spacing = ui.spacing().item_spacing.x * 2.0;
    let show_stats = stat_char_count > 0 && (reviewed_width + stat_width + spacing) <= available;

    ui.horizontal(|ui| {
        ns_label(
            ui,
            egui::RichText::new(reviewed_text)
                .monospace()
                .size(sidebar_font_size)
                .color(egui::Color32::from_rgb(0x8B, 0x94, 0x9E)),
        );
        if show_stats {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(ref text) = deleted_text {
                    ns_label(
                        ui,
                        egui::RichText::new(text)
                            .monospace()
                            .size(sidebar_font_size)
                            .color(COLOR_DELETED),
                    );
                }
                if added_text.is_some() && deleted_text.is_some() {
                    ns_label(ui, egui::RichText::new(" ").size(sidebar_font_size));
                }
                if let Some(ref text) = added_text {
                    ns_label(
                        ui,
                        egui::RichText::new(text)
                            .monospace()
                            .size(sidebar_font_size)
                            .color(COLOR_ADDED),
                    );
                }
            });
        }
    });
    ui.add_space(4.0);

    // Progress bar.
    let bar_height = 3.0;
    let bar_width = ui.available_width();
    let (bar_rect, _) =
        ui.allocate_exact_size(egui::vec2(bar_width, bar_height), egui::Sense::hover());
    let bar_track = egui::Color32::from_rgb(0x3C, 0x3C, 0x3C);
    let bar_fill = egui::Color32::from_rgb(0x3D, 0x8A, 0x3D);
    ui.painter().rect_filled(bar_rect, 1.5, bar_track);
    if total_count > 0 {
        let fraction = reviewed_count as f32 / total_count as f32;
        let fill_rect =
            egui::Rect::from_min_size(bar_rect.min, egui::vec2(bar_width * fraction, bar_height));
        ui.painter().rect_filled(fill_rect, 1.5, bar_fill);
    }

    ui.add_space(4.0);
    ui.separator();
    ui.add_space(4.0);

    let selected = state.selected_file;
    let nf = state.settings.behavior.use_nerdfont_icons;

    // Use cached flat tree.
    let flat = state.flat_tree();

    // Track deferred actions.
    let mut clicked_file: Option<usize> = None;
    let mut toggled_file: Option<usize> = None;
    let mut toggle_dir_path: Option<Vec<usize>> = None;
    let mut exclude_dir_path: Option<std::path::PathBuf> = None;

    // Read pointer state once; per-row hit tests below are plain rect checks
    // instead of per-row input-lock round trips.
    let (primary_clicked, secondary_clicked, any_click, interact_pos, hover_pos) = ui.input(|i| {
        (
            i.pointer.primary_clicked(),
            i.pointer.secondary_clicked(),
            i.pointer.any_click(),
            i.pointer.interact_pos(),
            i.pointer.hover_pos(),
        )
    });
    let hit = |rect: egui::Rect, pos: Option<egui::Pos2>| pos.is_some_and(|p| rect.contains(p));

    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            for entry in flat {
                let indent = entry.depth as f32 * INDENT_PX;

                match &entry.kind {
                    FlatEntryKind::Dir {
                        name,
                        path,
                        expanded,
                        dir_path,
                    } => {
                        // Skip directories that are excluded (or nested under an excluded dir).
                        if state
                            .excluded_dirs
                            .iter()
                            .any(|ex| dir_path.starts_with(ex))
                        {
                            continue;
                        }
                        let dir_name = name.clone();
                        let available_w = ui.available_width();
                        let dir_row = ui.horizontal(|ui| {
                            ui.set_min_width(available_w);
                            ui.set_max_width(available_w);
                            ui.add_space(indent);
                            let arrow = if *expanded {
                                icon_dir_expanded(nf)
                            } else {
                                icon_dir_collapsed(nf)
                            };
                            // Arrow icon.
                            ui.add(
                                egui::Label::new(egui::RichText::new(arrow).color(DIR_COLOR))
                                    .selectable(false),
                            );
                            // Dir name — truncate in remaining space, egui shows tooltip when elided.
                            let remaining = ui.available_width();
                            let dir_label = format!("{dir_name}/");
                            ui.allocate_ui_with_layout(
                                egui::vec2(remaining, file_row_height),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(&dir_label).color(DIR_COLOR),
                                        )
                                        .selectable(false)
                                        .truncate(),
                                    );
                                },
                            );
                        });

                        // Click detection via passive pointer query (avoids stealing hover from label).
                        let row_rect = dir_row.response.rect;
                        if primary_clicked && hit(row_rect, interact_pos) {
                            toggle_dir_path = Some(path.clone());
                        }

                        // Right-click context menu. At most one popup can be
                        // open, so only build it when opening or already open.
                        let row_secondary = secondary_clicked && hit(row_rect, interact_pos);
                        let popup_id = ui.id().with(("dir_ctx", dir_path));
                        if row_secondary || egui::Popup::is_id_open(ui.ctx(), popup_id) {
                            let set_cmd = row_secondary.then_some(egui::SetOpenCommand::Bool(true));
                            egui::Popup::new(popup_id, ui.ctx().clone(), row_rect, ui.layer_id())
                                .open_memory(set_cmd)
                                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                                .layout(egui::Layout::top_down_justified(egui::Align::Min))
                                .show(|ui| {
                                    ui.set_min_width(140.0);
                                    if ui.button(format!("Exclude {dir_name}/")).clicked() {
                                        exclude_dir_path = Some(dir_path.clone());
                                        ui.close();
                                    }
                                });
                        }

                        // Hover highlight.
                        if hit(row_rect, hover_pos) {
                            ui.painter().rect_filled(
                                row_rect,
                                0.0,
                                egui::Color32::from_white_alpha(5),
                            );
                        }
                    }
                    FlatEntryKind::File { name, file_idx } => {
                        let file_idx = *file_idx;
                        // Skip excluded files.
                        if state.is_file_excluded(file_idx) {
                            continue;
                        }
                        let kind = state.file_pairs[file_idx].kind;
                        let reviewed = state
                            .review_state
                            .is_reviewed(&state.file_pairs[file_idx].relative_path);
                        let stat = state.diff_stats.get(file_idx).copied();
                        let is_selected = file_idx == selected;

                        // Reserve row space first, so we know the rect for bg painting.
                        let row_response = ui.horizontal(|ui| {
                            render_file_row_inner(
                                ui,
                                state,
                                file_idx,
                                name,
                                kind,
                                stat,
                                is_selected,
                                reviewed,
                                indent,
                                file_row_height,
                                sidebar_font_size,
                            )
                        });

                        // Entire row is clickable (except checkbox area which is handled above).
                        let row_rect = row_response.response.rect;
                        let cb_clicked = row_response.inner;

                        if cb_clicked {
                            toggled_file = Some(file_idx);
                        }

                        // Check if the pointer clicked anywhere in the row.
                        if any_click && hit(row_rect, interact_pos) && !cb_clicked {
                            clicked_file = Some(file_idx);
                        }

                        // Hover detection for highlight and tooltip.
                        let pointer_in_row = hit(row_rect, hover_pos);

                        // Highlight row on hover/selection.
                        if is_selected {
                            ui.painter().rect_filled(
                                row_rect,
                                0.0,
                                egui::Color32::from_white_alpha(10),
                            );
                        } else if pointer_in_row {
                            ui.painter().rect_filled(
                                row_rect,
                                0.0,
                                egui::Color32::from_white_alpha(5),
                            );
                        }

                        // Auto-scroll to keep selected file visible (only on file change).
                        if is_selected && state.sidebar_scroll_to_selected {
                            row_response
                                .response
                                .scroll_to_me(Some(egui::Align::Center));
                        }
                    }
                }
            }

            // Bottom padding so last item doesn't bleed into status bar.
            ui.add_space(8.0);
        });

    // Apply deferred mutations.
    if let Some(path) = toggle_dir_path {
        toggle_dir(&mut state.file_tree, &path);
        state.rebuild_flat_tree();
    }
    if let Some(idx) = toggled_file {
        let path = state.file_pairs[idx].relative_path.clone();
        state.review_state.toggle(&path);
        state.refresh_review_counts();
    }
    if let Some(idx) = clicked_file {
        state.select_file(idx);
    }
    if let Some(dir) = exclude_dir_path {
        state.exclude_dir(&dir);
    }

    // Consume the scroll-to-selected flag after this frame.
    state.sidebar_scroll_to_selected = false;
}

/// Render the inner contents of a file row in the sidebar.
///
/// Returns `true` if the review checkbox was toggled.
#[allow(clippy::too_many_arguments)]
fn render_file_row_inner(
    ui: &mut egui::Ui,
    state: &AppState,
    file_idx: usize,
    name: &str,
    kind: FileChangeKind,
    stat: Option<DiffStat>,
    is_selected: bool,
    reviewed: bool,
    indent: f32,
    row_height: f32,
    font_size: f32,
) -> bool {
    ui.set_min_width(ui.available_width());

    // Accent gutter: always reserve space, only paint for selected.
    let (rect, _) = ui.allocate_exact_size(egui::vec2(2.0, row_height), egui::Sense::hover());
    if is_selected {
        ui.painter().rect_filled(rect, 0.0, ACCENT_COLOR);
    }
    ui.add_space(2.0);
    ui.add_space(indent);

    // Review checkbox.
    let mut reviewed_mut = reviewed;
    let cb_response = ui.checkbox(&mut reviewed_mut, "");
    let cb_clicked = cb_response.changed();
    if cb_response.has_focus() {
        cb_response.surrender_focus();
    }

    // Colored change kind badge (with permission indicator).
    let has_mode_change = state.file_pairs[file_idx].mode_change().is_some();
    let perm_only = has_mode_change && stat.is_some_and(|s| s.added == 0 && s.deleted == 0);
    if perm_only {
        ui.add(
            egui::Label::new(
                egui::RichText::new("P")
                    .color(COLOR_PERMISSION)
                    .strong()
                    .monospace(),
            )
            .selectable(false),
        );
    } else {
        let (symbol, color) = kind_symbol_colored(kind);
        ui.add(
            egui::Label::new(
                egui::RichText::new(symbol)
                    .color(color)
                    .strong()
                    .monospace(),
            )
            .selectable(false),
        );
        if has_mode_change {
            ui.add(
                egui::Label::new(
                    egui::RichText::new("P")
                        .color(COLOR_PERMISSION)
                        .strong()
                        .monospace(),
                )
                .selectable(false),
            );
        }
    }

    // Filename + diff stats.
    let is_binary = state.diff_cache.get(&file_idx).is_some_and(|d| d.binary);
    let stat_text = if is_binary {
        Some(BIN_LABEL.to_string())
    } else if let Some(s) = stat {
        let mut t = String::new();
        if s.added > 0 {
            let _ = write!(t, "+{}", s.added);
        }
        if s.added > 0 && s.deleted > 0 {
            t.push(' ');
        }
        if s.deleted > 0 {
            let _ = write!(t, "-{}", s.deleted);
        }
        if t.is_empty() { None } else { Some(t) }
    } else {
        None
    };

    let stat_width = if let Some(ref t) = stat_text {
        let galley = ui.painter().layout_no_wrap(
            t.clone(),
            egui::FontId::monospace(font_size * 0.85),
            egui::Color32::WHITE,
        );
        galley.size().x + ui.spacing().item_spacing.x * 2.0
    } else {
        0.0
    };
    let name_width = (ui.available_width() - stat_width).max(30.0);

    let text = if is_selected {
        egui::RichText::new(name).strong()
    } else {
        egui::RichText::new(name)
    };
    ui.allocate_ui_with_layout(
        egui::vec2(name_width, row_height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.add(egui::Label::new(text).selectable(false).truncate());
        },
    );

    // Diff stats (right-aligned in remaining space).
    if stat_text.is_some() {
        let remaining = ui.available_width();
        if remaining > 10.0 {
            ui.allocate_ui_with_layout(
                egui::vec2(remaining, row_height),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    if is_binary {
                        render_bin_pill(ui, font_size);
                    } else if let Some(s) = stat {
                        if s.deleted > 0 {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!("-{}", s.deleted))
                                        .color(COLOR_DELETED)
                                        .small()
                                        .monospace(),
                                )
                                .selectable(false),
                            );
                        }
                        if s.added > 0 {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!("+{}", s.added))
                                        .color(COLOR_ADDED)
                                        .small()
                                        .monospace(),
                                )
                                .selectable(false),
                            );
                        }
                    }
                },
            );
        }
    }

    cb_clicked
}

/// Render the "BIN" pill badge (rounded background with white text).
fn render_bin_pill(ui: &mut egui::Ui, font_size: f32) {
    let font = egui::FontId::monospace(font_size * 0.85);
    let galley = ui
        .painter()
        .layout_no_wrap(BIN_LABEL.to_string(), font, egui::Color32::WHITE);
    let text_size = galley.size();
    let pad = egui::vec2(4.0, 2.0);
    let pill_size = text_size + pad * 2.0;
    let (pill_rect, _) = ui.allocate_exact_size(pill_size, egui::Sense::hover());
    ui.painter().rect_filled(
        pill_rect,
        egui::CornerRadius::same(3),
        egui::Color32::from_gray(0x44),
    );
    ui.painter()
        .galley(pill_rect.min + pad, galley, egui::Color32::TRANSPARENT);
}

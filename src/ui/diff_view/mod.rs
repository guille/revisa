mod header;
mod input;

use crate::app::{AppState, FileDiffData};
use crate::domain::fold::{DiffMode, FoldState, Segment, UnifiedSubRow};
use crate::domain::hunk::AlignedRow;
use crate::domain::settings::Settings;
use crate::highlight::StyledSpan;
use crate::ui::common::{
    FONT_BOLD, FONT_BOLD_ITALIC, FONT_ITALIC, icon_fold_down, icon_fold_single, icon_fold_up,
};
use eframe::egui;

/// Width of the line number gutter in pixels.
const GUTTER_WIDTH: f32 = 50.0;
/// Right-side padding for the text clip area (prevents text from touching panel edge).
const TEXT_RIGHT_PAD: f32 = 15.0;
/// Right padding for gutter line numbers (from right edge of gutter).
const GUTTER_TEXT_RIGHT_PAD: f32 = 10.0;
/// Scale factor for nerdfont icons relative to font_size.
/// Plain-text fallbacks use 1.0 (no scaling).
const ICON_SCALE: f32 = 1.4;
/// Total gutter width in unified mode (two number columns + two thin separators).
const UNIFIED_GUTTER_WIDTH: f32 = GUTTER_WIDTH * 2.0 + 2.0;
/// Brightness deltas for fold-bar hover effect (added to bg_fold RGB channels).
const FOLD_HOVER_DR: u8 = 0x08;
const FOLD_HOVER_DG: u8 = 0x0B;
const FOLD_HOVER_DB: u8 = 0x0E;

/// Derived view configuration from Settings, passed to render functions.
#[derive(Clone)]
pub struct DiffViewCtx {
    pub(super) line_height: f32,
    pub(super) font_size: f32,
    pub(super) gutter_font_size: f32,
    pub(super) nf: bool,
    // Colors
    pub(super) bg_added: egui::Color32,
    pub(super) bg_removed: egui::Color32,
    pub(super) bg_padding: egui::Color32,
    pub(super) bg_fold: egui::Color32,
    pub(super) bg_fold_hover: egui::Color32,
    pub(super) bg_header: egui::Color32,
    pub(super) fg_fold_text: egui::Color32,
    pub(super) fg_fold_line: egui::Color32,
    pub(super) fg_gutter: egui::Color32,
    pub(super) fg_gutter_separator: egui::Color32,
    pub(super) gutter_bg: egui::Color32,
    pub(super) fg_gutter_added: egui::Color32,
    pub(super) fg_gutter_removed: egui::Color32,
    pub(super) fold_row_height: usize,
    pub(super) fold_expand_step: usize,
    /// Computed height of the unified header bar (line_height + inner margin + stroke).
    pub(super) panel_header_height: f32,
    /// Horizontal scroll step in pixels (derived from font_size).
    pub(super) h_scroll_step: f32,
    /// Vertical offset to center text within a row.
    pub(super) text_y_offset: f32,
    /// Whether text_y_offset has been calibrated from actual glyph metrics.
    pub(super) text_y_calibrated: bool,
    /// Font families for style variants (regular is always FontFamily::Monospace).
    pub(super) font_bold: egui::FontFamily,
    pub(super) font_italic: egui::FontFamily,
    pub(super) font_bold_italic: egui::FontFamily,
    /// Whether bold+italic needs synthetic italic skew (true when no real bold-italic font).
    pub(super) synthetic_bold_italic: bool,
}

impl DiffViewCtx {
    pub fn from_settings(s: &Settings, fv: crate::app::FontVariants) -> Self {
        let bg_fold = s.colors.bg_fold.to_egui();
        // Derive hover color: slightly brighter than fold bg.
        let bg_fold_hover = egui::Color32::from_rgb(
            s.colors.bg_fold.r.saturating_add(FOLD_HOVER_DR),
            s.colors.bg_fold.g.saturating_add(FOLD_HOVER_DG),
            s.colors.bg_fold.b.saturating_add(FOLD_HOVER_DB),
        );
        Self {
            line_height: s.behavior.line_height,
            font_size: s.font.size,
            gutter_font_size: s.font.gutter_size,
            nf: s.behavior.use_nerdfont_icons,
            bg_added: s.colors.bg_added.to_egui(),
            bg_removed: s.colors.bg_removed.to_egui(),
            bg_padding: s.colors.bg_padding.to_egui(),
            bg_fold,
            bg_fold_hover,
            bg_header: s.colors.bg_header.to_egui(),
            fg_fold_text: s.colors.fg_fold_text.to_egui(),
            fg_fold_line: s.colors.fg_fold_line.to_egui(),
            fg_gutter: s.colors.fg_gutter.to_egui(),
            fg_gutter_separator: s.colors.fg_gutter_separator.to_egui(),
            gutter_bg: s.colors.bg_app.to_egui(),
            fg_gutter_added: s.colors.fg_gutter_added.to_egui(),
            fg_gutter_removed: s.colors.fg_gutter_removed.to_egui(),
            fold_row_height: s.behavior.fold_row_height,
            fold_expand_step: s.behavior.fold_expand_step,
            // Header height: inner content (line_height) + inner_margin (4+4) + stroke (1).
            panel_header_height: s.behavior.line_height + 9.0,
            // Horizontal scroll: ~5 characters per step.
            h_scroll_step: s.font.size * 0.6 * 5.0,
            // Center text vertically in each row (refined once from actual glyph metrics).
            text_y_offset: (s.behavior.line_height - s.font.size) / 2.0,
            text_y_calibrated: false,
            font_bold: if fv.has_bold {
                egui::FontFamily::Name(FONT_BOLD.into())
            } else {
                egui::FontFamily::Monospace
            },
            font_italic: if fv.has_italic {
                egui::FontFamily::Name(FONT_ITALIC.into())
            } else {
                egui::FontFamily::Monospace
            },
            font_bold_italic: if fv.has_bold_italic {
                egui::FontFamily::Name(FONT_BOLD_ITALIC.into())
            } else if fv.has_bold {
                // Fallback: bold without italic.
                egui::FontFamily::Name(FONT_BOLD.into())
            } else {
                egui::FontFamily::Monospace
            },
            synthetic_bold_italic: !fv.has_bold_italic && fv.has_bold,
        }
    }
}

/// Estimate the number of fully visible rows from the diff panel height.
/// Subtracts space for the panel header.
fn estimate_viewport_rows_from_height(panel_h: f32, line_height: f32, header_height: f32) -> usize {
    ((panel_h - header_height) / line_height)
        .floor()
        .clamp(1.0, 10_000.0) as usize
}

/// Render the scroll-synced diff panels.
pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    show_inner(ui, state, true);
}

/// Render the diff panels without handling input (used when picker is active).
pub fn show_no_input(ui: &mut egui::Ui, state: &mut AppState) {
    show_inner(ui, state, false);
}

fn show_inner(ui: &mut egui::Ui, state: &mut AppState, handle_input_enabled: bool) {
    let ctx = ui.ctx().clone();

    // Calibrate glyph measurement once (font doesn't change at runtime).
    if !state.diff_view_ctx.text_y_calibrated {
        let sample = ui.painter().layout_no_wrap(
            "Mg".to_string(),
            egui::FontId::monospace(state.diff_view_ctx.font_size),
            egui::Color32::WHITE,
        );
        let glyph_h = sample.size().y;
        state.diff_view_ctx.text_y_offset =
            ((state.diff_view_ctx.line_height - glyph_h) / 2.0).max(0.0);
        state.diff_view_ctx.text_y_calibrated = true;
    }

    let vctx = state.diff_view_ctx.clone();
    let line_height = vctx.line_height;

    // If diff data isn't cached yet, show a loading indicator but still handle file navigation.
    if !state.diff_cache.contains_key(&state.selected_file) {
        if handle_input_enabled {
            input::handle_input(&ctx, state, 0);
        }
        show_loading_spinner(ui);
        ctx.request_repaint();
        return;
    }

    // Apply deferred goto-line if the diff just became available.
    apply_pending_goto_line(state);

    let diff_mode = state.diff_mode;
    // NOTE: total_view_rows_for_mode ensures unified offsets are computed when in unified mode.
    // This MUST happen before handle_input, which calls resolve_unified_view_row (requires offsets).
    let total_rows = state
        .diff_cache
        .get_mut(&state.selected_file)
        .expect("selected_file always present in diff_cache")
        .total_view_rows_for_mode(diff_mode);
    // Handle keyboard and scroll input (may change selected_file).
    if handle_input_enabled {
        input::handle_input(&ctx, state, total_rows);
    }

    // Drain pending mouse wheel delta (exponential approach for smooth scrolling).
    input::drain_pending_wheel(&ctx, state);

    // Apply momentum scrolling.
    input::apply_momentum(&ctx, state, total_rows);

    // Re-check: if selected file changed during input handling and diff isn't ready, bail to loading.
    if !state.diff_cache.contains_key(&state.selected_file) {
        show_loading_spinner(ui);
        ctx.request_repaint();
        return;
    }
    // Re-compute total_rows for the potentially new selected file.
    let total_rows = state
        .diff_cache
        .get_mut(&state.selected_file)
        .expect("selected file always present in diff_cache")
        .total_view_rows_for_mode(diff_mode);

    let selected = state.selected_file;

    // Get filename for panel headers.
    let filename = state.file_pairs[selected]
        .relative_path
        .to_string_lossy()
        .to_string();

    let resp = egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(state.diff_view_ctx.gutter_bg))
        .show(ui, |ui| {
            // NOTE: We mutate state inside this render closure to capture diff_rect
            // and clamp scroll from within CentralPanel, where the available rect
            // accurately reflects space after sidebar and status bar panels.
            state.diff_rect = Some(ui.available_rect_before_wrap());

            // Clamp scroll_y using the accurate rect.
            let vp_rows = state.diff_rect.map_or(30, |r| {
                estimate_viewport_rows_from_height(
                    r.height(),
                    line_height,
                    state.diff_view_ctx.panel_header_height,
                )
            });
            state.clamp_scroll_y(total_rows, vp_rows);

            // If a centered-scroll was requested (e.g. goto-line), apply it now.
            if let Some(center_row) = state.scroll.pending_center_row.take() {
                let target = center_row.saturating_sub(vp_rows / 2);
                state.scroll_to_row(target);
                state.clamp_scroll_y(total_rows, vp_rows);
            }

            // Derive scroll position.
            let scroll_y_offset = state.scroll.y % line_height;
            let scroll_x = state.scroll.x;

            // Render unified header spanning both panels (always shown, even for
            // binary/too-large/loading placeholders).
            let available = ui.available_size();
            let full_width = available.x;
            render_header_and_dispatch(ui, state, selected, &vctx, diff_mode, &filename);

            // Borrow diff data immutably for rendering.
            // If file is too large, show a centered message with a "Calculate anyway" button.
            if let Some(msg) = state.diff_cache[&selected].too_large_message.clone() {
                let is_computing = state.force_computing.contains(&selected);
                let available = ui.available_size();
                ui.allocate_ui_with_layout(
                    available,
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| {
                        ui.add_space((available.y / 2.0 - 40.0).max(0.0));
                        if is_computing {
                            ui.add(egui::Spinner::new().size(24.0));
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new("Computing diff...")
                                    .color(egui::Color32::from_gray(0x8B))
                                    .size(14.0),
                            );
                            ctx.request_repaint();
                        } else {
                            ui.label(
                                egui::RichText::new(msg)
                                    .size(16.0)
                                    .color(egui::Color32::from_gray(0xAA))
                                    .italics(),
                            );
                            ui.add_space(16.0);
                            let btn = egui::Button::new(
                                egui::RichText::new("Calculate anyway").size(14.0),
                            )
                            .corner_radius(6.0)
                            .stroke(egui::Stroke::new(1.0, egui::Color32::from_gray(0x5A)));
                            if ui.add_sized(egui::vec2(160.0, 32.0), btn).clicked() {
                                state.dispatch_force_compute(selected);
                            }
                        }
                    },
                );
                return 0.0;
            }

            if state.diff_cache[&selected].binary {
                let available = ui.available_size();
                ui.allocate_ui_with_layout(
                    available,
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| {
                        ui.add_space((available.y / 2.0 - 20.0).max(0.0));
                        ui.label(
                            egui::RichText::new("Binary file contents not shown")
                                .size(16.0)
                                .color(egui::Color32::from_gray(0xAA))
                                .italics(),
                        );
                    },
                );
                return 0.0;
            }

            let available = ui.available_size();
            // Render one extra line to cover the sub-pixel offset at top/bottom.
            let visible_lines = ((available.y - line_height) / line_height)
                .ceil()
                .clamp(0.0, 10_000.0) as usize
                + 2;

            // Re-read diff_mode after potential toggle.
            let diff_mode = state.diff_mode;
            // Recalculate total_rows in case diff_mode was toggled by header click.
            let total_rows = state
                .diff_cache
                .get_mut(&selected)
                .expect("selected file always present in diff_cache")
                .total_view_rows_for_mode(diff_mode);
            let diff_data = &state.diff_cache[&selected];
            let scroll_row = state.scroll_row();
            let scroll_start = scroll_row;
            let scroll_end = (scroll_start + visible_lines).min(total_rows);

            // Render diff content (mode-dependent).
            let search_render_map = state.search.render_map();
            let current_search_match = state.search.current().cloned();
            let search_colors = (
                state.settings.colors.bg_search_match.to_egui(),
                state.settings.colors.bg_search_match_current.to_egui(),
            );
            match diff_mode {
                DiffMode::SideBySide => {
                    let separator_width = 1.0;
                    let panel_width = ((full_width - separator_width) / 2.0).max(100.0);
                    render_diff_content(
                        ui,
                        &ContentRenderParams {
                            data: diff_data,
                            fold_state: &diff_data.fold_state,
                            scroll_start,
                            scroll_end,
                            scroll_x,
                            scroll_y_offset,
                            vctx: &vctx,
                            search_render_map,
                            current_search_match: current_search_match.clone(),
                            search_colors,
                        },
                        panel_width,
                        separator_width,
                    )
                }
                DiffMode::Unified => render_diff_content_unified(
                    ui,
                    &ContentRenderParams {
                        data: diff_data,
                        fold_state: &diff_data.fold_state,
                        scroll_start,
                        scroll_end,
                        scroll_x,
                        scroll_y_offset,
                        vctx: &vctx,
                        search_render_map,
                        current_search_match,
                        search_colors,
                    },
                    full_width,
                ),
            }
        });

    // Use measured galley widths for precise scroll clamping.
    let text_area = match diff_mode {
        DiffMode::SideBySide => {
            let panel_width = state
                .diff_rect
                .map_or(400.0, |r| ((r.width() - 1.0) / 2.0).max(100.0));
            panel_width - GUTTER_WIDTH - TEXT_RIGHT_PAD
        }
        DiffMode::Unified => {
            let fw = state.diff_rect.map_or(800.0, |r| r.width());
            fw - UNIFIED_GUTTER_WIDTH - TEXT_RIGHT_PAD
        }
    };
    let max_galley = resp.inner;
    // Allow scrolling until the longest line has TEXT_RIGHT_PAD of breathing room.
    state.scroll.max_x = (max_galley + TEXT_RIGHT_PAD - text_area).max(0.0);

    // Render horizontal scrollbar overlay when content overflows.
    if state.scroll.max_x > 0.0
        && let Some(diff_rect) = state.diff_rect
    {
        render_h_scrollbar(ui, state, diff_rect, diff_mode);
    }
}

/// Apply a deferred goto-line request if the diff data just became available.
fn apply_pending_goto_line(state: &mut AppState) {
    if let Some(line) = state.scroll.pending_goto_line.take() {
        let selected = state.selected_file;
        if let Some(diff_data) = state.diff_cache.get_mut(&selected) {
            let data_idx = diff_data
                .line_to_data_row(line)
                .unwrap_or_else(|| diff_data.aligned_rows.len().saturating_sub(1));
            diff_data.fold_state.expose_data_row(data_idx);
            diff_data.ensure_unified_offsets_if_needed(state.diff_mode);
            if let Some(view_row) = diff_data
                .fold_state
                .data_to_view_row_for_mode(data_idx, state.diff_mode)
            {
                state.scroll.pending_center_row = Some(view_row);
            }
        }
    }
}

/// Render the unified file header bar and dispatch its actions (copy, picker, mode toggle, etc).
fn render_header_and_dispatch(
    ui: &mut egui::Ui,
    state: &mut AppState,
    selected: usize,
    vctx: &DiffViewCtx,
    diff_mode: DiffMode,
    filename: &str,
) {
    let file_kind = state.file_pairs[selected].kind;
    let old_path = state.file_pairs[selected]
        .old_relative_path
        .as_ref()
        .map(|p| p.to_string_lossy().to_string());
    let editor_configured = state.settings.behavior.editor.is_some()
        || std::env::var("VISUAL").is_ok()
        || std::env::var("EDITOR").is_ok();
    let arrow = crate::ui::common::icon_rename_arrow(vctx.nf);
    let mode_change = state.file_pairs[selected]
        .mode_change()
        .map(|(a, b)| format!("{a:o}{arrow}{b:o}"));
    let mode_tooltip = state.file_pairs[selected].mode_change().map(|(old, new)| {
        use crate::domain::file_pair::format_rwx;
        let roles = ["owner", "group", "other"];
        let mut lines = Vec::new();
        for (i, role) in roles.iter().enumerate() {
            let shift = (2 - i) * 3;
            let old_bits = (old >> shift) & 0o7;
            let new_bits = (new >> shift) & 0o7;
            if old_bits != new_bits {
                lines.push(format!(
                    "{role}:  {}{arrow}{}",
                    format_rwx(old_bits),
                    format_rwx(new_bits),
                ));
            }
        }
        lines.join("\n")
    });
    let header_actions = header::render_unified_header(
        ui,
        filename,
        old_path.as_deref(),
        file_kind,
        mode_change.as_deref(),
        mode_tooltip.as_deref(),
        state.copied_at,
        vctx,
        diff_mode,
        editor_configured,
        state.search.open && state.sidebar_visible,
        state.picker_open,
        &state.settings.keybinds,
    );
    if header_actions.copy_clicked {
        ui.ctx().copy_text(filename.to_string());
        state.copied_at = Some(std::time::Instant::now());
    }
    if header_actions.picker_clicked {
        state.pending_open_picker = true;
    }
    if header_actions.search_clicked {
        state.search.open = !state.search.open;
        if state.search.open {
            state.search.needs_focus = true;
            state.search.sidebar_was_hidden = !state.sidebar_visible;
            state.sidebar_visible = true;
            if !state.search.query.is_empty() {
                state.dispatch_background_search();
            }
        } else if state.search.sidebar_was_hidden {
            state.sidebar_visible = false;
            state.search.sidebar_was_hidden = false;
        }
    }
    if header_actions.toggle_mode_clicked {
        state.toggle_diff_mode();
    }
    if header_actions.open_editor_clicked {
        state.open_in_editor();
    }
    // Consume deferred copy from keybind handler.
    if let Some(path) = state.pending_copy_path.take() {
        ui.ctx().copy_text(path);
        state.copied_at = Some(std::time::Instant::now());
    }
    // Request repaint while showing "copied" feedback.
    if state
        .copied_at
        .is_some_and(|t| t.elapsed().as_secs_f32() < 2.0)
    {
        ui.ctx().request_repaint();
    }
}

/// Height of the horizontal scrollbar track in pixels.
const H_SCROLLBAR_HEIGHT: f32 = 8.0;
/// Minimum thumb width in pixels to keep it easily clickable.
const H_SCROLLBAR_MIN_THUMB: f32 = 24.0;

/// Render a thin horizontal scrollbar at the bottom of the diff area.
fn render_h_scrollbar(
    ui: &mut egui::Ui,
    state: &mut AppState,
    diff_rect: egui::Rect,
    diff_mode: DiffMode,
) {
    let max_x = state.scroll.max_x;
    if max_x <= 0.0 {
        return;
    }

    // The scrollbar track spans the full width at the bottom of diff_rect.
    let track_left = diff_rect.min.x;
    let track_right = diff_rect.max.x;
    let track_width = track_right - track_left;
    if track_width < H_SCROLLBAR_MIN_THUMB * 2.0 {
        return;
    }

    let track_rect = egui::Rect::from_min_size(
        egui::pos2(track_left, diff_rect.max.y - H_SCROLLBAR_HEIGHT),
        egui::vec2(track_width, H_SCROLLBAR_HEIGHT),
    );

    // Compute thumb size and position.
    // The visible text area width determines the ratio (how much of total content is visible).
    let gutter_w = match diff_mode {
        DiffMode::SideBySide => GUTTER_WIDTH,
        DiffMode::Unified => UNIFIED_GUTTER_WIDTH,
    };
    let panel_text_w = match diff_mode {
        DiffMode::SideBySide => {
            let panel_w = ((track_width - 1.0) / 2.0).max(100.0);
            panel_w - gutter_w - TEXT_RIGHT_PAD
        }
        DiffMode::Unified => track_width - gutter_w - TEXT_RIGHT_PAD,
    };
    let total_content = panel_text_w + max_x;
    let thumb_ratio = (panel_text_w / total_content).clamp(0.05, 1.0);
    let thumb_width = (track_width * thumb_ratio).max(H_SCROLLBAR_MIN_THUMB);
    let scroll_ratio = if max_x > 0.0 {
        state.scroll.x / max_x
    } else {
        0.0
    };
    let thumb_max_travel = track_width - thumb_width;
    let thumb_x = track_left + scroll_ratio * thumb_max_travel;

    let thumb_rect = egui::Rect::from_min_size(
        egui::pos2(thumb_x, track_rect.min.y),
        egui::vec2(thumb_width, H_SCROLLBAR_HEIGHT),
    );

    // Interaction: check pointer state.
    let pointer_pos = ui.ctx().input(|i| i.pointer.hover_pos());
    let pointer_in_track = pointer_pos.is_some_and(|p| track_rect.contains(p));
    let pointer_in_thumb = pointer_pos.is_some_and(|p| thumb_rect.contains(p));

    // Handle drag.
    let primary_down = ui.ctx().input(|i| i.pointer.primary_down());
    let primary_pressed = ui.ctx().input(|i| i.pointer.primary_pressed());
    if state.scroll.h_scrollbar_drag {
        if primary_down {
            if let Some(pos) = pointer_pos {
                // Map pointer x to scroll position.
                let rel =
                    ((pos.x - track_left - thumb_width / 2.0) / thumb_max_travel).clamp(0.0, 1.0);
                state.scroll.x = rel * max_x;
                ui.ctx().request_repaint();
            }
        } else {
            state.scroll.h_scrollbar_drag = false;
        }
    } else if primary_pressed && pointer_in_track {
        // Click on track (or thumb): jump to position and begin drag.
        state.scroll.h_scrollbar_drag = true;
        if let Some(pos) = pointer_pos {
            let rel = ((pos.x - track_left - thumb_width / 2.0) / thumb_max_travel).clamp(0.0, 1.0);
            state.scroll.x = rel * max_x;
        }
        ui.ctx().request_repaint();
    }

    // Paint.
    let painter = ui.painter();
    let is_active = state.scroll.h_scrollbar_drag || pointer_in_track;
    let track_alpha = if is_active { 80 } else { 40 };
    let thumb_alpha = if state.scroll.h_scrollbar_drag {
        180
    } else if pointer_in_thumb {
        140
    } else if pointer_in_track {
        100
    } else {
        60
    };

    painter.rect_filled(
        track_rect,
        0.0,
        egui::Color32::from_black_alpha(track_alpha),
    );
    painter.rect_filled(
        thumb_rect,
        H_SCROLLBAR_HEIGHT / 2.0,
        egui::Color32::from_white_alpha(thumb_alpha),
    );

    // Request repaint while hovering for responsive opacity changes.
    if pointer_in_track {
        ui.ctx().request_repaint();
    }
}

#[derive(Clone, Copy)]
enum PanelSide {
    Left,
    Right,
}

/// Shared scroll/view parameters for content rendering functions.
struct ContentRenderParams<'a> {
    pub(super) data: &'a FileDiffData,
    pub(super) fold_state: &'a FoldState,
    pub(super) scroll_start: usize,
    pub(super) scroll_end: usize,
    pub(super) scroll_x: f32,
    pub(super) scroll_y_offset: f32,
    pub(super) vctx: &'a DiffViewCtx,
    /// Search matches for the current file, keyed by data_row.
    pub(super) search_render_map:
        &'a std::collections::HashMap<usize, Vec<crate::domain::search::SearchMatch>>,
    /// Currently focused search match (for highlight differentiation).
    pub(super) current_search_match: Option<crate::domain::search::SearchMatch>,
    /// Search highlight colors: (other_match, current_match).
    pub(super) search_colors: (egui::Color32, egui::Color32),
}

/// Shared painter/position parameters for row rendering functions.
struct RowRenderCtx<'a> {
    pub(super) painter: &'a egui::Painter,
    pub(super) text_painter: &'a egui::Painter,
    pub(super) left_x: f32,
    pub(super) y: f32,
    pub(super) scroll_x: f32,
    pub(super) vctx: &'a DiffViewCtx,
}

/// Shared parameters for fold row rendering.
struct FoldRowCtx<'a> {
    pub(super) painter: &'a egui::Painter,
    pub(super) left_x: f32,
    pub(super) y: f32,
    pub(super) panel_width: f32,
    pub(super) vctx: &'a DiffViewCtx,
}

/// Viewport-level parameters shared across all fold segments in a render pass.
struct FoldSegmentCtx<'a> {
    scroll_start: usize,
    view_rows_count: usize,
    top_left: egui::Pos2,
    full_width: f32,
    line_height: f32,
    scroll_y_offset: f32,
    hover_pos: Option<egui::Pos2>,
    full_painter: &'a egui::Painter,
    vctx: &'a DiffViewCtx,
    nf: bool,
}

/// Render both diff panels with spanning fold bars.
fn render_diff_content(
    ui: &mut egui::Ui,
    params: &ContentRenderParams,
    panel_width: f32,
    separator_width: f32,
) -> f32 {
    let ContentRenderParams {
        data,
        fold_state,
        scroll_start,
        scroll_end,
        scroll_x,
        scroll_y_offset,
        vctx,
        search_render_map,
        current_search_match,
        search_colors,
    } = params;
    let (scroll_start, scroll_end, scroll_x, scroll_y_offset) =
        (*scroll_start, *scroll_end, *scroll_x, *scroll_y_offset);
    let top_left = ui.cursor().min;
    let line_height = vctx.line_height;
    let nf = vctx.nf;
    let view_rows_count = scroll_end - scroll_start;
    let hover_pos = ui.ctx().input(|i| i.pointer.hover_pos());
    let mut max_galley_width: f32 = 0.0;

    let full_width = panel_width * 2.0 + separator_width;
    let right_x = top_left.x + panel_width + separator_width;

    // Clip rects for each panel.
    let content_height = (view_rows_count as f32) * line_height;

    let left_rect = egui::Rect::from_min_size(top_left, egui::vec2(panel_width, content_height));
    let right_rect = egui::Rect::from_min_size(
        egui::pos2(right_x, top_left.y),
        egui::vec2(panel_width, content_height),
    );
    let full_rect = egui::Rect::from_min_size(top_left, egui::vec2(full_width, content_height));

    let left_painter = ui.painter_at(left_rect);
    let right_painter = ui.painter_at(right_rect);
    let full_painter = ui.painter_at(full_rect);

    // Text clip rects (excludes gutter, with right padding).
    let left_text_rect = egui::Rect::from_min_size(
        egui::pos2(top_left.x + GUTTER_WIDTH, top_left.y),
        egui::vec2(panel_width - GUTTER_WIDTH - TEXT_RIGHT_PAD, content_height),
    );
    let right_text_rect = egui::Rect::from_min_size(
        egui::pos2(right_x + GUTTER_WIDTH, top_left.y),
        egui::vec2(panel_width - GUTTER_WIDTH - TEXT_RIGHT_PAD, content_height),
    );
    let left_text_painter = ui.painter_at(left_text_rect);
    let right_text_painter = ui.painter_at(right_text_rect);

    // Iterate segments once, rendering both panels and fold bars.
    let mut view_row = 0usize;
    let mut visual_idx = 0usize;

    let fold_seg_ctx = FoldSegmentCtx {
        scroll_start,
        view_rows_count,
        top_left,
        full_width,
        line_height,
        scroll_y_offset,
        hover_pos,
        full_painter: &full_painter,
        vctx,
        nf,
    };

    for seg in fold_state.segments() {
        let h = seg.height(vctx.fold_row_height);

        if view_row + h <= scroll_start {
            view_row += h;
            continue;
        }
        if view_row >= scroll_end {
            break;
        }

        match seg {
            Segment::Visible { data_range } => {
                let seg_view_start = view_row;
                let skip = scroll_start.saturating_sub(seg_view_start);
                let take = (scroll_end - seg_view_start).min(h) - skip;

                for i in 0..take {
                    let data_idx = data_range.start + skip + i;
                    let row = &data.aligned_rows[data_idx];
                    let y = top_left.y + visual_idx as f32 * line_height - scroll_y_offset;

                    // Draw separator line between panels.
                    let sep_x = top_left.x + panel_width;
                    full_painter.line_segment(
                        [egui::pos2(sep_x, y), egui::pos2(sep_x, y + line_height)],
                        egui::Stroke::new(separator_width, vctx.fg_gutter_separator),
                    );

                    // Left panel.
                    let row_matches = search_render_map
                        .get(&data_idx)
                        .map_or(&[][..], |v| v.as_slice());
                    let lw = render_data_row(
                        &RowRenderCtx {
                            painter: &left_painter,
                            text_painter: &left_text_painter,
                            left_x: top_left.x,
                            y,
                            scroll_x,
                            vctx,
                        },
                        data,
                        row,
                        data_idx,
                        &data.left_styled,
                        PanelSide::Left,
                        panel_width,
                        row_matches,
                        current_search_match.as_ref(),
                        *search_colors,
                    );
                    // Right panel.
                    let rw = render_data_row(
                        &RowRenderCtx {
                            painter: &right_painter,
                            text_painter: &right_text_painter,
                            left_x: right_x,
                            y,
                            scroll_x,
                            vctx,
                        },
                        data,
                        row,
                        data_idx,
                        &data.right_styled,
                        PanelSide::Right,
                        panel_width,
                        row_matches,
                        current_search_match.as_ref(),
                        *search_colors,
                    );
                    max_galley_width = max_galley_width.max(lw).max(rw);
                    visual_idx += 1;
                }
            }
            Segment::Fold {
                hidden_count,
                show_expand_up,
                show_expand_down,
                label,
                ..
            } => {
                render_fold_segment(
                    *hidden_count,
                    *show_expand_up,
                    *show_expand_down,
                    label,
                    view_row,
                    &mut visual_idx,
                    &fold_seg_ctx,
                );
            }
        }

        view_row += h;
    }

    // Reserve space.
    ui.allocate_space(egui::vec2(full_width, content_height));
    max_galley_width
}

/// Render unified (stacked) diff content — single panel with dual gutter.
fn render_diff_content_unified(
    ui: &mut egui::Ui,
    params: &ContentRenderParams,
    full_width: f32,
) -> f32 {
    let ContentRenderParams {
        data,
        fold_state,
        scroll_start,
        scroll_end,
        scroll_x,
        scroll_y_offset,
        vctx,
        search_render_map,
        current_search_match,
        search_colors,
    } = params;
    let (scroll_start, scroll_end, scroll_x, scroll_y_offset) =
        (*scroll_start, *scroll_end, *scroll_x, *scroll_y_offset);
    let top_left = ui.cursor().min;
    let line_height = vctx.line_height;
    let nf = vctx.nf;
    let view_rows_count = scroll_end - scroll_start;
    let hover_pos = ui.ctx().input(|i| i.pointer.hover_pos());
    let mut max_galley_width: f32 = 0.0;

    let offsets = fold_state
        .unified_offsets_ref()
        .expect("unified offsets must be computed before rendering");

    let content_height = (view_rows_count as f32) * line_height;

    let full_rect = egui::Rect::from_min_size(top_left, egui::vec2(full_width, content_height));
    let full_painter = ui.painter_at(full_rect);

    // Text clip rect (excludes dual gutter, with right padding).
    let text_rect = egui::Rect::from_min_size(
        egui::pos2(top_left.x + UNIFIED_GUTTER_WIDTH, top_left.y),
        egui::vec2(
            full_width - UNIFIED_GUTTER_WIDTH - TEXT_RIGHT_PAD,
            content_height,
        ),
    );
    let text_painter = ui.painter_at(text_rect);

    let mut view_row = 0usize;
    let mut visual_idx = 0usize;

    let fold_seg_ctx = FoldSegmentCtx {
        scroll_start,
        view_rows_count,
        top_left,
        full_width,
        line_height,
        scroll_y_offset,
        hover_pos,
        full_painter: &full_painter,
        vctx,
        nf,
    };

    for seg in fold_state.segments() {
        let seg_height = match seg {
            Segment::Visible { data_range } => offsets[data_range.end] - offsets[data_range.start],
            Segment::Fold { .. } => seg.height(vctx.fold_row_height),
        };

        if view_row + seg_height <= scroll_start {
            view_row += seg_height;
            continue;
        }
        if view_row >= scroll_end {
            break;
        }

        match seg {
            Segment::Visible { data_range } => {
                // We need to iterate data rows and expand modified ones.
                // First, skip data rows until we reach scroll_start within this segment.
                let seg_view_start = view_row;
                let mut seg_view_offset = 0usize;

                for data_idx in data_range.clone() {
                    let row = &data.aligned_rows[data_idx];
                    let row_h = if matches!(row, AlignedRow::Both { modified: true, .. }) {
                        2
                    } else {
                        1
                    };
                    let row_view_start = seg_view_start + seg_view_offset;

                    // Skip rows entirely before scroll window.
                    if row_view_start + row_h <= scroll_start {
                        seg_view_offset += row_h;
                        continue;
                    }
                    // Stop if past scroll window.
                    if row_view_start >= scroll_end {
                        break;
                    }

                    if row_h == 2 {
                        // Modified Both: render old sub-row then new sub-row.
                        let row_matches = search_render_map
                            .get(&data_idx)
                            .map_or(&[][..], |v| v.as_slice());
                        for sub_idx in 0..2usize {
                            let sub_view = row_view_start + sub_idx;
                            if sub_view < scroll_start || sub_view >= scroll_end {
                                continue;
                            }
                            let y = top_left.y + visual_idx as f32 * line_height - scroll_y_offset;
                            let sub = if sub_idx == 0 {
                                UnifiedSubRow::Old
                            } else {
                                UnifiedSubRow::New
                            };
                            let w = render_unified_data_row(
                                &RowRenderCtx {
                                    painter: &full_painter,
                                    text_painter: &text_painter,
                                    left_x: top_left.x,
                                    y,
                                    scroll_x,
                                    vctx,
                                },
                                data,
                                row,
                                data_idx,
                                sub,
                                full_width,
                                row_matches,
                                current_search_match.as_ref(),
                                *search_colors,
                            );
                            max_galley_width = max_galley_width.max(w);
                            visual_idx += 1;
                        }
                    } else {
                        // Single row: context, LeftOnly, or RightOnly.
                        let y = top_left.y + visual_idx as f32 * line_height - scroll_y_offset;
                        let sub = match row {
                            AlignedRow::LeftOnly { .. } => UnifiedSubRow::Old,
                            AlignedRow::RightOnly { .. } => UnifiedSubRow::New,
                            AlignedRow::Both { .. } => UnifiedSubRow::Single,
                        };
                        let row_matches = search_render_map
                            .get(&data_idx)
                            .map_or(&[][..], |v| v.as_slice());
                        let w = render_unified_data_row(
                            &RowRenderCtx {
                                painter: &full_painter,
                                text_painter: &text_painter,
                                left_x: top_left.x,
                                y,
                                scroll_x,
                                vctx,
                            },
                            data,
                            row,
                            data_idx,
                            sub,
                            full_width,
                            row_matches,
                            current_search_match.as_ref(),
                            *search_colors,
                        );
                        max_galley_width = max_galley_width.max(w);
                        visual_idx += 1;
                    }

                    seg_view_offset += row_h;
                }
            }
            Segment::Fold {
                hidden_count,
                show_expand_up,
                show_expand_down,
                label,
                ..
            } => {
                render_fold_segment(
                    *hidden_count,
                    *show_expand_up,
                    *show_expand_down,
                    label,
                    view_row,
                    &mut visual_idx,
                    &fold_seg_ctx,
                );
            }
        }

        view_row += seg_height;
    }

    // Reserve space.
    ui.allocate_space(egui::vec2(full_width, content_height));
    max_galley_width
}

/// Render a fold segment (expand-up / expand-down bars). Shared between SBS and unified modes.
fn render_fold_segment(
    hidden_count: usize,
    show_expand_up: bool,
    show_expand_down: bool,
    label: &str,
    seg_view_start: usize,
    visual_idx: &mut usize,
    ctx: &FoldSegmentCtx,
) {
    let skip = ctx.scroll_start.saturating_sub(seg_view_start);
    let is_single =
        show_expand_up && !show_expand_down && hidden_count <= ctx.vctx.fold_expand_step;

    let frh = ctx.vctx.fold_row_height;
    let mut local_row = 0usize;
    if show_expand_up {
        if local_row + frh > skip && *visual_idx < ctx.view_rows_count {
            let visible_offset = skip.saturating_sub(local_row);
            let y = ctx.top_left.y + *visual_idx as f32 * ctx.line_height
                - ctx.scroll_y_offset
                - visible_offset as f32 * ctx.line_height;
            let (icon, text) = if is_single {
                (icon_fold_single(ctx.nf), label)
            } else {
                (icon_fold_up(ctx.nf), label)
            };
            render_fold_row(
                &FoldRowCtx {
                    painter: ctx.full_painter,
                    left_x: ctx.top_left.x,
                    y,
                    panel_width: ctx.full_width,
                    vctx: ctx.vctx,
                },
                icon,
                text,
                ctx.hover_pos,
            );
            *visual_idx += frh.saturating_sub(visible_offset);
        }
        local_row += frh;
    }
    if show_expand_down && local_row + frh > skip && *visual_idx < ctx.view_rows_count {
        let visible_offset = skip.saturating_sub(local_row);
        let y = ctx.top_left.y + *visual_idx as f32 * ctx.line_height
            - ctx.scroll_y_offset
            - visible_offset as f32 * ctx.line_height;
        render_fold_row(
            &FoldRowCtx {
                painter: ctx.full_painter,
                left_x: ctx.top_left.x,
                y,
                panel_width: ctx.full_width,
                vctx: ctx.vctx,
            },
            icon_fold_down(ctx.nf),
            label,
            ctx.hover_pos,
        );
        *visual_idx += frh.saturating_sub(visible_offset);
    }
}

/// Render a single data row in unified mode (dual gutter + text).
#[allow(clippy::too_many_arguments)]
fn render_unified_data_row(
    rctx: &RowRenderCtx,
    data: &FileDiffData,
    row: &AlignedRow,
    data_idx: usize,
    sub: UnifiedSubRow,
    full_width: f32,
    search_matches: &[crate::domain::search::SearchMatch],
    current_search: Option<&crate::domain::search::SearchMatch>,
    search_colors: (egui::Color32, egui::Color32),
) -> f32 {
    let RowRenderCtx {
        painter,
        text_painter,
        left_x,
        y,
        scroll_x,
        vctx,
    } = rctx;
    let (left_x, y, scroll_x) = (*left_x, *y, *scroll_x);
    let line_height = vctx.line_height;

    // Background color.
    let row_bg = match (sub, row) {
        (UnifiedSubRow::Old, _) | (_, AlignedRow::LeftOnly { .. }) => vctx.bg_removed,
        (UnifiedSubRow::New, _) | (_, AlignedRow::RightOnly { .. }) => vctx.bg_added,
        _ => egui::Color32::TRANSPARENT,
    };

    // Draw dual gutter background.
    let gutter_rect = egui::Rect::from_min_size(
        egui::pos2(left_x, y),
        egui::vec2(UNIFIED_GUTTER_WIDTH, line_height),
    );
    painter.rect_filled(gutter_rect, 0.0, vctx.gutter_bg);

    // Draw row background (text area).
    if row_bg != egui::Color32::TRANSPARENT {
        let text_row_rect = egui::Rect::from_min_size(
            egui::pos2(left_x + UNIFIED_GUTTER_WIDTH, y),
            egui::vec2(full_width - UNIFIED_GUTTER_WIDTH, line_height),
        );
        painter.rect_filled(text_row_rect, 0.0, row_bg);
    }

    // Gutter separator lines (after old gutter, after new gutter).
    let old_sep_x = left_x + GUTTER_WIDTH - 1.0;
    painter.line_segment(
        [
            egui::pos2(old_sep_x, y),
            egui::pos2(old_sep_x, y + line_height),
        ],
        egui::Stroke::new(1.0, vctx.fg_gutter_separator),
    );
    let new_sep_x = left_x + UNIFIED_GUTTER_WIDTH - 1.0;
    painter.line_segment(
        [
            egui::pos2(new_sep_x, y),
            egui::pos2(new_sep_x, y + line_height),
        ],
        egui::Stroke::new(1.0, vctx.fg_gutter_separator),
    );

    // Determine gutter foreground color based on diff type.
    let gutter_fg = match (sub, row) {
        (UnifiedSubRow::Old, _) | (_, AlignedRow::LeftOnly { .. }) => vctx.fg_gutter_removed,
        (UnifiedSubRow::New, _) | (_, AlignedRow::RightOnly { .. }) => vctx.fg_gutter_added,
        _ => vctx.fg_gutter,
    };

    let is_diff_line = gutter_fg != vctx.fg_gutter;
    let gutter_font = if is_diff_line {
        egui::FontId::new(vctx.gutter_font_size, vctx.font_bold.clone())
    } else {
        egui::FontId::monospace(vctx.gutter_font_size)
    };

    // Old line number (left gutter).
    let old_num: Option<usize> = match (sub, row) {
        (UnifiedSubRow::New, _) | (_, AlignedRow::RightOnly { .. }) => None,
        (_, AlignedRow::Both { left_line, .. } | AlignedRow::LeftOnly { left_line }) => {
            Some(*left_line + 1)
        }
    };
    if let Some(num) = old_num {
        let pos = egui::pos2(
            left_x + GUTTER_WIDTH - GUTTER_TEXT_RIGHT_PAD,
            y + line_height / 2.0,
        );
        painter.text(
            pos,
            egui::Align2::RIGHT_CENTER,
            num,
            gutter_font.clone(),
            gutter_fg,
        );
    }

    // New line number (right gutter).
    let new_num: Option<usize> = match (sub, row) {
        (UnifiedSubRow::Old, _) | (_, AlignedRow::LeftOnly { .. }) => None,
        (_, AlignedRow::Both { right_line, .. } | AlignedRow::RightOnly { right_line }) => {
            Some(*right_line + 1)
        }
    };
    if let Some(num) = new_num {
        let pos = egui::pos2(
            left_x + GUTTER_WIDTH + GUTTER_WIDTH - GUTTER_TEXT_RIGHT_PAD,
            y + line_height / 2.0,
        );
        painter.text(pos, egui::Align2::RIGHT_CENTER, num, gutter_font, gutter_fg);
    }

    // Draw syntax-highlighted text.
    // Choose the correct styled spans and line text based on sub-row.
    let (styled, line_text) = match (sub, row) {
        (UnifiedSubRow::New, AlignedRow::Both { right_line, .. })
        | (_, AlignedRow::RightOnly { right_line }) => (
            &data.right_styled[data_idx],
            data.new_lines.line(*right_line),
        ),
        (_, AlignedRow::Both { left_line, .. } | AlignedRow::LeftOnly { left_line }) => {
            (&data.left_styled[data_idx], data.old_lines.line(*left_line))
        }
    };

    if styled.is_empty() {
        0.0
    } else {
        let layout_job = build_layout_job(line_text, styled, vctx.font_size, vctx);
        let galley = text_painter.layout_job(layout_job);
        let width = galley.size().x;
        let text_pos = egui::pos2(
            left_x + UNIFIED_GUTTER_WIDTH - scroll_x,
            y + vctx.text_y_offset,
        );

        text_painter.galley(
            text_pos,
            std::sync::Arc::clone(&galley),
            egui::Color32::TRANSPARENT,
        );

        // Paint search match highlights ON TOP of the galley so they overlay
        // diff background colors (green/red) instead of being hidden beneath them.
        let match_side = match sub {
            UnifiedSubRow::Old => crate::domain::search::MatchSide::Left,
            UnifiedSubRow::New | UnifiedSubRow::Single => crate::domain::search::MatchSide::Right,
        };
        paint_search_highlights(
            painter,
            &galley,
            text_pos,
            y,
            line_height,
            line_text,
            match_side,
            search_matches,
            current_search,
            search_colors,
        );

        width
    }
}

/// Render a single data row (gutter + text).
#[allow(clippy::too_many_arguments)]
fn render_data_row(
    rctx: &RowRenderCtx,
    data: &FileDiffData,
    row: &AlignedRow,
    data_idx: usize,
    styled_rows: &[Vec<StyledSpan>],
    side: PanelSide,
    panel_width: f32,
    search_matches: &[crate::domain::search::SearchMatch],
    current_search: Option<&crate::domain::search::SearchMatch>,
    search_colors: (egui::Color32, egui::Color32),
) -> f32 {
    let RowRenderCtx {
        painter,
        text_painter,
        left_x,
        y,
        scroll_x,
        vctx,
    } = rctx;
    let (left_x, y, scroll_x) = (*left_x, *y, *scroll_x);
    let line_height = vctx.line_height;
    // Determine row background color.
    let row_bg = match (side, row) {
        (
            PanelSide::Left,
            AlignedRow::Both { modified: true, .. } | AlignedRow::LeftOnly { .. },
        ) => vctx.bg_removed,
        (
            PanelSide::Right,
            AlignedRow::Both { modified: true, .. } | AlignedRow::RightOnly { .. },
        ) => vctx.bg_added,
        (
            _,
            AlignedRow::Both {
                modified: false, ..
            },
        ) => egui::Color32::TRANSPARENT,
        _ => vctx.bg_padding,
    };

    // Draw gutter background.
    let gutter_rect =
        egui::Rect::from_min_size(egui::pos2(left_x, y), egui::vec2(GUTTER_WIDTH, line_height));
    painter.rect_filled(gutter_rect, 0.0, vctx.gutter_bg);

    // Draw row background (text area only).
    if row_bg != egui::Color32::TRANSPARENT {
        let text_row_rect = egui::Rect::from_min_size(
            egui::pos2(left_x + GUTTER_WIDTH, y),
            egui::vec2(panel_width - GUTTER_WIDTH, line_height),
        );
        painter.rect_filled(text_row_rect, 0.0, row_bg);
    }

    // Draw gutter separator line.
    let sep_x = left_x + GUTTER_WIDTH - 1.0;
    painter.line_segment(
        [egui::pos2(sep_x, y), egui::pos2(sep_x, y + line_height)],
        egui::Stroke::new(1.0, vctx.fg_gutter_separator),
    );

    // Draw gutter (line number) with diff-colored foreground.
    let line_num: Option<usize> = match (side, row) {
        (
            PanelSide::Left,
            AlignedRow::Both { left_line, .. } | AlignedRow::LeftOnly { left_line },
        ) => Some(*left_line + 1),
        (
            PanelSide::Right,
            AlignedRow::Both { right_line, .. } | AlignedRow::RightOnly { right_line },
        ) => Some(*right_line + 1),
        _ => None,
    };
    let gutter_fg = match (side, row) {
        (
            PanelSide::Left,
            AlignedRow::Both { modified: true, .. } | AlignedRow::LeftOnly { .. },
        ) => vctx.fg_gutter_removed,
        (
            PanelSide::Right,
            AlignedRow::Both { modified: true, .. } | AlignedRow::RightOnly { .. },
        ) => vctx.fg_gutter_added,
        _ => vctx.fg_gutter,
    };
    let is_diff_line = gutter_fg != vctx.fg_gutter;
    let gutter_font = if is_diff_line {
        egui::FontId::new(vctx.gutter_font_size, vctx.font_bold.clone())
    } else {
        egui::FontId::monospace(vctx.gutter_font_size)
    };
    if let Some(num) = line_num {
        let gutter_text_pos = egui::pos2(
            left_x + GUTTER_WIDTH - GUTTER_TEXT_RIGHT_PAD,
            y + line_height / 2.0,
        );
        painter.text(
            gutter_text_pos,
            egui::Align2::RIGHT_CENTER,
            num,
            gutter_font,
            gutter_fg,
        );
    }

    // Draw syntax-highlighted text.
    let styled = &styled_rows[data_idx];
    if styled.is_empty() {
        0.0
    } else {
        let line_text = match (side, row) {
            (
                PanelSide::Left,
                AlignedRow::Both { left_line, .. } | AlignedRow::LeftOnly { left_line },
            ) => data.old_lines.line(*left_line),
            (
                PanelSide::Right,
                AlignedRow::Both { right_line, .. } | AlignedRow::RightOnly { right_line },
            ) => data.new_lines.line(*right_line),
            _ => "",
        };
        let layout_job = build_layout_job(line_text, styled, vctx.font_size, vctx);
        let galley = text_painter.layout_job(layout_job);
        let width = galley.size().x;
        let text_pos = egui::pos2(left_x + GUTTER_WIDTH - scroll_x, y + vctx.text_y_offset);

        text_painter.galley(
            text_pos,
            std::sync::Arc::clone(&galley),
            egui::Color32::TRANSPARENT,
        );

        // Paint search match highlights ON TOP of the galley.
        let match_side = match side {
            PanelSide::Left => crate::domain::search::MatchSide::Left,
            PanelSide::Right => crate::domain::search::MatchSide::Right,
        };
        paint_search_highlights(
            painter,
            &galley,
            text_pos,
            y,
            line_height,
            line_text,
            match_side,
            search_matches,
            current_search,
            search_colors,
        );
        width
    }
}

/// Paint search match highlight rectangles on top of a text galley.
#[allow(clippy::too_many_arguments)]
fn paint_search_highlights(
    painter: &egui::Painter,
    galley: &std::sync::Arc<egui::Galley>,
    text_pos: egui::Pos2,
    y: f32,
    line_height: f32,
    line_text: &str,
    match_side: crate::domain::search::MatchSide,
    search_matches: &[crate::domain::search::SearchMatch],
    current_search: Option<&crate::domain::search::SearchMatch>,
    search_colors: (egui::Color32, egui::Color32),
) {
    for sm in search_matches {
        if sm.side != match_side {
            continue;
        }
        let start_char =
            crate::domain::search::byte_offset_to_char_offset(line_text, sm.byte_range.start);
        let end_char =
            crate::domain::search::byte_offset_to_char_offset(line_text, sm.byte_range.end);
        let start_rect =
            galley.pos_from_cursor(egui::epaint::text::cursor::CCursor::new(start_char));
        let end_rect = galley.pos_from_cursor(egui::epaint::text::cursor::CCursor::new(end_char));
        let is_current = current_search.is_some_and(|c| c == sm);
        let bg = if is_current {
            search_colors.1
        } else {
            search_colors.0
        };
        let highlight_rect = egui::Rect::from_min_max(
            egui::pos2(text_pos.x + start_rect.min.x, y),
            egui::pos2(text_pos.x + end_rect.max.x, y + line_height),
        );
        painter.rect_filled(highlight_rect, 2.0, bg);
    }
}

/// Render a fold separator row.
fn render_fold_row(fctx: &FoldRowCtx, icon: &str, text: &str, hover_pos: Option<egui::Pos2>) {
    let FoldRowCtx {
        painter,
        left_x,
        y,
        panel_width,
        vctx,
    } = fctx;
    let (left_x, y, panel_width) = (*left_x, *y, *panel_width);
    let fold_height = vctx.fold_row_height as f32 * vctx.line_height;
    // Background.
    let row_rect =
        egui::Rect::from_min_size(egui::pos2(left_x, y), egui::vec2(panel_width, fold_height));
    painter.rect_filled(row_rect, 0.0, vctx.bg_fold);

    // Hover highlight.
    let is_hovered = hover_pos.is_some_and(|pos| row_rect.contains(pos));
    if is_hovered {
        painter.rect_filled(row_rect, 0.0, vctx.bg_fold_hover);
    }

    // Dashed horizontal line at top.
    let line_y = y + 0.5;
    painter.line_segment(
        [
            egui::pos2(left_x, line_y),
            egui::pos2(left_x + panel_width, line_y),
        ],
        egui::Stroke::new(1.0, vctx.fg_fold_line),
    );

    // Dashed horizontal line at bottom.
    let bottom_y = y + fold_height - 0.5;
    painter.line_segment(
        [
            egui::pos2(left_x, bottom_y),
            egui::pos2(left_x + panel_width, bottom_y),
        ],
        egui::Stroke::new(1.0, vctx.fg_fold_line),
    );

    // Label: render text centered, then icons on either side, all vertically centered.
    let icon_scale = if vctx.nf { ICON_SCALE } else { 1.0 };
    let icon_font = egui::FontId::monospace(vctx.font_size * icon_scale);
    let text_font = egui::FontId::monospace(vctx.font_size - 1.0);
    let center_x = left_x + panel_width / 2.0;
    let center_y = y + fold_height / 2.0;

    // Measure the text part to position icons around it.
    let text_galley =
        painter.layout_no_wrap(format!(" {text} "), text_font.clone(), vctx.fg_fold_text);
    let text_w = text_galley.size().x;
    let icon_galley = painter.layout_no_wrap(icon.to_owned(), icon_font.clone(), vctx.fg_fold_text);
    let icon_w = icon_galley.size().x;

    let total_w = icon_w + text_w + icon_w;
    let start_x = center_x - total_w / 2.0;

    // Left icon — vertically centered.
    let icon_pos = egui::pos2(start_x, center_y - icon_galley.size().y / 2.0);
    painter.galley(
        icon_pos,
        std::sync::Arc::clone(&icon_galley),
        vctx.fg_fold_text,
    );
    // Text — vertically centered.
    let text_pos = egui::pos2(start_x + icon_w, center_y - text_galley.size().y / 2.0);
    painter.galley(text_pos, text_galley, vctx.fg_fold_text);
    // Right icon — vertically centered.
    let right_icon_pos = egui::pos2(
        start_x + icon_w + text_w,
        center_y - icon_galley.size().y / 2.0,
    );
    painter.galley(right_icon_pos, icon_galley, vctx.fg_fold_text);
}

/// Build an egui LayoutJob from styled spans.
fn build_layout_job(
    text: &str,
    spans: &[StyledSpan],
    font_size: f32,
    vctx: &DiffViewCtx,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = f32::INFINITY; // no wrapping

    for span in spans {
        let span_text = &text[span.range.clone()];
        let fg =
            egui::Color32::from_rgba_unmultiplied(span.fg[0], span.fg[1], span.fg[2], span.fg[3]);
        let bg = if span.bg[3] > 0 {
            egui::Color32::from_rgba_unmultiplied(span.bg[0], span.bg[1], span.bg[2], span.bg[3])
        } else {
            egui::Color32::TRANSPARENT
        };

        let family = match (span.bold, span.italic) {
            (true, true) => vctx.font_bold_italic.clone(),
            (true, false) => vctx.font_bold.clone(),
            (false, true) => vctx.font_italic.clone(),
            (false, false) => egui::FontFamily::Monospace,
        };
        // Use synthetic italics when the selected font family doesn't include italic.
        let use_synthetic_italic = span.italic
            && if span.bold {
                vctx.synthetic_bold_italic
            } else {
                family == egui::FontFamily::Monospace
            };

        job.append(
            span_text,
            0.0,
            egui::text::TextFormat {
                font_id: egui::FontId::new(font_size, family),
                color: fg,
                background: bg,
                italics: use_synthetic_italic,
                ..Default::default()
            },
        );
    }

    job
}

/// Show a centered spinner with a "Computing diff..." label.
fn show_loading_spinner(ui: &mut egui::Ui) {
    egui::CentralPanel::default().show(ui, |ui| {
        ui.vertical_centered(|ui| {
            let available = ui.available_height();
            ui.add_space(available / 2.0 - 30.0);
            ui.add(egui::Spinner::new().size(24.0));
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Computing diff...")
                    .color(egui::Color32::from_gray(0x8B))
                    .size(14.0),
            );
        });
    });
}

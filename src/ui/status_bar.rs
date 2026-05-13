use crate::app::AppState;
use crate::ui::common;
use crate::ui::common::ns_label;
use eframe::egui;

/// Render the status bar content inside a horizontal layout.
/// `font_size` is the pre-computed status bar font size.
pub fn show(ui: &mut egui::Ui, state: &mut AppState, font_size: f32) {
    let nf = state.settings.behavior.use_nerdfont_icons;
    let muted = egui::Color32::from_rgb(0x8B, 0x94, 0x9E);
    let bright = egui::Color32::from_gray(0xCC);

    // Sidebar toggle button (left-most).
    let icon = if state.sidebar_visible {
        common::icon_sidebar_shown(nf)
    } else {
        common::icon_sidebar_hidden(nf)
    };
    let btn = ui.button(
        egui::RichText::new(icon)
            .monospace()
            .strong()
            .size(font_size),
    );
    if btn.clicked() {
        state.sidebar_visible = !state.sidebar_visible;
    }
    btn.on_hover_text(format!(
        "Toggle sidebar ({})",
        state.settings.keybinds.toggle_sidebar.display_string(nf)
    ));
    ui.add_space(common::PANEL_H_MARGIN);

    // Compute current hunk position.
    let hunk_info = if let Some(diff_data) = state.diff_cache.get(&state.selected_file) {
        let total_hunks = diff_data.hunks.len();
        // Use unified mode only if offsets are already computed; otherwise
        // fall back to side-by-side to avoid panicking on a not-yet-rendered file.
        let effective_mode = match state.diff_mode {
            crate::domain::fold::DiffMode::Unified
                if diff_data.fold_state.unified_offsets_ref().is_some() =>
            {
                crate::domain::fold::DiffMode::Unified
            }
            _ => crate::domain::fold::DiffMode::SideBySide,
        };
        let scroll_data_row = diff_data
            .fold_state
            .view_row_to_data_row_for_mode(state.scroll_row(), effective_mode);
        let total_view_rows = match effective_mode {
            crate::domain::fold::DiffMode::SideBySide => diff_data.fold_state.total_view_rows(),
            crate::domain::fold::DiffMode::Unified => diff_data
                .fold_state
                .total_view_rows_unified_cached()
                .unwrap_or(diff_data.fold_state.total_view_rows()),
        };
        let vp_rows = state.diff_rect.map_or(30, |r| {
            (r.height() / state.settings.behavior.line_height)
                .floor()
                .max(1.0) as usize
        });
        let bottom_data_row = diff_data.fold_state.view_row_to_data_row_for_mode(
            (state.scroll_row() + vp_rows).min(total_view_rows.saturating_sub(1)),
            effective_mode,
        );

        // Find the first hunk that overlaps the viewport.
        let in_viewport = diff_data
            .hunks
            .iter()
            .position(|h| h.row_range.start <= bottom_data_row && h.row_range.end > scroll_data_row)
            .map(|i| i + 1);

        let mut current_hunk = if let Some(h) = in_viewport {
            h
        } else {
            // No hunk starts in viewport — fall back to last-passed hunk.
            diff_data
                .hunks
                .iter()
                .rposition(|h| h.row_range.start <= scroll_data_row)
                .map_or(0, |i| i + 1)
        };

        // Before first hunk — show 1 to avoid confusing "0".
        if current_hunk == 0 && total_hunks > 0 {
            current_hunk = 1;
        }
        // When scrolled to the bottom, snap to the last hunk.
        if state.scroll_row() + vp_rows >= total_view_rows && total_hunks > 0 {
            current_hunk = total_hunks;
        }
        format!("Hunk {current_hunk}/{total_hunks}")
    } else {
        String::new()
    };

    if !hunk_info.is_empty() {
        ui.add(
            egui::Label::new(
                egui::RichText::new(&hunk_info)
                    .size(font_size)
                    .color(bright),
            )
            .selectable(false),
        );
    }

    // Inline diff stats.
    if let Some(stat) = state.diff_stats.get(state.selected_file)
        && (stat.added > 0 || stat.deleted > 0)
    {
        ns_label(ui, egui::RichText::new(" · ").color(muted).size(font_size));
        if stat.added > 0 {
            ns_label(
                ui,
                egui::RichText::new(format!("+{}", stat.added))
                    .size(font_size)
                    .monospace()
                    .color(common::COLOR_ADDED),
            );
        }
        if stat.added > 0 && stat.deleted > 0 {
            ns_label(ui, egui::RichText::new(" ").size(font_size));
        }
        if stat.deleted > 0 {
            ns_label(
                ui,
                egui::RichText::new(format!("-{}", stat.deleted))
                    .size(font_size)
                    .monospace()
                    .color(common::COLOR_DELETED),
            );
        }
    }

    // File position and review progress.
    let file_total = state.cached_visible_count;
    let file_num = (0..=state.selected_file)
        .filter(|&i| !state.is_file_excluded(i))
        .count();
    let reviewed_count = state.cached_reviewed_count;
    let reviewed_pct = if file_total > 0 {
        (reviewed_count as f32 / file_total as f32 * 100.0) as u32
    } else {
        0
    };
    ns_label(ui, egui::RichText::new(" · ").color(muted).size(font_size));
    ns_label(
        ui,
        egui::RichText::new(format!(
            "File {file_num}/{file_total} ({reviewed_pct}% reviewed)"
        ))
        .size(font_size)
        .color(muted),
    );

    // Right-aligned: next button + help button + loading indicator.
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        // Help button (rightmost).
        let help_icon = if nf { "\u{eb32}" } else { "?" };
        let help_btn = ui.button(
            egui::RichText::new(help_icon)
                .monospace()
                .size((font_size * 1.4).round()),
        );
        if help_btn.clicked() {
            state.help_open = !state.help_open;
        }
        help_btn.on_hover_text("Toggle help (?)");

        // "Next" button.
        let current_path = state.selected_pair().relative_path.clone();
        let is_reviewed = state.review_state.is_reviewed(&current_path);
        let next_label = if is_reviewed {
            "→ Next".to_string()
        } else {
            format!("{} Next", common::icon_check(nf))
        };
        let next_btn = ui.add(egui::Button::new(
            egui::RichText::new(&next_label)
                .monospace()
                .size(font_size)
                .color(muted),
        ));
        if next_btn.hovered() {
            // Re-draw with green color on hover.
            // (egui buttons handle hover styling via response; we use tooltip)
        }
        if next_btn.clicked() {
            if is_reviewed {
                state.select_next_file();
            } else {
                state.mark_reviewed_and_next();
            }
        }
        let kb_str = state
            .settings
            .keybinds
            .mark_reviewed_next
            .display_string(nf);
        let tooltip = if is_reviewed {
            format!("Go to next file ({kb_str})")
        } else {
            format!("Mark reviewed & go to next ({kb_str})")
        };
        next_btn.on_hover_text(tooltip);

        // Loading indicator (transient).
        let computed = state.files_computed;
        let total = state.file_pairs.len();
        if computed < total {
            ns_label(
                ui,
                egui::RichText::new(format!("Loading {computed}/{total}"))
                    .size(font_size)
                    .color(muted),
            );
            ui.add(egui::Spinner::new().size(font_size));
        } else if !state.force_computing.is_empty() {
            let n = state.force_computing.len();
            let label = if n == 1 {
                "Loading 1 file...".to_string()
            } else {
                format!("Loading {n} files...")
            };
            ns_label(ui, egui::RichText::new(label).size(font_size).color(muted));
            ui.add(egui::Spinner::new().size(font_size));
        }

        // Review complete indicator.
        if state.cached_visible_count > 0
            && state.cached_reviewed_count == state.cached_visible_count
        {
            let check = common::icon_check(nf);
            ns_label(
                ui,
                egui::RichText::new(format!("Review complete {check}"))
                    .size(font_size)
                    .color(egui::Color32::from_rgb(0x73, 0xC9, 0x91)),
            );
        }
    });
}

use crate::domain::file_pair::FileChangeKind;
use crate::domain::fold::DiffMode;
use crate::domain::settings::KeybindSettings;
use crate::ui::common::{
    PANEL_H_MARGIN_I8, collapse_path, icon_columns, icon_external, icon_picker, icon_rename_arrow,
    icon_search, icon_unified, kind_symbol_colored,
};
use eframe::egui;

use super::{DiffViewCtx, ICON_SCALE};

/// Actions returned by the unified header bar.
#[allow(clippy::struct_excessive_bools)]
pub(super) struct HeaderActions {
    pub copy_clicked: bool,
    pub picker_clicked: bool,
    pub toggle_mode_clicked: bool,
    pub open_editor_clicked: bool,
    pub search_clicked: bool,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_unified_header(
    ui: &mut egui::Ui,
    filename: &str,
    old_filename: Option<&str>,
    kind: FileChangeKind,
    mode_change: Option<&str>,
    mode_tooltip: Option<&str>,
    copied_at: Option<std::time::Instant>,
    vctx: &DiffViewCtx,
    diff_mode: DiffMode,
    editor_configured: bool,
    search_open: bool,
    picker_open: bool,
    keybinds: &KeybindSettings,
) -> HeaderActions {
    let mut actions = HeaderActions {
        copy_clicked: false,
        picker_clicked: false,
        toggle_mode_clicked: false,
        open_editor_clicked: false,
        search_clicked: false,
    };
    let nf = vctx.nf;
    let header_height = vctx.line_height + 8.0; // inner margin top+bottom (4+4)

    let muted_icon = egui::Color32::from_rgb(0x6B, 0x6B, 0x6B);
    let hover_icon = egui::Color32::from_gray(0xCC);

    let _frame_resp = egui::Frame::NONE
        .fill(vctx.bg_header)
        .stroke(egui::Stroke::new(1.0, vctx.fg_gutter_separator))
        .inner_margin(egui::Margin::symmetric(PANEL_H_MARGIN_I8, 4))
        .show(ui, |ui| {
            // Allocate the row space.
            let (rect, response) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), header_height - 8.0),
                egui::Sense::click(),
            );
            let painter = ui.painter();
            let center_y = rect.center().y;

            let pointer_pos = ui.input(|i| i.pointer.interact_pos());
            let was_clicked = ui.input(|i| i.pointer.any_click());

            let recently_copied = copied_at.is_some_and(|t| t.elapsed().as_secs_f32() < 2.0);
            let copy_icon = if recently_copied {
                "\u{f00c}"
            } else {
                "\u{f0c5}"
            };
            let copy_icon_color = if recently_copied {
                egui::Color32::from_rgb(0x3F, 0xB9, 0x50)
            } else {
                egui::Color32::from_gray(0x77)
            };

            let (symbol, color) = kind_symbol_colored(kind);
            let text_font = egui::FontId::monospace(vctx.font_size);
            let icon_scale = if vctx.nf { ICON_SCALE } else { 1.0 };
            let icon_font = egui::FontId::monospace(vctx.font_size * icon_scale);

            // Measure widths.
            let badge_galley = painter.layout_no_wrap(symbol.to_string(), text_font.clone(), color);
            let badge_w = badge_galley.size().x;
            let badge_h = badge_galley.size().y;
            let spacing = 6.0;

            // -- Left icons: search toggle + picker --
            let search_text = icon_search(nf);
            let search_galley_measure =
                painter.layout_no_wrap(search_text.to_string(), icon_font.clone(), muted_icon);
            let search_icon_w = search_galley_measure.size().x;
            let left_pad = 6.0;
            let left_gap = 8.0;

            // Search toggle icon (with active indicator).
            let search_x = rect.left();
            let search_rect = egui::Rect::from_min_size(
                egui::pos2(search_x - 2.0, rect.top()),
                egui::vec2(search_icon_w + left_pad, rect.height()),
            );
            let search_hovered = pointer_pos.is_some_and(|p| search_rect.contains(p));
            let search_active_color = egui::Color32::from_rgb(0xFF, 0xD7, 0x00);
            let search_color = if search_open {
                search_active_color
            } else if search_hovered {
                hover_icon
            } else {
                muted_icon
            };
            let search_galley =
                painter.layout_no_wrap(search_text.to_string(), icon_font.clone(), search_color);
            painter.galley(
                egui::pos2(search_x, center_y - search_galley.size().y / 2.0),
                search_galley,
                egui::Color32::TRANSPARENT,
            );
            if was_clicked && pointer_pos.is_some_and(|p| search_rect.contains(p)) {
                actions.search_clicked = true;
            }
            if search_hovered {
                let kb_str = keybinds.find.display_string(nf);
                let tip = if search_open {
                    format!("Close search ({kb_str})")
                } else {
                    format!("Find in files ({kb_str})")
                };
                response.clone().on_hover_text_at_pointer(tip);
            }

            // Picker icon.
            let picker_text = icon_picker(nf);
            let picker_galley_measure =
                painter.layout_no_wrap(picker_text.to_string(), icon_font.clone(), muted_icon);
            let picker_w = picker_galley_measure.size().x;
            let picker_x = search_x + search_icon_w + left_gap;
            let picker_rect = egui::Rect::from_min_size(
                egui::pos2(picker_x - 2.0, rect.top()),
                egui::vec2(picker_w + left_pad, rect.height()),
            );
            let picker_hovered = pointer_pos.is_some_and(|p| picker_rect.contains(p));
            let picker_color = if picker_open {
                search_active_color
            } else if picker_hovered {
                hover_icon
            } else {
                muted_icon
            };
            let picker_galley =
                painter.layout_no_wrap(picker_text.to_string(), icon_font.clone(), picker_color);
            painter.galley(
                egui::pos2(picker_x, center_y - picker_galley.size().y / 2.0),
                picker_galley,
                egui::Color32::TRANSPARENT,
            );
            if was_clicked && pointer_pos.is_some_and(|p| picker_rect.contains(p)) {
                actions.picker_clicked = true;
            }
            if picker_hovered {
                response.clone().on_hover_text_at_pointer(format!(
                    "Quick file picker ({})",
                    keybinds.quick_picker.display_string(nf)
                ));
            }

            // -- Right icons: mode toggle, editor, copy --
            let copy_galley =
                painter.layout_no_wrap(copy_icon.to_string(), icon_font.clone(), copy_icon_color);
            let copy_w = copy_galley.size().x;
            let right_pad = 6.0;
            let icon_gap = 8.0;

            // Mode toggle icon
            let mode_text = match diff_mode {
                DiffMode::SideBySide => icon_columns(nf),
                DiffMode::Unified => icon_unified(nf),
            };
            let mode_galley_muted =
                painter.layout_no_wrap(mode_text.to_string(), icon_font.clone(), muted_icon);
            let mode_w = mode_galley_muted.size().x;

            // Editor icon
            let editor_text = icon_external(nf);
            let editor_galley_muted =
                painter.layout_no_wrap(editor_text.to_string(), icon_font.clone(), muted_icon);
            let editor_w = editor_galley_muted.size().x;

            // Right side total width: mode + gap + editor + gap + copy + right_pad
            let right_icons_w = mode_w + icon_gap + editor_w + icon_gap + copy_w + right_pad;
            let left_icons_w = search_icon_w + left_gap + picker_w + left_pad;

            // Path collapse budget: full width minus badge, spacings, left/right icons, margins, mode annotation.
            let mode_annotation_w = if let Some(ms) = mode_change {
                let g = painter.layout_no_wrap(
                    format!("  {ms}"),
                    text_font.clone(),
                    egui::Color32::TRANSPARENT,
                );
                g.size().x
            } else {
                0.0
            };
            let path_budget = rect.width()
                - badge_w
                - spacing
                - left_icons_w
                - spacing
                - right_icons_w
                - spacing
                - mode_annotation_w;

            // Measure full path width and collapse if needed.
            let path_color = egui::Color32::from_gray(0x99);
            let dim_color = egui::Color32::from_gray(0x60);

            let ref_galley = painter.layout_no_wrap("M".to_string(), text_font.clone(), path_color);
            let char_w = ref_galley.size().x;

            // Build path galley: for renames show "old_path → new_path", otherwise just path.
            let mut job = egui::text::LayoutJob::default();
            if let Some(old) = old_filename {
                let arrow = icon_rename_arrow(nf);
                let arrow_chars = arrow.chars().count();
                let half_budget = ((path_budget / char_w) as usize).saturating_sub(arrow_chars) / 2;
                let old_segments = collapse_path(old, half_budget as f32 * char_w, char_w);
                for (text, dimmed) in &old_segments {
                    let c = if *dimmed { dim_color } else { path_color };
                    job.append(
                        text,
                        0.0,
                        egui::text::TextFormat {
                            font_id: text_font.clone(),
                            color: c,
                            ..Default::default()
                        },
                    );
                }
                job.append(
                    arrow,
                    0.0,
                    egui::text::TextFormat {
                        font_id: text_font.clone(),
                        color: dim_color,
                        ..Default::default()
                    },
                );
                let new_segments = collapse_path(filename, half_budget as f32 * char_w, char_w);
                for (text, dimmed) in &new_segments {
                    let c = if *dimmed { dim_color } else { path_color };
                    job.append(
                        text,
                        0.0,
                        egui::text::TextFormat {
                            font_id: text_font.clone(),
                            color: c,
                            ..Default::default()
                        },
                    );
                }
            } else {
                let segments = collapse_path(filename, path_budget, char_w);
                for (text, dimmed) in &segments {
                    let c = if *dimmed { dim_color } else { path_color };
                    job.append(
                        text,
                        0.0,
                        egui::text::TextFormat {
                            font_id: text_font.clone(),
                            color: c,
                            ..Default::default()
                        },
                    );
                }
            }
            let path_galley = painter.layout_job(job);
            let path_w = path_galley.size().x;

            // Center badge + path.
            let center_content = badge_w + spacing + path_w;
            let center_zone_left = rect.left() + left_icons_w + spacing;
            let center_zone_right = rect.right() - right_icons_w - spacing;
            let center_zone_w = center_zone_right - center_zone_left;
            let start_x = center_zone_left + ((center_zone_w - center_content) / 2.0).max(0.0);

            // Draw badge (vertically centered).
            painter.galley(
                egui::pos2(start_x, center_y - badge_galley.size().y / 2.0),
                badge_galley,
                egui::Color32::TRANSPARENT,
            );

            // Draw path (vertically centered using badge height as reference
            // so rename arrows with different glyph metrics don't shift the text).
            let ref_h = badge_h;
            let path_x = start_x + badge_w + spacing;
            painter.galley(
                egui::pos2(path_x, center_y - ref_h / 2.0),
                path_galley,
                egui::Color32::TRANSPARENT,
            );

            // Draw mode change annotation (dimmed, after path) with hover tooltip.
            if let Some(mode_str) = mode_change {
                let mode_galley = painter.layout_no_wrap(
                    format!("  {mode_str}"),
                    text_font.clone(),
                    egui::Color32::from_gray(0x70),
                );
                let mode_pos = egui::pos2(path_x + path_w, center_y - ref_h / 2.0);
                painter.galley(mode_pos, mode_galley, egui::Color32::TRANSPARENT);

                if let Some(tip) = mode_tooltip {
                    let mode_rect =
                        egui::Rect::from_min_size(mode_pos, egui::vec2(mode_annotation_w, ref_h));
                    if ui.input(|i| i.pointer.hover_pos().is_some_and(|p| mode_rect.contains(p))) {
                        response.clone().on_hover_text_at_pointer(tip);
                    }
                }
            }

            // -- Draw right-side icons (right to left) --

            // Copy icon (rightmost).
            let copy_x = rect.right() - copy_w - right_pad;
            let copy_rect = egui::Rect::from_min_size(
                egui::pos2(copy_x - 2.0, rect.top()),
                egui::vec2(copy_w + 6.0, rect.height()),
            );
            painter.galley(
                egui::pos2(copy_x, center_y - copy_galley.size().y / 2.0),
                copy_galley,
                egui::Color32::TRANSPARENT,
            );
            if was_clicked && pointer_pos.is_some_and(|p| copy_rect.contains(p)) {
                actions.copy_clicked = true;
            }
            if !recently_copied && pointer_pos.is_some_and(|p| copy_rect.contains(p)) {
                response.clone().on_hover_text_at_pointer(format!(
                    "Copy path to clipboard ({})",
                    keybinds.copy_path.display_string(nf)
                ));
            }

            // Editor icon (left of copy).
            let editor_x = copy_x - icon_gap - editor_w;
            let editor_rect = egui::Rect::from_min_size(
                egui::pos2(editor_x - 2.0, rect.top()),
                egui::vec2(editor_w + 6.0, rect.height()),
            );
            let editor_hovered = pointer_pos.is_some_and(|p| editor_rect.contains(p));
            let editor_color = if !editor_configured {
                egui::Color32::from_gray(0x44)
            } else if editor_hovered {
                hover_icon
            } else {
                muted_icon
            };
            let editor_galley =
                painter.layout_no_wrap(editor_text.to_string(), icon_font.clone(), editor_color);
            painter.galley(
                egui::pos2(editor_x, center_y - editor_galley.size().y / 2.0),
                editor_galley,
                egui::Color32::TRANSPARENT,
            );
            if editor_configured
                && was_clicked
                && pointer_pos.is_some_and(|p| editor_rect.contains(p))
            {
                actions.open_editor_clicked = true;
            }
            if editor_hovered {
                response.clone().on_hover_text_at_pointer(format!(
                    "Open in editor ({})",
                    keybinds.open_in_editor.display_string(nf)
                ));
            }

            // Mode toggle icon (left of editor).
            let mode_x = editor_x - icon_gap - mode_w;
            let mode_rect = egui::Rect::from_min_size(
                egui::pos2(mode_x - 2.0, rect.top()),
                egui::vec2(mode_w + 6.0, rect.height()),
            );
            let mode_hovered = pointer_pos.is_some_and(|p| mode_rect.contains(p));
            let mode_color = if mode_hovered { hover_icon } else { muted_icon };
            let mode_galley =
                painter.layout_no_wrap(mode_text.to_string(), icon_font.clone(), mode_color);
            painter.galley(
                egui::pos2(mode_x, center_y - mode_galley.size().y / 2.0),
                mode_galley,
                egui::Color32::TRANSPARENT,
            );
            if was_clicked && pointer_pos.is_some_and(|p| mode_rect.contains(p)) {
                actions.toggle_mode_clicked = true;
            }
            if mode_hovered {
                response.on_hover_text_at_pointer(format!(
                    "Toggle unified/side-by-side ({})",
                    keybinds.toggle_diff_mode.display_string(nf)
                ));
            }
        });

    actions
}

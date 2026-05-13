use crate::app::AppState;
use crate::domain::fold::{DiffMode, ResolvedRow, Segment};
use eframe::egui;

use super::estimate_viewport_rows_from_height;

/// Scroll speed multiplier for mouse wheel.
const SCROLL_SPEED: f32 = 3.0;
/// Pixels per line unit for Line-type mouse wheel events.
const LINE_SCROLL_SPEED: f32 = 50.0;
/// Exponential friction factor for momentum scrolling.
const MOMENTUM_FRICTION: f32 = 5.0;
/// Minimum velocity threshold for momentum scrolling (pixels/second).
const MOMENTUM_MIN_VY: f32 = 10.0;
/// Drain fraction of `pending_wheel_y` per frame for smooth mouse wheel scrolling.
/// Each frame applies 30% of the remaining delta (exponential approach to target).
const WHEEL_DRAIN_RATE: f32 = 0.3;
/// Below this threshold, apply the remainder in one go to avoid infinite tiny steps.
const WHEEL_DRAIN_MIN: f32 = 0.5;
/// EMA speed factor for velocity tracking — higher values mean faster response.
const VELOCITY_EMA_SPEED: f32 = 15.0;
/// Maximum EMA alpha — caps how quickly velocity adapts to instantaneous changes.
const VELOCITY_EMA_MAX_ALPHA: f32 = 0.6;

pub(super) fn handle_input(ctx: &egui::Context, state: &mut AppState, total_rows: usize) {
    let line_height = state.settings.behavior.line_height;
    let kb = state.settings.keybinds;
    ctx.input(|input| {
        // Only process scroll events if the pointer is over the diff panel area.
        let pointer_over_diff = state.diff_rect.is_some_and(|rect| {
            input
                .pointer
                .hover_pos()
                .is_some_and(|pos| rect.contains(pos))
        });

        // Process scroll events directly from MouseWheel events for proper
        // trackpad vs mouse wheel handling.
        // - Point unit = trackpad: apply delta directly, track velocity for momentum
        // - Line/Page unit = mouse wheel: apply directly, no momentum
        let screen_h = input.content_rect().height();

        if pointer_over_diff {
            for event in &input.events {
                if let egui::Event::MouseWheel {
                    unit,
                    delta,
                    phase,
                    modifiers,
                } = event
                {
                    let scaled = match unit {
                        egui::MouseWheelUnit::Point => *delta,
                        egui::MouseWheelUnit::Line => LINE_SCROLL_SPEED * *delta,
                        egui::MouseWheelUnit::Page => screen_h * *delta,
                    };
                    let (dy, dx) = if modifiers.shift {
                        (0.0, scaled.x + scaled.y)
                    } else {
                        (scaled.y, scaled.x)
                    };

                    // Apply scroll or accumulate for smoothing.
                    if unit == &egui::MouseWheelUnit::Point {
                        // Trackpad: apply directly for zero-latency response.
                        if dy != 0.0 {
                            state.scroll.y -= dy * SCROLL_SPEED;
                        }
                        if dx != 0.0 {
                            state.scroll.x =
                                (state.scroll.x - dx * SCROLL_SPEED).clamp(0.0, state.scroll.max_x);
                        }

                        // Velocity tracking for momentum, using phase for clean lifecycle.
                        match phase {
                            egui::TouchPhase::Start => {
                                // New gesture: reset velocity.
                                state.scroll.vy = 0.0;
                            }
                            egui::TouchPhase::Move => {
                                // Mid-gesture: track velocity via EMA.
                                if dy != 0.0 {
                                    let dt = input.stable_dt.max(0.001);
                                    let inst_vy = -dy * SCROLL_SPEED / dt;
                                    let alpha =
                                        (dt * VELOCITY_EMA_SPEED).min(VELOCITY_EMA_MAX_ALPHA);
                                    state.scroll.vy =
                                        state.scroll.vy * (1.0 - alpha) + inst_vy * alpha;
                                }
                            }
                            egui::TouchPhase::End => {
                                // Finger lifted: momentum will take over in apply_momentum().
                                // scroll_vy already has the accumulated velocity.
                            }
                            egui::TouchPhase::Cancel => {
                                state.scroll.vy = 0.0;
                            }
                        }
                    } else {
                        // Mouse wheel: accumulate for smooth draining.
                        if dy != 0.0 {
                            state.scroll.pending_wheel_y += dy * SCROLL_SPEED;
                        }
                        if dx != 0.0 {
                            state.scroll.x =
                                (state.scroll.x - dx * SCROLL_SPEED).clamp(0.0, state.scroll.max_x);
                        }
                        state.scroll.vy = 0.0;
                    }
                }
            }
        } // end pointer_over_diff

        // Keyboard navigation.
        let vp_rows = state.diff_rect.map_or(30, |r| {
            estimate_viewport_rows_from_height(
                r.height(),
                line_height,
                state.diff_view_ctx.panel_header_height,
            )
        });
        for event in &input.events {
            if let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            {
                // Ctrl+F: toggle search sidebar.
                if kb.find.matches_strict(*key, *modifiers) {
                    state.search.open = !state.search.open;
                    if state.search.open {
                        state.search.needs_focus = true;
                        // Remember if the sidebar was hidden so we can restore on close.
                        state.search.sidebar_was_hidden = !state.sidebar_visible;
                        state.sidebar_visible = true;
                        // Trigger background search if query is non-empty.
                        if !state.search.query.is_empty() {
                            state.dispatch_background_search();
                        }
                    } else {
                        // Restore sidebar visibility if it was hidden before search.
                        if state.search.sidebar_was_hidden {
                            state.sidebar_visible = false;
                            state.search.sidebar_was_hidden = false;
                        }
                    }
                    continue;
                }

                // When search is open, reuse next_file/prev_file keybinds for
                // match navigation. Allow modifier-key bindings that don't
                // conflict with typing; suppress single-key bindings.
                if state.search.open {
                    if kb.next_file.matches_strict(*key, *modifiers) {
                        if let Some(m) = state.search.next_match().cloned() {
                            crate::ui::search_panel::navigate_to_match(state, &m);
                        }
                        continue;
                    } else if kb.prev_file.matches_strict(*key, *modifiers)
                        && let Some(m) = state.search.prev_match().cloned()
                    {
                        crate::ui::search_panel::navigate_to_match(state, &m);
                        continue;
                    }

                    // Allow modifier-key bindings through.
                    if kb.toggle_diff_mode.matches_strict(*key, *modifiers)
                        || kb.open_in_editor.matches_strict(*key, *modifiers)
                        || kb.copy_path.matches_strict(*key, *modifiers)
                    {
                        // Fall through to the normal keybind handling below.
                    } else {
                        // Suppress all other keybinds (single-key bindings
                        // would conflict with typing in the search input).
                        continue;
                    }
                }

                if kb.next_hunk.matches_strict(*key, *modifiers) {
                    let diff_data = &state.diff_cache[&state.selected_file];
                    let current_data_row = diff_data
                        .fold_state
                        .view_row_to_data_row_for_mode(state.scroll_row(), state.diff_mode);
                    if let Some(view_row) =
                        crate::domain::hunk::next_hunk_row(&diff_data.hunks, current_data_row)
                            .and_then(|dr| {
                                diff_data
                                    .fold_state
                                    .data_to_view_row_for_mode(dr, state.diff_mode)
                            })
                    {
                        state.scroll_to_row(view_row);
                    }
                } else if kb.prev_hunk.matches_strict(*key, *modifiers) {
                    let diff_data = &state.diff_cache[&state.selected_file];
                    let current_data_row = diff_data
                        .fold_state
                        .view_row_to_data_row_for_mode(state.scroll_row(), state.diff_mode);
                    if let Some(view_row) =
                        crate::domain::hunk::prev_hunk_row(&diff_data.hunks, current_data_row)
                            .and_then(|dr| {
                                diff_data
                                    .fold_state
                                    .data_to_view_row_for_mode(dr, state.diff_mode)
                            })
                    {
                        state.scroll_to_row(view_row);
                    }
                } else if kb.toggle_diff_mode.matches_strict(*key, *modifiers) {
                    state.toggle_diff_mode();
                } else if kb.mark_reviewed_next.matches_strict(*key, *modifiers) {
                    state.mark_reviewed_and_next();
                } else if kb.mark_reviewed.matches_strict(*key, *modifiers) {
                    let path = state.selected_pair().relative_path.clone();
                    state.review_state.toggle(&path);
                    state.refresh_review_counts();
                } else if kb.next_file.matches_strict(*key, *modifiers) {
                    state.select_next_file();
                } else if kb.prev_file.matches_strict(*key, *modifiers) {
                    state.select_prev_file();
                } else if kb.fold_all.matches_strict(*key, *modifiers) {
                    if let Some(diff_data) = state.diff_cache.get_mut(&state.selected_file) {
                        diff_data.fold_state.fold_all();
                    }
                } else if kb.unfold_all.matches_strict(*key, *modifiers) {
                    if let Some(diff_data) = state.diff_cache.get_mut(&state.selected_file) {
                        diff_data.fold_state.unfold_all();
                    }
                } else if kb.open_in_editor.matches_strict(*key, *modifiers) {
                    state.open_in_editor();
                } else if kb.copy_path.matches_strict(*key, *modifiers) {
                    let path = state.file_pairs[state.selected_file]
                        .relative_path
                        .to_string_lossy()
                        .to_string();
                    state.pending_copy_path = Some(path);
                    state.copied_at = Some(std::time::Instant::now());
                } else {
                    // Standard navigation keys (not configurable).
                    match key {
                        egui::Key::ArrowDown => {
                            state.scroll_to_row(
                                (state.scroll_row() + 1).min(total_rows.saturating_sub(1)),
                            );
                        }
                        egui::Key::ArrowUp => {
                            state.scroll_to_row(state.scroll_row().saturating_sub(1));
                        }
                        egui::Key::PageDown => {
                            state.scroll_to_row(
                                (state.scroll_row() + vp_rows).min(total_rows.saturating_sub(1)),
                            );
                        }
                        egui::Key::PageUp => {
                            state.scroll_to_row(state.scroll_row().saturating_sub(vp_rows));
                        }
                        egui::Key::Home => {
                            state.scroll_to_row(0);
                        }
                        egui::Key::End => {
                            state.scroll_to_row(total_rows.saturating_sub(1));
                        }
                        egui::Key::ArrowRight => {
                            state.scroll.x = (state.scroll.x + state.diff_view_ctx.h_scroll_step)
                                .min(state.scroll.max_x);
                        }
                        egui::Key::ArrowLeft => {
                            state.scroll.x =
                                (state.scroll.x - state.diff_view_ctx.h_scroll_step).max(0.0);
                        }
                        _ => {}
                    }
                }
            }
        }
    });

    // Fold separator click detection.
    // Check if the user clicked on a fold separator row and expand accordingly.
    let fold_action = ctx.input(|input| {
        if !input.pointer.any_click() {
            return None;
        }
        let click_pos = input.pointer.interact_pos()?;
        let diff_rect = state.diff_rect?;
        if !diff_rect.contains(click_pos) {
            return None;
        }
        // Map click Y to a view-row index.
        // The diff content starts below diff_rect.min.y by the panel header height.
        let content_top = diff_rect.min.y + state.diff_view_ctx.panel_header_height;
        let relative_y = click_pos.y - content_top + state.scroll.y % line_height;
        if relative_y < 0.0 {
            return None;
        }
        let view_row = state.scroll_row() + (relative_y / line_height) as usize;
        let diff_data = state.diff_cache.get(&state.selected_file)?;
        let total = match state.diff_mode {
            DiffMode::SideBySide => diff_data.fold_state.total_view_rows(),
            DiffMode::Unified => diff_data
                .fold_state
                .total_view_rows_unified_cached()
                .unwrap_or(diff_data.fold_state.total_view_rows()),
        };
        if view_row >= total {
            return None;
        }
        let resolved = match state.diff_mode {
            DiffMode::SideBySide => diff_data.fold_state.resolve_view_row(view_row),
            DiffMode::Unified => diff_data.fold_state.resolve_unified_view_row(view_row).0,
        };
        match resolved {
            ResolvedRow::ExpandUp { fold_id, .. } => Some((fold_id, true)),
            ResolvedRow::ExpandDown { fold_id, .. } => Some((fold_id, false)),
            ResolvedRow::Data(_) => None,
        }
    });

    if let Some((fold_id, is_up)) = fold_action {
        // Record anchor for scroll stability.
        let anchor_view = state.scroll_row();
        let sub_pixel = state.scroll.y % line_height;
        let anchor_data = state.diff_cache.get(&state.selected_file).and_then(|d| {
            let resolved = match state.diff_mode {
                DiffMode::SideBySide => d.fold_state.resolve_view_row(anchor_view),
                DiffMode::Unified => d.fold_state.resolve_unified_view_row(anchor_view).0,
            };
            match resolved {
                ResolvedRow::Data(idx) => Some(idx),
                _ => None,
            }
        });

        if let Some(diff_data) = state.diff_cache.get_mut(&state.selected_file) {
            // For single-row folds (small hidden region), expand fully.
            let is_small = diff_data.fold_state.segments().iter().any(|s| {
                matches!(s, Segment::Fold { fold_id: fid, hidden_count, show_expand_up: true, show_expand_down: false, .. }
                    if *fid == fold_id && *hidden_count <= state.settings.behavior.fold_expand_step)
            });
            if is_small {
                diff_data.fold_state.expand_up(fold_id);
                diff_data.fold_state.expand_down(fold_id);
            } else if is_up {
                diff_data.fold_state.expand_up(fold_id);
            } else {
                diff_data.fold_state.expand_down(fold_id);
            }
            // Re-ensure unified offsets after fold mutation (they were cleared by rebuild).
            diff_data.ensure_unified_offsets_if_needed(state.diff_mode);
        }

        // Restore scroll position.
        if let Some(new_view) = anchor_data.and_then(|data_idx| {
            state.diff_cache.get(&state.selected_file).and_then(|d| {
                d.fold_state
                    .data_to_view_row_for_mode(data_idx, state.diff_mode)
            })
        }) {
            state.scroll.y = new_view as f32 * line_height + sub_pixel;
        }
    }

    // Clamp scroll.
    let vp_rows = state.diff_rect.map_or(30, |r| {
        estimate_viewport_rows_from_height(
            r.height(),
            line_height,
            state.diff_view_ctx.panel_header_height,
        )
    });
    state.clamp_scroll_y(total_rows, vp_rows);
}

pub(super) fn drain_pending_wheel(ctx: &egui::Context, state: &mut AppState) {
    if state.scroll.pending_wheel_y.abs() < WHEEL_DRAIN_MIN {
        if state.scroll.pending_wheel_y != 0.0 {
            state.scroll.y -= state.scroll.pending_wheel_y;
            state.scroll.pending_wheel_y = 0.0;
        }
        return;
    }
    let applied = state.scroll.pending_wheel_y * WHEEL_DRAIN_RATE;
    state.scroll.y -= applied;
    state.scroll.pending_wheel_y -= applied;
    ctx.request_repaint();
}

pub(super) fn apply_momentum(ctx: &egui::Context, state: &mut AppState, total_rows: usize) {
    let line_height = state.settings.behavior.line_height;
    // Don't apply momentum while there are active scroll events or pending wheel drain.
    let has_scroll_input = ctx.input(|i| {
        i.events
            .iter()
            .any(|e| matches!(e, egui::Event::MouseWheel { .. }))
    });
    if has_scroll_input || state.scroll.pending_wheel_y.abs() >= WHEEL_DRAIN_MIN {
        ctx.request_repaint();
        return;
    }

    if state.scroll.vy.abs() < MOMENTUM_MIN_VY {
        state.scroll.vy = 0.0;
        return;
    }

    let dt = ctx.input(|i| i.predicted_dt);
    state.scroll.y += state.scroll.vy * dt;
    // Exponential decay.
    state.scroll.vy *= (-MOMENTUM_FRICTION * dt).exp();

    let vp_rows = state.diff_rect.map_or(30, |r| {
        estimate_viewport_rows_from_height(
            r.height(),
            line_height,
            state.diff_view_ctx.panel_header_height,
        )
    });
    state.clamp_scroll_y(total_rows, vp_rows);
    ctx.request_repaint();
}

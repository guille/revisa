use crate::app::AppState;
use eframe::egui;

/// Show the review-complete modal overlay.
/// Returns `true` if the user chose to close the application.
pub fn show(ctx: &egui::Context, state: &mut AppState) -> bool {
    if !state.review_complete.show {
        state.review_complete.was_open = false;
        return false;
    }

    let success_green = egui::Color32::from_rgb(0x73, 0xC9, 0x91);
    let mut close_app = false;

    let modal = egui::Modal::new(egui::Id::new("review_complete_modal"))
        .backdrop_color(egui::Color32::from_black_alpha(150))
        .frame(
            egui::Frame::popup(&ctx.global_style())
                .fill(egui::Color32::from_rgb(0x2D, 0x2D, 0x30))
                .inner_margin(egui::Margin::symmetric(32, 24))
                .corner_radius(8.0)
                .stroke(egui::Stroke::new(
                    1.0,
                    egui::Color32::from_rgb(0x3E, 0x3E, 0x42),
                )),
        );

    let resp = modal.show(ctx, |ui| {
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 16.0;
            ui.label(
                egui::RichText::new("You have reviewed all files!")
                    .size(18.0)
                    .color(success_green),
            );
            ui.horizontal(|ui| {
                let gap = 12.0;
                ui.spacing_mut().item_spacing.x = gap;
                let half = (ui.available_width() - gap) / 2.0;
                let btn_height = 28.0;
                let btn_size = egui::vec2(half, btn_height);
                // Primary action.
                let go_back = ui.add_sized(
                    btn_size,
                    egui::Button::new(egui::RichText::new("Go back").size(14.0)),
                );
                if !state.review_complete.was_open {
                    go_back.request_focus();
                }
                if go_back.clicked() {
                    ui.close();
                }
                // Secondary action.
                let close = ui.add_sized(
                    btn_size,
                    egui::Button::new(egui::RichText::new("Close revisa").size(14.0)),
                );
                if close.clicked() {
                    close_app = true;
                }
                move_focus_with_arrows(ui, &go_back, &close);
            });
        });
    });

    if resp.should_close() {
        state.review_complete.show = false;
        state.review_complete.dismissed = true;
    }
    state.review_complete.was_open = true;

    close_app
}

/// Cycle focus between the two buttons with the left/right arrows, and swallow
/// up/down so they leave focus alone. Tab is left to  egui.
fn move_focus_with_arrows(ui: &egui::Ui, go_back: &egui::Response, close: &egui::Response) {
    let no_mods = egui::Modifiers::NONE;
    let (left, right, up, down) = ui.input_mut(|i| {
        (
            i.consume_key(no_mods, egui::Key::ArrowLeft),
            i.consume_key(no_mods, egui::Key::ArrowRight),
            i.consume_key(no_mods, egui::Key::ArrowUp),
            i.consume_key(no_mods, egui::Key::ArrowDown),
        )
    });
    if !(left || right || up || down) {
        return;
    }

    // egui reads the arrows at the start of the pass, long before we get to consume
    // them, so the move it scheduled has to be cancelled explicitly.
    ui.memory_mut(|m| m.move_focus(egui::FocusDirection::None));

    if left || right {
        if go_back.has_focus() {
            close.request_focus();
        } else {
            go_back.request_focus();
        }
    }
}

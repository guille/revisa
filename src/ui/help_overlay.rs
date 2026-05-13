use crate::domain::settings::KeybindSettings;
use eframe::egui;

const KBD_BG: egui::Color32 = egui::Color32::from_rgb(0x4A, 0x4A, 0x55);
const KBD_FG: egui::Color32 = egui::Color32::from_rgb(0xE0, 0xE0, 0xE6);
const SCRIM_ALPHA: u8 = 160;

/// Show the help overlay if `open` is true.
/// Returns true if it should remain open (false if user dismissed it).
pub fn show(ctx: &egui::Context, open: &mut bool, keybinds: &KeybindSettings, nf: bool) -> bool {
    if !*open {
        return false;
    }

    // Any key press or click closes the overlay (beyond Modal's built-in Escape/backdrop).
    let mut should_close = false;
    ctx.input(|input| {
        for event in &input.events {
            match event {
                egui::Event::Key { pressed: true, .. }
                | egui::Event::PointerButton { pressed: true, .. } => {
                    should_close = true;
                }
                _ => {}
            }
        }
    });

    if should_close {
        *open = false;
        return false;
    }

    let entries = keybinds.help_entries(nf);

    let modal = egui::Modal::new(egui::Id::new("help_overlay"))
        .backdrop_color(egui::Color32::from_black_alpha(SCRIM_ALPHA))
        .frame(
            egui::Frame::popup(&ctx.global_style()).fill(egui::Color32::from_rgb(0x25, 0x25, 0x26)),
        );

    let resp = modal.show(ctx, |ui| {
        ui.set_min_width(420.0);
        ui.label(
            egui::RichText::new("Keyboard Shortcuts")
                .size(16.0)
                .color(egui::Color32::from_gray(0xE0)),
        );
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);

        egui::Grid::new("keybind_grid")
            .striped(true)
            .min_col_width(120.0)
            .show(ui, |ui| {
                for (key, desc) in &entries {
                    ui.label(
                        egui::RichText::new(format!(" {key} "))
                            .monospace()
                            .strong()
                            .size(12.0)
                            .color(KBD_FG)
                            .background_color(KBD_BG),
                    );
                    ui.label(egui::RichText::new(*desc).color(egui::Color32::from_gray(0xCC)));
                    ui.end_row();
                }
            });

        ui.add_space(4.0);
        ui.separator();
        ui.label(
            egui::RichText::new("Press any key or click to close")
                .italics()
                .size(10.0)
                .color(egui::Color32::from_gray(0x6E)),
        );
    });

    if resp.should_close() {
        *open = false;
        return false;
    }

    true
}

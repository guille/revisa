mod app;
#[cfg(feature = "dev-tools")]
mod bench;
mod domain;
mod highlight;
mod ui;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// A native GUI tool for reviewing git PR diffs.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Open the diff viewer for two directories.
    Diff {
        /// Path to the left (old) directory
        #[arg(long)]
        left: PathBuf,

        /// Path to the right (new) directory
        #[arg(long)]
        right: PathBuf,

        /// Path to a custom settings TOML file
        #[arg(long)]
        config: Option<PathBuf>,

        /// Re-read and compare file contents, skipping identical files.
        /// Useful when comparing arbitrary directories (not needed for git difftool).
        #[arg(long)]
        recheck: bool,
    },
    /// Build the syntax highlighting cache.
    ///
    /// Combines the bundled syntax set with any .sublime-syntax files found in
    /// the user syntaxes directory (~/.config/revisa/syntaxes/ by default)
    /// and writes a compiled cache to ~/.cache/revisa/syntaxes.bin.
    BuildCache {
        /// Directory containing extra .sublime-syntax files.
        /// Defaults to ~/.config/revisa/syntaxes/
        #[arg(long)]
        syntaxes_dir: Option<PathBuf>,

        /// (Dev only) Rebuild the bundled syntax cache from source.
        /// Reads from assets/bundled_syntaxes/ and writes assets/bundled_syntaxes.bin.
        #[cfg(feature = "dev-tools")]
        #[arg(long)]
        bundled: bool,
    },
    /// (Dev only) Run the domain-layer benchmark suite over a generated
    /// corpus (or an external left/right pair).
    #[cfg(feature = "dev-tools")]
    Bench {
        /// Only report stages whose name contains this substring.
        #[arg(long)]
        filter: Option<String>,

        /// Benchmark an existing left directory instead of generating a corpus.
        #[arg(long, requires = "right")]
        left: Option<PathBuf>,

        /// Benchmark an existing right directory instead of generating a corpus.
        #[arg(long, requires = "left")]
        right: Option<PathBuf>,

        /// Corpus size multiplier.
        #[arg(long, default_value_t = 1)]
        scale: usize,

        /// Timed iterations per stage; the median is reported.
        #[arg(long, default_value_t = 3)]
        iterations: usize,

        /// Corpus generator seed.
        #[arg(long, default_value_t = 0x00C0_FFEE)]
        seed: u64,

        /// Emit JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Diff {
            left,
            right,
            config,
            recheck,
        } => {
            run_diff(left, right, config, recheck);
        }
        Command::BuildCache {
            syntaxes_dir,
            #[cfg(feature = "dev-tools")]
            bundled,
        } => {
            #[cfg(feature = "dev-tools")]
            if bundled {
                run_build_bundled(syntaxes_dir);
                return;
            }
            run_build_cache(syntaxes_dir);
        }
        #[cfg(feature = "dev-tools")]
        Command::Bench {
            filter,
            left,
            right,
            scale,
            iterations,
            seed,
            json,
        } => {
            bench::run(&bench::Options {
                filter,
                left,
                right,
                scale,
                iterations,
                seed,
                json,
            });
        }
    }
}

fn run_build_cache(syntaxes_dir: Option<PathBuf>) {
    let dir = syntaxes_dir.unwrap_or_else(highlight::cache::default_syntaxes_dir);
    match highlight::cache::build_syntax_cache(&dir) {
        Ok(count) => {
            let cache_path = highlight::cache::syntax_cache_path();
            println!("Wrote {count} syntaxes to {}", cache_path.display());
        }
        Err(e) => {
            eprintln!("Error building syntax cache: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(feature = "dev-tools")]
fn run_build_bundled(syntaxes_dir: Option<PathBuf>) {
    let dir = syntaxes_dir.unwrap_or_else(|| std::path::PathBuf::from("assets/bundled_syntaxes"));
    match highlight::cache::build_bundled_cache(&dir) {
        Ok(count) => {
            println!("Wrote {count} syntaxes to assets/bundled_syntaxes.bin");
        }
        Err(e) => {
            eprintln!("Error building bundled syntax cache: {e}");
            std::process::exit(1);
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_diff(left: PathBuf, right: PathBuf, config: Option<PathBuf>, filter_unchanged: bool) {
    // Load settings from config file.
    let settings = match &config {
        Some(path) => domain::settings::Settings::load_from(path),
        None => domain::settings::Settings::load_default(),
    };
    let settings = match settings {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error loading settings: {e}");
            std::process::exit(1);
        }
    };

    let theme_path: Option<PathBuf> = settings.behavior.theme.clone();

    let file_pairs = match domain::file_pair::walk_and_pair(&left, &right, filter_unchanged) {
        Ok(pairs) => pairs,
        Err(e) => {
            eprintln!("Error scanning directories: {e}");
            std::process::exit(1);
        }
    };

    if file_pairs.is_empty() {
        eprintln!("No file differences found between the two directories.");
        std::process::exit(0);
    }

    let review_state = domain::review_state::ReviewState::new(
        file_pairs
            .iter()
            .map(|fp| fp.relative_path.clone())
            .collect(),
    );

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 900.0])
            .with_title(env!("CARGO_PKG_NAME")),
        ..Default::default()
    };

    if let Err(e) = eframe::run_native(
        env!("CARGO_PKG_NAME"),
        options,
        Box::new(|cc| {
            configure_visuals(&cc.egui_ctx, &settings);
            let font_variants = configure_fonts(&cc.egui_ctx, &settings);
            let state = app::AppState::new(
                file_pairs,
                review_state,
                theme_path.as_deref(),
                cc.egui_ctx.clone(),
                settings,
                font_variants,
            );
            Ok(Box::new(RevisaApp {
                state,
                picker: ui::quick_picker::QuickPicker::new(),
                default_sidebar_pct: domain::settings::Settings::default().behavior.sidebar_width,
            }))
        }),
    ) {
        eprintln!("Error: failed to start GUI: {e}");
        std::process::exit(1);
    }
}

fn configure_fonts(
    ctx: &eframe::egui::Context,
    settings: &domain::settings::Settings,
) -> app::FontVariants {
    use eframe::egui::{FontData, FontDefinitions, FontFamily};

    let mut fonts = FontDefinitions::default();

    let fc = fontconfig::Fontconfig::new().expect("failed to initialize fontconfig");

    // Try the user's configured font first, then common monospace fallbacks.
    let candidates: Vec<&str> = std::iter::once(settings.font.face.as_str())
        .chain([
            "monospace",
            "DejaVu Sans Mono",
            "Liberation Mono",
            "Noto Sans Mono",
            "Hack",
            "Fira Code",
            "Source Code Pro",
            "Ubuntu Mono",
            "Courier New",
        ])
        .collect();

    for family in &candidates {
        if let Ok(regular_font) = fc.find(family, None)
            && let Ok(data) = std::fs::read(&regular_font.path)
        {
            let font_key = family.to_string();
            if *family != settings.font.face.as_str() {
                eprintln!(
                    "Warning: font '{}' not found, falling back to '{font_key}'",
                    settings.font.face
                );
            }
            fonts.font_data.insert(
                font_key.clone(),
                std::sync::Arc::new(FontData::from_owned(data)),
            );
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .insert(0, font_key.clone());
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .insert(0, font_key);

            // Load style variants via fontconfig.
            let variants = [
                ("Bold", ui::common::FONT_BOLD),
                ("Italic", ui::common::FONT_ITALIC),
                ("Bold Italic", ui::common::FONT_BOLD_ITALIC),
            ];
            let mut available = [false; 3];
            for (i, (style, key)) in variants.iter().enumerate() {
                if let Ok(variant_font) = fc.find(family, Some(style)) {
                    // Only register if it's a different file than regular.
                    // NOTE: This fails for variable fonts (single .ttf with all weights/styles),
                    // where the bold variant lives in the same file. These fonts will fall back
                    // to regular + synthetic italic. See: https://github.com/emilk/egui/issues/3218
                    if variant_font.path != regular_font.path
                        && let Ok(data) = std::fs::read(&variant_font.path)
                    {
                        fonts.font_data.insert(
                            key.to_string(),
                            std::sync::Arc::new(FontData::from_owned(data)),
                        );
                        fonts
                            .families
                            .insert(FontFamily::Name((*key).into()), vec![key.to_string()]);
                        available[i] = true;
                    }
                }
            }

            ctx.set_fonts(fonts);
            return app::FontVariants {
                has_bold: available[0],
                has_italic: available[1],
                has_bold_italic: available[2],
            };
        }
    }

    eprintln!(
        "Error: could not find any suitable font. Tried: {}.\n\
         Configure a font via font.face in your config file.",
        candidates.join(", ")
    );
    std::process::exit(1);
}

/// Configure the app's visual theme.
fn configure_visuals(ctx: &eframe::egui::Context, settings: &domain::settings::Settings) {
    use eframe::egui::{Color32, Visuals};

    let mut visuals = Visuals::dark();

    let bg = settings.colors.bg_app.to_egui();
    visuals.window_fill = bg;
    visuals.panel_fill = bg;
    visuals.extreme_bg_color = bg;
    visuals.faint_bg_color = Color32::from_rgb(0x33, 0x33, 0x33);

    ctx.set_visuals(visuals);
}

/// Minimum consecutive frames with stable `pixels_per_point` before rendering.
/// Avoids the layout shift caused by winit's Wayland backend reporting ppi=1.0
/// before the compositor provides the actual scale factor.
const PPI_STABLE_THRESHOLD: u8 = 2;

struct RevisaApp {
    state: app::AppState,
    picker: ui::quick_picker::QuickPicker,
    /// Default sidebar width percentage (used when sidebar_width is set to 0).
    default_sidebar_pct: f32,
}

impl eframe::App for RevisaApp {
    /// Track display scale factor stabilization and invalidate panel state on changes.
    ///
    /// On Wayland, winit initializes `scale_factor` to 1.0 and updates it
    /// asynchronously once the compositor maps the window to a surface.  This
    /// means the first 1-2 frames may report an incorrect ppi, causing panels
    /// to render at the wrong viewport size and produce a visible layout shift.
    ///
    /// We track consecutive frames with a stable ppi value.  The `ui()` method
    /// skips rendering (painting only `bg_app`) until ppi has been stable for
    /// `PPI_STABLE_THRESHOLD` frames.  On ppi changes we also clear persisted
    /// `PanelState` so panels re-layout from defaults at the correct size.
    fn raw_input_hook(
        &mut self,
        ctx: &eframe::egui::Context,
        raw_input: &mut eframe::egui::RawInput,
    ) {
        let ppi = raw_input
            .viewports
            .get(&raw_input.viewport_id)
            .and_then(|v| v.native_pixels_per_point);
        match ppi {
            Some(ppi) if (self.state.last_ppi - ppi).abs() > f32::EPSILON => {
                self.state.last_ppi = ppi;
                self.state.ppi_stable_frames = 0;
                ctx.data_mut(|d| {
                    d.remove::<eframe::egui::containers::panel::PanelState>(eframe::egui::Id::new(
                        "file_list",
                    ));
                    d.remove::<eframe::egui::containers::panel::PanelState>(eframe::egui::Id::new(
                        "status_bar",
                    ));
                });
            }
            Some(_) => {
                self.state.ppi_stable_frames = self.state.ppi_stable_frames.saturating_add(1);
            }
            // Platform doesn't report native ppi (e.g. some X11 setups).
            // Skip the warmup — there's no ppi instability to handle.
            None => {
                self.state.ppi_stable_frames =
                    self.state.ppi_stable_frames.max(PPI_STABLE_THRESHOLD);
            }
        }
    }

    fn ui(&mut self, root_ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        use eframe::egui;
        let ctx = root_ui.ctx().clone();

        // On Wayland with HiDPI, winit reports ppi=1.0 for the first 1-2 frames
        // before the compositor provides the actual value.  Rendering during this
        // period produces a visible layout shift when the viewport resizes.
        if self.state.ppi_stable_frames < PPI_STABLE_THRESHOLD {
            ctx.request_repaint();
            let bg = self.state.settings.colors.bg_app.to_egui();
            root_ui.painter().rect_filled(root_ui.max_rect(), 0.0, bg);
            root_ui.centered_and_justified(|ui| {
                ui.add(
                    eframe::egui::Spinner::new()
                        .size(32.0)
                        .color(bg.linear_multiply(2.0)),
                );
            });
            return;
        }

        // Derive UI font sizes from the main font size.
        let status_bar_font_size =
            (self.state.settings.font.size * ui::common::UI_FONT_RATIO).round();
        let status_bar_height = status_bar_font_size + 13.0;
        let sidebar_max = ctx.content_rect().width() * 0.35;
        let sidebar_pct = if self.state.settings.behavior.sidebar_width > 0.0 {
            self.state.settings.behavior.sidebar_width
        } else {
            // When configured to start hidden, use the default width when toggled on.
            self.default_sidebar_pct
        };
        let sidebar_default = ctx.content_rect().width() * sidebar_pct / 100.0;

        // Drain background diff computation results.
        self.state.poll_background();

        // Handle global keybinds (always active).
        let mut toggle_picker = false;
        let mut toggle_sidebar = false;
        let mut toggle_help = false;
        let mut goto_line = false;
        let kb = &self.state.settings.keybinds;
        ctx.input(|input| {
            for event in &input.events {
                match event {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        if kb.quick_picker.matches(*key, *modifiers) {
                            toggle_picker = true;
                        } else if kb.goto_line.matches(*key, *modifiers) {
                            goto_line = true;
                        } else if kb.toggle_sidebar.matches(*key, *modifiers) {
                            toggle_sidebar = true;
                        } else if *key == egui::Key::Questionmark
                            || (*key == egui::Key::F1 && modifiers.is_none())
                        {
                            toggle_help = true;
                        }
                    }
                    egui::Event::Text(text) if text == "?" => toggle_help = true,
                    _ => {}
                }
            }
        });

        // Help overlay takes priority over everything.
        if toggle_help && !self.picker.is_open() {
            self.state.help_open = !self.state.help_open;
        }
        if self.state.help_open {
            // Skip overlay input on the frame it was just opened, so the same
            // keypress that toggled it on doesn't immediately close it.
            if !toggle_help {
                ui::help_overlay::show(
                    &ctx,
                    &mut self.state.help_open,
                    &self.state.settings.keybinds,
                    self.state.settings.behavior.use_nerdfont_icons,
                );
            }
        }

        // Skip interactive actions when help or picker is active.
        if !self.state.help_open {
            if toggle_picker
                && let Some(orig) = self.picker.toggle(
                    self.state.selected_file,
                    self.state.scroll.y,
                    self.state.scroll.x,
                )
            {
                // Picker closed via toggle — restore original file and scroll.
                self.state.selected_file = orig;
                self.state.scroll.y = self.picker.saved_scroll_y();
                self.state.scroll.x = self.picker.saved_scroll_x();
            }
            if goto_line && !self.picker.is_open() {
                self.picker.open_goto_line(
                    self.state.selected_file,
                    self.state.scroll.y,
                    self.state.scroll.x,
                );
            }
            if toggle_sidebar && !self.picker.is_open() {
                self.state.sidebar_visible = !self.state.sidebar_visible;
            }
            // Deferred picker open from header icon click.
            if self.state.pending_open_picker && !self.picker.is_open() {
                self.state.pending_open_picker = false;
                self.picker.toggle(
                    self.state.selected_file,
                    self.state.scroll.y,
                    self.state.scroll.x,
                );
            }
        }

        // If picker is open, it takes priority for input.
        let picker_active = if self.state.help_open {
            false
        } else {
            ui::quick_picker::show(&ctx, &mut self.state, &mut self.picker)
        };
        self.state.picker_open = self.picker.is_open();

        if self.state.sidebar_visible {
            egui::Panel::left("file_list")
                .default_size(sidebar_default)
                .min_size(ui::file_list::SIDEBAR_MIN_WIDTH)
                .max_size(sidebar_max)
                .show(root_ui, |ui| {
                    if self.state.search.open {
                        ui::search_panel::show(ui, &mut self.state);
                    } else {
                        ui::file_list::show(ui, &mut self.state);
                    }
                });
        }

        // Status bar at bottom.
        let status_bar_bg = self.state.settings.colors.bg_header.to_egui();
        let border_color = self.state.settings.colors.fg_gutter_separator.to_egui();

        egui::Panel::bottom("status_bar")
            .exact_size(status_bar_height)
            .frame(
                egui::Frame::NONE
                    .fill(status_bar_bg)
                    .stroke(egui::Stroke::new(1.0, border_color))
                    .inner_margin(egui::Margin::symmetric(ui::common::PANEL_H_MARGIN_I8, 0)),
            )
            .show(root_ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui::status_bar::show(ui, &mut self.state, status_bar_font_size);
                });
            });

        if !picker_active && !self.state.help_open {
            ui::diff_view::show(root_ui, &mut self.state);
        } else {
            ui::diff_view::show_no_input(root_ui, &mut self.state);
        }

        // Review-complete modal overlay.
        if ui::review_complete::show(&ctx, &mut self.state) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

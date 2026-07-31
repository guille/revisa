use serde::Deserialize;
use std::fmt::Write;
use std::path::{Path, PathBuf};

/// Default line height as a multiplier of font size when not configured.
const DEFAULT_LINE_HEIGHT_MULT: f32 = 1.5;

/// Expand a leading `~` to the user's home directory.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    } else if path == "~"
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home);
    }
    PathBuf::from(path)
}

/// Parsed and validated application settings.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub font: FontSettings,
    pub colors: ColorSettings,
    pub behavior: BehaviorSettings,
    pub keybinds: KeybindSettings,
}

#[derive(Debug, Clone)]
pub struct FontSettings {
    pub face: String,
    pub size: f32,
    pub gutter_size: f32,
}

/// An RGBA color parsed from a hex string (`#RRGGBB` defaults to alpha 255).
#[derive(Debug, Clone, Copy)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub fn to_egui(self) -> eframe::egui::Color32 {
        eframe::egui::Color32::from_rgba_unmultiplied(self.r, self.g, self.b, self.a)
    }

    /// Convert to [R, G, B, A] byte array (used for span-level highlight colors).
    pub fn to_rgba(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }
}

#[allow(clippy::many_single_char_names)]
fn parse_hex_color(s: &str, field_name: &str) -> Result<Rgba, String> {
    let s = s.trim();
    if !s.starts_with('#') || (s.len() != 7 && s.len() != 9) {
        return Err(format!(
            "{field_name}: expected \"#RRGGBB\" or \"#RRGGBBAA\" hex color, got \"{s}\""
        ));
    }
    let r = u8::from_str_radix(&s[1..3], 16)
        .map_err(|_| format!("{field_name}: invalid hex color \"{s}\""))?;
    let g = u8::from_str_radix(&s[3..5], 16)
        .map_err(|_| format!("{field_name}: invalid hex color \"{s}\""))?;
    let b = u8::from_str_radix(&s[5..7], 16)
        .map_err(|_| format!("{field_name}: invalid hex color \"{s}\""))?;
    let a = if s.len() == 9 {
        u8::from_str_radix(&s[7..9], 16)
            .map_err(|_| format!("{field_name}: invalid hex color \"{s}\""))?
    } else {
        255
    };
    Ok(Rgba { r, g, b, a })
}

#[derive(Debug, Clone, Copy)]
pub struct ColorSettings {
    pub bg_app: Rgba,
    pub bg_added: Rgba,
    pub bg_removed: Rgba,
    pub bg_inline_added: Rgba,
    pub bg_inline_removed: Rgba,
    pub bg_padding: Rgba,
    pub bg_fold: Rgba,
    pub bg_header: Rgba,
    pub fg_fold_text: Rgba,
    pub fg_fold_line: Rgba,
    pub fg_gutter: Rgba,
    pub fg_gutter_added: Rgba,
    pub fg_gutter_removed: Rgba,
    pub fg_gutter_separator: Rgba,
    pub bg_search_match: Rgba,
    pub bg_search_match_current: Rgba,
}

/// Configured default diff mode: a concrete mode, or `Auto` (resolved from
/// window width once, on the first rendered frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffModePreference {
    SideBySide,
    Unified,
    Auto,
}

#[derive(Debug, Clone)]
pub struct BehaviorSettings {
    pub use_nerdfont_icons: bool,
    pub fold_context: usize,
    pub fold_expand_step: usize,
    pub sidebar_width: f32,
    pub theme: Option<PathBuf>,
    pub line_height: f32,
    pub fold_row_height: usize,
    /// Editor command for "open in editor". Falls back to $VISUAL, then $EDITOR.
    pub editor: Option<String>,
    /// Default diff view mode: "side-by-side", "unified" or "auto".
    pub default_diff_mode: DiffModePreference,
    /// Maximum lines per file before showing "too large" placeholder. 0 = no limit.
    pub max_diff_lines: usize,
}

/// A keybind: an egui key plus modifier flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keybind {
    pub key: eframe::egui::Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Keybind {
    /// Check whether this keybind matches a key event.
    pub fn matches(self, key: eframe::egui::Key, modifiers: eframe::egui::Modifiers) -> bool {
        key == self.key
            && modifiers.ctrl == self.ctrl
            && modifiers.shift == self.shift
            && modifiers.alt == self.alt
    }

    /// Strict keybind match: if the keybind has modifiers, require exact match.
    /// If the keybind has no modifiers, also require that no modifiers are pressed
    /// (prevents "." from firing on "Ctrl+.").
    pub fn matches_strict(
        self,
        key: eframe::egui::Key,
        modifiers: eframe::egui::Modifiers,
    ) -> bool {
        if self.ctrl || self.shift || self.alt {
            self.matches(key, modifiers)
        } else {
            key == self.key && !modifiers.any()
        }
    }

    /// Format the keybind for display, optionally using nerdfont icons for arrows.
    pub fn display_string(self, nf: bool) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("Ctrl+");
        }
        if self.shift {
            s.push_str("Shift+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        let name = match self.key {
            eframe::egui::Key::ArrowUp => {
                if nf {
                    "\u{eaa1}"
                } else {
                    "\u{2191}"
                }
            }
            eframe::egui::Key::ArrowDown => {
                if nf {
                    "\u{ea9a}"
                } else {
                    "\u{2193}"
                }
            }
            eframe::egui::Key::ArrowLeft => {
                if nf {
                    "\u{ea9b}"
                } else {
                    "\u{2190}"
                }
            }
            eframe::egui::Key::ArrowRight => {
                if nf {
                    "\u{ea9c}"
                } else {
                    "\u{2192}"
                }
            }
            eframe::egui::Key::Enter => "Enter",
            eframe::egui::Key::Space => "Space",
            eframe::egui::Key::Tab => "Tab",
            eframe::egui::Key::Escape => "Esc",
            other => {
                let _ = write!(s, "{other:?}");
                return s;
            }
        };
        s.push_str(name);
        s
    }
}

impl std::fmt::Display for Keybind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_string(false))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct KeybindSettings {
    pub next_hunk: Keybind,
    pub prev_hunk: Keybind,
    pub mark_reviewed_next: Keybind,
    pub mark_reviewed: Keybind,
    pub next_file: Keybind,
    pub prev_file: Keybind,
    pub quick_picker: Keybind,
    pub toggle_sidebar: Keybind,
    pub fold_all: Keybind,
    pub unfold_all: Keybind,
    pub open_in_editor: Keybind,
    pub copy_path: Keybind,
    pub toggle_diff_mode: Keybind,
    pub goto_line: Keybind,
    pub find: Keybind,
}

impl KeybindSettings {
    /// Return keybind descriptions for the help overlay.
    /// When `nf` is true, use nerdfont icons for arrow keys.
    pub fn help_entries(&self, nf: bool) -> Vec<(String, &'static str)> {
        let (up, down, left, right) = if nf {
            ("\u{eaa1}", "\u{ea9a}", "\u{ea9b}", "\u{ea9c}")
        } else {
            ("\u{2191}", "\u{2193}", "\u{2190}", "\u{2192}")
        };
        vec![
            ("F1 / ?".to_string(), "Show/hide this help"),
            (self.next_hunk.display_string(nf), "Next hunk"),
            (self.prev_hunk.display_string(nf), "Previous hunk"),
            (
                self.mark_reviewed_next.display_string(nf),
                "Mark reviewed & open next file",
            ),
            (
                self.mark_reviewed.display_string(nf),
                "Toggle reviewed on current file",
            ),
            (self.next_file.display_string(nf), "Next file"),
            (self.prev_file.display_string(nf), "Previous file"),
            (format!("{up}/{down}"), "Scroll up/down"),
            ("PgUp/PgDn".to_string(), "Scroll up/down (page)"),
            ("Home/End".to_string(), "Jump to start/end of file"),
            (format!("{left}/{right}"), "Scroll left/right"),
            (self.quick_picker.display_string(nf), "Quick file picker"),
            (
                self.toggle_sidebar.display_string(nf),
                "Toggle sidebar visibility",
            ),
            (
                self.fold_all.display_string(nf),
                "Fold all unchanged regions",
            ),
            (self.unfold_all.display_string(nf), "Expand all folds"),
            (
                self.open_in_editor.display_string(nf),
                "Open current file in editor",
            ),
            (
                self.copy_path.display_string(nf),
                "Copy file path to clipboard",
            ),
            (
                self.toggle_diff_mode.display_string(nf),
                "Toggle side-by-side / unified view",
            ),
            (self.goto_line.display_string(nf), "Go to line number"),
            (self.find.display_string(nf), "Find in files"),
        ]
    }
}

// --- Defaults ---

impl Default for FontSettings {
    fn default() -> Self {
        Self {
            face: "monospace".into(),
            size: 14.0,
            gutter_size: 13.0,
        }
    }
}

impl Default for ColorSettings {
    fn default() -> Self {
        Self {
            bg_app: Rgba {
                r: 0x2C,
                g: 0x2C,
                b: 0x2C,
                a: 255,
            },
            bg_added: Rgba {
                r: 0x1C,
                g: 0x4D,
                b: 0x2A,
                a: 255,
            },
            bg_removed: Rgba {
                r: 0x3A,
                g: 0x1E,
                b: 0x1E,
                a: 255,
            },
            bg_inline_added: Rgba {
                r: 0x2E,
                g: 0x7A,
                b: 0x42,
                a: 255,
            },
            bg_inline_removed: Rgba {
                r: 0x7A,
                g: 0x2E,
                b: 0x2E,
                a: 255,
            },
            bg_padding: Rgba {
                r: 35,
                g: 35,
                b: 40,
                a: 255,
            },
            bg_fold: Rgba {
                r: 0x1A,
                g: 0x2A,
                b: 0x3A,
                a: 255,
            },
            bg_header: Rgba {
                r: 0x1E,
                g: 0x1E,
                b: 0x1E,
                a: 255,
            },
            fg_fold_text: Rgba {
                r: 0x60,
                g: 0x90,
                b: 0xC0,
                a: 255,
            },
            fg_fold_line: Rgba {
                r: 0x40,
                g: 0x60,
                b: 0x80,
                a: 255,
            },
            fg_gutter: Rgba {
                r: 0x84,
                g: 0x78,
                b: 0x6A,
                a: 255,
            },
            fg_gutter_added: Rgba {
                r: 0x2E,
                g: 0x7A,
                b: 0x42,
                a: 255,
            },
            fg_gutter_removed: Rgba {
                r: 0x7A,
                g: 0x2E,
                b: 0x2E,
                a: 255,
            },
            fg_gutter_separator: Rgba {
                r: 0x3C,
                g: 0x3C,
                b: 0x3C,
                a: 255,
            },
            bg_search_match: Rgba {
                r: 0xFF,
                g: 0xCC,
                b: 0x00,
                a: 0x66,
            },
            bg_search_match_current: Rgba {
                r: 0xFF,
                g: 0xCC,
                b: 0x00,
                a: 0xAA,
            },
        }
    }
}

impl Default for BehaviorSettings {
    fn default() -> Self {
        Self {
            use_nerdfont_icons: true,
            fold_context: 5,
            fold_expand_step: 20,
            sidebar_width: 25.0,
            theme: None,
            line_height: (14.0 * DEFAULT_LINE_HEIGHT_MULT).round(), // Must match FontSettings default size
            fold_row_height: 2,
            editor: None,
            default_diff_mode: DiffModePreference::Auto,
            max_diff_lines: 4_000,
        }
    }
}

impl Default for KeybindSettings {
    fn default() -> Self {
        use eframe::egui::Key;
        Self {
            next_hunk: Keybind {
                key: Key::Period,
                ctrl: false,
                shift: false,
                alt: false,
            },
            prev_hunk: Keybind {
                key: Key::Comma,
                ctrl: false,
                shift: false,
                alt: false,
            },
            mark_reviewed_next: Keybind {
                key: Key::Enter,
                ctrl: false,
                shift: false,
                alt: false,
            },
            mark_reviewed: Keybind {
                key: Key::Space,
                ctrl: false,
                shift: false,
                alt: false,
            },
            next_file: Keybind {
                key: Key::ArrowDown,
                ctrl: true,
                shift: false,
                alt: false,
            },
            prev_file: Keybind {
                key: Key::ArrowUp,
                ctrl: true,
                shift: false,
                alt: false,
            },
            quick_picker: Keybind {
                key: Key::P,
                ctrl: true,
                shift: false,
                alt: false,
            },
            toggle_sidebar: Keybind {
                key: Key::B,
                ctrl: true,
                shift: false,
                alt: false,
            },
            fold_all: Keybind {
                key: Key::F,
                ctrl: true,
                shift: true,
                alt: false,
            },
            unfold_all: Keybind {
                key: Key::E,
                ctrl: true,
                shift: true,
                alt: false,
            },
            open_in_editor: Keybind {
                key: Key::O,
                ctrl: true,
                shift: false,
                alt: false,
            },
            copy_path: Keybind {
                key: Key::Y,
                ctrl: true,
                shift: false,
                alt: false,
            },
            toggle_diff_mode: Keybind {
                key: Key::M,
                ctrl: true,
                shift: false,
                alt: false,
            },
            goto_line: Keybind {
                key: Key::G,
                ctrl: true,
                shift: false,
                alt: false,
            },
            find: Keybind {
                key: Key::F,
                ctrl: true,
                shift: false,
                alt: false,
            },
        }
    }
}

// --- TOML deserialization (raw) ---

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSettings {
    font: RawFont,
    colors: RawColors,
    behavior: RawBehavior,
    keybinds: RawKeybinds,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawFont {
    face: Option<String>,
    size: Option<f64>,
    gutter_size: Option<f64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawColors {
    bg_app: Option<String>,
    bg_added: Option<String>,
    bg_removed: Option<String>,
    bg_inline_added: Option<String>,
    bg_inline_removed: Option<String>,
    bg_padding: Option<String>,
    bg_fold: Option<String>,
    bg_header: Option<String>,
    fg_fold_text: Option<String>,
    fg_fold_line: Option<String>,
    fg_gutter: Option<String>,
    fg_gutter_added: Option<String>,
    fg_gutter_removed: Option<String>,
    fg_gutter_separator: Option<String>,
    bg_search_match: Option<String>,
    bg_search_match_current: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawBehavior {
    use_nerdfont_icons: Option<bool>,
    fold_context: Option<u64>,
    fold_expand_step: Option<u64>,
    sidebar_width: Option<f64>,
    theme: Option<String>,
    line_height: Option<f64>,
    fold_row_height: Option<u64>,
    editor: Option<String>,
    default_diff_mode: Option<String>,
    max_diff_lines: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawKeybinds {
    next_hunk: Option<String>,
    prev_hunk: Option<String>,
    mark_reviewed_next: Option<String>,
    mark_reviewed: Option<String>,
    next_file: Option<String>,
    prev_file: Option<String>,
    quick_picker: Option<String>,
    toggle_sidebar: Option<String>,
    fold_all: Option<String>,
    unfold_all: Option<String>,
    open_in_editor: Option<String>,
    copy_path: Option<String>,
    toggle_diff_mode: Option<String>,
    goto_line: Option<String>,
    find: Option<String>,
}

// --- Keybind parsing ---

fn parse_keybind(s: &str, field_name: &str) -> Result<Keybind, String> {
    let s = s.trim();
    let parts: Vec<&str> = s.split('+').collect();
    let mut ctrl = false;
    let mut shift = false;
    let mut alt = false;

    // All parts except the last are modifiers.
    for &part in &parts[..parts.len() - 1] {
        match part.to_lowercase().as_str() {
            "ctrl" => ctrl = true,
            "shift" => shift = true,
            "alt" => alt = true,
            other => {
                return Err(format!(
                    "keybinds.{field_name}: unknown modifier \"{other}\" in \"{s}\""
                ));
            }
        }
    }

    let key_str = parts
        .last()
        .expect("split always yields at least one element");
    let key = parse_key_name(key_str)
        .ok_or_else(|| format!("keybinds.{field_name}: unknown key \"{key_str}\" in \"{s}\""))?;

    Ok(Keybind {
        key,
        ctrl,
        shift,
        alt,
    })
}

fn parse_key_name(s: &str) -> Option<eframe::egui::Key> {
    use eframe::egui::Key;
    // Case-insensitive matching for named keys; single characters match directly.
    match s.to_lowercase().as_str() {
        "enter" | "return" => Some(Key::Enter),
        "space" => Some(Key::Space),
        "escape" | "esc" => Some(Key::Escape),
        "tab" => Some(Key::Tab),
        "backspace" => Some(Key::Backspace),
        "delete" | "del" => Some(Key::Delete),
        "insert" => Some(Key::Insert),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "pageup" | "pgup" => Some(Key::PageUp),
        "pagedown" | "pgdown" => Some(Key::PageDown),
        "up" | "arrowup" => Some(Key::ArrowUp),
        "down" | "arrowdown" => Some(Key::ArrowDown),
        "left" | "arrowleft" => Some(Key::ArrowLeft),
        "right" | "arrowright" => Some(Key::ArrowRight),
        // Single characters
        "." | "period" => Some(Key::Period),
        "," | "comma" => Some(Key::Comma),
        ";" | "semicolon" => Some(Key::Semicolon),
        ":" | "colon" => Some(Key::Colon),
        "-" | "minus" => Some(Key::Minus),
        "=" | "equals" | "plus" => Some(Key::Plus),
        "[" => Some(Key::OpenBracket),
        "]" => Some(Key::CloseBracket),
        "`" | "backtick" => Some(Key::Backtick),
        "/" | "slash" => Some(Key::Slash),
        "\\" | "backslash" => Some(Key::Backslash),
        "?" => Some(Key::Questionmark),
        // Letters (case-insensitive)
        s if s.len() == 1 => {
            let ch = s.chars().next()?;
            match ch {
                'a'..='z' => {
                    // egui::Key::A through Key::Z
                    let idx = (ch as u8) - b'a';
                    let all_keys = [
                        Key::A,
                        Key::B,
                        Key::C,
                        Key::D,
                        Key::E,
                        Key::F,
                        Key::G,
                        Key::H,
                        Key::I,
                        Key::J,
                        Key::K,
                        Key::L,
                        Key::M,
                        Key::N,
                        Key::O,
                        Key::P,
                        Key::Q,
                        Key::R,
                        Key::S,
                        Key::T,
                        Key::U,
                        Key::V,
                        Key::W,
                        Key::X,
                        Key::Y,
                        Key::Z,
                    ];
                    Some(all_keys[idx as usize])
                }
                '0'..='9' => {
                    let all_nums = [
                        Key::Num0,
                        Key::Num1,
                        Key::Num2,
                        Key::Num3,
                        Key::Num4,
                        Key::Num5,
                        Key::Num6,
                        Key::Num7,
                        Key::Num8,
                        Key::Num9,
                    ];
                    Some(all_nums[(ch as u8 - b'0') as usize])
                }
                _ => None,
            }
        }
        // F-keys
        s if s.starts_with('f') && s.len() <= 3 => {
            let num: u8 = s[1..].parse().ok()?;
            match num {
                1 => Some(Key::F1),
                2 => Some(Key::F2),
                3 => Some(Key::F3),
                4 => Some(Key::F4),
                5 => Some(Key::F5),
                6 => Some(Key::F6),
                7 => Some(Key::F7),
                8 => Some(Key::F8),
                9 => Some(Key::F9),
                10 => Some(Key::F10),
                11 => Some(Key::F11),
                12 => Some(Key::F12),
                _ => None,
            }
        }
        _ => None,
    }
}

// --- Validation and conversion ---

impl Settings {
    /// Load settings from the default config path, or return defaults if the file
    /// doesn't exist.
    pub fn load_default() -> Result<Self, String> {
        let path = default_config_path();
        if path.exists() {
            Self::load_from(&path)
        } else {
            Ok(Self::default())
        }
    }

    /// Load and validate settings from a specific TOML file.
    pub fn load_from(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read config file {}: {e}", path.display()))?;
        Self::parse(&content)
    }

    /// Parse and validate settings from a TOML string.
    pub fn parse(toml_str: &str) -> Result<Self, String> {
        let raw: RawSettings =
            toml::from_str(toml_str).map_err(|e| format!("Invalid TOML in settings file: {e}"))?;
        Self::from_raw(raw)
    }

    fn from_raw(raw: RawSettings) -> Result<Self, String> {
        let defaults = Self::default();
        let mut errors: Vec<String> = Vec::new();

        // --- Font ---
        let font_size = raw.font.size.map_or(defaults.font.size, |v| v as f32);
        let gutter_size = raw
            .font
            .gutter_size
            .map_or(defaults.font.gutter_size, |v| v as f32);

        if font_size <= 0.0 {
            errors.push("font.size must be positive".into());
        }
        if gutter_size <= 0.0 {
            errors.push("font.gutter_size must be positive".into());
        }

        let font = FontSettings {
            face: raw.font.face.unwrap_or(defaults.font.face),
            size: font_size,
            gutter_size,
        };

        // --- Colors ---
        let d = defaults.colors;
        let colors = ColorSettings {
            bg_app: parse_color_or(
                raw.colors.bg_app.as_ref(),
                "colors.bg_app",
                d.bg_app,
                &mut errors,
            ),
            bg_added: parse_color_or(
                raw.colors.bg_added.as_ref(),
                "colors.bg_added",
                d.bg_added,
                &mut errors,
            ),
            bg_removed: parse_color_or(
                raw.colors.bg_removed.as_ref(),
                "colors.bg_removed",
                d.bg_removed,
                &mut errors,
            ),
            bg_inline_added: parse_color_or(
                raw.colors.bg_inline_added.as_ref(),
                "colors.bg_inline_added",
                d.bg_inline_added,
                &mut errors,
            ),
            bg_inline_removed: parse_color_or(
                raw.colors.bg_inline_removed.as_ref(),
                "colors.bg_inline_removed",
                d.bg_inline_removed,
                &mut errors,
            ),
            bg_padding: parse_color_or(
                raw.colors.bg_padding.as_ref(),
                "colors.bg_padding",
                d.bg_padding,
                &mut errors,
            ),
            bg_fold: parse_color_or(
                raw.colors.bg_fold.as_ref(),
                "colors.bg_fold",
                d.bg_fold,
                &mut errors,
            ),
            bg_header: parse_color_or(
                raw.colors.bg_header.as_ref(),
                "colors.bg_header",
                d.bg_header,
                &mut errors,
            ),
            fg_fold_text: parse_color_or(
                raw.colors.fg_fold_text.as_ref(),
                "colors.fg_fold_text",
                d.fg_fold_text,
                &mut errors,
            ),
            fg_fold_line: parse_color_or(
                raw.colors.fg_fold_line.as_ref(),
                "colors.fg_fold_line",
                d.fg_fold_line,
                &mut errors,
            ),
            fg_gutter: parse_color_or(
                raw.colors.fg_gutter.as_ref(),
                "colors.fg_gutter",
                d.fg_gutter,
                &mut errors,
            ),
            fg_gutter_added: parse_color_or(
                raw.colors.fg_gutter_added.as_ref(),
                "colors.fg_gutter_added",
                d.fg_gutter_added,
                &mut errors,
            ),
            fg_gutter_removed: parse_color_or(
                raw.colors.fg_gutter_removed.as_ref(),
                "colors.fg_gutter_removed",
                d.fg_gutter_removed,
                &mut errors,
            ),
            fg_gutter_separator: parse_color_or(
                raw.colors.fg_gutter_separator.as_ref(),
                "colors.fg_gutter_separator",
                d.fg_gutter_separator,
                &mut errors,
            ),
            bg_search_match: parse_color_or(
                raw.colors.bg_search_match.as_ref(),
                "colors.bg_search_match",
                d.bg_search_match,
                &mut errors,
            ),
            bg_search_match_current: parse_color_or(
                raw.colors.bg_search_match_current.as_ref(),
                "colors.bg_search_match_current",
                d.bg_search_match_current,
                &mut errors,
            ),
        };

        // --- Behavior ---
        let line_height_mult = raw
            .behavior
            .line_height
            .map_or(DEFAULT_LINE_HEIGHT_MULT, |v| v as f32);
        let line_height = (font_size * line_height_mult).round();
        let fold_row_height = raw
            .behavior
            .fold_row_height
            .map_or(defaults.behavior.fold_row_height, |v| v as usize);
        let fold_context = raw
            .behavior
            .fold_context
            .map_or(defaults.behavior.fold_context, |v| v as usize);
        let fold_expand_step = raw
            .behavior
            .fold_expand_step
            .map_or(defaults.behavior.fold_expand_step, |v| v as usize);
        let max_diff_lines = raw
            .behavior
            .max_diff_lines
            .map_or(defaults.behavior.max_diff_lines, |v| v as usize);

        if line_height_mult < 1.0 {
            errors.push("behavior.line_height must be at least 1.0".into());
        }
        if fold_row_height == 0 {
            errors.push("behavior.fold_row_height must be at least 1".into());
        }
        if fold_context == 0 {
            errors.push("behavior.fold_context must be at least 1".into());
        }
        if fold_expand_step == 0 {
            errors.push("behavior.fold_expand_step must be at least 1".into());
        }

        let theme = raw
            .behavior
            .theme
            .filter(|s| !s.is_empty())
            .map(|s| expand_tilde(&s));

        let behavior = BehaviorSettings {
            use_nerdfont_icons: raw
                .behavior
                .use_nerdfont_icons
                .unwrap_or(defaults.behavior.use_nerdfont_icons),
            fold_context,
            fold_expand_step,
            sidebar_width: raw
                .behavior
                .sidebar_width
                .map_or(defaults.behavior.sidebar_width, |v| v as f32),
            theme,
            line_height,
            fold_row_height,
            editor: raw.behavior.editor.filter(|s| !s.is_empty()),
            default_diff_mode: match raw.behavior.default_diff_mode.as_deref() {
                Some("unified") => DiffModePreference::Unified,
                Some("auto") | None => DiffModePreference::Auto,
                Some("side-by-side") => DiffModePreference::SideBySide,
                Some(other) => {
                    errors.push(format!("behavior.default_diff_mode: unknown mode '{other}', expected 'side-by-side', 'unified' or 'auto'"));
                    defaults.behavior.default_diff_mode
                }
            },
            max_diff_lines,
        };

        // --- Keybinds ---
        let dk = defaults.keybinds;
        let keybinds = KeybindSettings {
            next_hunk: parse_kb_or(
                raw.keybinds.next_hunk.as_ref(),
                "next_hunk",
                dk.next_hunk,
                &mut errors,
            ),
            prev_hunk: parse_kb_or(
                raw.keybinds.prev_hunk.as_ref(),
                "prev_hunk",
                dk.prev_hunk,
                &mut errors,
            ),
            mark_reviewed_next: parse_kb_or(
                raw.keybinds.mark_reviewed_next.as_ref(),
                "mark_reviewed_next",
                dk.mark_reviewed_next,
                &mut errors,
            ),
            mark_reviewed: parse_kb_or(
                raw.keybinds.mark_reviewed.as_ref(),
                "mark_reviewed",
                dk.mark_reviewed,
                &mut errors,
            ),
            next_file: parse_kb_or(
                raw.keybinds.next_file.as_ref(),
                "next_file",
                dk.next_file,
                &mut errors,
            ),
            prev_file: parse_kb_or(
                raw.keybinds.prev_file.as_ref(),
                "prev_file",
                dk.prev_file,
                &mut errors,
            ),
            quick_picker: parse_kb_or(
                raw.keybinds.quick_picker.as_ref(),
                "quick_picker",
                dk.quick_picker,
                &mut errors,
            ),
            toggle_sidebar: parse_kb_or(
                raw.keybinds.toggle_sidebar.as_ref(),
                "toggle_sidebar",
                dk.toggle_sidebar,
                &mut errors,
            ),
            fold_all: parse_kb_or(
                raw.keybinds.fold_all.as_ref(),
                "fold_all",
                dk.fold_all,
                &mut errors,
            ),
            unfold_all: parse_kb_or(
                raw.keybinds.unfold_all.as_ref(),
                "unfold_all",
                dk.unfold_all,
                &mut errors,
            ),
            open_in_editor: parse_kb_or(
                raw.keybinds.open_in_editor.as_ref(),
                "open_in_editor",
                dk.open_in_editor,
                &mut errors,
            ),
            copy_path: parse_kb_or(
                raw.keybinds.copy_path.as_ref(),
                "copy_path",
                dk.copy_path,
                &mut errors,
            ),
            toggle_diff_mode: parse_kb_or(
                raw.keybinds.toggle_diff_mode.as_ref(),
                "toggle_diff_mode",
                dk.toggle_diff_mode,
                &mut errors,
            ),
            goto_line: parse_kb_or(
                raw.keybinds.goto_line.as_ref(),
                "goto_line",
                dk.goto_line,
                &mut errors,
            ),
            find: parse_kb_or(raw.keybinds.find.as_ref(), "find", dk.find, &mut errors),
        };

        if !errors.is_empty() {
            return Err(format!(
                "Settings validation errors:\n  - {}",
                errors.join("\n  - ")
            ));
        }

        Ok(Self {
            font,
            colors,
            behavior,
            keybinds,
        })
    }
}

fn parse_color_or(
    raw: Option<&String>,
    field_name: &str,
    default: Rgba,
    errors: &mut Vec<String>,
) -> Rgba {
    match raw {
        Some(s) => match parse_hex_color(s, field_name) {
            Ok(c) => c,
            Err(e) => {
                errors.push(e);
                default
            }
        },
        None => default,
    }
}

fn parse_kb_or(
    raw: Option<&String>,
    field_name: &str,
    default: Keybind,
    errors: &mut Vec<String>,
) -> Keybind {
    match raw {
        Some(s) => match parse_keybind(s, field_name) {
            Ok(kb) => kb,
            Err(e) => {
                errors.push(e);
                default
            }
        },
        None => default,
    }
}

/// Returns the default config file path, respecting `XDG_CONFIG_HOME`.
pub fn default_config_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg)
            .join(env!("CARGO_PKG_NAME"))
            .join("config.toml")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home)
            .join(".config")
            .join(env!("CARGO_PKG_NAME"))
            .join("config.toml")
    } else {
        // Last resort
        PathBuf::from(".config")
            .join(env!("CARGO_PKG_NAME"))
            .join("config.toml")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_defaults_are_valid() {
        let s = Settings::default();
        assert_eq!(s.font.face, "monospace");
        assert_eq!(s.behavior.fold_context, 5);
        assert!(s.behavior.use_nerdfont_icons);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_empty_toml_gives_defaults() {
        let s = Settings::parse("").unwrap();
        assert_eq!(s.font.size, 14.0);
        assert_eq!(s.colors.bg_app.r, 0x2C);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_partial_override() {
        let toml = r"
[font]
size = 16.0

[behavior]
fold_context = 3
";
        let s = Settings::parse(toml).unwrap();
        assert_eq!(s.font.size, 16.0);
        assert_eq!(s.font.face, "monospace"); // default kept
        assert_eq!(s.behavior.fold_context, 3);
        assert_eq!(s.behavior.fold_expand_step, 20); // default kept
    }

    #[test]
    fn test_default_diff_mode_parsing() {
        let toml = r#"
[behavior]
default_diff_mode = "side-by-side"
"#;
        let s = Settings::parse(toml).unwrap();
        assert_eq!(s.behavior.default_diff_mode, DiffModePreference::SideBySide);

        let s = Settings::parse("").unwrap();
        assert_eq!(s.behavior.default_diff_mode, DiffModePreference::Auto);

        let toml = r#"
[behavior]
default_diff_mode = "vertical"
"#;
        let err = Settings::parse(toml).unwrap_err();
        assert!(err.contains("behavior.default_diff_mode"));
        assert!(err.contains("'auto'"));
    }

    #[test]
    fn test_color_parsing() {
        let toml = r##"
[colors]
bg_app = "#FF0000"
"##;
        let s = Settings::parse(toml).unwrap();
        assert_eq!(s.colors.bg_app.r, 0xFF);
        assert_eq!(s.colors.bg_app.g, 0x00);
        assert_eq!(s.colors.bg_app.b, 0x00);
    }

    #[test]
    fn test_invalid_color_error() {
        let toml = r#"
[colors]
bg_app = "not-a-color"
"#;
        let err = Settings::parse(toml).unwrap_err();
        assert!(err.contains("colors.bg_app"));
        assert!(err.contains("#RRGGBB"));
    }

    #[test]
    fn test_keybind_parsing() {
        let toml = r#"
[keybinds]
next_hunk = "n"
fold_all = "Ctrl+Shift+F"
prev_file = "Alt+Up"
"#;
        let s = Settings::parse(toml).unwrap();
        assert_eq!(s.keybinds.next_hunk.key, eframe::egui::Key::N);
        assert!(!s.keybinds.next_hunk.ctrl);
        assert!(s.keybinds.fold_all.ctrl);
        assert!(s.keybinds.fold_all.shift);
        assert_eq!(s.keybinds.fold_all.key, eframe::egui::Key::F);
        assert!(s.keybinds.prev_file.alt);
        assert_eq!(s.keybinds.prev_file.key, eframe::egui::Key::ArrowUp);
    }

    #[test]
    fn test_invalid_keybind_error() {
        let toml = r#"
[keybinds]
next_hunk = "Ctrl+Banana"
"#;
        let err = Settings::parse(toml).unwrap_err();
        assert!(err.contains("next_hunk"));
        assert!(err.contains("Banana"));
    }

    #[test]
    fn test_invalid_modifier_error() {
        let toml = r#"
[keybinds]
next_hunk = "Super+A"
"#;
        let err = Settings::parse(toml).unwrap_err();
        assert!(err.contains("unknown modifier"));
        assert!(err.contains("Super"));
    }

    #[test]
    fn test_behavior_validation() {
        let toml = r"
[behavior]
line_height = 0.5
fold_row_height = 0
fold_context = 0
";
        let err = Settings::parse(toml).unwrap_err();
        assert!(err.contains("line_height must be at least 1.0"));
        assert!(err.contains("fold_row_height must be at least 1"));
        assert!(err.contains("fold_context must be at least 1"));
    }

    #[test]
    fn test_font_validation() {
        let toml = r"
[font]
size = -1.0
";
        let err = Settings::parse(toml).unwrap_err();
        assert!(err.contains("font.size must be positive"));
    }

    #[test]
    fn test_theme_empty_string_is_none() {
        let toml = r#"
[behavior]
theme = ""
"#;
        let s = Settings::parse(toml).unwrap();
        assert!(s.behavior.theme.is_none());
    }

    #[test]
    fn test_theme_nonempty_is_some() {
        let toml = r#"
[behavior]
theme = "/path/to/dracula.tmTheme"
"#;
        let s = Settings::parse(toml).unwrap();
        assert_eq!(
            s.behavior.theme.unwrap().to_str().unwrap(),
            "/path/to/dracula.tmTheme"
        );
    }

    #[test]
    #[allow(unsafe_code)]
    fn test_xdg_config_home() {
        // SAFETY: This test runs single-threaded; no other thread reads this env var.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", "/tmp/test-xdg") };
        let p = default_config_path();
        assert_eq!(p, PathBuf::from("/tmp/test-xdg/revisa/config.toml"));
        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
    }

    #[test]
    fn test_sidebar_width_zero() {
        let toml = r"
[behavior]
sidebar_width = 0
";
        let s = Settings::parse(toml).unwrap();
        assert!((s.behavior.sidebar_width - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_rgb_to_egui() {
        let rgb = Rgba {
            r: 0xFF,
            g: 0x00,
            b: 0xAB,
            a: 255,
        };
        let c = rgb.to_egui();
        assert_eq!(
            c,
            eframe::egui::Color32::from_rgba_unmultiplied(0xFF, 0x00, 0xAB, 0xFF)
        );
    }

    #[test]
    fn test_keybind_matches() {
        use eframe::egui::{Key, Modifiers};
        let kb = Keybind {
            key: Key::P,
            ctrl: true,
            shift: false,
            alt: false,
        };
        assert!(kb.matches(
            Key::P,
            Modifiers {
                ctrl: true,
                ..Default::default()
            }
        ));
        assert!(!kb.matches(Key::P, Modifiers::NONE));
        assert!(!kb.matches(
            Key::Q,
            Modifiers {
                ctrl: true,
                ..Default::default()
            }
        ));
    }

    #[test]
    fn test_keybind_matches_strict() {
        use eframe::egui::{Key, Modifiers};
        let kb = Keybind {
            key: Key::Period,
            ctrl: false,
            shift: false,
            alt: false,
        };
        assert!(kb.matches_strict(Key::Period, Modifiers::NONE));
        assert!(!kb.matches_strict(
            Key::Period,
            Modifiers {
                ctrl: true,
                ..Default::default()
            }
        ));

        // With modifiers required
        let kb2 = Keybind {
            key: Key::F,
            ctrl: true,
            shift: true,
            alt: false,
        };
        assert!(kb2.matches_strict(
            Key::F,
            Modifiers {
                ctrl: true,
                shift: true,
                ..Default::default()
            }
        ));
    }

    #[test]
    fn test_theme_tilde_expansion() {
        let toml = r#"
[behavior]
theme = "~/themes/dracula.tmTheme"
"#;
        let s = Settings::parse(toml).unwrap();
        let home = std::env::var("HOME").unwrap();
        let expected = format!("{home}/themes/dracula.tmTheme");
        assert_eq!(s.behavior.theme.unwrap().to_str().unwrap(), expected);
    }

    #[test]
    fn test_expand_tilde_no_tilde() {
        assert_eq!(
            expand_tilde("/absolute/path"),
            PathBuf::from("/absolute/path")
        );
        assert_eq!(
            expand_tilde("relative/path"),
            PathBuf::from("relative/path")
        );
    }

    #[test]
    fn test_expand_tilde_bare() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_tilde("~"), PathBuf::from(&home));
    }
}

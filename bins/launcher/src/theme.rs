// SPDX-License-Identifier: GPL-3.0-or-later
//! The launcher's look, taken from the project website: the crest colours (a near-black violet
//! ground, violet for what the player presses, crystal for focus, teal for links), flat tone
//! layers divided by 1px rules, 3px corners, a light serif for headings and a plain sans for
//! facts. Night is the website's own theme (the default); day is a light theme of the same
//! colours. Only installed system fonts are used: the website's own fallbacks (Georgia and the
//! system sans) and their Linux equivalents.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Shadow, Stroke,
    TextStyle, Visuals,
};
use serde::{Deserialize, Serialize};

/// The theme the player picked; night is the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    /// The website's dark violet theme.
    #[default]
    Night,
    /// A light theme of the same colours.
    Day,
}

impl ThemeChoice {
    /// Both choices, in the order the settings show them.
    pub const ALL: [Self; 2] = [Self::Night, Self::Day];

    /// The name shown on the theme button.
    pub fn label(self) -> &'static str {
        match self {
            Self::Night => "Night",
            Self::Day => "Day",
        }
    }

    fn egui_theme(self) -> egui::Theme {
        match self {
            Self::Night => egui::Theme::Dark,
            Self::Day => egui::Theme::Light,
        }
    }
}

/// The colours of one theme, named as on the website.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// The deepest layer: the top bar and the field fills.
    pub abyss: Color32,
    /// The window background.
    pub ground: Color32,
    /// Tracks and quiet fills.
    pub stone: Color32,
    /// The selected choice.
    pub stone_hi: Color32,
    /// Borders of outlined controls, and the text selection.
    pub vein: Color32,
    /// 1px divider lines.
    pub rule: Color32,
    /// The one action colour: the main button fill and a hovered border.
    pub violet: Color32,
    /// The main button fill on hover.
    pub violet_hover: Color32,
    /// Text on violet.
    pub on_violet: Color32,
    /// Response: the focus outline, inline code and the one stressed phrase.
    pub crystal: Color32,
    /// Links.
    pub link: Color32,
    /// Borders of text fields.
    pub edge: Color32,
    /// Text.
    pub ink: Color32,
    /// Secondary text: meta lines, labels and hints.
    pub ink_2: Color32,
    /// Ready state.
    pub good: Color32,
    /// Busy state and downtime notes.
    pub warn: Color32,
    /// Error state.
    pub bad: Color32,
}

/// The night theme: the website's colours.
pub const NIGHT: Palette = Palette {
    abyss: Color32::from_rgb(0x05, 0x04, 0x09),
    ground: Color32::from_rgb(0x10, 0x0e, 0x18),
    stone: Color32::from_rgb(0x24, 0x1b, 0x35),
    stone_hi: Color32::from_rgb(0x33, 0x24, 0x4c),
    vein: Color32::from_rgb(0x4c, 0x2a, 0x60),
    rule: Color32::from_rgb(0x2c, 0x22, 0x40),
    violet: Color32::from_rgb(0xa3, 0x47, 0xbe),
    violet_hover: Color32::from_rgb(0x8f, 0x3a, 0xa8),
    on_violet: Color32::WHITE,
    crystal: Color32::from_rgb(0xed, 0x8e, 0xf2),
    link: Color32::from_rgb(0x7c, 0xc3, 0xd8),
    edge: Color32::from_rgb(0x7a, 0x55, 0x98),
    ink: Color32::from_rgb(0xec, 0xe6, 0xf2),
    ink_2: Color32::from_rgb(0xb9, 0xae, 0xcb),
    good: Color32::from_rgb(0x6c, 0xc4, 0x89),
    warn: Color32::from_rgb(0xe0, 0xb4, 0x5a),
    bad: Color32::from_rgb(0xe2, 0x67, 0x5f),
};

/// The day theme: the crest colours on light violet paper, darkened to keep AA contrast.
pub const DAY: Palette = Palette {
    abyss: Color32::from_rgb(0xfc, 0xfa, 0xff),
    ground: Color32::from_rgb(0xf4, 0xf0, 0xf8),
    stone: Color32::from_rgb(0xe6, 0xde, 0xef),
    stone_hi: Color32::from_rgb(0xdc, 0xcf, 0xe9),
    vein: Color32::from_rgb(0xb3, 0x9b, 0xc9),
    rule: Color32::from_rgb(0xdd, 0xd3, 0xe8),
    violet: Color32::from_rgb(0x8a, 0x2f, 0xa5),
    violet_hover: Color32::from_rgb(0x74, 0x25, 0x8b),
    on_violet: Color32::WHITE,
    crystal: Color32::from_rgb(0xa0, 0x1f, 0x9d),
    link: Color32::from_rgb(0x1d, 0x6b, 0x80),
    edge: Color32::from_rgb(0x7a, 0x55, 0x98),
    ink: Color32::from_rgb(0x1d, 0x16, 0x26),
    ink_2: Color32::from_rgb(0x5b, 0x4e, 0x6c),
    good: Color32::from_rgb(0x2b, 0x76, 0x4a),
    warn: Color32::from_rgb(0x7a, 0x56, 0x00),
    bad: Color32::from_rgb(0xb3, 0x26, 0x1e),
};

impl Palette {
    /// The palette of the theme egui shows now.
    pub fn current(visuals: &Visuals) -> &'static Self {
        if visuals.dark_mode { &NIGHT } else { &DAY }
    }
}

/// Corner of buttons, fields, badges and pictures.
pub const CORNER: u8 = 3;
/// Width of every border and divider.
pub const LINE_WIDTH: f32 = 1.0;
/// Width of the focus outline.
const FOCUS_WIDTH: f32 = 2.0;
/// Opacity factor of a disabled control.
const DISABLED_ALPHA: f32 = 0.45;
/// Space between items, and inside buttons.
const ITEM_SPACING: [f32; 2] = [8.0, 8.0];
const BUTTON_PADDING: [f32; 2] = [16.0, 7.0];
/// Height of a control.
pub const CONTROL_HEIGHT: f32 = 32.0;

/// The egui visuals of a palette.
pub fn visuals(palette: &Palette) -> Visuals {
    let mut visuals = if palette == &NIGHT {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    let corner = CornerRadius::same(CORNER);
    let control = |weak_fill: Color32, border: Color32| egui::style::WidgetVisuals {
        bg_fill: palette.abyss,
        weak_bg_fill: weak_fill,
        bg_stroke: Stroke::new(LINE_WIDTH, border),
        corner_radius: corner,
        fg_stroke: Stroke::new(LINE_WIDTH, palette.ink),
        expansion: 0.0,
    };
    let widgets = &mut visuals.widgets;
    widgets.noninteractive = control(palette.ground, palette.rule);
    widgets.inactive = control(Color32::TRANSPARENT, palette.vein);
    widgets.hovered = control(Color32::TRANSPARENT, palette.violet);
    widgets.active = control(palette.stone, palette.violet);
    widgets.open = widgets.active;
    visuals.selection.bg_fill = palette.vein;
    visuals.selection.stroke = Stroke::new(FOCUS_WIDTH, palette.crystal);
    visuals.text_cursor.stroke = Stroke::new(FOCUS_WIDTH, palette.ink);
    visuals.hyperlink_color = palette.link;
    visuals.faint_bg_color = palette.stone;
    visuals.extreme_bg_color = palette.abyss;
    visuals.text_edit_bg_color = Some(palette.abyss);
    visuals.code_bg_color = palette.stone;
    visuals.warn_fg_color = palette.warn;
    visuals.error_fg_color = palette.bad;
    visuals.window_fill = palette.stone;
    visuals.window_stroke = Stroke::new(LINE_WIDTH, palette.vein);
    visuals.window_corner_radius = corner;
    visuals.menu_corner_radius = corner;
    visuals.window_shadow = Shadow::NONE;
    visuals.popup_shadow = Shadow::NONE;
    visuals.panel_fill = palette.ground;
    visuals.striped = false;
    visuals.disabled_alpha = DISABLED_ALPHA;
    visuals
}

/// Family of the light serif of headings (the website's display face).
pub const SERIF: &str = "serif";
/// Family of the semibold sans of buttons, labels and badges.
pub const SEMIBOLD: &str = "semibold";
/// Font name of the body face and of the data face.
const BODY_FACE: &str = "body";
const DATA_FACE: &str = "data";
/// Text style of the top bar wordmark.
pub const WORDMARK_STYLE: &str = "wordmark";
/// Text style of a section heading ("News", "Servers").
pub const TITLE_STYLE: &str = "title";
/// Text style of a post title.
pub const POST_TITLE_STYLE: &str = "post title";
/// Text style of a field label and a badge.
pub const LABEL_STYLE: &str = "label";

/// Font size of each role.
const BODY_SIZE: f32 = 14.0;
const SMALL_SIZE: f32 = 12.5;
const BUTTON_SIZE: f32 = 14.0;
const HEADING_SIZE: f32 = 30.0;
const WORDMARK_SIZE: f32 = 21.0;
const TITLE_SIZE: f32 = 22.0;
const POST_TITLE_SIZE: f32 = 18.0;
const LABEL_SIZE: f32 = 12.5;
const DATA_SIZE: f32 = 12.5;

fn family(name: &str) -> FontFamily {
    FontFamily::Name(name.into())
}

/// Spacing and text sizes, the same in both themes.
pub fn apply_style(style: &mut egui::Style) {
    style.spacing.item_spacing = ITEM_SPACING.into();
    style.spacing.button_padding = BUTTON_PADDING.into();
    style.spacing.interact_size.y = CONTROL_HEIGHT;
    style.text_styles = [
        (TextStyle::Body, FontId::proportional(BODY_SIZE)),
        (TextStyle::Small, FontId::proportional(SMALL_SIZE)),
        (
            TextStyle::Button,
            FontId::new(BUTTON_SIZE, family(SEMIBOLD)),
        ),
        (TextStyle::Heading, FontId::new(HEADING_SIZE, family(SERIF))),
        (TextStyle::Monospace, FontId::monospace(DATA_SIZE)),
        (
            TextStyle::Name(WORDMARK_STYLE.into()),
            FontId::new(WORDMARK_SIZE, family(SERIF)),
        ),
        (
            TextStyle::Name(TITLE_STYLE.into()),
            FontId::new(TITLE_SIZE, family(SERIF)),
        ),
        (
            TextStyle::Name(POST_TITLE_STYLE.into()),
            FontId::new(POST_TITLE_SIZE, family(SERIF)),
        ),
        (
            TextStyle::Name(LABEL_STYLE.into()),
            FontId::new(LABEL_SIZE, family(SEMIBOLD)),
        ),
    ]
    .into();
}

/// A font role and the installed files that can fill it, best first.
struct FontRole {
    name: &'static str,
    files: &'static [&'static str],
}

/// Installed faces per role on Linux: Noto (light serif for headings), then DejaVu.
#[cfg(unix)]
const FONT_ROLES: [FontRole; 4] = [
    FontRole {
        name: BODY_FACE,
        files: &[
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ],
    },
    FontRole {
        name: SEMIBOLD,
        files: &[
            "/usr/share/fonts/truetype/noto/NotoSans-SemiBold.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        ],
    },
    FontRole {
        name: SERIF,
        files: &[
            "/usr/share/fonts/truetype/noto/NotoSerif-Light.ttf",
            "/usr/share/fonts/truetype/noto/NotoSerif-Regular.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf",
        ],
    },
    FontRole {
        name: DATA_FACE,
        files: &[
            "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        ],
    },
];

/// Installed faces per role on Windows, under the Windows font folder: Segoe UI, Georgia (the
/// website's serif fallback), Cascadia Mono or Consolas.
#[cfg(windows)]
const FONT_ROLES: [FontRole; 4] = [
    FontRole {
        name: BODY_FACE,
        files: &["segoeui.ttf"],
    },
    FontRole {
        name: SEMIBOLD,
        files: &["seguisb.ttf", "segoeuib.ttf"],
    },
    FontRole {
        name: SERIF,
        files: &["georgia.ttf", "times.ttf"],
    },
    FontRole {
        name: DATA_FACE,
        files: &["CascadiaMono.ttf", "consola.ttf"],
    },
];

/// Windows folder variable and the font folder under it.
#[cfg(windows)]
const WINDOWS_DIR_ENV: &str = "WINDIR";
#[cfg(windows)]
const WINDOWS_FONTS_DIR: &str = "Fonts";

/// The folder the font file names of [`FONT_ROLES`] are relative to.
fn font_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os(WINDOWS_DIR_ENV)
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(WINDOWS_FONTS_DIR)
    }
    #[cfg(unix)]
    {
        PathBuf::new()
    }
}

/// The first readable file of `files` under `dir`.
fn first_font(dir: &Path, files: &[&str]) -> Option<Vec<u8>> {
    files
        .iter()
        .find_map(|file| std::fs::read(dir.join(file)).ok())
}

/// egui's built-in faces with each found system face first in its family. The serif and
/// semibold families start as copies of the body family, so a role with no installed face uses
/// the body faces.
fn font_definitions(found: &[(&'static str, Vec<u8>)]) -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let proportional = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    for name in [SERIF, SEMIBOLD] {
        fonts.families.insert(family(name), proportional.clone());
    }
    for (role, data) in found {
        fonts.font_data.insert(
            (*role).to_owned(),
            Arc::new(FontData::from_owned(data.clone())),
        );
        let target = match *role {
            SERIF | SEMIBOLD => family(role),
            DATA_FACE => FontFamily::Monospace,
            _ => FontFamily::Proportional,
        };
        fonts
            .families
            .entry(target)
            .or_default()
            .insert(0, (*role).to_owned());
    }
    fonts
}

/// Sets the fonts, the style and both themes, and shows `choice`.
pub fn install(ctx: &egui::Context, choice: ThemeChoice) {
    let dir = font_dir();
    let found: Vec<_> = FONT_ROLES
        .iter()
        .filter_map(|role| first_font(&dir, role.files).map(|data| (role.name, data)))
        .collect();
    ctx.set_fonts(font_definitions(&found));
    ctx.set_visuals_of(egui::Theme::Dark, visuals(&NIGHT));
    ctx.set_visuals_of(egui::Theme::Light, visuals(&DAY));
    ctx.all_styles_mut(apply_style);
    show(ctx, choice);
}

/// Shows the theme of `choice`.
pub fn show(ctx: &egui::Context, choice: ThemeChoice) {
    ctx.set_theme(choice.egui_theme());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG 2.1 AA: normal text, and large text or a control border.
    const TEXT_CONTRAST: f64 = 4.5;
    const LARGE_CONTRAST: f64 = 3.0;
    const SRGB_KNEE: f64 = 0.040_45;
    const SRGB_LINEAR_SLOPE: f64 = 12.92;
    const SRGB_OFFSET: f64 = 0.055;
    const SRGB_GAMMA: f64 = 2.4;
    const LUMINANCE_WEIGHTS: [f64; 3] = [0.2126, 0.7152, 0.0722];
    const CONTRAST_OFFSET: f64 = 0.05;
    const CHANNEL_MAX: f64 = 255.0;

    fn luminance(color: Color32) -> f64 {
        let linear = |channel: u8| {
            let value = f64::from(channel) / CHANNEL_MAX;
            if value <= SRGB_KNEE {
                value / SRGB_LINEAR_SLOPE
            } else {
                ((value + SRGB_OFFSET) / (1.0 + SRGB_OFFSET)).powf(SRGB_GAMMA)
            }
        };
        let [r, g, b] = LUMINANCE_WEIGHTS;
        r * linear(color.r()) + g * linear(color.g()) + b * linear(color.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (light, dark) = {
            let (x, y) = (luminance(a), luminance(b));
            if x > y { (x, y) } else { (y, x) }
        };
        (light + CONTRAST_OFFSET) / (dark + CONTRAST_OFFSET)
    }

    #[test]
    fn text_colours_pass_aa_where_they_show_in_both_themes() {
        for palette in [&NIGHT, &DAY] {
            for (name, color) in [("ink", palette.ink), ("ink_2", palette.ink_2)] {
                assert!(
                    contrast(color, palette.stone_hi) >= TEXT_CONTRAST,
                    "{name} on the selected choice"
                );
            }
            for layer in [palette.abyss, palette.ground] {
                for (name, color) in [
                    ("ink", palette.ink),
                    ("ink_2", palette.ink_2),
                    ("crystal", palette.crystal),
                    ("link", palette.link),
                    ("good", palette.good),
                    ("warn", palette.warn),
                    ("bad", palette.bad),
                ] {
                    assert!(
                        contrast(color, layer) >= TEXT_CONTRAST,
                        "{name} on {layer:?}"
                    );
                }
            }
            for fill in [palette.violet, palette.violet_hover] {
                assert!(contrast(palette.on_violet, fill) >= TEXT_CONTRAST);
            }
            assert!(contrast(palette.edge, palette.abyss) >= LARGE_CONTRAST);
            assert!(contrast(palette.crystal, palette.ground) >= LARGE_CONTRAST);
        }
    }

    #[test]
    fn night_uses_the_website_colours() {
        assert_eq!(NIGHT.ground, Color32::from_hex("#100e18").expect("hex"));
        assert_eq!(NIGHT.violet, Color32::from_hex("#a347be").expect("hex"));
        assert_eq!(NIGHT.crystal, Color32::from_hex("#ed8ef2").expect("hex"));
        assert_eq!(NIGHT.link, Color32::from_hex("#7cc3d8").expect("hex"));
    }

    #[test]
    fn each_theme_has_its_own_visuals() {
        assert!(visuals(&NIGHT).dark_mode);
        assert!(!visuals(&DAY).dark_mode);
        assert_eq!(visuals(&NIGHT).panel_fill, NIGHT.ground);
        assert_eq!(visuals(&DAY).panel_fill, DAY.ground);
        assert_eq!(visuals(&NIGHT).hyperlink_color, NIGHT.link);
        assert_eq!(visuals(&NIGHT).selection.stroke.color, NIGHT.crystal);
        assert_eq!(Palette::current(&visuals(&DAY)), &DAY);
        assert_eq!(Palette::current(&visuals(&NIGHT)), &NIGHT);
    }

    #[test]
    fn the_first_readable_font_file_is_taken() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("second.ttf"), b"second").expect("font");
        std::fs::write(dir.path().join("third.ttf"), b"third").expect("font");
        assert_eq!(
            first_font(dir.path(), &["first.ttf", "second.ttf", "third.ttf"]),
            Some(b"second".to_vec())
        );
        assert_eq!(first_font(dir.path(), &["none.ttf"]), None);
    }

    #[test]
    fn found_faces_go_first_and_every_family_exists() {
        let none = font_definitions(&[]);
        for name in [SERIF, SEMIBOLD] {
            assert_eq!(
                none.families.get(&family(name)),
                none.families.get(&FontFamily::Proportional)
            );
        }
        let fonts = font_definitions(&[(SERIF, b"s".to_vec()), (DATA_FACE, b"d".to_vec())]);
        assert_eq!(fonts.families[&family(SERIF)][0], SERIF);
        assert_eq!(fonts.families[&FontFamily::Monospace][0], DATA_FACE);
        assert_ne!(fonts.families[&FontFamily::Proportional][0], SERIF);
        assert_ne!(fonts.families[&family(SEMIBOLD)][0], SERIF);
    }

    #[test]
    fn the_theme_choice_is_saved_in_lower_case() {
        assert_eq!(
            serde_json::to_string(&ThemeChoice::Day).expect("json"),
            "\"day\""
        );
        assert_eq!(ThemeChoice::default(), ThemeChoice::Night);
    }
}

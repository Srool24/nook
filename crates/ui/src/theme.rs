//! Design tokens for the "liquid glass bento" language, plus font setup.

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId};
use serde::{Deserialize, Serialize};

/// Fallback accent when no wallpaper color is available: electric violet.
pub const FALLBACK_ACCENT: Color32 = Color32::from_rgb(0x7C, 0x5C, 0xFF);

/// 8 px spacing grid.
pub const SPACE: f32 = 8.0;
pub const TILE_RADIUS: f32 = 16.0;
pub const TILE_PADDING: f32 = 12.0;

pub const SIZE_LABEL: f32 = 11.0;
pub const SIZE_BODY: f32 = 13.0;
pub const SIZE_BIG: f32 = 28.0;
pub const SIZE_ICON: f32 = 18.0;

pub const FAMILY_SEMIBOLD: &str = "nook-semibold";
pub const FAMILY_ICON_FILL: &str = "phosphor-fill";
pub const FAMILY_ICON_REGULAR: &str = "phosphor";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Dark,
    Light,
    #[default]
    Auto,
}

/// User styling on top of the built-in palette (see the settings page).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    /// Opacity of the glass over the blurred backdrop, 0..1.
    pub glass_opacity: f32,
    /// Color the glass is tinted toward; `None` keeps the neutral glass.
    pub tint: Option<Color32>,
    /// Film-grain strength, 0..1.
    pub grain: f32,
    /// Size of one grain speck in physical pixels.
    pub grain_px: f32,
}

impl Default for Look {
    fn default() -> Self {
        Self {
            glass_opacity: 0.70,
            tint: None,
            grain: 0.35,
            grain_px: 1.0,
        }
    }
}

/// Grain alpha at full strength. The grain is two-sided (light and dark
/// specks), so it reads at a lower alpha than a white-only one would.
const GRAIN_MAX_DARK: f32 = 22.0;
const GRAIN_MAX_LIGHT: f32 = 16.0;

/// Resolved colors for one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub dark: bool,
    pub accent: Color32,
    /// Glass fill, top and bottom of the vertical gradient.
    pub glass_top: Color32,
    pub glass_bottom: Color32,
    /// 1 px rim around the shell (stands in for the outer shadow, see NOTES).
    pub rim: Color32,
    /// 1 px inner highlight along the top edge.
    pub highlight: Color32,
    pub noise_alpha: u8,
    /// Physical pixels per grain speck.
    pub grain_px: f32,
    pub tile_top: Color32,
    pub tile_bottom: Color32,
    pub tile_rim: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    /// The collapsed pill is always "ink" (a hardware-notch black), whatever
    /// the theme; it blends into the theme's glass as the shell opens.
    pub pill_top: Color32,
    pub pill_bottom: Color32,
    pub pill_rim: Color32,
    pub pill_highlight: Color32,
    pub pill_text: Color32,
    pub pill_text_dim: Color32,
}

impl Theme {
    /// `translucent` is true when a blurred backdrop (acrylic) sits behind
    /// the window; otherwise the glass is drawn nearly opaque.
    pub fn new(dark: bool, accent: Color32, translucent: bool) -> Self {
        Self::styled(dark, accent, translucent, &Look::default())
    }

    /// [`Theme::new`] with the user's [`Look`] applied.
    pub fn styled(dark: bool, accent: Color32, translucent: bool, look: &Look) -> Self {
        let glass_alpha = if translucent {
            look.glass_opacity.clamp(0.2, 1.0)
        } else {
            0.96
        };
        let grain = look.grain.clamp(0.0, 1.0);
        let grain_px = look.grain_px.clamp(1.0, 4.0);
        // The tint keeps the theme's lightness: a dark theme gets a deep
        // shade of the color, a light theme a pale one, so text stays legible.
        let tinted = |base: Color32, amount: f32| match look.tint {
            Some(t) => mix(base, t, amount),
            None => base,
        };
        let ink_alpha = if translucent { 0.90 } else { 1.0 };
        let ink = Ink {
            pill_top: with_alpha(Color32::from_rgb(24, 24, 28), ink_alpha),
            pill_bottom: with_alpha(Color32::from_rgb(6, 6, 8), ink_alpha),
            pill_rim: Color32::from_rgba_unmultiplied(255, 255, 255, 22),
            pill_highlight: Color32::from_rgba_unmultiplied(255, 255, 255, 34),
            pill_text: Color32::from_rgb(244, 244, 246),
            pill_text_dim: Color32::from_rgb(150, 150, 160),
        };
        if dark {
            Self {
                dark,
                accent,
                glass_top: with_alpha(tinted(Color32::from_rgb(34, 34, 40), 0.30), glass_alpha),
                glass_bottom: with_alpha(tinted(Color32::from_rgb(18, 18, 22), 0.20), glass_alpha),
                rim: Color32::from_rgba_unmultiplied(0, 0, 0, 110),
                highlight: Color32::from_rgba_unmultiplied(255, 255, 255, 46),
                noise_alpha: (grain * GRAIN_MAX_DARK).round() as u8,
                grain_px,
                tile_top: Color32::from_rgba_unmultiplied(255, 255, 255, 18),
                tile_bottom: Color32::from_rgba_unmultiplied(255, 255, 255, 8),
                tile_rim: Color32::from_rgba_unmultiplied(255, 255, 255, 14),
                text: Color32::from_rgb(242, 242, 245),
                text_dim: Color32::from_rgb(168, 168, 178),
                text_faint: Color32::from_rgb(110, 110, 120),
                ..ink.into_theme()
            }
        } else {
            Self {
                dark,
                accent,
                glass_top: with_alpha(tinted(Color32::from_rgb(252, 252, 253), 0.14), glass_alpha),
                glass_bottom: with_alpha(
                    tinted(Color32::from_rgb(236, 236, 240), 0.20),
                    glass_alpha,
                ),
                rim: Color32::from_rgba_unmultiplied(0, 0, 0, 40),
                highlight: Color32::from_rgba_unmultiplied(255, 255, 255, 200),
                noise_alpha: (grain * GRAIN_MAX_LIGHT).round() as u8,
                grain_px,
                tile_top: Color32::from_rgba_unmultiplied(255, 255, 255, 150),
                tile_bottom: Color32::from_rgba_unmultiplied(255, 255, 255, 90),
                tile_rim: Color32::from_rgba_unmultiplied(0, 0, 0, 14),
                text: Color32::from_rgb(24, 24, 28),
                text_dim: Color32::from_rgb(92, 92, 104),
                text_faint: Color32::from_rgb(150, 150, 160),
                ..ink.into_theme()
            }
        }
    }

    /// Glass fill (top, bottom) at shell `progress`: ink pill at 0, theme
    /// glass from ~70% open.
    pub fn glass_at(&self, progress: f32) -> (Color32, Color32) {
        let t = blend(progress);
        (
            mix(self.pill_top, self.glass_top, t),
            mix(self.pill_bottom, self.glass_bottom, t),
        )
    }

    pub fn rim_at(&self, progress: f32) -> Color32 {
        mix(self.pill_rim, self.rim, blend(progress))
    }

    pub fn highlight_at(&self, progress: f32) -> Color32 {
        mix(self.pill_highlight, self.highlight, blend(progress))
    }

    /// Accent-tinted gradient for active tiles.
    pub fn active_tile(&self) -> (Color32, Color32) {
        let a = if self.dark { 0.42 } else { 0.30 };
        let b = if self.dark { 0.20 } else { 0.14 };
        (with_alpha(self.accent, a), with_alpha(self.accent, b))
    }

    /// Accent readable as text/icon color on the current glass.
    pub fn accent_fg(&self) -> Color32 {
        if self.dark {
            mix(self.accent, Color32::WHITE, 0.25)
        } else {
            mix(self.accent, Color32::BLACK, 0.15)
        }
    }

    pub fn font(size: f32) -> FontId {
        FontId::proportional(size)
    }

    pub fn font_semibold(size: f32) -> FontId {
        FontId::new(size, FontFamily::Name(FAMILY_SEMIBOLD.into()))
    }
}

/// Smoothstep of `progress / 0.7`: how far the pill look has turned into
/// the panel look.
fn blend(progress: f32) -> f32 {
    let t = (progress / 0.7).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The theme-independent pill colors.
struct Ink {
    pill_top: Color32,
    pill_bottom: Color32,
    pill_rim: Color32,
    pill_highlight: Color32,
    pill_text: Color32,
    pill_text_dim: Color32,
}

impl Ink {
    /// A `Theme` carrying only the ink fields, for struct-update syntax.
    fn into_theme(self) -> Theme {
        let z = Color32::TRANSPARENT;
        Theme {
            dark: true,
            accent: z,
            glass_top: z,
            glass_bottom: z,
            rim: z,
            highlight: z,
            noise_alpha: 0,
            grain_px: 1.0,
            tile_top: z,
            tile_bottom: z,
            tile_rim: z,
            text: z,
            text_dim: z,
            text_faint: z,
            pill_top: self.pill_top,
            pill_bottom: self.pill_bottom,
            pill_rim: self.pill_rim,
            pill_highlight: self.pill_highlight,
            pill_text: self.pill_text,
            pill_text_dim: self.pill_text_dim,
        }
    }
}

/// Multiplies a color's alpha (unmultiplied semantics).
pub fn with_alpha(c: Color32, alpha: f32) -> Color32 {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    let a = (a as f32 * alpha.clamp(0.0, 1.0)).round() as u8;
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// Mixes two colors in gamma space; `t = 0` is `a`.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let [ar, ag, ab, aa] = a.to_srgba_unmultiplied();
    let [br, bg, bb, ba] = b.to_srgba_unmultiplied();
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(l(ar, br), l(ag, bg), l(ab, bb), l(aa, ba))
}

/// Parses `#RRGGBB`.
pub fn parse_hex(s: &str) -> Option<Color32> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

pub fn to_hex(c: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}

/// Installs the text and icon fonts.
///
/// Text uses the system's Segoe UI (regular + semibold) on Windows, whose
/// digits are tabular by default; egui cannot switch OpenType features such
/// as `tnum`, which rules out Inter's proportional default figures. On Linux
/// the same rule picks Noto Sans (tabular by default, installed on Fedora),
/// then DejaVu Sans, then whatever fontconfig calls `sans-serif`; GNOME's
/// own Adwaita Sans is Inter-based and would make timer digits jitter. If
/// nothing is found, egui's bundled font is used. See docs/NOTES.md.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let (regular, semibold) = system_text_fonts();
    if let Some(data) = regular {
        fonts.font_data.insert("segoe".into(), data.into());
        if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
            list.insert(0, "segoe".into());
        }
    }
    let mut semibold_family = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    if let Some(data) = semibold {
        fonts.font_data.insert("segoe-semibold".into(), data.into());
        semibold_family.insert(0, "segoe-semibold".into());
    }
    #[cfg(not(windows))]
    for (i, (regular, semibold)) in script_fallbacks().into_iter().enumerate() {
        let name = format!("script-{i}");
        fonts.font_data.insert(name.clone(), regular.into());
        if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
            list.push(name.clone());
        }
        let bold = match semibold {
            Some(data) => {
                let bold = format!("{name}-semibold");
                fonts.font_data.insert(bold.clone(), data.into());
                bold
            }
            None => name,
        };
        semibold_family.push(bold);
    }
    fonts
        .families
        .insert(FontFamily::Name(FAMILY_SEMIBOLD.into()), semibold_family);

    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    egui_phosphor::add_font_bytes_as_family(
        &mut fonts,
        FAMILY_ICON_FILL,
        egui_phosphor::Variant::Fill.font_bytes(),
    );
    ctx.set_fonts(fonts);
}

/// Regular and semibold text faces from the system, if found.
#[cfg(windows)]
fn system_text_fonts() -> (Option<FontData>, Option<FontData>) {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let load = |file: &str| {
        std::fs::read(format!("{windir}\\Fonts\\{file}"))
            .ok()
            .map(FontData::from_owned)
    };
    (load("segoeui.ttf"), load("seguisb.ttf"))
}

/// Regular and semibold text faces from fontconfig, if found. A variable
/// font (Fedora ships Noto Sans as one) is used for both, at the `wght`
/// axis values 400 and 600. fontconfig reports a variable font's named
/// instance as `instance << 16 | face`, and does not flag it as variable,
/// so the face index is the low 16 bits and the axis is set whenever the
/// font has one.
#[cfg(not(windows))]
fn system_text_fonts() -> (Option<FontData>, Option<FontData>) {
    for family in ["Noto Sans", "DejaVu Sans", "sans-serif"] {
        let regular = fc_match(family, "regular", 400.0);
        if regular.is_some() {
            return (regular, fc_match(family, "semibold", 600.0));
        }
    }
    (None, None)
}

#[cfg(not(windows))]
fn fc_match(family: &str, weight: &str, wght: f32) -> Option<FontData> {
    let (found, file, index, _) = fc_query(&format!("{family}:weight={weight}"))?;
    // fontconfig always answers; only take a substitute for the generic name.
    if family != "sans-serif" && !found.eq_ignore_ascii_case(family) {
        return None;
    }
    fc_load(&file, index, wght)
}

/// Family, file, face index and supported languages (`he|hu|...`) of the
/// font fontconfig picks for `pattern`.
#[cfg(not(windows))]
fn fc_query(pattern: &str) -> Option<(String, String, u32, String)> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{family[0]}\n%{file}\n%{index}\n%{lang}", pattern])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let found = lines.next()?.trim().to_owned();
    let file = lines.next()?.trim().to_owned();
    let index: u32 = lines
        .next()
        .and_then(|i| i.trim().parse().ok())
        .unwrap_or(0)
        & 0xFFFF;
    let langs = lines.next().unwrap_or("").trim().to_owned();
    Some((found, file, index, langs))
}

#[cfg(not(windows))]
fn fc_load(file: &str, index: u32, wght: f32) -> Option<FontData> {
    let bytes = std::fs::read(file).ok()?;
    let mut data = FontData::from_owned(bytes);
    data.index = index;
    if data.variation_axes().iter().any(|a| a.tag == "wght") {
        data.tweak.coords = egui::epaint::text::VariationCoords::new([(b"wght", wght)]);
    }
    Some(data)
}

/// Regular and semibold faces for scripts the text face may lack, to fall
/// back on glyph by glyph: Fedora's Noto Sans has no Hebrew or Arabic
/// (Segoe UI has both), so typed Hebrew showed as boxes. Each face once,
/// and only one that covers the language.
#[cfg(not(windows))]
fn script_fallbacks() -> Vec<(FontData, Option<FontData>)> {
    let mut seen = Vec::new();
    let mut faces = Vec::new();
    for lang in ["he", "ar"] {
        let Some((_, file, index, langs)) =
            fc_query(&format!("sans-serif:lang={lang}:weight=regular"))
        else {
            continue;
        };
        if seen.contains(&file) || !langs.split('|').any(|l| l == lang) {
            continue;
        }
        let Some(regular) = fc_load(&file, index, 400.0) else {
            continue;
        };
        seen.push(file);
        let semibold = fc_query(&format!("sans-serif:lang={lang}:weight=semibold"))
            .and_then(|(_, file, index, _)| fc_load(&file, index, 600.0));
        faces.push((regular, semibold));
    }
    faces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        assert_eq!(parse_hex("#7C5CFF"), Some(FALLBACK_ACCENT));
        assert_eq!(to_hex(FALLBACK_ACCENT), "#7C5CFF");
        assert_eq!(parse_hex("nope"), None);
    }

    #[test]
    fn alpha_and_mix() {
        assert_eq!(with_alpha(Color32::WHITE, 0.5).a(), 128);
        assert_eq!(mix(Color32::BLACK, Color32::WHITE, 1.0), Color32::WHITE);
    }

    #[test]
    fn look_controls_grain_and_glass() {
        let off = Look {
            grain: 0.0,
            ..Look::default()
        };
        assert_eq!(
            Theme::styled(true, FALLBACK_ACCENT, true, &off).noise_alpha,
            0
        );
        let clear = Look {
            glass_opacity: 0.5,
            ..Look::default()
        };
        let t = Theme::styled(true, FALLBACK_ACCENT, true, &clear);
        assert_eq!(t.glass_top.to_srgba_unmultiplied()[3], 128);
        // The solid material ignores the opacity setting.
        let t = Theme::styled(true, FALLBACK_ACCENT, false, &clear);
        assert_eq!(t.glass_top.to_srgba_unmultiplied()[3], 245);
    }
}

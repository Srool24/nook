//! Fonts on Linux: finding them with fontconfig and drawing text with
//! `ab_glyph` (GDI does both on Windows).
//!
//! Used for text burned into screenshots, the labels of the snip overlay and
//! the candidate renderings of the font detector. Characters the main face
//! lacks (Hebrew in Noto Sans, for one) come from the face fontconfig names
//! for them, and right-to-left runs are put in visual order, so mixed text
//! reads the way GDI's `DrawText` would show it.

use ab_glyph::{Font, FontVec, Glyph, PxScale, ScaleFont, VariableFont, point};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

/// One installed face, as fontconfig lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Installed {
    pub family: String,
    pub file: PathBuf,
    pub index: u32,
    /// fontconfig weight (80 regular, 180 semibold, 200 bold); the default
    /// weight of a variable font.
    pub weight: i32,
    /// fontconfig slant (0 roman, 100 italic, 110 oblique).
    pub slant: i32,
    pub variable: bool,
}

const FC_FIELDS: &str = "%{family[0]}\t%{weight}\t%{slant}\t%{file}\t%{index}\t%{variable}\n";

fn parse_fc_line(line: &str) -> Option<Installed> {
    let mut it = line.split('\t');
    let family = it.next()?.trim().to_owned();
    let number = |s: &str| -> i32 {
        // Variable fonts print a range: "[100 900]".
        let s = s.trim().trim_start_matches('[');
        s.split_whitespace()
            .next()
            .and_then(|n| n.parse::<f32>().ok())
            .map_or(80, |n| n as i32)
    };
    let weight_field = it.next()?;
    let slant = number(it.next()?);
    let file = PathBuf::from(it.next()?.trim());
    let index = it.next()?.trim().parse().unwrap_or(0);
    let variable = it.next().is_some_and(|v| v.trim() == "True");
    let weight = if weight_field.trim().starts_with('[') {
        80
    } else {
        number(weight_field)
    };
    (!family.is_empty() && file.is_absolute()).then_some(Installed {
        family,
        file,
        index,
        weight,
        slant,
        variable,
    })
}

/// Every scalable face fontconfig knows (listed once per run).
pub fn installed() -> &'static [Installed] {
    static LIST: OnceLock<Vec<Installed>> = OnceLock::new();
    LIST.get_or_init(|| {
        let text = super::cmd::output("fc-list", &["--format", FC_FIELDS, ":scalable=true"])
            .unwrap_or_default();
        let mut faces: Vec<Installed> = text.lines().filter_map(parse_fc_line).collect();
        faces.sort_by_key(|f| f.family.to_lowercase());
        faces
    })
}

/// What fontconfig picks for a pattern such as `"Noto Sans:weight=semibold"`.
pub fn fc_match(pattern: &str) -> Option<Installed> {
    let text = super::cmd::output("fc-match", &["--format", FC_FIELDS, pattern]).ok()?;
    text.lines().next().and_then(parse_fc_line)
}

/// Loads a face, at the `wght` given for a variable font.
pub fn load(face: &Installed, wght: Option<f32>) -> Option<FontVec> {
    let bytes = std::fs::read(&face.file).ok()?;
    let mut font = FontVec::try_from_vec_and_index(bytes, face.index).ok()?;
    if face.variable
        && let Some(w) = wght
    {
        font.set_variation(b"wght", w);
    }
    Some(font)
}

/// A face plus the styling that has to be faked because the family has no
/// such face (as GDI does for missing bold and italic).
pub struct Styled {
    pub font: Arc<FontVec>,
    pub fake_bold: bool,
    pub fake_italic: bool,
}

/// The installed face of `family` closest to the asked weight and slant.
pub fn styled(family: &str, bold: bool, italic: bool) -> Option<Styled> {
    let faces: Vec<&Installed> = installed()
        .iter()
        .filter(|f| f.family.eq_ignore_ascii_case(family))
        .collect();
    let want_weight = if bold { 200 } else { 80 };
    let best = faces.iter().min_by_key(|f| {
        let slanted = f.slant > 0;
        let slant_miss = i32::from(slanted != italic) * 1000;
        let weight_miss = if f.variable {
            0
        } else {
            (f.weight - want_weight).abs()
        };
        slant_miss + weight_miss
    })?;
    let font = load(best, Some(if bold { 700.0 } else { 400.0 }))?;
    Some(Styled {
        font: Arc::new(font),
        fake_bold: bold && !best.variable && best.weight < 150,
        fake_italic: italic && best.slant == 0,
    })
}

/// The face for overlay labels and burned-in text: a semibold sans, the
/// counterpart of Segoe UI Semibold.
pub fn ui_semibold() -> Option<Arc<FontVec>> {
    static FACE: OnceLock<Option<Arc<FontVec>>> = OnceLock::new();
    FACE.get_or_init(|| {
        ["Noto Sans", "DejaVu Sans", "sans-serif"]
            .into_iter()
            .find_map(|family| {
                let face = fc_match(&format!("{family}:weight=semibold"))?;
                if family != "sans-serif" && !face.family.eq_ignore_ascii_case(family) {
                    return None;
                }
                load(&face, Some(600.0)).map(Arc::new)
            })
    })
    .clone()
}

/// A face that has `c`, for characters the main face lacks. Looked up once
/// per 128-character block.
fn fallback_for(c: char) -> Option<Arc<FontVec>> {
    static CACHE: OnceLock<Mutex<HashMap<u32, Option<Arc<FontVec>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let block = u32::from(c) >> 7;
    if let Some(hit) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&block) {
        if hit.as_ref().is_some_and(|f| f.glyph_id(c).0 != 0) {
            return hit.clone();
        }
        if hit.is_none() {
            return None;
        }
    }
    let found = fc_match(&format!(
        "sans-serif:weight=semibold:charset={:x}",
        u32::from(c)
    ))
    .and_then(|face| load(&face, Some(600.0)))
    .filter(|f| f.glyph_id(c).0 != 0)
    .map(Arc::new);
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(block, found.clone());
    found
}

/// Glyphs laid out on one line, top-left at (0, 0).
pub struct Line {
    glyphs: Vec<(Arc<FontVec>, Glyph)>,
    pub width: f32,
    pub height: f32,
}

/// Pixel scale that makes one em `em` pixels tall (GDI's negative
/// `lfHeight`).
fn em_scale(font: &FontVec, em: f32) -> PxScale {
    let upem = font.units_per_em().unwrap_or(1000.0);
    PxScale::from(em * font.height_unscaled() / upem)
}

fn is_rtl(c: char) -> bool {
    matches!(u32::from(c), 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF)
}

fn is_strong_ltr(c: char) -> bool {
    c.is_alphabetic() && !is_rtl(c)
}

/// Characters in display order: a paragraph whose first strong character is
/// right-to-left runs right to left, and right-to-left runs inside
/// left-to-right text are reversed in place. Digits and Latin words inside
/// right-to-left text keep their own order.
pub fn visual_order(text: &str) -> Vec<char> {
    let chars: Vec<char> = text.chars().collect();
    let rtl_paragraph = chars
        .iter()
        .find(|c| is_rtl(**c) || is_strong_ltr(**c))
        .is_some_and(|c| is_rtl(*c));
    // Runs: true for right-to-left. Neutrals (spaces, punctuation) between
    // two runs of one direction take it; otherwise the paragraph's.
    let mut runs: Vec<(bool, Vec<char>)> = Vec::new();
    let mut neutrals: Vec<char> = Vec::new();
    for &c in &chars {
        let dir = if is_rtl(c) {
            true
        } else if is_strong_ltr(c) || c.is_ascii_digit() {
            false
        } else {
            neutrals.push(c);
            continue;
        };
        match runs.last_mut() {
            Some((rd, run)) if *rd == dir => run.append(&mut neutrals),
            _ if !neutrals.is_empty() => runs.push((rtl_paragraph, std::mem::take(&mut neutrals))),
            _ => {}
        }
        match runs.last_mut() {
            Some((rd, run)) if *rd == dir => run.push(c),
            _ => runs.push((dir, vec![c])),
        }
    }
    if !neutrals.is_empty() {
        runs.push((rtl_paragraph, neutrals));
    }
    if rtl_paragraph {
        runs.reverse();
    }
    runs.into_iter()
        .flat_map(|(rtl, mut run)| {
            if rtl {
                run.reverse();
            }
            run
        })
        .collect()
}

/// Lays out one line of `text` in `font` at `em` pixels per em.
pub fn layout(font: &Arc<FontVec>, text: &str, em: f32) -> Line {
    let scale = em_scale(font, em);
    let main = font.as_scaled(scale);
    let ascent = main.ascent();
    let height = main.ascent() - main.descent();
    let mut x = 0.0f32;
    let mut prev: Option<(Arc<FontVec>, ab_glyph::GlyphId)> = None;
    let mut glyphs = Vec::new();
    for c in visual_order(text) {
        if c.is_control() {
            continue;
        }
        let face = if font.glyph_id(c).0 != 0 || c.is_whitespace() {
            font.clone()
        } else {
            fallback_for(c).unwrap_or_else(|| font.clone())
        };
        let sc = em_scale(&face, em);
        let scaled = face.as_scaled(sc);
        let id = face.glyph_id(c);
        if let Some((pf, pid)) = &prev
            && Arc::ptr_eq(pf, &face)
        {
            x += scaled.kern(*pid, id);
        }
        glyphs.push((
            face.clone(),
            id.with_scale_and_position(sc, point(x, ascent)),
        ));
        x += scaled.h_advance(id);
        prev = Some((face, id));
    }
    Line {
        glyphs,
        width: x,
        height,
    }
}

impl Line {
    /// Calls `put(x, y, coverage)` for every covered pixel, relative to
    /// the line's top-left corner.
    pub fn draw(&self, mut put: impl FnMut(i32, i32, f32)) {
        for (font, glyph) in &self.glyphs {
            if let Some(outline) = font.outline_glyph(glyph.clone()) {
                let b = outline.px_bounds();
                let (ox, oy) = (b.min.x as i32, b.min.y as i32);
                outline.draw(|x, y, c| put(ox + x as i32, oy + y as i32, c));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fc_list_lines() {
        let f = parse_fc_line(
            "Noto Sans\t[100 900]\t0\t/usr/share/fonts/google-noto-vf/NotoSans[wght].ttf\t0\tTrue",
        )
        .expect("line");
        assert_eq!(f.family, "Noto Sans");
        assert!(f.variable);
        assert_eq!((f.weight, f.slant, f.index), (80, 0, 0));
        let g = parse_fc_line(
            "DejaVu Serif\t200\t100\t/usr/share/fonts/dejavu/DejaVuSerif-BoldItalic.ttf\t0\tFalse",
        )
        .expect("line");
        assert_eq!((g.weight, g.slant, g.variable), (200, 100, false));
        assert!(parse_fc_line("broken").is_none());
    }

    #[test]
    fn orders_right_to_left_text() {
        let s: String = visual_order("abc").into_iter().collect();
        assert_eq!(s, "abc");
        let s: String = visual_order("שלום").into_iter().collect();
        assert_eq!(s, "םולש");
        let s: String = visual_order("hi שלום!").into_iter().collect();
        assert_eq!(s, "hi םולש!");
        let s: String = visual_order("שלום 2024").into_iter().collect();
        assert_eq!(s, "2024 םולש");
    }
}

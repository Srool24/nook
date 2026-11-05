//! Text recognition with the OCR engine built into Windows
//! (`Windows.Media.Ocr`), the one PowerToys Text Extractor and Text Grab
//! use. It runs on the device, needs no package identity (tested from the
//! plain exe), and reads a typical selection in well under 100 ms.
//!
//! It only knows the languages whose OCR packs are installed (Settings,
//! Time & language, Language & region). Two tweaks from those projects make
//! it noticeably better on screen text:
//!
//! - **Upscaling.** The engine is tuned for text larger than most UI fonts,
//!   so small selections are scaled up (bilinear, up to 3x) first.
//! - **Padding.** Glyphs touching the edge of the image are often dropped;
//!   the image gets a margin filled with its own border color.
//!
//! Lines are joined with line breaks (or spaces in single-line mode).
//! Chinese and Japanese results come back with a space between every
//! character, so those spaces are removed.
//!
//! On Linux the engine is Tesseract (`dnf install tesseract` plus a
//! `tesseract-langpack-*` per language), run on the same prepared image;
//! its word boxes come from the TSV output.

#[cfg(target_os = "linux")]
pub use linux::{languages, recognize};

use crate::capture::Frame;
#[cfg(windows)]
use crate::{Result, Win32Context};
#[cfg(windows)]
use windows::Globalization::Language;
#[cfg(windows)]
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
#[cfg(windows)]
use windows::Media::Ocr::OcrEngine;
#[cfg(windows)]
use windows::Storage::Streams::DataWriter;
#[cfg(windows)]
use windows::core::HSTRING;

/// Selections whose longer edge is under this get scaled up.
const UPSCALE_BELOW: u32 = 1200;
const MAX_UPSCALE: u32 = 3;
/// Margin added around the image, in pixels (after scaling).
const PADDING: u32 = 24;

/// An installed OCR language: BCP-47 tag and display name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OcrLanguage {
    pub tag: String,
    pub name: String,
}

/// Languages the engine can read on this machine.
#[cfg(windows)]
pub fn languages() -> Vec<OcrLanguage> {
    let Ok(list) = OcrEngine::AvailableRecognizerLanguages() else {
        return Vec::new();
    };
    list.into_iter()
        .filter_map(|l| {
            Some(OcrLanguage {
                tag: l.LanguageTag().ok()?.to_string(),
                name: l.DisplayName().ok()?.to_string(),
            })
        })
        .collect()
}

/// A recognized word and its box in the input frame's pixels.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OcrWord {
    pub text: String,
    /// Index into [`OcrText::lines`].
    pub line: usize,
    /// `(x, y, width, height)`.
    pub rect: (f32, f32, f32, f32),
}

/// Recognized text, one entry per line, top to bottom.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OcrText {
    pub lines: Vec<String>,
    /// Every word, in reading order.
    pub words: Vec<OcrWord>,
    /// The language the engine used.
    pub language: String,
}

impl OcrText {
    /// The text for the clipboard: lines on their own rows (CRLF), or all
    /// on one line.
    pub fn joined(&self, single_line: bool) -> String {
        let sep = if single_line { " " } else { "\r\n" };
        self.lines.join(sep)
    }
}

/// Reads the text in `frame`. `language` is a BCP-47 tag; `None` uses the
/// first of the user's display languages the engine supports.
#[cfg(windows)]
pub fn recognize(frame: &Frame, language: Option<&str>) -> Result<OcrText> {
    let engine = match language {
        Some(tag) => {
            let lang = Language::CreateLanguage(&HSTRING::from(tag)).api("Language")?;
            OcrEngine::TryCreateFromLanguage(&lang).api("OcrEngine::TryCreateFromLanguage")?
        }
        None => OcrEngine::TryCreateFromUserProfileLanguages()
            .api("OcrEngine::TryCreateFromUserProfileLanguages")?,
    };
    let tag = engine
        .RecognizerLanguage()
        .and_then(|l| l.LanguageTag())
        .map(|t| t.to_string())
        .unwrap_or_default();

    let max = OcrEngine::MaxImageDimension().unwrap_or(10_000);
    let prepared = prepare(frame, max);
    let writer = DataWriter::new().api("DataWriter")?;
    writer
        .WriteBytes(&prepared.bgra_bytes())
        .api("DataWriter::WriteBytes")?;
    let buffer = writer.DetachBuffer().api("DataWriter::DetachBuffer")?;
    let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
        &buffer,
        BitmapPixelFormat::Bgra8,
        prepared.width as i32,
        prepared.height as i32,
        BitmapAlphaMode::Premultiplied,
    )
    .api("SoftwareBitmap::CreateCopyWithAlphaFromBuffer")?;
    let result = engine
        .RecognizeAsync(&bitmap)
        .api("OcrEngine::RecognizeAsync")?
        .join()
        .api("OcrEngine::RecognizeAsync(join)")?;

    // Word boxes come back in the prepared image: undo padding and scale.
    let sx = (prepared.width - 2 * PADDING) as f32 / frame.width.max(1) as f32;
    let sy = (prepared.height - 2 * PADDING) as f32 / frame.height.max(1) as f32;
    let pad = PADDING as f32;
    let squeeze = is_cjk(&tag);
    let mut lines = Vec::new();
    let mut words = Vec::new();
    for line in result.Lines().api("OcrResult::Lines")? {
        let text = line.Text().api("OcrLine::Text")?.to_string();
        let text = if squeeze { squeeze_cjk(&text) } else { text };
        if text.trim().is_empty() {
            continue;
        }
        for word in line.Words().api("OcrLine::Words")? {
            let r = word.BoundingRect().api("OcrWord::BoundingRect")?;
            words.push(OcrWord {
                text: word.Text().api("OcrWord::Text")?.to_string(),
                line: lines.len(),
                rect: (
                    (r.X - pad) / sx,
                    (r.Y - pad) / sy,
                    r.Width / sx,
                    r.Height / sy,
                ),
            });
        }
        lines.push(text);
    }
    Ok(OcrText {
        lines,
        words,
        language: tag,
    })
}

/// Scale factor for a selection of `w` x `h`, keeping under `max`.
fn upscale_factor(w: u32, h: u32, max: u32) -> u32 {
    let long = w.max(h).max(1);
    if long >= UPSCALE_BELOW {
        return 1;
    }
    let want = UPSCALE_BELOW.div_ceil(long).clamp(1, MAX_UPSCALE);
    let fits = (max.saturating_sub(2 * PADDING) / long).max(1);
    want.min(fits)
}

/// Upscales and pads `frame` for the engine, and keeps it under `max`.
fn prepare(frame: &Frame, max: u32) -> Frame {
    let factor = upscale_factor(frame.width, frame.height, max);
    let mut img = image::DynamicImage::ImageRgb8(frame.to_rgb_image());
    let room = max.saturating_sub(2 * PADDING).max(1);
    if factor > 1 {
        img = img.resize_exact(
            frame.width * factor,
            frame.height * factor,
            image::imageops::FilterType::Triangle,
        );
    } else if img.width().max(img.height()) > room {
        img = img.resize(room, room, image::imageops::FilterType::Triangle);
    }
    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    let fill = border_color(&rgb);
    let (pw, ph) = (w + 2 * PADDING, h + 2 * PADDING);
    let mut pixels = vec![fill; (pw * ph) as usize];
    for (x, y, p) in rgb.enumerate_pixels() {
        let [r, g, b] = p.0;
        pixels[((y + PADDING) * pw + x + PADDING) as usize] =
            0xFF00_0000 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
    }
    Frame {
        origin: frame.origin,
        width: pw,
        height: ph,
        pixels,
    }
}

/// Average color of the outermost pixels, as opaque BGRA.
fn border_color(img: &image::RgbImage) -> u32 {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return 0xFFFF_FFFF;
    }
    let mut sum = [0u64; 3];
    let mut n = 0u64;
    let mut add = |x: u32, y: u32| {
        let p = img.get_pixel(x, y).0;
        for (s, c) in sum.iter_mut().zip(p) {
            *s += u64::from(c);
        }
        n += 1;
    };
    for x in 0..w {
        add(x, 0);
        add(x, h - 1);
    }
    for y in 0..h {
        add(0, y);
        add(w - 1, y);
    }
    let avg = |i: usize| (sum[i] / n.max(1)) as u32;
    0xFF00_0000 | (avg(0) << 16) | (avg(1) << 8) | avg(2)
}

fn is_cjk(tag: &str) -> bool {
    let lang = tag.split('-').next().unwrap_or_default();
    matches!(lang, "zh" | "ja")
}

fn is_cjk_char(c: char) -> bool {
    matches!(c,
        '\u{3000}'..='\u{30FF}' // punctuation, hiragana, katakana
        | '\u{3400}'..='\u{4DBF}'
        | '\u{4E00}'..='\u{9FFF}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{FF00}'..='\u{FFEF}') // full-width forms
}

/// Drops spaces that sit between two CJK characters.
fn squeeze_cjk(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == ' '
            && i > 0
            && i + 1 < chars.len()
            && is_cjk_char(chars[i - 1])
            && is_cjk_char(chars[i + 1])
        {
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{OcrLanguage, OcrText, OcrWord, PADDING, is_cjk, prepare, squeeze_cjk};
    use crate::capture::Frame;
    use crate::sys::cmd;
    use crate::{PlatformError, Result};

    /// Tesseract's language codes and their BCP-47 tags and names.
    const LANGS: &[(&str, &str, &str)] = &[
        ("eng", "en", "English"),
        ("heb", "he", "Hebrew"),
        ("ara", "ar", "Arabic"),
        ("deu", "de", "German"),
        ("fra", "fr", "French"),
        ("spa", "es", "Spanish"),
        ("ita", "it", "Italian"),
        ("por", "pt", "Portuguese"),
        ("nld", "nl", "Dutch"),
        ("rus", "ru", "Russian"),
        ("ukr", "uk", "Ukrainian"),
        ("pol", "pl", "Polish"),
        ("ces", "cs", "Czech"),
        ("tur", "tr", "Turkish"),
        ("ell", "el", "Greek"),
        ("swe", "sv", "Swedish"),
        ("dan", "da", "Danish"),
        ("nor", "nb", "Norwegian"),
        ("fin", "fi", "Finnish"),
        ("hun", "hu", "Hungarian"),
        ("ron", "ro", "Romanian"),
        ("bul", "bg", "Bulgarian"),
        ("hin", "hi", "Hindi"),
        ("tha", "th", "Thai"),
        ("vie", "vi", "Vietnamese"),
        ("ind", "id", "Indonesian"),
        ("kor", "ko", "Korean"),
        ("jpn", "ja", "Japanese"),
        ("chi_sim", "zh-Hans", "Chinese (Simplified)"),
        ("chi_tra", "zh-Hant", "Chinese (Traditional)"),
        ("fas", "fa", "Persian"),
    ];

    fn installed() -> Vec<String> {
        cmd::output("tesseract", &["--list-langs"])
            .map(|out| {
                out.lines()
                    .skip(1)
                    .map(|l| l.trim().to_owned())
                    .filter(|l| !l.is_empty() && l != "osd" && l != "equ")
                    .collect()
            })
            .unwrap_or_default()
    }

    fn tag_of(code: &str) -> String {
        LANGS
            .iter()
            .find(|(c, _, _)| *c == code)
            .map_or_else(|| code.to_owned(), |(_, t, _)| (*t).to_owned())
    }

    /// Languages the engine can read on this machine.
    pub fn languages() -> Vec<OcrLanguage> {
        installed()
            .into_iter()
            .map(|code| {
                let (tag, name) = LANGS.iter().find(|(c, _, _)| *c == code).map_or_else(
                    || (code.clone(), code.clone()),
                    |(_, t, n)| ((*t).into(), (*n).into()),
                );
                OcrLanguage { tag, name }
            })
            .collect()
    }

    /// The Tesseract languages to use: the asked tag's, or the user's
    /// language plus English (most screens mix the two).
    fn pick(language: Option<&str>, installed: &[String]) -> Option<String> {
        let code_for = |tag: &str| {
            let lower = tag.to_ascii_lowercase();
            LANGS
                .iter()
                .find(|(_, t, _)| t.to_ascii_lowercase() == lower)
                .or_else(|| {
                    let base = lower.split(['-', '_']).next().unwrap_or("");
                    LANGS
                        .iter()
                        .find(|(_, t, _)| t.to_ascii_lowercase() == base)
                })
                .map(|(c, _, _)| (*c).to_owned())
                .filter(|c| installed.contains(c))
        };
        if let Some(tag) = language {
            return code_for(tag);
        }
        let env = std::env::var("LANGUAGE")
            .ok()
            .and_then(|l| l.split(':').next().map(str::to_owned))
            .filter(|l| !l.is_empty())
            .or_else(|| std::env::var("LC_ALL").ok())
            .or_else(|| std::env::var("LANG").ok())
            .unwrap_or_default();
        let base = env.split(['.', '@']).next().unwrap_or("").replace('_', "-");
        let mut codes: Vec<String> = Vec::new();
        if let Some(c) = code_for(&base) {
            codes.push(c);
        }
        if installed.iter().any(|c| c == "eng") && !codes.iter().any(|c| c == "eng") {
            codes.push("eng".into());
        }
        if codes.is_empty() {
            codes.extend(installed.first().cloned());
        }
        (!codes.is_empty()).then(|| codes.join("+"))
    }

    /// Reads the text in `frame`. `language` is a BCP-47 tag; `None` uses
    /// the user's language (with English) when its pack is installed.
    pub fn recognize(frame: &Frame, language: Option<&str>) -> Result<OcrText> {
        if cmd::which("tesseract").is_none() {
            return Err(PlatformError::Other(
                "Tesseract is not installed (sudo dnf install tesseract)".into(),
            ));
        }
        let codes = pick(language, &installed()).ok_or_else(|| {
            PlatformError::Other("no OCR language installed (tesseract-langpack-eng)".into())
        })?;
        let prepared = prepare(frame, 10_000);
        let png = prepared.encode_png()?;
        let path = std::env::temp_dir().join(format!("nook-ocr-{}.png", std::process::id()));
        std::fs::write(&path, png).map_err(|e| PlatformError::other("writing the OCR image", e))?;
        let out = cmd::output(
            "tesseract",
            &[
                &path.display().to_string(),
                "stdout",
                "-l",
                &codes,
                "--psm",
                "3",
                "tsv",
            ],
        );
        let _ = std::fs::remove_file(&path);
        let tsv = out?;
        let first = codes.split('+').next().unwrap_or("eng");
        let tag = tag_of(first);
        let sx = (prepared.width - 2 * PADDING) as f32 / frame.width.max(1) as f32;
        let sy = (prepared.height - 2 * PADDING) as f32 / frame.height.max(1) as f32;
        let mut text = parse_tsv(&tsv, PADDING as f32, sx, sy);
        if is_cjk(&tag) {
            for l in &mut text.lines {
                *l = squeeze_cjk(l);
            }
        }
        text.language = tag;
        Ok(text)
    }

    /// Lines and word boxes from Tesseract's TSV, boxes mapped back from
    /// the prepared image (padding `pad`, scale `sx`, `sy`).
    pub(super) fn parse_tsv(tsv: &str, pad: f32, sx: f32, sy: f32) -> OcrText {
        let mut lines: Vec<String> = Vec::new();
        let mut words: Vec<OcrWord> = Vec::new();
        let mut current: Option<(u32, u32, u32)> = None;
        for row in tsv.lines().skip(1) {
            let f: Vec<&str> = row.split('\t').collect();
            if f.len() < 12 || f[0] != "5" {
                continue;
            }
            let text = f[11].trim();
            if text.is_empty() {
                continue;
            }
            let num = |i: usize| f[i].parse::<u32>().unwrap_or(0);
            let num_f = |i: usize| f[i].parse::<f32>().unwrap_or(0.0);
            let key = (num(2), num(3), num(4));
            if current != Some(key) {
                current = Some(key);
                lines.push(String::new());
            }
            let line = lines.len() - 1;
            let l = &mut lines[line];
            if !l.is_empty() {
                l.push(' ');
            }
            l.push_str(text);
            words.push(OcrWord {
                text: text.to_owned(),
                line,
                rect: (
                    (num_f(6) - pad) / sx,
                    (num_f(7) - pad) / sy,
                    num_f(8) / sx,
                    num_f(9) / sy,
                ),
            });
        }
        OcrText {
            lines,
            words,
            language: String::new(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_tesseract_tsv() {
            let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
1\t1\t0\t0\t0\t0\t0\t0\t500\t200\t-1\t\n\
5\t1\t1\t1\t1\t1\t24\t24\t60\t20\t96\tHello\n\
5\t1\t1\t1\t1\t2\t94\t24\t70\t20\t95\tworld\n\
5\t1\t1\t1\t2\t1\t24\t64\t40\t20\t91\tagain\n";
            let t = parse_tsv(tsv, 24.0, 2.0, 2.0);
            assert_eq!(t.lines, ["Hello world", "again"]);
            assert_eq!(t.words.len(), 3);
            assert_eq!(t.words[1].line, 0);
            assert_eq!(t.words[2].line, 1);
            assert_eq!(t.words[0].rect, (0.0, 0.0, 30.0, 10.0));
        }

        #[test]
        fn picks_languages() {
            let have: Vec<String> = ["eng", "heb"].map(String::from).to_vec();
            assert_eq!(pick(Some("he-IL"), &have).as_deref(), Some("heb"));
            assert_eq!(pick(Some("de"), &have), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upscales_small_selections_only() {
        assert_eq!(upscale_factor(200, 40, 10_000), 3);
        assert_eq!(upscale_factor(700, 100, 10_000), 2);
        assert_eq!(upscale_factor(1600, 300, 10_000), 1);
        // Never past the engine's limit.
        assert_eq!(upscale_factor(900, 10, 1000), 1);
    }

    #[test]
    fn pads_with_the_border_color() {
        let frame = Frame {
            origin: (0, 0),
            width: 2000,
            height: 2,
            pixels: vec![0xFF10_2030; 4000],
        };
        let p = prepare(&frame, 10_000);
        assert_eq!((p.width, p.height), (2000 + 2 * PADDING, 2 + 2 * PADDING));
        assert_eq!(p.pixels[0], 0xFF10_2030);
    }

    #[test]
    fn squeezes_cjk_spaces() {
        assert_eq!(squeeze_cjk("日 本 語 の text 文"), "日本語の text 文");
        assert!(is_cjk("ja-JP") && is_cjk("zh-Hans-CN") && !is_cjk("ko"));
    }

    #[test]
    fn joins_lines() {
        let t = OcrText {
            lines: vec!["a b".into(), "c".into()],
            language: "en".into(),
            ..Default::default()
        };
        assert_eq!(t.joined(false), "a b\r\nc");
        assert_eq!(t.joined(true), "a b c");
    }
}

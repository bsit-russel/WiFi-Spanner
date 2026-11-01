// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! Finding a floor plan's scale from the dimensions printed on it.
//!
//! The plan's text is read with the OCR engine (see `assets/ocr`). Numbers that sit on a
//! dimension line give a candidate scale each: the length of the line segment they label in
//! pixels, against the value they state. The candidates are compared and the scale most of
//! them agree on wins.

use ocrs::{ImageSource, OcrEngine, OcrEngineParams};
use rten::Model;

const DETECTION_MODEL: &[u8] = include_bytes!("../assets/ocr/text-detection.rten");
const RECOGNITION_MODEL: &[u8] = include_bytes!("../assets/ocr/text-recognition.rten");

fn engine() -> Result<OcrEngine, String> {
    let load = |bytes: &'static [u8]| Model::load_static_slice(bytes).map_err(|e| format!("could not load the text reader: {e}"));
    OcrEngine::new(OcrEngineParams { detection_model: Some(load(DETECTION_MODEL)?), recognition_model: Some(load(RECOGNITION_MODEL)?), ..Default::default() })
        .map_err(|e| format!("could not start the text reader: {e}"))
}

/// A stretch of dimension line between two ticks, and the number written beside it.
#[derive(Debug, Clone, PartialEq)]
pub struct Dimension {
    /// Length of the stretch in plan pixels.
    pub length_px: f32,
    /// What was read beside it, e.g. `4590` or `3.2m`.
    pub text: String,
}

/// Dark enough to be drawing rather than paper.
const INK: u8 = 200;
/// A dimension line is at least this long in plan pixels to be considered.
const MIN_LINE_PX: usize = 60;

struct Ink {
    w: usize,
    h: usize,
    dark: Vec<bool>,
}

impl Ink {
    fn new(rgb: &[u8], (w, h): (u32, u32)) -> Ink {
        let (w, h) = (w as usize, h as usize);
        let dark = (0..w * h)
            .map(|i| {
                let (r, g, b) = (rgb[i * 3] as u32, rgb[i * 3 + 1] as u32, rgb[i * 3 + 2] as u32);
                ((r * 30 + g * 59 + b * 11) / 100) < INK as u32
            })
            .collect();
        Ink { w, h, dark }
    }

    fn at(&self, x: isize, y: isize) -> bool {
        x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h && self.dark[y as usize * self.w + x as usize]
    }

    /// Thin horizontal lines as `(y, x0, x1)`: long runs of ink one or two pixels thick.
    fn horizontal_lines(&self) -> Vec<(usize, usize, usize)> {
        let mut out = Vec::new();
        for y in 2..self.h.saturating_sub(2) {
            let mut x = 0;
            while x < self.w {
                if !self.dark[y * self.w + x] {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < self.w && self.dark[y * self.w + x] {
                    x += 1;
                }
                if x - start >= MIN_LINE_PX {
                    // One pixel line: clear two rows away on both sides over most of its length.
                    let clear = (start..x).filter(|&c| !self.at(c as isize, y as isize - 3) && !self.at(c as isize, y as isize + 3)).count();
                    // Rows of the same line (anti-aliasing) are skipped: only keep the first row.
                    let above = (start..x).filter(|&c| self.at(c as isize, y as isize - 1)).count();
                    if clear * 10 >= (x - start) * 8 && above * 2 < (x - start) {
                        out.push((y, start, x));
                    }
                }
            }
        }
        out
    }

    /// Columns along the line at row `y` where something crosses it: a tick or an extension line.
    fn ticks(&self, y: usize, x0: usize, x1: usize) -> Vec<f32> {
        let mut hits: Vec<usize> = Vec::new();
        for x in x0..x1 {
            let up = (3..=8).filter(|&d| self.at(x as isize, y as isize - d)).count();
            let down = (3..=8).filter(|&d| self.at(x as isize, y as isize + d)).count();
            if up >= 2 && down >= 2 {
                hits.push(x);
            }
        }
        let mut ticks: Vec<(usize, usize)> = Vec::new();
        for x in hits {
            match ticks.last_mut() {
                Some(last) if x <= last.1 + 4 => last.1 = x,
                _ => ticks.push((x, x)),
            }
        }
        ticks.into_iter().map(|(a, b)| (a + b) as f32 / 2.0).collect()
    }
}

/// Dimension lines running along a picture's rows, each stretch with the number read above it.
fn horizontal_dimensions(ink: &Ink, engine: &OcrEngine, rgb: &[u8]) -> Vec<Dimension> {
    let mut found = Vec::new();
    for (y, x0, x1) in ink.horizontal_lines() {
        let ticks = ink.ticks(y, x0, x1);
        for pair in ticks.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if b - a < 40.0 {
                continue;
            }
            if let Some(text) = read_above(ink, engine, rgb, a as usize + 3, b as usize - 3, y) {
                found.push(Dimension { length_px: b - a, text });
            }
        }
    }
    found
}

/// The number written in the band just above a line between `x0` and `x1`, if there is one.
fn read_above(ink: &Ink, engine: &OcrEngine, rgb: &[u8], x0: usize, x1: usize, y: usize) -> Option<String> {
    // The first row of text above the line: ink rows from just above it, until a gap of blank rows.
    let row_has_ink = |yy: usize| (x0..x1.min(ink.w)).any(|xx| ink.dark[yy * ink.w + xx]);
    let mut yy = y.saturating_sub(2);
    let mut blank = 0;
    while yy > 0 && blank < 40 && !row_has_ink(yy) {
        blank += 1;
        yy -= 1;
    }
    let by = yy;
    let mut gap = 0;
    while yy > 0 && gap < 4 && by - yy < 40 {
        gap = if row_has_ink(yy) { 0 } else { gap + 1 };
        yy -= 1;
    }
    let ty = yy + gap + 1;
    if by <= ty + 6 || by - ty > 30 || y - by > 24 {
        return None;
    }
    let (mut lx, mut rx) = (usize::MAX, 0);
    for yy in ty..=by {
        for xx in x0..x1.min(ink.w) {
            if ink.dark[yy * ink.w + xx] {
                lx = lx.min(xx);
                rx = rx.max(xx);
            }
        }
    }
    if lx == usize::MAX || rx - lx < 8 {
        return None;
    }
    read_crop(engine, rgb, ink.w, (lx.saturating_sub(4), ty.saturating_sub(4), (rx + 5).min(ink.w), (by + 5).min(ink.h)))
}

/// OCR of one small region, scaled up on white paper. `None` unless it reads as a measurement.
fn read_crop(engine: &OcrEngine, rgb: &[u8], stride_px: usize, (x0, y0, x1, y1): (usize, usize, usize, usize)) -> Option<String> {
    let (w, h) = (x1 - x0, y1 - y0);
    let k = (48.0 / h as f32).clamp(1.0, 4.0);
    let (cw, ch) = (((w as f32 * k) as u32).max(8), ((h as f32 * k) as u32).max(8));
    let mut crop = image::RgbImage::new(w as u32, h as u32);
    for (x, y, px) in crop.enumerate_pixels_mut() {
        let i = ((y0 + y as usize) * stride_px + x0 + x as usize) * 3;
        *px = image::Rgb([rgb[i], rgb[i + 1], rgb[i + 2]]);
    }
    let scaled = image::imageops::resize(&crop, cw, ch, image::imageops::FilterType::CatmullRom);
    // White margin around the text helps the reader.
    let mut canvas = image::RgbImage::from_pixel(cw + 40, ch + 40, image::Rgb([255, 255, 255]));
    image::imageops::overlay(&mut canvas, &scaled, 20, 20);
    let source = ImageSource::from_bytes(canvas.as_raw(), canvas.dimensions()).ok()?;
    let input = engine.prepare_input(source).ok()?;
    let text = engine.get_text(&input).ok()?;
    // The longest word that is a measurement; stray marks from nearby lines are ignored.
    text.split_whitespace().filter(|w| parse_measure(w).is_some()).max_by_key(|w| w.chars().filter(|c| c.is_ascii_digit()).count()).map(str::to_string)
}

/// A measurement read off a plan: its length in metres, when its unit is written; bare numbers
/// are returned with the number alone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Measure {
    Metres(f32),
    Bare(f32),
}

/// `4590`, `3.2m`, `450 cm`, `12'6"`, `4,5 m`... as a measurement.
pub fn parse_measure(text: &str) -> Option<Measure> {
    let s: String = text.trim().chars().filter(|c| !c.is_whitespace()).collect::<String>().replace(',', ".").replace('O', "0").replace('o', "0").replace('l', "1");
    if s.is_empty() {
        return None;
    }
    // Feet and inches: 12'6", 12'-6", 12', 6".
    if s.contains('\'') || s.contains('"') {
        let (feet, rest) = s.split_once('\'').unwrap_or(("", &s));
        let feet: f32 = if feet.is_empty() { 0.0 } else { feet.parse().ok()? };
        let rest = rest.trim_start_matches('-').trim_end_matches('"');
        let inches: f32 = if rest.is_empty() { 0.0 } else { rest.parse().ok()? };
        return Some(Measure::Metres((feet * 12.0 + inches) * 0.0254));
    }
    let lower = s.to_lowercase();
    for (suffix, to_m) in [("mm", 0.001), ("cm", 0.01), ("ft", 0.3048), ("m", 1.0)] {
        if let Some(n) = lower.strip_suffix(suffix) {
            let v: f32 = n.parse().ok()?;
            return (v > 0.0).then_some(Measure::Metres(v * to_m));
        }
    }
    let v: f32 = lower.parse().ok()?;
    (v > 0.0).then_some(Measure::Bare(v))
}

/// What the plan says its scale is.
#[derive(Debug, Clone, PartialEq)]
pub struct Detected {
    pub metres_per_px: f32,
    /// How many dimensions agreed, and a few of them to show.
    pub agreeing: usize,
    pub examples: Vec<String>,
    /// The unit assumed for numbers that did not state one.
    pub assumed: Option<&'static str>,
    /// For a scale note (1:N) read on a page of known size: N and the page's long side in millimetres.
    pub ratio: Option<(u32, f32)>,
}

/// Find the plan's scale from the dimensions printed on it.
pub fn detect(rgb: &[u8], size: (u32, u32)) -> Result<Detected, String> {
    let engine = engine()?;
    let ink = Ink::new(rgb, size);
    let mut dims = horizontal_dimensions(&ink, &engine, rgb);
    // Vertical dimensions: turn the picture on its side, read, and the text is read upright by turning it back.
    let (w, h) = size;
    let mut turned = image::RgbImage::new(h, w);
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 3) as usize;
            turned.put_pixel(h - 1 - y, x, image::Rgb([rgb[i], rgb[i + 1], rgb[i + 2]]));
        }
    }
    let turned_ink = Ink::new(turned.as_raw(), (h, w));
    dims.extend(horizontal_dimensions(&turned_ink, &engine, turned.as_raw()));
    match from_dimensions(&dims, size) {
        Ok(found) => Ok(found),
        // No dimension lines: a plan page drawn to scale on a standard sheet states it as a ratio.
        Err(why) => match page_long_side_mm(size) {
            Some(guess) => ratio_scale(&engine, rgb, size, guess).ok_or(why),
            None => Err(why),
        },
    }
}

/// Long sides of the A-series sheets in millimetres.
const SHEETS_MM: [(&str, f32); 6] = [("A0", 1189.0), ("A1", 841.0), ("A2", 594.0), ("A3", 420.0), ("A4", 297.0), ("A5", 210.0)];

/// A4's long side, when the picture has the proportions of an A-series page: a plan on a
/// standard sheet (a PDF page) is taken to be A4 unless its scale note names another size.
fn page_long_side_mm(size: (u32, u32)) -> Option<f32> {
    let ratio = size.0.max(size.1) as f32 / size.0.min(size.1).max(1) as f32;
    ((ratio - std::f32::consts::SQRT_2).abs() < 0.04).then_some(297.0)
}

/// `1:100`, `1 : 50`, `1/200`, optionally followed by `@A3`: the ratio and the sheet's long side, if named.
pub fn parse_ratio(text: &str) -> Option<(u32, Option<f32>)> {
    let chars: Vec<char> = text.chars().collect();
    for i in 0..chars.len() {
        if chars[i] != '1' || (i > 0 && chars[i - 1].is_ascii_alphanumeric()) {
            continue;
        }
        let mut j = i + 1;
        while j < chars.len() && chars[j] == ' ' {
            j += 1;
        }
        if j >= chars.len() || !matches!(chars[j], ':' | '/') {
            continue;
        }
        j += 1;
        while j < chars.len() && chars[j] == ' ' {
            j += 1;
        }
        let digits: String = chars[j..].iter().take_while(|c| c.is_ascii_digit()).collect();
        let Ok(n) = digits.parse::<u32>() else { continue };
        if !(2..=5000).contains(&n) {
            continue;
        }
        let rest: String = chars[j + digits.len()..].iter().take(6).collect::<String>().to_uppercase();
        let sheet = rest.split_once('@').and_then(|(_, s)| SHEETS_MM.iter().find(|(name, _)| s.trim_start().starts_with(name))).map(|(_, mm)| *mm);
        return Some((n, sheet));
    }
    None
}

/// Look for a scale note anywhere on the page and turn it into a scale for the page's size.
fn ratio_scale(engine: &OcrEngine, rgb: &[u8], size: (u32, u32), assumed_long_mm: f32) -> Option<Detected> {
    let source = ImageSource::from_bytes(rgb, size).ok()?;
    let input = engine.prepare_input(source).ok()?;
    let text = engine.get_text(&input).ok()?;
    let (n, sheet) = parse_ratio(&text)?;
    let long_mm = sheet.unwrap_or(assumed_long_mm);
    let mpp = long_mm / size.0.max(size.1) as f32 * n as f32 / 1000.0;
    let (w, h) = (size.0 as f32 * mpp, size.1 as f32 * mpp);
    (1.0..=500.0).contains(&w.max(h)).then(|| Detected { metres_per_px: mpp, agreeing: 1, examples: vec![format!("1:{n}")], assumed: None, ratio: Some((n, long_mm)) })
}

/// Combine what was read into one scale.
pub fn from_dimensions(dims: &[Dimension], size: (u32, u32)) -> Result<Detected, String> {
    let parsed: Vec<(f32, Measure, &str)> = dims.iter().filter_map(|d| Some((d.length_px, parse_measure(&d.text)?, d.text.as_str()))).collect();
    if parsed.is_empty() {
        return Err("No dimensions were found on the plan.".into());
    }
    // Unwritten units: whole numbers of three digits or more are millimetres, anything smaller or with decimals is metres.
    let bare: Vec<f32> = parsed.iter().filter_map(|(_, m, _)| if let Measure::Bare(v) = m { Some(*v) } else { None }).collect();
    let (unit, to_m): (Option<&'static str>, f32) = if bare.is_empty() {
        (None, 1.0)
    } else if bare.iter().all(|v| v.fract() == 0.0 && *v >= 100.0) || bare.iter().filter(|v| **v >= 100.0).count() * 2 > bare.len() {
        (Some("millimetres"), 0.001)
    } else {
        (Some("metres"), 1.0)
    };
    let mut candidates: Vec<(f32, &str)> = parsed
        .iter()
        .map(|(px, m, text)| {
            let metres = match m {
                Measure::Metres(v) => *v,
                Measure::Bare(v) => v * to_m,
            };
            (metres / px, *text)
        })
        .filter(|(mpp, _)| mpp.is_finite() && *mpp > 0.0)
        .collect();
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
    // The biggest group of candidates within 3 % of each other.
    let (mut start, mut most) = (0, 0);
    for i in 0..candidates.len() {
        let n = candidates[i..].iter().take_while(|c| c.0 <= candidates[i].0 * 1.03).count();
        if n > most {
            (start, most) = (i, n);
        }
    }
    let best = (start, start + most - 1);
    let group = &candidates[best.0..=best.1];
    let mpp = group[group.len() / 2].0;
    let (w, h) = (size.0 as f32 * mpp, size.1 as f32 * mpp);
    if group.len() < 2 && candidates.len() > 1 {
        return Err(format!("The dimensions read from the plan do not agree ({} read).", candidates.len()));
    }
    if !(1.0..=500.0).contains(&w.max(h)) {
        return Err(format!("The scale found would make the plan {w:.0} x {h:.0} m, which looks wrong."));
    }
    Ok(Detected { metres_per_px: mpp, agreeing: group.len(), examples: group.iter().take(4).map(|c| c.1.to_string()).collect(), assumed: unit, ratio: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dim(length_px: f32, text: &str) -> Dimension {
        Dimension { length_px, text: text.into() }
    }

    #[test]
    fn measurements_in_any_unit_are_understood() {
        let m = |s: &str| parse_measure(s);
        assert_eq!(m("4590"), Some(Measure::Bare(4590.0)));
        assert_eq!(m("14970,"), Some(Measure::Bare(14970.0)));
        assert_eq!(m("3,2m"), Some(Measure::Metres(3.2)));
        assert_eq!(m("450 cm"), Some(Measure::Metres(4.5)));
        assert_eq!(m("2400mm"), Some(Measure::Metres(2.4)));
        assert_eq!(m("12ft"), Some(Measure::Metres(12.0 * 0.3048)));
        let Some(Measure::Metres(v)) = m("12'-6\"") else { panic!("feet and inches") };
        assert!((v - 3.81).abs() < 1e-4, "{v}");
        assert_eq!(m("NORTH"), None);
        assert_eq!(m(""), None);
    }

    #[test]
    fn the_scale_most_dimensions_agree_on_wins() {
        // 66.3 px per metre, with two misreads among the numbers.
        let dims = [dim(992.0, "14970"), dim(304.0, "4590"), dim(362.0, "5460"), dim(73.0, "1100"), dim(150.0, "228"), dim(80.0, "12003")];
        let found = from_dimensions(&dims, (2828, 4000)).unwrap();
        assert!((found.metres_per_px - 0.01509).abs() < 0.0002, "{}", found.metres_per_px);
        assert_eq!(found.agreeing, 4);
        assert_eq!(found.assumed, Some("millimetres"));
    }

    #[test]
    fn written_units_and_feet_need_no_assumption() {
        let dims = [dim(200.0, "3.2m"), dim(250.0, "4 m"), dim(100.0, "1.6m")];
        let found = from_dimensions(&dims, (1000, 800)).unwrap();
        assert!((found.metres_per_px - 0.016).abs() < 1e-4);
        assert_eq!(found.assumed, None);
        let feet = from_dimensions(&[dim(100.0, "10'"), dim(200.0, "20'")], (1000, 800)).unwrap();
        assert!((feet.metres_per_px - 0.03048).abs() < 1e-4);
        let metres = from_dimensions(&[dim(100.0, "4.5"), dim(200.0, "9.0")], (1000, 800)).unwrap();
        assert_eq!(metres.assumed, Some("metres"));
    }

    #[test]
    fn scale_notes_are_read_as_ratios() {
        assert_eq!(parse_ratio("SCALE 1:100"), Some((100, None)));
        assert_eq!(parse_ratio("Scale: 1 : 50 @ A3"), Some((50, Some(420.0))));
        assert_eq!(parse_ratio("1/200@a4"), Some((200, Some(297.0))));
        assert_eq!(parse_ratio("Page 11:30 and 21:5"), None);
        assert_eq!(parse_ratio("1:1"), None);
        assert_eq!(page_long_side_mm((2000, 1414)), Some(297.0));
        assert_eq!(page_long_side_mm((2828, 4000)), Some(297.0));
        assert_eq!(page_long_side_mm((1000, 1000)), None);
    }

    #[test]
    fn nothing_or_nonsense_is_refused() {
        assert!(from_dimensions(&[], (100, 100)).is_err());
        assert!(from_dimensions(&[dim(10.0, "4590"), dim(300.0, "4590"), dim(20.0, "800")], (2800, 4000)).is_err());
    }

    /// Try the detector on a plan picture: `WS_PLAN=/path/plan.png cargo test --release detect_on_plan -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn detect_on_plan() {
        let path = std::env::var("WS_PLAN").expect("WS_PLAN");
        let img = image::open(path).unwrap().into_rgb8();
                let t = std::time::Instant::now();
        println!("{:?} in {:?}", detect(img.as_raw(), img.dimensions()), t.elapsed());
    }
}

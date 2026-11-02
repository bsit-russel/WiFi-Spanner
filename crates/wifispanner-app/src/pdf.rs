// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! Rasterising PDF floor plans (pure Rust, no system libraries).

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::hayro_syntax::page::Page;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings, render};

/// Longest side of an imported page in pixels. Large enough to read small
/// text on a plan; the texture is downscaled further for display.
const PAGE_SIDE_PX: f32 = 4000.0;
const MAX_SCALE: f32 = 8.0;
/// The longest side a rendered page may have, whatever the page claims to measure: a PDF can
/// declare a page of any size, and the picture is one pixel buffer.
const MAX_OUTPUT_PX: f32 = 8000.0;

pub fn is_pdf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"%PDF")
}

fn open(bytes: Vec<u8>) -> Result<Pdf, String> {
    Pdf::new(bytes).map_err(|e| format!("not a readable PDF ({e:?})"))
}

pub fn page_count(bytes: &[u8]) -> Result<usize, String> {
    Ok(open(bytes.to_vec())?.pages().len())
}

/// Render one page (0-based) for use as a floor plan, on a white background.
pub fn render_page(bytes: Vec<u8>, index: usize) -> Result<Vec<u8>, String> {
    let pdf = open(bytes)?;
    let pages = pdf.pages();
    let page = pages.get(index).ok_or("that page does not exist")?;
    rasterize(page, &RenderCache::new(), PAGE_SIDE_PX)
}

/// Render every page as a small PNG, in order, handing each to `on_page`
/// (which returns false to stop early). Pages that fail to render are skipped.
pub fn thumbnails(bytes: Vec<u8>, side_px: f32, mut on_page: impl FnMut(usize, Vec<u8>) -> bool) {
    let Ok(pdf) = open(bytes) else { return };
    let cache = RenderCache::new();
    for (i, page) in pdf.pages().iter().enumerate() {
        if let Ok(png) = rasterize(page, &cache, side_px)
            && !on_page(i, png)
        {
            return;
        }
    }
}

/// The scale to draw a `w` by `h` point page at so its longer side is about `target_side` pixels,
/// never above [`MAX_OUTPUT_PX`]. `None` when the page has no real size.
fn output_scale(w: f32, h: f32, target_side: f32) -> Option<f32> {
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return None;
    }
    let longest = w.max(h);
    let scale = (target_side / longest).clamp(0.1, MAX_SCALE);
    // The floor on the scale must not let a huge page through.
    Some(scale.min(MAX_OUTPUT_PX / longest))
}

fn rasterize<'a>(page: &'a Page<'a>, cache: &RenderCache<'a>, target_side: f32) -> Result<Vec<u8>, String> {
    let (w, h) = page.render_dimensions();
    let scale = output_scale(w, h, target_side).ok_or("the PDF page has no usable size")?;
    let rendered = render(
        page,
        cache,
        &InterpreterSettings::default(),
        &RenderSettings::default(),
        &PixmapSettings { x_scale: scale, y_scale: scale, bg_color: WHITE },
    );
    rendered.into_png().map_err(|e| format!("could not encode the page ({e:?})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-page PDF with a filled black square, built with correct offsets.
    fn tiny_pdf() -> Vec<u8> {
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R >>".to_string(),
            {
                let c = "0 0 0 rg 0 0 100 100 re f";
                format!("<< /Length {} >>\nstream\n{c}\nendstream", c.len())
            },
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
        for off in offsets {
            out.extend(format!("{off:010} 00000 n \n").bytes());
        }
        out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).bytes());
        out
    }

    #[test]
    fn a_page_is_never_drawn_wider_than_the_limit_and_nonsense_sizes_are_refused() {
        // An ordinary page keeps its scale.
        assert_eq!(output_scale(200.0, 100.0, 4000.0), Some(8.0_f32.min(4000.0 / 200.0)));
        // A page claiming to be 1,000,000 points wide: the 0.1 floor alone would give 100,000 px.
        let scale = output_scale(1_000_000.0, 500.0, 4000.0).unwrap();
        assert!(1_000_000.0 * scale <= MAX_OUTPUT_PX + 0.5, "{scale}");
        // A tall page is bounded on its longer side too.
        assert!(80_000.0 * output_scale(100.0, 80_000.0, 4000.0).unwrap() <= MAX_OUTPUT_PX + 0.5);
        for (w, h) in [(0.0, 10.0), (10.0, -1.0), (f32::NAN, 5.0), (f32::INFINITY, 5.0)] {
            assert_eq!(output_scale(w, h, 4000.0), None, "{w} x {h}");
        }
    }

    #[test]
    fn renders_a_page_scaled_up_to_the_cap() {
        let bytes = tiny_pdf();
        assert!(is_pdf(&bytes));
        assert_eq!(page_count(&bytes), Ok(1));
        let img = image::load_from_memory(&render_page(bytes, 0).unwrap()).unwrap().to_rgba8();
        assert_eq!((img.width(), img.height()), (1600, 800));
        // Left half is the black square, right half the white background.
        assert_eq!(img.get_pixel(400, 400).0[..3], [0, 0, 0]);
        assert_eq!(img.get_pixel(1200, 400).0[..3], [255, 255, 255]);
    }

    #[test]
    fn missing_page_and_garbage_are_errors() {
        assert!(render_page(tiny_pdf(), 1).is_err());
        assert!(page_count(b"%PDF-1.4 nonsense").is_err());
    }

    #[test]
    fn thumbnails_arrive_per_page_and_can_stop_early() {
        let mut got = Vec::new();
        thumbnails(tiny_pdf(), 100.0, |i, png| {
            got.push((i, png));
            true
        });
        assert_eq!(got.len(), 1);
        let img = image::load_from_memory(&got[0].1).unwrap();
        assert_eq!(img.width(), 100);
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! Interpolating survey measurements into a heatmap grid.

/// Signal range of the colour ramp, in dBm.
pub const RAMP_MIN: f64 = -90.0;
pub const RAMP_MAX: f64 = -45.0;

/// A measured value at a floor plan position (pixels).
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub x: f32,
    pub y: f32,
    pub dbm: f64,
}

/// Row-major grid covering the whole floor plan.
#[derive(Debug, Clone)]
pub struct Grid {
    pub w: usize,
    pub h: usize,
    /// Interpolated signal per cell.
    pub dbm: Vec<f32>,
    /// 1 near a sample, fading to 0 at the fade distance. Cells with low
    /// confidence are extrapolation, so they are drawn faintly.
    pub confidence: Vec<f32>,
    /// Width and height of one cell in floor plan pixels.
    pub step_px: f32,
}

/// Inverse-distance-weighted interpolation (power 2) over a grid of at most
/// `max_cells` along the longer side. `fade_px` is the distance from the
/// nearest sample at which confidence reaches zero (it is full up to half).
pub fn interpolate(samples: &[Sample], img_w: u32, img_h: u32, max_cells: usize, fade_px: f32) -> Grid {
    let longest = img_w.max(img_h).max(1) as f32;
    let step = (longest / max_cells as f32).max(1.0);
    let (w, h) = (((img_w as f32 / step).ceil() as usize).max(1), ((img_h as f32 / step).ceil() as usize).max(1));
    let mut dbm = vec![crate::survey::MISSING_DBM as f32; w * h];
    let mut confidence = vec![0.0; w * h];
    if samples.is_empty() {
        return Grid { w, h, dbm, confidence, step_px: step };
    }
    let fade_px = fade_px.max(1.0);
    for gy in 0..h {
        for gx in 0..w {
            let (px, py) = ((gx as f32 + 0.5) * step, (gy as f32 + 0.5) * step);
            let (mut num, mut den, mut nearest) = (0.0f64, 0.0f64, f32::MAX);
            for s in samples {
                let d2 = (s.x - px).powi(2) + (s.y - py).powi(2);
                nearest = nearest.min(d2);
                let wgt = 1.0 / (d2 as f64 + 1.0);
                num += wgt * s.dbm;
                den += wgt;
            }
            let i = gy * w + gx;
            dbm[i] = (num / den) as f32;
            let d = nearest.sqrt();
            confidence[i] = (1.0 - (d - fade_px * 0.5) / (fade_px * 0.5)).clamp(0.0, 1.0);
        }
    }
    Grid { w, h, dbm, confidence, step_px: step }
}

/// Distance in plan pixels over which a heatmap's confidence fades out from a
/// survey point. It is `fade_m` metres when the scale is known, but kept
/// between 4% and 80% of the plan's longer side so that a wrong scale (for
/// example from two nearly identical clicks) cannot shrink the measured area
/// to nothing and leave the whole map looking unmeasured.
pub fn fade_px(fade_m: f32, metres_per_px: Option<f32>, img_w: u32, img_h: u32) -> f32 {
    let longest = img_w.max(img_h).max(1) as f32;
    match metres_per_px.filter(|m| m.is_finite() && *m > 0.0) {
        Some(m) => (fade_m / m).clamp(longest * 0.04, longest * 0.8),
        None => longest * 0.25,
    }
}

/// The fade distance used when there are too few points to tell their spacing.
pub const DEFAULT_FADE_M: f32 = 8.0;
/// Limits on the automatic fade distance, in metres.
const MIN_FADE_M: f32 = 3.0;
const MAX_FADE_M: f32 = 15.0;

/// The fade distance, in plan pixels, chosen from how far apart the survey
/// points are: one and a half times the typical gap to the nearest neighbour,
/// so the whole of a well-covered area stays solid while the colour stops
/// where nothing was measured. Kept between 3 m and 15 m when the scale is known
/// (between 4% and 40% of the plan's longer side when it is not).
pub fn auto_fade_px(points: &[[f32; 2]], metres_per_px: Option<f32>, img_w: u32, img_h: u32) -> f32 {
    let longest = img_w.max(img_h).max(1) as f32;
    let mut gaps: Vec<f32> = points
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            points
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, q)| (p[0] - q[0]).hypot(p[1] - q[1]))
                .filter(|d| *d > 0.5)
                .min_by(f32::total_cmp)
        })
        .collect();
    if gaps.is_empty() {
        return fade_px(DEFAULT_FADE_M, metres_per_px, img_w, img_h);
    }
    gaps.sort_by(f32::total_cmp);
    let typical = gaps[gaps.len() / 2];
    let (lo, hi) = match metres_per_px.filter(|m| m.is_finite() && *m > 0.0) {
        Some(m) => (MIN_FADE_M / m, MAX_FADE_M / m),
        None => (longest * 0.04, longest * 0.4),
    };
    // Never outside what `fade_px` allows either.
    (typical * 1.5).clamp(lo.max(longest * 0.04), hi.min(longest * 0.8).max(lo.max(longest * 0.04)))
}

/// Plan sizes (the longer side, in metres) a calibrated scale may imply.
/// Anything outside is almost certainly a calibration mistake.
pub const PLAUSIBLE_PLAN_M: std::ops::RangeInclusive<f32> = 1.0..=2000.0;

/// Whether a scale gives a believable plan size.
pub fn scale_is_plausible(metres_per_px: f32, img_w: u32, img_h: u32) -> bool {
    PLAUSIBLE_PLAN_M.contains(&(img_w.max(img_h) as f32 * metres_per_px))
}

/// Thresholds for rating coverage, in dBm. Roughly: enough for reliable
/// video and voice, enough for browsing, and below that.
pub const GOOD_DBM: f64 = -67.0;
pub const FAIR_DBM: f64 = -75.0;
/// Cells at least this confident count as measured area.
const MEASURED_CONFIDENCE: f32 = 0.5;

/// Share of the surveyed area in each coverage class.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coverage {
    /// Cells close enough to a sample to count as surveyed.
    pub measured_cells: usize,
    pub good_pct: f32,
    pub fair_pct: f32,
    pub poor_pct: f32,
}

pub fn coverage(grid: &Grid) -> Coverage {
    let (mut good, mut fair, mut poor) = (0usize, 0usize, 0usize);
    for (&d, &c) in grid.dbm.iter().zip(&grid.confidence) {
        if c < MEASURED_CONFIDENCE {
            continue;
        }
        match d as f64 {
            d if d >= GOOD_DBM => good += 1,
            d if d >= FAIR_DBM => fair += 1,
            _ => poor += 1,
        }
    }
    let n = good + fair + poor;
    let pct = |x: usize| if n == 0 { 0.0 } else { x as f32 * 100.0 / n as f32 };
    Coverage { measured_cells: n, good_pct: pct(good), fair_pct: pct(fair), poor_pct: pct(poor) }
}

/// Colour ramps for heatmaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ColorScheme {
    /// Red (weak) through amber and yellow to green (strong).
    #[default]
    Classic,
    /// Blue (weak) through cyan, green and yellow to red (strong).
    Rainbow,
    /// Blue (weak) to red (strong).
    CoolWarm,
}

impl ColorScheme {
    pub const ALL: [ColorScheme; 3] = [ColorScheme::Classic, ColorScheme::Rainbow, ColorScheme::CoolWarm];

    pub fn name(self) -> &'static str {
        match self {
            ColorScheme::Classic => "Red to green",
            ColorScheme::Rainbow => "Blue to red",
            ColorScheme::CoolWarm => "Cool to warm",
        }
    }

    fn stops(self) -> &'static [[f64; 3]] {
        match self {
            ColorScheme::Classic => &[[200.0, 45.0, 50.0], [240.0, 140.0, 50.0], [235.0, 205.0, 60.0], [140.0, 205.0, 90.0], [40.0, 170.0, 110.0]],
            ColorScheme::Rainbow => &[[50.0, 70.0, 220.0], [40.0, 190.0, 220.0], [60.0, 190.0, 90.0], [240.0, 220.0, 60.0], [225.0, 50.0, 45.0]],
            ColorScheme::CoolWarm => &[[60.0, 90.0, 220.0], [140.0, 150.0, 225.0], [225.0, 225.0, 225.0], [240.0, 150.0, 120.0], [200.0, 50.0, 60.0]],
        }
    }
}

/// Colour for a signal level on a scheme whose ramp spans `min_dbm` to
/// `max_dbm`; levels outside are clamped to the ends.
pub fn color_in(scheme: ColorScheme, dbm: f64, min_dbm: f64, max_dbm: f64) -> [u8; 3] {
    let stops = scheme.stops();
    let t = ((dbm - min_dbm) / (max_dbm - min_dbm).max(1.0)).clamp(0.0, 1.0) * (stops.len() - 1) as f64;
    let i = (t.floor() as usize).min(stops.len() - 2);
    let f = t - i as f64;
    std::array::from_fn(|c| (stops[i][c] + (stops[i + 1][c] - stops[i][c]) * f).round() as u8)
}

/// Colour for a signal level with the default scheme and range.
pub fn color_for(dbm: f64) -> [u8; 3] {
    color_in(ColorScheme::Classic, dbm, RAMP_MIN, RAMP_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_matches_a_sample_at_its_location() {
        let g = interpolate(&[Sample { x: 5.0, y: 5.0, dbm: -55.0 }], 10, 10, 10, 8.0);
        assert!((g.dbm[5 * g.w + 5] + 55.0).abs() < 0.01);
        assert_eq!(g.confidence[5 * g.w + 5], 1.0);
    }

    #[test]
    fn interpolation_stays_between_samples_and_confidence_fades() {
        let s = [Sample { x: 0.0, y: 50.0, dbm: -50.0 }, Sample { x: 99.0, y: 50.0, dbm: -80.0 }];
        let g = interpolate(&s, 100, 100, 100, 20.0);
        let mid = g.dbm[50 * g.w + 50];
        assert!(mid < -50.0 && mid > -80.0);
        assert_eq!(g.confidence[50 * g.w + 50], 0.0);
        assert_eq!(g.confidence[50 * g.w], 1.0);
    }

    #[test]
    fn ramp_ends_are_clamped() {
        assert_eq!(color_for(-200.0), color_for(RAMP_MIN));
        assert_eq!(color_for(0.0), color_for(RAMP_MAX));
    }

    #[test]
    fn coverage_counts_only_measured_cells() {
        let g = Grid { w: 4, h: 1, dbm: vec![-50.0, -70.0, -90.0, -50.0], confidence: vec![1.0, 1.0, 1.0, 0.0], step_px: 2.0 };
        let c = coverage(&g);
        assert_eq!(c.measured_cells, 3);
        assert!((c.good_pct - 100.0 / 3.0).abs() < 0.01);
        assert!((c.fair_pct - 100.0 / 3.0).abs() < 0.01);
        assert!((c.poor_pct - 100.0 / 3.0).abs() < 0.01);
        let none = coverage(&Grid { w: 1, h: 1, dbm: vec![-50.0], confidence: vec![0.0], step_px: 1.0 });
        assert_eq!(none.measured_cells, 0);
    }

    #[test]
    fn schemes_run_weak_to_strong_and_respect_the_range() {
        for scheme in ColorScheme::ALL {
            assert_ne!(color_in(scheme, -90.0, -90.0, -45.0), color_in(scheme, -45.0, -90.0, -45.0));
            assert_eq!(color_in(scheme, -200.0, -90.0, -45.0), color_in(scheme, -90.0, -90.0, -45.0));
            // Narrowing the range moves where a level falls on the ramp.
            assert_ne!(color_in(scheme, -60.0, -90.0, -45.0), color_in(scheme, -60.0, -70.0, -50.0));
        }
        assert_eq!(color_for(-60.0), color_in(ColorScheme::Classic, -60.0, RAMP_MIN, RAMP_MAX));
    }

    #[test]
    fn fade_distance_follows_the_scale_but_survives_a_wrong_one() {
        // 8 m at 0.05 m/px is 160 px: exactly the 4% floor of a 4000 px plan.
        assert_eq!(fade_px(8.0, Some(0.05), 2828, 4000), 160.0);
        assert_eq!(fade_px(20.0, Some(0.05), 2828, 4000), 400.0);
        // The real-world mistake: 13 m per pixel would give 0.6 px.
        assert_eq!(fade_px(8.0, Some(13.28), 2828, 4000), 160.0);
        assert_eq!(fade_px(8.0, Some(0.00001), 2828, 4000), 3200.0);
        assert_eq!(fade_px(8.0, None, 2828, 4000), 1000.0);
        assert_eq!(fade_px(8.0, Some(f32::NAN), 2828, 4000), 1000.0);
    }

    #[test]
    fn the_fade_distance_follows_the_spacing_of_the_points() {
        // 0.05 m per pixel: points 100 px (5 m) apart give 7.5 m = 150 px.
        let row: Vec<[f32; 2]> = (0..5).map(|i| [100.0 * i as f32, 50.0]).collect();
        assert!((auto_fade_px(&row, Some(0.05), 2000, 1000) - 150.0).abs() < 0.01);
        // Close points are held to the 3 m floor, distant ones to the 15 m ceiling.
        let tight: Vec<[f32; 2]> = (0..5).map(|i| [10.0 * i as f32, 50.0]).collect();
        assert_eq!(auto_fade_px(&tight, Some(0.05), 1000, 500), 60.0);
        let sparse: Vec<[f32; 2]> = (0..3).map(|i| [900.0 * i as f32, 50.0]).collect();
        assert_eq!(auto_fade_px(&sparse, Some(0.05), 2900, 1000), 300.0);
        // One point, or none, cannot tell the spacing: the 8 m default.
        assert_eq!(auto_fade_px(&[[5.0, 5.0]], Some(0.05), 2000, 1000), 160.0);
        assert_eq!(auto_fade_px(&[], Some(0.05), 2000, 1000), 160.0);
        // Points on top of each other are one place.
        assert_eq!(auto_fade_px(&[[5.0, 5.0], [5.0, 5.0]], Some(0.05), 2000, 1000), 160.0);
        // One lonely outlier does not change the typical gap.
        let mut with_outlier = row.clone();
        with_outlier.push([1900.0, 900.0]);
        assert!((auto_fade_px(&with_outlier, Some(0.05), 2000, 1000) - 150.0).abs() < 0.01);
        // Without a scale it is a share of the plan, and never zero.
        let no_scale = auto_fade_px(&row, None, 2000, 1000);
        assert!((80.0..=800.0).contains(&no_scale), "{no_scale}");
    }

    #[test]
    fn implausible_plan_sizes_are_recognised() {
        assert!(scale_is_plausible(0.0525, 2828, 4000), "a house is about 210 m across at this scale: fine");
        assert!(!scale_is_plausible(13.28, 2828, 4000), "53 km");
        assert!(!scale_is_plausible(0.0001, 2828, 4000), "0.4 m");
    }
}

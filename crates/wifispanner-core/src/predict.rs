// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! Predicted coverage from planned access points and the walls drawn on a
//! floor: free-space loss at one metre, a path-loss exponent beyond that, and
//! the loss of each wall the straight line from the AP to the receiver
//! passes through. A wall only counts where the line is lower than the wall,
//! so an AP mounted above shelving reaches over it.
//!
//! These are textbook starting values. Calibrating them against a real
//! survey is a later step, so treat predictions as estimates.

use serde::{Deserialize, Serialize};
use wifispanner_scan::Band;

use crate::heatmap::Grid;
use crate::obstacles::Area;
use crate::survey::Floor;

/// A frequency band, in a form that can be saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RadioBand {
    Ghz2_4,
    Ghz5,
    Ghz6,
}

impl RadioBand {
    pub const ALL: [RadioBand; 3] = [RadioBand::Ghz2_4, RadioBand::Ghz5, RadioBand::Ghz6];

    pub fn band(self) -> Band {
        match self {
            RadioBand::Ghz2_4 => Band::Ghz2_4,
            RadioBand::Ghz5 => Band::Ghz5,
            RadioBand::Ghz6 => Band::Ghz6,
        }
    }

    pub fn from_band(b: Band) -> Self {
        match b {
            Band::Ghz2_4 => RadioBand::Ghz2_4,
            Band::Ghz5 => RadioBand::Ghz5,
            Band::Ghz6 => RadioBand::Ghz6,
        }
    }
}

/// One radio of an access point: its band, transmit power and antenna gain.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Radio {
    pub band: RadioBand,
    pub tx_dbm: f32,
    pub gain_dbi: f32,
    /// The most the radio may radiate (transmit power plus gain) where a
    /// regulatory limit is below that sum.
    #[serde(default)]
    pub eirp_cap_dbm: Option<f32>,
}

/// The channel an access point should use on one of its bands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    pub band: RadioBand,
    pub number: u32,
}

/// An access point on the plan: one that is really there, or one being planned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedAp {
    pub name: String,
    /// Position in floor plan pixels.
    pub x: f32,
    pub y: f32,
    /// Mounting height above the floor, in metres.
    pub height_m: f32,
    /// Used for any band the access point has no entry in `radios` for, when
    /// `radios` is empty: a plain AP with one power and gain on every band.
    pub tx_dbm: f32,
    pub gain_dbi: f32,
    pub enabled: bool,
    /// True for an access point that is installed (plotted while surveying);
    /// false for one only planned.
    #[serde(default)]
    pub existing: bool,
    /// The catalogue model it was taken from, if any.
    #[serde(default)]
    pub model: Option<String>,
    /// Per-band power and gain, from the model or set by hand. Empty means
    /// `tx_dbm` and `gain_dbi` on every band; otherwise only these bands exist.
    #[serde(default)]
    pub radios: Vec<Radio>,
    /// The measured access point this stands for (see [`crate::survey::ap_base_key`]),
    /// so reports can place it where it really is.
    #[serde(default)]
    pub link: Option<String>,
    /// Unique on its floor; 0 until given one.
    #[serde(default)]
    pub id: u64,
    /// For a plan access point standing for an installed one: that one's `id`.
    #[serde(default)]
    pub from: Option<u64>,
    /// The channels recommended for it, one per band, once a channel plan has been made.
    #[serde(default)]
    pub channels: Vec<Channel>,
}

impl PlannedAp {
    pub const DEFAULT_HEIGHT_M: f32 = 2.7;
    pub const DEFAULT_TX_DBM: f32 = 20.0;
    pub const DEFAULT_GAIN_DBI: f32 = 3.0;

    pub fn new(name: String, x: f32, y: f32) -> Self {
        PlannedAp {
            name,
            x,
            y,
            height_m: Self::DEFAULT_HEIGHT_M,
            tx_dbm: Self::DEFAULT_TX_DBM,
            gain_dbi: Self::DEFAULT_GAIN_DBI,
            enabled: true,
            existing: false,
            model: None,
            radios: Vec::new(),
            link: None,
            id: 0,
            from: None,
            channels: Vec::new(),
        }
    }

    /// What the access point radiates on a band (power plus gain, held to the
    /// radio's regulatory cap), or `None` when it has no radio there.
    pub fn eirp_dbm(&self, band: Band) -> Option<f32> {
        let (tx, gain) = self.radio(band)?;
        let cap = self.radios.iter().find(|r| r.band == RadioBand::from_band(band)).and_then(|r| r.eirp_cap_dbm);
        Some(cap.map_or(tx + gain, |c| (tx + gain).min(c)))
    }

    /// Transmit power and antenna gain on a band, or `None` when the access
    /// point has no radio there.
    pub fn radio(&self, band: Band) -> Option<(f32, f32)> {
        if self.radios.is_empty() {
            return Some((self.tx_dbm, self.gain_dbi));
        }
        let b = RadioBand::from_band(band);
        self.radios.iter().find(|r| r.band == b).map(|r| (r.tx_dbm, r.gain_dbi))
    }
}

/// Path-loss exponent beyond the first metre; indoors it runs from about 2 to 4.
pub const PATH_LOSS_EXPONENT: f32 = 3.0;
/// Receivers are modelled this high above the floor unless told otherwise (a phone or laptop in use).
pub const DEFAULT_RX_HEIGHT_M: f32 = 1.0;
/// Grid resolution along the longer side of the plan.
pub const GRID_CELLS: usize = 160;

/// A frequency standing for each band.
pub fn band_mhz(band: Band) -> f32 {
    match band {
        Band::Ghz2_4 => 2437.0,
        Band::Ghz5 => 5500.0,
        Band::Ghz6 => 6000.0,
    }
}

/// Free-space loss at one metre.
fn loss_at_1m(band: Band) -> f32 {
    20.0 * band_mhz(band).log10() - 27.56
}

fn wall_loss(material: crate::Material, band: Band) -> f32 {
    let a = material.attenuation();
    match band {
        Band::Ghz2_4 => a.ghz2_4,
        Band::Ghz5 => a.ghz5,
        Band::Ghz6 => a.ghz6,
    }
}

/// A wall in metres, ready for line tests.
struct MetreWall {
    a: [f32; 2],
    b: [f32; 2],
    height: f32,
    loss: f32,
}

fn metre_walls(floor: &Floor, mpp: f32, band: Band) -> Vec<MetreWall> {
    floor
        .walls
        .iter()
        .map(|w| MetreWall {
            a: [w.a[0] * mpp, w.a[1] * mpp],
            b: [w.b[0] * mpp, w.b[1] * mpp],
            height: w.effective_height_m(floor.ceiling_m),
            loss: wall_loss(w.material, band),
        })
        .collect()
}

/// What lies between the access points and the receivers, in metres.
pub(crate) struct Scene {
    walls: Vec<MetreWall>,
    /// Obstacle areas with their points in metres, and each one's loss per metre and height.
    areas: Vec<(Area, f32, f32)>,
}

pub(crate) fn scene(floor: &Floor, mpp: f32, band: Band) -> Scene {
    let areas = floor
        .areas
        .iter()
        .map(|a| {
            let pts = a.points.iter().map(|p| [p[0] * mpp, p[1] * mpp]).collect();
            (Area { points: pts, kind: a.kind, height_m: a.height_m }, a.kind.db_per_m(), a.effective_height_m(floor.ceiling_m))
        })
        .collect();
    Scene { walls: metre_walls(floor, mpp, band), areas }
}

/// Loss in dB along the line through obstacle areas: each area's dB per metre
/// for the length of the line inside it that is lower than the area is tall.
fn areas_between(areas: &[(Area, f32, f32)], from: [f32; 3], to: [f32; 3]) -> f32 {
    let length = ((to[0] - from[0]).powi(2) + (to[1] - from[1]).powi(2) + (to[2] - from[2]).powi(2)).sqrt();
    let mut loss = 0.0;
    for (area, db_per_m, height) in areas {
        for (t0, t1) in area.inside_ranges([from[0], from[1]], [to[0], to[1]]) {
            let dz = to[2] - from[2];
            let (lo, hi) = if dz.abs() < 1e-6 {
                if from[2] <= *height { (t0, t1) } else { continue }
            } else {
                // The line is below the height on one side of this parameter.
                let tc = (height - from[2]) / dz;
                if dz > 0.0 { (t0, t1.min(tc)) } else { (t0.max(tc), t1) }
            };
            if hi > lo {
                loss += db_per_m * (hi - lo) * length;
            }
        }
    }
    loss
}

/// Loss in dB from the walls the line `from` to `to` (x, y, height in metres) passes through.
fn walls_between(walls: &[MetreWall], from: [f32; 3], to: [f32; 3]) -> f32 {
    let (rx, ry) = (to[0] - from[0], to[1] - from[1]);
    let mut loss = 0.0;
    for w in walls {
        let (sx, sy) = (w.b[0] - w.a[0], w.b[1] - w.a[1]);
        let denom = rx * sy - ry * sx;
        if denom.abs() < 1e-9 {
            continue;
        }
        let (qx, qy) = (w.a[0] - from[0], w.a[1] - from[1]);
        let t = (qx * sy - qy * sx) / denom;
        let u = (qx * ry - qy * rx) / denom;
        if !(0.0..=1.0).contains(&t) || !(0.0..=1.0).contains(&u) {
            continue;
        }
        let z = from[2] + (to[2] - from[2]) * t;
        if z <= w.height {
            loss += w.loss;
        }
    }
    loss
}

/// Signal in dBm at `rx` (x, y, height in metres) from one AP.
pub(crate) fn signal_from(ap: &PlannedAp, eirp_dbm: f32, ap_xy_m: [f32; 2], rx: [f32; 3], scene: &Scene, band: Band) -> f32 {
    let from = [ap_xy_m[0], ap_xy_m[1], ap.height_m];
    let d = ((rx[0] - from[0]).powi(2) + (rx[1] - from[1]).powi(2) + (rx[2] - from[2]).powi(2)).sqrt().max(1.0);
    eirp_dbm - loss_at_1m(band) - 10.0 * PATH_LOSS_EXPONENT * d.log10() - walls_between(&scene.walls, from, rx) - areas_between(&scene.areas, from, rx)
}

/// The access point that serves a spot best and its signal there, for a
/// receiver `rx_height_m` above the floor at `pos_px` on the plan; `None` when
/// no enabled access point has a radio on the band, or the scale is not set.
pub fn best_at(floor: &Floor, aps: &[PlannedAp], band: Band, pos_px: [f32; 2], rx_height_m: f32) -> Option<(usize, f32)> {
    let mpp = floor.metres_per_px.filter(|m| m.is_finite() && *m > 0.0)?;
    let scene = scene(floor, mpp, band);
    let rx = [pos_px[0] * mpp, pos_px[1] * mpp, rx_height_m];
    aps.iter()
        .enumerate()
        .filter(|(_, a)| a.enabled)
        .filter_map(|(i, a)| a.eirp_dbm(band).map(|e| (i, signal_from(a, e, [a.x * mpp, a.y * mpp], rx, &scene, band))))
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

/// Why a prediction cannot be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    NoScale,
    NoAps,
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Unavailable::NoScale => "Set the plan's scale first, so distances are real.",
            Unavailable::NoAps => "No access point on this band yet. Plot the existing ones while surveying, or place planned ones here.",
        })
    }
}

/// The strongest signal from any enabled AP at each cell of a grid over the
/// floor, for receivers `rx_height_m` above the floor.
pub fn predict(floor: &Floor, band: Band, rx_height_m: f32) -> Result<Grid, Unavailable> {
    let mpp = floor.metres_per_px.filter(|m| m.is_finite() && *m > 0.0).ok_or(Unavailable::NoScale)?;
    // Enabled access points with a radio on this band, and what each radiates.
    let aps: Vec<(&PlannedAp, f32)> = floor.aps.iter().filter(|a| a.enabled).filter_map(|a| a.eirp_dbm(band).map(|eirp| (a, eirp))).collect();
    if aps.is_empty() {
        return Err(Unavailable::NoAps);
    }
    let longest = floor.width_px.max(floor.height_px).max(1) as f32;
    let step = (longest / GRID_CELLS as f32).max(1.0);
    let w = ((floor.width_px as f32 / step).ceil() as usize).max(1);
    let h = ((floor.height_px as f32 / step).ceil() as usize).max(1);
    let walls = scene(floor, mpp, band);
    let ap_m: Vec<[f32; 2]> = aps.iter().map(|(a, _)| [a.x * mpp, a.y * mpp]).collect();
    let mut dbm = vec![0.0f32; w * h];
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(h);
    let rows_per = h.div_ceil(threads);
    std::thread::scope(|scope| {
        for (chunk_index, rows) in dbm.chunks_mut(rows_per * w).enumerate() {
            let (aps, ap_m, walls) = (&aps, &ap_m, &walls);
            scope.spawn(move || {
                for (i, cell) in rows.iter_mut().enumerate() {
                    let index = chunk_index * rows_per * w + i;
                    let (gx, gy) = (index % w, index / w);
                    let rx = [(gx as f32 + 0.5) * step * mpp, (gy as f32 + 0.5) * step * mpp, rx_height_m];
                    *cell = aps.iter().zip(ap_m).map(|((ap, eirp), xy)| signal_from(ap, *eirp, *xy, rx, walls, band)).fold(f32::MIN, f32::max);
                }
            });
        }
    });
    Ok(Grid { w, h, dbm, confidence: vec![1.0; w * h], step_px: step })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::floors::Placement;
    use crate::walls::Wall;
    use crate::Material;

    fn floor(aps: Vec<PlannedAp>, walls: Vec<Wall>) -> Floor {
        Floor {
            name: "t".into(),
            image: vec![],
            width_px: 200,
            height_px: 100,
            metres_per_px: Some(0.1),
            points: vec![],
            walls,
            ceiling_m: 3.0,
            placement: Placement::default(),
            aps,
            survey_aps: vec![],
            areas: vec![],
        }
    }

    fn ap(x: f32, y: f32, height: f32) -> PlannedAp {
        PlannedAp { height_m: height, ..PlannedAp::new("ap".into(), x, y) }
    }

    fn at(g: &Grid, mpp: f32, x_m: f32, y_m: f32) -> f32 {
        let (gx, gy) = ((x_m / mpp / g.step_px) as usize, (y_m / mpp / g.step_px) as usize);
        g.dbm[gy * g.w + gx]
    }

    #[test]
    fn free_space_loss_at_one_metre_matches_the_textbook() {
        assert!((loss_at_1m(Band::Ghz2_4) - 40.2).abs() < 0.2);
        assert!((loss_at_1m(Band::Ghz5) - 47.3).abs() < 0.2);
        assert!(loss_at_1m(Band::Ghz6) > loss_at_1m(Band::Ghz5));
    }

    #[test]
    fn signal_falls_with_distance_and_is_strongest_under_the_ap() {
        let f = floor(vec![ap(100.0, 50.0, 2.7)], vec![]);
        let g = predict(&f, Band::Ghz5, 1.0).unwrap();
        let (near, mid, far) = (at(&g, 0.1, 10.0, 5.0), at(&g, 0.1, 15.0, 5.0), at(&g, 0.1, 19.5, 5.0));
        assert!(near > mid && mid > far, "{near} {mid} {far}");
        // 20 dBm + 3 dBi - 47.3 dB at 1 m, about 1.7 m away in 3D.
        assert!(near < 20.0 + 3.0 - 47.3 + 1.0 && near > -40.0, "{near}");
    }

    #[test]
    fn a_wall_between_costs_its_attenuation() {
        let clear = floor(vec![ap(20.0, 50.0, 2.7)], vec![]);
        let wall = Wall { height_m: None, ..Wall::new([100.0, 0.0], [100.0, 100.0], Material::Concrete) };
        let walled = floor(vec![ap(20.0, 50.0, 2.7)], vec![wall]);
        let (a, b) = (predict(&clear, Band::Ghz5, 1.0).unwrap(), predict(&walled, Band::Ghz5, 1.0).unwrap());
        let drop = at(&a, 0.1, 15.0, 5.0) - at(&b, 0.1, 15.0, 5.0);
        assert!((drop - Material::Concrete.attenuation().ghz5).abs() < 0.01, "{drop}");
        assert!((at(&a, 0.1, 5.0, 5.0) - at(&b, 0.1, 5.0, 5.0)).abs() < 1e-3, "same side of the wall is unaffected");
    }

    #[test]
    fn an_ap_mounted_above_a_low_wall_reaches_over_it() {
        let shelf = Wall { height_m: Some(2.0), ..Wall::new([100.0, 0.0], [100.0, 100.0], Material::Metal) };
        let low_ap = floor(vec![ap(20.0, 50.0, 1.5)], vec![shelf]);
        let high_ap = floor(vec![ap(20.0, 50.0, 6.0)], vec![shelf]);
        let (l, h) = (predict(&low_ap, Band::Ghz5, 1.0).unwrap(), predict(&high_ap, Band::Ghz5, 1.0).unwrap());
        let behind = (at(&l, 0.1, 15.0, 5.0), at(&h, 0.1, 15.0, 5.0));
        // The low AP's line stays under the 2 m shelf; the high AP's line crosses it above 2 m.
        assert!(behind.0 < -60.0, "blocked: {}", behind.0);
        assert!(behind.1 > behind.0 + 20.0, "over the top: {} vs {}", behind.1, behind.0);
    }

    #[test]
    fn the_strongest_enabled_ap_wins_and_disabled_ones_are_ignored() {
        let mut off = ap(20.0, 50.0, 2.7);
        off.enabled = false;
        let only_far = floor(vec![off, ap(180.0, 50.0, 2.7)], vec![]);
        let both = floor(vec![ap(20.0, 50.0, 2.7), ap(180.0, 50.0, 2.7)], vec![]);
        let (a, b) = (predict(&only_far, Band::Ghz5, 1.0).unwrap(), predict(&both, Band::Ghz5, 1.0).unwrap());
        assert!(at(&b, 0.1, 2.0, 5.0) > at(&a, 0.1, 2.0, 5.0) + 10.0);
        assert!((at(&a, 0.1, 18.0, 5.0) - at(&b, 0.1, 18.0, 5.0)).abs() < 1e-3);
    }

    #[test]
    fn an_obstacle_area_costs_its_loss_for_each_metre_the_signal_is_inside_it() {
        use crate::obstacles::{Area, ObstacleKind};
        let mut f = floor(vec![ap(20.0, 50.0, 1.0)], vec![]);
        let clear = predict(&f, Band::Ghz5, 1.0).unwrap();
        // 5 m deep (50 px at 0.1 m/px), 8 dB per metre, tall enough to catch a line at 1 m.
        f.areas = vec![Area { height_m: Some(2.0), ..Area::rectangle([60.0, 0.0], [110.0, 100.0], ObstacleKind::ShelfWarehouse) }];
        let shelved = predict(&f, Band::Ghz5, 1.0).unwrap();
        let drop = at(&clear, 0.1, 15.0, 5.0) - at(&shelved, 0.1, 15.0, 5.0);
        assert!((drop - 40.0).abs() < 0.5, "5 m x 8 dB/m, got {drop}");
        assert!((at(&clear, 0.1, 3.0, 5.0) - at(&shelved, 0.1, 3.0, 5.0)).abs() < 1e-3, "in front of the shelves nothing changes");
        // An AP above the top of the area is not slowed by it, while a lower one is.
        f.aps = vec![ap(20.0, 50.0, 6.0)];
        f.ceiling_m = 10.0;
        let high = predict(&f, Band::Ghz5, 1.0).unwrap();
        f.areas[0].height_m = Some(8.0);
        let tall = predict(&f, Band::Ghz5, 1.0).unwrap();
        assert!(at(&high, 0.1, 15.0, 5.0) > at(&tall, 0.1, 15.0, 5.0) + 10.0, "the line is only inside the area where it is low enough");
    }

    #[test]
    fn best_at_names_the_serving_ap_and_agrees_with_the_map() {
        let f = floor(vec![ap(20.0, 50.0, 2.7), ap(180.0, 50.0, 2.7)], vec![]);
        let (i, dbm) = best_at(&f, &f.aps, Band::Ghz5, [30.0, 50.0], 1.0).unwrap();
        assert_eq!(i, 0);
        let (j, _) = best_at(&f, &f.aps, Band::Ghz5, [170.0, 50.0], 1.0).unwrap();
        assert_eq!(j, 1);
        let grid = predict(&f, Band::Ghz5, 1.0).unwrap();
        let step = grid.step_px;
        let cell = grid.dbm[((50.0 / step) as usize) * grid.w + (30.0 / step) as usize];
        assert!((cell - dbm).abs() < 1.0, "{cell} vs {dbm}");
        let mut none = f.clone();
        none.metres_per_px = None;
        assert!(best_at(&none, &none.aps, Band::Ghz5, [30.0, 50.0], 1.0).is_none());
    }

    #[test]
    fn an_ap_only_covers_the_bands_it_has_radios_for() {
        let mut dual = ap(100.0, 50.0, 2.7);
        dual.radios = vec![
            Radio { band: RadioBand::Ghz2_4, tx_dbm: 22.0, gain_dbi: 4.0, eirp_cap_dbm: None },
            Radio { band: RadioBand::Ghz5, tx_dbm: 26.0, gain_dbi: 6.0, eirp_cap_dbm: Some(30.0) },
        ];
        assert_eq!(dual.radio(Band::Ghz5), Some((26.0, 6.0)));
        assert_eq!(dual.radio(Band::Ghz6), None);
        let f = floor(vec![dual.clone()], vec![]);
        assert!(predict(&f, Band::Ghz5, 1.0).is_ok());
        assert_eq!(predict(&f, Band::Ghz6, 1.0).unwrap_err(), Unavailable::NoAps, "no 6 GHz radio");
        // The 5 GHz radio is capped at 30 dBm EIRP.
        let plain = floor(vec![ap(100.0, 50.0, 2.7)], vec![]);
        let diff = at(&predict(&f, Band::Ghz5, 1.0).unwrap(), 0.1, 12.0, 5.0) - at(&predict(&plain, Band::Ghz5, 1.0).unwrap(), 0.1, 12.0, 5.0);
        // 26 + 6 = 32 dBm, held to the 30 dBm cap, against the plain AP's 23.
        assert!((diff - 7.0).abs() < 0.01, "{diff}");
        assert_eq!(dual.eirp_dbm(Band::Ghz2_4), Some(26.0));
    }

    #[test]
    fn older_projects_with_plain_aps_still_open() {
        let old = r#"{"name":"a","x":1.0,"y":2.0,"height_m":2.7,"tx_dbm":20.0,"gain_dbi":3.0,"enabled":true}"#;
        let a: PlannedAp = serde_json::from_str(old).unwrap();
        assert!(!a.existing && a.radios.is_empty() && a.model.is_none() && a.link.is_none());
        assert_eq!(a.radio(Band::Ghz6), Some((20.0, 3.0)));
    }

    #[test]
    fn it_says_why_it_cannot_predict() {
        let mut f = floor(vec![ap(20.0, 50.0, 2.7)], vec![]);
        f.metres_per_px = None;
        assert_eq!(predict(&f, Band::Ghz5, 1.0).unwrap_err(), Unavailable::NoScale);
        assert_eq!(predict(&floor(vec![], vec![]), Band::Ghz5, 1.0).unwrap_err(), Unavailable::NoAps);
    }
}

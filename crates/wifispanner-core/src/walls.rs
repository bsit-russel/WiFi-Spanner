// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! Walls drawn on a floor plan, and the snapping used while drawing them.

use serde::{Deserialize, Serialize};

use crate::Material;

/// Ceiling height assumed for a floor, in metres.
pub const DEFAULT_CEILING_M: f32 = 3.0;

/// A straight wall between two points in floor plan pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Wall {
    pub a: [f32; 2],
    pub b: [f32; 2],
    pub material: Material,
    /// Height in metres; `None` is the full height of the floor. A wall lower
    /// than an access point lets its signal pass over the top.
    pub height_m: Option<f32>,
}

impl Wall {
    pub fn new(a: [f32; 2], b: [f32; 2], material: Material) -> Self {
        Wall { a, b, material, height_m: material.default_height_m() }
    }

    pub fn length_px(&self) -> f32 {
        (self.a[0] - self.b[0]).hypot(self.a[1] - self.b[1])
    }

    /// The wall's height given the floor's ceiling height.
    pub fn effective_height_m(&self, ceiling_m: f32) -> f32 {
        self.height_m.unwrap_or(ceiling_m).clamp(0.0, ceiling_m.max(0.0))
    }

    /// The point of the wall nearest to `p`.
    pub fn closest_point(&self, p: [f32; 2]) -> [f32; 2] {
        let (dx, dy) = (self.b[0] - self.a[0], self.b[1] - self.a[1]);
        let len2 = dx * dx + dy * dy;
        let t = if len2 == 0.0 { 0.0 } else { (((p[0] - self.a[0]) * dx + (p[1] - self.a[1]) * dy) / len2).clamp(0.0, 1.0) };
        [self.a[0] + t * dx, self.a[1] + t * dy]
    }

    /// Distance in pixels from `p` to the nearest point of the wall.
    pub fn distance_to(&self, p: [f32; 2]) -> f32 {
        let c = self.closest_point(p);
        (p[0] - c[0]).hypot(p[1] - c[1])
    }
}

impl Wall {
    /// What is left of this wall once `cutter` is drawn over it: where the
    /// cutter runs along the wall (both its ends within `tol` pixels of the
    /// wall's line) the covered stretch is removed, leaving up to two pieces.
    /// A wall the cutter does not lie along is returned whole.
    pub fn carve(&self, cutter: &Wall, tol: f32) -> Vec<Wall> {
        let (dx, dy) = (self.b[0] - self.a[0], self.b[1] - self.a[1]);
        let len = dx.hypot(dy);
        if len < 1.0 {
            return vec![*self];
        }
        let along = |p: [f32; 2]| ((p[0] - self.a[0]) * dx + (p[1] - self.a[1]) * dy) / (len * len);
        let off_line = |p: [f32; 2]| (dx * (p[1] - self.a[1]) - dy * (p[0] - self.a[0])).abs() / len;
        if off_line(cutter.a) > tol || off_line(cutter.b) > tol {
            return vec![*self];
        }
        let (t0, t1) = (along(cutter.a).min(along(cutter.b)), along(cutter.a).max(along(cutter.b)));
        let (lo, hi) = (t0.max(0.0), t1.min(1.0));
        if lo >= hi {
            return vec![*self];
        }
        let at = |t: f32| [self.a[0] + t * dx, self.a[1] + t * dy];
        let mut out = Vec::new();
        if lo * len > MIN_PIECE_PX {
            out.push(Wall { b: at(lo), ..*self });
        }
        if (1.0 - hi) * len > MIN_PIECE_PX {
            out.push(Wall { a: at(hi), ..*self });
        }
        out
    }

    /// True when any part of the wall lies inside the rectangle.
    pub fn intersects_rect(&self, min: [f32; 2], max: [f32; 2]) -> bool {
        // Liang-Barsky clipping of the segment against the rectangle.
        let (dx, dy) = (self.b[0] - self.a[0], self.b[1] - self.a[1]);
        let (mut t0, mut t1) = (0.0f32, 1.0f32);
        for (p, q) in [(-dx, self.a[0] - min[0]), (dx, max[0] - self.a[0]), (-dy, self.a[1] - min[1]), (dy, max[1] - self.a[1])] {
            if p == 0.0 {
                if q < 0.0 {
                    return false;
                }
            } else {
                let r = q / p;
                if p < 0.0 {
                    t0 = t0.max(r);
                } else {
                    t1 = t1.min(r);
                }
                if t0 > t1 {
                    return false;
                }
            }
        }
        true
    }
}

/// Pieces shorter than this (pixels) left over by [`Wall::carve`] are dropped.
const MIN_PIECE_PX: f32 = 2.0;

/// Where a wall being drawn should land.
///
/// The end of a wall snaps to an existing wall end within `radius` pixels.
/// Failing that it snaps onto the nearest wall within `radius`, anywhere
/// along its length, so that a door or window drawn over a wall lies exactly
/// on it. Failing that, when there is a `start`, the direction snaps to the nearest
/// multiple of 15 degrees if within a few degrees of it, which keeps walls
/// square and straight without forcing it.
pub fn snap(p: [f32; 2], start: Option<[f32; 2]>, walls: &[Wall], radius: f32) -> [f32; 2] {
    let nearest = walls
        .iter()
        .flat_map(|w| [w.a, w.b])
        .map(|e| (e, (e[0] - p[0]).hypot(e[1] - p[1])))
        .filter(|(_, d)| *d <= radius)
        .min_by(|x, y| x.1.total_cmp(&y.1));
    if let Some((e, _)) = nearest {
        return e;
    }
    let along = walls
        .iter()
        .map(|w| (w.closest_point(p), w.distance_to(p)))
        .filter(|(_, d)| *d <= radius)
        .min_by(|x, y| x.1.total_cmp(&y.1));
    if let Some((c, _)) = along {
        return c;
    }
    let Some(s) = start else { return p };
    let (dx, dy) = (p[0] - s[0], p[1] - s[1]);
    let len = dx.hypot(dy);
    if len < 1.0 {
        return p;
    }
    let step = 15f32.to_radians();
    let angle = dy.atan2(dx);
    let snapped = (angle / step).round() * step;
    if (angle - snapped).abs() <= 4f32.to_radians() {
        [s[0] + len * snapped.cos(), s[1] + len * snapped.sin()]
    } else {
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(a: [f32; 2], b: [f32; 2]) -> Wall {
        Wall::new(a, b, Material::Concrete)
    }

    #[test]
    fn new_walls_take_the_materials_default_height() {
        assert_eq!(Wall::new([0.0; 2], [1.0, 0.0], Material::Metal).height_m, Some(2.0));
        assert_eq!(Wall::new([0.0; 2], [1.0, 0.0], Material::Glass).height_m, None);
    }

    #[test]
    fn height_is_the_ceiling_unless_set_and_never_above_it() {
        let mut w = wall([0.0; 2], [1.0, 0.0]);
        assert_eq!(w.effective_height_m(3.0), 3.0);
        w.height_m = Some(1.5);
        assert_eq!(w.effective_height_m(3.0), 1.5);
        w.height_m = Some(9.0);
        assert_eq!(w.effective_height_m(3.0), 3.0);
    }

    #[test]
    fn distance_is_to_the_segment_not_the_infinite_line() {
        let w = wall([0.0, 0.0], [10.0, 0.0]);
        assert_eq!(w.distance_to([5.0, 3.0]), 3.0);
        assert_eq!(w.distance_to([14.0, 3.0]), 5.0);
        assert_eq!(wall([2.0, 2.0], [2.0, 2.0]).distance_to([5.0, 6.0]), 5.0);
    }

    #[test]
    fn ends_snap_to_existing_wall_ends_within_the_radius() {
        let walls = [wall([100.0, 100.0], [200.0, 100.0])];
        assert_eq!(snap([104.0, 97.0], None, &walls, 8.0), [100.0, 100.0]);
        assert_eq!(snap([120.0, 97.0], None, &walls, 8.0), [120.0, 100.0], "mid-wall: onto the line, not an end");
        assert_eq!(snap([120.0, 80.0], None, &walls, 8.0), [120.0, 80.0], "too far from the wall");
    }

    #[test]
    fn points_snap_onto_the_line_of_a_nearby_wall() {
        let walls = [wall([0.0, 0.0], [200.0, 0.0])];
        assert_eq!(snap([80.0, 5.0], None, &walls, 8.0), [80.0, 0.0]);
        assert_eq!(snap([80.0, 12.0], None, &walls, 8.0), [80.0, 12.0], "too far from the line");
        assert_eq!(snap([3.0, 4.0], None, &walls, 8.0), [0.0, 0.0], "an end wins over the line");
        // A door drawn along the wall now lies on it and replaces that stretch.
        let (a, b) = (snap([60.0, 5.0], None, &walls, 8.0), snap([90.0, -6.0], Some(snap([60.0, 5.0], None, &walls, 8.0)), &walls, 8.0));
        let door = Wall::new(a, b, Material::Wood);
        assert_eq!(walls[0].carve(&door, 0.5).len(), 2);
    }

    #[test]
    fn directions_snap_to_15_degrees_only_when_close() {
        let start = Some([0.0, 0.0]);
        let s = snap([100.0, 3.0], start, &[], 8.0);
        assert!((s[1]).abs() < 1e-3 && (s[0] - 100.0).abs() < 0.2, "nearly horizontal becomes horizontal, got {s:?}");
        let off = [100.0, 12.0];
        assert_eq!(snap(off, start, &[], 8.0), off, "a deliberate angle is left alone");
        let diag = snap([100.0, 98.0], start, &[], 8.0);
        assert!((diag[0] - diag[1]).abs() < 0.5, "45 degrees is a multiple of 15, got {diag:?}");
    }

    #[test]
    fn a_door_drawn_along_a_wall_removes_that_stretch() {
        let w = Wall::new([0.0, 0.0], [100.0, 0.0], Material::Concrete);
        let door = Wall::new([40.0, 2.0], [60.0, -1.0], Material::Wood);
        let parts = w.carve(&door, 5.0);
        assert_eq!(parts.len(), 2);
        assert_eq!((parts[0].a, parts[0].b[0].round()), ([0.0, 0.0], 40.0));
        assert_eq!((parts[1].a[0].round(), parts[1].b), (60.0, [100.0, 0.0]));
        assert!(parts.iter().all(|p| p.material == Material::Concrete));
    }

    #[test]
    fn carving_handles_ends_overhang_and_unrelated_walls() {
        let w = Wall::new([0.0, 0.0], [100.0, 0.0], Material::Concrete);
        let at_end = w.carve(&Wall::new([70.0, 0.0], [100.0, 0.0], Material::Wood), 5.0);
        assert_eq!(at_end.len(), 1, "only the stretch before the door remains");
        assert!(w.carve(&Wall::new([-20.0, 0.0], [120.0, 0.0], Material::Glass), 5.0).is_empty(), "fully covered");
        let across = Wall::new([50.0, -20.0], [50.0, 20.0], Material::Wood);
        assert_eq!(w.carve(&across, 5.0), vec![w], "a crossing wall does not cut");
        let beside = Wall::new([20.0, 30.0], [60.0, 30.0], Material::Wood);
        assert_eq!(w.carve(&beside, 5.0), vec![w], "a parallel wall elsewhere does not cut");
        let past = Wall::new([120.0, 0.0], [150.0, 0.0], Material::Wood);
        assert_eq!(w.carve(&past, 5.0), vec![w], "no overlap");
    }

    #[test]
    fn boxes_select_walls_they_touch() {
        let w = Wall::new([0.0, 0.0], [100.0, 100.0], Material::Concrete);
        assert!(w.intersects_rect([40.0, 40.0], [60.0, 60.0]), "crossing the box");
        assert!(w.intersects_rect([-10.0, -10.0], [10.0, 10.0]), "an end inside");
        assert!(w.intersects_rect([-10.0, -10.0], [200.0, 200.0]), "fully inside");
        assert!(!w.intersects_rect([60.0, 0.0], [90.0, 30.0]), "beside the diagonal");
        let flat = Wall::new([0.0, 5.0], [100.0, 5.0], Material::Concrete);
        assert!(flat.intersects_rect([20.0, 0.0], [30.0, 10.0]));
        assert!(!flat.intersects_rect([20.0, 6.0], [30.0, 10.0]));
    }

    #[test]
    fn walls_round_trip_through_json() {
        let w = Wall { a: [1.0, 2.0], b: [3.0, 4.0], material: Material::ThickConcrete, height_m: Some(1.2) };
        let back: Wall = serde_json::from_str(&serde_json::to_string(&w).unwrap()).unwrap();
        assert_eq!(w, back);
    }
}

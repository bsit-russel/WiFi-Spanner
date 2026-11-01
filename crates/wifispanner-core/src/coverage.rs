// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! The area of the plan painted for the optimiser: where access points may stand and where
//! coverage counts. It is kept as the brush strokes, which stay small and can be undone, and
//! turned into a grid of cells when it is needed.

use serde::{Deserialize, Serialize};

/// The most cells along the longer side of a rasterised area.
const CELLS_ACROSS: f32 = 400.0;

/// One brush stroke, in plan pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    pub points: Vec<[f32; 2]>,
    /// Half the brush width.
    pub radius: f32,
    /// Rubs out what is painted instead of painting.
    #[serde(default)]
    pub erase: bool,
    /// The stroke came back to where it began, so the inside is covered too.
    #[serde(default)]
    pub closed: bool,
}

impl Stroke {
    /// Would a stroke ending at `end` close the loop that began at the first point? It does when
    /// the end is back within the brush's reach of the start after covering enough ground to
    /// have gone round something.
    pub fn closes(points: &[[f32; 2]], radius: f32) -> bool {
        let (Some(first), Some(last)) = (points.first(), points.last()) else { return false };
        let length: f32 = points.windows(2).map(|w| dist(w[0], w[1])).sum();
        points.len() > 3 && length > radius * 8.0 && dist(*first, *last) <= radius * 1.5
    }
}

/// The painted area as cells: true where something is painted.
#[derive(Debug, Clone)]
pub struct Mask {
    pub cell: f32,
    pub w: usize,
    pub h: usize,
    pub cells: Vec<bool>,
}

impl Mask {
    /// Paint `strokes` in order over a plan `width` by `height` pixels.
    pub fn from_strokes(strokes: &[Stroke], width: f32, height: f32) -> Mask {
        let cell = (width.max(height) / CELLS_ACROSS).max(1.0);
        let (w, h) = (((width / cell).ceil() as usize).max(1), ((height / cell).ceil() as usize).max(1));
        let mut mask = Mask { cell, w, h, cells: vec![false; w * h] };
        for s in strokes {
            mask.apply(s);
        }
        mask
    }

    fn apply(&mut self, s: &Stroke) {
        let value = !s.erase;
        let r = s.radius.max(0.5);
        let segments: Vec<([f32; 2], [f32; 2])> = match s.points.as_slice() {
            [] => return,
            [p] => vec![(*p, *p)],
            pts => pts.windows(2).map(|w| (w[0], w[1])).collect(),
        };
        for (a, b) in &segments {
            let (min, max) = ([a[0].min(b[0]) - r, a[1].min(b[1]) - r], [a[0].max(b[0]) + r, a[1].max(b[1]) + r]);
            self.each_cell(min, max, |m, i, centre| {
                if dist_to_segment(centre, *a, *b) <= r {
                    m.cells[i] = value;
                }
            });
        }
        if s.closed && s.points.len() > 2 {
            let (mut min, mut max) = ([f32::MAX; 2], [f32::MIN; 2]);
            for p in &s.points {
                (min, max) = ([min[0].min(p[0]), min[1].min(p[1])], [max[0].max(p[0]), max[1].max(p[1])]);
            }
            self.each_cell(min, max, |m, i, centre| {
                if inside_polygon(&s.points, centre) {
                    m.cells[i] = value;
                }
            });
        }
    }

    /// Run `f` for every cell whose centre may lie in the box from `min` to `max`.
    fn each_cell(&mut self, min: [f32; 2], max: [f32; 2], mut f: impl FnMut(&mut Mask, usize, [f32; 2])) {
        let lo = |v: f32, n: usize| ((v / self.cell).floor().max(0.0) as usize).min(n);
        let hi = |v: f32, n: usize| ((v / self.cell).ceil().max(0.0) as usize).min(n);
        let (x0, x1, y0, y1) = (lo(min[0], self.w), hi(max[0], self.w), lo(min[1], self.h), hi(max[1], self.h));
        for y in y0..y1 {
            for x in x0..x1 {
                let centre = [(x as f32 + 0.5) * self.cell, (y as f32 + 0.5) * self.cell];
                f(self, y * self.w + x, centre);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.cells.iter().any(|c| *c)
    }

    /// Is the plan point `p` painted?
    pub fn contains(&self, p: [f32; 2]) -> bool {
        if p[0] < 0.0 || p[1] < 0.0 {
            return false;
        }
        let (x, y) = ((p[0] / self.cell) as usize, (p[1] / self.cell) as usize);
        x < self.w && y < self.h && self.cells[y * self.w + x]
    }

    /// The box round the painted cells, as plan points.
    pub fn bounds(&self) -> Option<([f32; 2], [f32; 2])> {
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for (i, on) in self.cells.iter().enumerate() {
            if *on {
                let (x, y) = (i % self.w, i / self.w);
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
        (x0 != usize::MAX).then(|| ([x0 as f32 * self.cell, y0 as f32 * self.cell], [x1 as f32 * self.cell, y1 as f32 * self.cell]))
    }

    /// Does any part of the line from `a` to `b` pass through paint?
    pub fn touches_line(&self, a: [f32; 2], b: [f32; 2]) -> bool {
        let steps = ((dist(a, b) / self.cell).ceil() as usize).max(1);
        (0..=steps).any(|i| {
            let t = i as f32 / steps as f32;
            self.contains([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t])
        })
    }
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

fn dist_to_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 <= f32::EPSILON { 0.0 } else { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0) };
    dist(p, [a[0] + t * dx, a[1] + t * dy])
}

/// Even-odd test: is `p` inside the closed outline `pts`?
fn inside_polygon(pts: &[[f32; 2]], p: [f32; 2]) -> bool {
    let mut inside = false;
    let mut j = pts.len() - 1;
    for i in 0..pts.len() {
        let (a, b) = (pts[i], pts[j]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(points: &[[f32; 2]], radius: f32, closed: bool) -> Stroke {
        Stroke { points: points.to_vec(), radius, erase: false, closed }
    }

    #[test]
    fn a_stroke_paints_along_its_path_only() {
        let m = Mask::from_strokes(&[stroke(&[[10.0, 50.0], [90.0, 50.0]], 5.0, false)], 100.0, 100.0);
        assert!(m.contains([50.0, 52.0]) && !m.contains([50.0, 70.0]));
    }

    #[test]
    fn a_closed_loop_fills_inside_and_an_open_one_does_not() {
        let square = [[10.0, 10.0], [90.0, 10.0], [90.0, 90.0], [10.0, 90.0], [10.0, 10.0]];
        let open = Mask::from_strokes(&[stroke(&square, 3.0, false)], 100.0, 100.0);
        let closed = Mask::from_strokes(&[stroke(&square, 3.0, true)], 100.0, 100.0);
        assert!(!open.contains([50.0, 50.0]));
        assert!(closed.contains([50.0, 50.0]) && !closed.contains([3.0, 50.0]));
    }

    #[test]
    fn erasing_rubs_out_paint_and_bounds_follow() {
        let mut rub = stroke(&[[40.0, 50.0], [60.0, 50.0]], 8.0, false);
        rub.erase = true;
        let m = Mask::from_strokes(&[stroke(&[[10.0, 50.0], [90.0, 50.0]], 5.0, false), rub], 100.0, 100.0);
        assert!(!m.contains([50.0, 50.0]) && m.contains([20.0, 50.0]));
        let (lo, hi) = m.bounds().unwrap();
        assert!(lo[0] <= 10.0 && hi[0] >= 90.0 && hi[1] < 70.0);
        assert!(Mask::from_strokes(&[], 100.0, 100.0).is_empty());
    }

    #[test]
    fn a_stroke_closes_only_when_it_comes_back_after_going_round() {
        let ring: Vec<[f32; 2]> = (0..=24).map(|i| [50.0 + 30.0 * (i as f32 * 0.2618).cos(), 50.0 + 30.0 * (i as f32 * 0.2618).sin()]).collect();
        assert!(Stroke::closes(&ring, 4.0));
        assert!(!Stroke::closes(&ring[..12], 4.0));
        assert!(!Stroke::closes(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [0.0, 0.0]], 4.0));
    }

    #[test]
    fn a_wall_is_touched_when_paint_bleeds_over_it() {
        let m = Mask::from_strokes(&[stroke(&[[10.0, 50.0], [90.0, 50.0]], 6.0, false)], 100.0, 100.0);
        assert!(m.touches_line([50.0, 54.0], [50.0, 90.0]));
        assert!(!m.touches_line([50.0, 62.0], [50.0, 90.0]));
    }
}

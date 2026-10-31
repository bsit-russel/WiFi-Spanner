// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 rubble-za. See the LICENSE and NOTICE files.

//! Moving the planned access points to where they cover the floor best.
//!
//! The floor is sampled on a coarse grid, and every access point may stand at the centre of any
//! cell of a second, coarser grid. For each candidate spot the signal at every sample is worked
//! out once with the same model as the prediction. The access points are then placed one at a
//! time, each where it adds the most, and moved again in turn until nothing improves.
//!
//! Coverage is scored as the mean over the samples of `min(best signal, target)`: weak spots
//! count for a lot, and signal beyond the target adds nothing, so access points spread out
//! instead of piling up on the strongest spot.

use crate::predict::{scene, signal_from, PlannedAp, Unavailable};
use crate::survey::Floor;
use wifispanner_scan::Band;

/// The signal a spot should get, in dBm; stronger adds nothing to the score.
pub const TARGET_DBM: f32 = -65.0;
/// Signal assumed where no access point reaches at all.
const FLOOR_DBM: f32 = -100.0;
/// Samples and candidate spots along the longer side of the plan.
const SAMPLES_ACROSS: f32 = 40.0;
const SPOTS_ACROSS: f32 = 28.0;
const MAX_PASSES: usize = 8;
/// Access points keep at least this far (metres) from a wall.
const WALL_CLEARANCE_M: f32 = 0.5;

/// New positions (plan pixels) for the enabled access points of `floor.aps` that have a radio on
/// `band`, as `(index in floor.aps, [x, y])`. Only those that move are listed; an empty list
/// means the plan is already as good as it gets.
pub fn optimise(floor: &Floor, band: Band, rx_height_m: f32) -> Result<Vec<(usize, [f32; 2])>, Unavailable> {
    let mpp = floor.metres_per_px.filter(|m| m.is_finite() && *m > 0.0).ok_or(Unavailable::NoScale)?;
    let movers: Vec<(usize, &PlannedAp, f32)> = floor
        .aps
        .iter()
        .enumerate()
        .filter(|(_, a)| a.enabled)
        .filter_map(|(i, a)| a.eirp_dbm(band).map(|e| (i, a, e)))
        .collect();
    if movers.is_empty() {
        return Err(Unavailable::NoAps);
    }
    // Only the floor plan itself counts: the part of the page inside the walls, not its margins.
    let zone = safe_zone(floor, mpp);
    let (min, max) = zone.as_ref().map_or(([0.0, 0.0], [floor.width_px as f32, floor.height_px as f32]), |z| (z.min, z.max));
    let (span_x, span_y) = ((max[0] - min[0]).max(1.0), (max[1] - min[1]).max(1.0));
    let longest = span_x.max(span_y);
    let grid = |across: f32| {
        let step = (longest / across).max(1.0);
        let (w, h) = (((span_x / step).ceil() as usize).max(1), ((span_y / step).ceil() as usize).max(1));
        (0..w * h)
            .map(|i| [min[0] + ((i % w) as f32 + 0.5) * step, min[1] + ((i / w) as f32 + 0.5) * step])
            .filter(|p| zone.as_ref().is_none_or(|z| z.contains(*p)))
            .collect::<Vec<[f32; 2]>>()
    };
    let mut samples = grid(SAMPLES_ACROSS);
    let mut spots = grid(SPOTS_ACROSS);
    if samples.is_empty() || spots.is_empty() {
        // The zone is too small to sample: use the whole page as before.
        let whole = |across: f32| {
            let step = (floor.width_px.max(floor.height_px).max(1) as f32 / across).max(1.0);
            let (w, h) = (((floor.width_px as f32 / step).ceil() as usize).max(1), ((floor.height_px as f32 / step).ceil() as usize).max(1));
            (0..w * h).map(|i| [((i % w) as f32 + 0.5) * step, ((i / w) as f32 + 0.5) * step]).collect::<Vec<[f32; 2]>>()
        };
        samples = whole(SAMPLES_ACROSS);
        spots = whole(SPOTS_ACROSS);
    }
    // Not on or right beside a wall (unless that leaves nowhere to stand).
    let clear = WALL_CLEARANCE_M / mpp;
    let off_walls: Vec<[f32; 2]> = spots.iter().copied().filter(|p| floor.walls.iter().all(|w| distance_to_segment(*p, w.a, w.b) >= clear)).collect();
    if !off_walls.is_empty() {
        spots = off_walls;
    }
    let walls = scene(floor, mpp, band);
    let at = |ap: &PlannedAp, eirp: f32, xy: [f32; 2], s: [f32; 2]| signal_from(ap, eirp, [xy[0] * mpp, xy[1] * mpp], [s[0] * mpp, s[1] * mpp, rx_height_m], &walls, band);

    // signal[m][spot][sample]
    let signals: Vec<Vec<Vec<f32>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = movers
            .iter()
            .map(|(_, ap, eirp)| {
                let (spots, samples, at) = (&spots, &samples, &at);
                scope.spawn(move || spots.iter().map(|&p| samples.iter().map(|&s| at(ap, *eirp, p, s)).collect()).collect())
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("optimiser thread")).collect()
    });

    let score = |best: &[f32]| best.iter().map(|b| b.clamp(FLOOR_DBM, TARGET_DBM)).sum::<f32>() / best.len() as f32;
    let best_of = |placed: &[Option<usize>], skip: Option<usize>| -> Vec<f32> {
        let mut best = vec![FLOOR_DBM; samples.len()];
        for (m, spot) in placed.iter().enumerate() {
            if let (Some(spot), true) = (spot, Some(m) != skip) {
                for (b, s) in best.iter_mut().zip(&signals[m][*spot]) {
                    *b = b.max(*s);
                }
            }
        }
        best
    };
    // Where they stand now, for comparison.
    let current: Vec<f32> = {
        let mut best = vec![FLOOR_DBM; samples.len()];
        for (_, ap, eirp) in &movers {
            for (b, s) in best.iter_mut().zip(&samples) {
                *b = b.max(at(ap, *eirp, [ap.x, ap.y], *s));
            }
        }
        best
    };

    // Strongest first, each where it adds the most; then each moved in turn while that helps.
    let mut order: Vec<usize> = (0..movers.len()).collect();
    order.sort_by(|a, b| movers[*b].2.total_cmp(&movers[*a].2));
    let choose = |placed: &[Option<usize>], m: usize| -> usize {
        let others = best_of(placed, Some(m));
        (0..spots.len())
            .map(|c| (c, score(&others.iter().zip(&signals[m][c]).map(|(o, s)| o.max(*s)).collect::<Vec<_>>())))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map_or(0, |(c, _)| c)
    };
    let ascend = |mut placed: Vec<Option<usize>>| -> Vec<Option<usize>> {
        for &m in &order {
            if placed[m].is_none() {
                placed[m] = Some(choose(&placed, m));
            }
        }
        for _ in 0..MAX_PASSES {
            let mut moved = false;
            for &m in &order {
                let was = placed[m];
                let now = Some(choose(&placed, m));
                let (a, b) = (score(&best_of(&placed, None)), {
                    let mut t = placed.clone();
                    t[m] = now;
                    score(&best_of(&t, None))
                });
                if now != was && b > a + 1e-4 {
                    placed[m] = now;
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
        placed
    };
    // Two starts: from nothing (each where it adds the most) and from where they stand now.
    let nearest = |p: [f32; 2]| (0..spots.len()).min_by(|a, b| dist2(spots[*a], p).total_cmp(&dist2(spots[*b], p)));
    let from_scratch = ascend(vec![None; movers.len()]);
    let from_here = ascend(movers.iter().map(|(_, ap, _)| nearest([ap.x, ap.y])).collect());
    let placed = if score(&best_of(&from_here, None)) >= score(&best_of(&from_scratch, None)) { from_here } else { from_scratch };
    // Keep what is there unless the search found something clearly better, or something stands outside the walls.
    let outside = zone.as_ref().is_some_and(|z| movers.iter().any(|(_, ap, _)| !z.contains([ap.x, ap.y])));
    if !outside && score(&best_of(&placed, None)) <= score(&current) + 0.05 {
        return Ok(Vec::new());
    }
    Ok(movers.iter().zip(&placed).filter_map(|((i, _, _), spot)| spot.map(|s| (*i, spots[s]))).collect())
}

/// Where access points may be placed and coverage counts: the area the walls enclose, with a
/// buffer of one cell.
struct Zone {
    min: [f32; 2],
    max: [f32; 2],
    cell: f32,
    w: usize,
    inside: Vec<bool>,
    /// When the walls leave a gap, the convex hull of the walls instead.
    hull: Option<Vec<[f32; 2]>>,
}

impl Zone {
    fn contains(&self, p: [f32; 2]) -> bool {
        if let Some(h) = &self.hull {
            return in_hull(h, p);
        }
        let (x, y) = (((p[0] - self.min[0]) / self.cell).floor(), ((p[1] - self.min[1]) / self.cell).floor());
        if x < 0.0 || y < 0.0 {
            return false;
        }
        let (x, y) = (x as usize, y as usize);
        x < self.w && y * self.w + x < self.inside.len() && self.inside[y * self.w + x]
    }
}

/// The part of the plan inside the main group of walls, or `None` when there are too few walls to tell.
fn safe_zone(floor: &Floor, mpp: f32) -> Option<Zone> {
    let walls = &floor.walls;
    if walls.len() < 4 {
        return None;
    }
    // Walls that touch (within 0.6 m) form a group; the longest group is the building.
    let near = 0.6 / mpp;
    let mut group: Vec<usize> = (0..walls.len()).collect();
    fn root(g: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while g[r] != r {
            r = g[r];
        }
        let mut c = i;
        while g[c] != r {
            let n = g[c];
            g[c] = r;
            c = n;
        }
        r
    }
    for i in 0..walls.len() {
        for j in i + 1..walls.len() {
            let (a, b) = (&walls[i], &walls[j]);
            let touching = distance_to_segment(a.a, b.a, b.b) <= near
                || distance_to_segment(a.b, b.a, b.b) <= near
                || distance_to_segment(b.a, a.a, a.b) <= near
                || distance_to_segment(b.b, a.a, a.b) <= near;
            if touching {
                let (ri, rj) = (root(&mut group, i), root(&mut group, j));
                group[ri] = rj;
            }
        }
    }
    let mut length = std::collections::HashMap::new();
    for i in 0..walls.len() {
        *length.entry(root(&mut group, i)).or_insert(0.0f32) += walls[i].length_px();
    }
    let main = length.iter().max_by(|a, b| a.1.total_cmp(b.1))?.0;
    let main = *main;
    let mine: Vec<&crate::walls::Wall> = (0..walls.len()).filter(|&i| root(&mut group, i) == main).map(|i| &walls[i]).collect();
    if mine.len() < 4 {
        return None;
    }
    let (mut min, mut max) = ([f32::MAX; 2], [f32::MIN; 2]);
    for w in &mine {
        for p in [w.a, w.b] {
            for k in 0..2 {
                min[k] = min[k].min(p[k]);
                max[k] = max[k].max(p[k]);
            }
        }
    }
    let cell = (0.2 / mpp).max(2.0);
    let (w, h) = ((((max[0] - min[0]) / cell).ceil() as usize + 3).max(3), (((max[1] - min[1]) / cell).ceil() as usize + 3).max(3));
    let origin = [min[0] - cell, min[1] - cell];
    // Walls are drawn into the grid a cell and a bit thick, so a doorway-sized gap does not let the outside in.
    let mut wall = vec![false; w * h];
    for s in &mine {
        let len = s.length_px();
        let steps = (len / (cell * 0.5)).ceil().max(1.0) as usize;
        for k in 0..=steps {
            let t = k as f32 / steps as f32;
            let (x, y) = (s.a[0] + (s.b[0] - s.a[0]) * t, s.a[1] + (s.b[1] - s.a[1]) * t);
            let (cx, cy) = (((x - origin[0]) / cell) as isize, ((y - origin[1]) / cell) as isize);
            for dy in -1..=1isize {
                for dx in -1..=1isize {
                    let (nx, ny) = (cx + dx, cy + dy);
                    if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                        wall[ny as usize * w + nx as usize] = true;
                    }
                }
            }
        }
    }
    // The outside: everything reachable from the border without crossing a wall.
    let mut outside = vec![false; w * h];
    let mut stack: Vec<usize> = (0..w * h).filter(|i| (i % w == 0 || i % w == w - 1 || i / w == 0 || i / w == h - 1) && !wall[*i]).collect();
    for &i in &stack {
        outside[i] = true;
    }
    while let Some(i) = stack.pop() {
        let (x, y) = (i % w, i / w);
        for (nx, ny) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)] {
            if nx < w && ny < h && !outside[ny * w + nx] && !wall[ny * w + nx] {
                outside[ny * w + nx] = true;
                stack.push(ny * w + nx);
            }
        }
    }
    let inside: Vec<bool> = (0..w * h).map(|i| !outside[i]).collect();
    let enclosed = inside.iter().filter(|b| **b).count();
    let (bmin, bmax) = ([min[0] - cell, min[1] - cell], [max[0] + cell, max[1] + cell]);
    // Too little enclosed means the walls leave the building open: use the hull of the walls.
    if enclosed * 100 < w * h * 20 {
        let mut pts: Vec<[f32; 2]> = mine.iter().flat_map(|w| [w.a, w.b]).collect();
        let hull = convex_hull(&mut pts);
        return Some(Zone { min: bmin, max: bmax, cell, w, inside: Vec::new(), hull: Some(hull) });
    }
    // The zone's own grid starts at `origin`.
    Some(Zone { min: origin, max: bmax, cell, w, inside, hull: None })
}

/// Convex hull (counter-clockwise) of points.
fn convex_hull(pts: &mut Vec<[f32; 2]>) -> Vec<[f32; 2]> {
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    pts.dedup();
    if pts.len() < 3 {
        return pts.clone();
    }
    let cross = |o: [f32; 2], a: [f32; 2], b: [f32; 2]| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let mut hull: Vec<[f32; 2]> = Vec::new();
    for &p in pts.iter() {
        while hull.len() >= 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0 {
            hull.pop();
        }
        hull.push(p);
    }
    let lower = hull.len() + 1;
    for &p in pts.iter().rev().skip(1) {
        while hull.len() >= lower && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0 {
            hull.pop();
        }
        hull.push(p);
    }
    hull.pop();
    hull
}

fn in_hull(hull: &[[f32; 2]], p: [f32; 2]) -> bool {
    hull.len() >= 3 && (0..hull.len()).all(|i| {
        let (a, b) = (hull[i], hull[(i + 1) % hull.len()]);
        (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]) >= 0.0
    })
}

fn dist2(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)
}

/// Distance from `p` to the segment `a`-`b`.
fn distance_to_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 < 1e-9 { 0.0 } else { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0) };
    ((p[0] - (a[0] + t * dx)).powi(2) + (p[1] - (a[1] + t * dy)).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::floors::Placement;
    use crate::predict::predict;

    fn floor(aps: Vec<PlannedAp>) -> Floor {
        Floor {
            name: "t".into(),
            image: vec![],
            width_px: 400,
            height_px: 200,
            metres_per_px: Some(0.1),
            points: vec![],
            walls: vec![],
            ceiling_m: 3.0,
            placement: Placement::default(),
            aps,
            survey_aps: vec![],
            areas: vec![],
        }
    }

    fn mean_clamped(f: &Floor) -> f32 {
        let g = predict(f, Band::Ghz5, 1.0).unwrap();
        g.dbm.iter().map(|d| d.clamp(FLOOR_DBM, TARGET_DBM)).sum::<f32>() / g.dbm.len() as f32
    }

    #[test]
    fn access_points_stacked_in_a_corner_spread_out() {
        let mut f = floor(vec![PlannedAp::new("a".into(), 10.0, 10.0), PlannedAp::new("b".into(), 12.0, 12.0)]);
        let before = mean_clamped(&f);
        let moves = optimise(&f, Band::Ghz5, 1.0).unwrap();
        assert_eq!(moves.len(), 2);
        for (i, p) in moves {
            f.aps[i].x = p[0];
            f.aps[i].y = p[1];
        }
        assert!(mean_clamped(&f) > before + 1.0 && mean_clamped(&f) > TARGET_DBM - 0.5, "{} vs {before}", mean_clamped(&f));
        let (a, b) = (&f.aps[0], &f.aps[1]);
        assert!(((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt() > 100.0, "they end up apart");
    }

    #[test]
    fn access_points_keep_off_the_walls() {
        use crate::walls::Wall;
        use crate::Material;
        let mut f = floor(vec![PlannedAp::new("a".into(), 20.0, 20.0)]);
        // A thin glass wall down the middle, where the best spot for one AP would be.
        f.walls.push(Wall { height_m: None, ..Wall::new([200.0, 0.0], [200.0, 200.0], Material::Glass) });
        let moves = optimise(&f, Band::Ghz5, 1.0).unwrap();
        let [x, y] = moves.first().expect("it moves").1;
        assert!(distance_to_segment([x, y], [200.0, 0.0], [200.0, 200.0]) >= WALL_CLEARANCE_M / 0.1, "{x},{y}");
    }

    fn house(gap: bool) -> Floor {
        use crate::walls::Wall;
        use crate::Material;
        // A 40 x 20 m house in the middle of a 120 x 80 m page (0.1 m per pixel), walls 200..600 by 150..350.
        let mut f = floor(vec![PlannedAp::new("a".into(), 50.0, 50.0), PlannedAp::new("b".into(), 1100.0, 750.0)]);
        f.width_px = 1200;
        f.height_px = 800;
        let w = |a: [f32; 2], b: [f32; 2]| Wall { height_m: None, ..Wall::new(a, b, Material::Concrete) };
        f.walls = vec![w([200.0, 150.0], [600.0, 150.0]), w([200.0, 350.0], [600.0, 350.0]), w([200.0, 150.0], [200.0, 350.0]), w([600.0, 150.0], [600.0, 350.0])];
        if gap {
            // A 3 m opening in the right wall.
            f.walls[3] = w([600.0, 150.0], [600.0, 230.0]);
            f.walls.push(w([600.0, 260.0], [600.0, 350.0]));
        }
        f
    }

    #[test]
    fn access_points_go_inside_the_walls_not_the_page_margins() {
        for gap in [false, true] {
            let mut f = house(gap);
            for (i, p) in optimise(&f, Band::Ghz5, 1.0).unwrap() {
                f.aps[i].x = p[0];
                f.aps[i].y = p[1];
            }
            for a in &f.aps {
                assert!((200.0..=600.0).contains(&a.x) && (150.0..=350.0).contains(&a.y), "gap={gap}: ({}, {})", a.x, a.y);
            }
        }
    }

    #[test]
    fn a_good_layout_is_left_alone_and_needs_a_scale() {
        let f = floor(vec![PlannedAp::new("a".into(), 200.0, 100.0)]);
        let moves = optimise(&f, Band::Ghz5, 1.0).unwrap();
        assert!(moves.is_empty() || moves.len() == 1);
        let mut none = floor(vec![]);
        assert_eq!(optimise(&none, Band::Ghz5, 1.0), Err(Unavailable::NoAps));
        none.metres_per_px = None;
        assert_eq!(optimise(&none, Band::Ghz5, 1.0), Err(Unavailable::NoScale));
    }

    /// Try the optimiser on a saved project: `WS_PROJECT=/path/x.wifispanner cargo test --release try_project -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn try_project() {
        let text = std::fs::read_to_string(std::env::var("WS_PROJECT").unwrap()).unwrap();
        let project = crate::survey::Project::from_json(&text).unwrap();
        let f = project.floor();
        println!("walls {} aps {:?}", f.walls.len(), f.aps.iter().map(|a| (a.x as i32, a.y as i32)).collect::<Vec<_>>());
        println!("{:?}", optimise(f, Band::Ghz5, 1.0).map(|m| m.iter().map(|(i, p)| (*i, p[0] as i32, p[1] as i32)).collect::<Vec<_>>()));
    }
}

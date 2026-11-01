// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

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

use crate::coverage::Mask;
use crate::predict::{scene, signal_from, PlannedAp, Unavailable};
use crate::survey::Floor;
use wifispanner_scan::Band;

/// The signal a spot should get, in dBm; stronger adds nothing to the score.
pub const TARGET_DBM: f32 = -68.0;
/// Signal assumed where no access point reaches at all.
const FLOOR_DBM: f32 = -100.0;
/// Samples and candidate spots along the longer side of the plan.
const SAMPLES_ACROSS: f32 = 40.0;
const SPOTS_ACROSS: f32 = 28.0;
const MAX_PASSES: usize = 8;
/// Access points keep at least this far (metres) from a wall.
const WALL_CLEARANCE_M: f32 = 0.5;
/// Gaps in the walls up to twice this (metres) are treated as closed when finding the building's outline.
const SEAL_M: f32 = 3.0;
/// Coverage also counts this far (metres) outside the walls.
const BUFFER_M: f32 = 1.2;

/// A signal this strong (dBm) or better counts as good coverage.
pub const GOOD_DBM: f32 = -70.0;
/// Enough coverage that no more access points are suggested: the share of the floor with a good signal.
const ENOUGH_SHARE: f32 = 0.9;
/// Never suggest more than this many extra access points.
const MAX_EXTRA: usize = 4;
/// Roaming: a client should hear a second access point at least this well (dBm) where it leaves the first.
pub const ROAM_DBM: f32 = -78.0;
/// How much the second access point's signal counts beside the first's in the score.
const ROAM_WEIGHT: f32 = 0.03;
/// An extra access point must add at least this share of the floor to be worth suggesting.
const WORTH_SHARE: f32 = 0.02;

/// What the optimiser is to achieve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Goal {
    /// The signal (dBm) that counts as good coverage; wider channels need a stronger one.
    pub good_dbm: f32,
    /// False to only judge the access points where they stand (coverage, roaming, more access
    /// points needed) without moving any.
    pub move_aps: bool,
}

impl Default for Goal {
    fn default() -> Self {
        Goal { good_dbm: GOOD_DBM, move_aps: true }
    }
}

/// What the optimiser found.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// New positions for access points that should move, as `(index in floor.aps, [x, y])`.
    pub moves: Vec<(usize, [f32; 2])>,
    /// The share of the floor (0 to 1) with a good signal before and after those moves.
    pub coverage_before: f32,
    pub coverage_after: f32,
    /// Where more access points would help, with the coverage they would reach, when the floor is not covered well enough.
    pub extra: Vec<[f32; 2]>,
    pub coverage_with_extra: f32,
    /// The share of the floor where a second access point is heard at `ROAM_DBM` or better, after the moves: room to roam.
    pub roaming_after: f32,
    /// The access point (index in `floor.aps`) the extra ones should be copies of: the strongest.
    pub template: usize,
}

/// New positions (plan pixels) for the enabled access points of `floor.aps` that have a radio on
/// `band`, as `(index in floor.aps, [x, y])`. Only those that move are listed; an empty list
/// means the plan is already as good as it gets.
pub fn optimise(floor: &Floor, band: Band, rx_height_m: f32, goal: &Goal) -> Result<Outcome, Unavailable> {
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
    let painted = painted_mask(floor);
    let zone = painted.as_ref().and_then(painted_zone).or_else(|| if painted.is_some() { None } else { safe_zone(floor, mpp) });
    let (min, max) = zone.as_ref().map_or(([0.0, 0.0], [floor.width_px as f32, floor.height_px as f32]), |z| (z.min, z.max));
    let (span_x, span_y) = ((max[0] - min[0]).max(1.0), (max[1] - min[1]).max(1.0));
    let longest = span_x.max(span_y);
    let grid = |across: f32| {
        let step = (longest / across).max(1.0);
        let (w, h) = (((span_x / step).ceil() as usize).max(1), ((span_y / step).ceil() as usize).max(1));
        (0..w * h)
            .map(|i| [min[0] + ((i % w) as f32 + 0.5) * step, min[1] + ((i / w) as f32 + 0.5) * step])
            .collect::<Vec<[f32; 2]>>()
    };
    let mut samples: Vec<[f32; 2]> = grid(SAMPLES_ACROSS).into_iter().filter(|p| zone.as_ref().is_none_or(|z| z.contains(*p))).collect();
    let mut spots: Vec<[f32; 2]> = grid(SPOTS_ACROSS).into_iter().filter(|p| zone.as_ref().is_none_or(|z| z.can_place(*p))).collect();
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
    // Only between walls, even where a room is not closed: never outside the premises.
    let inside: Vec<[f32; 2]> = spots.iter().copied().filter(|p| crate::coverage::may_place(&floor.walls, *p)).collect();
    if !inside.is_empty() {
        spots = inside;
    }
    // Not on or right beside a wall (unless that leaves nowhere to stand).
    let clear = WALL_CLEARANCE_M / mpp;
    let off_walls: Vec<[f32; 2]> = spots.iter().copied().filter(|p| floor.walls.iter().all(|w| distance_to_segment(*p, w.a, w.b) >= clear)).collect();
    if !off_walls.is_empty() {
        spots = off_walls;
    }
    let mut walls = scene(floor, mpp, band);
    // With an area painted, only the walls it reaches count: the paint bleeds a little over them.
    if let Some(mask) = &painted {
        walls.retain_walls(|a, b| mask.touches_line([a[0] / mpp, a[1] / mpp], [b[0] / mpp, b[1] / mpp]));
    }
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
    // The strongest and second-strongest signal at every sample for some placement.
    let top2_of = |placed: &[Option<usize>], skip: Option<usize>| -> (Vec<f32>, Vec<f32>) {
        let (mut best, mut second) = (vec![FLOOR_DBM; samples.len()], vec![FLOOR_DBM; samples.len()]);
        for (m, spot) in placed.iter().enumerate() {
            if let (Some(spot), true) = (spot, Some(m) != skip) {
                for ((b, s2), s) in best.iter_mut().zip(second.iter_mut()).zip(&signals[m][*spot]) {
                    if *s > *b {
                        (*s2, *b) = (*b, *s);
                    } else if *s > *s2 {
                        *s2 = *s;
                    }
                }
            }
        }
        (best, second)
    };
    let roam_score = |second: &[f32]| second.iter().map(|b| b.clamp(FLOOR_DBM, ROAM_DBM)).sum::<f32>() / second.len() as f32;
    let total = |best: &[f32], second: &[f32]| score(best) + ROAM_WEIGHT * roam_score(second);
    let total_of = |placed: &[Option<usize>]| {
        let (b, s) = top2_of(placed, None);
        total(&b, &s)
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

    let current_second: Vec<f32> = {
        let mut best = vec![FLOOR_DBM; samples.len()];
        let mut second = vec![FLOOR_DBM; samples.len()];
        for (_, ap, eirp) in &movers {
            for ((b, s2), p) in best.iter_mut().zip(second.iter_mut()).zip(&samples) {
                let s = at(ap, *eirp, [ap.x, ap.y], *p);
                if s > *b {
                    (*s2, *b) = (*b, s);
                } else if s > *s2 {
                    *s2 = s;
                }
            }
        }
        second
    };
    // Strongest first, each where it adds the most; then each moved in turn while that helps.
    let mut order: Vec<usize> = (0..movers.len()).collect();
    order.sort_by(|a, b| movers[*b].2.total_cmp(&movers[*a].2));
    let choose = |placed: &[Option<usize>], m: usize| -> usize {
        let (b1, b2) = top2_of(placed, Some(m));
        (0..spots.len())
            .map(|c| {
                let (mut first, mut second) = (b1.clone(), b2.clone());
                for ((f, s2), s) in first.iter_mut().zip(second.iter_mut()).zip(&signals[m][c]) {
                    if *s > *f {
                        (*s2, *f) = (*f, *s);
                    } else if *s > *s2 {
                        *s2 = *s;
                    }
                }
                (c, total(&first, &second))
            })
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
                let (a, b) = (total_of(&placed), {
                    let mut t = placed.clone();
                    t[m] = now;
                    total_of(&t)
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
    let (from_scratch, from_here) = if goal.move_aps {
        (ascend(vec![None; movers.len()]), ascend(movers.iter().map(|(_, ap, _)| nearest([ap.x, ap.y])).collect()))
    } else {
        (vec![None; movers.len()], vec![None; movers.len()])
    };
    let placed = if total_of(&from_here) >= total_of(&from_scratch) { from_here } else { from_scratch };
    // Keep what is there unless the search found something clearly better, or something stands outside the walls.
    let outside = zone.as_ref().is_some_and(|z| movers.iter().any(|(_, ap, _)| !z.can_place([ap.x, ap.y])));
    // Coverage is judged inside the walls; the margin around the building only steers the search.
    let core: Vec<bool> = samples.iter().map(|p| zone.as_ref().is_none_or(|z| z.can_place(*p))).collect();
    let core_count = core.iter().filter(|c| **c).count().max(1);
    let share_at = |v: &[f32], limit: f32| v.iter().zip(&core).filter(|(b, c)| **c && **b >= limit).count() as f32 / core_count as f32;
    let share = |best: &[f32]| share_at(best, goal.good_dbm);
    let keep = !goal.move_aps || (!outside && total_of(&placed) <= total(&current, &current_second) + 0.05);
    let (moves, after, after_second): (Vec<(usize, [f32; 2])>, Vec<f32>, Vec<f32>) = if keep {
        (Vec::new(), current.clone(), current_second.clone())
    } else {
        let (b, s) = top2_of(&placed, None);
        (movers.iter().zip(&placed).filter_map(|((i, _, _), spot)| spot.map(|s| (*i, spots[s]))).collect(), b, s)
    };
    // If the floor is still not well covered, where would more access points like the strongest one go?
    let template = order[0];
    let mut best = after.clone();
    let (coverage_after, mut with_extra) = (share(&after), share(&after));
    let mut extra = Vec::new();
    while coverage_after < ENOUGH_SHARE && with_extra < ENOUGH_SHARE && extra.len() < MAX_EXTRA {
        let Some(c) = (0..spots.len()).max_by(|a, b| {
            let s = |c: usize| score(&best.iter().zip(&signals[template][c]).map(|(o, s)| o.max(*s)).collect::<Vec<_>>());
            s(*a).total_cmp(&s(*b))
        }) else {
            break;
        };
        let next: Vec<f32> = best.iter().zip(&signals[template][c]).map(|(o, s)| o.max(*s)).collect();
        if share(&next) < with_extra + WORTH_SHARE {
            break;
        }
        extra.push(spots[c]);
        with_extra = share(&next);
        best = next;
    }
    Ok(Outcome { moves, coverage_before: share(&current), coverage_after, extra, coverage_with_extra: with_extra, roaming_after: share_at(&after_second, ROAM_DBM), template: movers[template].0 })
}

/// Where access points may be placed and coverage counts: the area the walls enclose, with a
/// buffer of one cell.
struct Zone {
    min: [f32; 2],
    max: [f32; 2],
    cell: f32,
    w: usize,
    /// Where coverage counts: the area inside all the walls with a margin around them.
    inside: Vec<bool>,
    /// Where access points may stand: the same without the margin.
    placing: Vec<bool>,
    /// When the walls leave a gap, the convex hull of the walls instead.
    hull: Option<Vec<[f32; 2]>>,
}

impl Zone {
    /// Is `p` somewhere coverage counts?
    fn contains(&self, p: [f32; 2]) -> bool {
        self.lookup(&self.inside, p)
    }

    /// May an access point stand at `p`?
    fn can_place(&self, p: [f32; 2]) -> bool {
        self.lookup(&self.placing, p)
    }

    fn lookup(&self, grid: &[bool], p: [f32; 2]) -> bool {
        if let Some(h) = &self.hull {
            return in_hull(h, p);
        }
        let (x, y) = (((p[0] - self.min[0]) / self.cell).floor(), ((p[1] - self.min[1]) / self.cell).floor());
        if x < 0.0 || y < 0.0 {
            return false;
        }
        let (x, y) = (x as usize, y as usize);
        x < self.w && y * self.w + x < grid.len() && grid[y * self.w + x]
    }
}

/// The area painted on the plan, if there is any.
fn painted_mask(floor: &Floor) -> Option<Mask> {
    if floor.zone.is_empty() {
        return None;
    }
    Some(Mask::from_strokes(&floor.zone, floor.width_px as f32, floor.height_px as f32)).filter(|m| !m.is_empty())
}

/// The painted area as the zone the optimiser works in: the same cells for coverage and standing.
fn painted_zone(mask: &Mask) -> Option<Zone> {
    let (min, max) = mask.bounds()?;
    let (x0, y0) = ((min[0] / mask.cell).round() as usize, (min[1] / mask.cell).round() as usize);
    let (w, h) = (((max[0] - min[0]) / mask.cell).round() as usize, ((max[1] - min[1]) / mask.cell).round() as usize);
    let cells: Vec<bool> = (0..w * h).map(|i| mask.cells[(y0 + i / w) * mask.w + x0 + i % w]).collect();
    Some(Zone { min, max, cell: mask.cell, w, inside: cells.clone(), placing: cells, hull: None })
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
    // The building is its longest group of walls, and any other group of real size (a garage or an
    // annexe standing a little apart); small groups elsewhere on the page are not the building.
    let longest = length.values().copied().fold(0.0f32, f32::max);
    let mut keep: Vec<usize> = length.iter().filter(|(_, l)| **l >= longest * 0.15).map(|(r, _)| *r).collect();
    // Smaller groups inside the building's outline (a kitchen block, a few walls of an open-plan area) belong to it.
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for i in 0..walls.len() {
        if keep.contains(&root(&mut group, i)) {
            for p in [walls[i].a, walls[i].b] {
                for k in 0..2 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
        }
    }
    let slack = 1.0 / mpp;
    for i in 0..walls.len() {
        let c = [(walls[i].a[0] + walls[i].b[0]) / 2.0, (walls[i].a[1] + walls[i].b[1]) / 2.0];
        let r = root(&mut group, i);
        if !keep.contains(&r) && (0..2).all(|k| c[k] >= lo[k] - slack && c[k] <= hi[k] + slack) {
            keep.push(r);
        }
    }
    let mine: Vec<&crate::walls::Wall> = (0..walls.len()).filter(|&i| keep.contains(&root(&mut group, i))).map(|i| &walls[i]).collect();
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
    // The margin kept around the walls, in cells: it closes gaps in the walls (a doorway, the space
    // between a garage and the house) and is the buffer around the building where coverage counts.
    let margin = (SEAL_M / (cell * mpp)).ceil() as usize;
    let buffer = (BUFFER_M / (cell * mpp)).ceil() as usize;
    let pad = margin + 3;
    let (w, h) = ((((max[0] - min[0]) / cell).ceil() as usize) + 2 * pad, (((max[1] - min[1]) / cell).ceil() as usize) + 2 * pad);
    let origin = [min[0] - pad as f32 * cell, min[1] - pad as f32 * cell];
    let mut wall = vec![false; w * h];
    for s in &mine {
        let len = s.length_px();
        let steps = (len / (cell * 0.5)).ceil().max(1.0) as usize;
        for k in 0..=steps {
            let t = k as f32 / steps as f32;
            let (x, y) = (s.a[0] + (s.b[0] - s.a[0]) * t, s.a[1] + (s.b[1] - s.a[1]) * t);
            let (cx, cy) = (((x - origin[0]) / cell) as isize, ((y - origin[1]) / cell) as isize);
            if cx >= 0 && cy >= 0 && (cx as usize) < w && (cy as usize) < h {
                wall[cy as usize * w + cx as usize] = true;
            }
        }
    }
    // Close the walls: grow them by the margin, then shrink back, which bridges gaps (a doorway, an
    // open side, the space between a garage and the house) without fattening the walls. What the
    // closed walls enclose is the building.
    let grown = distance_within(&wall, w, h, margin);
    let grown_mask: Vec<bool> = grown.iter().map(|d| (*d as usize) <= margin).collect();
    let away = distance_within(&grown_mask.iter().map(|g| !g).collect::<Vec<_>>(), w, h, margin);
    let closed: Vec<bool> = (0..w * h).map(|i| grown_mask[i] && (away[i] as usize) > margin).collect();
    let mut outside = vec![false; w * h];
    let mut stack: Vec<usize> = (0..w * h).filter(|i| (i % w == 0 || i % w == w - 1 || i / w == 0 || i / w == h - 1) && !closed[*i]).collect();
    for &i in &stack {
        outside[i] = true;
    }
    while let Some(i) = stack.pop() {
        let (x, y) = (i % w, i / w);
        for (nx, ny) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)] {
            if nx < w && ny < h && !outside[ny * w + nx] && !closed[ny * w + nx] {
                outside[ny * w + nx] = true;
                stack.push(ny * w + nx);
            }
        }
    }
    // Where a wall is missing (drawn walls often leave out an open side), cells with walls on both
    // sides of them along their row and along their column are inside the building too.
    let mut ortho = vec![false; w * h];
    let mut across_row = vec![false; w * h];
    for y in 0..h {
        let hits: Vec<usize> = (0..w).filter(|x| wall[y * w + x]).collect();
        if let (Some(first), Some(last)) = (hits.first(), hits.last()) {
            for x in *first..=*last {
                across_row[y * w + x] = true;
            }
        }
    }
    for x in 0..w {
        let hits: Vec<usize> = (0..h).filter(|y| wall[y * w + x]).collect();
        if let (Some(first), Some(last)) = (hits.first(), hits.last()) {
            for y in *first..=*last {
                ortho[y * w + x] = across_row[y * w + x];
            }
        }
    }
    let placing: Vec<bool> = (0..w * h).map(|i| !outside[i] || ortho[i]).collect();
    // Coverage counts inside the walls and a buffer around them.
    let from_placing = distance_within(&placing, w, h, buffer);
    let inside: Vec<bool> = from_placing.iter().map(|d| (*d as usize) <= buffer).collect();
    let (bmin, bmax) = ([min[0] - margin as f32 * cell, min[1] - margin as f32 * cell], [max[0] + margin as f32 * cell, max[1] + margin as f32 * cell]);
    // Too little enclosed means the walls do not make a building: use the hull of the walls.
    let enclosed = placing.iter().filter(|b| **b).count();
    if enclosed * 100 < w * h * 8 {
        let mut pts: Vec<[f32; 2]> = mine.iter().flat_map(|w| [w.a, w.b]).collect();
        let hull = convex_hull(&mut pts);
        return Some(Zone { min: bmin, max: bmax, cell, w, inside: Vec::new(), placing: Vec::new(), hull: Some(hull) });
    }
    Some(Zone { min: origin, max: [origin[0] + w as f32 * cell, origin[1] + h as f32 * cell], cell, w, inside, placing, hull: None })
}

/// Distance in cells (8-connected) from the nearest `true` cell, counted up to `cap` (cells further
/// than that read `cap + 1`).
fn distance_within(sources: &[bool], w: usize, h: usize, cap: usize) -> Vec<u16> {
    let mut d = vec![(cap + 1) as u16; w * h];
    let mut frontier: Vec<usize> = (0..w * h).filter(|i| sources[*i]).collect();
    for &i in &frontier {
        d[i] = 0;
    }
    for step in 1..=cap {
        let mut next = Vec::new();
        for &i in &frontier {
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            for dy in -1..=1isize {
                for dx in -1..=1isize {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h && d[ny as usize * w + nx as usize] > step as u16 {
                        d[ny as usize * w + nx as usize] = step as u16;
                        next.push(ny as usize * w + nx as usize);
                    }
                }
            }
        }
        frontier = next;
    }
    d
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
            zone: vec![],
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
        let moves = optimise(&f, Band::Ghz5, 1.0, &Goal::default()).unwrap().moves;
        assert_eq!(moves.len(), 2);
        for (i, p) in moves {
            f.aps[i].x = p[0];
            f.aps[i].y = p[1];
        }
        assert!(mean_clamped(&f) > before + 0.5 && mean_clamped(&f) > TARGET_DBM - 0.5, "{} vs {before}", mean_clamped(&f));
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
        let moves = optimise(&f, Band::Ghz5, 1.0, &Goal::default()).unwrap().moves;
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
            for (i, p) in optimise(&f, Band::Ghz5, 1.0, &Goal::default()).unwrap().moves {
                f.aps[i].x = p[0];
                f.aps[i].y = p[1];
            }
            for a in &f.aps {
                assert!((200.0..=600.0).contains(&a.x) && (150.0..=350.0).contains(&a.y), "gap={gap}: ({}, {})", a.x, a.y);
            }
        }
    }

    #[test]
    fn a_house_one_access_point_cannot_cover_gets_more_suggested() {
        use crate::walls::Wall;
        use crate::Material;
        // A 60 x 20 m building with a concrete wall every 10 m: one access point cannot serve it.
        let mut f = floor(vec![PlannedAp::new("a".into(), 300.0, 100.0)]);
        f.width_px = 800;
        f.height_px = 400;
        let w = |a: [f32; 2], b: [f32; 2]| Wall { height_m: None, ..Wall::new(a, b, Material::Concrete) };
        f.walls = vec![w([100.0, 100.0], [700.0, 100.0]), w([100.0, 300.0], [700.0, 300.0]), w([100.0, 100.0], [100.0, 300.0]), w([700.0, 100.0], [700.0, 300.0])];
        for x in [200.0, 300.0, 400.0, 500.0, 600.0] {
            f.walls.push(w([x, 100.0], [x, 300.0]));
        }
        let out = optimise(&f, Band::Ghz5, 1.0, &Goal::default()).unwrap();
        assert!(out.coverage_after < 0.95, "{}", out.coverage_after);
        assert!(!out.extra.is_empty() && out.coverage_with_extra > out.coverage_after + 0.02, "{out:?}");
        // A small open room needs nothing more.
        let small = optimise(&house(false), Band::Ghz5, 1.0, &Goal::default()).unwrap();
        assert!(small.extra.is_empty(), "{small:?}");
    }

    #[test]
    fn a_detached_garage_group_of_walls_is_part_of_the_zone() {
        use crate::walls::Wall;
        use crate::Material;
        let mut f = house(false);
        let w = |a: [f32; 2], b: [f32; 2]| Wall { height_m: None, ..Wall::new(a, b, Material::Concrete) };
        // A garage 4 m to the left of the house, not touching it.
        f.walls.extend([w([60.0, 150.0], [160.0, 150.0]), w([60.0, 350.0], [160.0, 350.0]), w([60.0, 150.0], [60.0, 350.0]), w([160.0, 150.0], [160.0, 350.0])]);
        let zone = safe_zone(&f, 0.1).unwrap();
        assert!(zone.contains([100.0, 250.0]) && zone.contains([400.0, 250.0]));
    }

    #[test]
    fn a_painted_area_is_the_only_place_access_points_go() {
        use crate::coverage::Stroke;
        let mut f = house(false);
        let (w, h) = (f.width_px as f32, f.height_px as f32);
        // Paint a loop round the left third of the plan: the inside fills.
        let (x0, x1, y0, y1) = (20.0, w / 3.0, 20.0, h - 20.0);
        let ring = vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]];
        f.zone = vec![Stroke { points: ring, radius: 4.0, erase: false, closed: true }];
        f.aps = vec![PlannedAp::new("a".into(), w * 0.8, h * 0.5), PlannedAp::new("b".into(), w * 0.9, h * 0.2)];
        let out = optimise(&f, Band::Ghz5, 1.0, &Goal::default()).unwrap();
        assert!(!out.moves.is_empty());
        for m in &out.moves {
            assert!(m.1[0] <= x1 + 4.0 && m.1[0] >= x0 - 4.0 && m.1[1] >= y0 - 4.0 && m.1[1] <= y1 + 4.0, "moved outside the painted area: {m:?}");
        }
    }

    #[test]
    fn a_good_layout_is_left_alone_and_needs_a_scale() {
        let f = floor(vec![PlannedAp::new("a".into(), 200.0, 100.0)]);
        let moves = optimise(&f, Band::Ghz5, 1.0, &Goal::default()).unwrap().moves;
        assert!(moves.is_empty() || moves.len() == 1);
        let mut none = floor(vec![]);
        assert_eq!(optimise(&none, Band::Ghz5, 1.0, &Goal::default()), Err(Unavailable::NoAps));
        none.metres_per_px = None;
        assert_eq!(optimise(&none, Band::Ghz5, 1.0, &Goal::default()), Err(Unavailable::NoScale));
    }

    /// Try the optimiser on a saved project: `WS_PROJECT=/path/x.wifispanner cargo test --release try_project -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn try_project() {
        let text = std::fs::read_to_string(std::env::var("WS_PROJECT").unwrap()).unwrap();
        let project = crate::survey::Project::from_json(&text).unwrap();
        let f = project.floor();
        println!("walls {} aps {:?}", f.walls.len(), f.aps.iter().map(|a| (a.x as i32, a.y as i32)).collect::<Vec<_>>());
        println!("{:?}", optimise(f, Band::Ghz5, 1.0, &Goal::default()));
    }

    /// Write the zone as text for a look: `WS_PROJECT=... WS_ZONE=/path/zone.txt cargo test --release dump_zone -- --ignored`.
    #[test]
    #[ignore]
    fn dump_zone() {
        let text = std::fs::read_to_string(std::env::var("WS_PROJECT").unwrap()).unwrap();
        let project = crate::survey::Project::from_json(&text).unwrap();
        let f = project.floor();
        let z = safe_zone(f, f.metres_per_px.unwrap()).unwrap();
        let mut out = format!("{} {} {} {} {}\n", z.min[0], z.min[1], z.cell, z.w, z.inside.len() / z.w.max(1));
        for (i, c) in z.inside.iter().enumerate() {
            out.push(if z.placing.get(i).copied().unwrap_or(false) { '2' } else if *c { '1' } else { '0' });
            if i % z.w == z.w - 1 {
                out.push('\n');
            }
        }
        std::fs::write(std::env::var("WS_ZONE").unwrap(), out).unwrap();
    }
}

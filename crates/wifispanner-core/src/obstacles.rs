// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! Areas that absorb signal along the way through them, such as parked cars,
//! foliage, shelving and crowds, drawn as rectangles or polygons.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ObstacleKind {
    Car,
    Cubicles,
    Elevator,
    FoliageHeavy,
    FoliageLight,
    HumanCrowd,
    Machinery,
    ServerRack,
    ShelfGroceries,
    ShelfLiquids,
    ShelfWarehouse,
    Truck,
}

impl ObstacleKind {
    pub const ALL: [ObstacleKind; 12] = [
        ObstacleKind::Car,
        ObstacleKind::Cubicles,
        ObstacleKind::Elevator,
        ObstacleKind::FoliageHeavy,
        ObstacleKind::FoliageLight,
        ObstacleKind::HumanCrowd,
        ObstacleKind::Machinery,
        ObstacleKind::ServerRack,
        ObstacleKind::ShelfGroceries,
        ObstacleKind::ShelfLiquids,
        ObstacleKind::ShelfWarehouse,
        ObstacleKind::Truck,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ObstacleKind::Car => "Car",
            ObstacleKind::Cubicles => "Cubicles",
            ObstacleKind::Elevator => "Elevator",
            ObstacleKind::FoliageHeavy => "Foliage (Heavy)",
            ObstacleKind::FoliageLight => "Foliage (Light)",
            ObstacleKind::HumanCrowd => "Human Crowd",
            ObstacleKind::Machinery => "Machinery",
            ObstacleKind::ServerRack => "Server Rack",
            ObstacleKind::ShelfGroceries => "Shelf (Groceries)",
            ObstacleKind::ShelfLiquids => "Shelf (Liquids)",
            ObstacleKind::ShelfWarehouse => "Shelf (Warehouse)",
            ObstacleKind::Truck => "Truck",
        }
    }

    /// Loss in dB for each metre of the area the signal passes through.
    pub fn db_per_m(self) -> f32 {
        match self {
            ObstacleKind::Car => 10.0,
            ObstacleKind::Cubicles => 2.0,
            ObstacleKind::Elevator => 10.0,
            ObstacleKind::FoliageHeavy => 2.0,
            ObstacleKind::FoliageLight => 1.0,
            ObstacleKind::HumanCrowd => 2.0,
            ObstacleKind::Machinery => 8.0,
            ObstacleKind::ServerRack => 5.0,
            ObstacleKind::ShelfGroceries => 4.0,
            ObstacleKind::ShelfLiquids => 8.0,
            ObstacleKind::ShelfWarehouse => 8.0,
            ObstacleKind::Truck => 10.0,
        }
    }

    /// How tall a new area of this kind starts, in metres. `None` is the full
    /// height of the floor.
    pub fn default_height_m(self) -> Option<f32> {
        match self {
            ObstacleKind::Car => Some(1.5),
            ObstacleKind::Cubicles => Some(1.5),
            ObstacleKind::Elevator => None,
            ObstacleKind::FoliageHeavy => Some(2.5),
            ObstacleKind::FoliageLight => Some(2.0),
            ObstacleKind::HumanCrowd => Some(1.8),
            ObstacleKind::Machinery => Some(2.5),
            ObstacleKind::ServerRack => Some(2.0),
            ObstacleKind::ShelfGroceries => Some(2.0),
            ObstacleKind::ShelfLiquids => Some(2.0),
            ObstacleKind::ShelfWarehouse => Some(4.0),
            ObstacleKind::Truck => Some(3.0),
        }
    }

    /// Fill colour.
    pub fn colour(self) -> [u8; 3] {
        match self {
            ObstacleKind::Car | ObstacleKind::Truck | ObstacleKind::Elevator => [220, 90, 70],
            ObstacleKind::Cubicles | ObstacleKind::HumanCrowd => [235, 190, 60],
            ObstacleKind::FoliageHeavy => [40, 150, 70],
            ObstacleKind::FoliageLight => [120, 200, 90],
            ObstacleKind::Machinery | ObstacleKind::ShelfLiquids | ObstacleKind::ShelfWarehouse | ObstacleKind::ShelfGroceries => [150, 150, 160],
            ObstacleKind::ServerRack => [60, 190, 200],
        }
    }
}

/// An area of one kind of obstacle, as a polygon in floor plan pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Area {
    pub points: Vec<[f32; 2]>,
    pub kind: ObstacleKind,
    /// Height in metres; `None` is the full height of the floor.
    pub height_m: Option<f32>,
}

impl Area {
    pub fn new(points: Vec<[f32; 2]>, kind: ObstacleKind) -> Self {
        Area { points, kind, height_m: kind.default_height_m() }
    }

    pub fn rectangle(a: [f32; 2], b: [f32; 2], kind: ObstacleKind) -> Self {
        let (x0, x1, y0, y1) = (a[0].min(b[0]), a[0].max(b[0]), a[1].min(b[1]), a[1].max(b[1]));
        Area::new(vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]], kind)
    }

    pub fn effective_height_m(&self, ceiling_m: f32) -> f32 {
        self.height_m.unwrap_or(ceiling_m).clamp(0.0, ceiling_m.max(0.0))
    }

    /// Whether `p` is inside the polygon.
    pub fn contains(&self, p: [f32; 2]) -> bool {
        let n = self.points.len();
        let mut inside = false;
        let mut j = n.wrapping_sub(1);
        for i in 0..n {
            let (a, b) = (self.points[i], self.points[j]);
            if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
                inside = !inside;
            }
            j = i;
        }
        inside
    }

    /// True when any part of the polygon lies inside the rectangle.
    pub fn intersects_rect(&self, min: [f32; 2], max: [f32; 2]) -> bool {
        let n = self.points.len();
        if n == 0 {
            return false;
        }
        if self.points.iter().any(|p| p[0] >= min[0] && p[0] <= max[0] && p[1] >= min[1] && p[1] <= max[1]) {
            return true;
        }
        if self.contains(min) || self.contains(max) || self.contains([min[0], max[1]]) || self.contains([max[0], min[1]]) {
            return true;
        }
        (0..n).any(|i| {
            let edge = crate::walls::Wall::new(self.points[i], self.points[(i + 1) % n], crate::Material::Concrete);
            edge.intersects_rect(min, max)
        })
    }

    /// The parts of the segment `from` to `to` inside the polygon, as ranges
    /// of the segment's length from 0 to 1.
    pub fn inside_ranges(&self, from: [f32; 2], to: [f32; 2]) -> Vec<(f32, f32)> {
        let n = self.points.len();
        if n < 3 {
            return Vec::new();
        }
        let (rx, ry) = (to[0] - from[0], to[1] - from[1]);
        let mut cuts = vec![0.0f32, 1.0];
        for i in 0..n {
            let (a, b) = (self.points[i], self.points[(i + 1) % n]);
            let (sx, sy) = (b[0] - a[0], b[1] - a[1]);
            let denom = rx * sy - ry * sx;
            if denom.abs() < 1e-9 {
                continue;
            }
            let (qx, qy) = (a[0] - from[0], a[1] - from[1]);
            let t = (qx * sy - qy * sx) / denom;
            let u = (qx * ry - qy * rx) / denom;
            if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
                cuts.push(t);
            }
        }
        cuts.sort_by(f32::total_cmp);
        cuts.windows(2)
            .filter(|w| w[1] - w[0] > 1e-6)
            .filter(|w| {
                let m = (w[0] + w[1]) / 2.0;
                self.contains([from[0] + rx * m, from[1] + ry * m])
            })
            .map(|w| (w[0], w[1]))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> Area {
        Area::rectangle([10.0, 10.0], [20.0, 20.0], ObstacleKind::ShelfWarehouse)
    }

    #[test]
    fn kinds_carry_their_loss_and_start_height() {
        assert_eq!(ObstacleKind::Car.db_per_m(), 10.0);
        assert_eq!(ObstacleKind::FoliageLight.db_per_m(), 1.0);
        assert_eq!(ObstacleKind::Elevator.default_height_m(), None);
        assert_eq!(Area::new(vec![], ObstacleKind::Car).height_m, Some(1.5));
    }

    #[test]
    fn points_are_inside_or_out() {
        let a = square();
        assert!(a.contains([15.0, 15.0]) && !a.contains([25.0, 15.0]) && !a.contains([5.0, 5.0]));
        let l = Area::new(vec![[0.0, 0.0], [20.0, 0.0], [20.0, 5.0], [5.0, 5.0], [5.0, 20.0], [0.0, 20.0]], ObstacleKind::Car);
        assert!(l.contains([2.0, 15.0]) && !l.contains([15.0, 15.0]), "an L shape has a notch");
    }

    #[test]
    fn a_line_through_a_square_is_inside_for_its_middle_part() {
        let r = square().inside_ranges([0.0, 15.0], [30.0, 15.0]);
        assert_eq!(r.len(), 1);
        assert!((r[0].0 - 10.0 / 30.0).abs() < 1e-5 && (r[0].1 - 20.0 / 30.0).abs() < 1e-5, "{r:?}");
        assert!(square().inside_ranges([0.0, 0.0], [30.0, 0.0]).is_empty(), "misses it");
        let starts_inside = square().inside_ranges([15.0, 15.0], [30.0, 15.0]);
        assert_eq!(starts_inside.len(), 1);
        assert!(starts_inside[0].0 < 1e-5);
    }

    #[test]
    fn a_line_through_a_notched_shape_is_inside_twice() {
        let u = Area::new(vec![[0.0, 0.0], [30.0, 0.0], [30.0, 10.0], [20.0, 10.0], [20.0, 4.0], [10.0, 4.0], [10.0, 10.0], [0.0, 10.0]], ObstacleKind::Car);
        assert_eq!(u.inside_ranges([-5.0, 7.0], [35.0, 7.0]).len(), 2, "through both arms of the U");
    }

    #[test]
    fn boxes_select_areas_they_touch_or_sit_inside() {
        let a = square();
        assert!(a.intersects_rect([0.0, 0.0], [12.0, 12.0]), "a corner in");
        assert!(a.intersects_rect([12.0, 12.0], [14.0, 14.0]), "box wholly inside the polygon");
        assert!(a.intersects_rect([0.0, 0.0], [30.0, 30.0]), "polygon inside the box");
        assert!(a.intersects_rect([5.0, 14.0], [25.0, 16.0]), "a bar across it");
        assert!(!a.intersects_rect([30.0, 30.0], [40.0, 40.0]));
    }
}

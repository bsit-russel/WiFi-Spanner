// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! Where the floors of a building sit relative to each other, so one floor
//! can be overlaid on another to check coverage between them.

use serde::{Deserialize, Serialize};

/// A 2D affine transform: `x' = a x + c y + tx`, `y' = b x + d y + ty`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Affine {
    pub const IDENTITY: Affine = Affine { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 0.0, ty: 0.0 };

    /// Scale, then rotate by `deg` degrees, then move by `offset`.
    pub fn new(scale: f32, deg: f32, offset: [f32; 2]) -> Self {
        let (s, c) = deg.to_radians().sin_cos();
        Affine { a: scale * c, b: scale * s, c: -scale * s, d: scale * c, tx: offset[0], ty: offset[1] }
    }

    pub fn apply(&self, p: [f32; 2]) -> [f32; 2] {
        [self.a * p[0] + self.c * p[1] + self.tx, self.b * p[0] + self.d * p[1] + self.ty]
    }

    /// `self` applied after `first`.
    pub fn after(&self, first: &Affine) -> Affine {
        let t = self.apply([first.tx, first.ty]);
        Affine {
            a: self.a * first.a + self.c * first.b,
            b: self.b * first.a + self.d * first.b,
            c: self.a * first.c + self.c * first.d,
            d: self.b * first.c + self.d * first.d,
            tx: t[0],
            ty: t[1],
        }
    }

    pub fn inverse(&self) -> Option<Affine> {
        let det = self.a * self.d - self.b * self.c;
        if det.abs() < 1e-9 {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        Some(Affine { a, b, c, d, tx: -(a * self.tx + c * self.ty), ty: -(b * self.tx + d * self.ty) })
    }
}

/// How a floor sits against the building's first floor, in that floor's pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Placement {
    pub offset: [f32; 2],
    pub rotation_deg: f32,
}

impl Placement {
    /// From the floor's own pixels to the first floor's pixels. `scale` is
    /// this floor's metres per pixel over the first floor's, when both are known.
    pub fn transform(&self, scale: f32) -> Affine {
        Affine::new(scale, self.rotation_deg, self.offset)
    }

    /// The placement after turning to `deg` about `centre` (this floor's own
    /// pixels), keeping that point where it is.
    pub fn rotated_about(&self, scale: f32, centre: [f32; 2], deg: f32) -> Placement {
        let fixed = self.transform(scale).apply(centre);
        let turned = Affine::new(scale, deg, [0.0, 0.0]).apply(centre);
        Placement { offset: [fixed[0] - turned[0], fixed[1] - turned[1]], rotation_deg: deg }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3
    }

    #[test]
    fn inverse_undoes_the_transform() {
        let t = Affine::new(2.0, 30.0, [10.0, -4.0]);
        let p = [7.0, 3.0];
        assert!(close(t.inverse().unwrap().apply(t.apply(p)), p));
        assert!(Affine::new(0.0, 0.0, [0.0; 2]).inverse().is_none());
    }

    #[test]
    fn composing_matches_applying_in_turn() {
        let (f, g) = (Affine::new(1.5, 20.0, [3.0, 4.0]), Affine::new(0.5, -45.0, [-2.0, 9.0]));
        let p = [5.0, -6.0];
        assert!(close(g.after(&f).apply(p), g.apply(f.apply(p))));
    }

    #[test]
    fn rotating_about_a_point_keeps_it_still() {
        let placement = Placement { offset: [40.0, 25.0], rotation_deg: 10.0 };
        let centre = [100.0, 60.0];
        let before = placement.transform(1.0).apply(centre);
        let turned = placement.rotated_about(1.0, centre, 75.0);
        assert!(close(turned.transform(1.0).apply(centre), before));
        assert_eq!(turned.rotation_deg, 75.0);
    }

    #[test]
    fn a_quarter_turn_swaps_axes() {
        assert!(close(Affine::new(1.0, 90.0, [0.0; 2]).apply([1.0, 0.0]), [0.0, 1.0]));
    }
}

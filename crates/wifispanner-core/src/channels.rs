// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 rubble-za. See the LICENSE and NOTICE files.

//! A channel plan: which channel each access point should use on each band.
//!
//! Access points that hear each other well must not share a channel, or they take turns on the
//! air. The plan gives every band a short list of channels that do not overlap (1, 6 and 11 on
//! 2.4 GHz; four 40 MHz blocks that need no radar checks on 5 GHz; preferred scanning channels on
//! 6 GHz) and picks, for the whole house, the choice with the least co-channel interference
//! between access points, counting each pair by how strongly the two hear each other through the
//! walls. Neighbouring access points land on different channels, so a client roaming between
//! them keeps a clean link on each.

use crate::predict::{scene, signal_from, Channel, RadioBand};
use crate::survey::Floor;
use wifispanner_scan::Band;

pub const CHANNELS_2_4: [u32; 3] = [1, 6, 11];
/// Primary channels of the 40 MHz blocks 36-40, 44-48, 149-153 and 157-161.
pub const CHANNELS_5: [u32; 4] = [36, 44, 149, 157];
/// Preferred scanning channels of the 6 GHz band.
pub const CHANNELS_6: [u32; 6] = [5, 21, 37, 53, 69, 85];

/// Two access points hearing each other better than this share their air if on one channel.
const AUDIBLE_DBM: f32 = -92.0;
/// Up to this many access points are planned by trying every combination.
const EXHAUSTIVE_UP_TO: usize = 8;

fn candidates(band: RadioBand) -> &'static [u32] {
    match band {
        RadioBand::Ghz2_4 => &CHANNELS_2_4,
        RadioBand::Ghz5 => &CHANNELS_5,
        RadioBand::Ghz6 => &CHANNELS_6,
    }
}

/// The plan for every enabled access point of `floor.aps`, as `(index in floor.aps, channels)`.
pub fn plan(floor: &Floor) -> Vec<(usize, Vec<Channel>)> {
    let mut out: Vec<(usize, Vec<Channel>)> = floor.aps.iter().enumerate().filter(|(_, a)| a.enabled).map(|(i, _)| (i, Vec::new())).collect();
    for band in RadioBand::ALL {
        let on_band: Vec<usize> = (0..out.len()).filter(|&k| floor.aps[out[k].0].eirp_dbm(band.band()).is_some()).collect();
        if on_band.is_empty() {
            continue;
        }
        let pool = candidates(band);
        let assigned = assign(floor, band, &on_band.iter().map(|&k| out[k].0).collect::<Vec<_>>(), pool.len());
        for (slot, &k) in on_band.iter().enumerate() {
            out[k].1.push(Channel { band, number: pool[assigned[slot]] });
        }
    }
    out
}

/// How much two access points disturb each other when on one channel: 0 when they cannot hear each other.
fn coupling(floor: &Floor, band: Band, mpp: Option<f32>, a: usize, b: usize) -> f32 {
    let Some(mpp) = mpp else { return 1.0 };
    let scene = scene(floor, mpp, band);
    let hear = |from: usize, to: usize| {
        let (f, t) = (&floor.aps[from], &floor.aps[to]);
        let Some(eirp) = f.eirp_dbm(band) else { return f32::MIN };
        signal_from(f, eirp, [f.x * mpp, f.y * mpp], [t.x * mpp, t.y * mpp, t.height_m], &scene, band)
    };
    let strongest = hear(a, b).max(hear(b, a));
    (strongest - AUDIBLE_DBM).max(0.0)
}

/// A channel index for each of `aps` (indices into `floor.aps`), from a pool of `k`.
fn assign(floor: &Floor, band: RadioBand, aps: &[usize], k: usize) -> Vec<usize> {
    let n = aps.len();
    let mpp = floor.metres_per_px.filter(|m| m.is_finite() && *m > 0.0);
    let mut w = vec![vec![0.0f32; n]; n];
    for i in 0..n {
        for j in i + 1..n {
            w[i][j] = coupling(floor, band.band(), mpp, aps[i], aps[j]);
            w[j][i] = w[i][j];
        }
    }
    let cost = |c: &[usize]| -> f32 { (0..n).flat_map(|i| (i + 1..n).map(move |j| (i, j))).filter(|&(i, j)| c[i] == c[j]).map(|(i, j)| w[i][j]).sum() };
    if n <= EXHAUSTIVE_UP_TO {
        // Every combination, the first access point fixed to the first channel (the rest follow by symmetry).
        let mut best = (f32::MAX, vec![0; n]);
        let mut c = vec![0usize; n];
        loop {
            let v = cost(&c);
            if v < best.0 - 1e-6 {
                best = (v, c.clone());
            }
            // Next combination, leaving c[0] at 0.
            let mut i = n;
            loop {
                if i <= 1 {
                    return best.1;
                }
                i -= 1;
                c[i] += 1;
                if c[i] < k {
                    break;
                }
                c[i] = 0;
            }
        }
    }
    // Many access points: one at a time onto its quietest channel, then a few passes to improve.
    let mut c = vec![0usize; n];
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|a, b| w[*b].iter().sum::<f32>().total_cmp(&w[*a].iter().sum::<f32>()));
    let mut placed = vec![false; n];
    for &i in &order {
        c[i] = (0..k).min_by(|&x, &y| {
            let load = |ch: usize| (0..n).filter(|&j| placed[j] && c[j] == ch).map(|j| w[i][j]).sum::<f32>();
            load(x).total_cmp(&load(y))
        }).unwrap_or(0);
        placed[i] = true;
    }
    for _ in 0..4 {
        for &i in &order {
            c[i] = (0..k).min_by(|&x, &y| {
                let load = |ch: usize| (0..n).filter(|&j| j != i && c[j] == ch).map(|j| w[i][j]).sum::<f32>();
                load(x).total_cmp(&load(y))
            }).unwrap_or(0);
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::floors::Placement;
    use crate::predict::PlannedAp;

    fn floor(aps: Vec<PlannedAp>) -> Floor {
        Floor {
            name: "t".into(),
            image: vec![],
            width_px: 1000,
            height_px: 400,
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

    fn channel(plan: &[(usize, Vec<Channel>)], ap: usize, band: RadioBand) -> u32 {
        plan.iter().find(|p| p.0 == ap).unwrap().1.iter().find(|c| c.band == band).unwrap().number
    }

    #[test]
    fn neighbours_get_different_channels_and_distant_ones_may_share() {
        // Three in a row 10 m apart, a fourth 80 m away: the middle one differs from both ends.
        let aps = vec![
            PlannedAp::new("a".into(), 100.0, 200.0),
            PlannedAp::new("b".into(), 200.0, 200.0),
            PlannedAp::new("c".into(), 300.0, 200.0),
            PlannedAp::new("d".into(), 900.0, 200.0),
        ];
        let p = plan(&floor(aps));
        let ch = |i| channel(&p, i, RadioBand::Ghz2_4);
        assert_ne!(ch(0), ch(1));
        assert_ne!(ch(1), ch(2));
        assert_eq!(p.len(), 4);
        // 5 GHz has four channels: all four of these can differ where it matters.
        let c5 = |i| channel(&p, i, RadioBand::Ghz5);
        assert_ne!(c5(0), c5(1));
        assert_ne!(c5(1), c5(2));
    }

    #[test]
    fn a_lone_access_point_gets_the_first_channel_of_each_band_it_has() {
        let p = plan(&floor(vec![PlannedAp::new("a".into(), 100.0, 100.0)]));
        let chans = &p[0].1;
        assert!(chans.contains(&Channel { band: RadioBand::Ghz2_4, number: 1 }));
        assert!(chans.contains(&Channel { band: RadioBand::Ghz5, number: 36 }));
    }
}

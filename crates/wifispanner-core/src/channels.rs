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

/// How wide the channels are (MHz) on each band, and whether channels that must check for radar
/// (DFS, part of 5 GHz) may be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanOptions {
    pub width_2_4: u32,
    pub width_5: u32,
    pub width_6: u32,
    pub allow_dfs: bool,
}

impl Default for PlanOptions {
    fn default() -> Self {
        PlanOptions { width_2_4: 20, width_5: 40, width_6: 80, allow_dfs: false }
    }
}

impl PlanOptions {
    pub const WIDTHS_2_4: [u32; 2] = [20, 40];
    pub const WIDTHS_5: [u32; 4] = [20, 40, 80, 160];
    pub const WIDTHS_6: [u32; 5] = [20, 40, 80, 160, 320];

    pub fn width(&self, band: RadioBand) -> u32 {
        match band {
            RadioBand::Ghz2_4 => self.width_2_4,
            RadioBand::Ghz5 => self.width_5,
            RadioBand::Ghz6 => self.width_6,
        }
    }

    /// The signal (dBm) that counts as good on a band. Every doubling of the width lets in twice
    /// the noise (3 dB), so wider channels need a stronger signal for the same quality of link.
    pub fn good_dbm(&self, band: RadioBand) -> f32 {
        -67.0 + 3.0 * (self.width(band) as f32 / 20.0).log2()
    }
}

/// 5 GHz channels that need no radar checks, then the ones that do (DFS), for each width: the
/// first channel of each block.
fn pool_5(width: u32, allow_dfs: bool) -> Vec<u32> {
    let (free, dfs): (&[u32], &[u32]) = match width {
        20 => (&[36, 40, 44, 48, 149, 153, 157, 161, 165], &[52, 56, 60, 64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144]),
        40 => (&[36, 44, 149, 157], &[52, 60, 100, 108, 116, 124, 132, 140]),
        80 => (&[36, 149], &[52, 100, 116, 132]),
        // 160 MHz blocks 36-64 and 100-128 both include DFS channels.
        _ => (&[], &[36, 100]),
    };
    free.iter().chain(if allow_dfs { dfs } else { &[] }).copied().collect()
}

/// Centre channels of the 6 GHz blocks of a width.
fn pool_6(width: u32) -> Vec<u32> {
    match width {
        20 => vec![5, 21, 37, 53, 69, 85, 101, 117, 133, 149, 165, 181, 197, 213, 229],
        40 => (0..29).map(|k| 3 + 8 * k).collect(),
        80 => (0..14).map(|k| 7 + 16 * k).collect(),
        160 => (0..7).map(|k| 15 + 32 * k).collect(),
        _ => vec![31, 95, 159],
    }
}

/// At most this many channels of a band are tried: a house needs few, and more only slow the search.
const POOL_LIMIT: usize = 6;

/// The channels to choose from on a band, and a note when the settings leave none (the plan then
/// uses the next narrower width that has some).
fn pool(band: RadioBand, o: &PlanOptions) -> (Vec<u32>, u32, Option<String>) {
    let mut width = o.width(band);
    let mut note = None;
    loop {
        let list = match band {
            RadioBand::Ghz2_4 => if width <= 20 { vec![1, 6, 11] } else { vec![3] },
            RadioBand::Ghz5 => pool_5(width, o.allow_dfs),
            RadioBand::Ghz6 => pool_6(width),
        };
        if !list.is_empty() || width <= 20 {
            let list: Vec<u32> = list.into_iter().take(POOL_LIMIT).collect();
            return (list, width, note);
        }
        note = Some(format!("{} MHz channels on {} need DFS (radar-checked) channels, which are not allowed: planned at {} MHz instead.", o.width(band), band.band(), width / 2));
        width /= 2;
    }
}

/// A channel plan: what each access point should use, the width it is planned at, and what to warn about.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// For every enabled access point: `(index in floor.aps, channels)`.
    pub assignments: Vec<(usize, Vec<Channel>)>,
    /// The width actually planned per band (it can differ from the setting when none fits).
    pub widths: Vec<(RadioBand, u32)>,
    /// Pairs of access points (indices in `floor.aps`) that hear each other well yet have to share a channel, with the band.
    pub shared: Vec<(usize, usize, RadioBand)>,
    pub notes: Vec<String>,
}

/// The plan for every enabled access point of `floor.aps`.
pub fn plan(floor: &Floor, options: &PlanOptions) -> Plan {
    let mut out: Vec<(usize, Vec<Channel>)> = floor.aps.iter().enumerate().filter(|(_, a)| a.enabled).map(|(i, _)| (i, Vec::new())).collect();
    let (mut widths, mut shared, mut notes) = (Vec::new(), Vec::new(), Vec::new());
    for band in RadioBand::ALL {
        let on_band: Vec<usize> = (0..out.len()).filter(|&k| floor.aps[out[k].0].eirp_dbm(band.band()).is_some()).collect();
        if on_band.is_empty() {
            continue;
        }
        let (channels, width, note) = pool(band, options);
        notes.extend(note);
        widths.push((band, width));
        let indices: Vec<usize> = on_band.iter().map(|&k| out[k].0).collect();
        let (assigned, weights) = assign(floor, band, &indices, channels.len().max(1));
        for (slot, &k) in on_band.iter().enumerate() {
            out[k].1.push(Channel { band, number: channels[assigned[slot]] });
        }
        for i in 0..indices.len() {
            for j in i + 1..indices.len() {
                // Heard better than -82 dBm (10 dB over the cut-off) and on the same channel.
                if assigned[i] == assigned[j] && weights[i][j] >= 10.0 {
                    shared.push((indices[i], indices[j], band));
                }
            }
        }
    }
    Plan { assignments: out, widths, shared, notes }
}

/// Two access points hearing each other better than this share their air if on one channel.
const AUDIBLE_DBM: f32 = -92.0;
/// Up to this many access points are planned by trying every combination.
const EXHAUSTIVE_UP_TO: usize = 8;

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
fn assign(floor: &Floor, band: RadioBand, aps: &[usize], k: usize) -> (Vec<usize>, Vec<Vec<f32>>) {
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
                    return (best.1, w);
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
    (c, w)
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
        let p = plan(&floor(aps), &PlanOptions::default()).assignments;
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
        let p = plan(&floor(vec![PlannedAp::new("a".into(), 100.0, 100.0)]), &PlanOptions::default()).assignments;
        let chans = &p[0].1;
        assert!(chans.contains(&Channel { band: RadioBand::Ghz2_4, number: 1 }));
        assert!(chans.contains(&Channel { band: RadioBand::Ghz5, number: 36 }));
    }

    #[test]
    fn wider_channels_leave_fewer_to_choose_from_and_dfs_adds_some() {
        let o = |w, dfs| PlanOptions { width_5: w, allow_dfs: dfs, ..PlanOptions::default() };
        assert_eq!(pool(RadioBand::Ghz5, &o(80, false)).0, vec![36, 149]);
        assert!(pool(RadioBand::Ghz5, &o(80, true)).0.len() > 2);
        // 160 MHz without DFS has no channel: the plan drops to 80 and says so.
        let (list, width, note) = pool(RadioBand::Ghz5, &o(160, false));
        assert_eq!((list, width), (vec![36, 149], 80));
        assert!(note.unwrap().contains("DFS"));
        assert_eq!(pool(RadioBand::Ghz6, &PlanOptions { width_6: 320, ..PlanOptions::default() }).0, vec![31, 95, 159]);
    }

    #[test]
    fn wider_channels_need_a_stronger_signal_to_count_as_good() {
        let o = |w| PlanOptions { width_5: w, ..PlanOptions::default() };
        assert_eq!(o(20).good_dbm(RadioBand::Ghz5), -67.0);
        assert!((o(80).good_dbm(RadioBand::Ghz5) - -61.0).abs() < 1e-4);
        assert!((o(160).good_dbm(RadioBand::Ghz5) - -58.0).abs() < 1e-4);
    }

    #[test]
    fn neighbours_forced_onto_one_channel_are_reported() {
        let aps = vec![PlannedAp::new("a".into(), 100.0, 200.0), PlannedAp::new("b".into(), 200.0, 200.0), PlannedAp::new("c".into(), 300.0, 200.0)];
        // 2.4 GHz at 40 MHz has one channel: all three share it.
        let p = plan(&floor(aps), &PlanOptions { width_2_4: 40, ..PlanOptions::default() });
        assert!(p.shared.iter().any(|s| s.2 == RadioBand::Ghz2_4), "{:?}", p.shared);
    }
}

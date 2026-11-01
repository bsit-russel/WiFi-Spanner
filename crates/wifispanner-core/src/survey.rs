// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! Survey data: a floor plan with the measurements taken on it.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::floors::{Affine, Placement};
use wifispanner_scan::{Band, ScanResult};

/// Signal assumed for an AP that was not heard at a surveyed spot.
pub const MISSING_DBM: f64 = -100.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    /// File format version, for migrating old projects.
    pub version: u32,
    /// The building's floors, lowest first. Never empty.
    #[serde(default)]
    pub floors: Vec<Floor>,
    /// The floor being worked on.
    #[serde(default)]
    pub active: usize,
    /// Projects saved before there were several floors hold just this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    floor: Option<Floor>,
}

impl Project {
    pub const VERSION: u32 = 2;

    pub fn new(floor: Floor) -> Self {
        Project { version: Self::VERSION, floors: vec![floor], active: 0, floor: None }
    }

    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }

    pub fn from_json(s: &str) -> serde_json::Result<Self> {
        let mut p: Project = serde_json::from_str(s)?;
        if let Some(old) = p.floor.take() {
            p.floors.insert(0, old);
        }
        if p.floors.is_empty() {
            return Err(serde::de::Error::custom("the project has no floors"));
        }
        p.version = Self::VERSION;
        p.active = p.active.min(p.floors.len() - 1);
        for f in &mut p.floors {
            f.migrate_ap_layers();
        }
        Ok(p)
    }

    /// The floor being worked on.
    pub fn floor(&self) -> &Floor {
        &self.floors[self.active.min(self.floors.len() - 1)]
    }

    pub fn floor_mut(&mut self) -> &mut Floor {
        let i = self.active.min(self.floors.len() - 1);
        &mut self.floors[i]
    }

    /// Add a floor above the others and make it the active one.
    pub fn add_floor(&mut self, floor: Floor) {
        self.floors.push(floor);
        self.active = self.floors.len() - 1;
    }

    /// Remove floor `i` unless it is the only one; returns whether it went.
    pub fn remove_floor(&mut self, i: usize) -> bool {
        if self.floors.len() < 2 || i >= self.floors.len() {
            return false;
        }
        self.floors.remove(i);
        self.active = self.active.min(self.floors.len() - 1);
        true
    }

    /// Metres-per-pixel of floor `i` over that of the first floor, so that
    /// calibrated floors line up at true size; 1 when either is unknown.
    fn scale_of(&self, i: usize) -> f32 {
        match (self.floors[i].metres_per_px, self.floors[0].metres_per_px) {
            (Some(m), Some(base)) if m > 0.0 && base > 0.0 => m / base,
            _ => 1.0,
        }
    }

    /// From floor `i`'s pixels to the first floor's pixels.
    pub fn to_base(&self, i: usize) -> Affine {
        self.floors[i].placement.transform(self.scale_of(i))
    }

    /// From floor `from`'s pixels to floor `onto`'s pixels: where one floor's
    /// features land when drawn over another.
    pub fn overlay_transform(&self, from: usize, onto: usize) -> Affine {
        match self.to_base(onto).inverse() {
            Some(inv) => inv.after(&self.to_base(from)),
            None => Affine::IDENTITY,
        }
    }

    /// Slide floor `i` by `delta` of floor `onto`'s pixels.
    pub fn nudge_overlay(&mut self, i: usize, onto: usize, delta: [f32; 2]) {
        let onto_to_base = self.to_base(onto);
        let d = [onto_to_base.a * delta[0] + onto_to_base.c * delta[1], onto_to_base.b * delta[0] + onto_to_base.d * delta[1]];
        let p = &mut self.floors[i].placement;
        p.offset = [p.offset[0] + d[0], p.offset[1] + d[1]];
    }

    /// Turn floor `i` to `deg` about its own centre.
    pub fn rotate_floor(&mut self, i: usize, deg: f32) {
        let scale = self.scale_of(i);
        let f = &self.floors[i];
        let centre = [f.width_px as f32 / 2.0, f.height_px as f32 / 2.0];
        self.floors[i].placement = f.placement.rotated_about(scale, centre, deg);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Floor {
    pub name: String,
    /// The floor plan exactly as imported (PNG or JPEG bytes).
    #[serde(with = "base64_bytes")]
    pub image: Vec<u8>,
    pub width_px: u32,
    pub height_px: u32,
    /// Set by calibrating against a known distance.
    pub metres_per_px: Option<f32>,
    pub points: Vec<SurveyPoint>,
    /// Walls drawn on the plan. Older projects have none.
    #[serde(default)]
    pub walls: Vec<crate::walls::Wall>,
    /// Ceiling height in metres, the height of a full-height wall.
    #[serde(default = "default_ceiling")]
    pub ceiling_m: f32,
    /// How this floor sits against the first one, for overlays.
    #[serde(default)]
    pub placement: Placement,
    /// Access points placed in the planner.
    #[serde(default)]
    pub aps: Vec<crate::predict::PlannedAp>,
    /// The access points as plotted while surveying: where they really were
    /// when the survey was taken. Planning works on copies in `aps`, so moving
    /// those never changes this record.
    #[serde(default)]
    pub survey_aps: Vec<crate::predict::PlannedAp>,
    /// Areas that absorb signal (shelving, cars, foliage...).
    #[serde(default)]
    pub areas: Vec<crate::obstacles::Area>,
    /// Where the optimiser may place access points and counts coverage, painted by the user.
    /// Empty means it works the area out from the walls.
    #[serde(default)]
    pub zone: Vec<crate::coverage::Stroke>,
}

impl Floor {
    /// The access points of the planning layer (`true`) or the survey layer.
    pub fn layer(&self, planning: bool) -> &Vec<crate::predict::PlannedAp> {
        if planning { &self.aps } else { &self.survey_aps }
    }

    pub fn layer_mut(&mut self, planning: bool) -> &mut Vec<crate::predict::PlannedAp> {
        if planning { &mut self.aps } else { &mut self.survey_aps }
    }

    /// An id no access point on this floor has.
    pub fn next_ap_id(&self) -> u64 {
        self.aps.iter().chain(&self.survey_aps).map(|a| a.id).max().unwrap_or(0) + 1
    }

    /// Plot an installed access point: it goes in the survey record and a copy
    /// goes in the plan, standing for the same device until it is moved there.
    pub fn plot_installed(&mut self, mut ap: crate::predict::PlannedAp) {
        ap.id = self.next_ap_id();
        ap.existing = true;
        ap.from = None;
        let copy = crate::predict::PlannedAp { id: ap.id + 1, from: Some(ap.id), ..ap.clone() };
        self.survey_aps.push(ap);
        self.aps.push(copy);
    }

    /// Bring projects saved before the two layers into the new shape: give
    /// every access point an id, and copy installed ones into the survey record.
    pub fn migrate_ap_layers(&mut self) {
        let mut next = self.next_ap_id();
        for a in self.aps.iter_mut().chain(self.survey_aps.iter_mut()).filter(|a| a.id == 0) {
            a.id = next;
            next += 1;
        }
        if self.survey_aps.is_empty() {
            let installed: Vec<_> = self.aps.iter().filter(|a| a.existing && a.from.is_none()).cloned().collect();
            for a in installed {
                let mut record = a.clone();
                record.id = next;
                next += 1;
                if let Some(plan) = self.aps.iter_mut().find(|p| p.id == a.id) {
                    plan.from = Some(record.id);
                }
                self.survey_aps.push(record);
            }
        }
    }
}

fn default_ceiling() -> f32 {
    crate::walls::DEFAULT_CEILING_M
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurveyPoint {
    /// Position in floor plan pixels.
    pub x: f32,
    pub y: f32,
    pub unix_secs: u64,
    pub dwell_secs: u64,
    /// Noise floor measured by the adapter during the dwell, when its driver
    /// reports one. Older projects and many drivers have none.
    #[serde(default)]
    pub noise_dbm: Option<f64>,
    pub observations: Vec<Observation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub bssid: String,
    pub ssid: Option<String>,
    pub freq_mhz: u32,
    pub channel: u32,
    pub width_mhz: u32,
    pub signal_dbm: f64,
    /// Scans in the dwell that heard this AP.
    pub samples: u32,
}

impl Observation {
    pub fn band(&self) -> Band {
        Band::from_freq(self.freq_mhz)
    }
}

impl SurveyPoint {
    /// Keeps only APs freshly heard during the dwell; entries the kernel
    /// merely had cached from earlier (possibly elsewhere) are dropped.
    pub fn from_scan(x: f32, y: f32, dwell_secs: u64, scan: &ScanResult) -> Self {
        let unix_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let observations = scan
            .networks
            .iter()
            .filter(|n| n.samples > 0)
            .filter_map(|n| {
                Some(Observation {
                    bssid: n.bssid.clone(),
                    ssid: n.ssid.clone(),
                    freq_mhz: n.freq_mhz,
                    channel: n.channel,
                    width_mhz: n.width_mhz,
                    signal_dbm: n.signal_dbm?,
                    samples: n.samples,
                })
            })
            .collect();
        SurveyPoint { x, y, unix_secs, dwell_secs, noise_dbm: scan.noise_dbm, observations }
    }
}

/// Identifies the physical access point behind a BSSID. One AP radio
/// broadcasts several SSIDs from BSSIDs that differ only in the last octet
/// and the locally-administered bit of the first, so those are ignored.
/// Falls back to the BSSID itself when it is not a MAC address.
pub fn ap_key(bssid: &str, band: Band) -> String {
    let parts: Vec<&str> = bssid.split(':').collect();
    if parts.len() != 6 {
        return format!("{bssid}/{band}");
    }
    let Ok(first) = u8::from_str_radix(parts[0], 16) else { return format!("{bssid}/{band}") };
    format!("{:02x}:{}:{}:{}:{}/{band}", first & !0x02, parts[1], parts[2], parts[3], parts[4]).to_lowercase()
}

/// Like [`ap_key`] but without the band, so an AP's radios on every band share it.
pub fn ap_base_key(bssid: &str) -> String {
    let key = ap_key(bssid, Band::Ghz5);
    key.rsplit_once('/').map_or(key.clone(), |(base, _)| base.to_string())
}

/// Which APs a heatmap is drawn for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Best signal from any AP.
    Any,
    /// Best signal from any AP broadcasting this SSID.
    Ssid(String),
    Bssid(String),
}

impl Floor {
    /// Best matching signal at a point, or [`MISSING_DBM`] if none was heard.
    pub fn signal_at(point: &SurveyPoint, target: &Target, band: Option<Band>) -> f64 {
        Self::signal_at_min(point, target, band, MISSING_DBM)
    }

    /// Like [`Floor::signal_at`], but readings weaker than `min_dbm` count as
    /// not heard.
    pub fn signal_at_min(point: &SurveyPoint, target: &Target, band: Option<Band>, min_dbm: f64) -> f64 {
        point
            .observations
            .iter()
            .filter(|o| o.signal_dbm >= min_dbm)
            .filter(|o| band.is_none_or(|b| o.band() == b))
            .filter(|o| match target {
                Target::Any => true,
                Target::Ssid(s) => o.ssid.as_deref() == Some(s.as_str()),
                Target::Bssid(b) => &o.bssid == b,
            })
            .map(|o| o.signal_dbm)
            .fold(MISSING_DBM, f64::max)
    }

    pub fn ssids(&self) -> BTreeSet<String> {
        self.points.iter().flat_map(|p| &p.observations).filter_map(|o| o.ssid.clone()).collect()
    }

    pub fn bssids(&self) -> BTreeSet<String> {
        self.points.iter().flat_map(|p| &p.observations).map(|o| o.bssid.clone()).collect()
    }
}

mod base64_bytes {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(bssid: &str, ssid: &str, freq: u32, dbm: f64) -> Observation {
        Observation {
            bssid: bssid.into(),
            ssid: Some(ssid.into()),
            freq_mhz: freq,
            channel: 1,
            width_mhz: 20,
            signal_dbm: dbm,
            samples: 3,
        }
    }

    fn point() -> SurveyPoint {
        SurveyPoint {
            x: 1.0,
            y: 2.0,
            unix_secs: 0,
            dwell_secs: 5,
            noise_dbm: None,
            observations: vec![
                obs("a", "Home", 2437, -60.0),
                obs("b", "Home", 5180, -50.0),
                obs("c", "Other", 5180, -40.0),
            ],
        }
    }

    fn blank(name: &str, mpp: Option<f32>) -> Floor {
        Floor { name: name.into(), image: vec![], width_px: 100, height_px: 50, metres_per_px: mpp, points: vec![], walls: vec![], ceiling_m: 3.0, placement: Placement::default(), aps: vec![], survey_aps: vec![], areas: vec![], zone: vec![] }
    }

    #[test]
    fn a_project_saved_with_one_floor_opens_as_the_first_floor() {
        let old = r#"{"version":1,"floor":{"name":"Old","image":"","width_px":1,"height_px":1,"metres_per_px":null,"points":[]}}"#;
        let p = Project::from_json(old).unwrap();
        assert_eq!((p.floors.len(), p.floor().name.as_str(), p.version), (1, "Old", Project::VERSION));
        let saved = p.to_json().unwrap();
        assert!(!saved.contains("\"floor\":"), "saved in the new shape: {saved}");
        assert_eq!(Project::from_json(&saved).unwrap().floors.len(), 1);
        assert!(Project::from_json(r#"{"version":2,"floors":[]}"#).is_err());
    }

    #[test]
    fn floors_are_added_on_top_and_the_last_one_cannot_be_removed() {
        let mut p = Project::new(blank("G", None));
        assert!(!p.remove_floor(0));
        p.add_floor(blank("1", None));
        assert_eq!((p.floors.len(), p.active, p.floor().name.as_str()), (2, 1, "1"));
        assert!(p.remove_floor(1));
        assert_eq!((p.floors.len(), p.active), (1, 0));
    }

    #[test]
    fn overlays_line_up_calibrated_floors_at_true_size() {
        let mut p = Project::new(blank("G", Some(0.05)));
        p.add_floor(blank("1", Some(0.10)));
        // A point on the upper floor is half as many ground-floor pixels away: it is twice as coarse.
        let on_ground = p.overlay_transform(1, 0).apply([100.0, 0.0]);
        assert!((on_ground[0] - 200.0).abs() < 1e-3, "{on_ground:?}");
        p.nudge_overlay(1, 0, [10.0, 5.0]);
        let moved = p.overlay_transform(1, 0).apply([0.0, 0.0]);
        assert!((moved[0] - 10.0).abs() < 1e-3 && (moved[1] - 5.0).abs() < 1e-3);
        let back = p.overlay_transform(0, 1).apply(moved);
        assert!(back[0].abs() < 1e-3 && back[1].abs() < 1e-3, "the reverse overlay undoes it: {back:?}");
    }

    #[test]
    fn turning_a_floor_keeps_its_centre_in_place() {
        let mut p = Project::new(blank("G", None));
        p.add_floor(blank("1", None));
        let before = p.overlay_transform(1, 0).apply([50.0, 25.0]);
        p.rotate_floor(1, 33.0);
        let after = p.overlay_transform(1, 0).apply([50.0, 25.0]);
        assert!((before[0] - after[0]).abs() < 1e-3 && (before[1] - after[1]).abs() < 1e-3);
        assert_eq!(p.floors[1].placement.rotation_deg, 33.0);
    }

    #[test]
    fn projects_saved_before_walls_still_open() {
        let old = r#"{"version":1,"floor":{"name":"x","image":"","width_px":1,"height_px":1,"metres_per_px":null,"points":[]}}"#;
        let p = Project::from_json(old).unwrap();
        assert!(p.floor().walls.is_empty());
        assert_eq!(p.floor().ceiling_m, crate::walls::DEFAULT_CEILING_M);
    }

    #[test]
    fn an_aps_radios_share_a_base_key_across_bands() {
        assert_eq!(ap_base_key("50:3d:d1:e7:4c:cc"), ap_base_key("52:3d:d1:e7:4c:ce"));
        assert!(!ap_base_key("50:3d:d1:e7:4c:cc").contains('/'));
    }

    #[test]
    fn plotting_an_installed_ap_also_puts_a_copy_in_the_plan_which_can_then_move() {
        let mut f = blank("G", None);
        f.plot_installed(crate::predict::PlannedAp::new("Reception".into(), 10.0, 20.0));
        assert_eq!((f.survey_aps.len(), f.aps.len()), (1, 1));
        let (record, copy) = (&f.survey_aps[0], &f.aps[0]);
        assert!(record.existing && copy.existing);
        assert_eq!(copy.from, Some(record.id));
        assert_ne!(copy.id, record.id);
        f.aps[0].x = 99.0;
        assert_eq!(f.survey_aps[0].x, 10.0, "moving the plan's copy leaves the survey record alone");
        assert!(f.layer(true)[0].x == 99.0 && f.layer(false)[0].x == 10.0);
        let again = f.next_ap_id();
        assert!(again > f.aps[0].id && again > f.survey_aps[0].id);
    }

    #[test]
    fn projects_saved_with_installed_aps_in_one_list_get_a_survey_record() {
        let mut f = blank("G", None);
        let mut installed = crate::predict::PlannedAp::new("Old".into(), 5.0, 5.0);
        installed.existing = true;
        let planned = crate::predict::PlannedAp::new("Idea".into(), 8.0, 8.0);
        f.aps = vec![installed, planned];
        f.migrate_ap_layers();
        assert_eq!(f.survey_aps.len(), 1, "only the installed one is a survey record");
        assert_eq!(f.survey_aps[0].name, "Old");
        assert_eq!(f.aps[0].from, Some(f.survey_aps[0].id));
        assert!(f.aps.iter().chain(&f.survey_aps).all(|a| a.id != 0));
        let ids: std::collections::BTreeSet<_> = f.aps.iter().chain(&f.survey_aps).map(|a| a.id).collect();
        assert_eq!(ids.len(), 3, "ids are unique");
        let mut again = f.clone();
        again.migrate_ap_layers();
        assert_eq!(again.survey_aps.len(), 1, "migrating twice changes nothing");
    }

    #[test]
    fn bssids_of_one_radio_share_a_key() {
        let key = |b: &str| ap_key(b, Band::Ghz5);
        assert_eq!(key("50:3d:d1:e7:4c:cc"), key("52:3d:d1:e7:4c:ce"), "other SSIDs of the same radio");
        assert_ne!(key("50:3d:d1:e7:4c:cc"), key("50:3d:d1:e7:4d:cc"));
        assert_ne!(ap_key("50:3d:d1:e7:4c:cc", Band::Ghz2_4), key("50:3d:d1:e7:4c:cc"), "bands are separate radios");
        assert_eq!(ap_key("weird", Band::Ghz5), "weird/5 GHz");
    }

    #[test]
    fn signal_filters_by_target_and_band() {
        let p = point();
        assert_eq!(Floor::signal_at(&p, &Target::Any, None), -40.0);
        assert_eq!(Floor::signal_at(&p, &Target::Ssid("Home".into()), None), -50.0);
        assert_eq!(Floor::signal_at(&p, &Target::Ssid("Home".into()), Some(Band::Ghz2_4)), -60.0);
        assert_eq!(Floor::signal_at(&p, &Target::Bssid("c".into()), Some(Band::Ghz2_4)), MISSING_DBM);
    }

    #[test]
    fn weak_readings_below_the_detection_level_are_ignored() {
        let p = point();
        assert_eq!(Floor::signal_at_min(&p, &Target::Ssid("Home".into()), Some(Band::Ghz2_4), -55.0), MISSING_DBM);
        assert_eq!(Floor::signal_at_min(&p, &Target::Ssid("Home".into()), None, -55.0), -50.0);
    }

    #[test]
    fn project_round_trips_through_json() {
        let floor = Floor {
            name: "Ground".into(),
            image: vec![0, 1, 2, 255],
            width_px: 10,
            height_px: 20,
            metres_per_px: Some(0.05),
            points: vec![point()],
            walls: vec![crate::walls::Wall::new([1.0, 2.0], [3.0, 4.0], crate::Material::Metal)],
            ceiling_m: 4.5,
            placement: Placement::default(),
            aps: vec![],
            survey_aps: vec![],
            areas: vec![],
            zone: vec![],
        };
        let back = Project::from_json(&Project::new(floor).to_json().unwrap()).unwrap();
        assert_eq!(back.floor().walls.len(), 1);
        assert_eq!(back.floor().ceiling_m, 4.5);
        assert_eq!(back.floor().image, vec![0, 1, 2, 255]);
        assert_eq!(back.floor().points[0].observations.len(), 3);
        assert_eq!(back.floor().metres_per_px, Some(0.05));
    }
}

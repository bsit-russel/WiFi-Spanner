// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! A catalogue of access point models, so that power and antenna gain need
//! not be entered by hand and a plan can use the bands a model really has.
//!
//! Only entries marked `verified` were checked against the maker's published
//! specifications. The rest are typical values for a class of device. Power
//! limits differ by country, so check any figure that matters. Models can be
//! added in the app and are kept in a file beside the settings.

use serde::{Deserialize, Serialize};

use crate::predict::{Radio, RadioBand};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApModel {
    pub vendor: String,
    pub name: String,
    /// Wi-Fi generation, mounting or other short description.
    #[serde(default)]
    pub note: String,
    pub radios: Vec<Radio>,
    /// Checked against the maker's specifications.
    #[serde(default)]
    pub verified: bool,
}

impl ApModel {
    /// The name shown in lists, and the key a plan stores.
    pub fn id(&self) -> String {
        format!("{} {}", self.vendor, self.name)
    }

    pub fn bands(&self) -> Vec<RadioBand> {
        self.radios.iter().map(|r| r.band).collect()
    }
}

fn radio(band: RadioBand, tx_dbm: f32, gain_dbi: f32) -> Radio {
    Radio { band, tx_dbm, gain_dbi, eirp_cap_dbm: None }
}

fn model(vendor: &str, name: &str, note: &str, radios: Vec<Radio>, verified: bool) -> ApModel {
    ApModel { vendor: vendor.into(), name: name.into(), note: note.into(), radios, verified }
}

/// Ubiquiti's access points, read from the specification pages on
/// techspecs.ui.com on 2026-10-29 (maximum transmit power and the antenna
/// gain of the built-in antenna, per band).
const UNIFI: &str = include_str!("../assets/unifi-models.json");

/// The models that come with the app.
pub fn builtin() -> Vec<ApModel> {
    let mut models: Vec<ApModel> = serde_json::from_str(UNIFI).expect("the bundled Ubiquiti models are valid");
    models.extend(generic());
    models
}

fn generic() -> Vec<ApModel> {
    use RadioBand::*;
    vec![
        // Typical values for a class of device.
        model("Generic", "Wi-Fi 5 dual-band ceiling AP", "Wi-Fi 5, 2.4 + 5 GHz", vec![radio(Ghz2_4, 20.0, 3.0), radio(Ghz5, 20.0, 4.0)], false),
        model("Generic", "Wi-Fi 6 dual-band ceiling AP", "Wi-Fi 6, 2.4 + 5 GHz", vec![radio(Ghz2_4, 22.0, 4.0), radio(Ghz5, 23.0, 5.0)], false),
        model("Generic", "Wi-Fi 6E / 7 tri-band ceiling AP", "Wi-Fi 6E or 7, 2.4 + 5 + 6 GHz", vec![radio(Ghz2_4, 22.0, 4.0), radio(Ghz5, 23.0, 5.0), radio(Ghz6, 20.0, 5.0)], false),
        model("Generic", "High-gain directional (warehouse)", "5 GHz, narrow beam for high bays", vec![radio(Ghz5, 23.0, 10.0)], false),
        model("Generic", "Outdoor omni", "2.4 + 5 GHz, outdoor", vec![radio(Ghz2_4, 23.0, 5.0), radio(Ghz5, 23.0, 6.0)], false),
        model("Generic", "Low-power wall plate", "Wi-Fi 5, 2.4 + 5 GHz, in-room", vec![radio(Ghz2_4, 17.0, 3.0), radio(Ghz5, 17.0, 3.0)], false),
    ]
}

/// Models matching every word of `query` in their vendor, name or note,
/// ignoring case. An empty query matches everything.
pub fn search<'a>(models: &'a [ApModel], query: &str) -> Vec<&'a ApModel> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    models
        .iter()
        .filter(|m| {
            let hay = format!("{} {} {}", m.vendor, m.name, m.note).to_lowercase();
            words.iter().all(|w| hay.contains(w))
        })
        .collect()
}

/// The built-in models followed by the user's own, where a user's model with
/// the same name replaces the built-in one.
pub fn merged(user: Vec<ApModel>) -> Vec<ApModel> {
    let mut all: Vec<ApModel> = builtin().into_iter().filter(|b| !user.iter().any(|u| u.id() == b.id())).collect();
    all.extend(user);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_needs_every_word_and_ignores_case() {
        let all = builtin();
        assert!(search(&all, "u7 PRO").iter().any(|m| m.name == "UniFi U7 Pro"));
        assert!(search(&all, "u7 pro").len() > 1, "the whole U7 Pro family matches");
        assert!(search(&all, "").len() == all.len());
        assert!(search(&all, "generic 6e").iter().all(|m| m.vendor == "Generic" && m.bands().contains(&RadioBand::Ghz6)));
        assert!(search(&all, "nonexistent").is_empty());
    }

    #[test]
    fn built_in_models_are_sane_and_only_checked_ones_say_so() {
        for m in builtin() {
            assert!(!m.radios.is_empty(), "{}", m.id());
            assert!(m.radios.iter().all(|r| (5.0..=36.0).contains(&r.tx_dbm) && (0.0..=20.0).contains(&r.gain_dbi)), "{}", m.id());
            let mut bands = m.bands();
            bands.dedup();
            assert_eq!(bands.len(), m.radios.len(), "one radio per band: {}", m.id());
        }
        assert!(builtin().iter().filter(|m| m.verified).all(|m| m.vendor != "Generic"));
    }

    #[test]
    fn the_ubiquiti_models_match_what_their_pages_say() {
        let all = builtin();
        let find = |name: &str| all.iter().find(|m| m.name == name).unwrap_or_else(|| panic!("{name}")).clone();
        let u7 = find("UniFi U7 Pro");
        assert_eq!(u7.radios.iter().map(|r| (r.band, r.tx_dbm, r.gain_dbi)).collect::<Vec<_>>(), vec![(RadioBand::Ghz2_4, 23.0, 4.0), (RadioBand::Ghz5, 26.0, 6.0), (RadioBand::Ghz6, 23.0, 5.8)]);
        assert!(find("UniFi U6 Pro").radios.iter().all(|r| r.band != RadioBand::Ghz6), "Wi-Fi 6 has no 6 GHz radio");
        let campus = find("UniFi E7 Campus");
        assert_eq!(campus.radios.iter().find(|r| r.band == RadioBand::Ghz6).unwrap().eirp_cap_dbm, Some(36.0), "its 30 dBm + 12 dBi is held to 36 dBm EIRP");
        assert!(all.iter().filter(|m| m.vendor == "Ubiquiti").count() >= 35);
        assert!(all.iter().filter(|m| m.vendor == "Ubiquiti").all(|m| m.verified));
    }

    #[test]
    fn a_users_model_replaces_a_built_in_of_the_same_name() {
        let mine = ApModel { radios: vec![Radio { band: RadioBand::Ghz5, tx_dbm: 10.0, gain_dbi: 1.0, eirp_cap_dbm: None }], ..builtin().remove(0) };
        let all = merged(vec![mine.clone()]);
        assert_eq!(all.iter().filter(|m| m.id() == mine.id()).count(), 1);
        assert_eq!(all.last().unwrap().radios.len(), 1);
    }

    #[test]
    fn models_round_trip_through_json() {
        let m = builtin().remove(0);
        let back: ApModel = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(m, back);
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! User settings, kept in `$XDG_CONFIG_HOME/wifispanner/settings.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use wifispanner_core::heatmap::{ColorScheme, RAMP_MAX, RAMP_MIN};

use crate::theme::{Density, Skin, ThemeMode};
use wifispanner_scan::iw::{WifiCard, default_card};

/// Selectable scan times in seconds: how long scanning continues before the
/// scans are collated into one reading.
pub const DWELLS: [u64; 3] = [3, 5, 10];

/// Limits of the signal sliders, in dBm.
pub const SIGNAL_FLOOR: f64 = -100.0;
pub const SIGNAL_CEIL: f64 = -10.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MarkerSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl MarkerSize {
    pub const ALL: [MarkerSize; 3] = [MarkerSize::Small, MarkerSize::Medium, MarkerSize::Large];

    pub fn name(self) -> &'static str {
        match self {
            MarkerSize::Small => "Small",
            MarkerSize::Medium => "Medium",
            MarkerSize::Large => "Large",
        }
    }

    /// Radius in screen points.
    pub fn radius(self) -> f32 {
        match self {
            MarkerSize::Small => 5.0,
            MarkerSize::Medium => 7.0,
            MarkerSize::Large => 10.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // General
    /// Light, dark, or follow the desktop.
    pub theme_mode: ThemeMode,
    /// Row height, text size and spacing of lists and tables.
    pub density: Density,
    /// The design the app wears: Material or Carbon.
    pub design: Skin,
    /// Look for a newer release on GitHub when the app starts.
    pub check_updates: bool,
    /// The one-time offer to add a launcher and icon has been shown.
    pub launcher_prompted: bool,
    /// Where projects are kept; `None` is the default folder.
    pub projects_dir: Option<PathBuf>,
    /// Size of the window when it was last used, restored on the next start.
    pub window_size: Option<[f32; 2]>,
    /// The screen the window size was saved on; a size is only trusted at
    /// startup when this is known, since the window cannot be moved back on
    /// screen afterwards.
    #[serde(default)]
    pub screen_size: Option<[f32; 2]>,
    // Scanner
    pub dwell_secs: u64,
    /// Primary card; `None` is Auto (the connected card, or the first).
    pub card: Option<String>,
    /// Scan with every card on its own radio at once.
    pub use_all: bool,
    // Survey
    pub show_plan: bool,
    pub show_path: bool,
    /// Shade areas far from any survey point grey instead of leaving them clear.
    pub gray_unmeasured: bool,
    pub marker_size: MarkerSize,
    pub show_numbers: bool,
    // Heatmap
    pub color_scheme: ColorScheme,
    pub min_dbm: f64,
    pub max_dbm: f64,
    /// Readings weaker than this count as not heard.
    pub detect_dbm: f64,
    // Channels
    /// Channel widths (MHz) the channel plan uses on each band, and whether it may use DFS channels.
    pub plan_width_2_4: u32,
    pub plan_width_5: u32,
    pub plan_width_6: u32,
    pub allow_dfs: bool,
    // Input
    /// Pen mode: one finger on the touch screen moves the plan and never draws; the pen draws.
    pub pen_mode: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme_mode: ThemeMode::System,
            density: Density::Default,
            design: Skin::Material,
            check_updates: true,
            launcher_prompted: false,
            projects_dir: None,
            window_size: None,
            screen_size: None,
            dwell_secs: 3,
            card: None,
            use_all: false,
            show_plan: true,
            show_path: false,
            gray_unmeasured: true,
            marker_size: MarkerSize::Medium,
            show_numbers: true,
            color_scheme: ColorScheme::Classic,
            min_dbm: RAMP_MIN,
            max_dbm: RAMP_MAX,
            detect_dbm: SIGNAL_FLOOR,
            plan_width_2_4: 20,
            plan_width_5: 40,
            plan_width_6: 80,
            allow_dfs: false,
            pen_mode: false,
        }
    }
}

impl Settings {
    /// The channel widths and DFS choice for the channel plan.
    pub fn plan_options(&self) -> wifispanner_core::channels::PlanOptions {
        wifispanner_core::channels::PlanOptions { width_2_4: self.plan_width_2_4, width_5: self.plan_width_5, width_6: self.plan_width_6, allow_dfs: self.allow_dfs }
    }

    fn path() -> PathBuf {
        crate::store::config_dir().join("settings.json")
    }

    pub fn load() -> Settings {
        let mut s: Settings = std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        if !DWELLS.contains(&s.dwell_secs) {
            s.dwell_secs = Settings::default().dwell_secs;
        }
        s
    }

    /// Best effort: a failure to save should never interrupt a survey.
    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, text);
        }
    }

    /// The primary card: the chosen one if present, else the default.
    pub fn primary<'a>(&self, cards: &'a [WifiCard]) -> Option<&'a WifiCard> {
        self.card.as_ref().and_then(|n| cards.iter().find(|c| &c.name == n)).or_else(|| default_card(cards))
    }

    /// Interface names to scan with: the primary card, plus with `use_all`
    /// every other card that sits on a different physical radio (two cards on
    /// one radio could not scan at the same time).
    pub fn active_cards(&self, cards: &[WifiCard]) -> Vec<String> {
        let Some(primary) = self.primary(cards) else { return Vec::new() };
        let mut chosen = vec![primary];
        if self.use_all {
            for c in cards {
                if chosen.iter().all(|o| o.phy != c.phy) {
                    chosen.push(c);
                }
            }
        }
        chosen.into_iter().map(|c| c.name.clone()).collect()
    }

    /// Number of separate physical radios among `cards`.
    pub fn radio_count(cards: &[WifiCard]) -> usize {
        cards.iter().map(|c| &c.phy).collect::<std::collections::HashSet<_>>().len()
    }

    pub fn reset_survey(&mut self) {
        let d = Settings::default();
        self.show_plan = d.show_plan;
        self.show_path = d.show_path;
        self.gray_unmeasured = d.gray_unmeasured;
        self.marker_size = d.marker_size;
        self.show_numbers = d.show_numbers;
    }

    pub fn reset_heatmap(&mut self) {
        let d = Settings::default();
        self.color_scheme = d.color_scheme;
        self.min_dbm = d.min_dbm;
        self.max_dbm = d.max_dbm;
        self.detect_dbm = d.detect_dbm;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(name: &str, phy: &str, connected: bool) -> WifiCard {
        WifiCard { name: name.into(), phy: phy.into(), driver: None, connected }
    }

    #[test]
    fn auto_is_the_connected_card() {
        let cards = [card("wlan0", "phy#0", false), card("wlan1", "phy#1", true)];
        assert_eq!(Settings::default().active_cards(&cards), vec!["wlan1"]);
    }

    #[test]
    fn a_missing_chosen_card_falls_back_to_the_default() {
        let cards = [card("wlan0", "phy#0", true)];
        let s = Settings { card: Some("gone".into()), ..Default::default() };
        assert_eq!(s.active_cards(&cards), vec!["wlan0"]);
    }

    #[test]
    fn use_all_adds_each_other_radio_once() {
        let cards = [card("a", "phy#0", true), card("c", "phy#0", false), card("b", "phy#1", false)];
        let mut s = Settings::default();
        assert_eq!(s.active_cards(&cards), vec!["a"], "off by default");
        s.use_all = true;
        assert_eq!(s.active_cards(&cards), vec!["a", "b"], "c shares a's radio");
        assert_eq!(Settings::radio_count(&cards), 2);
    }

    #[test]
    fn a_removed_scan_time_falls_back_to_the_default() {
        let dir = std::env::temp_dir().join(format!("ws-settings-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("wifispanner")).unwrap();
        std::fs::write(dir.join("wifispanner/settings.json"), "{\"dwell_secs\": 1}").unwrap();
        // Safety: set before any other thread in this test reads the variable.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };
        assert_eq!(Settings::load().dwell_secs, Settings::default().dwell_secs);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn old_or_partial_files_still_load() {
        let s: Settings = serde_json::from_str("{\"dwell_secs\": 5, \"selected_cards\": [\"x\"]}").unwrap();
        assert_eq!((s.dwell_secs, s.use_all, s.show_plan), (5, false, true));
    }

    #[test]
    fn resets_only_touch_their_own_section() {
        let mut s = Settings { show_path: true, min_dbm: -80.0, dwell_secs: 10, ..Default::default() };
        s.reset_survey();
        assert!(!s.show_path && s.min_dbm == -80.0);
        s.reset_heatmap();
        assert_eq!((s.min_dbm, s.dwell_secs), (RAMP_MIN, 10));
    }
}

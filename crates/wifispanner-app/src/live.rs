// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! Real-time analysis tab: the collated network table.

use eframe::egui;
use wifispanner_scan::ScanResult;

pub fn show(ui: &mut egui::Ui, latest: Option<&ScanResult>) {
    let Some(r) = latest else {
        ui.centered_and_justified(|ui| ui.label(egui::RichText::new("Waiting for first scan...").size(18.0)));
        return;
    };
    let mut nets: Vec<_> = r.networks.iter().collect();
    nets.sort_by(|a, b| b.signal_dbm.partial_cmp(&a.signal_dbm).unwrap_or(std::cmp::Ordering::Equal));

    egui::ScrollArea::both().show(ui, |ui| {
        crate::theme::data_table(ui, "networks", &["SSID", "BSSID", "Ch", "Band", "Width", "Standard", "Security", "Signal", "Seen", "Util"], 16.0, |ui| {
            use crate::theme::cell;
            for n in nets {
                let ssid = n.ssid.as_deref().unwrap_or("<hidden>");
                cell(ui, |ui| {
                    if n.associated {
                        ui.strong(format!("{ssid} (connected)"));
                    } else {
                        ui.label(ssid);
                    }
                });
                cell(ui, |ui| ui.monospace(&n.bssid));
                cell(ui, |ui| ui.label(n.channel.to_string()));
                cell(ui, |ui| ui.label(n.band.to_string()));
                cell(ui, |ui| ui.label(format!("{} MHz", n.width_mhz)));
                cell(ui, |ui| ui.label(n.standard.to_string()));
                cell(ui, |ui| ui.label(n.security.to_string()));
                cell(ui, |ui| match n.signal_dbm {
                    Some(s) => ui.colored_label(signal_color(s, ui.visuals().dark_mode), format!("{s:.0} dBm")),
                    None => ui.label("-"),
                });
                cell(ui, |ui| ui.label(if n.samples == 0 { "stale".to_string() } else { n.samples.to_string() }));
                cell(ui, |ui| ui.label(n.channel_util_pct.map_or("-".into(), |u| format!("{u:.0}%"))));
                ui.end_row();
            }
        });
    });
}

/// Signal strength colour, with darker shades on a light background so the
/// numbers stay readable.
pub fn signal_color(dbm: f64, dark: bool) -> egui::Color32 {
    let (good, ok, weak, bad) = if dark {
        ((80, 200, 120), (230, 200, 60), (240, 140, 50), (230, 80, 80))
    } else {
        ((0, 120, 62), (140, 100, 0), (184, 80, 0), (186, 26, 26))
    };
    let (r, g, b) = match dbm {
        d if d >= -60.0 => good,
        d if d >= -70.0 => ok,
        d if d >= -80.0 => weak,
        _ => bad,
    };
    egui::Color32::from_rgb(r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_colours_differ_by_strength_and_by_theme() {
        for dark in [true, false] {
            let all = [-50.0, -65.0, -75.0, -90.0].map(|d| signal_color(d, dark));
            assert_eq!(all.iter().collect::<std::collections::HashSet<_>>().len(), 4, "four distinct bands (dark={dark})");
        }
        assert_ne!(signal_color(-50.0, true), signal_color(-50.0, false));
    }
}

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
    use crate::table::{Cell, Column};
    let cols = [
        Column { name: "SSID", desc_first: false },
        Column { name: "BSSID", desc_first: false },
        Column { name: "Ch", desc_first: false },
        Column { name: "Band", desc_first: false },
        Column { name: "Width", desc_first: true },
        Column { name: "Standard", desc_first: false },
        Column { name: "Security", desc_first: false },
        Column { name: "Signal", desc_first: true },
        Column { name: "Seen", desc_first: true },
        Column { name: "Util", desc_first: true },
    ];
    let dark = ui.visuals().dark_mode;
    let rows: Vec<Vec<Cell>> = r
        .networks
        .iter()
        .map(|n| {
            let ssid = n.ssid.as_deref().unwrap_or("<hidden>");
            let name = if n.associated { Cell::text(format!("{ssid} (connected)")).strong() } else { Cell::text(ssid) };
            let signal = match n.signal_dbm {
                Some(s) => Cell::number(Some(s), format!("{s:.0} dBm")).colour(signal_color(s, dark)),
                None => Cell::number(None, "-"),
            };
            vec![
                name,
                Cell::text(&n.bssid).mono(),
                Cell::number(Some(n.channel as f64), n.channel.to_string()),
                Cell::text(n.band.to_string()),
                Cell::number(Some(n.width_mhz as f64), format!("{} MHz", n.width_mhz)),
                Cell::text(n.standard.to_string()),
                Cell::text(n.security.to_string()),
                signal,
                Cell::number(Some(n.samples as f64), if n.samples == 0 { "stale".to_string() } else { n.samples.to_string() }),
                Cell::number(n.channel_util_pct.map(|u| u as f64), n.channel_util_pct.map_or("-".into(), |u| format!("{u:.0}%"))),
            ]
        })
        .collect();
    // The page's margin is on the left and top only, so the scroll bar sits at the window's edge.
    egui::Frame::new().inner_margin(egui::Margin { left: 16, top: 16, right: 0, bottom: 0 }).show(ui, |ui| {
        crate::table::show(ui, "networks", &cols, Some(0), &[(7, true)], rows);
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

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! Fake scanner for developing without Wi-Fi hardware.

use std::time::Duration;

use crate::{Band, Network, Result, ScanResult, Scanner, Security, Standard};

#[derive(Default)]
pub struct MockScanner {
    tick: u64,
}

struct Fake {
    ssid: Option<&'static str>,
    freq: u32,
    base_dbm: f64,
    width: u32,
    standard: Standard,
    security: Security,
    util: Option<f32>,
}

const FAKES: &[Fake] = &[
    Fake { ssid: Some("Spanner-Office"), freq: 5180, base_dbm: -48.0, width: 80, standard: Standard::Wifi6, security: Security::Wpa3, util: Some(18.0) },
    Fake { ssid: Some("Spanner-Office"), freq: 2437, base_dbm: -55.0, width: 20, standard: Standard::Wifi6, security: Security::Wpa3, util: Some(42.0) },
    Fake { ssid: Some("Spanner-Office"), freq: 6055, base_dbm: -61.0, width: 160, standard: Standard::Wifi7, security: Security::Wpa3, util: Some(4.0) },
    Fake { ssid: Some("Neighbour_5G"), freq: 5745, base_dbm: -72.0, width: 80, standard: Standard::Wifi5, security: Security::Wpa2, util: None },
    Fake { ssid: Some("Neighbour_2G"), freq: 2462, base_dbm: -79.0, width: 20, standard: Standard::Wifi4, security: Security::Wpa2, util: None },
    Fake { ssid: Some("Guest"), freq: 5240, base_dbm: -66.0, width: 40, standard: Standard::Wifi5, security: Security::Open, util: Some(9.0) },
    Fake { ssid: None, freq: 2412, base_dbm: -84.0, width: 20, standard: Standard::Legacy, security: Security::Wpa2, util: None },
];

impl Scanner for MockScanner {
    fn name(&self) -> String {
        "mock".to_string()
    }

    fn scan(&mut self) -> Result<ScanResult> {
        self.tick += 1;
        std::thread::sleep(Duration::from_millis(300));

        let networks: Vec<Network> = FAKES
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let jitter = ((self.tick * 7 + i as u64 * 13) % 7) as f64 - 3.0;
                let band = Band::from_freq(f.freq);
                Network {
                    bssid: format!("02:00:00:00:00:{:02x}", i + 1),
                    ssid: f.ssid.map(str::to_string),
                    associated: i == 0,
                    freq_mhz: f.freq,
                    channel: band.channel(f.freq),
                    band,
                    signal_dbm: Some(f.base_dbm + jitter),
                    age_ms: Some(100),
                    width_mhz: f.width,
                    standard: f.standard,
                    security: f.security,
                    station_count: f.util.map(|_| 3 + i as u32),
                    channel_util_pct: f.util,
                    samples: 1,
                }
            })
            .collect();

        Ok(ScanResult {
            interface: "mock0".to_string(),
            fresh_count: networks.len(),
            networks,
            noise_dbm: Some(-94.0),
            channel_busy_pct: Some(12.0),
            scan_ms: 300,
            notes: vec!["mock data, not a real scan".to_string()],
        })
    }
}

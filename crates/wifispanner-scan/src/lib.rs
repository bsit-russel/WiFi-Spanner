// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! Wi-Fi scanning backends for WiFi Spanner.
//!
//! The parsers are platform-independent so they can be tested anywhere;
//! only [`default_scanner`] picks a backend by operating system.

use std::fmt;

pub use anyhow::Result;

pub mod dwell;
pub mod iw;
pub mod mock;
pub mod parse;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Band {
    Ghz2_4,
    Ghz5,
    Ghz6,
}

impl Band {
    pub fn from_freq(mhz: u32) -> Band {
        match mhz {
            0..=2999 => Band::Ghz2_4,
            3000..=5924 => Band::Ghz5,
            _ => Band::Ghz6,
        }
    }

    pub fn channel(self, mhz: u32) -> u32 {
        match self {
            Band::Ghz2_4 if mhz == 2484 => 14,
            Band::Ghz2_4 => mhz.saturating_sub(2407) / 5,
            Band::Ghz5 => mhz.saturating_sub(5000) / 5,
            Band::Ghz6 => mhz.saturating_sub(5950) / 5,
        }
    }
}

impl fmt::Display for Band {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            Band::Ghz2_4 => "2.4 GHz",
            Band::Ghz5 => "5 GHz",
            Band::Ghz6 => "6 GHz",
        })
    }
}

/// Highest 802.11 generation advertised by an access point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Standard {
    Legacy,
    Wifi4,
    Wifi5,
    Wifi6,
    Wifi7,
}

impl fmt::Display for Standard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            Standard::Legacy => "a/b/g",
            Standard::Wifi4 => "Wi-Fi 4",
            Standard::Wifi5 => "Wi-Fi 5",
            Standard::Wifi6 => "Wi-Fi 6",
            Standard::Wifi7 => "Wi-Fi 7",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    Open,
    Wep,
    Wpa,
    Wpa2,
    Wpa3,
}

impl fmt::Display for Security {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            Security::Open => "Open",
            Security::Wep => "WEP",
            Security::Wpa => "WPA",
            Security::Wpa2 => "WPA2",
            Security::Wpa3 => "WPA3",
        })
    }
}

/// One access point (BSSID) seen in a scan.
#[derive(Debug, Clone)]
pub struct Network {
    pub bssid: String,
    /// `None` for hidden networks.
    pub ssid: Option<String>,
    /// True when this is the AP the adapter is currently connected to.
    pub associated: bool,
    pub freq_mhz: u32,
    pub channel: u32,
    pub band: Band,
    pub signal_dbm: Option<f64>,
    /// Milliseconds since the kernel last heard this AP.
    pub age_ms: Option<u64>,
    pub width_mhz: u32,
    pub standard: Standard,
    pub security: Security,
    /// From the AP's BSS Load element, when advertised.
    pub station_count: Option<u32>,
    pub channel_util_pct: Option<f32>,
    /// Scans that heard this AP afresh. 1 for a single scan, 0 when a dwell
    /// only has a stale cache entry for it.
    pub samples: u32,
}

#[derive(Debug, Clone, Default)]
pub struct ScanResult {
    pub interface: String,
    pub networks: Vec<Network>,
    /// Noise floor on the channel currently in use.
    pub noise_dbm: Option<f64>,
    /// Airtime busy percentage on the channel currently in use.
    pub channel_busy_pct: Option<f32>,
    /// How long the scan took from trigger to settled results.
    pub scan_ms: u64,
    /// Networks whose data was refreshed by this scan.
    pub fresh_count: usize,
    pub notes: Vec<String>,
}

pub trait Scanner: Send {
    fn name(&self) -> String;
    fn scan(&mut self) -> Result<ScanResult>;
}

/// Wi-Fi cards available for scanning. Non-Linux builds get one mock card.
pub fn list_cards() -> Result<Vec<iw::WifiCard>> {
    #[cfg(target_os = "linux")]
    {
        iw::list_cards()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(vec![iw::WifiCard { name: "mock0".into(), phy: "phy#0".into(), driver: None, connected: true }])
    }
}

/// The real scanner on Linux; a mock elsewhere so the UI can be developed
/// on any machine.
pub fn default_scanner(interface: Option<String>) -> Result<Box<dyn Scanner>> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(iw::IwScanner::detect(interface)?))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = interface;
        Ok(Box::new(mock::MockScanner::default()))
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! Parsers for `iw` text output.

use crate::{Band, Network, Security, Standard};

/// Per-channel statistics from `iw dev <if> survey dump`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Survey {
    pub freq_mhz: u32,
    pub in_use: bool,
    pub noise_dbm: Option<f64>,
    pub active_ms: Option<u64>,
    pub busy_ms: Option<u64>,
}

impl Survey {
    pub fn busy_pct(&self) -> Option<f32> {
        match (self.busy_ms, self.active_ms) {
            (Some(busy), Some(active)) if active > 0 => Some(busy as f32 * 100.0 / active as f32),
            _ => None,
        }
    }
}

/// First interface name listed by `iw dev`.
/// A station-mode Wi-Fi interface from `iw dev`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceInfo {
    pub name: String,
    /// The physical radio, e.g. `phy#0`. Interfaces sharing one cannot scan
    /// at the same time.
    pub phy: String,
}

/// All `managed` (station) interfaces. Monitor, AP and P2P-device entries
/// cannot be used for site surveys and are skipped.
pub fn interfaces(text: &str) -> Vec<InterfaceInfo> {
    let (mut out, mut phy) = (Vec::new(), String::new());
    let mut current: Option<String> = None;
    let flush = |name: &mut Option<String>, kind: Option<&str>, phy: &str, out: &mut Vec<InterfaceInfo>| {
        if let (Some(n), Some("managed")) = (name.take(), kind) {
            out.push(InterfaceInfo { name: n, phy: phy.to_string() });
        }
    };
    let mut kind: Option<String> = None;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("phy#") {
            flush(&mut current, kind.take().as_deref(), &phy, &mut out);
            phy = t.to_string();
        } else if let Some(name) = t.strip_prefix("Interface ") {
            flush(&mut current, kind.take().as_deref(), &phy, &mut out);
            current = Some(name.trim().to_string());
        } else if t.starts_with("Unnamed/non-netdev") {
            flush(&mut current, kind.take().as_deref(), &phy, &mut out);
        } else if let Some(k) = t.strip_prefix("type ") {
            kind = Some(k.trim().to_string());
        }
    }
    flush(&mut current, kind.as_deref(), &phy, &mut out);
    out
}

pub fn first_interface(text: &str) -> Option<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("Interface "))
        .map(|name| name.trim().to_string())
        .next()
}

struct Acc {
    net: Network,
    rsn: bool,
    wpa: bool,
    sae: bool,
    privacy: bool,
    ht: bool,
    vht: bool,
    he: bool,
    eht: bool,
}

impl Acc {
    fn new(bssid: String, associated: bool) -> Self {
        Acc {
            net: Network {
                bssid,
                ssid: None,
                associated,
                freq_mhz: 0,
                channel: 0,
                band: Band::Ghz2_4,
                signal_dbm: None,
                age_ms: None,
                width_mhz: 20,
                standard: Standard::Legacy,
                security: Security::Open,
                station_count: None,
                channel_util_pct: None,
                samples: 1,
            },
            rsn: false,
            wpa: false,
            sae: false,
            privacy: false,
            ht: false,
            vht: false,
            he: false,
            eht: false,
        }
    }

    fn finish(mut self) -> Network {
        let n = &mut self.net;
        n.band = Band::from_freq(n.freq_mhz);
        n.channel = n.band.channel(n.freq_mhz);
        n.standard = if self.eht {
            Standard::Wifi7
        } else if self.he {
            Standard::Wifi6
        } else if self.vht {
            Standard::Wifi5
        } else if self.ht {
            Standard::Wifi4
        } else {
            Standard::Legacy
        };
        if n.band == Band::Ghz6 {
            n.standard = n.standard.max(Standard::Wifi6);
        }
        n.security = if self.sae {
            Security::Wpa3
        } else if self.rsn {
            Security::Wpa2
        } else if self.wpa {
            Security::Wpa
        } else if self.privacy {
            Security::Wep
        } else {
            Security::Open
        };
        self.net
    }
}

/// Parse `iw dev <if> scan dump` (or `scan`) output.
pub fn parse_scan_dump(text: &str) -> Vec<Network> {
    let mut out = Vec::new();
    let mut cur: Option<Acc> = None;

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("BSS ") {
            if let Some(acc) = cur.take() {
                out.push(acc.finish());
            }
            let bssid = rest.split(['(', ' ']).next().unwrap_or("").to_lowercase();
            cur = Some(Acc::new(bssid, rest.contains("-- associated")));
            continue;
        }
        let Some(acc) = cur.as_mut() else { continue };
        let t = line.trim();

        if let Some(v) = t.strip_prefix("freq:") {
            if let Ok(f) = v.trim().parse::<f64>() {
                acc.net.freq_mhz = f.round() as u32;
            }
        } else if let Some(v) = t.strip_prefix("signal:") {
            if v.contains("dBm") {
                acc.net.signal_dbm = v.trim().split_whitespace().next().and_then(|s| s.parse().ok());
            }
        } else if let Some(v) = t.strip_prefix("last seen:") {
            if let Some(ms) = v.trim().strip_suffix("ms ago") {
                acc.net.age_ms = ms.trim().parse().ok();
            }
        } else if line.trim_start().starts_with("SSID:") {
            let v = line.trim_start().trim_start_matches("SSID:");
            let v = v.strip_prefix(' ').unwrap_or(v);
            acc.net.ssid = if v.replace("\\x00", "").is_empty() { None } else { Some(v.to_string()) };
        } else if t.starts_with("capability:") {
            acc.privacy = t.contains("Privacy");
        } else if t.starts_with("RSN:") {
            acc.rsn = true;
        } else if t.starts_with("WPA:") {
            acc.wpa = true;
        } else if t.starts_with("* Authentication suites:") {
            acc.sae |= t.contains("SAE");
        } else if t.starts_with("HT capabilities:") {
            acc.ht = true;
        } else if t.starts_with("VHT capabilities:") {
            acc.vht = true;
        } else if t.starts_with("HE capabilities:") {
            acc.he = true;
        } else if t.starts_with("EHT capabilities:") {
            acc.eht = true;
        } else if t.starts_with("* secondary channel offset:") {
            if t.ends_with("above") || t.ends_with("below") {
                acc.net.width_mhz = acc.net.width_mhz.max(40);
            }
        } else if t.starts_with("* channel width:") {
            if let Some(w) = mhz_in_parens(t) {
                acc.net.width_mhz = acc.net.width_mhz.max(w);
            }
        } else if let Some(v) = t.strip_prefix("* station count:") {
            acc.net.station_count = v.trim().parse().ok();
        } else if let Some(v) = t.strip_prefix("* channel utilisation:") {
            // Reported as "<n>/255".
            acc.net.channel_util_pct = v
                .trim()
                .split('/')
                .next()
                .and_then(|n| n.parse::<f32>().ok())
                .map(|n| n * 100.0 / 255.0);
        }
    }
    if let Some(acc) = cur {
        out.push(acc.finish());
    }
    out
}

/// "1 (80 MHz)" -> 80; "0 (20 or 40 MHz)" -> 40.
fn mhz_in_parens(t: &str) -> Option<u32> {
    let before = &t[..t.find("MHz")?];
    before.split_whitespace().last()?.trim_start_matches('(').parse().ok()
}

/// Parse `iw dev <if> survey dump` output.
pub fn parse_survey_dump(text: &str) -> Vec<Survey> {
    let mut out: Vec<Survey> = Vec::new();
    for line in text.lines() {
        if line.starts_with("Survey data") {
            out.push(Survey::default());
            continue;
        }
        let Some(s) = out.last_mut() else { continue };
        let t = line.trim();
        let num = |key: &str| t.strip_prefix(key).and_then(|v| v.split_whitespace().next());
        if let Some(v) = num("frequency:") {
            s.freq_mhz = v.parse().unwrap_or(0);
            s.in_use = t.contains("[in use]");
        } else if let Some(v) = num("noise:") {
            s.noise_dbm = v.parse().ok();
        } else if let Some(v) = num("channel active time:") {
            s.active_ms = v.parse().ok();
        } else if let Some(v) = num("channel busy time:") {
            s.busy_ms = v.parse().ok();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCAN: &str = "BSS 74:ac:b9:11:22:33(on wlp2s0) -- associated
\tlast seen: 1234.5s [boottime]
\tTSF: 1 usec
\tfreq: 5180.0
\tbeacon interval: 100 TUs
\tcapability: ESS Privacy SpectrumMgmt (0x1111)
\tsignal: -52.00 dBm
\tlast seen: 120 ms ago
\tSSID: Home Net
\tDS Parameter set: channel 36
\tHT capabilities:
\t\tCapabilities: 0x19ef
\tHT operation:
\t\t * primary channel: 36
\t\t * secondary channel offset: above
\t\t * STA channel width: any
\tVHT capabilities:
\t\tVHT Capabilities (0x338001b2):
\tVHT operation:
\t\t * channel width: 1 (80 MHz)
\tRSN:\t * Version: 1
\t\t * Authentication suites: PSK SAE
\tBSS Load:
\t\t * station count: 3
\t\t * channel utilisation: 51/255
\t\t * available admission capacity: 0 [*32us]
\tHE capabilities:
\t\tHE MAC Capabilities (0x000800200010):
BSS 0a:00:00:00:00:01(on wlp2s0)
\tfreq: 2437
\tcapability: ESS (0x0401)
\tsignal: -81.00 dBm
\tlast seen: 9000 ms ago
\tSSID:
\tDS Parameter set: channel 6
";

    #[test]
    fn parses_scan_dump() {
        let nets = parse_scan_dump(SCAN);
        assert_eq!(nets.len(), 2);

        let a = &nets[0];
        assert_eq!(a.bssid, "74:ac:b9:11:22:33");
        assert!(a.associated);
        assert_eq!(a.ssid.as_deref(), Some("Home Net"));
        assert_eq!((a.freq_mhz, a.channel, a.band), (5180, 36, Band::Ghz5));
        assert_eq!(a.signal_dbm, Some(-52.0));
        assert_eq!(a.age_ms, Some(120));
        assert_eq!(a.width_mhz, 80);
        assert_eq!(a.standard, Standard::Wifi6);
        assert_eq!(a.security, Security::Wpa3);
        assert_eq!(a.station_count, Some(3));
        assert!((a.channel_util_pct.unwrap() - 20.0).abs() < 0.1);

        let b = &nets[1];
        assert_eq!(b.ssid, None);
        assert_eq!((b.channel, b.band), (6, Band::Ghz2_4));
        assert_eq!(b.security, Security::Open);
        assert_eq!(b.standard, Standard::Legacy);
    }

    #[test]
    fn six_ghz_channels() {
        assert_eq!(Band::from_freq(5955), Band::Ghz6);
        assert_eq!(Band::Ghz6.channel(5955), 1);
        assert_eq!(Band::Ghz2_4.channel(2484), 14);
    }

    #[test]
    fn parses_survey() {
        let text = "Survey data from wlp2s0
\tfrequency:\t\t\t5180 MHz [in use]
\tnoise:\t\t\t\t-95 dBm
\tchannel active time:\t\t1000 ms
\tchannel busy time:\t\t250 ms
Survey data from wlp2s0
\tfrequency:\t\t\t2412 MHz
";
        let s = parse_survey_dump(text);
        assert_eq!(s.len(), 2);
        assert!(s[0].in_use && s[0].freq_mhz == 5180);
        assert_eq!(s[0].noise_dbm, Some(-95.0));
        assert_eq!(s[0].busy_pct(), Some(25.0));
        assert!(!s[1].in_use);
    }

    #[test]
    fn finds_interface() {
        let text = "phy#0\n\tInterface wlp2s0\n\t\tifindex 3\n";
        assert_eq!(first_interface(text).as_deref(), Some("wlp2s0"));
    }

    #[test]
    fn lists_only_station_interfaces_with_their_radio() {
        let text = "phy#1\n\tInterface wlan1\n\t\tifindex 5\n\t\ttype managed\nphy#0\n\tUnnamed/non-netdev interface\n\t\ttype P2P-device\n\tInterface wlp2s0\n\t\tifindex 2\n\t\ttype managed\n\tInterface mon0\n\t\ttype monitor\n";
        let got = interfaces(text);
        assert_eq!(
            got,
            vec![
                InterfaceInfo { name: "wlan1".into(), phy: "phy#1".into() },
                InterfaceInfo { name: "wlp2s0".into(), phy: "phy#0".into() },
            ]
        );
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! Phase 0 spike: trigger a scan and print what the radio sees.
//!
//! Usage: spike [INTERFACE] [--mock] [--watch] [--max-age SECONDS]

use anyhow::Result;
use wifispanner_scan::{ScanResult, Scanner, default_scanner, iw, mock::MockScanner};

fn main() -> Result<()> {
    let mut iface = None;
    let (mut mock, mut watch, mut max_age_s) = (false, false, 20u64);

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--mock" => mock = true,
            "--watch" => watch = true,
            "--max-age" => max_age_s = args.next().and_then(|v| v.parse().ok()).unwrap_or(max_age_s),
            "-h" | "--help" => {
                println!("Usage: spike [INTERFACE] [--mock] [--watch] [--max-age SECONDS]");
                return Ok(());
            }
            _ if a.starts_with('-') => anyhow::bail!("unknown option `{a}` (try --help)"),
            _ => {
                wifispanner_scan::iw::check_interface_name(&a)?;
                iface = Some(a);
            }
        }
    }

    let mut scanner: Box<dyn Scanner> =
        if mock { Box::new(MockScanner::default()) } else { default_scanner(iface)? };

    println!("scanner: {}", scanner.name());
    if let Some(reg) = iw::regulatory_domain() {
        println!("regulatory domain: {reg}");
    }

    loop {
        let result = scanner.scan()?;
        print_result(&result, max_age_s * 1000);
        if !watch {
            return Ok(());
        }
    }
}

fn print_result(r: &ScanResult, max_age_ms: u64) {
    println!();
    println!(
        "{}: scan settled in {} ms, {} refreshed, noise {}, channel busy {}",
        r.interface,
        r.scan_ms,
        r.fresh_count,
        r.noise_dbm.map_or("n/a".into(), |n| format!("{n:.0} dBm")),
        r.channel_busy_pct.map_or("n/a".into(), |b| format!("{b:.0}%")),
    );
    for note in &r.notes {
        println!("note: {note}");
    }

    let mut nets: Vec<_> = r
        .networks
        .iter()
        .filter(|n| n.age_ms.is_none_or(|a| a <= max_age_ms))
        .collect();
    nets.sort_by(|a, b| b.signal_dbm.partial_cmp(&a.signal_dbm).unwrap_or(std::cmp::Ordering::Equal));

    println!(
        "{:<2} {:<26} {:<17} {:>3} {:<7} {:>5} {:<8} {:<5} {:>7} {:>6} {:>4} {:>5}",
        "", "SSID", "BSSID", "CH", "BAND", "WIDTH", "STD", "SEC", "dBm", "AGE s", "STA", "UTIL"
    );
    for n in nets {
        println!(
            "{:<2} {:<26} {:<17} {:>3} {:<7} {:>5} {:<8} {:<5} {:>7} {:>6} {:>4} {:>5}",
            if n.associated { "*" } else { "" },
            n.ssid.as_deref().unwrap_or("<hidden>"),
            n.bssid,
            n.channel,
            n.band,
            n.width_mhz,
            n.standard,
            n.security,
            n.signal_dbm.map_or("-".into(), |s| format!("{s:.0}")),
            n.age_ms.map_or("-".into(), |a| format!("{:.1}", a as f64 / 1000.0)),
            n.station_count.map_or("-".into(), |c| c.to_string()),
            n.channel_util_pct.map_or("-".into(), |u| format!("{u:.0}%")),
        );
    }
}

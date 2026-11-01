// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! Dwell scanning: scan repeatedly for a fixed time at one spot, then collate
//! the scans into a single result.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::{Network, Result, ScanResult, Scanner};

/// Slack when judging whether a cache entry was refreshed during a scan.
const FRESH_SLACK_MS: u64 = 100;

/// Run back-to-back scans until `dwell` has elapsed (always at least one scan)
/// and collate them. `on_scan` is called after each scan with the count so far.
pub fn scan_for(
    scanner: &mut dyn Scanner,
    dwell: Duration,
    mut on_scan: impl FnMut(usize),
) -> Result<ScanResult> {
    let start = Instant::now();
    let mut scans = Vec::new();
    loop {
        scans.push(scanner.scan()?);
        on_scan(scans.len());
        if start.elapsed() >= dwell {
            break;
        }
    }
    let mut out = collate(&scans);
    out.scan_ms = start.elapsed().as_millis() as u64;
    Ok(out)
}

/// Dwell scan with several radios at once, one thread each, then merge. A
/// radio that fails is left out with a note as long as one succeeds.
/// `on_scan` receives the total number of scans completed across radios.
pub fn scan_for_many(
    scanners: &mut [Box<dyn Scanner>],
    dwell: Duration,
    on_scan: &(impl Fn(usize) + Sync),
) -> Result<ScanResult> {
    if let [only] = scanners {
        return scan_for(only.as_mut(), dwell, on_scan);
    }
    let total = AtomicUsize::new(0);
    let results: Vec<Result<ScanResult>> = std::thread::scope(|scope| {
        let handles: Vec<_> = scanners
            .iter_mut()
            .map(|s| {
                let total = &total;
                scope.spawn(move || {
                    scan_for(s.as_mut(), dwell, |_| on_scan(total.fetch_add(1, Ordering::Relaxed) + 1))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err(anyhow::anyhow!("a scanner thread panicked"))))
            .collect()
    });
    let mut ok = Vec::new();
    let mut failures = Vec::new();
    for r in results {
        match r {
            Ok(r) => ok.push(r),
            Err(e) => failures.push(format!("{e:#}")),
        }
    }
    if ok.is_empty() {
        return Err(anyhow::anyhow!(failures.join("; ")));
    }
    let mut merged = merge_radios(ok);
    merged.notes.extend(failures.into_iter().map(|f| format!("a radio failed and was skipped: {f}")));
    Ok(merged)
}

/// Combine results taken at the same time by different radios. An AP heard by
/// more than one radio keeps the reading with the most scans behind it (the
/// strongest if tied). Radios have different antennas, so their dBm values
/// are not averaged together.
pub fn merge_radios(results: Vec<ScanResult>) -> ScanResult {
    let mut best: HashMap<String, Network> = HashMap::new();
    let mut out = ScanResult::default();
    let mut names = Vec::new();
    for r in results {
        names.push(r.interface.clone());
        out.noise_dbm = out.noise_dbm.or(r.noise_dbm);
        out.channel_busy_pct = out.channel_busy_pct.or(r.channel_busy_pct);
        out.scan_ms = out.scan_ms.max(r.scan_ms);
        out.notes.extend(r.notes.iter().map(|n| format!("{}: {n}", r.interface)));
        for n in r.networks {
            let better = best.get(&n.bssid).is_none_or(|old| {
                (n.samples, n.signal_dbm.unwrap_or(f64::MIN)) > (old.samples, old.signal_dbm.unwrap_or(f64::MIN))
            });
            if better {
                best.insert(n.bssid.clone(), n);
            }
        }
    }
    out.interface = names.join(" + ");
    out.fresh_count = best.values().filter(|n| n.samples > 0).count();
    out.networks = best.into_values().collect();
    out
}

/// Merge scans of the same spot. Per BSSID the signal is the mean of the scans
/// that heard it afresh, averaged as power (mW) rather than as dBm. An AP the
/// kernel only has a stale cache entry for (6 GHz is scanned sparsely) is kept
/// with `samples == 0` and its last known signal.
pub fn collate(scans: &[ScanResult]) -> ScanResult {
    let Some(last) = scans.last() else { return ScanResult::default() };

    struct Acc {
        latest: Network,
        mw_sum: f64,
        samples: u32,
    }
    let mut by_bssid: HashMap<&str, Acc> = HashMap::new();
    let (mut noise, mut busy) = (Vec::new(), Vec::new());

    for scan in scans {
        noise.extend(scan.noise_dbm);
        busy.extend(scan.channel_busy_pct);
        for n in &scan.networks {
            let fresh = n.age_ms.is_none_or(|a| a <= scan.scan_ms + FRESH_SLACK_MS);
            let acc = by_bssid.entry(&n.bssid).or_insert_with(|| Acc {
                latest: n.clone(),
                mw_sum: 0.0,
                samples: 0,
            });
            // Later scans carry the freshest metadata (ssid, width, load...).
            acc.latest = n.clone();
            if let (true, Some(dbm)) = (fresh, n.signal_dbm) {
                acc.mw_sum += 10f64.powf(dbm / 10.0);
                acc.samples += 1;
            }
        }
    }

    let networks = by_bssid
        .into_values()
        .map(|a| {
            let mut n = a.latest;
            n.samples = a.samples;
            if a.samples > 0 {
                n.signal_dbm = Some(10.0 * (a.mw_sum / a.samples as f64).log10());
            }
            n
        })
        .collect::<Vec<_>>();

    let mean = |v: &[f64]| (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64);
    let fresh_count = networks.iter().filter(|n| n.samples > 0).count();
    let mut notes: Vec<String> = scans.iter().flat_map(|s| s.notes.iter().cloned()).collect();
    notes.dedup();
    ScanResult {
        interface: last.interface.clone(),
        networks,
        noise_dbm: mean(&noise),
        channel_busy_pct: mean(&busy.iter().map(|&b| b as f64).collect::<Vec<_>>()).map(|b| b as f32),
        scan_ms: scans.iter().map(|s| s.scan_ms).sum(),
        fresh_count,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Band, Security, Standard};

    fn net(bssid: &str, dbm: f64, age_ms: u64) -> Network {
        Network {
            bssid: bssid.into(),
            ssid: Some("x".into()),
            associated: false,
            freq_mhz: 5180,
            channel: 36,
            band: Band::Ghz5,
            signal_dbm: Some(dbm),
            age_ms: Some(age_ms),
            width_mhz: 80,
            standard: Standard::Wifi6,
            security: Security::Wpa3,
            station_count: None,
            channel_util_pct: None,
            samples: 1,
        }
    }

    fn scan(networks: Vec<Network>) -> ScanResult {
        ScanResult { networks, scan_ms: 3000, ..Default::default() }
    }

    #[test]
    fn averages_power_not_dbm() {
        let r = collate(&[scan(vec![net("a", -40.0, 500)]), scan(vec![net("a", -70.0, 500)])]);
        let n = &r.networks[0];
        assert_eq!(n.samples, 2);
        // mean of 1e-4 mW and 1e-7 mW is ~5e-5 mW, i.e. about -43 dBm
        assert!((n.signal_dbm.unwrap() + 43.0).abs() < 0.1);
    }

    #[test]
    fn stale_entries_are_kept_but_not_counted() {
        let r = collate(&[scan(vec![net("a", -50.0, 500), net("six", -55.0, 14_500)])]);
        let six = r.networks.iter().find(|n| n.bssid == "six").unwrap();
        assert_eq!(six.samples, 0);
        assert_eq!(six.signal_dbm, Some(-55.0));
        assert_eq!(r.fresh_count, 1);
    }

    #[test]
    fn ap_seen_in_one_of_three_scans_has_one_sample() {
        let r = collate(&[
            scan(vec![net("a", -50.0, 100)]),
            scan(vec![net("a", -50.0, 100), net("b", -60.0, 100)]),
            scan(vec![net("a", -50.0, 100)]),
        ]);
        let by = |id: &str| r.networks.iter().find(|n| n.bssid == id).unwrap().samples;
        assert_eq!((by("a"), by("b")), (3, 1));
    }

    #[test]
    fn merge_keeps_the_better_supported_reading_and_the_union_of_aps() {
        let mut a = net("both", -70.0, 100);
        a.samples = 2;
        let mut b = net("both", -60.0, 100);
        b.samples = 5;
        let only_b = net("six", -55.0, 100);
        let r1 = ScanResult { interface: "wlan0".into(), networks: vec![a], scan_ms: 3000, ..Default::default() };
        let r2 = ScanResult { interface: "wlan1".into(), networks: vec![b, only_b], scan_ms: 3500, ..Default::default() };
        let m = merge_radios(vec![r1, r2]);
        assert_eq!(m.interface, "wlan0 + wlan1");
        assert_eq!(m.networks.len(), 2);
        let both = m.networks.iter().find(|n| n.bssid == "both").unwrap();
        assert_eq!((both.samples, both.signal_dbm), (5, Some(-60.0)));
        assert_eq!(m.scan_ms, 3500);
    }
}

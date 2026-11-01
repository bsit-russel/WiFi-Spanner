// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! Linux scanner: NetworkManager triggers the scan, `iw` reads the kernel's
//! scan cache. Neither step needs root.

use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use crate::parse;
use crate::{ScanResult, Scanner};

const POLL: Duration = Duration::from_millis(500);
const MAX_WAIT: Duration = Duration::from_secs(15);

pub struct IwScanner {
    iface: String,
}

impl IwScanner {
    pub fn detect(iface: Option<String>) -> Result<Self> {
        let iface = match iface {
            Some(i) => i,
            None => {
                let out = run("iw", &["dev"]).context("could not list wireless interfaces")?;
                parse::first_interface(&out).ok_or_else(|| anyhow!("no wireless interface found"))?
            }
        };
        Ok(IwScanner { iface })
    }

    pub fn interface(&self) -> &str {
        &self.iface
    }
}

impl Scanner for IwScanner {
    fn name(&self) -> String {
        format!("iw + NetworkManager on {}", self.iface)
    }

    fn scan(&mut self) -> Result<ScanResult> {
        let mut notes = Vec::new();
        let start = Instant::now();

        let triggered = match run("nmcli", &["device", "wifi", "rescan", "ifname", &self.iface]) {
            Ok(_) => true,
            Err(e) => {
                notes.push(format!("scan trigger failed, showing cached results: {e:#}"));
                false
            }
        };
        let deadline = if triggered { MAX_WAIT } else { Duration::from_secs(1) };

        // Results stream into the kernel cache while the radio hops
        // channels, so wait until the count of refreshed entries stops growing.
        let (mut last_fresh, mut stable) = (0usize, 0u32);
        let (networks, fresh_count) = loop {
            std::thread::sleep(POLL);
            let dump = run("iw", &["dev", &self.iface, "scan", "dump"])?;
            let elapsed_ms = start.elapsed().as_millis() as u64;
            let networks = parse::parse_scan_dump(&dump);
            let fresh = networks
                .iter()
                .filter(|n| n.age_ms.is_some_and(|a| a + 100 < elapsed_ms))
                .count();
            stable = if fresh > 0 && fresh == last_fresh { stable + 1 } else { 0 };
            last_fresh = fresh;
            if stable >= 2 || start.elapsed() >= deadline {
                break (networks, fresh);
            }
        };
        if triggered && fresh_count == 0 {
            notes.push(format!("no fresh results within {} s", MAX_WAIT.as_secs()));
        }

        let (mut noise_dbm, mut channel_busy_pct) = (None, None);
        match run("iw", &["dev", &self.iface, "survey", "dump"]) {
            Ok(text) => {
                if let Some(s) = parse::parse_survey_dump(&text).into_iter().find(|s| s.in_use) {
                    noise_dbm = s.noise_dbm;
                    channel_busy_pct = s.busy_pct();
                }
            }
            Err(e) => notes.push(format!("survey data unavailable: {e:#}")),
        }

        Ok(ScanResult {
            interface: self.iface.clone(),
            networks,
            noise_dbm,
            channel_busy_pct,
            scan_ms: start.elapsed().as_millis() as u64,
            fresh_count,
            notes,
        })
    }
}

/// A usable Wi-Fi card as shown in the settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiCard {
    pub name: String,
    pub phy: String,
    pub driver: Option<String>,
    pub connected: bool,
}

impl WifiCard {
    pub fn label(&self) -> String {
        let mut s = self.name.clone();
        if let Some(d) = &self.driver {
            s.push_str(&format!("  ({d})"));
        }
        if self.connected {
            s.push_str("  connected");
        }
        s
    }
}

/// Station-mode Wi-Fi cards on this machine.
pub fn list_cards() -> Result<Vec<WifiCard>> {
    let out = run("iw", &["dev"]).context("could not list wireless interfaces")?;
    Ok(parse::interfaces(&out)
        .into_iter()
        .map(|i| {
            let driver = std::fs::read_link(format!("/sys/class/net/{}/device/driver", i.name))
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
            let connected = run("iw", &["dev", &i.name, "link"]).is_ok_and(|t| t.starts_with("Connected to"));
            WifiCard { name: i.name, phy: i.phy, driver, connected }
        })
        .collect())
}

/// The card to use when none is chosen: the one with a connection, else the first.
pub fn default_card(cards: &[WifiCard]) -> Option<&WifiCard> {
    cards.iter().find(|c| c.connected).or_else(|| cards.first())
}

/// Regulatory domain as printed by `iw reg get`, for diagnostics.
pub fn regulatory_domain() -> Option<String> {
    let out = run("iw", &["reg", "get"]).ok()?;
    out.lines().find(|l| l.starts_with("country ")).map(|l| l.trim().to_string())
}

fn run(program: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to run `{program}` (is it installed and on PATH?)"))?;
    if !out.status.success() {
        bail!(
            "`{program} {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

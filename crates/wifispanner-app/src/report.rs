// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! HTML survey report: one self-contained file with a cover and contents, the
//! survey path, estimated AP positions and, for each band, maps of coverage
//! (strongest, second and third AP), signal-to-noise and co-channel
//! interference, plus tables of the measurements. Coverage maps for single
//! networks are added only for the SSIDs chosen in [`ReportOptions`]. Print
//! it to PDF from a browser if needed.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use eframe::egui;
use wifispanner_core::heatmap::{self, ColorScheme, FAIR_DBM, GOOD_DBM, Grid, RAMP_MAX, RAMP_MIN, Sample};
use wifispanner_core::survey::{Floor, MISSING_DBM, Observation, Project, SurveyPoint, Target, ap_base_key, ap_key};
use wifispanner_scan::Band;

use crate::settings::Settings;
use crate::store;

const HEAT_CELLS: usize = 256;
const HEAT_OPACITY: f32 = 0.65;
const BANDS: [Band; 3] = [Band::Ghz2_4, Band::Ghz5, Band::Ghz6];
/// Noise floor assumed when the adapter reports none.
pub const ASSUMED_NOISE_DBM: f64 = -95.0;
/// Signal-to-noise range of the SNR maps, in dB.
const SNR_MIN: f64 = 5.0;
const SNR_MAX: f64 = 35.0;
/// An AP is only placed on the plan when heard at least this strongly
/// somewhere; weaker ones are neighbours outside the surveyed area.
const PLACE_MIN_DBM: f64 = -75.0;
/// Only APs at least this strong count as interference.
const INTERFERENCE_MIN_DBM: f64 = -82.0;
/// The interference maps run from clean (0) to dirty (this many overlaps).
const INTERFERENCE_MAX: f64 = 4.0;

/// How a report's heatmaps are drawn: the heatmap settings from the Settings
/// dialog, so a report matches what is on screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReportStyle {
    pub scheme: ColorScheme,
    pub min_dbm: f64,
    pub max_dbm: f64,
    /// Readings weaker than this count as not heard.
    pub detect_dbm: f64,
}

impl Default for ReportStyle {
    fn default() -> Self {
        ReportStyle { scheme: ColorScheme::Classic, min_dbm: RAMP_MIN, max_dbm: RAMP_MAX, detect_dbm: MISSING_DBM }
    }
}

impl ReportStyle {
    pub fn from_settings(s: &Settings) -> Self {
        ReportStyle { scheme: s.color_scheme, min_dbm: s.min_dbm, max_dbm: s.max_dbm, detect_dbm: s.detect_dbm }
    }

    /// A sentence saying how to read the colours.
    fn describe(&self) -> String {
        let mut text = format!(
            "Signal maps use the \"{}\" colours from {:.0} dBm (weakest) to {:.0} dBm (strongest).",
            self.scheme.name(),
            self.min_dbm,
            self.max_dbm
        );
        if self.detect_dbm > MISSING_DBM {
            text.push_str(&format!(" Readings weaker than {:.0} dBm are treated as not heard.", self.detect_dbm));
        }
        text
    }
}

/// What goes into a report beyond the standard sections.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportOptions {
    /// Networks that get a coverage map of their own.
    pub ssids: BTreeSet<String>,
    /// The floor the report is about.
    pub floor: usize,
}

/// Build the report for `project` and write it to the project's reports
/// folder, returning the file written.
pub fn write_report(project_path: &Path, project: &Project, style: &ReportStyle, opts: &ReportOptions) -> Result<PathBuf, String> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let html = build_html(project, &store::format_unix(now, "+%Y-%m-%d %H:%M"), style, opts)?;
    let dir = store::reports_dir(project_path);
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let path = dir.join(format!("report-{}.html", store::format_unix(now, "+%Y%m%d-%H%M%S")));
    std::fs::write(&path, html).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    Ok(path)
}

/// The networks of a project with the share of survey points that heard each,
/// strongest first: what the report dialog offers as tickboxes.
pub fn networks_heard(floor: &Floor) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = floor
        .ssids()
        .into_iter()
        .map(|ssid| {
            let t = Target::Ssid(ssid.clone());
            let n = floor.points.iter().filter(|p| Floor::signal_at(p, &t, None) > MISSING_DBM).count();
            (ssid, n)
        })
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// The dialog shown before a report is made, to choose its networks.
pub struct OptionsDialog {
    path: PathBuf,
    project: Project,
    networks: Vec<(String, usize)>,
    chosen: BTreeSet<String>,
    floor: usize,
}

/// What the user did in the dialog.
pub enum DialogResult {
    Open,
    Generate(ReportOptions),
    Cancel,
}

impl OptionsDialog {
    pub fn new(path: PathBuf, project: Project) -> Self {
        // Start with the active floor, or the first one that has measurements.
        let floor = if project.floor().points.is_empty() { project.floors.iter().position(|f| !f.points.is_empty()).unwrap_or(project.active) } else { project.active };
        let networks = networks_heard(&project.floors[floor]);
        OptionsDialog { path, project, networks, chosen: BTreeSet::new(), floor }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    pub fn show(&mut self, ctx: &egui::Context) -> DialogResult {
        use crate::theme::{self, ButtonKind};
        let mut result = DialogResult::Open;
        theme::dialog(ctx, "Generate report", 460.0, |ui| {
            ui.label("Every report has the survey path, access point positions and, for each band, maps of coverage, signal-to-noise and interference. Tick networks to add a coverage map for each.");
            if self.project.floors.len() > 1 {
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label("Floor");
                    for (i, f) in self.project.floors.iter().enumerate() {
                        let label = format!("{} ({} points)", f.name, f.points.len());
                        if theme::chip(ui, self.floor == i, None, &label).clicked() && self.floor != i {
                            self.floor = i;
                            self.networks = networks_heard(f);
                            self.chosen.clear();
                        }
                    }
                });
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if theme::button(ui, ButtonKind::Text, None, "All").clicked() {
                    self.chosen = self.networks.iter().map(|n| n.0.clone()).collect();
                }
                if theme::button(ui, ButtonKind::Text, None, "None").clicked() {
                    self.chosen.clear();
                }
            });
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                if self.networks.is_empty() {
                    ui.label("No named networks were heard.");
                }
                let total = self.project.floors[self.floor].points.len();
                for (ssid, heard) in &self.networks {
                    let mut on = self.chosen.contains(ssid);
                    if ui.checkbox(&mut on, format!("{ssid}   (heard at {heard} of {total} points)")).changed() {
                        if on {
                            self.chosen.insert(ssid.clone());
                        } else {
                            self.chosen.remove(ssid);
                        }
                    }
                }
            });
            ui.add_space(12.0);
            theme::actions(ui, |ui| {
                if theme::button(ui, ButtonKind::Filled, None, "Generate").clicked() {
                    result = DialogResult::Generate(ReportOptions { ssids: self.chosen.clone(), floor: self.floor });
                }
                if theme::button(ui, ButtonKind::Text, None, "Cancel").clicked() {
                    result = DialogResult::Cancel;
                }
            });
        });
        result
    }
}

/// What a map shows.
#[derive(Debug, Clone, PartialEq)]
enum Metric {
    /// Strongest signal of a network (or of any).
    Signal(Target),
    /// Signal of the n-th strongest other access point (1 = second).
    Rank(usize),
    /// Strongest signal above the noise floor.
    Snr,
    /// Access points sharing the strongest one's channel.
    Interference,
}

struct View {
    title: String,
    metric: Metric,
    band: Option<Band>,
}

/// The physical access points heard at a point on a band, strongest first.
/// One entry per AP however many SSIDs it broadcasts.
fn aps_at<'a>(p: &'a SurveyPoint, band: Option<Band>, detect: f64) -> Vec<&'a Observation> {
    let mut best: BTreeMap<String, &Observation> = BTreeMap::new();
    for o in p.observations.iter().filter(|o| o.signal_dbm >= detect && band.is_none_or(|b| o.band() == b)) {
        let e = best.entry(ap_key(&o.bssid, o.band())).or_insert(o);
        if o.signal_dbm > e.signal_dbm {
            *e = o;
        }
    }
    let mut v: Vec<_> = best.into_values().collect();
    v.sort_by(|a, b| b.signal_dbm.total_cmp(&a.signal_dbm));
    v
}

fn overlaps(a: &Observation, b: &Observation) -> bool {
    let (wa, wb) = (a.width_mhz.max(20) as i64, b.width_mhz.max(20) as i64);
    (a.freq_mhz as i64 - b.freq_mhz as i64).abs() * 2 < wa + wb
}

/// The noise floor used at a point: measured when the adapter gave one.
fn noise_at(p: &SurveyPoint) -> f64 {
    p.noise_dbm.unwrap_or(ASSUMED_NOISE_DBM)
}

/// The value a map shows at a survey point. Interference is encoded so that
/// clean is high and dirty is low, like signal, so one colour ramp serves all.
fn value_at(p: &SurveyPoint, v: &View, style: &ReportStyle) -> f64 {
    match &v.metric {
        Metric::Signal(t) => Floor::signal_at_min(p, t, v.band, style.detect_dbm),
        Metric::Rank(n) => aps_at(p, v.band, style.detect_dbm).get(*n).map_or(MISSING_DBM, |o| o.signal_dbm),
        Metric::Snr => aps_at(p, v.band, style.detect_dbm).first().map_or(MISSING_DBM, |o| o.signal_dbm - noise_at(p)),
        Metric::Interference => {
            let aps = aps_at(p, v.band, style.detect_dbm.max(INTERFERENCE_MIN_DBM));
            let n = aps.first().map_or(0, |first| aps[1..].iter().filter(|o| overlaps(first, o)).count());
            INTERFERENCE_MAX - (n as f64).min(INTERFERENCE_MAX)
        }
    }
}

/// Colour range and ramp for a metric.
fn ramp(v: &View, style: &ReportStyle) -> (ColorScheme, f64, f64) {
    match v.metric {
        Metric::Signal(_) | Metric::Rank(_) => (style.scheme, style.min_dbm, style.max_dbm),
        Metric::Snr => (style.scheme, SNR_MIN, SNR_MAX),
        Metric::Interference => (ColorScheme::Classic, 0.0, INTERFERENCE_MAX),
    }
}

fn heard_on(floor: &Floor, style: &ReportStyle, band: Band) -> bool {
    floor.points.iter().any(|p| !aps_at(p, Some(band), style.detect_dbm).is_empty())
}

/// Bands with at least one reading above the detection level.
fn bands_heard(floor: &Floor, style: &ReportStyle) -> Vec<Band> {
    BANDS.into_iter().filter(|&b| heard_on(floor, style, b)).collect()
}

/// A named group of maps in the report.
struct Section {
    id: &'static str,
    title: &'static str,
    note: String,
    views: Vec<View>,
}

/// The coverage maps: all networks, each band, then each ticked network.
fn coverage_views(floor: &Floor, style: &ReportStyle, opts: &ReportOptions) -> Vec<View> {
    let mut out = Vec::new();
    let bands = bands_heard(floor, style);
    let mut push = |label: String, target: Target| {
        let heard_in: Vec<Band> = bands
            .iter()
            .copied()
            .filter(|&b| floor.points.iter().any(|p| Floor::signal_at_min(p, &target, Some(b), style.detect_dbm) > MISSING_DBM))
            .collect();
        if heard_in.is_empty() {
            return;
        }
        out.push(View { title: format!("{label}: all bands"), metric: Metric::Signal(target.clone()), band: None });
        if heard_in.len() > 1 {
            for b in heard_in {
                out.push(View { title: format!("{label}: {b}"), metric: Metric::Signal(target.clone()), band: Some(b) });
            }
        }
    };
    push("Strongest signal from any network".into(), Target::Any);
    for ssid in &opts.ssids {
        push(format!("Network \"{ssid}\""), Target::Ssid(ssid.clone()));
    }
    out
}

fn per_band(floor: &Floor, style: &ReportStyle, title: &str, metric: Metric) -> Vec<View> {
    bands_heard(floor, style)
        .into_iter()
        .map(|b| View { title: format!("{title}: {b}"), metric: metric.clone(), band: Some(b) })
        .collect()
}

fn sections(floor: &Floor, style: &ReportStyle, opts: &ReportOptions) -> Vec<Section> {
    let measured = floor.points.iter().filter(|p| p.noise_dbm.is_some()).count();
    let noise_note = if measured == floor.points.len() {
        "Signal-to-noise is the strongest signal minus the noise floor measured by the adapter at each point.".to_string()
    } else if measured == 0 {
        format!("The adapter did not report a noise floor, so {ASSUMED_NOISE_DBM:.0} dBm is assumed everywhere. Treat these maps as signal strength on another scale, not as measured SNR.")
    } else {
        format!("The adapter reported a noise floor at {measured} of {} points; {ASSUMED_NOISE_DBM:.0} dBm is assumed at the others.", floor.points.len())
    };
    vec![
        Section {
            id: "coverage",
            title: "Coverage",
            note: format!("Strongest signal at each spot. {}", style.describe()),
            views: coverage_views(floor, style, opts),
        },
        Section {
            id: "secondary",
            title: "Secondary coverage",
            note: "Signal of the second strongest access point at each spot: how well a client could roam. Spots with only one access point in range show as not covered.".into(),
            views: per_band(floor, style, "Second strongest AP", Metric::Rank(1)),
        },
        Section {
            id: "tertiary",
            title: "Tertiary coverage",
            note: "Signal of the third strongest access point at each spot.".into(),
            views: per_band(floor, style, "Third strongest AP", Metric::Rank(2)),
        },
        Section {
            id: "snr",
            title: "Signal-to-noise (SNR)",
            note: format!("{noise_note} Scale {SNR_MIN:.0} to {SNR_MAX:.0} dB."),
            views: per_band(floor, style, "SNR", Metric::Snr),
        },
        Section {
            id: "interference",
            title: "Interference",
            note: format!("Access points heard at {INTERFERENCE_MIN_DBM:.0} dBm or stronger on the same channel as the strongest one at each spot: 0 is clean, {INTERFERENCE_MAX:.0} or more is crowded."),
            views: per_band(floor, style, "Co-channel APs", Metric::Interference),
        },
    ]
}

fn grid_for(floor: &Floor, view: &View, style: &ReportStyle) -> Grid {
    let samples: Vec<Sample> = floor.points.iter().map(|p| Sample { x: p.x, y: p.y, dbm: value_at(p, view, style) }).collect();
    let positions: Vec<[f32; 2]> = floor.points.iter().map(|p| [p.x, p.y]).collect();
    let fade_px = heatmap::auto_fade_px(&positions, floor.metres_per_px, floor.width_px, floor.height_px);
    heatmap::interpolate(&samples, floor.width_px, floor.height_px, HEAT_CELLS, fade_px)
}

fn heat_png(grid: &Grid, scheme: ColorScheme, min: f64, max: f64) -> Result<Vec<u8>, String> {
    let mut img = image::RgbaImage::new(grid.w as u32, grid.h as u32);
    for (i, px) in img.pixels_mut().enumerate() {
        let [r, g, b] = heatmap::color_in(scheme, grid.dbm[i] as f64, min, max);
        *px = image::Rgba([r, g, b, (grid.confidence[i] * 255.0) as u8]);
    }
    let mut buf = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(buf)
}

/// An access point placed on the plan from where it was heard loudest.
#[derive(Debug, Clone, PartialEq)]
struct PlacedAp {
    x: f32,
    y: f32,
    bssid: String,
    ssids: BTreeSet<String>,
    channel: u32,
    width: u32,
    band: Band,
    best: f64,
    /// Placed where the user plotted the installed access point, not estimated.
    plotted: bool,
    /// The name given to a plotted access point.
    name: Option<String>,
}

/// Estimates where each physical AP is: the signal-weighted centre of the
/// three survey points that heard it best. This is a guess from signal
/// strength alone, good to a few metres at best.
fn place_aps(floor: &Floor, band: Band) -> Vec<PlacedAp> {
    struct Seen<'a> {
        readings: Vec<(f32, f32, f64)>,
        best: &'a Observation,
        ssids: BTreeSet<String>,
    }
    let mut seen: BTreeMap<String, Seen> = BTreeMap::new();
    for p in &floor.points {
        for o in p.observations.iter().filter(|o| o.band() == band) {
            let e = seen.entry(ap_key(&o.bssid, band)).or_insert(Seen { readings: Vec::new(), best: o, ssids: BTreeSet::new() });
            e.readings.push((p.x, p.y, o.signal_dbm));
            if o.signal_dbm > e.best.signal_dbm {
                e.best = o;
            }
            if let Some(s) = o.ssid.as_ref().filter(|s| !s.is_empty()) {
                e.ssids.insert(s.clone());
            }
        }
    }
    let mut out = Vec::new();
    for (_, mut s) in seen {
        s.readings.sort_by(|a, b| b.2.total_cmp(&a.2));
        if s.readings[0].2 < PLACE_MIN_DBM {
            continue;
        }
        let top = &s.readings[..s.readings.len().min(3)];
        let weights: Vec<f64> = top.iter().map(|r| 10f64.powf(r.2 / 10.0)).collect();
        let total: f64 = weights.iter().sum();
        let x = top.iter().zip(&weights).map(|(r, w)| r.0 as f64 * w).sum::<f64>() / total;
        let y = top.iter().zip(&weights).map(|(r, w)| r.1 as f64 * w).sum::<f64>() / total;
        out.push(PlacedAp {
            x: x as f32,
            y: y as f32,
            bssid: s.best.bssid.clone(),
            ssids: s.ssids,
            channel: s.best.channel,
            width: s.best.width_mhz,
            band,
            best: s.readings[0].2,
            plotted: false,
            name: None,
        });
    }
    // Installed access points plotted on the plan stand where they really are:
    // one linked to a measured access point replaces its estimate, and any
    // other on this band is added as it is.
    for plotted in floor.survey_aps.iter().filter(|a| a.radio(band).is_some()) {
        let linked = plotted.link.as_ref().and_then(|key| out.iter_mut().find(|p| !p.plotted && ap_base_key(&p.bssid) == *key));
        match linked {
            Some(p) => {
                (p.x, p.y, p.plotted, p.name) = (plotted.x, plotted.y, true, Some(plotted.name.clone()));
            }
            None => out.push(PlacedAp {
                x: plotted.x,
                y: plotted.y,
                bssid: String::new(),
                ssids: BTreeSet::new(),
                channel: 0,
                width: 0,
                band,
                best: f64::NEG_INFINITY,
                plotted: true,
                name: Some(plotted.name.clone()),
            }),
        }
    }
    out.sort_by(|a, b| b.best.total_cmp(&a.best));
    out
}

pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

fn legend_for(scheme: ColorScheme, min: f64, max: f64, low: &str, high: &str) -> String {
    let stops = (0..=10)
        .map(|i| {
            let [r, g, b] = heatmap::color_in(scheme, min + (max - min) * i as f64 / 10.0, min, max);
            format!("rgb({r},{g},{b}) {}%", i * 10)
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("<div class=\"legend\"><span>{low}</span><div class=\"bar\" style=\"background:linear-gradient(90deg,{stops})\"></div><span>{high}</span></div>")
}

fn legend(style: &ReportStyle) -> String {
    legend_for(style.scheme, style.min_dbm, style.max_dbm, &format!("{:.0} dBm", style.min_dbm), &format!("{:.0} dBm or better", style.max_dbm))
}

fn legend_of(v: &View, style: &ReportStyle) -> String {
    let (scheme, min, max) = ramp(v, style);
    match v.metric {
        Metric::Signal(_) | Metric::Rank(_) => legend(style),
        Metric::Snr => legend_for(scheme, min, max, &format!("{min:.0} dB"), &format!("{max:.0} dB or better")),
        Metric::Interference => legend_for(scheme, min, max, "4 or more overlapping APs", "none"),
    }
}

/// Opens a figure: the plan, and room for what is drawn over it.
fn figure_open(id: &str, title: &str, floor: &Floor) -> String {
    let (w, h) = (floor.width_px, floor.height_px);
    format!("<section class=\"fig\" id=\"{id}\"><h3>{}</h3><svg viewBox=\"0 0 {w} {h}\"><use href=\"#plan\"/>{}", esc(title), walls_svg(floor))
}

/// The drawn obstacle areas and walls over the plan: areas tinted by kind,
/// walls coloured by material and dashed when lower than the ceiling.
fn walls_svg(floor: &Floor) -> String {
    let unit = floor.width_px.max(floor.height_px) as f32 / 900.0;
    let mut o = String::new();
    for a in &floor.areas {
        let [r, g, b] = a.kind.colour();
        let pts: Vec<String> = a.points.iter().map(|p| format!("{},{}", p[0], p[1])).collect();
        let _ = write!(o, "<polygon points=\"{}\" fill=\"rgb({r},{g},{b})\" fill-opacity=\"0.28\" stroke=\"rgb({r},{g},{b})\" stroke-width=\"{}\"/>", pts.join(" "), 1.5 * unit);
    }
    for w in &floor.walls {
        let ([r, g, b], width) = w.material.style();
        let low = w.effective_height_m(floor.ceiling_m) < floor.ceiling_m - 0.01;
        let _ = write!(
            o,
            "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"rgb({r},{g},{b})\" stroke-width=\"{}\" stroke-linecap=\"round\"{}/>",
            w.a[0],
            w.a[1],
            w.b[0],
            w.b[1],
            width * unit,
            if low { format!(" stroke-dasharray=\"{} {}\"", 8.0 * unit, 5.0 * unit) } else { String::new() }
        );
    }
    o
}

fn heat_layer(floor: &Floor, png: &[u8]) -> String {
    format!(
        "<image width=\"{}\" height=\"{}\" preserveAspectRatio=\"none\" opacity=\"{HEAT_OPACITY}\" href=\"data:image/png;base64,{}\"/>",
        floor.width_px,
        floor.height_px,
        B64.encode(png)
    )
}

fn point_markers(floor: &Floor, marker_r: f32) -> String {
    let mut o = String::new();
    for (n, p) in floor.points.iter().enumerate() {
        let _ = write!(
            o,
            "<circle cx=\"{}\" cy=\"{}\" r=\"{marker_r}\" fill=\"#1e5ac8\" stroke=\"#fff\" stroke-width=\"{}\"/><text x=\"{}\" y=\"{}\" font-size=\"{}\" text-anchor=\"middle\" fill=\"#fff\" font-family=\"sans-serif\">{}</text>",
            p.x,
            p.y,
            marker_r / 5.0,
            p.x,
            p.y + marker_r * 0.35,
            marker_r,
            n + 1
        );
    }
    o
}

/// Coverage statistics for a signal view.
fn summary_row(floor: &Floor, v: &View, grid: &Grid, id: &str) -> String {
    let cov = heatmap::coverage(grid);
    format!(
        "<tr><td><a href=\"#{id}\">{}</a></td><td>{}</td><td class=\"good\">{:.0}%</td><td class=\"fair\">{:.0}%</td><td class=\"poor\">{:.0}%</td></tr>",
        esc(&v.title),
        match floor.metres_per_px {
            Some(m) => format!("{:.0} m&sup2;", cov.measured_cells as f32 * (grid.step_px * m).powi(2)),
            None => "scale not set".to_string(),
        },
        cov.good_pct,
        cov.fair_pct,
        cov.poor_pct
    )
}

pub fn build_html(project: &Project, generated: &str, style: &ReportStyle, opts: &ReportOptions) -> Result<String, String> {
    let floor = &project.floors[opts.floor.min(project.floors.len() - 1)];
    if floor.points.is_empty() {
        return Err("the project has no survey points yet".into());
    }
    let (w, h) = (floor.width_px, floor.height_px);
    let marker_r = w.max(h) as f32 / 110.0;
    let mut o = String::new();

    let _ = write!(
        o,
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>{} - WiFi survey report</title><style>{CSS}</style></head><body>",
        esc(&floor.name)
    );
    // The plan is stored once and referenced by every figure.
    let _ = write!(
        o,
        "<svg width=\"0\" height=\"0\" style=\"position:absolute\"><defs><image id=\"plan\" width=\"{w}\" height=\"{h}\" href=\"data:{};base64,{}\"/></defs></svg>",
        mime_of(&floor.image),
        B64.encode(&floor.image)
    );

    // Cover.
    let first = floor.points.iter().map(|p| p.unix_secs).min().unwrap_or(0);
    let last = floor.points.iter().map(|p| p.unix_secs).max().unwrap_or(0);
    let named: Vec<String> = floor.ssids().into_iter().filter(|s| !s.is_empty()).collect();
    let shown = named.iter().take(4).map(|s| esc(s)).collect::<Vec<_>>().join(", ");
    let more = if named.len() > 4 { format!(" + {} more", named.len() - 4) } else { String::new() };
    let _ = write!(
        o,
        "<header class=\"cover\"><h1>{}</h1><p class=\"ssids\">SSIDs: {shown}{more}</p><p class=\"meta\">Generated {} &middot; {} survey points &middot; measured {} to {} &middot; scale {}</p></header>",
        esc(&floor.name),
        esc(generated),
        floor.points.len(),
        esc(&store::format_unix(first, "+%Y-%m-%d %H:%M")),
        esc(&store::format_unix(last, "+%H:%M")),
        match floor.metres_per_px {
            Some(m) => format!("{:.1} px/m", 1.0 / m),
            None => "not set (positions in pixels)".to_string(),
        }
    );

    let mut toc: Vec<(String, String)> = Vec::new();
    let mut body = String::new();

    // Survey path: the points in the order they were taken, joined.
    toc.push(("paths".into(), "Survey path".into()));
    let _ = write!(body, "<div class=\"sec\" id=\"paths\"><h2>Survey path</h2><p class=\"meta\">Survey points in the order they were taken.</p>{}", figure_open("paths-map", "Survey points", floor));
    let line: Vec<String> = floor.points.iter().map(|p| format!("{},{}", p.x, p.y)).collect();
    let _ = write!(
        body,
        "<polyline points=\"{}\" fill=\"none\" stroke=\"#1e5ac8\" stroke-width=\"{}\" stroke-dasharray=\"{} {}\" opacity=\"0.7\"/>{}</svg></section></div>",
        line.join(" "),
        marker_r / 5.0,
        marker_r / 2.0,
        marker_r / 2.0,
        point_markers(floor, marker_r)
    );

    // Estimated AP positions, one map per band.
    let bands = bands_heard(floor, style);
    let mut placement = String::new();
    for &band in &bands {
        let aps = place_aps(floor, band);
        let mut fig = figure_open(&format!("aps-{}", band_id(band)), &format!("Access points: {band} ({})", aps.len()), floor);
        let mut rows = String::new();
        for (i, ap) in aps.iter().enumerate() {
            let r = marker_r * 1.5;
            let _ = write!(
                fig,
                "<circle cx=\"{}\" cy=\"{}\" r=\"{r}\" fill=\"#fff\" stroke=\"#1e5ac8\" stroke-width=\"{}\"/><text x=\"{}\" y=\"{}\" font-size=\"{}\" text-anchor=\"middle\" fill=\"#1e5ac8\" font-weight=\"700\" font-family=\"sans-serif\">{}</text>\
                 <text x=\"{}\" y=\"{}\" font-size=\"{}\" text-anchor=\"middle\" fill=\"#1c1c1c\" stroke=\"#fff\" stroke-width=\"{}\" paint-order=\"stroke\" font-family=\"sans-serif\">{}</text>",
                ap.x,
                ap.y,
                marker_r / 4.0,
                ap.x,
                ap.y + r * 0.35,
                r * 1.0,
                i + 1,
                ap.x,
                ap.y + r * 2.4,
                marker_r * 1.4,
                marker_r / 3.0,
                if ap.channel == 0 { esc(ap.name.as_deref().unwrap_or("")) } else { format!("ch {}/{}", ap.channel, ap.width) }
            );
            let ssids = ap.ssids.iter().cloned().collect::<Vec<_>>().join(", ");
            let _ = write!(
                rows,
                "<tr><td>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                i + 1,
                if ap.bssid.is_empty() { "not measured".to_string() } else { esc(&ap.bssid) },
                esc(&match &ap.name {
                    Some(n) if ssids.is_empty() => n.clone(),
                    Some(n) => format!("{n}: {ssids}"),
                    None => ssids,
                }),
                if ap.channel == 0 { "-".to_string() } else { ap.channel.to_string() },
                if ap.width == 0 { "-".to_string() } else { format!("{} MHz", ap.width) },
                if ap.best.is_finite() { format!("{:.0} dBm", ap.best) } else { "-".to_string() },
                esc(&format!("{}{}", position_xy(floor, ap.x, ap.y), if ap.plotted { " (plotted)" } else { " (estimated)" }))
            );
        }
        fig.push_str("</svg></section>");
        placement.push_str(&fig);
        if aps.is_empty() {
            let _ = write!(placement, "<p class=\"meta\">No {band} access point was heard at {PLACE_MIN_DBM:.0} dBm or stronger.</p>");
        } else {
            let _ = write!(placement, "<table><tr><th>#</th><th>BSSID</th><th>SSIDs</th><th>Channel</th><th>Width</th><th>Best signal</th><th>Estimated position</th></tr>{rows}</table>");
        }
    }
    if !placement.is_empty() {
        toc.push(("placement".into(), "AP placement".into()));
        let _ = write!(
            body,
            "<div class=\"sec\" id=\"placement\"><h2>AP placement</h2><p class=\"meta\">Installed access points plotted on the plan are shown where they really are; the rest are estimated from signal strength alone, at the signal-weighted centre of the three survey points that heard them best, so those positions are approximate. Only access points heard at {PLACE_MIN_DBM:.0} dBm or stronger are placed. One entry per radio, however many SSIDs it broadcasts.</p>{placement}</div>"
        );
    }

    // Heatmap sections.
    let mut summary = String::from("<table><tr><th>View</th><th>Surveyed area (approx.)</th><th>Good</th><th>Fair</th><th>Poor</th></tr>");
    let mut any_maps = false;
    let mut idx = 0;
    for sec in sections(floor, style, opts) {
        if sec.views.is_empty() {
            continue;
        }
        any_maps = true;
        toc.push((sec.id.into(), sec.title.into()));
        let _ = write!(body, "<div class=\"sec\" id=\"{}\"><h2>{}</h2><p class=\"meta\">{}</p>", sec.id, sec.title, esc(&sec.note));
        for v in &sec.views {
            let id = format!("v{idx}");
            idx += 1;
            let grid = grid_for(floor, v, style);
            if sec.id == "coverage" {
                summary.push_str(&summary_row(floor, v, &grid, &id));
            }
            let (scheme, min, max) = ramp(v, style);
            body.push_str(&figure_open(&id, &v.title, floor));
            body.push_str(&heat_layer(floor, &heat_png(&grid, scheme, min, max)?));
            body.push_str(&point_markers(floor, marker_r));
            let _ = write!(body, "</svg>{}</section>", legend_of(v, style));
        }
        body.push_str("</div>");
    }
    summary.push_str("</table>");
    if !any_maps {
        let _ = write!(body, "<div class=\"sec\"><h2>Coverage</h2><p class=\"meta\">No network was heard at or above the detection level, so there are no heatmaps. {}</p></div>", esc(&style.describe()));
    } else {
        let _ = write!(
            body,
            "<div class=\"sec\" id=\"summary\"><h2>Coverage summary</h2><p class=\"meta\">Share of the surveyed area by signal: good is {GOOD_DBM:.0} dBm or better, fair is {FAIR_DBM:.0} to {GOOD_DBM:.0}, poor is weaker. Areas far from any survey point are left out.</p>{summary}</div>"
        );
        toc.push(("summary".into(), "Coverage summary".into()));
    }

    // Survey points.
    toc.push(("points".into(), "Survey points".into()));
    body.push_str("<div class=\"sec\" id=\"points\"><h2>Survey points</h2><table><tr><th>#</th><th>Position</th><th>Time</th><th>Dwell</th><th>APs heard</th><th>Strongest</th><th>Noise floor</th></tr>");
    for (n, p) in floor.points.iter().enumerate() {
        let best = p.observations.iter().max_by(|a, b| a.signal_dbm.total_cmp(&b.signal_dbm));
        let _ = write!(
            body,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{} s</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            n + 1,
            esc(&position(floor, p)),
            esc(&store::format_unix(p.unix_secs, "+%H:%M:%S")),
            p.dwell_secs,
            p.observations.len(),
            best.map_or("none".into(), |b| format!("{} {:.0} dBm", esc(b.ssid.as_deref().unwrap_or("<hidden>")), b.signal_dbm)),
            p.noise_dbm.map_or("not reported".into(), |n| format!("{n:.0} dBm"))
        );
    }
    body.push_str("</table></div>");

    // Access points across the whole survey.
    struct Ap<'a> {
        ssid: Option<&'a str>,
        band: Band,
        channel: u32,
        width: u32,
        best: f64,
        seen_at: usize,
    }
    let mut aps: BTreeMap<&str, Ap> = BTreeMap::new();
    for p in &floor.points {
        for ob in &p.observations {
            let ap = aps.entry(&ob.bssid).or_insert(Ap { ssid: ob.ssid.as_deref(), band: ob.band(), channel: ob.channel, width: ob.width_mhz, best: f64::MIN, seen_at: 0 });
            ap.best = ap.best.max(ob.signal_dbm);
            ap.seen_at += 1;
        }
    }
    let mut aps: Vec<_> = aps.into_iter().collect();
    aps.sort_by(|a, b| b.1.best.total_cmp(&a.1.best));
    toc.push(("aplist".into(), "AP list".into()));
    body.push_str("<div class=\"sec\" id=\"aplist\"><h2>AP list</h2><table><tr><th>SSID</th><th>BSSID</th><th>Band</th><th>Channel</th><th>Width</th><th>Best signal</th><th>Seen at</th></tr>");
    for (bssid, a) in aps {
        let _ = write!(
            body,
            "<tr><td>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td><td>{} MHz</td><td>{:.0} dBm</td><td>{} of {} points</td></tr>",
            esc(a.ssid.unwrap_or("<hidden>")),
            esc(bssid),
            a.band,
            a.channel,
            a.width,
            a.best,
            a.seen_at,
            floor.points.len()
        );
    }
    body.push_str("</table></div>");

    o.push_str("<nav class=\"toc\"><h2>Contents</h2><ol>");
    for (id, title) in &toc {
        let _ = write!(o, "<li><a href=\"#{id}\">{}</a></li>", esc(title));
    }
    o.push_str("</ol></nav>");
    o.push_str(&body);
    o.push_str("</body></html>");
    Ok(o)
}

fn band_id(b: Band) -> &'static str {
    match b {
        Band::Ghz2_4 => "2",
        Band::Ghz5 => "5",
        Band::Ghz6 => "6",
    }
}

fn position(floor: &Floor, p: &SurveyPoint) -> String {
    position_xy(floor, p.x, p.y)
}

fn position_xy(floor: &Floor, x: f32, y: f32) -> String {
    match floor.metres_per_px {
        Some(m) => format!("{:.1} m, {:.1} m", x * m, y * m),
        None => format!("{x:.0} px, {y:.0} px"),
    }
}

fn mime_of(image: &[u8]) -> &'static str {
    if image.starts_with(&[0x89, b'P', b'N', b'G']) { "image/png" } else { "image/jpeg" }
}

const CSS: &str = "body{font-family:system-ui,sans-serif;max-width:1100px;margin:2rem auto;padding:0 1rem;color:#1c1c1c}\
h1{margin-bottom:.2rem;font-size:2.2rem}h2{margin-top:2rem;border-bottom:1px solid #ccc;padding-bottom:.2rem}h3{margin:.2rem 0}\
.cover{padding:3rem 0 1rem;text-align:center}.cover .ssids{font-size:1.1rem}\
.toc ol{columns:2;line-height:1.8}.sec{break-before:page}\
.meta{color:#555;margin-top:.2rem}table{border-collapse:collapse;width:100%;margin:.6rem 0}\
th,td{border:1px solid #ccc;padding:.3rem .5rem;text-align:left;font-size:.9rem}th{background:#f1f1f1}\
td.good{color:#1c7a4a;font-weight:600}td.fair{color:#a87400;font-weight:600}td.poor{color:#b3261e;font-weight:600}\
.fig{break-inside:avoid;margin-top:1.5rem}.fig svg{width:100%;height:auto;border:1px solid #ccc;background:#fff}\
.legend{display:flex;align-items:center;gap:.6rem;font-size:.8rem;margin-top:.4rem}.legend .bar{height:12px;width:240px}\
a{color:#1e5ac8}@media print{body{margin:0;max-width:none}}";

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(bssid: &str, ssid: &str, freq: u32, dbm: f64) -> Observation {
        Observation {
            bssid: bssid.into(),
            ssid: Some(ssid.into()),
            freq_mhz: freq,
            channel: Band::from_freq(freq).channel(freq),
            width_mhz: 20,
            signal_dbm: dbm,
            samples: 4,
        }
    }

    fn at(x: f32, observations: Vec<Observation>) -> SurveyPoint {
        SurveyPoint { x, y: 10.0, unix_secs: 1_700_000_000, dwell_secs: 5, noise_dbm: None, observations }
    }

    fn point(x: f32, ssid: &str, dbm: f64) -> SurveyPoint {
        at(x, vec![obs("aa:bb:cc:00:00:01", ssid, 5180, dbm)])
    }

    fn project(points: Vec<SurveyPoint>) -> Project {
        let mut png = Vec::new();
        image::RgbaImage::new(40, 20).write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        Project::new(Floor { name: "Test <plan>".into(), image: png, width_px: 40, height_px: 20, metres_per_px: Some(0.5), points, walls: Vec::new(), ceiling_m: 3.0, placement: Default::default(), aps: Vec::new(), survey_aps: Vec::new(), areas: Vec::new(), zone: Vec::new() })
    }

    fn html(p: &Project) -> String {
        build_html(p, "now", &ReportStyle::default(), &ReportOptions::default()).unwrap()
    }

    fn with_ssid(ssid: &str) -> ReportOptions {
        ReportOptions { ssids: [ssid.to_string()].into(), ..Default::default() }
    }

    #[test]
    fn report_escapes_untrusted_text_and_names_the_floor() {
        let evil = "<script>alert(1)</script>";
        let out = build_html(&project(vec![point(5.0, evil, -50.0), point(30.0, evil, -72.0)]), "now", &ReportStyle::default(), &with_ssid(evil)).unwrap();
        assert!(!out.contains("<script>"));
        assert!(out.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(out.contains("Test &lt;plan&gt;"));
        assert!(out.contains("2.5 m, 5.0 m"), "positions in metres when scaled");
    }

    #[test]
    fn single_networks_get_a_map_only_when_ticked() {
        let p = project(vec![point(5.0, "Mine", -50.0), point(30.0, "Mine", -72.0)]);
        assert!(!html(&p).contains("Network &quot;Mine&quot;"));
        let out = build_html(&p, "now", &ReportStyle::default(), &with_ssid("Mine")).unwrap();
        assert!(out.contains("Network &quot;Mine&quot;: all bands"));
        let never_heard = build_html(&p, "now", &ReportStyle::default(), &with_ssid("Ghost")).unwrap();
        assert!(!never_heard.contains("Ghost"), "a network nobody heard has no map");
    }

    #[test]
    fn the_dialog_lists_networks_by_how_widely_they_were_heard() {
        let p = project(vec![point(5.0, "Few", -50.0), point(6.0, "Many", -50.0), point(7.0, "Many", -60.0)]);
        let nets = networks_heard(p.floor());
        assert_eq!(nets, vec![("Many".to_string(), 2), ("Few".to_string(), 1)]);
    }

    #[test]
    fn report_has_cover_contents_and_every_section_for_each_heard_band() {
        let p = project(vec![
            at(5.0, vec![obs("aa:00:00:00:00:01", "Mine", 5180, -50.0), obs("bb:00:00:00:00:01", "Other", 5180, -60.0), obs("cc:00:00:00:00:01", "Mine", 2437, -55.0)]),
            at(30.0, vec![obs("aa:00:00:00:00:01", "Mine", 5180, -70.0), obs("bb:00:00:00:00:01", "Other", 5180, -62.0)]),
        ]);
        let out = html(&p);
        for id in ["paths", "placement", "coverage", "secondary", "tertiary", "snr", "interference", "points", "aplist"] {
            assert!(out.contains(&format!("href=\"#{id}\"")), "contents lists {id}");
            assert!(out.contains(&format!("id=\"{id}\"")), "section {id} exists");
        }
        assert!(out.contains("SSIDs: Mine, Other"));
        assert!(out.contains("SNR: 5 GHz") && out.contains("SNR: 2.4 GHz"));
        assert!(!out.contains("6 GHz"), "a band nobody heard is left out");
    }

    #[test]
    fn secondary_coverage_ranks_physical_aps_not_ssids() {
        // One radio broadcasting two SSIDs counts once; a second radio is the runner-up.
        let p = at(
            5.0,
            vec![obs("aa:00:00:00:00:01", "A", 5180, -50.0), obs("a8:00:00:00:00:02", "B", 5180, -52.0), obs("cc:00:00:00:00:01", "A", 5180, -65.0)],
        );
        let v = |n| View { title: String::new(), metric: Metric::Rank(n), band: Some(Band::Ghz5) };
        let style = ReportStyle::default();
        assert_eq!(value_at(&p, &v(0), &style), -50.0);
        assert_eq!(value_at(&p, &v(1), &style), -65.0, "the second SSID of the first radio is not a second AP");
        assert_eq!(value_at(&p, &v(2), &style), MISSING_DBM);
    }

    #[test]
    fn snr_uses_the_measured_noise_floor_or_an_assumed_one() {
        let mut p = point(5.0, "Mine", -60.0);
        let v = View { title: String::new(), metric: Metric::Snr, band: Some(Band::Ghz5) };
        let style = ReportStyle::default();
        assert_eq!(value_at(&p, &v, &style), -60.0 - ASSUMED_NOISE_DBM);
        p.noise_dbm = Some(-90.0);
        assert_eq!(value_at(&p, &v, &style), 30.0);
        let none_measured = html(&project(vec![point(5.0, "Mine", -60.0)]));
        assert!(none_measured.contains("did not report a noise floor"));
        let mut measured = project(vec![point(5.0, "Mine", -60.0)]);
        measured.floor_mut().points[0].noise_dbm = Some(-92.0);
        assert!(html(&measured).contains("measured by the adapter"));
    }

    #[test]
    fn interference_counts_aps_on_the_strongest_ones_channel() {
        let a = obs("aa:00:00:00:00:01", "A", 5180, -50.0);
        let same = obs("bb:00:00:00:00:01", "B", 5180, -60.0);
        let other_channel = obs("cc:00:00:00:00:01", "C", 5240, -60.0);
        let too_weak = obs("dd:00:00:00:00:01", "D", 5180, -90.0);
        let p = at(5.0, vec![a, same, other_channel, too_weak]);
        let v = View { title: String::new(), metric: Metric::Interference, band: Some(Band::Ghz5) };
        assert_eq!(value_at(&p, &v, &ReportStyle::default()), INTERFERENCE_MAX - 1.0);
    }

    #[test]
    fn aps_are_placed_near_where_they_were_loudest() {
        let p = project(vec![
            at(5.0, vec![obs("aa:00:00:00:00:01", "A", 5180, -45.0)]),
            at(30.0, vec![obs("aa:00:00:00:00:01", "A", 5180, -70.0)]),
        ]);
        let aps = place_aps(p.floor(), Band::Ghz5);
        assert_eq!(aps.len(), 1);
        assert!(aps[0].x < 10.0, "pulled towards the loud point, got {}", aps[0].x);
        let faint = project(vec![at(5.0, vec![obs("aa:00:00:00:00:01", "A", 5180, -85.0)])]);
        assert!(place_aps(faint.floor(), Band::Ghz5).is_empty(), "a distant neighbour is not placed");
    }

    #[test]
    fn drawn_walls_appear_on_every_map_and_low_ones_are_dashed() {
        let mut p = project(vec![point(5.0, "Mine", -50.0)]);
        p.floor_mut().walls.push(wifispanner_core::walls::Wall::new([1.0, 1.0], [30.0, 1.0], wifispanner_core::Material::Concrete));
        p.floor_mut().walls.push(wifispanner_core::walls::Wall::new([1.0, 5.0], [30.0, 5.0], wifispanner_core::Material::Metal));
        let out = html(&p);
        let figures = out.matches("<section class=\"fig\"").count();
        assert_eq!(out.matches("<line ").count(), 2 * figures);
        assert_eq!(out.matches("stroke-linecap=\"round\" stroke-dasharray").count(), figures, "only the shelf is lower than the ceiling");
    }

    #[test]
    fn a_report_is_about_the_chosen_floor() {
        let mut p = project(vec![point(5.0, "Ground", -50.0)]);
        let mut upper = p.floor().clone();
        upper.name = "Upper".into();
        upper.points = vec![point(5.0, "Roof", -50.0)];
        p.add_floor(upper);
        let ground = build_html(&p, "now", &ReportStyle::default(), &ReportOptions { floor: 0, ..Default::default() }).unwrap();
        let roof = build_html(&p, "now", &ReportStyle::default(), &ReportOptions { floor: 1, ..Default::default() }).unwrap();
        assert!(ground.contains("SSIDs: Ground") && !ground.contains("Roof"));
        assert!(roof.contains("SSIDs: Roof") && roof.contains("<h1>Upper</h1>"));
        let dialog = OptionsDialog::new(std::path::PathBuf::new(), p);
        assert_eq!(dialog.floor, 1, "the active floor is offered first");
    }

    #[test]
    fn plotted_access_points_replace_the_estimate_for_their_linked_radio() {
        let mut p = project(vec![
            at(5.0, vec![obs("aa:00:00:00:00:01", "A", 5180, -45.0)]),
            at(30.0, vec![obs("aa:00:00:00:00:01", "A", 5180, -70.0)]),
        ]);
        let mut real = wifispanner_core::predict::PlannedAp::new("Reception".into(), 33.0, 12.0);
        real.existing = true;
        real.link = Some(ap_base_key("aa:00:00:00:00:01"));
        let mut other = wifispanner_core::predict::PlannedAp::new("Store".into(), 2.0, 2.0);
        other.existing = true;
        let mut planned_only = wifispanner_core::predict::PlannedAp::new("Idea".into(), 9.0, 9.0);
        planned_only.existing = false;
        p.floor_mut().survey_aps = vec![real, other];
        p.floor_mut().aps = vec![planned_only];
        let aps = place_aps(p.floor(), Band::Ghz5);
        assert_eq!(aps.len(), 2, "the estimate, replaced, plus the unlinked plotted one; planned ones are not in a survey report");
        let linked = aps.iter().find(|a| !a.bssid.is_empty()).unwrap();
        assert_eq!((linked.x, linked.y, linked.plotted), (33.0, 12.0, true));
        assert!(aps.iter().any(|a| a.bssid.is_empty() && a.name.as_deref() == Some("Store")));
        let out = html(&p);
        assert!(out.contains("(plotted)") && out.contains("not measured"));
    }

    #[test]
    fn obstacle_areas_are_drawn_on_the_maps() {
        let mut p = project(vec![point(5.0, "Mine", -50.0)]);
        p.floor_mut().areas.push(wifispanner_core::obstacles::Area::rectangle([2.0, 2.0], [20.0, 10.0], wifispanner_core::obstacles::ObstacleKind::Car));
        let out = html(&p);
        assert_eq!(out.matches("<polygon ").count(), out.matches("<section class=\"fig\"").count());
    }

    #[test]
    fn empty_surveys_are_refused() {
        assert!(build_html(&project(vec![]), "now", &ReportStyle::default(), &ReportOptions::default()).is_err());
    }

    #[test]
    fn report_embeds_the_projects_own_floor_plan_bytes() {
        let mut p = project(vec![point(5.0, "Mine", -50.0)]);
        p.floor_mut().image = (0..=255u8).chain(0..=255u8).collect();
        let out = html(&p);
        assert!(out.contains(&B64.encode(&p.floor().image)));
        assert_eq!(out.matches("id=\"plan\"").count(), 1, "the plan is embedded once and reused");
    }

    #[test]
    fn heatmap_settings_change_the_legend_and_are_stated_in_the_report() {
        let p = project(vec![point(5.0, "Mine", -50.0), point(30.0, "Mine", -60.0)]);
        let classic = html(&p);
        let custom = ReportStyle { scheme: ColorScheme::Rainbow, min_dbm: -80.0, max_dbm: -50.0, detect_dbm: -100.0 };
        let rainbow = build_html(&p, "now", &custom, &ReportOptions::default()).unwrap();
        assert!(rainbow.contains(">-80 dBm<") && rainbow.contains(">-50 dBm or better<"), "legend follows the range");
        assert!(rainbow.contains("Blue to red") && rainbow.contains("from -80 dBm (weakest) to -50 dBm (strongest)"));
        assert!(classic.contains("Red to green") && !classic.contains("treated as not heard"));
        assert_ne!(legend(&ReportStyle::default()), legend(&custom), "a different scheme and range give a different ramp");
        let grid = heatmap::interpolate(&[Sample { x: 5.0, y: 5.0, dbm: -60.0 }], 40, 20, 8, 20.0);
        assert_ne!(heat_png(&grid, ColorScheme::Classic, -90.0, -45.0).unwrap(), heat_png(&grid, ColorScheme::Rainbow, -80.0, -50.0).unwrap());
    }

    #[test]
    fn the_detection_level_drops_weak_readings_from_the_heatmaps() {
        let p = project(vec![point(5.0, "Faint", -85.0), point(30.0, "Faint", -88.0)]);
        assert!(!coverage_views(p.floor(), &ReportStyle::default(), &ReportOptions::default()).is_empty());
        let strict = ReportStyle { detect_dbm: -70.0, ..ReportStyle::default() };
        assert!(coverage_views(p.floor(), &strict, &ReportOptions::default()).is_empty(), "nothing is strong enough to count");
        let out = build_html(&p, "now", &strict, &ReportOptions::default()).unwrap();
        assert!(out.contains("treated as not heard") && out.contains("No network was heard at or above the detection level"));
        // The raw data tables still list everything that was seen.
        assert!(out.contains("AP list") && out.contains("-85 dBm"));
    }

    #[test]
    fn style_is_taken_from_the_settings() {
        let s = Settings { color_scheme: ColorScheme::CoolWarm, min_dbm: -90.0, max_dbm: -40.0, detect_dbm: -75.0, ..Default::default() };
        assert_eq!(ReportStyle::from_settings(&s), ReportStyle { scheme: ColorScheme::CoolWarm, min_dbm: -90.0, max_dbm: -40.0, detect_dbm: -75.0 });
    }
}

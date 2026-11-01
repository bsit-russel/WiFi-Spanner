// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! The project gallery shown when the Surveying tab opens: a "New project"
//! tile followed by a card for each saved project, with a thumbnail of its
//! floor plan. Reports are reached from each card's menu.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::SystemTime;

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions, Vec2};
use egui_material_icons::icons::{ICON_ADD, ICON_DESCRIPTION, ICON_FOLDER, ICON_FOLDER_OPEN, ICON_IMAGE, ICON_MORE_VERT, ICON_OPEN_IN_NEW, ICON_REFRESH};
use wifispanner_core::survey::Project;

use crate::settings::Settings;
use crate::theme::{self, ButtonKind};
use crate::{report, store};

/// Card geometry in points.
const CARD_W: f32 = 224.0;
const THUMB_H: f32 = 150.0;
const CARD_H: f32 = 224.0;
const GAP: f32 = 16.0;
/// Thumbnails are made no larger than this, which is plenty for a card.
const THUMB_MAX: (u32, u32) = (480, 360);

/// What the user chose in the gallery.
pub enum GalleryAction {
    /// Start a new project from a floor plan.
    New,
    Open(PathBuf),
}

#[derive(Clone, Default)]
struct Summary {
    floors: usize,
    points: usize,
    aps: usize,
    error: Option<String>,
}

/// A project file read in the background: its summary and a thumbnail.
struct Loaded {
    path: PathBuf,
    modified: Option<SystemTime>,
    summary: Summary,
    /// RGBA pixels and size of the thumbnail.
    thumb: Option<([usize; 2], Vec<u8>)>,
}

struct Entry {
    path: PathBuf,
    name: String,
    modified: Option<SystemTime>,
}

#[derive(Default)]
pub struct Gallery {
    entries: Vec<Entry>,
    /// Loaded summaries, kept while a file's modified time is unchanged.
    loaded: HashMap<PathBuf, (Option<SystemTime>, Summary)>,
    thumbs: HashMap<PathBuf, TextureHandle>,
    rx: Option<Receiver<Loaded>>,
    stale: bool,
    /// The project whose reports dialog is open, and its reports.
    reports_of: Option<PathBuf>,
    reports: Vec<store::ReportFile>,
    status: String,
    /// The dialog choosing what goes into a report, while it is open.
    report_dialog: Option<report::OptionsDialog>,
    /// The last status shown as a snackbar, to spot changes.
    toasted: String,
}

impl Gallery {
    /// Ask for the list to be re-read the next time the gallery is shown.
    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    /// A new status message since the last call, for a snackbar.
    pub fn take_toast(&mut self) -> Option<String> {
        if self.status == self.toasted {
            return None;
        }
        self.toasted = self.status.clone();
        (!self.status.is_empty()).then(|| self.status.clone())
    }

    /// Re-read the projects folder and load whatever is new or changed.
    fn refresh(&mut self, ctx: &egui::Context) {
        self.stale = false;
        let mut entries: Vec<Entry> = store::project_files()
            .into_iter()
            .map(|path| {
                let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                let name = path.file_stem().map_or("project".into(), |s| s.to_string_lossy().into_owned());
                Entry { path, name, modified }
            })
            .collect();
        entries.sort_by(|a, b| b.modified.cmp(&a.modified));
        let wanted: Vec<_> = entries
            .iter()
            .filter(|e| self.loaded.get(&e.path).is_none_or(|(m, _)| *m != e.modified))
            .map(|e| (e.path.clone(), e.modified))
            .collect();
        self.loaded.retain(|p, _| entries.iter().any(|e| &e.path == p));
        self.thumbs.retain(|p, _| entries.iter().any(|e| &e.path == p));
        self.entries = entries;
        if !wanted.is_empty() {
            let (tx, rx) = mpsc::channel();
            let repaint = ctx.clone();
            std::thread::spawn(move || {
                for (path, modified) in wanted {
                    if tx.send(load_one(&path, modified)).is_err() {
                        return;
                    }
                    repaint.request_repaint();
                }
            });
            self.rx = Some(rx);
        }
    }

    /// Take finished background loads and turn thumbnails into textures.
    fn poll(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.rx else { return };
        for loaded in rx.try_iter().collect::<Vec<_>>() {
            if let Some((size, rgba)) = loaded.thumb {
                let image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
                let name = format!("thumb-{}", loaded.path.display());
                self.thumbs.insert(loaded.path.clone(), ctx.load_texture(name, image, TextureOptions::LINEAR));
            }
            self.loaded.insert(loaded.path, (loaded.modified, loaded.summary));
        }
    }

    /// Open the dialog that chooses what the report contains.
    fn generate_report(&mut self, path: &Path) {
        let project = std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|t| Project::from_json(&t).map_err(|e| e.to_string()));
        match project {
            Ok(p) if p.floors.iter().all(|f| f.points.is_empty()) => self.status = "Could not create report: the project has no survey points yet".into(),
            Ok(p) => self.report_dialog = Some(report::OptionsDialog::new(path.to_path_buf(), p)),
            Err(e) => self.status = format!("Could not create report: {e}"),
        }
    }

    fn write_report(&mut self, dialog: &report::OptionsDialog, style: &report::ReportStyle, opts: &report::ReportOptions) {
        let path = dialog.path();
        match report::write_report(path, dialog.project(), style, opts) {
            Ok(file) => {
                self.status = format!("Report saved: {}", file.display());
                // Show the folder with the new report highlighted rather than opening it in a browser.
                if let Err(e) = store::reveal(&file) {
                    self.status.push_str(&format!(" ({e})"));
                }
            }
            Err(e) => self.status = format!("Could not create report: {e}"),
        }
        self.reports = store::reports_for(path);
    }

    pub fn show(&mut self, ui: &mut egui::Ui, settings: &Settings) -> Option<GalleryAction> {
        if self.stale || (self.entries.is_empty() && self.loaded.is_empty() && self.rx.is_none()) {
            self.refresh(ui.ctx());
        }
        self.poll(ui.ctx());
        let mut action = None;
        let mut menu_choice: Option<(PathBuf, MenuChoice)> = None;
        // Cards grow and shrink with the density setting.
        let scale = theme::density(ui.ctx()).card_scale();
        egui::CentralPanel::default().frame(theme::page_frame(ui.ctx())).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Projects").size(24.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::button(ui, ButtonKind::Text, Some(ICON_REFRESH), "Refresh").clicked() {
                        self.stale = true;
                    }
                    if theme::button(ui, ButtonKind::Text, Some(ICON_FOLDER), "Show folder").clicked() {
                        let dir = store::projects_dir();
                        let _ = std::fs::create_dir_all(&dir);
                        if let Err(e) = store::open_externally(&dir) {
                            self.status = e;
                        }
                    }
                    if theme::button(ui, ButtonKind::Tonal, Some(ICON_FOLDER_OPEN), "Open project file").clicked()
                        && let Some(p) = rfd::FileDialog::new()
                            .add_filter("WiFi Spanner project", &[store::EXTENSION])
                            .set_directory(store::projects_dir())
                            .pick_file()
                    {
                        action = Some(GalleryAction::Open(p));
                    }
                });
            });
            ui.add_space(12.0);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::splat(GAP * scale);
                    if new_card(ui, scale).clicked() {
                        action = Some(GalleryAction::New);
                    }
                    for e in &self.entries {
                        let summary = self.loaded.get(&e.path).map(|(_, s)| s);
                        let card = project_card(ui, e, summary, self.thumbs.get(&e.path), scale);
                        if card.open {
                            action = Some(GalleryAction::Open(e.path.clone()));
                        }
                        if let Some(choice) = card.menu {
                            menu_choice = Some((e.path.clone(), choice));
                        }
                    }
                });
            });
        });

        if let Some((path, choice)) = menu_choice {
            match choice {
                MenuChoice::Open => action = Some(GalleryAction::Open(path)),
                MenuChoice::Report => self.generate_report(&path),
                MenuChoice::Reports => {
                    self.reports = store::reports_for(&path);
                    self.reports_of = Some(path);
                }
                MenuChoice::ShowFile => {
                    if let Some(dir) = path.parent()
                        && let Err(e) = store::open_externally(dir)
                    {
                        self.status = e;
                    }
                }
            }
        }
        self.reports_dialog(ui.ctx());
        if let Some(mut dialog) = self.report_dialog.take() {
            match dialog.show(ui.ctx()) {
                report::DialogResult::Open => self.report_dialog = Some(dialog),
                report::DialogResult::Cancel => {}
                report::DialogResult::Generate(opts) => self.write_report(&dialog, &report::ReportStyle::from_settings(settings), &opts),
            }
        }
        action
    }

    fn reports_dialog(&mut self, ctx: &egui::Context) {
        let Some(path) = self.reports_of.clone() else { return };
        let name = path.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
        let can_report = self.loaded.get(&path).is_some_and(|(_, s)| s.points > 0);
        let mut close = false;
        let mut generate = false;
        theme::dialog(ctx, &format!("Reports: {name}"), 520.0, |ui| {
            if self.reports.is_empty() {
                ui.weak("No reports yet for this project.");
            }
            egui::ScrollArea::vertical().max_height(260.0).auto_shrink([false, true]).show(ui, |ui| {
                for r in &self.reports {
                    theme::card(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let file = r.path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
                            ui.vertical(|ui| {
                                ui.label(theme::medium(file));
                                ui.weak(r.modified.map_or(String::new(), store::format_time));
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if theme::button(ui, ButtonKind::Tonal, Some(ICON_OPEN_IN_NEW), "Open").clicked()
                                    && let Err(e) = store::open_externally(&r.path)
                                {
                                    self.status = e;
                                }
                                if theme::button(ui, ButtonKind::Text, Some(ICON_FOLDER), "Show").clicked()
                                    && let Err(e) = store::reveal(&r.path)
                                {
                                    self.status = e;
                                }
                            });
                        });
                    });
                    ui.add_space(6.0);
                }
            });
            theme::actions(ui, |ui| {
                if theme::button(ui, ButtonKind::Text, None, "Close").clicked() {
                    close = true;
                }
                if !self.reports.is_empty() && theme::button(ui, ButtonKind::Text, Some(ICON_FOLDER), "Show folder").clicked() {
                    if let Err(e) = store::open_externally(&store::reports_dir(&path)) {
                        self.status = e;
                    }
                }
                let new = ui.add_enabled_ui(can_report, |ui| theme::button(ui, ButtonKind::Filled, Some(ICON_DESCRIPTION), "Generate report")).inner;
                generate = new.clicked();
            });
        });
        if generate {
            self.generate_report(&path);
        }
        if close {
            self.reports_of = None;
        }
    }
}

enum MenuChoice {
    Open,
    Report,
    Reports,
    ShowFile,
}

struct CardResult {
    open: bool,
    menu: Option<MenuChoice>,
}

/// The "New project" tile.
fn new_card(ui: &mut egui::Ui, s: f32) -> egui::Response {
    let r = theme::roles(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(CARD_W, CARD_H) * s, Sense::click());
    let painter = ui.painter();
    let card_radius = theme::radius(ui.ctx(), theme::Shape::Card, 0.0) as f32 * s;
    painter.rect_stroke(rect, card_radius, Stroke::new(1.5, r.outline), egui::StrokeKind::Inside);
    if resp.hovered() {
        painter.rect_filled(rect, card_radius, r.primary.gamma_multiply(0.08));
    }
    theme::paint_icon(painter, ui.ctx(), ICON_ADD, rect.center() - Vec2::new(0.0, 14.0 * s), 56.0 * s, r.primary);
    painter.text(
        rect.center() + Vec2::new(0.0, 36.0 * s),
        egui::Align2::CENTER_CENTER,
        "New project",
        { let mut f = theme::medium_style().resolve(ui.style()); f.size = 15.0 * s; f },
        r.on_surface,
    );
    painter.text(
        rect.center() + Vec2::new(0.0, 58.0 * s),
        egui::Align2::CENTER_CENTER,
        "from a floor plan",
        egui::FontId::new(13.0 * s, egui::FontFamily::Proportional),
        r.on_surface_variant,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn project_card(ui: &mut egui::Ui, e: &Entry, summary: Option<&Summary>, thumb: Option<&TextureHandle>, s: f32) -> CardResult {
    let r = theme::roles(ui.ctx());
    let (card_w, thumb_h) = (CARD_W * s, THUMB_H * s);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(CARD_W, CARD_H) * s, Sense::click());
    let painter = ui.painter();
    let card_radius = theme::radius(ui.ctx(), theme::Shape::Card, 0.0) as f32 * s;
    painter.rect(rect, card_radius, r.surface_container_low, Stroke::new(1.0, r.outline_variant), egui::StrokeKind::Inside);
    if resp.hovered() {
        painter.rect_filled(rect, card_radius, r.on_surface.gamma_multiply(0.06));
    }

    // Thumbnail of the floor plan, fitted inside its well.
    let inset = 10.0 * s;
    let well = Rect::from_min_size(rect.min + Vec2::splat(inset), Vec2::new(card_w - 2.0 * inset, thumb_h));
    painter.rect_filled(well, CornerRadius::same((theme::radius(ui.ctx(), theme::Shape::Row, 0.0) as f32 * s) as u8), r.surface_container_lowest);
    match thumb {
        Some(tex) => {
            let size = tex.size_vec2();
            let fit = (well.width() / size.x).min(well.height() / size.y);
            let fitted = Rect::from_center_size(well.center(), size * fit);
            painter.image(tex.id(), fitted, Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => {
            let icon = if summary.is_some_and(|s| s.error.is_some()) { ICON_DESCRIPTION } else { ICON_IMAGE };
            theme::paint_icon(painter, ui.ctx(), icon, well.center(), 40.0 * s, r.outline_variant);
        }
    }

    // Name (one line, ellipsised) and details.
    let text_w = card_w - 2.0 * inset - 36.0 * s;
    let mut name = egui::text::LayoutJob::simple(e.name.clone(), { let mut f = theme::medium_style().resolve(ui.style()); f.size = 15.0 * s; f }, r.on_surface, text_w);
    name.wrap.max_rows = 1;
    name.wrap.break_anywhere = true;
    name.wrap.overflow_character = Some('…');
    painter.galley(rect.min + Vec2::new(12.0 * s, thumb_h + 20.0 * s), painter.layout_job(name), r.on_surface);
    let details = match summary {
        Some(s) if s.error.is_some() => "Could not be read".to_string(),
        Some(s) if s.floors > 1 => format!("{} floors  ·  {} points  ·  {}", s.floors, s.points, e.modified.map_or("unknown".into(), time_ago)),
        Some(s) => format!("{} points  ·  {}", s.points, e.modified.map_or("unknown".into(), time_ago)),
        None => "Loading...".to_string(),
    };
    let mut detail_job = egui::text::LayoutJob::simple(details, egui::FontId::new(13.0 * s, egui::FontFamily::Proportional), r.on_surface_variant, text_w);
    detail_job.wrap.max_rows = 1;
    painter.galley(rect.min + Vec2::new(12.0 * s, thumb_h + 42.0 * s), painter.layout_job(detail_job), r.on_surface_variant);

    // The "..." menu sits on top of the card, so it gets its clicks first.
    let more_rect = Rect::from_min_size(rect.right_bottom() - Vec2::new(44.0, 52.0) * s, Vec2::splat(36.0 * s));
    let more = ui.interact(more_rect, resp.id.with("more"), Sense::click());
    if more.hovered() || more.is_pointer_button_down_on() {
        painter_circle(ui, more_rect, r.on_surface.gamma_multiply(0.1));
    }
    theme::paint_icon(ui.painter(), ui.ctx(), ICON_MORE_VERT, more_rect.center(), 22.0 * s, r.on_surface_variant);
    let mut menu = None;
    let usable = summary.is_some_and(|s| s.error.is_none());
    egui::Popup::menu(&more).show(|ui| {
        ui.set_min_width(210.0);
        ui.add_enabled_ui(usable, |ui| {
            if ui.add(theme::menu_entry(ui, Some(ICON_IMAGE), "Open")).clicked() {
                menu = Some(MenuChoice::Open);
            }
            if ui.add(theme::menu_entry(ui, Some(ICON_DESCRIPTION), "Generate report")).clicked() {
                menu = Some(MenuChoice::Report);
            }
            if ui.add(theme::menu_entry(ui, Some(ICON_OPEN_IN_NEW), "Reports...")).clicked() {
                menu = Some(MenuChoice::Reports);
            }
        });
        if ui.add(theme::menu_entry(ui, Some(ICON_FOLDER), "Show in folder")).clicked() {
            menu = Some(MenuChoice::ShowFile);
        }
    });
    let open = resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && usable;
    CardResult { open, menu }
}

fn painter_circle(ui: &egui::Ui, rect: Rect, color: Color32) {
    ui.painter().circle_filled(rect.center(), rect.width() / 2.0, color);
}

/// "just now", "5 minutes ago", "3 days ago"...
fn time_ago(then: SystemTime) -> String {
    let secs = SystemTime::now().duration_since(then).map_or(0, |d| d.as_secs());
    ago(secs)
}

fn ago(secs: u64) -> String {
    let (n, unit) = match secs {
        0..=59 => return "just now".into(),
        60..=3599 => (secs / 60, "minute"),
        3600..=86_399 => (secs / 3600, "hour"),
        86_400..=2_591_999 => (secs / 86_400, "day"),
        2_592_000..=31_535_999 => (secs / 2_592_000, "month"),
        _ => (secs / 31_536_000, "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

fn load_one(path: &Path, modified: Option<SystemTime>) -> Loaded {
    let mut summary = Summary::default();
    let mut thumb = None;
    match std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|t| Project::from_json(&t).map_err(|e| e.to_string())) {
        Ok(p) => {
            summary.points = p.floors.iter().map(|f| f.points.len()).sum();
            summary.aps = p.floors.iter().flat_map(|f| f.bssids()).collect::<std::collections::BTreeSet<_>>().len();
            summary.floors = p.floors.len();
            thumb = make_thumb(&p.floor().image);
        }
        Err(e) => summary.error = Some(e),
    }
    Loaded { path: path.to_path_buf(), modified, summary, thumb }
}

/// A small RGBA version of an image, keeping its proportions.
fn make_thumb(bytes: &[u8]) -> Option<([usize; 2], Vec<u8>)> {
    let img = image::load_from_memory(bytes).ok()?.thumbnail(THUMB_MAX.0, THUMB_MAX.1).to_rgba8();
    Some(([img.width() as usize, img.height() as usize], img.into_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wifispanner_core::survey::{Floor, Observation, SurveyPoint};

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::RgbaImage::new(w, h).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    #[test]
    fn times_read_naturally() {
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(60), "1 minute ago");
        assert_eq!(ago(2 * 3600), "2 hours ago");
        assert_eq!(ago(86_400), "1 day ago");
        assert_eq!(ago(3 * 86_400), "3 days ago");
        assert_eq!(ago(40 * 86_400), "1 month ago");
        assert_eq!(ago(800 * 86_400), "2 years ago");
    }

    #[test]
    fn thumbnails_shrink_but_keep_proportions() {
        let (size, rgba) = make_thumb(&png(2000, 1000)).unwrap();
        assert_eq!(size, [480, 240]);
        assert_eq!(rgba.len(), 480 * 240 * 4);
        let (small, _) = make_thumb(&png(100, 50)).unwrap();
        assert!(small[0] <= 480 && small[1] <= 360);
        assert!(make_thumb(b"not an image").is_none());
    }

    #[test]
    fn a_project_file_gives_a_summary_and_a_thumbnail_of_its_plan() {
        let dir = std::env::temp_dir().join(format!("ws-gallery-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let point = SurveyPoint {
            x: 1.0,
            y: 2.0,
            unix_secs: 1,
            dwell_secs: 3,
            noise_dbm: None,
            observations: vec![Observation { bssid: "a".into(), ssid: None, freq_mhz: 5180, channel: 36, width_mhz: 80, signal_dbm: -50.0, samples: 1 }],
        };
        let floor = Floor { name: "x".into(), image: png(600, 800), width_px: 600, height_px: 800, metres_per_px: None, points: vec![point], walls: Vec::new(), ceiling_m: 3.0, placement: Default::default(), aps: Vec::new(), survey_aps: Vec::new(), areas: Vec::new(), zone: Vec::new() };
        let good = dir.join("good.wifispanner");
        std::fs::write(&good, Project::new(floor).to_json().unwrap()).unwrap();
        let bad = dir.join("bad.wifispanner");
        std::fs::write(&bad, "not json").unwrap();

        let loaded = load_one(&good, None);
        assert_eq!((loaded.summary.points, loaded.summary.aps, loaded.summary.error.is_none()), (1, 1, true));
        assert_eq!(loaded.thumb.unwrap().0, [270, 360], "800 tall plan fits 360 high");
        let broken = load_one(&bad, None);
        assert!(broken.summary.error.is_some() && broken.thumb.is_none());
        std::fs::remove_dir_all(dir).ok();
    }
}

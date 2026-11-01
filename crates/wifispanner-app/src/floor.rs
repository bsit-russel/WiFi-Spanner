// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! The Surveying workspace: import a plan, calibrate its scale, click spots to
//! scan, and view the measurements as a heatmap.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::Duration;
use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions, Vec2};
use wifispanner_core::heatmap::{self, Sample};
use wifispanner_core::survey::{Floor, Project, SurveyPoint, Target, ap_base_key};
use wifispanner_core::catalog::{self, ApModel};
use wifispanner_core::predict::{self, PlannedAp, Radio, RadioBand};
use wifispanner_core::obstacles::{Area, ObstacleKind};
use wifispanner_core::walls::{self, Wall};
use wifispanner_core::Material;
use wifispanner_scan::{Band, ScanResult};

use crate::settings::Settings;
use crate::theme::{self, ButtonKind};
use egui_material_icons::icons::{ICON_AUTO_FIX_HIGH, ICON_ADD, ICON_BRUSH, ICON_ARCHITECTURE, ICON_CATEGORY, ICON_CROP_FREE, ICON_EDIT, ICON_ROUTER, ICON_SMARTPHONE, ICON_ARROW_BACK, ICON_OPEN_WITH, ICON_RESTART_ALT, ICON_VISIBILITY, ICON_VISIBILITY_OFF, ICON_DELETE, ICON_DESCRIPTION, ICON_FOLDER_OPEN, ICON_IMAGE, ICON_PLACE, ICON_SAVE, ICON_STRAIGHTEN, ICON_UNDO};
use crate::{Cmd, pdf, report, store};
use crate::live::signal_color;

/// What the tab needs from the app each frame.
pub struct Env<'a> {
    pub tx: &'a Sender<Cmd>,
    pub dwell_secs: u64,
    pub scans_so_far: usize,
    pub settings: &'a Settings,
}

/// Which tab the workspace is showing: measuring a building or planning one.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WorkMode {
    Survey,
    Plan,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Survey,
    Scale,
    /// Draw walls: click the start, click the end.
    Walls,
    /// Pick walls and devices by clicking or dragging a box, then edit them together.
    Select,
    /// Line up the ghosted overlay floor with the active one by dragging it.
    Align,
    /// Place access points (installed ones while surveying, planned ones while planning).
    Aps,
    /// Draw obstacle areas (shelving, cars, foliage...).
    Obstacles,
    /// Paint where the optimiser may place access points and count coverage.
    Zone,
    /// Drop a simulated client on the plan and see how it would connect.
    Client,
}

/// Textures are capped at this size per side; positions stay in plan pixels.
const MAX_TEXTURE_SIDE: u32 = 4096;
/// Preview size when choosing a PDF page: render side and button size.
const THUMB_SIDE_PX: f32 = 360.0;
const THUMB_CELL: f32 = 150.0;
/// The page picker shows this many pages per row, then scrolls.
const PICKER_COLUMNS: usize = 4;
const PICKER_GAP: f32 = 10.0;
/// Four cells, three gaps and room for the scroll bar.
const PICKER_W: f32 = 4.0 * THUMB_CELL + 3.0 * PICKER_GAP + 34.0;
/// Heatmap grid resolution along the longer side.
const HEAT_CELLS: usize = 256;
/// The two points used to set the scale must be at least this far apart (plan
/// pixels): closer than that (a double-click, say) the result is meaningless.
const MIN_SCALE_PX: f32 = 25.0;
/// A click this close (screen pixels) to a marker selects it instead of scanning.
const PICK_RADIUS: f32 = 10.0;
/// A wall end snaps to another wall's end this close (screen pixels).
const WALL_SNAP_PX: f32 = 12.0;
/// A click this close (screen pixels) to a wall selects it.
const WALL_PICK_PX: f32 = 8.0;
/// A new wall replaces the part of an older one it lies along within this many screen pixels.
const WALL_CARVE_PX: f32 = 8.0;
/// Positions of planning access points by id: before the optimiser ran, and what it set.
struct Optimised {
    manual: Vec<(u64, [f32; 2])>,
    placed: Vec<(u64, [f32; 2])>,
    /// Access points added on the optimiser's suggestion.
    added: Vec<u64>,
}

/// What the optimiser reports, shown until dismissed: coverage, roaming, more access points it
/// recommends (waiting for a yes or no) and the channel plan.
struct Suggestion {
    extra: Vec<[f32; 2]>,
    template_id: u64,
    after: f32,
    with_extra: f32,
    roaming: f32,
    widths: Vec<(wifispanner_core::predict::RadioBand, u32)>,
    warnings: Vec<String>,
    dfs: bool,
    good_dbm: f32,
    /// The access points were only judged where they stand, after a settings change.
    rechecked: bool,
}

/// What a channel plan came to: the widths it was made at and what to warn about.
struct ChannelReport {
    widths: Vec<(wifispanner_core::predict::RadioBand, u32)>,
    warnings: Vec<String>,
}

/// Give every enabled access point its recommended channels.
fn apply_channel_plan(floor: &mut Floor, options: &wifispanner_core::channels::PlanOptions) -> ChannelReport {
    let plan = wifispanner_core::channels::plan(floor, options);
    let mut warnings = plan.notes.clone();
    for (a, b, band) in &plan.shared {
        warnings.push(format!(
            "{} and {} hear each other well but have to share a {} channel. A narrower width or lower transmit power avoids it.",
            floor.aps[*a].name,
            floor.aps[*b].name,
            band.band()
        ));
    }
    for (i, channels) in plan.assignments {
        floor.aps[i].channels = channels;
    }
    ChannelReport { widths: plan.widths, warnings }
}

fn planning_channels(mode: WorkMode) -> bool {
    mode == WorkMode::Plan
}

/// Whether the optimiser moved any access point.
fn placed_any(out: &wifispanner_core::optimise::Outcome) -> bool {
    !out.moves.is_empty()
}

/// How long the optimise button is held to restore the survey positions.
const HOLD_SECS: f32 = 0.7;
/// How many undo steps are kept.
const WALL_HISTORY: usize = 100;
/// A click this close (screen pixels) to an access point picks it.
const AP_PICK_PX: f32 = 16.0;
/// The stacked-layers picture for the floors button (white on transparent, tinted when drawn).
const LAYERS_ICON: &[u8] = include_bytes!("../assets/layers.png");
/// How far down the right edge the bubbles start, below the floors button.
/// The Add Devices picture: an access point seen from above (a circle with a smaller one inside)
/// with a plus over its lower right, separated from it by a border in the button's own colour.
fn paint_add_device(painter: &egui::Painter, centre: Pos2, size: f32, colour: Color32, background: Color32) {
    let (ring, c) = (size * 0.5, centre - Vec2::splat(size * 0.06));
    painter.circle_stroke(c, ring, Stroke::new(1.8, colour));
    painter.circle_stroke(c, ring * 0.45, Stroke::new(1.8, colour));
    let (p, arm) = (c + Vec2::splat(ring * 0.72), size * 0.3);
    for stroke in [Stroke::new(5.5, background), Stroke::new(2.2, colour)] {
        painter.line_segment([p - Vec2::new(arm, 0.0), p + Vec2::new(arm, 0.0)], stroke);
        painter.line_segment([p - Vec2::new(0.0, arm), p + Vec2::new(0.0, arm)], stroke);
    }
}

/// The Select picture: a dashed selection box with a mouse pointer over its lower right, the
/// pointer edged in the button's colour to stand clear of the box.
fn paint_select(painter: &egui::Painter, centre: Pos2, size: f32, colour: Color32, background: Color32) {
    let half = size * 0.42;
    let c = centre - Vec2::splat(size * 0.04);
    let corners = [c + Vec2::new(-half, -half), c + Vec2::new(half, -half), c + Vec2::new(half, half), c + Vec2::new(-half, half), c + Vec2::new(-half, -half)];
    painter.extend(egui::Shape::dashed_line(&corners, Stroke::new(1.6, colour), size * 0.17, size * 0.12));
    let k = size * 0.62;
    let tip = c + Vec2::new(size * 0.02, -size * 0.02);
    let pointer: Vec<Pos2> = [(0.0, 0.0), (0.0, 0.82), (0.2, 0.65), (0.36, 1.0), (0.5, 0.93), (0.35, 0.59), (0.62, 0.59)]
        .iter()
        .map(|(x, y)| tip + Vec2::new(*x, *y) * k)
        .collect();
    painter.add(egui::Shape::convex_polygon(pointer.clone(), background, Stroke::new(3.5, background)));
    painter.add(egui::Shape::closed_line(pointer.clone(), Stroke::new(3.5, background)));
    painter.add(egui::Shape::convex_polygon(pointer, colour, Stroke::new(0.8, colour)));
}

/// Keep a typed distance to digits, one comma or point, and at most three decimals.
fn clean_distance(text: &mut String) {
    let mut out = String::new();
    let (mut sep, mut decimals) = (false, 0);
    for c in text.chars() {
        match c {
            '0'..='9' if !sep && out.len() < 7 => out.push(c),
            '0'..='9' if sep && decimals < 3 => {
                out.push(c);
                decimals += 1;
            }
            ',' | '.' if !sep && !out.is_empty() => {
                out.push(c);
                sep = true;
            }
            _ => {}
        }
    }
    if *text != out {
        *text = out;
    }
}

/// Size of the round buttons over the plan, with the density.
fn round_size(ctx: &egui::Context) -> f32 {
    theme::density(ctx).button_height() + 4.0
}

/// How far down the right edge the menus start, below the round buttons: the floors button,
/// then Select, and in Planning the devices and optimise buttons as well.
fn right_top(ctx: &egui::Context, planning: bool) -> f32 {
    let buttons = if planning { 4.0 } else { 2.0 };
    12.0 + buttons * round_size(ctx) + 8.0 * buttons
}

/// The scale (metres per plan pixel) implied by `metres` between two points
/// `px_apart` pixels apart on a plan of `w` x `h` pixels, or why it is not
/// believable.
fn check_scale(px_apart: f32, metres: f32, w: u32, h: u32) -> Result<f32, String> {
    if px_apart < MIN_SCALE_PX {
        return Err(format!("The two points are only {px_apart:.0} px apart. Cancel and click two points much further apart."));
    }
    let mpp = metres / px_apart;
    if !heatmap::scale_is_plausible(mpp, w, h) {
        let plan_m = w.max(h) as f32 * mpp;
        return Err(format!("That would make the plan {plan_m:.0} m across, which looks wrong. Check the distance you entered."));
    }
    Ok(mpp)
}

pub struct FloorTab {
    project: Option<Project>,
    path: Option<PathBuf>,
    texture: Option<TextureHandle>,
    offset: Vec2,
    zoom: f32,
    needs_fit: bool,
    tool: Tool,
    scale_clicks: Vec<Pos2>,
    scale_input: String,
    selected: Option<usize>,
    pending: Option<Pos2>,
    show_heat: bool,
    target: Target,
    band: Option<Band>,
    opacity: f32,
    heat_texture: Option<TextureHandle>,
    heat_dirty: bool,
    status: String,
    /// The last status shown as a snackbar, to spot changes.
    toasted: String,
    /// A project was just loaded, so the app should show the workspace.
    opened: bool,
    /// The user asked to go back to the project gallery.
    back: bool,
    /// Display settings from the settings dialog, copied in each frame.
    view: Settings,
    pdf_picker: Option<PdfPicker>,
    mode: WorkMode,
    /// Planning: the AP picked or being dragged, what new APs start with, and the prediction shown.
    aps_selected: BTreeSet<usize>,
    ap_drag: Option<usize>,
    ap_box_start: Option<Pos2>,
    /// New access points start like the last one edited.
    ap_template: PlannedAp,
    /// The model catalogue (built in plus the user's own), the user's own, and the open picker's search.
    models: Vec<ApModel>,
    user_models: Vec<ApModel>,
    model_picker: Option<String>,
    show_pred: bool,
    pred_band: Band,
    rx_height: f32,
    pred_texture: Option<TextureHandle>,
    pred_dirty: bool,
    pred_note: Option<String>,
    /// Another floor shown ghosted over the active one, and how strongly.
    overlay: Option<usize>,
    overlay_opacity: f32,
    floor_textures: HashMap<usize, TextureHandle>,
    /// The next plan that loads becomes a new floor instead of a new project.
    adding_floor: bool,
    rename: Option<String>,
    /// The floors panel is showing, and the picture on the button that opens it.
    floors_open: bool,
    /// The Add Devices menu is showing (it sits behind its own button).
    devices_open: bool,
    /// The optimiser: running on a thread, and (once applied) where the access points stood before and where it put them.
    optimising: Option<Receiver<Result<wifispanner_core::optimise::Outcome, String>>>,
    suggestion: Option<Suggestion>,
    /// The optimiser is running only to re-judge the layout (after a settings change), not to move anything.
    optimise_checking: bool,
    /// The channel settings last applied, to notice a change.
    last_plan_options: Option<wifispanner_core::channels::PlanOptions>,
    optimised: Option<Optimised>,
    /// When the optimise button was pressed down, and whether that press already did its long-press job.
    optimise_hold: Option<(std::time::Instant, bool)>,
    /// Reading the plan's dimensions for its scale: the thread's answer, what it found, and why it failed.
    autoscale_rx: Option<Receiver<Result<crate::autoscale::Detected, String>>>,
    autoscale_found: Option<crate::autoscale::Detected>,
    autoscale_error: Option<String>,
    /// Looking for walls on the plan picture: the thread's answer, what it found, and why it failed.
    wallscan_rx: Option<Receiver<Result<crate::wallscan::Found, String>>>,
    wallscan_found: Option<crate::wallscan::Found>,
    wallscan_error: Option<String>,
    layers_icon: Option<TextureHandle>,
    /// Wall drawing: the material and height for the next wall, where the wall
    /// being drawn started, and the wall picked in the list.
    wall_material: Material,
    wall_height: Option<f32>,
    wall_start: Option<Pos2>,
    wall_mode: WallMode,
    /// Obstacle areas: what the next one is, how it is drawn, and its half-drawn corners.
    obstacle_kind: ObstacleKind,
    obstacle_height: Option<f32>,
    area_mode: AreaMode,
    area_start: Option<Pos2>,
    poly_points: Vec<Pos2>,
    selected_areas: BTreeSet<usize>,
    /// The simulated client: where it is, which device it is and (for a custom one) its bands.
    client: Option<Pos2>,
    client_kind: usize,
    client_bands: [bool; 3],
    /// Shift is held: no snapping.
    no_snap: bool,
    model_search: String,
    selected_walls: BTreeSet<usize>,
    /// Earlier states of the walls, for undo.
    wall_history: Vec<(Vec<Wall>, Vec<Area>)>,
    last_wall_edit: Option<std::time::Instant>,
    /// Plan position where a selection box was started.
    box_start: Option<Pos2>,
    /// A wall or obstacle area is being dragged (moved), not a box drawn.
    obj_drag: bool,
    /// The Coverage tool: rubbing out instead of painting, the brush width in metres, the stroke
    /// being drawn, and the picture of what is painted.
    zone_erase: bool,
    zone_brush_m: f32,
    zone_stroke: Vec<[f32; 2]>,
    zone_tex: Option<TextureHandle>,
    zone_dirty: bool,
    /// The tool to go back to once the area is painted for the optimiser.
    zone_return: Option<Tool>,
    /// The kind of object the Select tool works on: that of the tool it was chosen from (all when none).
    select_only: Option<Tool>,
    report_dialog: Option<report::OptionsDialog>,
}

/// How an obstacle area is drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AreaMode {
    Rectangle,
    Polygon,
}

/// Client devices to simulate: name and the bands each can use (2.4, 5, 6 GHz).
const CLIENTS: [(&str, [bool; 3]); 6] = [
    ("WiFi 7 Device", [true, true, true]),
    ("WiFi 6E Device", [true, true, true]),
    ("WiFi 6 Device", [true, true, false]),
    ("WiFi 5 Device", [true, true, false]),
    ("WiFi 4 Device", [true, true, false]),
    ("Custom Client", [true, true, false]),
];
const CUSTOM_CLIENT: usize = 5;
/// Below this a client cannot keep a connection.
const NO_COVERAGE_DBM: f32 = -85.0;

fn rating(dbm: f32) -> &'static str {
    match dbm {
        d if d >= -55.0 => "excellent",
        d if d >= -67.0 => "good",
        d if d >= -75.0 => "fair",
        d if d >= -82.0 => "weak",
        _ => "very weak",
    }
}

fn rating_colour(dbm: f32) -> Color32 {
    match dbm {
        d if d >= -55.0 => Color32::from_rgb(60, 180, 100),
        d if d >= -67.0 => Color32::from_rgb(140, 190, 70),
        d if d >= -75.0 => Color32::from_rgb(230, 170, 40),
        d if d >= -82.0 => Color32::from_rgb(235, 120, 50),
        _ => Color32::from_rgb(220, 70, 60),
    }
}

/// What the wall tool draws.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WallMode {
    Wall,
    Room,
}

/// What the access point palette asked for.
enum ApEdit {
    /// The model new devices start as (or custom power when `None`).
    Template(Option<ApModel>),
    Search(String),
    Name(String),
    Height(f32),
    Tx(f32),
    Gain(f32),
    Enabled(bool),
    Existing(bool),
    Delete,
    Duplicate,
    Model(ApModel),
    Custom,
    PickModel,
    SaveModel,
    Link(Option<String>),
}

/// What the floors palette asked for.
enum FloorEdit {
    Activate(usize),
    ToggleOverlay(usize),
    Add,
    Rename,
    Remove,
    Align,
    Rotate(f32),
    ResetPlacement,
    Opacity(f32),
}

/// What the wall palette asked for.
enum WallEdit {
    Mode(WallMode),
    Material(Material),
    Height(Option<f32>),
    AreaMode(AreaMode),
    AreaKind(ObstacleKind),
    AreaHeight(Option<f32>),
    ClientKind(usize),
    ClientBand(usize, bool),
    ClientHeight(f32),
    Ceiling(f32),
    AutoScale,
    DetectWalls,
    ZoneErase(bool),
    ZoneBrush(f32),
    ZoneUndo,
    ZoneClear,
    ZoneRun,
    ZoneCancel,
    Undo,
    Delete,
}

/// Thumbnails of a multi-page PDF, shown so the user can pick a page.
struct PdfPicker {
    name: String,
    bytes: Vec<u8>,
    selected: usize,
    thumbs: Vec<Option<TextureHandle>>,
    rx: Receiver<(usize, Vec<u8>)>,
}

impl Default for FloorTab {
    fn default() -> Self {
        FloorTab {
            project: None,
            path: None,
            texture: None,
            offset: Vec2::ZERO,
            zoom: 1.0,
            needs_fit: true,
            tool: Tool::Survey,
            scale_clicks: Vec::new(),
            scale_input: String::new(),
            selected: None,
            pending: None,
            show_heat: false,
            target: Target::Any,
            band: None,
            opacity: 0.65,
            heat_texture: None,
            heat_dirty: true,
            status: "Open a floor plan image to begin.".into(),
            toasted: "Open a floor plan image to begin.".into(),
            opened: false,
            back: false,
            view: Settings::default(),
            pdf_picker: None,
            mode: WorkMode::Survey,
            aps_selected: BTreeSet::new(),
            ap_drag: None,
            ap_box_start: None,
            ap_template: PlannedAp::new(String::new(), 0.0, 0.0),
            models: Vec::new(),
            user_models: Vec::new(),
            model_picker: None,
            show_pred: true,
            pred_band: Band::Ghz5,
            rx_height: predict::DEFAULT_RX_HEIGHT_M,
            pred_texture: None,
            pred_dirty: true,
            pred_note: None,
            overlay: None,
            overlay_opacity: 0.5,
            floor_textures: HashMap::new(),
            adding_floor: false,
            rename: None,
            floors_open: false,
            devices_open: false,
            optimising: None,
            suggestion: None,
            optimise_checking: false,
            last_plan_options: None,
            optimised: None,
            optimise_hold: None,
            autoscale_rx: None,
            autoscale_found: None,
            autoscale_error: None,
            wallscan_rx: None,
            wallscan_found: None,
            wallscan_error: None,
            layers_icon: None,
            wall_material: Material::Concrete,
            wall_height: Material::Concrete.default_height_m(),
            wall_start: None,
            wall_mode: WallMode::Wall,
            obstacle_kind: ObstacleKind::ShelfWarehouse,
            obstacle_height: ObstacleKind::ShelfWarehouse.default_height_m(),
            area_mode: AreaMode::Rectangle,
            area_start: None,
            poly_points: Vec::new(),
            selected_areas: BTreeSet::new(),
            client: None,
            client_kind: 0,
            client_bands: [true, true, false],
            no_snap: false,
            model_search: String::new(),
            selected_walls: BTreeSet::new(),
            wall_history: Vec::new(),
            last_wall_edit: None,
            box_start: None,
            obj_drag: false,
            zone_erase: false,
            zone_brush_m: 1.0,
            zone_stroke: Vec::new(),
            zone_tex: None,
            zone_dirty: true,
            zone_return: None,
            select_only: None,
            report_dialog: None,
        }
    }
}

impl FloorTab {
    pub fn add_point(&mut self, x: f32, y: f32, dwell_secs: u64, scan: &ScanResult) {
        self.pending = None;
        let Some(p) = self.project.as_mut() else { return };
        let point = SurveyPoint::from_scan(x, y, dwell_secs, scan);
        self.status = format!("Point {} saved: {} APs heard.", p.floor().points.len() + 1, point.observations.len());
        p.floor_mut().points.push(point);
        self.selected = Some(p.floor().points.len() - 1);
        self.heat_dirty = true;
        self.autosave();
    }

    /// True once after a project was loaded.
    pub fn take_opened(&mut self) -> bool {
        std::mem::take(&mut self.opened)
    }

    /// True once after the user pressed the back button.
    pub fn take_back(&mut self) -> bool {
        std::mem::take(&mut self.back)
    }

    /// A new status message since the last call, for a snackbar.
    pub fn take_toast(&mut self) -> Option<String> {
        if self.status == self.toasted {
            return None;
        }
        self.toasted = self.status.clone();
        (!self.status.is_empty()).then(|| self.status.clone())
    }

    pub fn scan_failed(&mut self) {
        self.pending = None;
        self.status = "Scan failed, no point was saved.".into();
    }

    fn autosave(&mut self) {
        self.pred_dirty = true;
        if let Some(path) = self.path.clone() {
            self.save_to(&path);
        }
    }

    fn save_to(&mut self, path: &PathBuf) {
        let Some(p) = &self.project else { return };
        match p.to_json().map_err(|e| e.to_string()).and_then(|j| std::fs::write(path, j).map_err(|e| e.to_string())) {
            Ok(()) => self.path = Some(path.clone()),
            Err(e) => self.status = format!("Could not save {}: {e}", path.display()),
        }
    }

    fn load_plan(&mut self, ctx: &egui::Context, path: &std::path::Path) {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => return self.status = format!("Could not read {}: {e}", path.display()),
        };
        let name = path.file_stem().map_or("Floor".into(), |s| s.to_string_lossy().into_owned());
        if !pdf::is_pdf(&bytes) {
            return self.finish_plan(ctx, bytes, name, "");
        }
        match pdf::page_count(&bytes) {
            Err(e) => self.status = format!("Could not import PDF: {e}"),
            Ok(1) => self.import_pdf_page(ctx, bytes, 0, 1, name),
            Ok(n) => {
                let (tx, rx) = mpsc::channel();
                let (data, repaint) = (bytes.clone(), ctx.clone());
                std::thread::spawn(move || {
                    pdf::thumbnails(data, THUMB_SIDE_PX, |i, png| {
                        let open = tx.send((i, png)).is_ok();
                        repaint.request_repaint();
                        open
                    })
                });
                self.pdf_picker = Some(PdfPicker { name, bytes, selected: 0, thumbs: vec![None; n], rx });
            }
        }
    }

    fn import_pdf_page(&mut self, ctx: &egui::Context, bytes: Vec<u8>, index: usize, count: usize, name: String) {
        match pdf::render_page(bytes, index) {
            Ok(png) => {
                let note = if count > 1 { format!(" Imported page {} of {count}.", index + 1) } else { String::new() };
                self.finish_plan(ctx, png, name, &note);
            }
            Err(e) => self.status = format!("Could not import PDF page: {e}"),
        }
    }

    fn finish_plan(&mut self, ctx: &egui::Context, bytes: Vec<u8>, name: String, note: &str) {
        let dims = image::load_from_memory(&bytes).map(|i| (i.width(), i.height()));
        let (w, h) = match dims {
            Ok(d) => d,
            Err(e) => return self.status = format!("Not a usable image: {e}"),
        };
        let floor = Floor { name, image: bytes, width_px: w, height_px: h, metres_per_px: None, points: Vec::new(), walls: Vec::new(), ceiling_m: wifispanner_core::walls::DEFAULT_CEILING_M, placement: Default::default(), aps: Vec::new(), survey_aps: Vec::new(), areas: Vec::new(), zone: Vec::new() };
        if std::mem::take(&mut self.adding_floor)
            && let Some(p) = self.project.as_mut()
        {
            let below = p.active;
            p.add_floor(floor);
            self.overlay = Some(below);
            self.after_floor_change(ctx);
            self.status = format!("Floor added.{note} Set its scale, then use the floors panel to overlay the floor below and line them up.");
            self.tool = Tool::Scale;
            self.autosave();
            return;
        }
        let name = floor.name.clone();
        self.set_project(ctx, Project::new(floor), None);
        self.status = format!("Floor plan loaded.{note} Set the scale first: click two points a known distance apart.");
        self.tool = Tool::Scale;
        // Create the project file straight away so every point is autosaved.
        match store::new_project_path(&name) {
            Ok(path) => {
                self.save_to(&path);
                if self.path.is_some() {
                    self.status.push_str(&format!(" Saved as {}.", path.display()));
                }
            }
            Err(e) => self.status.push_str(&format!(" Could not create a project file ({e}); use Save as."))
        }
    }

    fn pdf_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut picker) = self.pdf_picker.take() else { return };
        while let Ok((i, png)) = picker.rx.try_recv() {
            if let Some(slot) = picker.thumbs.get_mut(i) {
                *slot = decode_texture(ctx, &png, &format!("pdf-thumb-{i}"));
            }
        }
        let count = picker.thumbs.len();
        let (mut import, mut cancel) = (false, false);
        theme::dialog(ctx, "Choose a page to import", PICKER_W, |ui| {
            ui.label(format!("{} has {count} pages. Click a page, then Import (or double-click it).", picker.name));
            let loaded = picker.thumbs.iter().filter(|t| t.is_some()).count();
            if loaded < count {
                ui.weak(format!("Rendering previews... {loaded} of {count}"));
            }
            ui.add_space(8.0);
            // Four pages across, then a list that scrolls up and down, however many pages there are.
            let height = (ctx.content_rect().height() - 320.0).clamp(180.0, 520.0);
            egui::ScrollArea::vertical().max_height(height).auto_shrink([false, true]).show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(PICKER_GAP);
                // Room above the first row so the selection ring is not clipped.
                ui.add_space(5.0);
                for first in (0..count).step_by(PICKER_COLUMNS) {
                    ui.horizontal(|ui| {
                        for i in first..(first + PICKER_COLUMNS).min(count) {
                            ui.vertical(|ui| {
                                let selected = picker.selected == i;
                                let size = Vec2::splat(THUMB_CELL);
                                let resp = match &picker.thumbs[i] {
                                    Some(t) => ui.add(egui::Button::image(egui::Image::new(t).max_size(size)).selected(selected).min_size(size)),
                                    None => ui.add_sized(size, egui::Button::new("Loading...").selected(selected)),
                                };
                                if selected {
                                    // A bold ring in the primary colour, so the chosen page is unmistakable.
                                    let primary = theme::roles(ui.ctx()).primary;
                                    ui.painter().rect_stroke(resp.rect.expand(1.5), 20.0, Stroke::new(3.0, primary), egui::StrokeKind::Outside);
                                    ui.label(theme::medium(format!("Page {}", i + 1)).color(primary));
                                } else {
                                    ui.label(format!("Page {}", i + 1));
                                }
                                if resp.clicked() {
                                    picker.selected = i;
                                }
                                if resp.double_clicked() {
                                    picker.selected = i;
                                    import = true;
                                }
                            });
                        }
                    });
                }
            });
            theme::actions(ui, |ui| {
                if theme::button(ui, ButtonKind::Filled, None, &format!("Import page {}", picker.selected + 1)).clicked() {
                    import = true;
                }
                cancel = theme::button(ui, ButtonKind::Text, None, "Cancel").clicked();
            });
        });
        if import {
            let PdfPicker { name, bytes, selected, .. } = picker;
            self.import_pdf_page(ctx, bytes, selected, count, name);
        } else if !cancel {
            self.pdf_picker = Some(picker);
        }
    }

    /// A project is open (so its file must stay where it is).
    pub fn has_open_project(&self) -> bool {
        self.path.is_some()
    }

    pub fn open_project(&mut self, ctx: &egui::Context, path: PathBuf) {
        let loaded = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| Project::from_json(&t).map_err(|e| e.to_string()));
        match loaded {
            Ok(p) => {
                self.set_project(ctx, p, Some(path));
                self.status = "Project opened.".into();
            }
            Err(e) => self.status = format!("Could not open project: {e}"),
        }
    }

    fn set_project(&mut self, ctx: &egui::Context, project: Project, path: Option<PathBuf>) {
        self.opened = true;
        self.texture = decode_texture(ctx, &project.floor().image, "floor-plan");
        self.project = Some(project);
        self.path = path;
        self.selected = None;
        self.pending = None;
        self.scale_clicks.clear();
        self.needs_fit = true;
        self.heat_dirty = true;
        self.tool = Tool::Survey;
        self.target = Target::Any;
        self.band = None;
    }

    /// Reload everything that follows the active floor.
    fn after_floor_change(&mut self, ctx: &egui::Context) {
        self.floor_textures.clear();
        self.texture = self.project.as_ref().and_then(|p| decode_texture(ctx, &p.floor().image, "floor-plan"));
        self.zone_dirty = true;
        self.zone_stroke.clear();
        self.selected = None;
        self.pending = None;
        self.scale_clicks.clear();
        self.selected_walls.clear();
        self.selected_areas.clear();
        self.client = None;
        self.wall_start = None;
        self.wall_history.clear();
        self.aps_selected.clear();
        self.pred_dirty = true;
        self.needs_fit = true;
        self.heat_dirty = true;
        let active = self.project.as_ref().map_or(0, |p| p.active);
        if self.overlay.is_some_and(|o| o == active || self.project.as_ref().is_none_or(|p| o >= p.floors.len())) {
            self.overlay = None;
        }
        if self.overlay.is_none() && self.tool == Tool::Align {
            self.tool = Tool::Survey;
        }
    }

    fn set_active(&mut self, ctx: &egui::Context, i: usize) {
        if let Some(p) = self.project.as_mut()
            && i < p.floors.len()
            && p.active != i
        {
            p.active = i;
            self.after_floor_change(ctx);
            self.autosave();
        }
    }

    /// The overlay floor and its picture, when one is being shown.
    fn overlay_texture(&mut self, ctx: &egui::Context) -> Option<(usize, TextureHandle)> {
        let p = self.project.as_ref()?;
        let o = self.overlay.filter(|&o| o != p.active && o < p.floors.len())?;
        if !self.floor_textures.contains_key(&o) {
            let tex = decode_texture(ctx, &p.floors[o].image, &format!("floor-overlay-{o}"))?;
            self.floor_textures.insert(o, tex);
        }
        Some((o, self.floor_textures[&o].clone()))
    }

    /// Ask for a plan to add as another floor of this project.
    fn add_floor_pick(&mut self, ctx: &egui::Context) {
        if let Some(path) = rfd::FileDialog::new().add_filter("Floor plans (PNG, JPG, PDF)", &["png", "jpg", "jpeg", "pdf"]).set_title("Add a floor").pick_file() {
            self.adding_floor = true;
            self.load_plan(ctx, &path);
        }
    }

    fn apply_floor_edit(&mut self, ctx: &egui::Context, edit: FloorEdit) {
        match edit {
            FloorEdit::Activate(i) => self.set_active(ctx, i),
            FloorEdit::ToggleOverlay(i) => {
                self.overlay = if self.overlay == Some(i) { None } else { Some(i) };
                if self.overlay.is_none() && self.tool == Tool::Align {
                    self.tool = Tool::Survey;
                }
            }
            FloorEdit::Add => self.add_floor_pick(ctx),
            FloorEdit::Rename => self.rename = self.project.as_ref().map(|p| p.floor().name.clone()),
            FloorEdit::Remove => {
                let Some(p) = self.project.as_ref() else { return };
                let (i, f) = (p.active, p.floor());
                let ok = rfd::MessageDialog::new()
                    .set_title("Remove floor?")
                    .set_description(format!("Remove \"{}\" with its {} survey points and {} walls? This cannot be undone.", f.name, f.points.len(), f.walls.len()))
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    == rfd::MessageDialogResult::Yes;
                if ok && let Some(p) = self.project.as_mut() {
                    p.remove_floor(i);
                    self.overlay = None;
                    self.after_floor_change(ctx);
                    self.autosave();
                }
            }
            FloorEdit::Align => self.tool = if self.tool == Tool::Align { Tool::Survey } else { Tool::Align },
            FloorEdit::Rotate(deg) => {
                if let (Some(o), Some(p)) = (self.overlay, self.project.as_mut()) {
                    p.rotate_floor(o, deg);
                    self.autosave();
                }
            }
            FloorEdit::ResetPlacement => {
                if let (Some(o), Some(p)) = (self.overlay, self.project.as_mut()) {
                    p.floors[o].placement = Default::default();
                    self.autosave();
                }
            }
            FloorEdit::Opacity(a) => self.overlay_opacity = a,
        }
    }

    /// The floors panel floating over the left of the plan: switch floor, add
    /// one, and overlay another to line them up.
    fn floors_bubble(&self, ctx: &egui::Context, canvas: Rect) -> Vec<FloorEdit> {
        let r = theme::roles(ctx);
        let Some(p) = self.project.as_ref() else { return Vec::new() };
        let mut edits = Vec::new();
        egui::Area::new(egui::Id::unique("floors_bubble"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(canvas.right_top() + Vec2::new(-12.0, right_top(ctx, self.mode == WorkMode::Plan)))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(r.surface_container_high)
                    .stroke(Stroke::new(1.0, r.outline_variant))
                    .corner_radius(theme::radius(ctx, theme::Shape::Panel, 0.0))
                    .inner_margin(14)
                    .shadow(theme::shadow(r.shadow, 16, 0.18))
                    .show(ui, |ui| {
                        ui.set_width(210.0);
                        ui.label(theme::heading(ui, "Floors"));
                        ui.add_space(4.0);
                        // Highest floor first, like the building.
                        for (i, fl) in p.floors.iter().enumerate().rev() {
                            ui.horizontal(|ui| {
                                if i == p.active {
                                    ui.add_space(26.0);
                                } else {
                                    let shown = self.overlay == Some(i);
                                    let icon = if shown { ICON_VISIBILITY } else { ICON_VISIBILITY_OFF };
                                    let (eye, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
                                    theme::paint_icon(ui.painter(), ctx, icon, eye.center(), 18.0, if resp.hovered() { r.on_surface } else { r.on_surface_variant });
                                    if resp.on_hover_text("Overlay this floor, ghosted, over the active one").clicked() {
                                        edits.push(FloorEdit::ToggleOverlay(i));
                                    }
                                }
                                if ui.add_sized([150.0, 26.0], egui::Button::selectable(i == p.active, &fl.name).frame_when_inactive(false)).clicked() {
                                    edits.push(FloorEdit::Activate(i));
                                }
                            });
                        }
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if theme::button(ui, ButtonKind::Tonal, Some(ICON_ADD), "Add floor").clicked() {
                                edits.push(FloorEdit::Add);
                            }
                        });
                        ui.horizontal(|ui| {
                            if theme::button(ui, ButtonKind::Text, None, "Rename").clicked() {
                                edits.push(FloorEdit::Rename);
                            }
                            if ui.add_enabled_ui(p.floors.len() > 1, |ui| theme::button(ui, ButtonKind::Text, None, "Remove")).inner.clicked() {
                                edits.push(FloorEdit::Remove);
                            }
                        });
                        if let Some(o) = self.overlay.filter(|&o| o != p.active && o < p.floors.len()) {
                            ui.separator();
                            ui.label(egui::RichText::new(format!("Overlay: {}", p.floors[o].name)).text_style(egui::TextStyle::Body));
                            let mut a = self.overlay_opacity;
                            ui.spacing_mut().slider_width = 130.0;
                            if theme::slider_f32(ui, &mut a, 0.1..=1.0, 2, "Opacity") {
                                edits.push(FloorEdit::Opacity(a));
                            }
                            if theme::chip(ui, self.tool == Tool::Align, Some(ICON_OPEN_WITH), "Align").clicked() {
                                edits.push(FloorEdit::Align);
                            }
                            if self.tool == Tool::Align {
                                ui.label(egui::RichText::new("Drag the ghost floor to line it up with this one. Pan with the right mouse button.").text_style(egui::TextStyle::Body).color(r.on_surface_variant));
                                let mut deg = p.floors[o].placement.rotation_deg;
                                ui.horizontal(|ui| {
                                    ui.label("Rotate");
                                    if theme::field(ui, egui::DragValue::new(&mut deg).range(-180.0..=180.0).speed(0.1).suffix(" deg").fixed_decimals(1)).changed() {
                                        edits.push(FloorEdit::Rotate(deg));
                                    }
                                });
                                if theme::button(ui, ButtonKind::Text, Some(ICON_RESTART_ALT), "Reset").clicked() {
                                    edits.push(FloorEdit::ResetPlacement);
                                }
                            }
                        }
                    });
            });
        edits
    }

    fn rename_dialog(&mut self, ctx: &egui::Context) {
        let Some(name) = self.rename.as_mut() else { return };
        let (mut ok, mut cancel) = (false, false);
        theme::dialog(ctx, "Rename floor", 360.0, |ui| {
            let edit = theme::field(ui, egui::TextEdit::singleline(name).desired_width(f32::INFINITY));
            edit.request_focus();
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            theme::actions(ui, |ui| {
                ok = theme::button(ui, ButtonKind::Filled, None, "Rename").clicked() || enter;
                cancel = theme::button(ui, ButtonKind::Text, None, "Cancel").clicked();
            });
        });
        if ok {
            let new = self.rename.take().unwrap_or_default();
            if !new.trim().is_empty()
                && let Some(p) = self.project.as_mut()
            {
                p.floor_mut().name = new.trim().to_string();
                self.autosave();
            }
        } else if cancel {
            self.rename = None;
        }
    }

    /// Switch between the Surveying and Planning views of the open project.
    pub fn set_mode(&mut self, mode: WorkMode) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        self.tool = if mode == WorkMode::Plan { Tool::Aps } else { Tool::Survey };
        self.wall_start = None;
        self.selected_walls.clear();
        self.aps_selected.clear();
        self.pred_dirty = true;
    }

    pub fn show(&mut self, ui: &mut egui::Ui, env: &Env) {
        if self.view != *env.settings {
            self.view = env.settings.clone();
            self.heat_dirty = true;
        }
        let roles = theme::roles(ui.ctx());
        let tools = egui::Frame::new().fill(roles.surface).inner_margin(egui::Margin::symmetric(16, 10));
        let canvas = egui::Frame::new().fill(roles.surface_container_lowest);
        egui::Panel::top("floor_tools").frame(tools).show(ui, |ui| self.toolbar(ui));
        egui::CentralPanel::default().frame(canvas).show(ui, |ui| self.canvas(ui, env));
        self.show_dialogs(ui.ctx());
    }

    /// The plan's scale, for the status bar: how it is set, or why it looks wrong.
    pub fn scale_label(&self, ui: &mut egui::Ui) {
        let Some(f) = self.project.as_ref().map(|p| p.floor()) else { return };
        let error = theme::roles(ui.ctx()).error;
        let (text, colour, hover) = match f.metres_per_px {
            Some(m) if !heatmap::scale_is_plausible(m, f.width_px, f.height_px) => (
                "Scale looks wrong".to_string(),
                Some(error),
                Some(format!(
                    "The scale makes this plan {:.0} m across. Choose Set scale and click two points a known distance apart to redo it.",
                    f.width_px.max(f.height_px) as f32 * m
                )),
            ),
            Some(m) => (format!("Scale: {:.1} px/m", 1.0 / m), None, None),
            None => ("Scale not set".to_string(), Some(Color32::from_rgb(240, 140, 50)), None),
        };
        let mut rich = egui::RichText::new(text);
        if let Some(c) = colour {
            rich = rich.color(c);
        }
        let label = ui.add(egui::Label::new(rich).wrap_mode(egui::TextWrapMode::Extend));
        if let Some(h) = hover {
            label.on_hover_text(h);
        }
    }

    /// The scale and PDF page dialogs, shown whichever view is up.
    pub fn show_dialogs(&mut self, ctx: &egui::Context) {
        self.scale_dialog(ctx);
        self.pdf_dialog(ctx);
        self.report_dialog(ctx);
        self.rename_dialog(ctx);
        self.model_dialog(ctx);
    }

    /// Ask for a floor plan file (PNG, JPG or PDF) and load it.
    pub fn pick_plan(&mut self, ctx: &egui::Context) {
        if self.confirm_discard()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Floor plans (PNG, JPG, PDF)", &["png", "jpg", "jpeg", "pdf"])
                .set_title("Open floor plan")
                .pick_file()
        {
            self.load_plan(ctx, &path);
        }
    }

    /// Ask for a saved project file and open it.
    pub fn pick_project(&mut self, ctx: &egui::Context) {
        if self.confirm_discard()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("WiFi Spanner project", &[store::EXTENSION])
                .set_directory(store::projects_dir())
                .pick_file()
        {
            self.open_project(ctx, path);
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // Fields in the bars are as tall as the buttons for the chosen density.
        ui.spacing_mut().interact_size.y = theme::density(&ctx).button_height();
        let (tool, survey, enabled) = (self.tool, self.mode == WorkMode::Survey, self.project.is_some());
        let (mut back, mut picked) = (false, None);
        let size = theme::density(&ctx).button_height();
        ui.horizontal(|ui| {
            // The tools get the width left after the button, and scroll when it is short.
            let room = (ui.available_width() - size - ui.spacing().item_spacing.x).max(100.0);
            ui.allocate_ui_with_layout(egui::vec2(room, size), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                egui::ScrollArea::horizontal().auto_shrink([false, true]).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        back = theme::button(ui, ButtonKind::Text, Some(ICON_ARROW_BACK), "Projects").clicked();
                        ui.separator();
                        // While selecting, the chip of the kind being selected stays lit as well.
                        let lit = if tool == Tool::Select { self.select_only.unwrap_or(tool) } else { tool };
                        picked = Self::tool_chips(ui, lit, survey, enabled);
                    });
                });
            });
            self.settings_button(ui, &ctx);
        });
        if back {
            self.back = true;
        }
        if let Some(t) = picked {
            self.set_tool(t);
            if t == Tool::Aps && !survey {
                self.devices_open = true;
            }
        }
    }

    /// The button at the right of the bar, three dots in a triangle, and the
    /// menu it opens: the project's file actions and the view controls.
    fn settings_button(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let r = theme::roles(ctx);
        let size = theme::density(ctx).button_height();
        let (rect, button) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
        let open = egui::Popup::is_id_open(ctx, button.id.with("popup")) || button.is_pointer_button_down_on();
        if open || button.hovered() {
            let opacity = if open { 0.12 } else { 0.08 };
            ui.painter().rect_filled(rect, theme::radius(ctx, theme::Shape::Pill, size), theme::state_layer(Color32::TRANSPARENT, r.on_surface, opacity));
        }
        // One dot above two, evenly round the centre.
        let (c, spread) = (rect.center(), size * 0.17);
        for offset in [Vec2::new(0.0, -spread * 1.05), Vec2::new(-spread * 1.1, spread * 0.8), Vec2::new(spread * 1.1, spread * 0.8)] {
            ui.painter().circle_filled(c + offset, size * 0.065, r.on_surface);
        }
        let button = button.on_hover_text("Project and view settings");
        egui::Popup::menu(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| self.settings_menu(ui, ctx));
    }

    fn settings_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let r = theme::roles(ui.ctx());
        ui.set_min_width(300.0);
        ui.label(theme::heading(ui, "Project"));
        if theme::button(ui, ButtonKind::Tonal, Some(ICON_IMAGE), "Open floor plan").clicked() {
            self.pick_plan(ctx);
            ui.close();
        }
        if theme::button(ui, ButtonKind::Tonal, Some(ICON_FOLDER_OPEN), "Open project").clicked() {
            self.pick_project(ctx);
            ui.close();
        }
        ui.add_enabled_ui(self.project.is_some(), |ui| {
            // One pill: Save on the left, Save as on the right.
            let (save, save_as) = theme::split_button(ui, ButtonKind::Tonal, ICON_SAVE, "Save", "Save as...");
            if save.clicked() {
                match self.path.clone() {
                    Some(p) => self.save_to(&p),
                    None => self.save_as(),
                }
                ui.close();
            }
            if save_as.clicked() {
                self.save_as();
                ui.close();
            }
            if self.mode == WorkMode::Survey {
                let has_points = self.project.as_ref().is_some_and(|p| !p.floor().points.is_empty());
                if ui.add_enabled_ui(has_points, |ui| theme::button(ui, ButtonKind::Filled, Some(ICON_DESCRIPTION), "Generate report")).inner.clicked() {
                    self.make_report();
                    ui.close();
                }
            }
        });
        if self.project.is_none() {
            return;
        }
        ui.separator();
        ui.label(theme::heading(ui, "View"));
        // Fields that can be edited get an outline, so they read as fields and not as labels.
        ui.scope(|ui| {
            let w = &mut ui.visuals_mut().widgets;
            let outline = Stroke::new(1.0, r.outline);
            w.inactive.bg_stroke = outline;
            w.inactive.weak_bg_fill = r.surface_container_highest;
            w.inactive.bg_fill = r.surface_container_highest;
            w.hovered.bg_stroke = Stroke::new(1.5, r.primary);
            w.active.bg_stroke = Stroke::new(1.5, r.primary);
            if self.mode == WorkMode::Survey {
                self.heat_controls(ui);
            } else {
                self.plan_controls(ui);
            }
        });
    }

    /// The measured heatmap's controls.
    fn heat_controls(&mut self, ui: &mut egui::Ui) {
        let Some(floor) = self.project.as_ref().map(|p| p.floor()) else { return };
        let mut changed = theme::switch_row(ui, &mut self.show_heat, "Heatmap").changed();
        ui.add_enabled_ui(self.show_heat, |ui| {
            let label = match &self.target {
                Target::Any => "Strongest of any AP".to_string(),
                Target::Ssid(s) => format!("SSID: {s}"),
                Target::Bssid(b) => format!("AP: {b}"),
            };
            theme::dropdown(egui::ComboBox::from_id_salt("heat_target").selected_text(label).width(260.0).show_ui(ui, |ui| {
                changed |= ui.selectable_value(&mut self.target, Target::Any, "Strongest of any AP").changed();
                for s in floor.ssids() {
                    changed |= ui.selectable_value(&mut self.target, Target::Ssid(s.clone()), format!("SSID: {s}")).changed();
                }
                for b in floor.bssids() {
                    changed |= ui.selectable_value(&mut self.target, Target::Bssid(b.clone()), format!("AP: {b}")).changed();
                }
            }), ui);
            theme::dropdown(egui::ComboBox::from_id_salt("heat_band")
                .selected_text(self.band.map_or("All bands".to_string(), |b| b.to_string()))
                .width(260.0)
                .show_ui(ui, |ui| {
                    changed |= ui.selectable_value(&mut self.band, None, "All bands").changed();
                    for b in [Band::Ghz2_4, Band::Ghz5, Band::Ghz6] {
                        changed |= ui.selectable_value(&mut self.band, Some(b), b.to_string()).changed();
                    }
                }), ui);
            ui.spacing_mut().slider_width = 160.0;
            theme::slider_f32(ui, &mut self.opacity, 0.1..=1.0, 2, "Opacity");
        });
        if changed {
            self.heat_dirty = true;
        }
    }

    /// The predicted coverage's controls.
    fn plan_controls(&mut self, ui: &mut egui::Ui) {
        let mut changed = theme::switch_row(ui, &mut self.show_pred, "Predicted coverage").changed();
        ui.add_enabled_ui(self.show_pred, |ui| {
            ui.horizontal(|ui| {
                for b in [Band::Ghz2_4, Band::Ghz5, Band::Ghz6] {
                    if theme::chip(ui, self.pred_band == b, None, &b.to_string()).clicked() {
                        self.pred_band = b;
                        changed = true;
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("Receiver height");
                changed |= theme::field(ui, egui::DragValue::new(&mut self.rx_height).range(0.0..=10.0).speed(0.05).suffix(" m").fixed_decimals(2)).changed();
            });
            ui.spacing_mut().slider_width = 160.0;
            theme::slider_f32(ui, &mut self.opacity, 0.1..=1.0, 2, "Opacity");
        });
        if changed {
            self.pred_dirty = true;
        }
    }

    /// The tool chips, drawn at the start of the second toolbar row. Returns the
    /// tool the user picked, if any.
    fn tool_chips(ui: &mut egui::Ui, current: Tool, survey: bool, enabled: bool) -> Option<Tool> {
        let chips = [
            (Tool::Survey, ICON_PLACE, if survey { "Survey" } else { "Survey data" }),
            (Tool::Walls, ICON_ARCHITECTURE, "Walls"),
            (Tool::Obstacles, ICON_CATEGORY, "Obstacles"),
            (Tool::Aps, ICON_ROUTER, if survey { "Plot AP" } else { "Devices" }),
            (Tool::Client, ICON_SMARTPHONE, "Client"),
            (Tool::Scale, ICON_STRAIGHTEN, "Scale"),
        ];
        let mut picked = None;
        for (tool, icon, label) in chips {
            if enabled && theme::chip(ui, current == tool, Some(icon), label).clicked() {
                picked = Some(tool);
            }
        }
        picked
    }

    /// Recompute the predicted coverage picture for the active floor.
    fn rebuild_prediction(&mut self, ctx: &egui::Context) {
        self.pred_dirty = false;
        let Some(floor) = self.project.as_ref().map(|p| p.floor()) else { return };
        match predict::predict(floor, self.pred_band, self.rx_height) {
            Ok(grid) => {
                let v = &self.view;
                let pixels = (0..grid.w * grid.h)
                    .map(|i| {
                        let [r, g, b] = heatmap::color_in(v.color_scheme, grid.dbm[i] as f64, v.min_dbm, v.max_dbm);
                        Color32::from_rgb(r, g, b)
                    })
                    .collect();
                let image = egui::ColorImage { size: [grid.w, grid.h], source_size: Vec2::new(grid.w as f32, grid.h as f32), pixels };
                self.pred_texture = Some(ctx.load_texture("prediction", image, TextureOptions::LINEAR));
                self.pred_note = None;
            }
            Err(why) => {
                self.pred_texture = None;
                self.pred_note = Some(why.to_string());
            }
        }
    }

    /// Load the model catalogue the first time it is needed.
    fn ensure_models(&mut self) {
        if !self.models.is_empty() {
            return;
        }
        self.user_models = std::fs::read_to_string(models_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        self.models = catalog::merged(self.user_models.clone());
    }

    fn apply_ap_edit(&mut self, edit: ApEdit) {
        let selected: Vec<usize> = self.aps_selected.iter().copied().collect();
        match edit {
            ApEdit::Search(s) => {
                self.model_search = s;
                return;
            }
            ApEdit::Template(m) => {
                self.ensure_models();
                match m {
                    Some(m) => {
                        self.ap_template.model = Some(m.id());
                        self.ap_template.radios = m.radios.clone();
                    }
                    None => {
                        self.ap_template.model = None;
                        self.ap_template.radios.clear();
                    }
                }
                return;
            }
            ApEdit::PickModel => {
                self.ensure_models();
                self.model_picker = Some(String::new());
                return;
            }
            _ => {}
        }
        let planning = self.mode == WorkMode::Plan;
        let Some(p) = self.project.as_mut() else { return };
        let fl = p.floor_mut();
        let selected: Vec<usize> = selected.into_iter().filter(|&i| i < fl.layer(planning).len()).collect();
        match edit {
            ApEdit::Delete => {
                let mut i = 0;
                fl.layer_mut(planning).retain(|_| {
                    i += 1;
                    !selected.contains(&(i - 1))
                });
                self.aps_selected.clear();
            }
            ApEdit::Duplicate => {
                let copies: Vec<PlannedAp> = {
                    let layer = fl.layer(planning);
                    selected.iter().map(|&i| PlannedAp { x: layer[i].x + 18.0, y: layer[i].y + 18.0, name: format!("{} copy", layer[i].name), from: None, ..layer[i].clone() }).collect()
                };
                self.aps_selected.clear();
                for mut c in copies {
                    c.id = fl.next_ap_id();
                    fl.layer_mut(planning).push(c);
                    self.aps_selected.insert(fl.layer(planning).len() - 1);
                }
            }
            ApEdit::SaveModel => {
                if let [i] = selected[..] {
                    let ap = &fl.layer(planning)[i];
                    let radios = if ap.radios.is_empty() {
                        vec![Radio { band: RadioBand::Ghz2_4, tx_dbm: ap.tx_dbm, gain_dbi: ap.gain_dbi, eirp_cap_dbm: None }, Radio { band: RadioBand::Ghz5, tx_dbm: ap.tx_dbm, gain_dbi: ap.gain_dbi, eirp_cap_dbm: None }]
                    } else {
                        ap.radios.clone()
                    };
                    let model = ApModel { vendor: "Custom".into(), name: ap.name.clone(), note: "Saved from a plan".into(), radios, verified: false };
                    self.user_models.retain(|m| m.id() != model.id());
                    self.user_models.push(model);
                    if let Ok(text) = serde_json::to_string_pretty(&self.user_models) {
                        let path = models_path();
                        if let Some(dir) = path.parent() {
                            let _ = std::fs::create_dir_all(dir);
                        }
                        let _ = std::fs::write(path, text);
                    }
                    self.models = catalog::merged(self.user_models.clone());
                    self.status = format!("Saved \"{}\" to your models.", fl.layer(planning)[i].name);
                }
                return;
            }
            edit => {
                for &i in &selected {
                    let ap = &mut fl.layer_mut(planning)[i];
                    match &edit {
                        ApEdit::Name(n) if selected.len() == 1 => ap.name = n.clone(),
                        ApEdit::Name(_) => {}
                        ApEdit::Height(h) => ap.height_m = *h,
                        ApEdit::Tx(t) => ap.tx_dbm = *t,
                        ApEdit::Gain(g) => ap.gain_dbi = *g,
                        ApEdit::Enabled(e) => ap.enabled = *e,
                        ApEdit::Existing(e) => ap.existing = *e,
                        ApEdit::Model(m) => {
                            ap.model = Some(m.id());
                            ap.radios = m.radios.clone();
                        }
                        ApEdit::Custom => {
                            ap.model = None;
                            ap.radios.clear();
                        }
                        ApEdit::Link(l) if selected.len() == 1 => ap.link = l.clone(),
                        _ => {}
                    }
                }
                if let Some(&i) = selected.last().filter(|_| planning) {
                    let t = &fl.layer(planning)[i];
                    self.ap_template = PlannedAp { name: String::new(), x: 0.0, y: 0.0, existing: false, link: None, ..t.clone() };
                }
            }
        }
        self.autosave();
    }

    /// Access points heard during the survey, strongest first, as (key, description).
    fn measured_aps(floor: &Floor) -> Vec<(String, String)> {
        let mut seen: std::collections::BTreeMap<String, (f64, std::collections::BTreeSet<String>, u32)> = Default::default();
        for p in &floor.points {
            for o in &p.observations {
                let e = seen.entry(ap_base_key(&o.bssid)).or_insert((f64::MIN, Default::default(), o.channel));
                if o.signal_dbm > e.0 {
                    e.0 = o.signal_dbm;
                    e.2 = o.channel;
                }
                if let Some(s) = o.ssid.as_ref().filter(|s| !s.is_empty()) {
                    e.1.insert(s.clone());
                }
            }
        }
        let mut v: Vec<_> = seen.into_iter().collect();
        v.sort_by(|a, b| b.1.0.total_cmp(&a.1.0));
        v.into_iter()
            .take(40)
            .map(|(key, (best, ssids, ch))| {
                let names = if ssids.is_empty() { "hidden".to_string() } else { ssids.into_iter().take(2).collect::<Vec<_>>().join(", ") };
                (key.clone(), format!("{names}  ch {ch}  {best:.0} dBm  {}", &key[..key.len().min(11)]))
            })
            .collect()
    }

    /// The searchable list of access point models.
    fn model_dialog(&mut self, ctx: &egui::Context) {
        let Some(query) = self.model_picker.as_mut() else { return };
        let mut chosen: Option<ApModel> = None;
        let (mut custom, mut close) = (false, false);
        let models = &self.models;
        theme::dialog(ctx, "Choose a model", 480.0, |ui| {
            let edit = theme::field(ui, egui::TextEdit::singleline(query).hint_text("Search, for example \"u7 pro\" or \"6e\"").desired_width(f32::INFINITY));
            edit.request_focus();
            let found = catalog::search(models, query);
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                if found.is_empty() {
                    ui.label("No model matches. Choose custom power, or save your own from an access point.");
                }
                for m in found {
                    let trust = if m.verified { "checked against the maker's specs" } else { "typical values" };
                    let text = format!("{}\n{}  |  {}", m.id(), m.note, trust);
                    if ui.selectable_label(false, text).clicked() {
                        chosen = Some(m.clone());
                    }
                    ui.add_space(2.0);
                }
            });
            theme::actions(ui, |ui| {
                close = theme::button(ui, ButtonKind::Text, None, "Close").clicked();
                custom = theme::button(ui, ButtonKind::Tonal, None, "Custom power").clicked();
            });
        });
        if let Some(m) = chosen {
            self.model_picker = None;
            self.apply_ap_edit(ApEdit::Model(m));
        } else if custom {
            self.model_picker = None;
            self.apply_ap_edit(ApEdit::Custom);
        } else if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.model_picker = None;
        }
    }

    /// Remember the walls so that undo can bring them back. Edits that arrive
    /// as a stream (dragging a height) share one step.
    fn remember_walls(&mut self, streaming: bool) {
        let Some(p) = self.project.as_ref() else { return };
        let now = std::time::Instant::now();
        let recent = self.last_wall_edit.is_some_and(|t| now.duration_since(t) < std::time::Duration::from_millis(600));
        self.last_wall_edit = Some(now);
        if streaming && recent {
            return;
        }
        self.wall_history.push((p.floor().walls.clone(), p.floor().areas.clone()));
        if self.wall_history.len() > WALL_HISTORY {
            self.wall_history.remove(0);
        }
    }

    fn undo_walls(&mut self) {
        if let (Some((walls, areas)), Some(p)) = (self.wall_history.pop(), self.project.as_mut()) {
            p.floor_mut().walls = walls;
            p.floor_mut().areas = areas;
            self.selected_walls.clear();
            self.selected_areas.clear();
            self.area_start = None;
            self.poly_points.clear();
            self.wall_start = None;
            self.autosave();
        }
    }

    fn delete_selected_walls(&mut self) {
        if self.selected_walls.is_empty() && self.selected_areas.is_empty() {
            return;
        }
        self.remember_walls(false);
        let selected = std::mem::take(&mut self.selected_walls);
        let areas = std::mem::take(&mut self.selected_areas);
        if let Some(p) = self.project.as_mut() {
            let mut i = 0;
            p.floor_mut().walls.retain(|_| {
                i += 1;
                !selected.contains(&(i - 1))
            });
            let mut j = 0;
            p.floor_mut().areas.retain(|_| {
                j += 1;
                !areas.contains(&(j - 1))
            });
        }
        self.autosave();
    }

    /// The walls the bubble's controls act on: the selection, or the next wall to be drawn.
    fn apply_wall_edit(&mut self, edit: WallEdit) {
        match edit {
            WallEdit::Mode(m) => {
                self.wall_mode = m;
                self.wall_start = None;
            }
            WallEdit::AreaMode(m) => {
                self.area_mode = m;
                self.area_start = None;
                self.poly_points.clear();
            }
            WallEdit::AreaKind(k) => {
                self.obstacle_kind = k;
                self.obstacle_height = k.default_height_m();
                if self.tool == Tool::Select && !self.selected_areas.is_empty() {
                    self.remember_walls(false);
                    let selected = self.selected_areas.clone();
                    if let Some(p) = self.project.as_mut() {
                        for i in selected {
                            if let Some(a) = p.floor_mut().areas.get_mut(i) {
                                a.kind = k;
                                a.height_m = k.default_height_m();
                            }
                        }
                    }
                    self.autosave();
                }
            }
            WallEdit::AreaHeight(h) => {
                if self.tool == Tool::Select && !self.selected_areas.is_empty() {
                    self.remember_walls(true);
                    let selected = self.selected_areas.clone();
                    if let Some(p) = self.project.as_mut() {
                        for i in selected {
                            if let Some(a) = p.floor_mut().areas.get_mut(i) {
                                a.height_m = h;
                            }
                        }
                    }
                    self.autosave();
                } else {
                    self.obstacle_height = h;
                }
            }
            WallEdit::ClientKind(i) => {
                self.client_kind = i.min(CLIENTS.len() - 1);
                if i != CUSTOM_CLIENT {
                    self.client_bands = CLIENTS[self.client_kind].1;
                }
            }
            WallEdit::ClientHeight(h) => {
                self.rx_height = h;
                self.pred_dirty = true;
            }
            WallEdit::ClientBand(i, on) => {
                self.client_kind = CUSTOM_CLIENT;
                self.client_bands[i] = on;
            }
            WallEdit::AutoScale => self.start_autoscale(),
            WallEdit::DetectWalls => self.start_wallscan(),
            WallEdit::ZoneErase(e) => self.zone_erase = e,
            WallEdit::ZoneBrush(m) => self.zone_brush_m = m,
            WallEdit::ZoneRun => {
                self.leave_zone();
                self.start_optimise(true);
            }
            WallEdit::ZoneCancel => self.leave_zone(),
            WallEdit::ZoneUndo => {
                if let Some(p) = self.project.as_mut() {
                    p.floor_mut().zone.pop();
                }
                self.zone_dirty = true;
                self.autosave();
            }
            WallEdit::ZoneClear => {
                if let Some(p) = self.project.as_mut() {
                    p.floor_mut().zone.clear();
                }
                self.zone_dirty = true;
                self.autosave();
            }
            WallEdit::Undo => self.undo_walls(),
            WallEdit::Delete => self.delete_selected_walls(),
            WallEdit::Ceiling(c) => {
                self.remember_walls(true);
                if let Some(p) = self.project.as_mut() {
                    p.floor_mut().ceiling_m = c;
                    for w in &mut p.floor_mut().walls {
                        if let Some(h) = &mut w.height_m {
                            *h = h.min(c);
                        }
                    }
                }
                self.autosave();
            }
            WallEdit::Material(m) => {
                self.wall_material = m;
                self.wall_height = m.default_height_m();
                if self.tool == Tool::Select && !self.selected_walls.is_empty() {
                    self.remember_walls(false);
                    let selected = self.selected_walls.clone();
                    if let Some(p) = self.project.as_mut() {
                        for i in selected {
                            if let Some(w) = p.floor_mut().walls.get_mut(i) {
                                w.material = m;
                                w.height_m = m.default_height_m();
                            }
                        }
                    }
                    self.autosave();
                }
            }
            WallEdit::Height(h) => {
                if self.tool == Tool::Select && !self.selected_walls.is_empty() {
                    self.remember_walls(true);
                    let selected = self.selected_walls.clone();
                    if let Some(p) = self.project.as_mut() {
                        for i in selected {
                            if let Some(w) = p.floor_mut().walls.get_mut(i) {
                                w.height_m = h;
                            }
                        }
                    }
                    self.autosave();
                } else {
                    self.wall_height = h;
                }
            }
        }
    }

    /// Switch tool, dropping whatever the old one had half done.
    fn set_tool(&mut self, tool: Tool) {
        if tool == Tool::Select && self.tool != Tool::Select {
            self.select_only = matches!(self.tool, Tool::Walls | Tool::Obstacles | Tool::Aps).then_some(self.tool);
        }
        self.tool = tool;
        if !matches!(tool, Tool::Aps | Tool::Select) {
            self.devices_open = false;
        }
        self.wall_start = None;
        self.area_start = None;
        self.poly_points.clear();
        self.ap_drag = None;
        self.box_start = None;
        self.zone_stroke.clear();
        if tool != Tool::Zone {
            self.zone_return = None;
        }
        if tool == Tool::Scale {
            self.scale_clicks.clear();
        }
        if matches!(tool, Tool::Walls) {
            self.selected_walls.clear();
        }
    }

    /// A list of choices in two columns: name on the left, loss and a colour dot on
    /// the right. Returns the index picked. Two columns keep long lists short enough
    /// to show whole.
    fn choice_rows(ui: &mut egui::Ui, rows: &[(String, String, Color32, bool)]) -> Option<usize> {
        let r = theme::roles(ui.ctx());
        let row_h = 28.0 * theme::density(ui.ctx()).font_scale();
        let gap = ui.spacing().item_spacing.x;
        let col_w = ((ui.available_width() - gap) / 2.0).floor();
        let font = egui::TextStyle::Body.resolve(ui.style());
        let mut picked = None;
        for (pair_index, pair) in rows.chunks(2).enumerate() {
            ui.horizontal(|ui| {
                for (j, (name, detail, colour, selected)) in pair.iter().enumerate() {
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(col_w, row_h), Sense::click());
                    if *selected {
                        ui.painter().rect_filled(rect, theme::radius(ui.ctx(), theme::Shape::Row, 0.0), r.secondary_container);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(rect, theme::radius(ui.ctx(), theme::Shape::Row, 0.0), theme::state_layer(Color32::TRANSPARENT, r.on_surface, 0.08));
                    }
                    ui.painter().circle_filled(rect.left_center() + Vec2::new(12.0, 0.0), 5.0, *colour);
                    ui.painter().text(rect.left_center() + Vec2::new(24.0, 0.0), egui::Align2::LEFT_CENTER, name, font.clone(), r.on_surface);
                    ui.painter().text(rect.right_center() - Vec2::new(8.0, 0.0), egui::Align2::RIGHT_CENTER, detail, font.clone(), r.on_surface_variant);
                    if resp.clicked() {
                        picked = Some(pair_index * 2 + j);
                    }
                }
            });
        }
        picked
    }

    /// The material list with each one's loss on the chosen band.
    fn material_rows(&self, ui: &mut egui::Ui, current: Option<Material>, edits: &mut Vec<WallEdit>) {
        let rows: Vec<(String, String, Color32, bool)> = Material::ALL
            .iter()
            .map(|&m| {
                let a = m.attenuation();
                let db = match self.pred_band {
                    Band::Ghz2_4 => a.ghz2_4,
                    Band::Ghz5 => a.ghz5,
                    Band::Ghz6 => a.ghz6,
                };
                let [r, g, b] = m.style().0;
                (m.name().to_string(), format!("{db:.0} dB"), Color32::from_rgb(r, g, b), current == Some(m))
            })
            .collect();
        if let Some(i) = Self::choice_rows(ui, &rows) {
            edits.push(WallEdit::Material(Material::ALL[i]));
        }
    }

    /// Height, ceiling and undo, under a wall menu. `shown` is the height the
    /// controls show: that of the selection or of the next wall.
    fn wall_height_controls(&self, ui: &mut egui::Ui, floor: &Floor, shown: Option<Option<f32>>, selecting: bool, edits: &mut Vec<WallEdit>) {
        let r = theme::roles(ui.ctx());
        let ceiling = floor.ceiling_m;
        ui.add_space(4.0);
        let mut full = shown.is_none_or(|h| h.is_none());
        let mixed = shown.is_none();
        let mut metres = shown.flatten().unwrap_or(ceiling);
        // Full height, the height and the ceiling share one row.
        ui.horizontal_wrapped(|ui| {
            if theme::switch_row(ui, &mut full, "Full height").changed() {
                edits.push(WallEdit::Height(if full { None } else { Some(ceiling) }));
            }
            ui.label("Height");
            if theme::field_enabled(ui, !full, egui::DragValue::new(&mut metres).range(0.1..=ceiling.max(0.2)).speed(0.05).suffix(" m").fixed_decimals(2)).changed() {
                edits.push(WallEdit::Height(Some(metres.min(ceiling))));
            }
            if mixed && selecting {
                ui.label(egui::RichText::new("(mixed)").text_style(egui::TextStyle::Body).color(r.on_surface_variant));
            }
            ui.label("Ceiling");
            let mut c = ceiling;
            if theme::field(ui, egui::DragValue::new(&mut c).range(1.0..=30.0).speed(0.05).suffix(" m").fixed_decimals(2)).changed() {
                edits.push(WallEdit::Ceiling(c));
            }
        });
    }

    /// The menu to the right of the plan for the active tool.
    fn right_panel(&self, ctx: &egui::Context, canvas: Rect) -> (Vec<WallEdit>, Vec<ApEdit>) {
        let r = theme::roles(ctx);
        let (mut walls_out, mut aps_out) = (Vec::new(), Vec::new());
        if !matches!(self.tool, Tool::Walls | Tool::Select | Tool::Aps | Tool::Obstacles | Tool::Zone | Tool::Client | Tool::Scale) {
            return (walls_out, aps_out);
        }
        if self.tool == Tool::Aps && (self.mode == WorkMode::Survey || !self.devices_open) {
            return (walls_out, aps_out);
        }
        let Some(floor) = self.project.as_ref().map(|p| p.floor()) else { return (walls_out, aps_out) };
        // The menu is as tall as its content. Where the window is too short to show it below
        // the round buttons it moves up, as far as the top of the plan, to fit. Its height
        // from the last frame decides, as it is only known once it has been drawn.
        let height_id = egui::Id::unique("right_panel_height");
        let last_height = ctx.data(|d| d.get_temp::<f32>(height_id)).unwrap_or(0.0);
        let default_top = canvas.top() + right_top(ctx, self.mode == WorkMode::Plan);
        let top = if last_height > 0.0 { default_top.min(ctx.content_rect().bottom() - 24.0 - last_height).max(canvas.top() + 8.0) } else { default_top };
        let x = canvas.right() - 12.0 - if self.floors_open { 250.0 } else { 0.0 };
        let shown = egui::Area::new(egui::Id::unique("right_panel"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(egui::pos2(x, top))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(r.surface_container_high)
                    .stroke(Stroke::new(1.0, r.outline_variant))
                    .corner_radius(theme::radius(ctx, theme::Shape::Panel, 0.0))
                    .inner_margin(10)
                    .shadow(theme::shadow(r.shadow, 16, 0.18))
                    .show(ui, |ui| match self.tool {
                        Tool::Aps => {
                            // Only the list of models scrolls: seven rows, as far as the window allows.
                            ui.set_width(300.0);
                            let room = (ctx.content_rect().bottom() - canvas.top() - 28.0).max(160.0);
                            self.devices_content(ui, floor, room, &mut aps_out);
                        }
                        tool => {
                            // The lists of materials and kinds are in two columns, so those menus are wider.
                            let small = tool == Tool::Scale;
                            let empty_select = tool == Tool::Select && self.selected_walls.is_empty() && self.aps_selected.is_empty() && self.selected_areas.is_empty();
                            ui.set_width(if small { 270.0 } else if empty_select { 240.0 } else if tool == Tool::Client { 250.0 } else if theme::skin(ctx) == theme::Skin::Carbon { 470.0 } else { 400.0 });
                            match tool {
                                Tool::Walls => self.walls_content(ui, floor, &mut walls_out),
                                Tool::Select => self.select_content(ui, floor, &mut walls_out, &mut aps_out),
                                Tool::Obstacles => self.obstacles_content(ui, floor, &mut walls_out),
                                Tool::Zone => self.zone_content(ui, floor, &mut walls_out),
                                Tool::Client => self.client_content(ui, floor, &mut walls_out),
                                Tool::Scale => self.scale_content(ui, &mut walls_out),
                                _ => {}
                            }
                        }
                    });
            });
        let height = shown.response.rect.height();
        if (height - last_height).abs() > 0.5 {
            ctx.data_mut(|d| d.insert_temp(height_id, height));
            ctx.request_repaint();
        }
        (walls_out, aps_out)
    }

    /// The wall menu: wall or room, material list, heights.
    fn walls_content(&self, ui: &mut egui::Ui, floor: &Floor, edits: &mut Vec<WallEdit>) {
        let how = match self.wall_mode {
            WallMode::Wall => "Click the start, then the end of a wall.",
            WallMode::Room => "Click two opposite corners of a room.",
        };
        ui.horizontal(|ui| {
            ui.label(theme::heading(ui, "Auto-draw Walls")).on_hover_text(format!("{how} Esc or right-click cancels. Hold Shift to turn snapping off."));
            if theme::chip(ui, self.wall_mode == WallMode::Wall, Some(ICON_EDIT), "Wall").clicked() {
                edits.push(WallEdit::Mode(WallMode::Wall));
            }
            if theme::chip(ui, self.wall_mode == WallMode::Room, Some(ICON_CROP_FREE), "Room").clicked() {
                edits.push(WallEdit::Mode(WallMode::Room));
            }
        });
        self.material_rows(ui, Some(self.wall_material), edits);
        self.wall_height_controls(ui, floor, Some(self.wall_height), false, edits);
        let busy = self.wallscan_rx.is_some();
        ui.horizontal_wrapped(|ui| {
            let label = if busy { "Looking for walls..." } else { "Detect walls from the plan" };
            if ui.add_enabled_ui(!busy && floor.metres_per_px.is_some(), |ui| theme::button(ui, ButtonKind::Tonal, Some(ICON_AUTO_FIX_HIGH), label)).inner
                .on_hover_text(if floor.metres_per_px.is_some() {
                    "Traces the dark wall bands on the plan, with doors and windows where they break the walls. Walls come in as concrete, doors as wood and windows as window glass; change them with Select."
                } else {
                    "Set the plan's scale first, so wall thicknesses and door and window sizes are known."
                })
                .clicked()
            {
                edits.push(WallEdit::DetectWalls);
            }
        });
        if let Some(why) = &self.wallscan_error {
            ui.label(egui::RichText::new(why).text_style(egui::TextStyle::Body).color(theme::roles(ui.ctx()).error));
        }
        if ui.add_enabled_ui(!self.wall_history.is_empty(), |ui| theme::button(ui, ButtonKind::Text, Some(ICON_UNDO), "Undo")).inner.clicked() {
            edits.push(WallEdit::Undo);
        }
    }

    /// The Scale menu: click two points of a known distance, or let the plan's own dimensions decide.
    fn scale_content(&self, ui: &mut egui::Ui, edits: &mut Vec<WallEdit>) {
        let r = theme::roles(ui.ctx());
        ui.label(theme::heading(ui, "Scale"));
        ui.label(egui::RichText::new("Click two points a known distance apart.").text_style(egui::TextStyle::Body).color(r.on_surface_variant));
        ui.add_space(6.0);
        let busy = self.autoscale_rx.is_some();
        let label = if busy { "Reading the plan..." } else { "Find from the plan's dimensions" };
        if ui.add_enabled_ui(!busy, |ui| theme::button(ui, ButtonKind::Tonal, Some(ICON_AUTO_FIX_HIGH), label)).inner
            .on_hover_text("Reads the dimension numbers printed on the plan (mm, cm, m or feet and inches) and works out the scale from them.")
            .clicked()
        {
            edits.push(WallEdit::AutoScale);
        }
        if let Some(why) = &self.autoscale_error {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(why).text_style(egui::TextStyle::Body).color(r.error));
        }
    }

    /// Look for walls on the plan picture in the background.
    fn start_wallscan(&mut self) {
        let Some(project) = self.project.as_ref() else { return };
        let Some(mpp) = project.floor().metres_per_px else { return };
        let bytes = project.floor().image.clone();
        self.wallscan_error = None;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = image::load_from_memory(&bytes).map_err(|e| format!("Could not open the plan picture: {e}")).and_then(|img| {
                let rgb = img.into_rgb8();
                let found = crate::wallscan::scan(rgb.as_raw(), rgb.dimensions(), mpp);
                if found.walls.is_empty() { Err("No walls were found. They need to be drawn as solid dark bands.".to_string()) } else { Ok(found) }
            });
            let _ = tx.send(result);
        });
        self.wallscan_rx = Some(rx);
    }

    /// Collect what was found, and ask how to add it.
    fn wallscan_tick(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.wallscan_rx {
            match rx.try_recv() {
                Ok(Ok(found)) => {
                    self.wallscan_found = Some(found);
                    self.wallscan_rx = None;
                }
                Ok(Err(why)) => {
                    self.wallscan_error = Some(why);
                    self.wallscan_rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(150)),
                Err(mpsc::TryRecvError::Disconnected) => self.wallscan_rx = None,
            }
        }
        let Some(found) = self.wallscan_found.clone() else { return };
        let existing = self.project.as_ref().map_or(0, |p| p.floor().walls.len());
        let (mut add, mut replace, mut close) = (false, false, false);
        theme::dialog(ctx, "Walls found", 440.0, |ui| {
            ui.label(format!("{} walls, {} doors and {} windows.", found.walls.len() - found.doors - found.windows, found.doors, found.windows));
            ui.add_space(4.0);
            ui.weak("Walls come in as concrete, doors as wood and windows as window glass. Select them to change the material or remove any that are not walls; Undo takes them all back out.");
            if existing > 0 {
                ui.add_space(4.0);
                ui.weak(format!("The plan already has {existing} walls."));
            }
            theme::actions(ui, |ui| {
                if theme::button(ui, ButtonKind::Filled, None, if existing > 0 { "Add to the plan" } else { "Add walls" }).clicked() {
                    add = true;
                }
                if existing > 0 && theme::button(ui, ButtonKind::Outlined, None, "Replace the walls").clicked() {
                    replace = true;
                }
                if theme::button(ui, ButtonKind::Text, None, "Cancel").clicked() {
                    close = true;
                }
            });
        });
        if add || replace {
            self.remember_walls(false);
            if let Some(p) = self.project.as_mut() {
                if replace {
                    p.floor_mut().walls.clear();
                }
                p.floor_mut().walls.extend(found.walls.iter().copied());
            }
            self.selected_walls.clear();
            self.status = format!("{} walls added from the plan.", found.walls.len());
            self.heat_dirty = true;
            self.pred_dirty = true;
            self.autosave();
        }
        if add || replace || close {
            self.wallscan_found = None;
        }
    }

    /// Read the plan's dimensions in the background.
    fn start_autoscale(&mut self) {
        let Some(project) = self.project.as_ref() else { return };
        let bytes = project.floor().image.clone();
        self.autoscale_error = None;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = image::load_from_memory(&bytes).map_err(|e| format!("Could not open the plan picture: {e}")).and_then(|img| {
                let rgb = img.into_rgb8();
                crate::autoscale::detect(rgb.as_raw(), rgb.dimensions())
            });
            let _ = tx.send(result);
        });
        self.autoscale_rx = Some(rx);
    }

    /// Collect the result, and ask before using it.
    fn autoscale_tick(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.autoscale_rx {
            match rx.try_recv() {
                Ok(Ok(found)) => {
                    self.autoscale_found = Some(found);
                    self.autoscale_rx = None;
                }
                Ok(Err(why)) => {
                    self.autoscale_error = Some(why);
                    self.autoscale_rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(150)),
                Err(mpsc::TryRecvError::Disconnected) => self.autoscale_rx = None,
            }
        }
        let Some(found) = self.autoscale_found.clone() else { return };
        let Some(floor) = self.project.as_ref().map(|p| p.floor()) else { return };
        let (w, h) = (floor.width_px as f32 * found.metres_per_px, floor.height_px as f32 * found.metres_per_px);
        let was = floor.metres_per_px;
        let (mut use_it, mut close) = (false, false);
        theme::dialog(ctx, "Scale found", 420.0, |ui| {
            ui.label(format!("{:.1} px per metre, which makes the plan {w:.1} x {h:.1} m.", 1.0 / found.metres_per_px));
            ui.add_space(4.0);
            if let Some((n, long_mm)) = found.ratio {
                ui.weak(format!("The plan has no dimension lines. It states a scale of 1:{n}, and the page was taken to be {long_mm:.0} mm on its long side (A4 unless the note names a sheet size)."));
            } else {
                ui.weak(format!(
                    "{} dimensions on the plan agree, for example {}.{}",
                    found.agreeing,
                    found.examples.join(", "),
                    found.assumed.map_or(String::new(), |u| format!(" Numbers without a unit were read as {u}."))
                ));
            }
            if let Some(m) = was {
                ui.add_space(4.0);
                ui.weak(format!("This replaces the current scale ({:.1} px per metre).", 1.0 / m));
            }
            theme::actions(ui, |ui| {
                if theme::button(ui, ButtonKind::Filled, None, "Use this scale").clicked() {
                    use_it = true;
                }
                if theme::button(ui, ButtonKind::Text, None, "Cancel").clicked() {
                    close = true;
                }
            });
        });
        if use_it {
            if let Some(p) = self.project.as_mut() {
                p.floor_mut().metres_per_px = Some(found.metres_per_px);
            }
            self.status = "Scale set from the plan's dimensions.".into();
            self.heat_dirty = true;
            self.pred_dirty = true;
            self.autosave();
            // A plan with no walls yet goes straight on to looking for them; the result is still
            // only a proposal to accept or cancel, and the wall tools work as before.
            if self.project.as_ref().is_some_and(|p| p.floor().walls.is_empty()) {
                self.start_wallscan();
            }
        }
        if use_it || close {
            self.autoscale_found = None;
        }
    }

    /// The menu for the current selection of walls and devices.
    fn select_content(&self, ui: &mut egui::Ui, floor: &Floor, walls_out: &mut Vec<WallEdit>, aps_out: &mut Vec<ApEdit>) {
        let r = theme::roles(ui.ctx());
        let walls: Vec<&Wall> = self.selected_walls.iter().filter_map(|&i| floor.walls.get(i)).collect();
        let aps: Vec<&PlannedAp> = self.aps_selected.iter().filter_map(|&i| floor.layer(self.mode == WorkMode::Plan).get(i)).collect();
        let areas: Vec<&Area> = self.selected_areas.iter().filter_map(|&i| floor.areas.get(i)).collect();
        ui.label(theme::heading(ui, "Select"));
        if walls.is_empty() && aps.is_empty() && areas.is_empty() {
            ui.label(egui::RichText::new("Click, or drag a box. Ctrl adds.").text_style(egui::TextStyle::Body).color(r.on_surface_variant)).on_hover_text("Click a wall, area or device, or drag a box over several. Ctrl or Shift adds to the selection, Ctrl+A selects everything.");
            return;
        }
        if !walls.is_empty() {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(format!("{} wall{}", walls.len(), if walls.len() == 1 { "" } else { "s" })).text_style(egui::TextStyle::Body).color(r.on_surface_variant));
            let first = walls[0];
            let material = walls.iter().all(|w| w.material == first.material).then_some(first.material);
            let height = walls.iter().all(|w| w.height_m == first.height_m).then_some(first.height_m);
            self.material_rows(ui, material, walls_out);
            self.wall_height_controls(ui, floor, height, true, walls_out);
        }
        if !areas.is_empty() {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(format!("{} obstacle area{}", areas.len(), if areas.len() == 1 { "" } else { "s" })).text_style(egui::TextStyle::Body).color(r.on_surface_variant));
            let first = areas[0];
            let kind = areas.iter().all(|a| a.kind == first.kind).then_some(first.kind);
            let height = areas.iter().all(|a| a.height_m == first.height_m).then_some(first.height_m);
            self.kind_rows(ui, kind, walls_out);
            self.area_height_controls(ui, floor, height, true, walls_out);
        }
        if !aps.is_empty() {
            ui.add_space(8.0);
            self.device_controls(ui, floor, &aps, aps_out);
        }
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled_ui(!self.wall_history.is_empty(), |ui| theme::button(ui, ButtonKind::Text, Some(ICON_UNDO), "Undo")).inner.clicked() {
                walls_out.push(WallEdit::Undo);
            }
            if theme::button(ui, ButtonKind::Outlined, Some(ICON_DELETE), "Delete").clicked() {
                if !walls.is_empty() || !areas.is_empty() {
                    walls_out.push(WallEdit::Delete);
                }
                if !aps.is_empty() {
                    aps_out.push(ApEdit::Delete);
                }
            }
        });
    }

    /// Obstacle kinds with their loss per metre.
    fn kind_rows(&self, ui: &mut egui::Ui, current: Option<ObstacleKind>, edits: &mut Vec<WallEdit>) {
        let rows: Vec<(String, String, Color32, bool)> = ObstacleKind::ALL
            .iter()
            .map(|&k| {
                let [r, g, b] = k.colour();
                (k.name().to_string(), format!("{:.0} dB/m", k.db_per_m()), Color32::from_rgb(r, g, b), current == Some(k))
            })
            .collect();
        if let Some(i) = Self::choice_rows(ui, &rows) {
            edits.push(WallEdit::AreaKind(ObstacleKind::ALL[i]));
        }
    }

    /// Height controls for obstacle areas.
    fn area_height_controls(&self, ui: &mut egui::Ui, floor: &Floor, shown: Option<Option<f32>>, selecting: bool, edits: &mut Vec<WallEdit>) {
        let r = theme::roles(ui.ctx());
        let ceiling = floor.ceiling_m;
        ui.add_space(4.0);
        let mut full = shown.is_none_or(|h| h.is_none());
        let mixed = shown.is_none();
        let mut metres = shown.flatten().unwrap_or(ceiling);
        ui.horizontal_wrapped(|ui| {
            if theme::switch_row(ui, &mut full, "Full height").changed() {
                edits.push(WallEdit::AreaHeight(if full { None } else { Some(ceiling) }));
            }
            ui.label("Height");
            if theme::field_enabled(ui, !full, egui::DragValue::new(&mut metres).range(0.1..=ceiling.max(0.2)).speed(0.05).suffix(" m").fixed_decimals(2)).changed() {
                edits.push(WallEdit::AreaHeight(Some(metres.min(ceiling))));
            }
            if mixed && selecting {
                ui.label(egui::RichText::new("(mixed)").text_style(egui::TextStyle::Body).color(r.on_surface_variant));
            }
        });
    }

    /// The obstacle menu: rectangle or polygon, kind, height.
    fn obstacles_content(&self, ui: &mut egui::Ui, floor: &Floor, edits: &mut Vec<WallEdit>) {
        let how = match self.area_mode {
            AreaMode::Rectangle => "Click two opposite corners.",
            AreaMode::Polygon => "Click each corner; double-click or Enter closes it.",
        };
        ui.horizontal(|ui| {
            ui.label(theme::heading(ui, "Obstacle Areas")).on_hover_text(format!("{how} Esc or right-click cancels. Signal loses the dB per metre listed for the stretch it passes through, below the area's height."));
            if theme::chip(ui, self.area_mode == AreaMode::Rectangle, Some(ICON_CROP_FREE), "Rectangle").clicked() {
                edits.push(WallEdit::AreaMode(AreaMode::Rectangle));
            }
            if theme::chip(ui, self.area_mode == AreaMode::Polygon, Some(ICON_EDIT), "Polygon").clicked() {
                edits.push(WallEdit::AreaMode(AreaMode::Polygon));
            }
        });
        self.kind_rows(ui, Some(self.obstacle_kind), edits);
        self.area_height_controls(ui, floor, Some(self.obstacle_height), false, edits);
        if ui.add_enabled_ui(!self.wall_history.is_empty(), |ui| theme::button(ui, ButtonKind::Text, Some(ICON_UNDO), "Undo")).inner.clicked() {
            edits.push(WallEdit::Undo);
        }
    }

    /// The coverage menu: brush, paint or erase, and what the paint is for.
    fn zone_content(&self, ui: &mut egui::Ui, floor: &Floor, edits: &mut Vec<WallEdit>) {
        let r = theme::roles(ui.ctx());
        ui.horizontal(|ui| {
            ui.label(theme::heading(ui, "Optimise: Paint the Area")).on_hover_text("The optimiser places access points and counts coverage only in the painted area, and uses only the walls it reaches.");
            if theme::chip(ui, !self.zone_erase, Some(ICON_BRUSH), "Paint").clicked() {
                edits.push(WallEdit::ZoneErase(false));
            }
            if theme::chip(ui, self.zone_erase, Some(ICON_DELETE), "Erase").clicked() {
                edits.push(WallEdit::ZoneErase(true));
            }
        });
        let how = if floor.zone.is_empty() {
            "Paint the area the access points should cover. Let the brush run a little over the walls. Join the end of a stroke to its start and the inside fills. Then press Optimise."
        } else {
            "The saved area is shown. Paint or erase to change it, then press Optimise. Join the end of a stroke to its start to fill the inside."
        };
        ui.label(egui::RichText::new(how).color(r.on_surface_variant));
        let mut brush = self.zone_brush_m;
        ui.spacing_mut().slider_width = 160.0;
        if theme::slider_f32(ui, &mut brush, 0.3..=4.0, 1, "Brush (m)") {
            edits.push(WallEdit::ZoneBrush(brush));
        }
        ui.horizontal(|ui| {
            if ui.add_enabled_ui(!floor.zone.is_empty(), |ui| theme::button(ui, ButtonKind::Text, Some(ICON_UNDO), "Undo")).inner.clicked() {
                edits.push(WallEdit::ZoneUndo);
            }
            if ui.add_enabled_ui(!floor.zone.is_empty(), |ui| theme::button(ui, ButtonKind::Text, Some(ICON_DELETE), "Clear all")).inner.clicked() {
                edits.push(WallEdit::ZoneClear);
            }
        });
        ui.horizontal(|ui| {
            if ui.add_enabled_ui(!floor.zone.is_empty(), |ui| theme::button(ui, ButtonKind::Filled, Some(egui_material_icons::icons::ICON_AUTO_FIX_HIGH), "Optimise")).inner.clicked() {
                edits.push(WallEdit::ZoneRun);
            }
            if theme::button(ui, ButtonKind::Text, Some(egui_material_icons::icons::ICON_CLOSE), "Cancel").clicked() {
                edits.push(WallEdit::ZoneCancel);
            }
        });
    }

    /// Back to the tool in use before the area was painted.
    fn leave_zone(&mut self) {
        let back = self.zone_return.take().unwrap_or(Tool::Aps);
        self.set_tool(back);
    }

    /// The brush radius in plan pixels.
    fn zone_radius_px(&self) -> f32 {
        let mpp = self.project.as_ref().and_then(|p| p.floor().metres_per_px).filter(|m| *m > 0.0);
        self.zone_brush_m / mpp.unwrap_or(0.05) / 2.0
    }

    /// Paint or rub out with the primary button; a click makes a dot.
    fn zone_input(&mut self, resp: &egui::Response, to_plan: &impl Fn(Pos2) -> Pos2, size: (f32, f32)) {
        let radius = self.zone_radius_px();
        let point = |pos: Pos2| {
            let p = to_plan(pos);
            [p.x.clamp(0.0, size.0), p.y.clamp(0.0, size.1)]
        };
        if resp.drag_started_by(egui::PointerButton::Primary) {
            self.zone_stroke.clear();
        }
        if resp.dragged_by(egui::PointerButton::Primary)
            && let Some(p) = resp.interact_pointer_pos().map(point)
            && self.zone_stroke.last().is_none_or(|l| ((l[0] - p[0]).powi(2) + (l[1] - p[1]).powi(2)).sqrt() >= radius * 0.25)
        {
            self.zone_stroke.push(p);
        }
        let finished = resp.drag_stopped_by(egui::PointerButton::Primary);
        if resp.clicked_by(egui::PointerButton::Primary)
            && let Some(p) = resp.interact_pointer_pos().map(point)
        {
            self.zone_stroke = vec![p];
        }
        if (finished || resp.clicked_by(egui::PointerButton::Primary)) && !self.zone_stroke.is_empty() {
            let points = std::mem::take(&mut self.zone_stroke);
            let closed = wifispanner_core::coverage::Stroke::closes(&points, radius);
            if let Some(p) = self.project.as_mut() {
                p.floor_mut().zone.push(wifispanner_core::coverage::Stroke { points, radius, erase: self.zone_erase, closed });
            }
            self.zone_dirty = true;
            self.autosave();
        }
    }

    /// The picture of the painted area, drawn over the plan.
    fn rebuild_zone_texture(&mut self, ctx: &egui::Context) {
        self.zone_dirty = false;
        let Some(floor) = self.project.as_ref().map(|p| p.floor()) else { return };
        if floor.zone.is_empty() {
            self.zone_tex = None;
            return;
        }
        let mask = wifispanner_core::coverage::Mask::from_strokes(&floor.zone, floor.width_px as f32, floor.height_px as f32);
        let paint = Color32::from_rgba_unmultiplied(0, 150, 140, 90);
        let pixels = mask.cells.iter().map(|on| if *on { paint } else { Color32::TRANSPARENT }).collect();
        let image = egui::ColorImage { size: [mask.w, mask.h], source_size: Vec2::new(mask.w as f32, mask.h as f32), pixels };
        self.zone_tex = Some(ctx.load_texture("coverage_zone", image, TextureOptions::NEAREST));
    }

    /// How the simulated client at `pos` would connect: for each band it can use,
    /// the best access point and signal, and the best of them overall.
    fn client_results(&self, floor: &Floor, pos: Pos2) -> Vec<(usize, Band, Option<(usize, f32)>)> {
        [Band::Ghz2_4, Band::Ghz5, Band::Ghz6]
            .into_iter()
            .enumerate()
            .filter(|(i, _)| self.client_bands[*i])
            .map(|(i, b)| (i, b, predict::best_at(floor, floor.layer(self.mode == WorkMode::Plan), b, [pos.x, pos.y], self.rx_height)))
            .collect()
    }

    /// The client menu: device type, receiver height, and what it would get where it stands.
    fn client_content(&self, ui: &mut egui::Ui, floor: &Floor, edits: &mut Vec<WallEdit>) {
        let r = theme::roles(ui.ctx());
        ui.label(theme::heading(ui, "Client Experience"));
        // What the client gets comes first, so it is in view however short the menu is.
        match self.client {
            None => {
                ui.label(egui::RichText::new("Click or drag on the plan to place a client.").text_style(egui::TextStyle::Body).color(r.on_surface_variant))
                    .on_hover_text("Pan with the right mouse button while the client tool is active.");
            }
            Some(_) if floor.metres_per_px.is_none() => {
                ui.label("Set the plan's scale first, so distances are real.");
            }
            Some(pos) => {
                let results = self.client_results(floor, pos);
                let layer = floor.layer(self.mode == WorkMode::Plan);
                let best = results.iter().filter_map(|(_, b, hit)| hit.map(|(ap, dbm)| (ap, *b, dbm))).max_by(|a, b| a.2.total_cmp(&b.2));
                match best {
                    Some((ap, band, dbm)) if dbm >= NO_COVERAGE_DBM => {
                        let name = layer.get(ap).map_or("access point", |a| a.name.as_str());
                        ui.label(egui::RichText::new(format!("Connected to {name}")).strong());
                        ui.label(egui::RichText::new(format!("{band}, {dbm:.0} dBm, {}", rating(dbm))).color(rating_colour(dbm)));
                    }
                    _ => {
                        ui.label(egui::RichText::new("No WiFi coverage").strong().color(r.error));
                    }
                }
                for (_, band, hit) in results {
                    match hit {
                        Some((ap, dbm)) => {
                            let name = layer.get(ap).map_or("", |a| a.name.as_str());
                            ui.label(egui::RichText::new(format!("{band}:  {dbm:.0} dBm  ({name})")).text_style(egui::TextStyle::Body).color(rating_colour(dbm)));
                        }
                        None => {
                            ui.label(egui::RichText::new(format!("{band}:  no access point")).text_style(egui::TextStyle::Body).color(r.on_surface_variant));
                        }
                    }
                }
            }
        }
        ui.separator();
        theme::dropdown(egui::ComboBox::from_id_salt("client_kind").selected_text(CLIENTS[self.client_kind].0).width(220.0).show_ui(ui, |ui| {
            for (i, (name, _)) in CLIENTS.iter().enumerate() {
                if ui.selectable_label(self.client_kind == i, *name).clicked() {
                    edits.push(WallEdit::ClientKind(i));
                }
            }
        }), ui);
        if self.client_kind == CUSTOM_CLIENT {
            ui.horizontal(|ui| {
                for (i, label) in ["2.4", "5", "6"].into_iter().enumerate() {
                    let mut on = self.client_bands[i];
                    if ui.checkbox(&mut on, label).changed() {
                        edits.push(WallEdit::ClientBand(i, on));
                    }
                }
                ui.label("GHz");
            });
        }
        ui.horizontal(|ui| {
            ui.label("Client height");
            let mut h = self.rx_height;
            if theme::field(ui, egui::DragValue::new(&mut h).range(0.0..=10.0).speed(0.05).suffix(" m").fixed_decimals(2)).changed() {
                edits.push(WallEdit::ClientHeight(h));
            }
        });
    }

    /// The "Add Devices" menu: a searchable list of models to place.
    fn devices_content(&self, ui: &mut egui::Ui, floor: &Floor, _room: f32, edits: &mut Vec<ApEdit>) {
        let r = theme::roles(ui.ctx());
        ui.label(theme::heading(ui, "Add Devices"));
        let layer = floor.layer(self.mode == WorkMode::Plan);
        let (installed, planned) = (layer.iter().filter(|a| a.existing).count(), layer.iter().filter(|a| !a.existing).count());
        let counts = if self.mode == WorkMode::Plan { format!("{installed} from the survey, {planned} planned on this floor") } else { format!("{installed} installed access points plotted on this floor") };
        ui.label(egui::RichText::new(counts).text_style(egui::TextStyle::Body).color(r.on_surface_variant));
        ui.label(egui::RichText::new("Pick a model, then click the plan.").text_style(egui::TextStyle::Body).color(r.on_surface_variant)).on_hover_text("Drag a placed device to move it.");
        let mut search = self.model_search.clone();
        if theme::field(ui, egui::TextEdit::singleline(&mut search).hint_text("Search").desired_width(f32::INFINITY)).changed() {
            edits.push(ApEdit::Search(search));
        }
        ui.add_space(4.0);
        let current = self.ap_template.model.clone();
        let custom = current.is_none();
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0 * theme::density(ui.ctx()).font_scale()), Sense::click());
        if custom {
            ui.painter().rect_filled(rect, theme::radius(ui.ctx(), theme::Shape::Row, 0.0), r.secondary_container);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, theme::radius(ui.ctx(), theme::Shape::Row, 0.0), theme::state_layer(Color32::TRANSPARENT, r.on_surface, 0.08));
        }
        ui.painter().text(rect.left_center() + Vec2::new(10.0, 0.0), egui::Align2::LEFT_CENTER, "Custom (set power by hand)", egui::TextStyle::Body.resolve(ui.style()), r.on_surface);
        if resp.clicked() {
            edits.push(ApEdit::Template(None));
        }
        // Below the title, counts, hint, search and the custom row.
        let row_h = 28.0 * theme::density(ui.ctx()).font_scale() + ui.spacing().item_spacing.y;
        // Always seven models in view before it scrolls, whatever the window size: the menu
        // moves up to make room rather than the list shrinking.
        let list_height = 7.0 * row_h + 2.0;
        egui::ScrollArea::vertical().min_scrolled_height(list_height).max_height(list_height).auto_shrink([false, true]).show(ui, |ui| {
            for m in catalog::search(&self.models, &self.model_search) {
                let on = current.as_deref() == Some(m.id().as_str());
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0 * theme::density(ui.ctx()).font_scale()), Sense::click());
                if on {
                    ui.painter().rect_filled(rect, theme::radius(ui.ctx(), theme::Shape::Row, 0.0), r.secondary_container);
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, theme::radius(ui.ctx(), theme::Shape::Row, 0.0), theme::state_layer(Color32::TRANSPARENT, r.on_surface, 0.08));
                }
                let body = egui::TextStyle::Body.resolve(ui.style());
                // The bands on the right; the maker's name and how far to trust the figures on hover.
                let bands: Vec<&str> = m.radios.iter().map(|rd| match rd.band {
                    RadioBand::Ghz2_4 => "2.4",
                    RadioBand::Ghz5 => "5",
                    RadioBand::Ghz6 => "6",
                }).collect();
                let tags = ui.painter().text(rect.right_center() - Vec2::new(10.0, 0.0), egui::Align2::RIGHT_CENTER, bands.join(" "), body.clone(), r.on_surface_variant);
                let name_room = (tags.left() - rect.left() - 20.0).max(40.0);
                let galley = ui.painter().layout(m.name.clone(), body, r.on_surface, name_room);
                ui.painter().galley(rect.left_center() + Vec2::new(10.0, -galley.size().y / 2.0), galley, r.on_surface);
                let resp = resp.on_hover_text(format!("{}  |  {}  |  {}", m.id(), m.note, if m.verified { "figures from the maker's specs" } else { "typical figures" }));
                if resp.clicked() {
                    edits.push(ApEdit::Template(Some(m.clone())));
                }
            }
        });
    }

    /// Controls for the selected devices.
    fn device_controls(&self, ui: &mut egui::Ui, floor: &Floor, picked: &[&PlannedAp], edits: &mut Vec<ApEdit>) {
        let r = theme::roles(ui.ctx());
        let first = picked[0];
        ui.label(egui::RichText::new(format!("{} device{}", picked.len(), if picked.len() == 1 { "" } else { "s" })).text_style(egui::TextStyle::Body).color(r.on_surface_variant));
        if picked.len() == 1 {
            let mut name = first.name.clone();
            if theme::field(ui, egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY)).changed() {
                edits.push(ApEdit::Name(name));
            }
        }
        if planning_channels(self.mode) && picked.len() == 1 && !first.channels.is_empty() {
            let text = first.channels.iter().map(|c| format!("{} {}", c.band.band(), c.number)).collect::<Vec<_>>().join("   ");
            ui.label(egui::RichText::new(format!("Channels: {text}")).text_style(egui::TextStyle::Body).color(r.on_surface_variant))
                .on_hover_text("From the optimiser's channel plan. Move or add access points and optimise again to refresh it.");
        }
        // While surveying an access point is only plotted: where it is, what it is called, and
        // which measured one it is. Its model, power and height are for Planning.
        let planning = self.mode == WorkMode::Plan;
        if planning {
            self.device_settings(ui, first, edits);
        }
        if picked.len() == 1 && first.existing {
            let options = Self::measured_aps(floor);
            if !options.is_empty() {
                ui.label(egui::RichText::new("Measured as").text_style(egui::TextStyle::Body).color(r.on_surface_variant));
                let current = first.link.as_ref().and_then(|k| options.iter().find(|o| &o.0 == k)).map_or("Not linked".to_string(), |o| o.1.clone());
                theme::dropdown(egui::ComboBox::from_id_salt("ap_link").selected_text(current).width(230.0).show_ui(ui, |ui| {
                    if ui.selectable_label(first.link.is_none(), "Not linked").clicked() {
                        edits.push(ApEdit::Link(None));
                    }
                    for (key, text) in &options {
                        if ui.selectable_label(first.link.as_ref() == Some(key), text).clicked() {
                            edits.push(ApEdit::Link(Some(key.clone())));
                        }
                    }
                }), ui);
            }
        }
        ui.horizontal_wrapped(|ui| {
            if theme::button(ui, ButtonKind::Text, None, "Duplicate").clicked() {
                edits.push(ApEdit::Duplicate);
            }
            if planning && picked.len() == 1 && theme::button(ui, ButtonKind::Text, None, "Save as model").clicked() {
                edits.push(ApEdit::SaveModel);
            }
        });
    }

    /// Enabled, installed, model, power and height of a device, for Planning.
    fn device_settings(&self, ui: &mut egui::Ui, first: &PlannedAp, edits: &mut Vec<ApEdit>) {
        let r = theme::roles(ui.ctx());
        let mut on = first.enabled;
        if theme::switch_row(ui, &mut on, "Enabled").changed() {
            edits.push(ApEdit::Enabled(on));
        }
        let mut inst = first.existing;
        if theme::switch_row(ui, &mut inst, "Installed (as built)").changed() {
            edits.push(ApEdit::Existing(inst));
        }
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Model").text_style(egui::TextStyle::Body).color(r.on_surface_variant));
        let label = first.model.clone().unwrap_or_else(|| "Custom (set power by hand)".into());
        if theme::button(ui, ButtonKind::Tonal, None, &label).clicked() {
            edits.push(ApEdit::PickModel);
        }
        if first.radios.is_empty() {
            let (mut t, mut g) = (first.tx_dbm, first.gain_dbi);
            egui::Grid::new("ap_power").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Power, all bands");
                if theme::field(ui, egui::DragValue::new(&mut t).range(0.0..=36.0).speed(0.1).suffix(" dBm").fixed_decimals(1)).changed() {
                    edits.push(ApEdit::Tx(t));
                }
                ui.end_row();
                ui.label("Antenna gain");
                if theme::field(ui, egui::DragValue::new(&mut g).range(0.0..=20.0).speed(0.1).suffix(" dBi").fixed_decimals(1)).changed() {
                    edits.push(ApEdit::Gain(g));
                }
                ui.end_row();
            });
        } else {
            for rd in &first.radios {
                let band = match rd.band {
                    RadioBand::Ghz2_4 => "2.4 GHz",
                    RadioBand::Ghz5 => "5 GHz",
                    RadioBand::Ghz6 => "6 GHz",
                };
                ui.label(egui::RichText::new(format!("{band}:  {:.0} dBm + {:.1} dBi{}", rd.tx_dbm, rd.gain_dbi, rd.eirp_cap_dbm.map_or(String::new(), |c| format!(" (max {c:.0} dBm EIRP)")))).text_style(egui::TextStyle::Body));
            }
            if ui.small_button("Use custom power instead").clicked() {
                edits.push(ApEdit::Custom);
            }
        }
        let mut h = first.height_m;
        ui.horizontal(|ui| {
            ui.label("Mount height");
            if theme::field(ui, egui::DragValue::new(&mut h).range(0.3..=30.0).speed(0.05).suffix(" m").fixed_decimals(2)).changed() {
                edits.push(ApEdit::Height(h));
            }
        });
    }

    /// Where a wall point at `plan` lands once snapped.
    fn snapped(&self, plan: Pos2, zoom: f32) -> Pos2 {
        if self.no_snap {
            return plan;
        }
        let walls = self.project.as_ref().map_or(&[][..], |p| &p.floor().walls[..]);
        let s = walls::snap([plan.x, plan.y], self.wall_start.map(|p| [p.x, p.y]), walls, WALL_SNAP_PX / zoom);
        Pos2::new(s[0], s[1])
    }

    /// The first click sets the start of a wall, the second its end; the
    /// next click starts another. The new wall replaces whatever stretch of an
    /// older wall it lies along, so a door drawn over a wall opens a gap.
    fn wall_click(&mut self, plan: Pos2, zoom: f32) {
        let at = self.snapped(plan, zoom);
        let Some(start) = self.wall_start else {
            self.wall_start = Some(at);
            return;
        };
        if start.distance(at) < 1.0 {
            return;
        }
        // A room is four walls round the two corners; a wall is just the one.
        let corners = match self.wall_mode {
            WallMode::Wall => vec![([start.x, start.y], [at.x, at.y])],
            WallMode::Room => {
                if (start.x - at.x).abs() < 2.0 || (start.y - at.y).abs() < 2.0 {
                    return;
                }
                let (tl, tr, br, bl) = ([start.x.min(at.x), start.y.min(at.y)], [start.x.max(at.x), start.y.min(at.y)], [start.x.max(at.x), start.y.max(at.y)], [start.x.min(at.x), start.y.max(at.y)]);
                vec![(tl, tr), (tr, br), (br, bl), (bl, tl)]
            }
        };
        self.remember_walls(false);
        let (material, height) = (self.wall_material, self.wall_height);
        if let Some(p) = self.project.as_mut() {
            let tol = WALL_CARVE_PX / zoom;
            for (a, b) in corners {
                let mut wall = Wall::new(a, b, material);
                wall.height_m = height;
                p.floor_mut().walls = p.floor().walls.iter().flat_map(|w| w.carve(&wall, tol)).collect();
                p.floor_mut().walls.push(wall);
            }
        }
        self.wall_start = None;
        self.autosave();
    }

    /// Place, pick and move access points. Placing: click empty space to add
    /// one. Selecting: click or drag a box, Shift or Ctrl adds. Either way,
    /// dragging an access point moves it, and the whole selection with it.
    fn ap_input(&mut self, resp: &egui::Response, ui: &egui::Ui, zoom: f32, to_plan: &impl Fn(Pos2) -> Pos2, to_screen: &impl Fn(Pos2) -> Pos2, size: (f32, f32)) {
        let selecting = self.tool == Tool::Select;
        let additive = ui.input(|i| i.modifiers.shift || i.modifiers.command);
        let near = |this: &Self, screen: Pos2| -> Option<usize> {
            this.project.as_ref()?.floor().layer(this.mode == WorkMode::Plan).iter().position(|a| to_screen(Pos2::new(a.x, a.y)).distance(screen) <= AP_PICK_PX)
        };
        if resp.drag_started_by(egui::PointerButton::Primary) {
            let origin = ui.input(|i| i.pointer.press_origin());
            match origin.and_then(|p| near(self, p)) {
                Some(i) => {
                    if !self.aps_selected.contains(&i) {
                        if !additive {
                            self.aps_selected.clear();
                        }
                        self.aps_selected.insert(i);
                    }
                    self.ap_drag = Some(i);
                }
                None if selecting => self.ap_box_start = origin.map(to_plan),
                None => {}
            }
        }
        if self.ap_drag.is_some() && resp.dragged_by(egui::PointerButton::Primary) {
            let d = resp.drag_delta() / zoom;
            let moving = self.aps_selected.clone();
            let planning = self.mode == WorkMode::Plan;
            if let Some(p) = self.project.as_mut() {
                let walls = p.floor().walls.clone();
                for i in moving {
                    if let Some(a) = p.floor_mut().layer_mut(planning).get_mut(i) {
                        // An access point stays between the walls: a step outside them is not taken.
                        let (x, y) = ((a.x + d.x).clamp(0.0, size.0), (a.y + d.y).clamp(0.0, size.1));
                        if wifispanner_core::coverage::may_place(&walls, [x, y]) {
                            (a.x, a.y) = (x, y);
                        }
                    }
                }
            }
        }
        if resp.drag_stopped_by(egui::PointerButton::Primary) {
            if self.ap_drag.take().is_some() {
                self.autosave();
            } else if let (Some(a), Some(b)) = (self.ap_box_start.take(), resp.interact_pointer_pos().map(to_plan)) {
                let (min, max) = (Pos2::new(a.x.min(b.x), a.y.min(b.y)), Pos2::new(a.x.max(b.x), a.y.max(b.y)));
                if !additive {
                    self.aps_selected.clear();
                }
                if let Some(p) = &self.project {
                    let inside = p.floor().layer(self.mode == WorkMode::Plan).iter().enumerate().filter(|(_, ap)| ap.x >= min.x && ap.x <= max.x && ap.y >= min.y && ap.y <= max.y).map(|(i, _)| i);
                    self.aps_selected.extend(inside);
                }
            }
        }
        if resp.clicked()
            && let Some(click) = resp.interact_pointer_pos()
        {
            match (near(self, click), additive) {
                (Some(i), true) => {
                    if !self.aps_selected.remove(&i) {
                        self.aps_selected.insert(i);
                    }
                }
                (Some(i), false) => self.aps_selected = BTreeSet::from([i]),
                (None, _) if selecting => {
                    if !additive {
                        self.aps_selected.clear();
                    }
                }
                (None, _) => {
                    let p = to_plan(click);
                    let allowed = self.project.as_ref().is_none_or(|pr| wifispanner_core::coverage::may_place(&pr.floor().walls, [p.x, p.y]));
                    if !allowed {
                        self.status = "Access points go between walls, not outside the premises.".into();
                    } else if p.x >= 0.0 && p.y >= 0.0 && p.x <= size.0 && p.y <= size.1 {
                        let planning = self.mode == WorkMode::Plan;
                        // Installed access points are plotted bare: their model and settings are not known while surveying.
                        let template = if planning { self.ap_template.clone() } else { PlannedAp::new(String::new(), 0.0, 0.0) };
                        if let Some(pr) = self.project.as_mut() {
                            let fl = pr.floor_mut();
                            let name = format!("AP {}", fl.layer(planning).len() + 1);
                            let ap = PlannedAp { name, x: p.x, y: p.y, existing: !planning, ..template };
                            if planning {
                                let mut ap = ap;
                                ap.id = fl.next_ap_id();
                                fl.aps.push(ap);
                            } else {
                                // Plotting an installed access point also leaves a copy in the plan.
                                fl.plot_installed(ap);
                            }
                            self.aps_selected = BTreeSet::from([fl.layer(planning).len() - 1]);
                        }
                        self.autosave();
                    }
                }
            }
        }
        if ui.input(|i| i.key_pressed(egui::Key::Delete)) {
            self.apply_ap_edit(ApEdit::Delete);
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.aps_selected.clear();
        }
        if selecting
            && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::A))
            && let Some(p) = &self.project
        {
            self.aps_selected = (0..p.floor().layer(self.mode == WorkMode::Plan).len()).collect();
        }
    }

    /// Draw an obstacle area: two corners for a rectangle, or points round a
    /// polygon closed by a double-click, Enter or a click on the first point.
    fn area_input(&mut self, resp: &egui::Response, ui: &egui::Ui, zoom: f32, to_plan: &impl Fn(Pos2) -> Pos2, size: (f32, f32)) {
        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
        if resp.secondary_clicked() || escape {
            self.area_start = None;
            self.poly_points.clear();
            return;
        }
        let inside = |p: Pos2| p.x >= 0.0 && p.y >= 0.0 && p.x <= size.0 && p.y <= size.1;
        let click = resp.interact_pointer_pos().map(to_plan).filter(|p| inside(*p));
        match self.area_mode {
            AreaMode::Rectangle => {
                if resp.clicked()
                    && let Some(p) = click
                {
                    match self.area_start.take() {
                        None => self.area_start = Some(p),
                        Some(a) if (a.x - p.x).abs() > 2.0 && (a.y - p.y).abs() > 2.0 => self.add_area(Area::rectangle([a.x, a.y], [p.x, p.y], self.obstacle_kind)),
                        Some(_) => {}
                    }
                }
            }
            AreaMode::Polygon => {
                let finish = resp.double_clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter));
                if resp.clicked()
                    && let Some(p) = click
                {
                    let closing = self.poly_points.len() >= 3 && self.poly_points[0].distance(p) <= WALL_SNAP_PX / zoom;
                    if closing {
                        self.finish_polygon();
                    } else if !finish {
                        self.poly_points.push(p);
                    } else {
                        // The second click of a double-click lands where the first did.
                        self.finish_polygon();
                    }
                } else if finish {
                    self.finish_polygon();
                }
            }
        }
    }

    fn finish_polygon(&mut self) {
        let mut pts: Vec<[f32; 2]> = self.poly_points.drain(..).map(|p| [p.x, p.y]).collect();
        pts.dedup_by(|a, b| (a[0] - b[0]).abs() < 1.0 && (a[1] - b[1]).abs() < 1.0);
        if pts.len() >= 3 {
            self.add_area(Area::new(pts, self.obstacle_kind));
        }
    }

    fn add_area(&mut self, mut area: Area) {
        self.remember_walls(false);
        area.height_m = self.obstacle_height;
        if let Some(p) = self.project.as_mut() {
            p.floor_mut().areas.push(area);
        }
        self.autosave();
    }

    /// Put the simulated client where the pointer is while it is pressed or clicked.
    fn client_input(&mut self, resp: &egui::Response, to_plan: &impl Fn(Pos2) -> Pos2, size: (f32, f32)) {
        if (resp.dragged_by(egui::PointerButton::Primary) || resp.clicked())
            && let Some(p) = resp.interact_pointer_pos().map(to_plan)
        {
            self.client = Some(Pos2::new(p.x.clamp(0.0, size.0), p.y.clamp(0.0, size.1)));
        }
    }

    /// Drag a wall or an obstacle area (whichever of `walls` and `areas` this tool deals with) from
    /// where the press began: it moves, with the rest of the selection if it is part of it. A drag
    /// that does not begin on one is left alone.
    fn object_drag(&mut self, resp: &egui::Response, ui: &egui::Ui, zoom: f32, to_plan: &impl Fn(Pos2) -> Pos2, to_screen: &impl Fn(Pos2) -> Pos2, walls: bool, areas: bool, avoid_devices: bool) {
        if resp.drag_started_by(egui::PointerButton::Primary) {
            self.obj_drag = false;
            let press = ui.input(|i| i.pointer.press_origin());
            // A device under the press takes the drag (it is moved instead).
            let on_device = avoid_devices && press.is_some_and(|o| self.project.as_ref().is_some_and(|p| p.floor().layer(self.mode == WorkMode::Plan).iter().any(|a| to_screen(Pos2::new(a.x, a.y)).distance(o) <= AP_PICK_PX)));
            let origin = press.map(to_plan).filter(|_| !on_device);
            let hit = origin.and_then(|p| {
                let floor = self.project.as_ref()?.floor();
                let radius = WALL_PICK_PX / zoom;
                let wall = if walls {
                    floor.walls.iter().enumerate().map(|(i, w)| (i, w.distance_to([p.x, p.y]))).filter(|(_, d)| *d <= radius).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(i, _)| i)
                } else {
                    None
                };
                let area = if wall.is_none() && areas { floor.areas.iter().rposition(|a| a.contains([p.x, p.y])) } else { None };
                (wall.is_some() || area.is_some()).then_some((wall, area))
            });
            if let Some((wall, area)) = hit {
                let already = wall.is_some_and(|i| self.selected_walls.contains(&i)) || area.is_some_and(|i| self.selected_areas.contains(&i));
                if !already {
                    if !ui.input(|i| i.modifiers.shift || i.modifiers.command) {
                        self.selected_walls.clear();
                        self.selected_areas.clear();
                    }
                    self.selected_walls.extend(wall);
                    self.selected_areas.extend(area);
                }
                self.remember_walls(false);
                self.obj_drag = true;
            }
        }
        if self.obj_drag && resp.dragged_by(egui::PointerButton::Primary) {
            let d = resp.drag_delta() / zoom;
            let (selected_walls, selected_areas) = (self.selected_walls.clone(), self.selected_areas.clone());
            if let Some(p) = self.project.as_mut() {
                let floor = p.floor_mut();
                for i in selected_walls {
                    if let Some(w) = floor.walls.get_mut(i) {
                        (w.a[0], w.a[1], w.b[0], w.b[1]) = (w.a[0] + d.x, w.a[1] + d.y, w.b[0] + d.x, w.b[1] + d.y);
                    }
                }
                for i in selected_areas {
                    if let Some(a) = floor.areas.get_mut(i) {
                        for pt in a.points.iter_mut() {
                            *pt = [pt[0] + d.x, pt[1] + d.y];
                        }
                    }
                }
            }
        }
        if self.obj_drag && resp.drag_stopped_by(egui::PointerButton::Primary) {
            self.obj_drag = false;
            self.heat_dirty = true;
            self.pred_dirty = true;
            self.autosave();
        }
    }

    /// Which kinds (walls, obstacle areas, devices) the Select tool works on.
    fn select_kinds(&self) -> (bool, bool, bool) {
        match self.select_only {
            Some(Tool::Walls) => (true, false, false),
            Some(Tool::Obstacles) => (false, true, false),
            Some(Tool::Aps) => (false, false, true),
            _ => (true, true, true),
        }
    }

    /// Select walls and obstacle areas by clicking or dragging a box; Shift or Ctrl adds.
    fn select_walls(&mut self, resp: &egui::Response, ui: &egui::Ui, zoom: f32, to_plan: &impl Fn(Pos2) -> Pos2, to_screen: &impl Fn(Pos2) -> Pos2) {
        let (walls_ok, areas_ok, devices_ok) = self.select_kinds();
        let additive = ui.input(|i| i.modifiers.shift || i.modifiers.command);
        let Some(floor) = self.project.as_ref().map(|p| p.floor()) else { return };
        if resp.drag_started_by(egui::PointerButton::Primary) {
            // A box starts only in open space: not on a device, wall or obstacle, which the drag moves instead.
            let origin = ui.input(|i| i.pointer.press_origin());
            let on_device = devices_ok && origin.is_some_and(|o| floor.layer(self.mode == WorkMode::Plan).iter().any(|a| to_screen(Pos2::new(a.x, a.y)).distance(o) <= AP_PICK_PX));
            self.box_start = if self.obj_drag || on_device { None } else { origin.map(to_plan) };
        }
        if resp.drag_stopped_by(egui::PointerButton::Primary) && self.ap_drag.is_some() {
            // A device was being moved, not a box drawn.
            self.box_start = None;
        } else if resp.drag_stopped_by(egui::PointerButton::Primary)
            && let (Some(a), Some(b)) = (self.box_start.take(), resp.interact_pointer_pos().map(to_plan))
        {
            let (min, max) = ([a.x.min(b.x), a.y.min(b.y)], [a.x.max(b.x), a.y.max(b.y)]);
            let hit = floor.walls.iter().enumerate().filter(|(_, w)| walls_ok && w.intersects_rect(min, max)).map(|(i, _)| i);
            let hit_areas = floor.areas.iter().enumerate().filter(|(_, ar)| areas_ok && ar.intersects_rect(min, max)).map(|(i, _)| i);
            if !additive {
                if walls_ok {
                    self.selected_walls.clear();
                }
                if areas_ok {
                    self.selected_areas.clear();
                }
            }
            self.selected_walls.extend(hit);
            self.selected_areas.extend(hit_areas);
        } else if resp.clicked()
            && let Some(p) = resp.interact_pointer_pos().map(to_plan)
        {
            let radius = WALL_PICK_PX / zoom;
            let nearest = floor
                .walls
                .iter()
                .enumerate()
                .map(|(i, w)| (i, w.distance_to([p.x, p.y])))
                .filter(|(_, d)| walls_ok && *d <= radius)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i);
            // With no wall under the click, an area under it is picked (the last drawn is on top).
            let area = if nearest.is_none() && areas_ok { floor.areas.iter().rposition(|a| a.contains([p.x, p.y])) } else { None };
            if !additive {
                if walls_ok {
                    self.selected_walls.clear();
                }
                if areas_ok {
                    self.selected_areas.clear();
                }
            }
            if let Some(i) = nearest {
                if additive && self.selected_walls.contains(&i) {
                    self.selected_walls.remove(&i);
                } else {
                    self.selected_walls.insert(i);
                }
            } else if let Some(i) = area {
                if additive && self.selected_areas.contains(&i) {
                    self.selected_areas.remove(&i);
                } else {
                    self.selected_areas.insert(i);
                }
            }
        }
    }

    /// Save the project, then open the dialog that chooses what the report contains.
    fn make_report(&mut self) {
        if self.path.is_none() {
            self.save_as();
        }
        let Some(path) = self.path.clone() else { return };
        self.save_to(&path);
        let Some(project) = &self.project else { return };
        self.report_dialog = Some(report::OptionsDialog::new(path, project.clone()));
    }

    fn report_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.report_dialog.take() else { return };
        match dialog.show(ctx) {
            report::DialogResult::Open => self.report_dialog = Some(dialog),
            report::DialogResult::Cancel => {}
            report::DialogResult::Generate(opts) => {
                match report::write_report(dialog.path(), dialog.project(), &report::ReportStyle::from_settings(&self.view), &opts) {
                    Ok(file) => {
                        self.status = format!("Report saved: {}", file.display());
                        // Show the folder with the new report highlighted rather than opening it in a browser.
                        if let Err(e) = store::reveal(&file) {
                            self.status.push_str(&format!(" ({e})"));
                        }
                    }
                    Err(e) => self.status = format!("Could not create report: {e}"),
                }
            }
        }
    }

    fn save_as(&mut self) {
        let name = self.project.as_ref().map_or("survey".into(), |p| p.floor().name.clone());
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("WiFi Spanner project", &[store::EXTENSION])
            .set_directory(store::projects_dir())
            .set_file_name(format!("{name}.wifispanner"))
            .save_file()
        {
            self.save_to(&path);
            if self.path.is_some() {
                self.status = format!("Saved to {}", path.display());
            }
        }
    }

    /// Ask before replacing a project that has measurements and no save file.
    fn confirm_discard(&self) -> bool {
        let unsaved = self.path.is_none() && self.project.as_ref().is_some_and(|p| !p.floor().points.is_empty());
        !unsaved
            || rfd::MessageDialog::new()
                .set_title("Discard survey?")
                .set_description("The current survey has not been saved. Discard its measurements?")
                .set_buttons(rfd::MessageButtons::YesNo)
                .show()
                == rfd::MessageDialogResult::Yes
    }

    /// The selected survey point: what was heard there, and delete. Shown
    /// over the left of the plan only while a point is selected.
    fn point_bubble(&mut self, ctx: &egui::Context, canvas: Rect) {
        let r = theme::roles(ctx);
        let Some(i) = self.selected else { return };
        let Some(p) = self.project.as_ref().and_then(|p| p.floor().points.get(i)) else { return };
        let mut delete = false;
        let mut close = false;
        egui::Area::new(egui::Id::unique("point_bubble"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::LEFT_TOP)
            .fixed_pos(canvas.left_top() + Vec2::new(12.0, 12.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(r.surface_container_high)
                    .stroke(Stroke::new(1.0, r.outline_variant))
                    .corner_radius(theme::radius(ctx, theme::Shape::Panel, 0.0))
                    .inner_margin(14)
                    .shadow(theme::shadow(r.shadow, 16, 0.18))
                    .show(ui, |ui| {
                        ui.set_width(300.0);
                        ui.horizontal(|ui| {
                            ui.label(theme::heading(ui, format!("Point #{}", i + 1)));
                            ui.label(egui::RichText::new(format!("{} APs, {} s dwell", p.observations.len(), p.dwell_secs)).text_style(egui::TextStyle::Body).color(r.on_surface_variant));
                        });
                        let mut obs: Vec<_> = p.observations.iter().collect();
                        obs.sort_by(|a, b| b.signal_dbm.total_cmp(&a.signal_dbm));
                        egui::ScrollArea::vertical().id_salt("obs").max_height(260.0).show(ui, |ui| {
                            theme::data_table(ui, "obs_grid", &["SSID", "Ch", "Band", "dBm", "Seen"], 10.0, |ui| {
                                for o in obs {
                                    theme::cell(ui, |ui| ui.label(o.ssid.as_deref().unwrap_or("<hidden>")));
                                    theme::cell(ui, |ui| ui.label(o.channel.to_string()));
                                    theme::cell(ui, |ui| ui.label(o.band().to_string()));
                                    theme::cell(ui, |ui| ui.colored_label(signal_color(o.signal_dbm, ui.visuals().dark_mode), format!("{:.0}", o.signal_dbm)));
                                    theme::cell(ui, |ui| ui.label(o.samples.to_string()));
                                    ui.end_row();
                                }
                            });
                        });
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            // Planning only looks at the survey; it never changes it.
                            if self.mode == WorkMode::Survey && theme::button(ui, ButtonKind::Outlined, Some(ICON_DELETE), "Delete").clicked() {
                                delete = true;
                            }
                            if theme::button(ui, ButtonKind::Text, None, "Close").clicked() {
                                close = true;
                            }
                        });
                    });
            });
        if close {
            self.selected = None;
        }
        if delete {
            if let Some(p) = self.project.as_mut() {
                p.floor_mut().points.remove(i);
            }
            self.selected = None;
            self.heat_dirty = true;
            self.autosave();
        }
    }

    /// The button over the top right of the plan that opens the floors panel.
    fn layers_button(&mut self, ctx: &egui::Context, canvas: Rect) {
        let r = theme::roles(ctx);
        if self.layers_icon.is_none() {
            self.layers_icon = decode_texture(ctx, LAYERS_ICON, "layers-icon");
        }
        let open = self.floors_open;
        let mut toggled = false;
        egui::Area::new(egui::Id::unique("layers_button"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(canvas.right_top() + Vec2::new(-12.0, 12.0))
            .show(ctx, |ui| {
                let (rect, resp) = ui.allocate_exact_size(Vec2::splat(round_size(ctx)), Sense::click());
                let fill = if open { r.secondary_container } else { r.surface_container_high };
                let fill = if resp.hovered() { theme::state_layer(fill, r.on_surface, 0.08) } else { fill };
                let painter = ui.painter();
                painter.rect(rect, theme::radius(ctx, theme::Shape::Pill, round_size(ctx)), fill, Stroke::new(1.0, r.outline_variant), egui::StrokeKind::Inside);
                let layers = if theme::skin(ctx) == theme::Skin::Carbon { theme::carbon_layers(ctx) } else { self.layers_icon.clone() };
                if let Some(icon) = &layers {
                    let side = round_size(ctx) * 0.5;
                    let colour = if open { r.on_secondary_container } else { r.on_surface };
                    painter.image(icon.id(), Rect::from_center_size(rect.center(), Vec2::splat(side)), Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), colour);
                }
                toggled = resp.on_hover_text("Floors").clicked();
            });
        if toggled {
            self.floors_open = !self.floors_open;
        }
    }

    /// Move the planning access points to where they cover best, in the background.
    fn start_optimise(&mut self, move_aps: bool) {
        let Some(project) = self.project.as_ref() else { return };
        let mut floor = project.floor().clone();
        floor.image = Vec::new();
        let (band, rx) = (self.pred_band, self.rx_height);
        let goal = wifispanner_core::optimise::Goal { good_dbm: self.view.plan_options().good_dbm(wifispanner_core::predict::RadioBand::from_band(band)), move_aps };
        let (tx, rx_chan) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(wifispanner_core::optimise::optimise(&floor, band, rx, &goal).map_err(|e| e.to_string()));
        });
        self.optimising = Some(rx_chan);
        self.optimise_checking = !move_aps;
    }

    /// Collect the optimiser's answer and apply it.
    fn optimise_tick(&mut self) {
        let Some(chan) = &self.optimising else { return };
        let Ok(result) = chan.try_recv() else { return };
        self.optimising = None;
        let options = self.view.plan_options();
        let checking = std::mem::take(&mut self.optimise_checking);
        let good_dbm = options.good_dbm(wifispanner_core::predict::RadioBand::from_band(self.pred_band));
        let had_report = self.suggestion.is_some();
        let Some(fl) = self.project.as_mut().map(|p| p.floor_mut()) else { return };
        let template_id = fl.aps.get(match &result { Ok(o) => o.template, Err(_) => 0 }).map_or(0, |a| a.id);
        match result {
            Err(why) => self.status = why,
            Ok(out) => {
                let percent = |v: f32| (v * 100.0).round() as i32;
                let mut manual = Vec::new();
                let mut placed = Vec::new();
                for (i, [x, y]) in out.moves.iter().copied() {
                    if let Some(a) = fl.aps.get_mut(i) {
                        manual.push((a.id, [a.x, a.y]));
                        (a.x, a.y) = (x, y);
                        placed.push((a.id, [x, y]));
                    }
                }
                // The channel plan goes with the new layout.
                let report = apply_channel_plan(fl, &options);
                if !placed.is_empty() {
                    self.optimised = Some(Optimised { manual, placed, added: Vec::new() });
                    self.pred_dirty = true;
                    self.autosave();
                }
                let covered = format!("{}% of the floor has a good signal ({good_dbm:.0} dBm or better)", percent(out.coverage_after));
                self.status = match (checking, placed_any(&out), out.extra.is_empty()) {
                    (true, _, true) => format!("Channel plan updated for the new settings: {covered}."),
                    (true, _, false) => format!("Channel plan updated for the new settings: {covered}. More access points are recommended."),
                    (false, true, true) => format!("Access points moved for the best coverage: {covered}. Click the button again to put them back."),
                    (false, false, true) => format!("The access points are already well placed: {covered}."),
                    (false, moved, false) => format!("{}{covered}. More access points are recommended.", if moved { "Access points moved. Now " } else { "" }),
                };
                // The report stays up until dismissed. After a settings change it only comes up when
                // there is something to act on, or when it was already showing.
                if !checking || had_report || !out.extra.is_empty() || !report.warnings.is_empty() {
                    self.suggestion = Some(Suggestion {
                        extra: out.extra,
                        template_id,
                        after: out.coverage_after,
                        with_extra: out.coverage_with_extra,
                        roaming: out.roaming_after,
                        widths: report.widths,
                        warnings: report.warnings,
                        dfs: options.allow_dfs,
                        good_dbm,
                        rechecked: checking,
                    });
                }
                self.autosave();
            }
        }
    }

    /// The optimiser's report: coverage, roaming, more access points to add, and the channel plan.
    fn suggestion_dialog(&mut self, ctx: &egui::Context) {
        let Some(s) = &self.suggestion else { return };
        let n = s.extra.len();
        let (mut add, mut close, mut repaint) = (false, false, false);
        let percent = |v: f32| (v * 100.0).round() as i32;
        let rows: Vec<(String, Vec<wifispanner_core::predict::Channel>)> = self
            .project
            .as_ref()
            .map(|p| p.floor().aps.iter().filter(|a| a.enabled).map(|a| (a.name.clone(), a.channels.clone())).collect())
            .unwrap_or_default();
        let r = theme::roles(ctx);
        theme::dialog(ctx, if s.rechecked { "Plan updated for the new settings" } else { "Access point plan" }, 520.0, |ui| {
            ui.label(format!("{}% of the floor has a good signal ({:.0} dBm or better for these channel widths).", percent(s.after), s.good_dbm));
            let roam = percent(s.roaming);
            ui.label(egui::RichText::new(format!(
                "{roam}% of the floor also hears a second access point at {} dBm or better, so there is room to roam.",
                wifispanner_core::optimise::ROAM_DBM
            )).color(if roam < 50 { r.error } else { r.on_surface_variant }));
            if n > 0 {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(format!("{n} more access point{} recommended", if n == 1 { "" } else { "s" })).strong());
                ui.label(format!("Adding {} would raise the good signal to {}% of the floor. The new one{} copy{} the strongest access point and go where {} help most.", if n == 1 { "it" } else { "them" }, percent(s.with_extra), if n == 1 { "" } else { "s" }, if n == 1 { "ies" } else { "" }, if n == 1 { "it" } else { "they" }));
            }
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Channel plan").strong());
            let widths: Vec<String> = s.widths.iter().map(|(b, w)| format!("{} {w} MHz", b.band())).collect();
            ui.weak(format!(
                "Neighbouring access points are on different channels to avoid interference, and overlap enough to roam. Planned at {}; DFS channels {}. Change these in Settings > Channels.",
                widths.join(", "),
                if s.dfs { "allowed" } else { "not used" }
            ));
            for w in &s.warnings {
                ui.label(egui::RichText::new(w).text_style(egui::TextStyle::Body).color(r.error));
            }
            let bands: Vec<wifispanner_core::predict::RadioBand> = wifispanner_core::predict::RadioBand::ALL.into_iter().filter(|b| rows.iter().any(|r| r.1.iter().any(|c| c.band == *b))).collect();
            let names: Vec<String> = std::iter::once("Access point".to_string()).chain(bands.iter().map(|b| b.band().to_string())).collect();
            let header: Vec<&str> = names.iter().map(String::as_str).collect();
            egui::ScrollArea::vertical().max_height(220.0).auto_shrink([false, true]).show(ui, |ui| {
                theme::data_table(ui, "channel_plan", &header, 24.0, |ui| {
                    for (name, chans) in &rows {
                        theme::cell(ui, |ui| ui.label(name));
                        for b in &bands {
                            theme::cell(ui, |ui| ui.label(chans.iter().find(|c| c.band == *b).map_or("-".to_string(), |c| c.number.to_string())));
                        }
                        ui.end_row();
                    }
                });
            });
            theme::actions(ui, |ui| {
                if n > 0 && theme::button(ui, ButtonKind::Filled, Some(ICON_ADD), &format!("Add {n}")).clicked() {
                    add = true;
                }
                if theme::button(ui, if n > 0 { ButtonKind::Text } else { ButtonKind::Filled }, None, if n > 0 { "Not now" } else { "Done" }).clicked() {
                    close = true;
                }
                if theme::button(ui, ButtonKind::Text, Some(ICON_BRUSH), "Change area").on_hover_text("Paint the area the optimiser works in again.").clicked() {
                    close = true;
                    repaint = true;
                }
            });
        });
        if repaint {
            self.zone_return = Some(self.tool);
            self.set_tool(Tool::Zone);
        }
        let options = self.view.plan_options();
        if add && let Some(s) = self.suggestion.as_mut() {
            let (extra, template_id) = (std::mem::take(&mut s.extra), s.template_id);
            s.after = s.with_extra;
            if let Some(fl) = self.project.as_mut().map(|p| p.floor_mut()) {
                let template = fl.aps.iter().find(|a| a.id == template_id).cloned();
                let mut ids = Vec::new();
                for [x, y] in extra {
                    let mut ap = template.clone().unwrap_or_else(|| PlannedAp::new(String::new(), 0.0, 0.0));
                    ap.name = format!("AP {}", fl.aps.len() + 1);
                    (ap.x, ap.y) = (x, y);
                    ap.id = fl.next_ap_id();
                    (ap.existing, ap.from, ap.link, ap.enabled) = (false, None, None, true);
                    ap.channels.clear();
                    ids.push(ap.id);
                    fl.aps.push(ap);
                }
                let report = apply_channel_plan(fl, &options);
                if let Some(s) = self.suggestion.as_mut() {
                    (s.widths, s.warnings) = (report.widths, report.warnings);
                }
                let o = self.optimised.get_or_insert(Optimised { manual: Vec::new(), placed: Vec::new(), added: Vec::new() });
                o.added.extend(ids);
            }
            self.status = "Access points added. Click the optimise button again to take them back out.".into();
            self.pred_dirty = true;
            self.autosave();
        } else if close {
            self.suggestion = None;
            self.autosave();
        }
    }

    /// Back to where the access points stood before the optimiser.
    fn undo_optimise(&mut self) {
        let (Some(o), Some(fl)) = (self.optimised.take(), self.project.as_mut().map(|p| p.floor_mut())) else { return };
        fl.aps.retain(|a| !o.added.contains(&a.id));
        self.aps_selected.clear();
        // (the channel plan is redone below, once the access points are back in place)
        for (id, [x, y]) in o.manual {
            if let Some(a) = fl.aps.iter_mut().find(|a| a.id == id) {
                (a.x, a.y) = (x, y);
            }
        }
        let options = self.view.plan_options();
        apply_channel_plan(self.project.as_mut().map(|p| p.floor_mut()).expect("a project is open"), &options);
        self.status = "Access points put back where you had them.".into();
        self.pred_dirty = true;
        self.autosave();
    }

    /// Every access point that stands for a surveyed one goes back to where it was surveyed.
    fn restore_survey_positions(&mut self) {
        self.undo_optimise();
        let Some(fl) = self.project.as_mut().map(|p| p.floor_mut()) else { return };
        let surveyed: Vec<(u64, [f32; 2])> = fl.survey_aps.iter().map(|a| (a.id, [a.x, a.y])).collect();
        let mut n = 0;
        for a in fl.aps.iter_mut() {
            if let Some((_, [x, y])) = a.from.and_then(|id| surveyed.iter().find(|s| s.0 == id)) {
                (a.x, a.y) = (*x, *y);
                n += 1;
            }
        }
        self.status = format!("{n} access point(s) back at their surveyed positions.");
        self.pred_dirty = true;
        self.autosave();
    }

    /// The button under the devices button: a click optimises or undoes, a long press restores the survey.
    fn optimise_button(&mut self, ctx: &egui::Context, canvas: Rect) {
        self.optimise_tick();
        self.suggestion_dialog(ctx);
        // New channel settings: redo the channel plan and the access point recommendations at once.
        let options = self.view.plan_options();
        if self.optimising.is_none() {
            match self.last_plan_options {
                Some(last) if last != options => {
                    self.last_plan_options = Some(options);
                    let planned = self.project.as_ref().is_some_and(|p| p.floor().metres_per_px.is_some() && p.floor().aps.iter().any(|a| a.enabled));
                    if planned {
                        self.start_optimise(false);
                    }
                }
                None => self.last_plan_options = Some(options),
                _ => {}
            }
        }
        // A hand edit of an optimised access point makes the layout the user's own again.
        if let (Some(o), Some(fl)) = (&self.optimised, self.project.as_ref().map(|p| p.floor())) {
            if o.placed.iter().any(|(id, [x, y])| fl.aps.iter().find(|a| a.id == *id).is_none_or(|a| (a.x, a.y) != (*x, *y))) {
                self.optimised = None;
            }
        }
        let r = theme::roles(ctx);
        let (active, busy) = (self.optimised.is_some(), self.optimising.is_some());
        let (mut click, mut long_press) = (false, false);
        let held = self.optimise_hold;
        egui::Area::new(egui::Id::unique("optimise_button"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(canvas.right_top() + Vec2::new(-12.0, 12.0 + 3.0 * (round_size(ctx) + 8.0)))
            .show(ctx, |ui| {
                let (rect, resp) = ui.allocate_exact_size(Vec2::splat(round_size(ctx)), Sense::click());
                let fill = if active { r.secondary_container } else { r.surface_container_high };
                let fill = if resp.hovered() { theme::state_layer(fill, r.on_surface, 0.08) } else { fill };
                let painter = ui.painter();
                painter.rect(rect, theme::radius(ctx, theme::Shape::Pill, round_size(ctx)), fill, Stroke::new(1.0, r.outline_variant), egui::StrokeKind::Inside);
                let colour = if active { r.on_secondary_container } else { r.on_surface };
                theme::paint_icon(painter, ctx, egui_material_icons::icons::ICON_AUTO_FIX_HIGH, rect.center(), round_size(ctx) * 0.5, colour);
                if let Some((since, false)) = held {
                    // A ring fills while the button is held.
                    let frac = (since.elapsed().as_secs_f32() / HOLD_SECS).min(1.0);
                    let radius = rect.width() * 0.5 - 2.0;
                    let points: Vec<Pos2> = (0..=(40.0 * frac) as usize).map(|i| {
                        let a = -std::f32::consts::FRAC_PI_2 + i as f32 / 40.0 * std::f32::consts::TAU;
                        rect.center() + Vec2::new(a.cos(), a.sin()) * radius
                    }).collect();
                    if points.len() > 1 {
                        painter.add(egui::Shape::line(points, Stroke::new(3.0, r.primary)));
                    }
                }
                let tip = if busy {
                    "Optimising..."
                } else if active {
                    "Click to put the access points back as you had them. Hold to restore the survey positions."
                } else {
                    "Optimise access point positions for the best coverage. Hold to restore the survey positions."
                };
                let resp = resp.on_hover_text(tip);
                if resp.is_pointer_button_down_on() {
                    let (since, fired) = held.unwrap_or((std::time::Instant::now(), false));
                    if !fired && since.elapsed().as_secs_f32() >= HOLD_SECS {
                        long_press = true;
                    }
                    self.optimise_hold = Some((since, fired || long_press));
                    ui.ctx().request_repaint();
                } else {
                    let fired = held.is_some_and(|h| h.1);
                    self.optimise_hold = None;
                    click = resp.clicked() && !fired;
                }
            });
        if busy {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if long_press {
            self.restore_survey_positions();
        } else if click && !busy {
            if self.optimised.is_some() {
                self.undo_optimise();
            } else if self.project.as_ref().is_some_and(|p| p.floor().metres_per_px.is_some() && p.floor().zone.is_empty()) {
                // The area to work in comes first: paint it, then the optimiser runs. Once painted
                // it is remembered with the project, and the optimiser goes straight to work.
                self.zone_return = Some(self.tool);
                self.set_tool(Tool::Zone);
            } else {
                self.start_optimise(true);
            }
        }
    }

    /// The Select tool's button, under the floors button.
    fn select_button(&mut self, ctx: &egui::Context, canvas: Rect) {
        let r = theme::roles(ctx);
        let active = self.tool == Tool::Select;
        let mut picked = false;
        egui::Area::new(egui::Id::unique("select_button"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(canvas.right_top() + Vec2::new(-12.0, 12.0 + round_size(ctx) + 8.0))
            .show(ctx, |ui| {
                let (rect, resp) = ui.allocate_exact_size(Vec2::splat(round_size(ctx)), Sense::click());
                let fill = if active { r.secondary_container } else { r.surface_container_high };
                let fill = if resp.hovered() { theme::state_layer(fill, r.on_surface, 0.08) } else { fill };
                let painter = ui.painter();
                painter.rect(rect, theme::radius(ctx, theme::Shape::Pill, round_size(ctx)), fill, Stroke::new(1.0, r.outline_variant), egui::StrokeKind::Inside);
                let colour = if active { r.on_secondary_container } else { r.on_surface };
                paint_select(painter, rect.center(), round_size(ctx) * 0.5, colour, fill);
                picked = resp.on_hover_text("Select").clicked();
            });
        if picked {
            self.set_tool(Tool::Select);
        }
    }

    /// The button under the floors button that opens the Add Devices menu.
    fn devices_button(&mut self, ctx: &egui::Context, canvas: Rect) {
        let r = theme::roles(ctx);
        let open = self.devices_open && (self.tool == Tool::Aps || (self.tool == Tool::Select && self.select_only == Some(Tool::Aps)));
        let mut toggled = false;
        egui::Area::new(egui::Id::unique("devices_button"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(canvas.right_top() + Vec2::new(-12.0, 12.0 + 2.0 * (round_size(ctx) + 8.0)))
            .show(ctx, |ui| {
                let (rect, resp) = ui.allocate_exact_size(Vec2::splat(round_size(ctx)), Sense::click());
                let fill = if open { r.secondary_container } else { r.surface_container_high };
                let fill = if resp.hovered() { theme::state_layer(fill, r.on_surface, 0.08) } else { fill };
                let painter = ui.painter();
                painter.rect(rect, theme::radius(ctx, theme::Shape::Pill, round_size(ctx)), fill, Stroke::new(1.0, r.outline_variant), egui::StrokeKind::Inside);
                let colour = if open { r.on_secondary_container } else { r.on_surface };
                paint_add_device(painter, rect.center(), round_size(ctx) * 0.5, colour, fill);
                toggled = resp.on_hover_text("Add Devices").clicked();
            });
        if toggled {
            self.devices_open = !self.devices_open;
            if self.devices_open {
                self.set_tool(Tool::Aps);
            }
        }
    }

    fn canvas(&mut self, ui: &mut egui::Ui, env: &Env) {
        let overlay_tex = self.overlay_texture(ui.ctx());
        let (Some(project), Some(tex)) = (self.project.as_ref(), self.texture.clone()) else {
            let ctx = ui.ctx().clone();
            theme::empty_state(ui, ICON_IMAGE, "No floor plan yet", "Open a floor plan or a saved project to start surveying.", |ui| {
                if theme::button(ui, ButtonKind::Filled, Some(ICON_IMAGE), "Open floor plan").clicked() {
                    self.pick_plan(&ctx);
                }
                ui.add_space(8.0);
                if theme::button(ui, ButtonKind::Tonal, Some(ICON_FOLDER_OPEN), "Open project").clicked() {
                    self.pick_project(&ctx);
                }
            });
            return;
        };
        let (img_w, img_h) = (project.floor().width_px as f32, project.floor().height_px as f32);
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());

        if self.needs_fit && rect.width() > 1.0 {
            self.zoom = (rect.width() / img_w).min(rect.height() / img_h) * 0.95;
            self.offset = (rect.size() - Vec2::new(img_w, img_h) * self.zoom) / 2.0;
            self.needs_fit = false;
        }
        // Selecting by box uses the primary drag, so the plan pans with the
        // middle or right button then.
        // In pen mode a finger only moves the plan: it never draws, selects or places.
        let finger = ui.ctx().data(|d| d.get_temp::<bool>(egui::Id::NULL.with("finger_touching"))).unwrap_or(false) && self.view.pen_mode;
        let panning = if finger {
            resp.dragged()
        } else if self.tool == Tool::Aps {
            resp.dragged() && self.ap_drag.is_none()
        } else if matches!(self.tool, Tool::Select | Tool::Align | Tool::Client | Tool::Zone) {
            resp.dragged_by(egui::PointerButton::Middle) || resp.dragged_by(egui::PointerButton::Secondary)
        } else {
            resp.dragged() && !self.obj_drag
        };
        if panning {
            self.offset += resp.drag_delta();
        }
        // Two fingers pan (and, through the zoom factor below, pinch to zoom) with any tool, like
        // drawing apps; the first finger's own drag, such as a selection box, is dropped.
        if let Some(touch) = ui.input(|i| i.multi_touch()) {
            self.offset += touch.translation_delta;
            self.box_start = None;
            self.ap_box_start = None;
        }
        if self.tool == Tool::Align
            && resp.dragged_by(egui::PointerButton::Primary)
            && let (Some((o, _)), Some(p)) = (&overlay_tex, self.project.as_mut())
        {
            let d = resp.drag_delta() / self.zoom;
            let active = p.active;
            p.nudge_overlay(*o, active, [d.x, d.y]);
        }
        if self.tool == Tool::Align && resp.drag_stopped_by(egui::PointerButton::Primary) {
            self.autosave();
        }
        if let Some(cursor) = resp.hover_pos() {
            let factor = ui.input(|i| (i.smooth_scroll_delta.y * 0.0015).exp() * i.zoom_delta());
            if (factor - 1.0).abs() > 1e-4 {
                let anchor = (cursor - rect.min - self.offset) / self.zoom;
                self.zoom = (self.zoom * factor).clamp(0.02, 50.0);
                self.offset = cursor - rect.min - anchor * self.zoom;
            }
        }
        let (offset, zoom) = (self.offset, self.zoom);
        let to_screen = |p: Pos2| rect.min + offset + p.to_vec2() * zoom;
        let to_plan = |s: Pos2| ((s - rect.min - offset) / zoom).to_pos2();

        self.no_snap = ui.input(|i| i.modifiers.shift);
        self.ensure_models();
        if !ui.ctx().egui_wants_keyboard_input() && !ui.input(|i| i.modifiers.command || i.modifiers.alt) {
            let keys = [
                (egui::Key::S, Tool::Select),
                (egui::Key::O, Tool::Obstacles),
                (egui::Key::E, Tool::Client),
                (egui::Key::W, Tool::Walls),
                (egui::Key::D, Tool::Aps),
                (egui::Key::P, Tool::Survey),
                (egui::Key::C, Tool::Scale),
            ];
            for (key, tool) in keys {
                if ui.input(|i| i.key_pressed(key)) {
                    self.set_tool(tool);
                    if tool == Tool::Aps && self.mode == WorkMode::Plan {
                        self.devices_open = true;
                    }
                }
            }
        }
        if finger {
            self.box_start = None;
            self.ap_box_start = None;
            self.zone_stroke.clear();
        } else if matches!(self.tool, Tool::Walls | Tool::Select) {
            let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if self.tool == Tool::Walls {
                self.object_drag(&resp, ui, zoom, &to_plan, &to_screen, true, false, false);
                if resp.secondary_clicked() || escape {
                    self.wall_start = None;
                } else if resp.clicked()
                    && let Some(plan) = resp.interact_pointer_pos().map(to_plan).filter(|p| p.x >= 0.0 && p.y >= 0.0 && p.x <= img_w && p.y <= img_h)
                {
                    self.wall_click(plan, zoom);
                }
            } else {
                let (walls_ok, areas_ok, devices_ok) = self.select_kinds();
                self.object_drag(&resp, ui, zoom, &to_plan, &to_screen, walls_ok, areas_ok, devices_ok);
                self.select_walls(&resp, ui, zoom, &to_plan, &to_screen);
                if devices_ok {
                    self.ap_input(&resp, ui, zoom, &to_plan, &to_screen, (img_w, img_h));
                }
                if escape {
                    self.selected_walls.clear();
                    self.selected_areas.clear();
                }
                if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::A))
                    && let Some(p) = &self.project
                {
                    let (walls_ok, areas_ok, _) = self.select_kinds();
                    self.selected_walls = if walls_ok { (0..p.floor().walls.len()).collect() } else { Default::default() };
                    self.selected_areas = if areas_ok { (0..p.floor().areas.len()).collect() } else { Default::default() };
                }
            }
            if ui.input(|i| i.key_pressed(egui::Key::Delete)) {
                self.delete_selected_walls();
            }
            if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Z)) {
                self.undo_walls();
            }
        } else if self.tool == Tool::Obstacles {
            self.object_drag(&resp, ui, zoom, &to_plan, &to_screen, false, true, false);
            self.area_input(&resp, ui, zoom, &to_plan, (img_w, img_h));
        } else if self.tool == Tool::Zone {
            self.zone_input(&resp, &to_plan, (img_w, img_h));
        } else if self.tool == Tool::Client {
            self.client_input(&resp, &to_plan, (img_w, img_h));
        } else if self.tool == Tool::Aps {
            self.ap_input(&resp, ui, zoom, &to_plan, &to_screen, (img_w, img_h));
        } else if resp.clicked()
            && let Some(click) = resp.interact_pointer_pos()
        {
            self.handle_click(click, to_plan(click), &to_screen, env);
        }

        if self.mode == WorkMode::Survey && self.show_heat && self.heat_dirty {
            self.rebuild_heat(ui.ctx());
        }
        if self.zone_dirty {
            self.rebuild_zone_texture(ui.ctx());
        }
        if self.mode == WorkMode::Plan && self.show_pred && self.pred_dirty && !self.ap_drag.is_some() {
            self.rebuild_prediction(ui.ctx());
        }

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        let plan_rect = Rect::from_min_max(to_screen(Pos2::ZERO), to_screen(Pos2::new(img_w, img_h)));
        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        if self.view.show_plan {
            painter.image(tex.id(), plan_rect, uv, Color32::WHITE);
        } else {
            painter.rect_filled(plan_rect, 0.0, ui.visuals().faint_bg_color);
        }
        if let (Some((o, otex)), Some(p)) = (&overlay_tex, self.project.as_ref()) {
            let t = p.overlay_transform(*o, p.active);
            let fl = &p.floors[*o];
            let corners = [[0.0, 0.0], [fl.width_px as f32, 0.0], [fl.width_px as f32, fl.height_px as f32], [0.0, fl.height_px as f32]].map(|c| {
                let q = t.apply(c);
                to_screen(Pos2::new(q[0], q[1]))
            });
            let tint = Color32::from_rgba_unmultiplied(255, 190, 140, (self.overlay_opacity * 255.0) as u8);
            let mut mesh = egui::Mesh::with_texture(otex.id());
            for (pos, uv) in corners.iter().zip([Pos2::new(0.0, 0.0), Pos2::new(1.0, 0.0), Pos2::new(1.0, 1.0), Pos2::new(0.0, 1.0)]) {
                mesh.vertices.push(egui::epaint::Vertex { pos: *pos, uv, color: tint });
            }
            mesh.indices.extend([0, 1, 2, 0, 2, 3]);
            painter.add(egui::Shape::mesh(mesh));
            painter.add(egui::Shape::closed_line(corners.to_vec(), Stroke::new(1.5, Color32::from_rgb(240, 140, 50).gamma_multiply(0.8))));
            for w in &fl.walls {
                let ([r, g, b], width) = w.material.style();
                let (a, c) = (t.apply(w.a), t.apply(w.b));
                painter.line_segment(
                    [to_screen(Pos2::new(a[0], a[1])), to_screen(Pos2::new(c[0], c[1]))],
                    Stroke::new(width * 0.8, Color32::from_rgb(r, g, b).gamma_multiply(self.overlay_opacity.max(0.4))),
                );
            }
            for sp in &fl.points {
                let q = t.apply([sp.x, sp.y]);
                painter.circle_stroke(to_screen(Pos2::new(q[0], q[1])), 5.0, Stroke::new(1.5, Color32::from_rgb(240, 140, 50)));
            }
        }
        if self.mode == WorkMode::Plan
            && self.show_pred
            && let Some(pred) = &self.pred_texture
        {
            painter.image(pred.id(), plan_rect, uv, Color32::from_white_alpha((self.opacity * 255.0) as u8));
        }
        if self.mode == WorkMode::Survey
            && self.show_heat
            && let Some(heat) = &self.heat_texture
        {
            painter.image(heat.id(), plan_rect, uv, Color32::from_white_alpha((self.opacity * 255.0) as u8));
        }

        if self.mode == WorkMode::Plan
            && self.tool == Tool::Zone
            && let Some(tex) = &self.zone_tex
            && let Some(f) = self.project.as_ref().map(|p| p.floor())
        {
            let cell = (f.width_px.max(f.height_px) as f32 / 400.0).max(1.0);
            let (w, h) = ((f.width_px as f32 / cell).ceil() * cell, (f.height_px as f32 / cell).ceil() * cell);
            let area = Rect::from_min_max(to_screen(Pos2::ZERO), to_screen(Pos2::new(w, h)));
            painter.image(tex.id(), area, uv, Color32::WHITE);
        }
        let floor = self.project.as_ref().expect("checked above").floor();
        let ceiling = floor.ceiling_m;
        for (i, area) in floor.areas.iter().enumerate() {
            let [cr, cg, cb] = area.kind.colour();
            let pts: Vec<Pos2> = area.points.iter().map(|p| to_screen(Pos2::new(p[0], p[1]))).collect();
            let selected = self.selected_areas.contains(&i);
            let outline = Stroke::new(if selected { 3.0 } else { 1.5 }, if selected { Color32::YELLOW } else { Color32::from_rgb(cr, cg, cb) });
            if is_convex(&pts) {
                painter.add(egui::Shape::convex_polygon(pts.clone(), Color32::from_rgba_unmultiplied(cr, cg, cb, 70), outline));
            } else {
                painter.add(egui::Shape::closed_line(pts.clone(), outline));
            }
            let centre = pts.iter().fold(Vec2::ZERO, |s, p| s + p.to_vec2()) / pts.len().max(1) as f32;
            painter.text(centre.to_pos2(), egui::Align2::CENTER_CENTER, area.kind.name(), egui::FontId::proportional(12.0), Color32::BLACK);
        }
        for (i, w) in floor.walls.iter().enumerate() {
            let ([r, g, b], width) = w.material.style();
            let (a, c) = (to_screen(Pos2::new(w.a[0], w.a[1])), to_screen(Pos2::new(w.b[0], w.b[1])));
            if self.selected_walls.contains(&i) {
                painter.line_segment([a, c], Stroke::new(width + 6.0, Color32::from_rgba_unmultiplied(255, 220, 0, 140)));
            }
            // Partial-height walls are dashed so that they read as "low".
            let colour = Color32::from_rgb(r, g, b);
            if w.effective_height_m(ceiling) < ceiling - 0.01 {
                painter.extend(egui::Shape::dashed_line(&[a, c], Stroke::new(width, colour), 9.0, 5.0));
            } else {
                painter.line_segment([a, c], Stroke::new(width, colour));
            }
        }
        if self.tool == Tool::Walls
            && let (Some(start), Some(hover)) = (self.wall_start, ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p)))
        {
            let end = self.snapped(to_plan(hover), zoom);
            let ([r, g, b], width) = self.wall_material.style();
            let (a, c) = (to_screen(start), to_screen(end));
            let ghost = Stroke::new(width, Color32::from_rgb(r, g, b).gamma_multiply(0.7));
            match self.wall_mode {
                WallMode::Wall => {
                    painter.line_segment([a, c], ghost);
                }
                WallMode::Room => {
                    painter.rect_stroke(Rect::from_two_pos(a, c), 0.0, ghost, egui::StrokeKind::Middle);
                }
            }
            painter.circle_stroke(c, 6.0, Stroke::new(2.0, Color32::from_rgb(240, 140, 50)));
            let len_px = start.distance(end);
            let label = match floor.metres_per_px {
                Some(m) => format!("{:.2} m", len_px * m),
                None => format!("{len_px:.0} px"),
            };
            painter.text((a + c.to_vec2()) / 2.0 + Vec2::new(8.0, -8.0), egui::Align2::LEFT_BOTTOM, label, egui::FontId::proportional(14.0), Color32::from_rgb(240, 140, 50));
            ui.ctx().request_repaint();
        }
        if self.tool == Tool::Select
            && let (Some(a), Some(b)) = (self.box_start, ui.input(|i| i.pointer.hover_pos()))
            && resp.dragged_by(egui::PointerButton::Primary)
        {
            let boxed = Rect::from_two_pos(to_screen(a), b);
            let accent = theme::roles(ui.ctx()).primary;
            painter.rect(boxed, 2.0, accent.gamma_multiply(0.15), Stroke::new(1.5, accent), egui::StrokeKind::Inside);
        }
        if self.tool == Tool::Zone {
            let colour = if self.zone_erase { Color32::from_rgb(220, 60, 60) } else { Color32::from_rgb(0, 150, 140) };
            let radius = self.zone_radius_px() * zoom;
            if self.zone_stroke.len() > 1 {
                let line: Vec<Pos2> = self.zone_stroke.iter().map(|p| to_screen(Pos2::new(p[0], p[1]))).collect();
                painter.add(egui::Shape::line(line, Stroke::new(radius * 2.0, colour.gamma_multiply(0.35))));
            }
            if let Some(hover) = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p)) {
                painter.circle(hover, radius, colour.gamma_multiply(0.2), Stroke::new(1.5, colour));
            }
        }
        if self.tool == Tool::Obstacles
            && let Some(hover) = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p))
        {
            let [cr, cg, cb] = self.obstacle_kind.colour();
            let stroke = Stroke::new(2.0, Color32::from_rgb(cr, cg, cb));
            match (self.area_mode, self.area_start) {
                (AreaMode::Rectangle, Some(a)) => {
                    painter.rect(Rect::from_two_pos(to_screen(a), hover), 0.0, Color32::from_rgba_unmultiplied(cr, cg, cb, 50), stroke, egui::StrokeKind::Middle);
                }
                (AreaMode::Polygon, _) if !self.poly_points.is_empty() => {
                    let mut line: Vec<Pos2> = self.poly_points.iter().map(|p| to_screen(*p)).collect();
                    line.push(hover);
                    painter.add(egui::Shape::line(line, stroke));
                    painter.circle_stroke(to_screen(self.poly_points[0]), 6.0, Stroke::new(2.0, Color32::from_rgb(240, 140, 50)));
                }
                _ => {}
            }
            ui.ctx().request_repaint();
        }
        if let Some(pos) = self.client.filter(|_| self.tool == Tool::Client) {
            let c = to_screen(pos);
            let best = self.client_results(floor, pos).into_iter().filter_map(|(_, b, hit)| hit.map(|(ap, dbm)| (ap, b, dbm))).max_by(|a, b| a.2.total_cmp(&b.2));
            let colour = match best {
                Some((ap, _, dbm)) if dbm >= NO_COVERAGE_DBM => {
                    if let Some(a) = floor.layer(self.mode == WorkMode::Plan).get(ap) {
                        painter.line_segment([c, to_screen(Pos2::new(a.x, a.y))], Stroke::new(2.0, rating_colour(dbm).gamma_multiply(0.8)));
                    }
                    rating_colour(dbm)
                }
                _ => Color32::from_rgb(220, 70, 60),
            };
            painter.circle(c, 14.0, colour, Stroke::new(2.5, Color32::WHITE));
            theme::paint_icon(&painter, ui.ctx(), ICON_SMARTPHONE, c, 16.0, Color32::WHITE);
        }
        if self.view.show_path && self.mode == WorkMode::Survey {
            let path: Vec<Pos2> = floor.points.iter().map(|p| to_screen(Pos2::new(p.x, p.y))).collect();
            painter.add(egui::Shape::line(path, Stroke::new(2.0, Color32::from_rgb(30, 90, 200).gamma_multiply(0.7))));
        }
        let radius = self.view.marker_size.radius();
        let radius = if self.mode == WorkMode::Plan { radius * 0.6 } else { radius };
        for (i, p) in floor.points.iter().enumerate() {
            let c = to_screen(Pos2::new(p.x, p.y));
            let selected = self.selected == Some(i);
            painter.circle(c, radius + if selected { 2.0 } else { 0.0 }, Color32::from_rgb(30, 90, 200), Stroke::new(2.0, if selected { Color32::YELLOW } else { Color32::WHITE }));
            if self.view.show_numbers {
                painter.text(c + Vec2::new(radius + 3.0, -radius - 3.0), egui::Align2::LEFT_BOTTOM, (i + 1).to_string(), egui::FontId::proportional(13.0), Color32::BLACK);
            }
        }
        if let Some(p) = self.pending {
            let c = to_screen(p);
            painter.circle_stroke(c, 12.0, Stroke::new(3.0, Color32::from_rgb(240, 140, 50)));
            painter.text(
                rect.center_top() + Vec2::new(0.0, 14.0),
                egui::Align2::CENTER_TOP,
                format!("Scanning for {} s... ({} scans so far). Stand still.", env.dwell_secs, env.scans_so_far),
                egui::FontId::proportional(18.0),
                Color32::from_rgb(240, 140, 50),
            );
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        }
        for (i, &p) in self.scale_clicks.iter().enumerate() {
            let c = to_screen(p);
            painter.circle(c, 5.0, Color32::from_rgb(220, 50, 50), Stroke::new(1.5, Color32::WHITE));
            if i == 1 {
                painter.line_segment([to_screen(self.scale_clicks[0]), c], Stroke::new(2.0, Color32::from_rgb(220, 50, 50)));
            }
        }
        // In the plan, where each installed access point really was is shown as a faint
        // ring, so a moved copy can be seen against the survey record.
        if self.mode == WorkMode::Plan {
            for a in &floor.survey_aps {
                let c = to_screen(Pos2::new(a.x, a.y));
                painter.circle_stroke(c, 13.0, Stroke::new(1.5, Color32::from_rgb(30, 90, 200).gamma_multiply(0.55)));
            }
        }
        for (i, a) in floor.layer(self.mode == WorkMode::Plan).iter().enumerate() {
            let c = to_screen(Pos2::new(a.x, a.y));
            let selected = self.aps_selected.contains(&i);
            let fill = match (a.enabled, a.existing) {
                (false, _) => Color32::GRAY,
                (true, true) => Color32::from_rgb(30, 90, 200),
                (true, false) => Color32::from_rgb(30, 150, 90),
            };
            painter.circle(c, 11.0 + if selected { 2.0 } else { 0.0 }, fill, Stroke::new(2.0, if selected { Color32::YELLOW } else { Color32::WHITE }));
            painter.text(c, egui::Align2::CENTER_CENTER, (i + 1).to_string(), egui::FontId::proportional(12.0), Color32::WHITE);
            painter.text(c + Vec2::new(15.0, -4.0), egui::Align2::LEFT_BOTTOM, format!("{}  {:.1} m", a.name, a.height_m), egui::FontId::proportional(12.0), Color32::BLACK);
        }
        if self.mode == WorkMode::Plan {
            if self.show_pred {
                if let Some(note) = &self.pred_note {
                    painter.text(rect.center_top() + Vec2::new(0.0, 14.0), egui::Align2::CENTER_TOP, note, egui::FontId::proportional(16.0), Color32::from_rgb(240, 140, 50));
                } else if self.pred_texture.is_some() {
                    draw_legend(&painter, rect, &self.view);
                }
            }
        }
        if self.mode == WorkMode::Survey && self.show_heat {
            draw_legend(&painter, rect, &self.view);
        }
        let (wall_edits, ap_edits) = self.right_panel(ui.ctx(), rect);
        for edit in wall_edits {
            self.apply_wall_edit(edit);
        }
        for edit in ap_edits {
            self.apply_ap_edit(edit);
        }
        self.autoscale_tick(ui.ctx());
        self.wallscan_tick(ui.ctx());
        self.layers_button(ui.ctx(), rect);
        self.select_button(ui.ctx(), rect);
        if self.mode == WorkMode::Plan {
            self.devices_button(ui.ctx(), rect);
            self.optimise_button(ui.ctx(), rect);
        }
        if self.floors_open {
            for edit in self.floors_bubble(ui.ctx(), rect) {
                self.apply_floor_edit(ui.ctx(), edit);
            }
        }
        if self.tool == Tool::Survey {
            self.point_bubble(ui.ctx(), rect);
        }
    }

    fn handle_click(&mut self, click: Pos2, plan: Pos2, to_screen: &impl Fn(Pos2) -> Pos2, env: &Env) {
        let Some(floor) = self.project.as_ref().map(|p| p.floor()) else { return };
        if plan.x < 0.0 || plan.y < 0.0 || plan.x > floor.width_px as f32 || plan.y > floor.height_px as f32 {
            return;
        }
        match self.tool {
            Tool::Walls | Tool::Select | Tool::Align | Tool::Aps | Tool::Obstacles | Tool::Zone | Tool::Client => {}
            Tool::Scale => {
                if self.scale_clicks.len() >= 2 {
                    self.scale_clicks.clear();
                }
                if let Some(first) = self.scale_clicks.first()
                    && first.distance(plan) < MIN_SCALE_PX
                {
                    self.status = "Click a second point further from the first one (at least the other end of a known distance).".into();
                    return;
                }
                self.scale_clicks.push(plan);
                if self.scale_clicks.len() == 2 {
                    self.scale_input.clear();
                }
            }
            Tool::Survey => {
                let near = floor
                    .points
                    .iter()
                    .position(|p| to_screen(Pos2::new(p.x, p.y)).distance(click) <= PICK_RADIUS);
                if near.is_some() {
                    self.selected = near;
                } else if self.pending.is_none() && self.mode == WorkMode::Survey {
                    self.pending = Some(plan);
                    let _ = env.tx.send(Cmd::Survey { x: plan.x, y: plan.y });
                }
            }
        }
    }

    fn scale_dialog(&mut self, ctx: &egui::Context) {
        if self.scale_clicks.len() != 2 {
            return;
        }
        let (a, b) = (self.scale_clicks[0], self.scale_clicks[1]);
        let (w, h) = self.project.as_ref().map_or((1, 1), |p| (p.floor().width_px, p.floor().height_px));
        let mut done = false;
        theme::dialog(ctx, "Set scale", 380.0, |ui| {
            ui.label(format!("The two points are {:.0} px apart. What is that distance in metres?", a.distance(b)));
            ui.add_space(8.0);
            let edit = theme::field(ui, egui::TextEdit::singleline(&mut self.scale_input).hint_text("Distance in metres").desired_width(f32::INFINITY));
            edit.request_focus();
            clean_distance(&mut self.scale_input);
            let metres = self.scale_input.trim().replace(',', ".").parse::<f32>().ok().filter(|m| m.is_finite() && *m > 0.0);
            // Typing nothing yet is not an error; a distance that cannot be right is.
            let check = match metres {
                Some(m) => Some(check_scale(a.distance(b), m, w, h)),
                None if a.distance(b) < MIN_SCALE_PX => Some(check_scale(a.distance(b), 1.0, w, h)),
                None => None,
            };
            if let Some(Err(why)) = &check {
                ui.add_space(6.0);
                ui.colored_label(theme::roles(ui.ctx()).error, why);
            }
            let scale = check.and_then(Result::ok);
            theme::actions(ui, |ui| {
                let set = ui.add_enabled_ui(scale.is_some(), |ui| theme::button(ui, ButtonKind::Filled, None, "Set scale")).inner;
                let ok = set.clicked() || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && scale.is_some());
                if ok && let (Some(mpp), Some(p)) = (scale, self.project.as_mut()) {
                    p.floor_mut().metres_per_px = Some(mpp);
                    self.status = "Scale set. Switch to \"Survey point\" and click where you are standing.".into();
                    self.tool = Tool::Survey;
                    self.heat_dirty = true;
                    done = true;
                }
                if theme::button(ui, ButtonKind::Text, None, "Cancel").clicked() {
                    done = true;
                }
            });
        });
        if done {
            self.scale_clicks.clear();
            self.autosave();
        }
    }

    fn rebuild_heat(&mut self, ctx: &egui::Context) {
        self.heat_dirty = false;
        let Some(floor) = self.project.as_ref().map(|p| p.floor()) else { return };
        let samples: Vec<Sample> = floor
            .points
            .iter()
            .map(|p| Sample { x: p.x, y: p.y, dbm: Floor::signal_at_min(p, &self.target, self.band, self.view.detect_dbm) })
            .collect();
        let positions: Vec<[f32; 2]> = floor.points.iter().map(|p| [p.x, p.y]).collect();
        let fade_px = heatmap::auto_fade_px(&positions, floor.metres_per_px, floor.width_px, floor.height_px);
        let grid = heatmap::interpolate(&samples, floor.width_px, floor.height_px, HEAT_CELLS, fade_px);
        let pixels = (0..grid.w * grid.h)
            .map(|i| {
                let v = &self.view;
                let [r, g, b] = heatmap::color_in(v.color_scheme, grid.dbm[i] as f64, v.min_dbm, v.max_dbm);
                let conf = grid.confidence[i];
                if !v.gray_unmeasured {
                    return Color32::from_rgba_unmultiplied(r, g, b, (conf * 255.0) as u8);
                }
                // Colour at `conf` opacity over a half-opaque grey wash.
                let (grey, grey_a) = (120.0, 0.5);
                let a = conf + grey_a * (1.0 - conf);
                let mix = |c: u8| ((c as f32 * conf + grey * grey_a * (1.0 - conf)) / a).round() as u8;
                Color32::from_rgba_unmultiplied(mix(r), mix(g), mix(b), (a * 255.0) as u8)
            })
            .collect();
        let image = egui::ColorImage { size: [grid.w, grid.h], source_size: Vec2::new(grid.w as f32, grid.h as f32), pixels };
        self.heat_texture = Some(ctx.load_texture("heatmap", image, TextureOptions::LINEAR));
    }
}

/// Whether a polygon's corners all turn the same way, so it can be filled in one go.
fn is_convex(pts: &[Pos2]) -> bool {
    let n = pts.len();
    if n < 3 {
        return false;
    }
    let mut sign = 0.0f32;
    for i in 0..n {
        let (a, b, c) = (pts[i], pts[(i + 1) % n], pts[(i + 2) % n]);
        let cross = (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x);
        if cross.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if sign != cross.signum() {
            return false;
        }
    }
    true
}

/// Where the user's own access point models are kept.
fn models_path() -> PathBuf {
    store::config_dir().join("ap-models.json")
}

fn decode_texture(ctx: &egui::Context, bytes: &[u8], name: &str) -> Option<TextureHandle> {
    let mut img = image::load_from_memory(bytes).ok()?;
    if img.width().max(img.height()) > MAX_TEXTURE_SIDE {
        img = img.resize(MAX_TEXTURE_SIDE, MAX_TEXTURE_SIDE, image::imageops::FilterType::Triangle);
    }
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some(ctx.load_texture(name, egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()), TextureOptions::LINEAR))
}

fn draw_legend(painter: &egui::Painter, rect: Rect, view: &Settings) {
    let (w, h, steps) = (220.0, 14.0, 44);
    let origin = rect.left_bottom() + Vec2::new(14.0, -38.0);
    for i in 0..steps {
        let t = i as f64 / (steps - 1) as f64;
        let [r, g, b] = heatmap::color_in(view.color_scheme, view.min_dbm + t * (view.max_dbm - view.min_dbm), view.min_dbm, view.max_dbm);
        let x = origin.x + w * i as f32 / steps as f32;
        painter.rect_filled(Rect::from_min_size(Pos2::new(x, origin.y), Vec2::new(w / steps as f32 + 1.0, h)), 0.0, Color32::from_rgb(r, g, b));
    }
    let font = egui::FontId::proportional(12.0);
    painter.text(origin + Vec2::new(0.0, h + 2.0), egui::Align2::LEFT_TOP, format!("{:.0} dBm", view.min_dbm), font.clone(), Color32::WHITE);
    painter.text(origin + Vec2::new(w, h + 2.0), egui::Align2::RIGHT_TOP, format!("{:.0}+", view.max_dbm), font, Color32::WHITE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_checks_reject_close_points_and_absurd_plan_sizes() {
        assert!(check_scale(1.0, 13.0, 2828, 4000).unwrap_err().contains("only 1 px apart"));
        let mpp = check_scale(190.0, 10.0, 2828, 4000).unwrap();
        assert!((mpp - 0.0526).abs() < 0.001);
        assert!(check_scale(100.0, 100_000.0, 2828, 4000).unwrap_err().contains("looks wrong"));
        assert!(check_scale(100.0, 0.001, 2828, 4000).unwrap_err().contains("looks wrong"));
    }
}

#[cfg(test)]
mod distance_tests {
    use super::clean_distance;

    #[test]
    fn typed_distances_keep_three_decimals_at_most() {
        let clean = |s: &str| {
            let mut s = s.to_string();
            clean_distance(&mut s);
            s
        };
        assert_eq!(clean("4,36"), "4,36");
        assert_eq!(clean("4,1"), "4,1");
        assert_eq!(clean("4,36666666666"), "4,366");
        assert_eq!(clean("12.5abc"), "12.5");
        assert_eq!(clean(",5"), "5");
        assert_eq!(clean("1,2,3"), "1,23");
    }
}

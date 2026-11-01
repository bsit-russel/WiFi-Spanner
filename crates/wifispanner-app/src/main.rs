// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! WiFi Spanner desktop app: a live network analyser and a floor plan survey.

mod autoscale;
mod wallscan;
mod floor;
mod gallery;
mod launcher;
mod pen;
mod live;
mod pdf;
mod report;
mod settings;
mod settings_ui;
mod store;
mod theme;
mod update;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::Duration;

use eframe::egui;
use settings::Settings;
use theme::ButtonKind;
use egui_material_icons::icons::{
    ICON_CLOSE, ICON_CONTENT_COPY, ICON_FOLDER, ICON_FOLDER_OPEN, ICON_HELP, ICON_IMAGE, ICON_INFO, ICON_POWER_SETTINGS_NEW, ICON_REFRESH, ICON_SETTINGS,
    ICON_SYSTEM_UPDATE, ICON_VIEW_LIST, ICON_WIFI,
};
use wifispanner_scan::dwell::scan_for_many;
use wifispanner_scan::iw::WifiCard;
use wifispanner_scan::{ScanResult, Scanner, default_scanner, list_cards};

const LOGO_PNG: &[u8] = include_bytes!("../../../assets/logo.png");
const BYTE_SIZE_PNG: &[u8] = include_bytes!("../../../assets/byte-size-it.png");
const BYTE_SIZE_NAME: &str = "Byte Size IT";
const BYTE_SIZE_URL: &str = "https://byte-size.co.za";
const BYTE_SIZE_SITE: &str = "byte-size.co.za";
const SUPPORT_EMAIL: &str = "bugsplat@byte-size.co.za";
/// Size of the splash card, drawn in the middle of the (see-through) window.
const SPLASH_SIZE: [f32; 2] = [560.0, 390.0];
/// Corner radius of the splash card, in points.
const SPLASH_RADIUS: f32 = 50.0;
/// Default size of the main window when none has been saved yet.
const MAIN_SIZE: [f32; 2] = [1200.0, 700.0];
/// The splash stays up at least this long so it can be read.
/// Wait before sizing the window after the splash, and between retries.
const RESIZE_DELAY: Duration = Duration::from_millis(500);
const SPLASH_MIN: Duration = Duration::from_millis(1800);
/// The project page, shown in About as the attribution link.
const REPO_URL: &str = "https://github.com/bsit-russel/WiFi-Spanner";
const TAGLINE: &str = "Your one-stop WiFi Scanner and Planner";

/// How long a snackbar message stays.
const TOAST_FOR: Duration = Duration::from_secs(5);

fn main() -> eframe::Result {
    // The window opens at its final size, so the desktop places it on screen
    // (an app cannot position itself on Wayland), and never has to grow later.
    // Until the scanner is ready it is borderless and see-through apart from
    // the splash card, and clicks pass through to whatever is behind it.
    let saved = Settings::load();
    let start = start_size(saved.window_size, saved.screen_size);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(start)
            .with_decorations(false)
            .with_transparent(true)
            .with_mouse_passthrough(true)
            .with_title("WiFi Spanner")
            .with_app_id("wifispanner")
            .with_icon(window_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "WiFi Spanner",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc.egui_ctx.clone())))),
    )
}

/// Requests to the scanning thread.
enum Cmd {
    /// Scan continuously for the live tab, or stop doing so.
    Live(bool),
    /// Scan with these interfaces from now on.
    SetCards(Vec<String>),
    /// One dwell scan for a survey point at floor plan position (x, y).
    Survey { x: f32, y: f32 },
}

enum Msg {
    Started(String),
    Progress(usize),
    Live(Box<ScanResult>),
    Point { x: f32, y: f32, dwell_secs: u64, result: Box<ScanResult> },
    Failed(String),
}

/// Handle to the scanning thread.
struct Worker {
    tx: Sender<Cmd>,
    dwell_secs: Arc<AtomicU64>,
}

impl Worker {
    fn spawn(ctx: egui::Context, dwell_secs: Arc<AtomicU64>, cards: Vec<String>) -> (Worker, Receiver<Msg>) {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let (tx, rx) = mpsc::channel();
        let dwell = dwell_secs.clone();
        std::thread::spawn(move || {
            let build = |names: &[String]| -> Vec<Box<dyn Scanner>> {
                let scanners: Vec<_> = names
                    .iter()
                    .filter_map(|n| match default_scanner(Some(n.clone())) {
                        Ok(s) => Some(s),
                        Err(e) => {
                            let _ = tx.send(Msg::Failed(format!("{n}: {e:#}")));
                            None
                        }
                    })
                    .collect();
                let name = scanners.iter().map(|s| s.name()).collect::<Vec<_>>().join(" and ");
                let _ = tx.send(Msg::Started(if name.is_empty() { "no Wi-Fi card".into() } else { name }));
                scanners
            };
            let mut scanners = build(&cards);
            let mut live = false;
            loop {
                let cmd = if live && !scanners.is_empty() {
                    match cmd_rx.try_recv() {
                        Ok(c) => Some(c),
                        Err(TryRecvError::Empty) => None,
                        Err(TryRecvError::Disconnected) => return,
                    }
                } else {
                    match cmd_rx.recv() {
                        Ok(c) => Some(c),
                        Err(_) => return,
                    }
                };
                let survey_at = match cmd {
                    Some(Cmd::Live(on)) => {
                        live = on;
                        continue;
                    }
                    Some(Cmd::SetCards(names)) => {
                        scanners = build(&names);
                        continue;
                    }
                    Some(Cmd::Survey { x, y }) => Some((x, y)),
                    None => None,
                };
                if scanners.is_empty() {
                    let _ = tx.send(Msg::Failed("no Wi-Fi card is available for scanning".into()));
                    ctx.request_repaint();
                    live = false;
                    continue;
                }
                let dwell_secs = dwell.load(Ordering::Relaxed);
                let progress = |n: usize| {
                    let _ = tx.send(Msg::Progress(n));
                    ctx.request_repaint();
                };
                let result = scan_for_many(&mut scanners, Duration::from_secs(dwell_secs), &progress);
                let msg = match (result, survey_at) {
                    (Ok(r), Some((x, y))) => Msg::Point { x, y, dwell_secs, result: Box::new(r) },
                    (Ok(r), None) => Msg::Live(Box::new(r)),
                    (Err(e), _) => Msg::Failed(format!("{e:#}")),
                };
                if tx.send(msg).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
        (Worker { tx: cmd_tx, dwell_secs }, rx)
    }
}

/// Where an in-place update stands. Nothing is downloaded until the user
/// confirms in the dialog.
enum InstallState {
    Idle,
    Confirm(update::UpdateInfo),
    Working { info: update::UpdateInfo, done: u64 },
    Failed { info: update::UpdateInfo, message: String },
}

enum InstallMsg {
    Progress(u64),
    Done(Result<(), String>),
}

/// Sizing the main window after the splash, then remembering what the user
/// resizes it to.
#[derive(Default)]
struct WindowSizing {
    /// When to (re)send the size request; `None` once it has taken effect.
    resize_at: Option<std::time::Instant>,
    tries: u8,
    target: egui::Vec2,
    /// Whether the size is being tracked for saving (after the resize settled).
    tracking: bool,
    last_size: egui::Vec2,
    changed_at: Option<std::time::Instant>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    /// Live network analysis.
    Scanning,
    /// Project gallery and the floor plan survey workspace.
    Surveying,
    /// The same projects, for placing access points and predicting coverage.
    Planning,
}

struct App {
    rx: Receiver<Msg>,
    worker: Worker,
    tab: Tab,
    scanner_name: String,
    scans_so_far: usize,
    error: Option<String>,
    latest: Option<ScanResult>,
    floor: floor::FloorTab,
    gallery: gallery::Gallery,
    /// On the Surveying tab: the project gallery (true) or a project's workspace.
    surveying_gallery: bool,
    settings: Settings,
    cards: Vec<WifiCard>,
    settings_open: bool,
    about_open: bool,
    help_open: bool,
    update_rx: Option<Receiver<(bool, Result<Option<update::UpdateInfo>, String>)>>,
    /// A newer release found, shown in a bar until dismissed.
    update_notice: Option<update::UpdateInfo>,
    /// Result of a manual check: message and an optional release link.
    update_message: Option<(String, Option<String>)>,
    projects_message: String,
    update_prefs: update::Prefs,
    /// The AppImage this process runs from, if it is one (enables in-place updates).
    appimage: Option<std::path::PathBuf>,
    install: InstallState,
    install_rx: Option<Receiver<InstallMsg>>,
    install_cancel: Arc<std::sync::atomic::AtomicBool>,
    /// Offer to add the desktop launcher (shown once, after the splash).
    launcher_prompt: bool,
    launcher_message: String,
    window: WindowSizing,
    /// Pen events from the Wayland tablet protocol, and where the pen was last.
    pen_rx: Option<std::sync::mpsc::Receiver<pen::PenEvent>>,
    pen_started: bool,
    /// The fingers on the touch screen now.
    touches: std::collections::HashSet<u64>,
    pen_pos: egui::Pos2,
    /// A short message shown as a snackbar, and when it appeared.
    toast: Option<(String, std::time::Instant)>,
    settings_section: settings_ui::Section,
    logo: Option<egui::TextureHandle>,
    byte_size_logo: Option<egui::TextureHandle>,
    /// Set while the splash is showing.
    splash_since: Option<std::time::Instant>,
    scanner_ready: bool,
    /// Width of the tab row, measured each frame to keep it centred.
    tabs_width: f32,
}

impl App {
    fn new(ctx: egui::Context) -> Self {
        let ctx_for_logo = ctx.clone();
        let settings = Settings::load();
        store::set_projects_dir(settings.projects_dir.clone());
        theme::install(&ctx, settings.theme_mode, settings.density, settings.design);
        let cards = list_cards().unwrap_or_default();
        let dwell = Arc::new(AtomicU64::new(settings.dwell_secs));
        let (worker, rx) = Worker::spawn(ctx, dwell, settings.active_cards(&cards));
        let _ = worker.tx.send(Cmd::Live(true));
        App {
            rx,
            worker,
            tab: Tab::Scanning,
            scanner_name: "starting...".into(),
            scans_so_far: 0,
            error: None,
            latest: None,
            floor: floor::FloorTab::default(),
            gallery: gallery::Gallery::default(),
            surveying_gallery: true,
            settings,
            cards,
            settings_open: false,
            about_open: false,
            help_open: false,
            update_rx: None,
            update_notice: None,
            update_message: None,
            projects_message: String::new(),
            update_prefs: update::Prefs::load(),
            appimage: update::running_appimage(),
            install: InstallState::Idle,
            install_rx: None,
            install_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            launcher_prompt: false,
            launcher_message: String::new(),
            window: WindowSizing::default(),
            pen_rx: None,
            pen_started: false,
            touches: std::collections::HashSet::new(),
            pen_pos: egui::Pos2::ZERO,
            toast: None,
            settings_section: settings_ui::Section::default(),
            byte_size_logo: byte_size_texture(&ctx_for_logo),
            logo: logo_texture(&ctx_for_logo),
            splash_since: Some(std::time::Instant::now()),
            scanner_ready: false,
            tabs_width: 0.0,
        }
    }

    fn drain(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Started(name) => {
                    self.scanner_name = name;
                    self.scanner_ready = true;
                }
                Msg::Progress(n) => self.scans_so_far = n,
                Msg::Live(r) => {
                    self.scans_so_far = 0;
                    self.latest = Some(*r);
                    self.error = None;
                }
                Msg::Point { x, y, dwell_secs, result } => {
                    self.scans_so_far = 0;
                    self.error = None;
                    self.floor.add_point(x, y, dwell_secs, &result);
                }
                Msg::Failed(e) => {
                    self.scanner_ready = true;
                    self.scans_so_far = 0;
                    self.floor.scan_failed();
                    self.error = Some(e);
                }
            }
        }
    }

    fn main_menu(&mut self, ui: &mut egui::Ui, action: &mut Option<MenuAction>) {
        ui.set_min_width(230.0);
        ui.spacing_mut().interact_size.y = 34.0;
        ui.spacing_mut().button_padding = egui::vec2(12.0, 6.0);
        theme::submenu(ui, Some(ICON_FOLDER), "File", |ui| {
            if ui.add(theme::menu_entry(ui, Some(ICON_IMAGE), "Open floor plan...")).clicked() {
                *action = Some(MenuAction::OpenPlan);
            }
            if ui.add(theme::menu_entry(ui, Some(ICON_FOLDER_OPEN), "Open project...")).clicked() {
                *action = Some(MenuAction::OpenProject);
            }
            ui.separator();
            if ui.add(theme::menu_entry(ui, Some(ICON_FOLDER), "Show projects folder")).clicked() {
                *action = Some(MenuAction::ShowProjectsFolder);
            }
        });
        if ui.add(theme::menu_entry(ui, Some(ICON_SETTINGS), "Settings...")).clicked() {
            self.settings_open = true;
        }
        theme::submenu(ui, Some(ICON_WIFI), "Adapters", |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            let before = self.settings.card.clone();
            if ui.add(theme::check_entry(ui, before.is_none(), "Auto (connected card)")).clicked() {
                self.settings.card = None;
            }
            for c in &self.cards {
                let selected = before.as_ref() == Some(&c.name);
                if ui.add(theme::check_entry(ui, selected, &c.label())).clicked() {
                    self.settings.card = Some(c.name.clone());
                }
            }
            if self.cards.is_empty() {
                ui.label("No Wi-Fi cards found");
            }
            ui.separator();
            let refresh = ui.add(theme::menu_entry(ui, Some(ICON_REFRESH), "Refresh cards")).clicked();
            if refresh {
                self.cards = list_cards().unwrap_or_default();
            }
            if refresh || before != self.settings.card {
                self.apply_settings();
            }
        });
        theme::submenu(ui, Some(ICON_VIEW_LIST), "Density", |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            let before = self.settings.density;
            for d in theme::Density::ALL {
                if ui.add(theme::check_entry(ui, before == d, d.name())).clicked() {
                    self.settings.density = d;
                }
            }
            if before != self.settings.density {
                theme::set_density(ui.ctx(), self.settings.density);
                self.settings.save();
            }
        });
        ui.separator();
        if ui.add(theme::menu_entry(ui, Some(ICON_SYSTEM_UPDATE), "Check for updates")).clicked() {
            *action = Some(MenuAction::CheckUpdates);
        }
        if ui.add(theme::menu_entry(ui, Some(ICON_HELP), "Report a bug...")).clicked() {
            self.help_open = true;
        }
        if ui.add(theme::menu_entry(ui, Some(ICON_INFO), "About...")).clicked() {
            self.about_open = true;
        }
        ui.separator();
        if ui.add(theme::menu_entry(ui, Some(ICON_POWER_SETTINGS_NEW), "Quit")).clicked() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Check GitHub on a background thread. A manual check reports every
    /// outcome; the automatic one only speaks up when there is a newer release.
    fn start_update_check(&mut self, ctx: &egui::Context, manual: bool) {
        if self.update_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send((manual, update::check(env!("CARGO_PKG_VERSION"))));
            repaint.request_repaint();
        });
        self.update_rx = Some(rx);
    }

    fn drain_update(&mut self) {
        let Some(rx) = &self.update_rx else { return };
        let Ok((manual, result)) = rx.try_recv() else { return };
        self.update_rx = None;
        match result {
            Ok(Some(info)) => {
                if manual {
                    let text = format!("WiFi Spanner {} is available. You have {}.", info.tag, env!("CARGO_PKG_VERSION"));
                    self.update_message = Some((text, Some(info.url.clone())));
                    self.update_notice = Some(info);
                } else if self.update_prefs.should_announce(&info.tag, update::unix_now()) {
                    // The automatic check respects "Dismiss" and "Remind me tomorrow".
                    self.update_notice = Some(info);
                }
            }
            Ok(None) if manual => {
                self.update_message = Some((format!("You have the latest version ({}).", env!("CARGO_PKG_VERSION")), None));
            }
            Err(e) if manual => {
                self.update_message = Some((format!("Could not check for updates: {e}"), None));
            }
            _ => {}
        }
    }

    fn update_notice_bar(&mut self, ui: &mut egui::Ui) {
        enum Choice {
            Update,
            Tomorrow,
            Dismiss,
        }
        let Some(info) = self.update_notice.clone() else { return };
        let can_install = info.asset.is_some() && self.appimage.is_some();
        let mut choice = None;
        let roles = theme::roles(ui.ctx());
        let banner = egui::Frame::new().fill(roles.secondary_container).inner_margin(egui::Margin::symmetric(16, 6));
        egui::Panel::top("update_notice").frame(banner).show(ui, |ui| {
            ui.visuals_mut().override_text_color = Some(roles.on_secondary_container);
            ui.horizontal(|ui| {
                ui.label(format!("WiFi Spanner {} is available (you have {}).", info.tag, env!("CARGO_PKG_VERSION")));
                ui.hyperlink_to(if can_install { "Release notes" } else { "View the release and download" }, &info.url);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let dismiss = theme::button(ui, ButtonKind::Text, None, "Dismiss");
                    if dismiss.on_hover_text("Skip this version. You will be told about newer ones.").clicked() {
                        choice = Some(Choice::Dismiss);
                    }
                    let tomorrow = theme::button(ui, ButtonKind::Text, None, "Remind me tomorrow");
                    if tomorrow.on_hover_text("Hide this for a day, then ask again.").clicked() {
                        choice = Some(Choice::Tomorrow);
                    }
                    if can_install && theme::button(ui, ButtonKind::Filled, Some(ICON_SYSTEM_UPDATE), "Update now").clicked() {
                        choice = Some(Choice::Update);
                    }
                });
            });
        });
        match choice {
            Some(Choice::Update) => self.install = InstallState::Confirm(info),
            Some(Choice::Tomorrow) => {
                self.update_prefs.snooze(&info.tag, update::unix_now());
                self.update_prefs.save();
                self.update_notice = None;
            }
            Some(Choice::Dismiss) => {
                self.update_prefs.skip(&info.tag);
                self.update_prefs.save();
                self.update_notice = None;
            }
            None => {}
        }
    }

    /// Start the download once the user has confirmed.
    fn start_install(&mut self, ctx: &egui::Context, info: update::UpdateInfo) {
        let (Some(asset), Some(target)) = (info.asset.clone(), self.appimage.clone()) else { return };
        let (tx, rx) = mpsc::channel();
        self.install_cancel.store(false, Ordering::Relaxed);
        let (cancel, repaint) = (self.install_cancel.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = update::install(&asset, &target, &cancel, |n| {
                let _ = tx.send(InstallMsg::Progress(n));
                repaint.request_repaint();
            });
            let _ = tx.send(InstallMsg::Done(result));
            repaint.request_repaint();
        });
        self.install_rx = Some(rx);
        self.install = InstallState::Working { info, done: 0 };
    }

    fn drain_install(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.install_rx else { return };
        let mut finished = None;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                InstallMsg::Progress(n) => {
                    if let InstallState::Working { done, .. } = &mut self.install {
                        *done = n;
                    }
                }
                InstallMsg::Done(result) => finished = Some(result),
            }
        }
        let Some(result) = finished else { return };
        self.install_rx = None;
        let InstallState::Working { info, .. } = std::mem::replace(&mut self.install, InstallState::Idle) else { return };
        let outcome = result.and_then(|()| match &self.appimage {
            Some(target) => update::restart(target),
            None => Err("not running as an AppImage".into()),
        });
        match outcome {
            Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(e) if e == "cancelled" => {}
            Err(message) => self.install = InstallState::Failed { info, message },
        }
    }

    fn install_window(&mut self, ctx: &egui::Context) {
        enum Act {
            Install(update::UpdateInfo),
            Cancel,
            Close,
        }
        let mut act = None;
        let appimage = self.appimage.clone();
        let (title, body): (&str, Box<dyn FnOnce(&mut egui::Ui, &mut Option<Act>)>) = match &self.install {
            InstallState::Idle => return,
            InstallState::Confirm(info) => {
                let info = info.clone();
                let size = info.asset.as_ref().map_or(0, |a| a.size) as f64 / 1_048_576.0;
                let path = appimage.map_or(String::new(), |p| p.display().to_string());
                ("Install update?", Box::new(move |ui, act| {
                    ui.label(format!("Install WiFi Spanner {} and restart?", info.tag));
                    if !info.notes.is_empty() {
                        ui.add_space(12.0);
                        ui.label(theme::medium("What's new"));
                        ui.add_space(4.0);
                        // Ten lines show at once; a longer list scrolls.
                        let line = ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().item_spacing.y;
                        egui::ScrollArea::vertical().max_height(line * 10.0).auto_shrink([false, true]).show(ui, |ui| {
                            for note in &info.notes {
                                ui.horizontal_top(|ui| {
                                    ui.label("\u{2022}");
                                    ui.add(egui::Label::new(note).wrap());
                                });
                            }
                        });
                    }
                    ui.add_space(8.0);
                    ui.weak(format!(
                        "This downloads {size:.1} MB from GitHub, checks it against the checksum GitHub publishes for the \
                         file, then replaces {path} and restarts the app. If anything doesn't check out, nothing is \
                         replaced. Your projects and settings are not touched."
                    ));
                    theme::actions(ui, |ui| {
                        if theme::button(ui, ButtonKind::Filled, Some(ICON_SYSTEM_UPDATE), "Install and restart").clicked() {
                            *act = Some(Act::Install(info.clone()));
                        }
                        if theme::button(ui, ButtonKind::Text, None, "Cancel").clicked() {
                            *act = Some(Act::Close);
                        }
                    });
                }))
            }
            InstallState::Working { info, done } => {
                let total = info.asset.as_ref().map_or(1, |a| a.size.max(1));
                let (frac, done) = (*done as f32 / total as f32, *done);
                let tag = info.tag.clone();
                ("Updating", Box::new(move |ui, act| {
                    ui.label(format!("Downloading WiFi Spanner {tag}..."));
                    ui.add_space(8.0);
                    ui.scope(|ui| {
                        ui.visuals_mut().selection.bg_fill = theme::roles(ui.ctx()).primary;
                        ui.add(egui::ProgressBar::new(frac).text(format!("{:.1} of {:.1} MB", done as f64 / 1_048_576.0, total as f64 / 1_048_576.0)));
                    });
                    theme::actions(ui, |ui| {
                        if theme::button(ui, ButtonKind::Text, None, "Cancel").clicked() {
                            *act = Some(Act::Cancel);
                        }
                    });
                }))
            }
            InstallState::Failed { info, message } => {
                let (url, message) = (info.url.clone(), message.clone());
                ("Update failed", Box::new(move |ui, act| {
                    ui.colored_label(theme::roles(ui.ctx()).error, format!("The update was not installed: {message}"));
                    ui.label("Your current version is unchanged.");
                    ui.hyperlink_to("Download it from the release page instead", url);
                    theme::actions(ui, |ui| {
                        if theme::button(ui, ButtonKind::Filled, None, "Close").clicked() {
                            *act = Some(Act::Close);
                        }
                    });
                }))
            }
        };
        theme::dialog(ctx, title, if matches!(self.install, InstallState::Confirm(_)) { 600.0 } else { 440.0 }, |ui| body(ui, &mut act));
        match act {
            Some(Act::Install(info)) => self.start_install(ctx, info),
            Some(Act::Cancel) => self.install_cancel.store(true, Ordering::Relaxed),
            Some(Act::Close) => self.install = InstallState::Idle,
            None => {}
        }
    }

    fn update_message_window(&mut self, ctx: &egui::Context) {
        let Some((text, url)) = self.update_message.clone() else { return };
        let mut close = false;
        theme::dialog(ctx, "Check for updates", 400.0, |ui| {
            ui.label(&text);
            if let Some(url) = &url {
                ui.hyperlink_to("View the release and download", url);
            }
            theme::actions(ui, |ui| {
                if theme::button(ui, ButtonKind::Filled, None, "OK").clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.update_message = None;
        }
    }

    /// Show messages from the tabs as a snackbar for a few seconds.
    fn toast_tick(&mut self, ctx: &egui::Context) {
        let new = match self.tab {
            Tab::Surveying | Tab::Planning => self.floor.take_toast().or_else(|| self.gallery.take_toast()),
            Tab::Scanning => None,
        };
        if let Some(text) = new {
            self.toast = Some((text, std::time::Instant::now()));
        }
        let Some((text, at)) = &self.toast else { return };
        let age = at.elapsed().as_secs_f32();
        let carbon = theme::skin(ctx) == theme::Skin::Carbon;
        let life = TOAST_FOR.as_secs_f32() + if carbon { theme::TOAST_SLIDE } else { 0.0 };
        if age >= life {
            self.toast = None;
            return;
        }
        let sliding = age < theme::TOAST_SLIDE || age > TOAST_FOR.as_secs_f32();
        ctx.request_repaint_after(Duration::from_millis(if sliding { 16 } else { 250 }));
        if theme::snackbar(ctx, text, age, TOAST_FOR.as_secs_f32()) {
            // Dismissed: Carbon slides out first, Material just goes.
            let leave = TOAST_FOR.as_secs_f32();
            if carbon && age < leave {
                self.toast = self.toast.take().map(|(t, _)| (t, std::time::Instant::now() - Duration::from_secs_f32(leave)));
            } else if !carbon {
                self.toast = None;
            }
        }
    }

    /// The size to open at: the last one used, kept within the screen, else
    /// the default.
    fn target_window_size(&self, ctx: &egui::Context) -> egui::Vec2 {
        let screen = ctx.input(|i| i.viewport().monitor_size).unwrap_or(egui::vec2(8000.0, 8000.0));
        restored_size(Some(start_size(self.settings.window_size, self.settings.screen_size)), screen)
    }

    /// Resize the window after the splash (retrying until it sticks), then
    /// save its size a moment after the user stops resizing it.
    fn window_size_tick(&mut self, ctx: &egui::Context) {
        let now = std::time::Instant::now();
        let size = ctx.content_rect().size();
        if let Some(at) = self.window.resize_at {
            if now < at {
                ctx.request_repaint_after(at - now);
                return;
            }
            if self.window.tries > 0 && (size - self.window.target).abs().max_elem() < 4.0 {
                self.window.resize_at = None;
                self.window.tracking = true;
                self.window.last_size = size;
                // It opens maximised; the size set above is what it comes back to.
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
                return;
            }
            if self.window.tries >= 4 {
                // The desktop would not resize it: leave it where it is.
                self.window.resize_at = None;
                self.window.tracking = true;
                self.window.last_size = size;
                // It opens maximised; the size set above is what it comes back to.
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
                return;
            }
            self.window.target = self.target_window_size(ctx);
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(self.window.target));
            if self.window.tries == 0 {
                if let Some(center) = egui::ViewportCommand::center_on_screen(ctx) {
                    ctx.send_viewport_cmd(center);
                }
            }
            self.window.tries += 1;
            self.window.resize_at = Some(now + RESIZE_DELAY);
            ctx.request_repaint_after(RESIZE_DELAY);
            return;
        }
        if !self.window.tracking {
            return;
        }
        // A maximised or full-screen size is not the size to come back to.
        let (maximized, fullscreen) = ctx.input(|i| (i.viewport().maximized.unwrap_or(false), i.viewport().fullscreen.unwrap_or(false)));
        if maximized || fullscreen {
            self.window.changed_at = None;
            return;
        }
        if (size - self.window.last_size).length() > 0.5 {
            self.window.last_size = size;
            self.window.changed_at = Some(now);
        }
        if let Some(t) = self.window.changed_at {
            if now.duration_since(t) >= Duration::from_secs(1) {
                self.window.changed_at = None;
                let new = [size.x.round(), size.y.round()];
                let screen = ctx.input(|i| i.viewport().monitor_size).map(|s| [s.x.round(), s.y.round()]);
                if self.settings.window_size != Some(new) || self.settings.screen_size != screen {
                    self.settings.window_size = Some(new);
                    self.settings.screen_size = screen;
                    self.settings.save();
                }
            } else {
                ctx.request_repaint_after(Duration::from_secs(1));
            }
        }
    }

    /// Add or remove the desktop launcher and icon.
    fn set_launcher(&mut self, add: bool) {
        let result = match (&self.appimage, add) {
            (Some(app), true) => launcher::install(app),
            (_, false) => launcher::remove(),
            (None, true) => Err("not running as an AppImage".into()),
        };
        self.launcher_message = match (result, add) {
            (Ok(()), true) => "Added. Restart WiFi Spanner to see the icon in the title bar.".into(),
            (Ok(()), false) => "Removed.".into(),
            (Err(e), _) => format!("Could not change the launcher: {e}"),
        };
    }

    /// The one-time offer to add the launcher. Either answer is remembered.
    fn launcher_prompt_window(&mut self, ctx: &egui::Context) {
        if !self.launcher_prompt {
            return;
        }
        let mut answer = None;
        theme::dialog(ctx, "Add WiFi Spanner to your application menu?", 440.0, |ui| {
            ui.label(
                "This adds a launcher and the WiFi Spanner icon to your home folder (a .desktop file under \
                 ~/.local/share/applications and an icon under ~/.local/share/icons), so the app shows its icon in \
                 the taskbar and title bar and can be started from the menu.",
            );
            ui.add_space(6.0);
            ui.weak("You can remove it any time in Settings > General.");
            theme::actions(ui, |ui| {
                if theme::button(ui, ButtonKind::Filled, None, "Add launcher").clicked() {
                    answer = Some(true);
                }
                if theme::button(ui, ButtonKind::Text, None, "Not now").clicked() {
                    answer = Some(false);
                }
            });
        });
        if let Some(add) = answer {
            if add {
                self.set_launcher(true);
            }
            self.launcher_prompt = false;
            self.settings.launcher_prompted = true;
            self.settings.save();
        }
    }

    /// Where to report a bug, ask for a feature or get help, with the details worth including.
    fn help_window(&mut self, ctx: &egui::Context) {
        if !self.help_open {
            return;
        }
        let mut close = false;
        theme::dialog(ctx, "Report a bug", 420.0, |ui| {
            ui.label("Found a bug? Email it to us:");
            ui.add_space(4.0);
            let subject = format!("WiFi Spanner {} bug report", env!("CARGO_PKG_VERSION")).replace(' ', "%20");
            ui.hyperlink_to(SUPPORT_EMAIL, format!("mailto:{SUPPORT_EMAIL}?subject={subject}"));
            ui.add_space(8.0);
            ui.label("It helps to include:");
            ui.label("  - what you did, what you expected and what happened");
            ui.label("  - the version and system below");
            ui.label("  - a screenshot, if it is something you can see");
            ui.add_space(8.0);
            ui.weak(support_details());
            ui.add_space(6.0);
            ui.weak(
                "This mailbox is not monitored directly. Reports are read and handled in order of how critical they \
                 are, not in the order received or by who sent them, and sending one does not guarantee a reply or \
                 a fix. Watch the change log shown with each update to see whether a bug has been fixed: no \
                 reply is sent when it is.",
            );
            ui.add_space(4.0);
            ui.weak(
                "Privacy: please leave out personal information, such as names, addresses and network names or \
                 passwords, and do not send survey projects unless asked to. If you choose to include personal \
                 information, you consent to Byte Size IT using it only to respond to your report.",
            );
            ui.hyperlink_to(BYTE_SIZE_SITE, BYTE_SIZE_URL);
            theme::actions(ui, |ui| {
                if theme::button(ui, ButtonKind::Text, Some(ICON_CLOSE), "Close").clicked() {
                    close = true;
                }
                if theme::button(ui, ButtonKind::Tonal, Some(ICON_CONTENT_COPY), "Copy details").clicked() {
                    ui.ctx().copy_text(support_details());
                }
                if theme::button(ui, ButtonKind::Tonal, Some(ICON_CONTENT_COPY), "Copy email").clicked() {
                    ui.ctx().copy_text(SUPPORT_EMAIL.to_owned());
                }
            });
        });
        if close {
            self.help_open = false;
        }
    }

    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let mut close = false;
        theme::dialog(ctx, "About WiFi Spanner", 380.0, |ui| {
            ui.vertical_centered(|ui| {
                if let Some(logo) = &self.logo {
                    ui.add(egui::Image::new(logo).fit_to_exact_size(egui::Vec2::splat(96.0)));
                }
                ui.label(egui::RichText::new("WiFi Spanner").heading());
                ui.label(TAGLINE);
                ui.weak(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.add_space(8.0);
                ui.weak(format!("by {BYTE_SIZE_NAME}"));
                ui.hyperlink_to(BYTE_SIZE_SITE, BYTE_SIZE_URL);
                if let Some(logo) = &self.byte_size_logo {
                    ui.add(egui::Image::new(logo).fit_to_exact_size(egui::Vec2::splat(40.0)));
                }
                ui.add_space(8.0);
                ui.label("Copyright (C) 2026 Byte Size IT");
                ui.hyperlink_to(REPO_URL, REPO_URL);
                ui.add_space(6.0);
                ui.weak(
                    "Free software under the GNU General Public License, version 3 or later, with attribution \
                     terms (see NOTICE). This program comes with ABSOLUTELY NO WARRANTY.",
                );
                ui.hyperlink_to("GNU General Public License", "https://www.gnu.org/licenses/gpl-3.0.html");
                ui.add_space(10.0);
                ui.weak("Report a bug:");
                ui.hyperlink_to(SUPPORT_EMAIL, format!("mailto:{SUPPORT_EMAIL}"));
            });
            theme::actions(ui, |ui| {
                if theme::button(ui, ButtonKind::Text, Some(ICON_CLOSE), "Close").clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.about_open = false;
        }
    }

    /// Push the current settings to the scanner thread and save them.
    fn apply_settings(&mut self) {
        self.worker.dwell_secs.store(self.settings.dwell_secs, Ordering::Relaxed);
        let _ = self.worker.tx.send(Cmd::SetCards(self.settings.active_cards(&self.cards)));
        self.settings.save();
    }

    fn settings_dialog(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let mut launcher = settings_ui::LauncherView {
            available: self.appimage.is_some(),
            installed: launcher::is_installed(),
            message: self.launcher_message.clone(),
            request: None,
            projects_dir: store::projects_dir().display().to_string(),
            projects_message: self.projects_message.clone(),
            move_projects: false,
        };
        if settings_ui::show(ctx, &mut open, &mut self.settings_section, &mut self.settings, &self.cards, &mut launcher) {
            theme::set_mode(ctx, self.settings.theme_mode);
            if theme::skin(ctx) != self.settings.design {
                theme::set_skin(ctx, self.settings.design);
            }
            theme::set_density(ctx, self.settings.density);
            self.apply_settings();
        }
        if let Some(add) = launcher.request {
            self.set_launcher(add);
        }
        if launcher.move_projects {
            self.move_projects_folder();
        }
        self.settings_open = open;
    }

    /// Ask for a folder and move every project (and its reports) into it.
    fn move_projects_folder(&mut self) {
        if self.floor.has_open_project() {
            self.projects_message = "Close the open project first (back to Projects), then move the folder.".into();
            return;
        }
        let old = store::projects_dir();
        let Some(new) = rfd::FileDialog::new().set_title("Move projects to").set_directory(&old).pick_folder() else { return };
        self.projects_message = match store::move_projects(&old, &new) {
            Ok(n) => {
                self.settings.projects_dir = Some(new.clone());
                store::set_projects_dir(Some(new.clone()));
                self.settings.save();
                format!("Moved {n} item(s) to {}.", new.display())
            }
            Err(e) => e,
        };
    }

    fn set_tab(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            if matches!(tab, Tab::Surveying | Tab::Planning) {
                self.gallery.mark_stale();
            }
            let _ = self.worker.tx.send(Cmd::Live(tab == Tab::Scanning));
        }
    }
}

impl eframe::App for App {
    /// Feed pen events into egui as pointer events.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // Which fingers are down, so the plan can tell a finger from the pen and the mouse. It is
        // still "touching" on the frame a finger lifts, so the lift is not taken as a pen stroke.
        let mut lifted = false;
        for e in &raw_input.events {
            if let egui::Event::Touch { id, phase, .. } = e {
                match phase {
                    egui::TouchPhase::Start | egui::TouchPhase::Move => {
                        self.touches.insert(id.0);
                    }
                    egui::TouchPhase::End | egui::TouchPhase::Cancel => {
                        self.touches.remove(&id.0);
                        lifted = true;
                    }
                }
            }
        }
        let touching = self.settings.pen_mode && (!self.touches.is_empty() || lifted);
        ctx.data_mut(|d| d.insert_temp(egui::Id::NULL.with("finger_touching"), touching));
        let Some(rx) = &self.pen_rx else { return };
        let events: Vec<_> = rx.try_iter().collect();
        let modifiers = ctx.input(|i| i.modifiers);
        raw_input.events.extend(pen::to_egui(events, &mut self.pen_pos, modifiers));
    }

    /// Fully clear while the splash is up, so its rounded corners are see-through;
    /// egui's usual dark clear colour afterwards.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.splash_since.is_some() {
            [0.0; 4]
        } else {
            egui::Color32::from_rgba_unmultiplied(12, 12, 12, 180).to_normalized_gamma_f32()
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if !self.pen_started {
            self.pen_started = true;
            self.pen_rx = pen::start(frame, ui.ctx().clone());
        }
        self.drain();
        self.drain_update();
        self.drain_install(ui.ctx());

        if let Some(since) = self.splash_since {
            if self.scanner_ready && since.elapsed() >= SPLASH_MIN {
                self.splash_since = None;
                let ctx = ui.ctx();
                ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Transparent(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(false));
                // Turning the title bar on takes its height out of the window's
                // content, so the size is restored a moment later (KDE applies
                // the decoration change after this frame and would override it).
                self.window.resize_at = Some(std::time::Instant::now() + RESIZE_DELAY);
                if self.settings.check_updates {
                    self.start_update_check(ctx, false);
                }
                if let Some(app) = &self.appimage {
                    if launcher::is_installed() {
                        launcher::refresh_if_stale(app);
                    } else if !self.settings.launcher_prompted {
                        self.launcher_prompt = true;
                    }
                }
            } else {
                draw_splash(ui, self.logo.as_ref(), self.byte_size_logo.as_ref());
                ui.ctx().request_repaint_after(Duration::from_millis(100));
                return;
            }
        }

        self.window_size_tick(ui.ctx());

        let mut action: Option<MenuAction> = None;
        let roles = theme::roles(ui.ctx());
        let bar_frame = egui::Frame::new().fill(roles.surface_container).inner_margin(egui::Margin::symmetric(12, 4));
        egui::Panel::top("status").frame(bar_frame).show(ui, |ui| {
            let bar_h = theme::density(ui.ctx()).bar_height();
            ui.spacing_mut().interact_size.y = bar_h;
            ui.spacing_mut().button_padding = egui::vec2(14.0, 10.0);
            ui.spacing_mut().item_spacing.x = 12.0;
            ui.horizontal(|ui| {
                let row = ui.available_rect_before_wrap();
                if let Some(logo) = &self.logo {
                    ui.add(egui::Image::new(logo).fit_to_exact_size(egui::Vec2::splat(bar_h - 14.0)));
                }
                ui.label(egui::RichText::new("WiFi Spanner").heading());
                // Tabs are centred on the bar; their width comes from the last
                // frame, as it is only known after they are laid out.
                let left = (row.center().x - self.tabs_width / 2.0).max(ui.cursor().min.x + 12.0);
                let rect = egui::Rect::from_min_size(
                    egui::pos2(left, row.top()),
                    egui::vec2(self.tabs_width + 8.0, bar_h),
                );
                let layout = egui::Layout::left_to_right(egui::Align::Center);
                let used = ui
                    .scope_builder(egui::UiBuilder::new().max_rect(rect).layout(layout), |ui| {
                        let mut tab = self.tab;
                        for (t, name) in [(Tab::Scanning, "Scanning"), (Tab::Surveying, "Surveying"), (Tab::Planning, "Planning")] {
                            if theme::tab(ui, tab == t, name, bar_h).clicked() {
                                tab = t;
                            }
                        }
                        self.set_tab(tab);
                        ui.min_rect().width()
                    })
                    .inner;
                if (used - self.tabs_width).abs() > 0.5 {
                    self.tabs_width = used;
                    ui.ctx().request_repaint();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let button = hamburger_button(ui, bar_h);
                    egui::Popup::menu(&button).show(|ui| self.main_menu(ui, &mut action));
                });
            });
        });
        match action {
            Some(MenuAction::OpenPlan) => {
                self.set_tab(Tab::Surveying);
                self.floor.pick_plan(ui.ctx());
            }
            Some(MenuAction::OpenProject) => {
                self.set_tab(Tab::Surveying);
                self.floor.pick_project(ui.ctx());
            }
            Some(MenuAction::ShowProjectsFolder) => {
                let dir = store::projects_dir();
                let _ = std::fs::create_dir_all(&dir);
                if let Err(e) = store::open_externally(&dir) {
                    self.error = Some(e);
                }
            }
            Some(MenuAction::CheckUpdates) => self.start_update_check(ui.ctx(), true),
            None => {}
        }
        self.update_notice_bar(ui);
        self.settings_dialog(ui.ctx());
        self.about_window(ui.ctx());
        self.help_window(ui.ctx());
        self.update_message_window(ui.ctx());
        self.install_window(ui.ctx());
        self.launcher_prompt_window(ui.ctx());
        self.toast_tick(ui.ctx());

        // Slim status bar at egui's default height, so it only fits its text.
        let status_frame = egui::Frame::new().fill(roles.surface_container).inner_margin(egui::Margin::symmetric(12, 1));
        egui::Panel::bottom("scan_status").frame(status_frame).show(ui, |ui| {
            // Information only: small text and no minimum row height keep it slim.
            ui.style_mut().override_font_id = Some(egui::FontId::new(12.0, egui::FontFamily::Proportional));
            ui.spacing_mut().interact_size.y = 14.0;
            ui.visuals_mut().override_text_color = Some(roles.on_surface_variant);
            ui.horizontal(|ui| {
                if self.scans_so_far > 0 {
                    ui.label(format!("scanning... {} done", self.scans_so_far));
                    ui.separator();
                }
                ui.label(&self.scanner_name);
                if self.tab == Tab::Scanning
                    && let Some(r) = &self.latest
                {
                    ui.separator();
                    ui.label(format!(
                        "{} networks ({} fresh), last dwell {:.1} s",
                        r.networks.len(),
                        r.fresh_count,
                        r.scan_ms as f64 / 1000.0
                    ));
                    if let Some(n) = r.noise_dbm {
                        ui.separator();
                        ui.label(format!("noise {n:.0} dBm"));
                    }
                }
                if let Some(e) = &self.error {
                    ui.separator();
                    ui.colored_label(egui::Color32::LIGHT_RED, e);
                }
                // The plan's scale sits at the right end while a plan is open.
                if self.tab != Tab::Scanning && !self.surveying_gallery {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| self.floor.scale_label(ui));
                }
            });
        });

        // A project opening (from the gallery, the menu or a new plan) shows its
        // workspace; the workspace's back button returns to the gallery.
        if self.floor.take_opened() {
            self.surveying_gallery = false;
        }
        if self.floor.take_back() {
            self.surveying_gallery = true;
            self.gallery.mark_stale();
        }
        match self.tab {
            Tab::Scanning => {
                egui::CentralPanel::default().frame(theme::page_frame(ui.ctx())).show(ui, |ui| live::show(ui, self.latest.as_ref()));
            }
            Tab::Surveying | Tab::Planning if self.surveying_gallery => {
                match self.gallery.show(ui, &self.settings) {
                    Some(gallery::GalleryAction::New) => self.floor.pick_plan(ui.ctx()),
                    Some(gallery::GalleryAction::Open(path)) => self.floor.open_project(ui.ctx(), path),
                    None => {}
                }
                // A multi-page PDF's page picker belongs to the workspace but opens from here.
                self.floor.show_dialogs(ui.ctx());
            }
            Tab::Surveying | Tab::Planning => {
                self.floor.set_mode(if self.tab == Tab::Planning { floor::WorkMode::Plan } else { floor::WorkMode::Survey });
                let env = floor::Env {
                    tx: &self.worker.tx,
                    dwell_secs: self.worker.dwell_secs.load(Ordering::Relaxed),
                    scans_so_far: self.scans_so_far,
                    settings: &self.settings,
                };
                self.floor.show(ui, &env);
            }
        }
    }
}

enum MenuAction {
    OpenPlan,
    OpenProject,
    ShowProjectsFolder,
    CheckUpdates,
}

/// An icon button with three bars, drawn with the painter, that shows a
/// Material state layer (a soft circle) on hover and press.
fn hamburger_button(ui: &mut egui::Ui, bar_h: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(bar_h - 6.0), egui::Sense::click());
    let roles = theme::roles(ui.ctx());
    let painter = ui.painter();
    if resp.hovered() || resp.is_pointer_button_down_on() {
        let opacity = if resp.is_pointer_button_down_on() { 0.12 } else { 0.08 };
        painter.circle_filled(rect.center(), rect.width() / 2.0, roles.on_surface.gamma_multiply(opacity));
    }
    let (c, half, gap) = (rect.center(), rect.width() * 0.26, rect.height() * 0.17);
    for dy in [-gap, 0.0, gap] {
        painter.line_segment(
            [egui::pos2(c.x - half, c.y + dy), egui::pos2(c.x + half, c.y + dy)],
            egui::Stroke::new(2.5, roles.on_surface_variant),
        );
    }
    resp.on_hover_text("Menu")
}

fn logo_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(LOGO_PNG).ok()?.resize(256, 256, image::imageops::FilterType::Lanczos3).to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some(ctx.load_texture("logo", egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()), egui::TextureOptions::LINEAR))
}

/// The version and system, for pasting into a bug report.
fn support_details() -> String {
    let system = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|t| t.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_owned())))
        .unwrap_or_else(|| std::env::consts::OS.to_owned());
    format!("WiFi Spanner {} on {system} ({})", env!("CARGO_PKG_VERSION"), std::env::consts::ARCH)
}

fn byte_size_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(BYTE_SIZE_PNG).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some(ctx.load_texture("byte_size_logo", egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()), egui::TextureOptions::LINEAR))
}

fn window_icon() -> egui::IconData {
    match image::load_from_memory(LOGO_PNG) {
        Ok(img) => {
            let img = img.resize(256, 256, image::imageops::FilterType::Lanczos3).to_rgba8();
            egui::IconData { width: img.width(), height: img.height(), rgba: img.into_raw() }
        }
        Err(_) => egui::IconData::default(),
    }
}

fn draw_splash(ui: &mut egui::Ui, logo: Option<&egui::TextureHandle>, byte_size: Option<&egui::TextureHandle>) {
    egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
        let window = ui.max_rect();
        let rect = egui::Rect::from_center_size(
            window.center(),
            egui::vec2(SPLASH_SIZE[0].min(window.width()), SPLASH_SIZE[1].min(window.height())),
        );
        let painter = ui.painter();
        let corners = [
            egui::Color32::from_rgb(226, 160, 100), // top left
            egui::Color32::from_rgb(238, 88, 112),  // top right
            egui::Color32::from_rgb(40, 150, 212),  // bottom right
            egui::Color32::from_rgb(122, 72, 200),  // bottom left
        ];
        let outline = rounded_rect_points(rect, SPLASH_RADIUS, 18);
        // A uniformly coloured, anti-aliased polygon underneath smooths the
        // edge; the gradient mesh is drawn over it.
        painter.add(egui::Shape::convex_polygon(outline.clone(), mix(corners, 0.5, 0.5), egui::Stroke::NONE));
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(rect.center(), mix(corners, 0.5, 0.5));
        for p in &outline {
            mesh.colored_vertex(*p, mix(corners, (p.x - rect.left()) / rect.width(), (p.y - rect.top()) / rect.height()));
        }
        let n = outline.len() as u32;
        for i in 0..n {
            mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
        }
        painter.add(egui::Shape::mesh(mesh));

        let c = rect.center() - egui::vec2(0.0, 25.0);
        if let Some(logo) = logo {
            let r = egui::Rect::from_center_size(c - egui::vec2(0.0, 62.0), egui::Vec2::splat(112.0));
            painter.image(logo.id(), r, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        }
        let shadow = egui::Color32::from_black_alpha(90);
        for (text, y, size) in [("WiFi Spanner", 50.0, 36.0), (TAGLINE, 92.0, 17.0)] {
            let font = egui::FontId::proportional(size);
            painter.text(c + egui::vec2(1.5, y + 1.5), egui::Align2::CENTER_CENTER, text, font.clone(), shadow);
            painter.text(c + egui::vec2(0.0, y), egui::Align2::CENTER_CENTER, text, font, egui::Color32::WHITE);
        }
        let soft = egui::Color32::from_white_alpha(215);
        painter.text(c + egui::vec2(0.0, 128.0), egui::Align2::CENTER_CENTER, format!("by {BYTE_SIZE_NAME}"), egui::FontId::proportional(13.0), soft);
        painter.text(c + egui::vec2(0.0, 145.0), egui::Align2::CENTER_CENTER, BYTE_SIZE_SITE, egui::FontId::proportional(12.0), egui::Color32::from_white_alpha(170));
        if let Some(byte_size) = byte_size {
            let r = egui::Rect::from_center_size(c + egui::vec2(0.0, 176.0), egui::Vec2::splat(30.0));
            painter.image(byte_size.id(), r, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        }
        painter.text(
            rect.center_bottom() - egui::vec2(0.0, 14.0),
            egui::Align2::CENTER_CENTER,
            "Loading...",
            egui::FontId::proportional(12.0),
            egui::Color32::from_white_alpha(190),
        );
    });
}

/// The size to open the window at: the saved one within sane limits, else the
/// default. The screen size is not known yet, so the desktop does the final fitting.
/// What a window leaves unusable on a screen (width, height): the title bar,
/// the desktop panel and a grip on each side. A window kept inside this can
/// always be reached and resized.
const SCREEN_MARGIN: [f32; 2] = [24.0, 120.0];

/// The size to open at. The desktop places the window before the app can move
/// it, so a saved size is used only when it is known to fit the screen it was
/// saved on; otherwise the default is safer than running off screen.
fn start_size(saved: Option<[f32; 2]>, screen: Option<[f32; 2]>) -> [f32; 2] {
    match (saved, screen) {
        (Some(size), Some([sw, sh])) => {
            let fit = restored_size(Some(size), egui::vec2(sw, sh));
            [fit.x, fit.y]
        }
        _ => MAIN_SIZE,
    }
}

/// The saved window size kept within sane limits and the screen, or the
/// default when nothing is saved.
fn restored_size(saved: Option<[f32; 2]>, screen: egui::Vec2) -> egui::Vec2 {
    match saved {
        Some([w, h]) => {
            // Leave room for the title bar, the desktop panel and the window's edges.
            let max = egui::vec2(screen.x - SCREEN_MARGIN[0], screen.y - SCREEN_MARGIN[1]);
            egui::vec2(w.clamp(480.0, max.x.max(480.0)), h.clamp(320.0, max.y.max(320.0)))
        }
        None => egui::vec2(MAIN_SIZE[0], MAIN_SIZE[1]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounded_outline_stays_inside_and_cuts_the_corners() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(560.0, 340.0));
        let pts = rounded_rect_points(rect, 150.0, 18);
        assert_eq!(pts.len(), 4 * 19);
        assert!(pts.iter().all(|p| rect.expand(0.01).contains(*p)));
        assert!(!pts.iter().any(|p| p.x < 1.0 && p.y < 1.0), "the top-left corner point is cut away");
        // The radius is capped at half the shorter side: 150 fits in 340, but 500 does not.
        let big = rounded_rect_points(rect, 500.0, 18);
        assert!(big.iter().all(|p| rect.expand(0.01).contains(*p)));
        assert!(big.iter().any(|p| (p.y - 170.0).abs() < 0.01), "capped radius still reaches the side midpoint");
    }

    #[test]
    fn start_size_uses_the_saved_size_within_limits() {
        let screen = Some([1920.0, 1080.0]);
        assert_eq!(start_size(None, screen), [1200.0, 700.0]);
        assert_eq!(start_size(Some([1500.0, 900.0]), None), [1200.0, 700.0], "a size from an unknown screen is not trusted");
        assert_eq!(start_size(Some([1500.0, 900.0]), screen), [1500.0, 900.0]);
        assert_eq!(start_size(Some([10.0, 99999.0]), screen), [480.0, 960.0]);
    }

    #[test]
    fn window_size_defaults_and_is_kept_on_screen() {
        let screen = egui::vec2(1920.0, 1080.0);
        assert_eq!(restored_size(None, screen), egui::vec2(1200.0, 700.0));
        assert_eq!(restored_size(Some([1400.0, 900.0]), screen), egui::vec2(1400.0, 900.0));
        assert_eq!(restored_size(Some([5000.0, 4000.0]), screen), egui::vec2(1896.0, 960.0), "a saved size from a bigger screen is shrunk");
        assert_eq!(restored_size(Some([1920.0, 1060.0]), screen), egui::vec2(1896.0, 960.0), "a window never fills the whole screen");
        assert_eq!(restored_size(Some([10.0, 10.0]), screen), egui::vec2(480.0, 320.0), "a tiny size is raised");
    }
}

/// Colour at (u, v) in 0..1 across a rectangle whose corners are, in order,
/// top left, top right, bottom right, bottom left.
fn mix(c: [egui::Color32; 4], u: f32, v: f32) -> egui::Color32 {
    let top = c[0].lerp_to_gamma(c[1], u);
    let bottom = c[3].lerp_to_gamma(c[2], u);
    top.lerp_to_gamma(bottom, v)
}

/// Points around a rounded rectangle, clockwise from the top-left arc. The
/// radius is limited to half the shorter side; each corner gets `segments` arc steps.
fn rounded_rect_points(rect: egui::Rect, radius: f32, segments: usize) -> Vec<egui::Pos2> {
    let r = radius.min(rect.width() / 2.0).min(rect.height() / 2.0).max(0.0);
    let centres = [
        (egui::pos2(rect.left() + r, rect.top() + r), 180.0f32),
        (egui::pos2(rect.right() - r, rect.top() + r), 270.0),
        (egui::pos2(rect.right() - r, rect.bottom() - r), 0.0),
        (egui::pos2(rect.left() + r, rect.bottom() - r), 90.0),
    ];
    let mut points = Vec::with_capacity(4 * (segments + 1));
    for (centre, start_deg) in centres {
        for i in 0..=segments {
            let a = (start_deg + 90.0 * i as f32 / segments as f32).to_radians();
            points.push(centre + egui::vec2(a.cos(), a.sin()) * r);
        }
    }
    points
}

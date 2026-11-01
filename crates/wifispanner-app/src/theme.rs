// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! Two looks for the same layouts: Material Design 3 (colour roles generated from
//! a seed colour, pills, tonal buttons) and IBM's Carbon (flat, square, IBM Plex).
//! egui has neither, so this styles egui's own widgets and draws the few
//! components it lacks (tabs, pills, a slider) to match each.

use std::sync::OnceLock;

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle};
use material_colors::{color::Rgb, theme::ThemeBuilder};
use serde::{Deserialize, Serialize};

/// The seed colour (the corporate teal). Every colour role derives from it.
pub const SEED: u32 = 0x008080;

/// Text format with the glyphs centred on the line, so an icon and its label
/// (which have different sizes) sit on the same middle line.
fn centred(font: FontId, color: Color32) -> egui::TextFormat {
    egui::TextFormat { valign: egui::Align::Center, ..egui::TextFormat::simple(font, color) }
}

/// Font families added at start. Roboto serves the Material look and IBM Plex the Carbon one.
const ROBOTO_MEDIUM_FAMILY: &str = "roboto-medium";
const PLEX_REGULAR_FAMILY: &str = "plex";
const PLEX_MEDIUM_FAMILY: &str = "plex-medium";
const PLEX_SEMIBOLD_FAMILY: &str = "plex-semibold";

/// Which design the app wears.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Skin {
    /// Material Design 3: pills, tonal buttons, soft corners, Roboto.
    #[default]
    Material,
    /// IBM Carbon: flat and square, IBM Plex, a blue accent.
    Carbon,
}

impl Skin {
    pub const ALL: [Skin; 2] = [Skin::Material, Skin::Carbon];

    pub fn name(self) -> &'static str {
        match self {
            Skin::Material => "Material Design",
            Skin::Carbon => "IBM Carbon",
        }
    }
}

/// The skin in use.
pub fn skin(ctx: &egui::Context) -> Skin {
    ctx.data(|d| d.get_temp(egui::Id::unique("skin"))).unwrap_or_default()
}

/// The text style for labels in the medium weight.
pub fn medium_style() -> TextStyle {
    TextStyle::Name("medium".into())
}

/// The text style for headings inside menus and panels.
pub fn heading_style() -> TextStyle {
    TextStyle::Name("heading".into())
}

type Icon = egui_material_icons::MaterialIcon;

/// Carbon's drawing for each Material icon the app uses: its name, its picture
/// (white on transparent, tinted when drawn), from IBM's Carbon icon set.
fn carbon_icon(icon: &Icon) -> Option<(&'static str, &'static [u8])> {
    use egui_material_icons::icons::*;
    macro_rules! art {
        ($name:literal) => {
            ($name, include_bytes!(concat!("../assets/carbon-icons/png/", $name, ".png")) as &'static [u8])
        };
    }
    let table: [(Icon, (&str, &'static [u8])); 32] = [
        (ICON_IMAGE, art!("image")),
        (ICON_FOLDER, art!("folder")),
        (ICON_FOLDER_OPEN, art!("folder--open")),
        (ICON_REFRESH, art!("renew")),
        (ICON_DESCRIPTION, art!("document")),
        (ICON_ADD, art!("add")),
        (ICON_UNDO, art!("undo")),
        (ICON_SYSTEM_UPDATE, art!("upgrade")),
        (ICON_SMARTPHONE, art!("mobile")),
        (ICON_ROUTER, art!("router")),
        (ICON_DELETE, art!("trash-can")),
        (ICON_OPEN_IN_NEW, art!("launch")),
        (ICON_MORE_VERT, art!("overflow-menu--vertical")),
        (ICON_EDIT, art!("edit")),
        (ICON_CROP_FREE, art!("area")),
        (ICON_WIFI, art!("wifi")),
        (ICON_VISIBILITY, art!("view")),
        (ICON_VISIBILITY_OFF, art!("view--off")),
        (ICON_VIEW_LIST, art!("list")),
        (ICON_STRAIGHTEN, art!("ruler")),
        (ICON_SETTINGS, art!("settings")),
        (ICON_SAVE, art!("save")),
        (ICON_RESTART_ALT, art!("reset")),
        (ICON_POWER_SETTINGS_NEW, art!("power")),
        (ICON_PLACE, art!("location")),
        (ICON_OPEN_WITH, art!("move")),
        (ICON_INFO, art!("information")),
        (ICON_CLOSE, art!("close")),
        (ICON_CHECK, art!("checkmark")),
        (ICON_CATEGORY, art!("category")),
        (ICON_ARROW_SELECTOR_TOOL, art!("cursor--1")),
        (ICON_ARROW_BACK, art!("arrow--left")),
    ];
    table.into_iter().find(|(i, _)| i.codepoint == icon.codepoint).map(|(_, art)| art)
}

/// The picture of an icon in Carbon, as a texture; `None` in Material or for an icon Carbon has no drawing for.
pub fn icon_texture(ctx: &egui::Context, icon: Icon) -> Option<egui::TextureHandle> {
    if skin(ctx) != Skin::Carbon {
        return None;
    }
    let (name, bytes) = carbon_icon(&icon)?;
    carbon_texture(ctx, name, bytes)
}

/// A named Carbon picture, loaded once.
pub fn carbon_texture(ctx: &egui::Context, name: &'static str, bytes: &'static [u8]) -> Option<egui::TextureHandle> {
    let id = egui::Id::unique(("carbon-icon", name));
    if let Some(tex) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return Some(tex);
    }
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let tex = ctx.load_texture(format!("carbon-{name}"), egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()), egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
    Some(tex)
}

/// The layers picture for the floors button in Carbon.
pub fn carbon_layers(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    carbon_texture(ctx, "layers", include_bytes!("../assets/carbon-icons/png/layers.png"))
}

/// Paint an icon centred on `centre`, `size` points across: Carbon's drawing in
/// Carbon, Material's glyph otherwise.
pub fn paint_icon(painter: &egui::Painter, ctx: &egui::Context, icon: Icon, centre: egui::Pos2, size: f32, colour: Color32) {
    match icon_texture(ctx, icon) {
        Some(tex) => {
            let rect = egui::Rect::from_center_size(centre, egui::Vec2::splat(size));
            painter.image(tex.id(), rect, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), colour);
        }
        None => {
            painter.text(centre, egui::Align2::CENTER_CENTER, icon.codepoint, FontId::new(size, icon.font_family()), colour);
        }
    }
}

/// A small icon as an image for a button or menu entry, tinted with `colour`.
fn icon_image(ctx: &egui::Context, icon: Icon, size: f32, colour: Color32) -> Option<egui::Image<'static>> {
    let tex = icon_texture(ctx, icon)?;
    Some(egui::Image::from_texture(egui::load::SizedTexture::from_handle(&tex)).fit_to_exact_size(egui::Vec2::splat(size)).tint(colour))
}

/// A menu entry with an icon, as an egui button: Carbon's drawing in Carbon, a
/// glyph in the text in Material. With no icon the label is indented to line up.
pub fn menu_entry(ui: &egui::Ui, icon: Option<Icon>, text: &str) -> egui::Button<'static> {
    if let Some(img) = icon.and_then(|i| icon_image(ui.ctx(), i, 18.0, ui.visuals().text_color())) {
        return egui::Button::new((img, text.to_string()));
    }
    egui::Button::new(icon_label(ui, icon, text))
}

/// A selectable menu entry with a check mark column that is blank when unchecked.
pub fn check_entry(ui: &egui::Ui, selected: bool, text: &str) -> egui::Button<'static> {
    let colour = if selected { ui.visuals().text_color() } else { Color32::TRANSPARENT };
    if let Some(img) = icon_image(ui.ctx(), egui_material_icons::icons::ICON_CHECK, 18.0, colour) {
        return egui::Button::selectable(selected, (img, text.to_string()));
    }
    egui::Button::selectable(selected, check_label(ui, selected, text))
}

/// A menu entry that opens a submenu.
pub fn submenu<R>(ui: &mut egui::Ui, icon: Option<Icon>, text: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> (egui::Response, Option<egui::InnerResponse<R>>) {
    let button = menu_entry(ui, icon, text).right_text(egui::containers::menu::SubMenuButton::arrow_atom(None));
    egui::containers::menu::SubMenuButton::from_button(button).ui(ui, add)
}

/// Carbon's text field look: a 1 px bottom edge at rest, a 2 px outline while focused. A
/// no-op in Material, whose fields are boxed by the widgets themselves.
pub fn field_edge(ui: &egui::Ui, resp: &egui::Response) {
    if skin(ui.ctx()) != Skin::Carbon {
        return;
    }
    let r = roles(ui.ctx());
    let rect = resp.rect;
    if resp.has_focus() {
        let focus = if ui.ctx().theme() == egui::Theme::Dark { Color32::WHITE } else { r.primary };
        ui.painter().rect_stroke(rect.shrink(1.0), 0.0, Stroke::new(2.0, focus), egui::StrokeKind::Inside);
    } else {
        ui.painter().line_segment([rect.left_bottom() - egui::vec2(0.0, 0.5), rect.right_bottom() - egui::vec2(0.0, 0.5)], Stroke::new(1.0, r.outline));
    }
}

/// Add a field (a text box or value box) and give it Carbon's edge.
pub fn field(ui: &mut egui::Ui, widget: impl egui::Widget) -> egui::Response {
    let resp = ui.add(widget);
    field_edge(ui, &resp);
    resp
}

/// Give a drop-down's closed box Carbon's edge, and pass it on.
pub fn dropdown<R>(shown: egui::InnerResponse<R>, ui: &egui::Ui) -> egui::InnerResponse<R> {
    field_edge(ui, &shown.response);
    shown
}

/// The kinds of rounded shape in the app; Material rounds them, Carbon squares them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// A button or chip, as tall as `height`.
    Pill,
    /// A list row or small control.
    Row,
    /// A card or tile.
    Card,
    /// A floating menu over the plan.
    Panel,
    Dialog,
}

/// Corner radius for a shape in the skin in use. `height` is only for pills.
pub fn radius(ctx: &egui::Context, shape: Shape, height: f32) -> u8 {
    match skin(ctx) {
        Skin::Carbon => 0,
        Skin::Material => match shape {
            Shape::Pill => (height / 2.0).round() as u8,
            Shape::Row => 8,
            Shape::Card => 12,
            Shape::Panel => 20,
            Shape::Dialog => 28,
        },
    }
}

/// How much room lists and tables take: row height, text size and spacing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Density {
    Compact,
    Smaller,
    #[default]
    Default,
    Larger,
}

impl Density {
    pub const ALL: [Density; 4] = [Density::Compact, Density::Smaller, Density::Default, Density::Larger];

    pub fn name(self) -> &'static str {
        match self {
            Density::Compact => "Compact",
            Density::Smaller => "Smaller",
            Density::Default => "Default",
            Density::Larger => "Larger",
        }
    }

    /// Minimum height of a table row, list item or field.
    pub fn row_height(self) -> f32 {
        match self {
            Density::Compact => 22.0,
            Density::Smaller => 30.0,
            Density::Default => 40.0,
            Density::Larger => 52.0,
        }
    }

    /// Height of buttons, chips and fields in the toolbars.
    pub fn button_height(self) -> f32 {
        match self {
            Density::Compact => 28.0,
            Density::Smaller => 34.0,
            Density::Default => 40.0,
            Density::Larger => 48.0,
        }
    }

    /// Height of the top bar's row (tabs and menu button).
    pub fn bar_height(self) -> f32 {
        match self {
            Density::Compact => 36.0,
            Density::Smaller => 42.0,
            Density::Default => 48.0,
            Density::Larger => 56.0,
        }
    }

    /// Vertical padding inside buttons, drop-downs and value fields, so a field's
    /// own height shrinks and grows with the density.
    pub fn padding_y(self) -> f32 {
        match self {
            Density::Compact => 3.0,
            Density::Smaller => 6.0,
            Density::Default => 9.0,
            Density::Larger => 11.0,
        }
    }

    /// Scale of the project cards in the gallery.
    pub fn card_scale(self) -> f32 {
        match self {
            Density::Compact => 0.7,
            Density::Smaller => 0.85,
            Density::Default => 1.0,
            Density::Larger => 1.2,
        }
    }

    /// Multiplier for body text sizes.
    pub fn font_scale(self) -> f32 {
        match self {
            Density::Compact => 0.84,
            Density::Smaller => 0.93,
            Density::Default => 1.0,
            Density::Larger => 1.15,
        }
    }

    /// Vertical gap between items.
    pub fn item_gap(self) -> f32 {
        match self {
            Density::Compact => 2.0,
            Density::Smaller => 5.0,
            Density::Default => 8.0,
            Density::Larger => 11.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ThemeMode {
    /// Follow the desktop's light or dark setting.
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeMode {
    pub const ALL: [ThemeMode; 3] = [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark];

    pub fn name(self) -> &'static str {
        match self {
            ThemeMode::System => "Follow system",
            ThemeMode::Light => "Light",
            ThemeMode::Dark => "Dark",
        }
    }

    fn preference(self) -> egui::ThemePreference {
        match self {
            ThemeMode::System => egui::ThemePreference::System,
            ThemeMode::Light => egui::ThemePreference::Light,
            ThemeMode::Dark => egui::ThemePreference::Dark,
        }
    }
}

/// The Material 3 colour roles this app uses, for one theme.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // some roles are for components added in later phases
pub struct Roles {
    pub primary: Color32,
    /// Accent colour for text and thin lines on the surface: the primary in Material, a lighter blue in dark Carbon.
    pub link: Color32,
    pub on_primary: Color32,
    pub primary_container: Color32,
    pub on_primary_container: Color32,
    pub secondary_container: Color32,
    pub on_secondary_container: Color32,
    pub error: Color32,
    pub error_container: Color32,
    pub on_error_container: Color32,
    pub surface: Color32,
    pub surface_container_lowest: Color32,
    pub surface_container_low: Color32,
    pub surface_container: Color32,
    pub surface_container_high: Color32,
    pub surface_container_highest: Color32,
    pub on_surface: Color32,
    pub on_surface_variant: Color32,
    pub outline: Color32,
    pub outline_variant: Color32,
    pub inverse_surface: Color32,
    pub inverse_on_surface: Color32,
    pub shadow: Color32,
}

fn c(rgb: Rgb) -> Color32 {
    Color32::from_rgb(rgb.red, rgb.green, rgb.blue)
}

fn roles_from(s: &material_colors::scheme::Scheme) -> Roles {
    Roles {
        primary: c(s.primary),
        link: c(s.primary),
        on_primary: c(s.on_primary),
        primary_container: c(s.primary_container),
        on_primary_container: c(s.on_primary_container),
        secondary_container: c(s.secondary_container),
        on_secondary_container: c(s.on_secondary_container),
        error: c(s.error),
        error_container: c(s.error_container),
        on_error_container: c(s.on_error_container),
        surface: c(s.surface),
        surface_container_lowest: c(s.surface_container_lowest),
        surface_container_low: c(s.surface_container_low),
        surface_container: c(s.surface_container),
        surface_container_high: c(s.surface_container_high),
        surface_container_highest: c(s.surface_container_highest),
        on_surface: c(s.on_surface),
        on_surface_variant: c(s.on_surface_variant),
        outline: c(s.outline),
        outline_variant: c(s.outline_variant),
        inverse_surface: c(s.inverse_surface),
        inverse_on_surface: c(s.inverse_on_surface),
        shadow: c(s.shadow),
    }
}

/// Carbon's tokens, from the White theme (light) and Gray 100 (dark), onto the roles this app uses.
fn carbon_roles(dark: bool) -> Roles {
    let rgb = |hex: u32| Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8);
    if dark {
        Roles {
            primary: rgb(0x0f62fe),
            link: rgb(0x78a9ff),
            on_primary: rgb(0xffffff),
            primary_container: rgb(0x393939),
            on_primary_container: rgb(0xf4f4f4),
            secondary_container: rgb(0x393939),
            on_secondary_container: rgb(0xf4f4f4),
            error: rgb(0xff8389),
            error_container: rgb(0x520408),
            on_error_container: rgb(0xffd7d9),
            surface: rgb(0x161616),
            surface_container_lowest: rgb(0x161616),
            surface_container_low: rgb(0x1c1c1c),
            surface_container: rgb(0x262626),
            surface_container_high: rgb(0x262626),
            surface_container_highest: rgb(0x393939),
            on_surface: rgb(0xf4f4f4),
            on_surface_variant: rgb(0xc6c6c6),
            outline: rgb(0x8d8d8d),
            outline_variant: rgb(0x525252),
            inverse_surface: rgb(0xf4f4f4),
            inverse_on_surface: rgb(0x161616),
            shadow: Color32::BLACK,
        }
    } else {
        Roles {
            primary: rgb(0x0f62fe),
            link: rgb(0x0f62fe),
            on_primary: rgb(0xffffff),
            primary_container: rgb(0xe0e0e0),
            on_primary_container: rgb(0x161616),
            secondary_container: rgb(0xe0e0e0),
            on_secondary_container: rgb(0x161616),
            error: rgb(0xda1e28),
            error_container: rgb(0xfff1f1),
            on_error_container: rgb(0xa2191f),
            surface: rgb(0xffffff),
            surface_container_lowest: rgb(0xf4f4f4),
            surface_container_low: rgb(0xf4f4f4),
            surface_container: rgb(0xf4f4f4),
            surface_container_high: rgb(0xffffff),
            surface_container_highest: rgb(0xe8e8e8),
            on_surface: rgb(0x161616),
            on_surface_variant: rgb(0x525252),
            outline: rgb(0x8d8d8d),
            outline_variant: rgb(0xc6c6c6),
            inverse_surface: rgb(0x393939),
            inverse_on_surface: rgb(0xf4f4f4),
            shadow: Color32::BLACK,
        }
    }
}

/// Light and dark roles of both skins, built once (Material's from the seed).
fn palettes() -> &'static [(Roles, Roles); 2] {
    static PALETTES: OnceLock<[(Roles, Roles); 2]> = OnceLock::new();
    PALETTES.get_or_init(|| {
        let theme = ThemeBuilder::with_source(Rgb::from_u32(SEED)).build();
        [(roles_from(&theme.schemes.light), roles_from(&theme.schemes.dark)), (carbon_roles(false), carbon_roles(true))]
    })
}

pub fn roles_for(theme: egui::Theme, skin: Skin) -> &'static Roles {
    let pair = &palettes()[match skin {
        Skin::Material => 0,
        Skin::Carbon => 1,
    }];
    match theme {
        egui::Theme::Light => &pair.0,
        egui::Theme::Dark => &pair.1,
    }
}

/// The roles for whichever skin and theme are showing now.
pub fn roles(ctx: &egui::Context) -> &'static Roles {
    roles_for(ctx.theme(), skin(ctx))
}

/// `color` with `layer` laid over it at `opacity`: Material's "state layer"
/// for hover (8%), focus and press (12%).
pub fn state_layer(color: Color32, layer: Color32, opacity: f32) -> Color32 {
    let mix = |a: u8, b: u8| (a as f32 * (1.0 - opacity) + b as f32 * opacity).round() as u8;
    Color32::from_rgb(mix(color.r(), layer.r()), mix(color.g(), layer.g()), mix(color.b(), layer.b()))
}

/// Install the fonts, colours and shapes, and choose the light/dark mode.
/// Call once at start; call `set_mode`, `set_density` or `set_skin` when the user changes them.
pub fn install(ctx: &egui::Context, mode: ThemeMode, density: Density, skin: Skin) {
    egui_material_icons::initialize(ctx);
    install_fonts(ctx);
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("density"), density));
    set_skin(ctx, skin);
    set_mode(ctx, mode);
}

/// Change the design: colours, shapes and type, for both light and dark.
pub fn set_skin(ctx: &egui::Context, skin: Skin) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("skin"), skin));
    ctx.set_visuals_of(egui::Theme::Light, visuals(roles_for(egui::Theme::Light, skin), false, skin));
    ctx.set_visuals_of(egui::Theme::Dark, visuals(roles_for(egui::Theme::Dark, skin), true, skin));
    let d = density(ctx);
    ctx.all_styles_mut(|style| shape_style(style, d, skin));
}

pub fn set_mode(ctx: &egui::Context, mode: ThemeMode) {
    ctx.set_theme(mode.preference());
}

/// Apply a density to both themes' type sizes and spacing.
pub fn set_density(ctx: &egui::Context, density: Density) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("density"), density));
    let skin = skin(ctx);
    ctx.all_styles_mut(|style| shape_style(style, density, skin));
}

/// The density in use.
pub fn density(ctx: &egui::Context) -> Density {
    ctx.data(|d| d.get_temp(egui::Id::unique("density"))).unwrap_or_default()
}

fn install_fonts(ctx: &egui::Context) {
    use damascene_fonts_roboto::{ROBOTO_MEDIUM, ROBOTO_REGULAR};
    use egui::{FontData, FontInsert, InsertFontFamily};
    use egui::epaint::text::FontPriority;
    // Roboto is the base family; IBM Plex has families of its own that the Carbon skin's text styles name.
    ctx.add_font(FontInsert::new(
        "roboto",
        FontData::from_static(ROBOTO_REGULAR),
        vec![
            InsertFontFamily { family: FontFamily::Proportional, priority: FontPriority::Highest },
            InsertFontFamily { family: FontFamily::Monospace, priority: FontPriority::Lowest },
        ],
    ));
    let named = |name: &str, data: &'static [u8]| {
        FontInsert::new(name, FontData::from_static(data), vec![InsertFontFamily { family: FontFamily::Name(name.into()), priority: FontPriority::Highest }])
    };
    ctx.add_font(named(ROBOTO_MEDIUM_FAMILY, ROBOTO_MEDIUM));
    ctx.add_font(named(PLEX_REGULAR_FAMILY, include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf")));
    ctx.add_font(named(PLEX_MEDIUM_FAMILY, include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf")));
    ctx.add_font(named(PLEX_SEMIBOLD_FAMILY, include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf")));
}

/// Type scale and spacing for a skin, shared by both themes.
fn shape_style(style: &mut egui::Style, density: Density, skin: Skin) {
    let scale = density.font_scale();
    let name = |n: &str| FontFamily::Name(n.into());
    style.text_styles = match skin {
        Skin::Material => vec![
            (TextStyle::Heading, FontId::new(24.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(15.0 * scale, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(15.0 * scale, name(ROBOTO_MEDIUM_FAMILY))),
            (TextStyle::Small, FontId::new(12.0 * scale.max(0.9), FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(14.0 * scale, FontFamily::Monospace)),
            (medium_style(), FontId::new(15.0 * scale, name(ROBOTO_MEDIUM_FAMILY))),
            (heading_style(), FontId::new(16.5 * scale, name(ROBOTO_MEDIUM_FAMILY))),
        ],
        // Carbon's body is 14 px; its buttons are the same size in the regular weight.
        Skin::Carbon => vec![
            (TextStyle::Heading, FontId::new(24.0, name(PLEX_SEMIBOLD_FAMILY))),
            (TextStyle::Body, FontId::new(14.0 * scale, name(PLEX_REGULAR_FAMILY))),
            (TextStyle::Button, FontId::new(14.0 * scale, name(PLEX_REGULAR_FAMILY))),
            (TextStyle::Small, FontId::new(12.0 * scale.max(0.9), name(PLEX_REGULAR_FAMILY))),
            (TextStyle::Monospace, FontId::new(14.0 * scale, FontFamily::Monospace)),
            (medium_style(), FontId::new(14.0 * scale, name(PLEX_MEDIUM_FAMILY))),
            (heading_style(), FontId::new(15.5 * scale, name(PLEX_SEMIBOLD_FAMILY))),
        ],
    }
    .into_iter()
    .collect();
    let sp = &mut style.spacing;
    sp.item_spacing = egui::vec2(10.0, density.item_gap());
    sp.button_padding = egui::vec2(if skin == Skin::Carbon { 16.0 } else { 20.0 }, density.padding_y());
    sp.interact_size = egui::vec2(48.0, density.row_height());
    sp.window_margin = Margin::same(if skin == Skin::Carbon { 16 } else { 24 });
    sp.menu_margin = Margin::symmetric(8, 8);
    sp.slider_width = 200.0;
}

/// A skin's colours and shapes mapped onto egui's widget states.
fn visuals(r: &Roles, dark: bool, skin: Skin) -> egui::Visuals {
    let carbon = skin == Skin::Carbon;
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = r.surface;
    v.window_fill = r.surface_container_high;
    v.extreme_bg_color = r.surface_container_lowest;
    v.faint_bg_color = r.surface_container_low;
    v.code_bg_color = r.surface_container_highest;
    v.hyperlink_color = r.link;
    v.error_fg_color = r.error;
    v.window_stroke = if carbon { Stroke::new(1.0, r.outline_variant) } else { Stroke::NONE };
    v.window_corner_radius = CornerRadius::same(if carbon { 0 } else { 28 });
    v.menu_corner_radius = CornerRadius::same(if carbon { 0 } else { 12 });
    v.window_shadow = shadow(r.shadow, if carbon { 12 } else { 24 }, if carbon { 0.30 } else { 0.20 });
    v.popup_shadow = shadow(r.shadow, if carbon { 8 } else { 14 }, if carbon { 0.30 } else { 0.22 });
    v.selection.bg_fill = if carbon { r.primary.gamma_multiply(0.4) } else { r.primary_container };
    // Also the keyboard focus ring: Carbon's is white on dark and blue on light.
    v.selection.stroke = Stroke::new(2.0, if carbon { if dark { Color32::WHITE } else { r.primary } } else { r.primary });
    v.text_cursor.stroke = Stroke::new(2.0, if carbon { r.on_surface } else { r.primary });
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.indent_has_left_vline = false;
    v.striped = true;

    // Pills in Material (egui rounds no further than half a control's height), squares in Carbon.
    let radius = CornerRadius::same(if carbon { 0 } else { 20 });
    let w = &mut v.widgets;
    // Labels and frames.
    w.noninteractive.bg_fill = r.surface;
    w.noninteractive.weak_bg_fill = r.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, r.outline_variant);
    w.noninteractive.fg_stroke = Stroke::new(1.0, r.on_surface);
    w.noninteractive.corner_radius = radius;
    if carbon {
        // Fields and controls: the field colour, a stronger bottom edge on hover, no tonal buttons.
        let field = if dark { Color32::from_rgb(0x26, 0x26, 0x26) } else { Color32::from_rgb(0xf4, 0xf4, 0xf4) };
        let hover = if dark { Color32::from_rgb(0x33, 0x33, 0x33) } else { Color32::from_rgb(0xe8, 0xe8, 0xe8) };
        let press = if dark { Color32::from_rgb(0x39, 0x39, 0x39) } else { Color32::from_rgb(0xe0, 0xe0, 0xe0) };
        w.inactive.bg_fill = field;
        w.inactive.weak_bg_fill = field;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, r.on_surface);
        w.inactive.corner_radius = radius;
        w.hovered.bg_fill = hover;
        w.hovered.weak_bg_fill = hover;
        w.hovered.bg_stroke = Stroke::NONE;
        w.hovered.fg_stroke = Stroke::new(1.0, r.on_surface);
        w.hovered.corner_radius = radius;
        w.active.bg_fill = press;
        w.active.weak_bg_fill = press;
        w.active.bg_stroke = Stroke::NONE;
        w.active.fg_stroke = Stroke::new(1.0, r.on_surface);
        w.active.corner_radius = radius;
    } else {
        // Buttons at rest are "tonal": secondary container with matching text.
        w.inactive.bg_fill = r.surface_container_highest;
        w.inactive.weak_bg_fill = r.secondary_container;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, r.on_secondary_container);
        w.inactive.corner_radius = radius;
        // Hover and press add Material's 8% and 12% state layers.
        w.hovered.bg_fill = state_layer(r.surface_container_highest, r.on_surface, 0.08);
        w.hovered.weak_bg_fill = state_layer(r.secondary_container, r.on_secondary_container, 0.08);
        w.hovered.bg_stroke = Stroke::new(1.0, r.outline);
        w.hovered.fg_stroke = Stroke::new(1.0, r.on_secondary_container);
        w.hovered.corner_radius = radius;
        w.active.bg_fill = state_layer(r.surface_container_highest, r.on_surface, 0.12);
        w.active.weak_bg_fill = state_layer(r.secondary_container, r.on_secondary_container, 0.12);
        w.active.bg_stroke = Stroke::new(1.0, r.primary);
        w.active.fg_stroke = Stroke::new(1.0, r.on_secondary_container);
        w.active.corner_radius = radius;
    }
    w.open = w.active;
    v
}

pub fn shadow(color: Color32, blur: u8, alpha: f32) -> egui::Shadow {
    egui::Shadow {
        offset: [0, 4],
        blur,
        spread: 0,
        color: Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), (alpha * 255.0) as u8),
    }
}

/// Medium-weight text, for headers and labels that need emphasis.
/// A slider with its value beside it for typing, and a label. The rail is
/// drawn here, rather than by egui, so that the fill ends exactly under the
/// handle and the handle can be dragged to both ends. Returns whether the
/// value changed. With `decimals` 0 the value is a whole number.
pub fn slider(ui: &mut egui::Ui, value: &mut f64, range: std::ops::RangeInclusive<f64>, decimals: usize, label: &str) -> bool {
    let r = roles(ui.ctx());
    let (lo, hi) = (*range.start(), *range.end());
    let scale = density(ui.ctx()).font_scale();
    let carbon = skin(ui.ctx()) == Skin::Carbon;
    let (handle_r, rail_h) = if carbon { (7.0 * scale, 2.0) } else { (8.0 * scale, 4.0) };
    let mut changed = false;
    ui.horizontal(|ui| {
        let width = ui.spacing().slider_width;
        let height = (handle_r * 2.0 + 6.0).max(ui.spacing().interact_size.y);
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click_and_drag());
        let (x0, x1) = (rect.left() + handle_r, rect.right() - handle_r);
        if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
            let t = (((p.x - x0) / (x1 - x0)) as f64).clamp(0.0, 1.0);
            let mut v = lo + t * (hi - lo);
            if decimals == 0 {
                v = v.round();
            }
            if v != *value {
                *value = v;
                changed = true;
            }
        }
        let t = ((*value - lo) / (hi - lo)).clamp(0.0, 1.0) as f32;
        let (cx, cy) = (x0 + t * (x1 - x0), rect.center().y);
        let rail = egui::Rect::from_min_max(egui::pos2(rect.left(), cy - rail_h / 2.0), egui::pos2(rect.right(), cy + rail_h / 2.0));
        let painter = ui.painter();
        // Carbon draws its slider in the text colour rather than the accent.
        let (rail_colour, fill_colour) = if carbon { (r.outline_variant, r.on_surface) } else { (r.outline_variant, r.primary) };
        painter.rect_filled(rail, rail_h / 2.0, rail_colour);
        painter.rect_filled(egui::Rect::from_min_max(rail.min, egui::pos2(cx, rail.max.y)), rail_h / 2.0, fill_colour);
        let grow = if resp.dragged() { 2.0 } else if resp.hovered() { 1.0 } else { 0.0 };
        painter.circle_filled(egui::pos2(cx, cy), handle_r + grow, fill_colour);
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let mut v = *value;
        let speed = (hi - lo) / 200.0;
        if field(ui, egui::DragValue::new(&mut v).range(lo..=hi).speed(speed).fixed_decimals(decimals)).changed() {
            *value = v;
            changed = true;
        }
        ui.label(label);
    });
    changed
}

/// [`slider`] for an `f32`.
pub fn slider_f32(ui: &mut egui::Ui, value: &mut f32, range: std::ops::RangeInclusive<f32>, decimals: usize, label: &str) -> bool {
    let mut v = *value as f64;
    let changed = slider(ui, &mut v, *range.start() as f64..=*range.end() as f64, decimals, label);
    if changed {
        *value = v as f32;
    }
    changed
}

/// A heading inside a menu or panel, in the accent colour.
pub fn heading(ui: &egui::Ui, text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).text_style(heading_style()).color(roles(ui.ctx()).link)
}

/// Text in the medium weight.
pub fn medium(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).text_style(medium_style())
}

/// Material button kinds: filled for the main action, tonal for secondary
/// ones, outlined for lower emphasis, text for the least (cancel, dismiss).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    Filled,
    Tonal,
    Outlined,
    Text,
}

/// How a pill (a button or a chip) looks in each state.
struct PillStyle {
    rest: Color32,
    hover: Color32,
    press: Color32,
    fg: Color32,
    stroke: Stroke,
    /// Text colour while hovered or pressed, where it changes (Carbon's tertiary button).
    hover_fg: Option<Color32>,
    /// A 2 px line along the bottom edge (Carbon's selected state).
    underline: Option<Color32>,
    /// Carbon's buttons: the label on the left and the icon on the right, rather than centred.
    icon_right: bool,
}

/// Vertical nudges, in points, that put the middle of the icon glyph and of
/// the label's capital letters on the pill's centre line. Icon fonts and text
/// fonts place their glyphs differently inside a line, so these were measured
/// from screenshots rather than left to the layout.
const ICON_NUDGE: f32 = -1.2;
const TEXT_NUDGE: f32 = 0.8;

/// Draw a pill: a rounded background with an optional leading icon and a
/// label, both centred. Greys out when the `Ui` is disabled.
/// One pill split down the middle by a divider into two buttons: the left has an
/// icon and a label, the right a label. Returns the left and right responses.
pub fn split_button(ui: &mut egui::Ui, kind: ButtonKind, icon: egui_material_icons::MaterialIcon, left: &str, right: &str) -> (egui::Response, egui::Response) {
    let r = roles(ui.ctx());
    let (fill, fg) = if skin(ui.ctx()) == Skin::Carbon {
        let (fill, _, _, fg, _) = carbon_button(if kind == ButtonKind::Filled { ButtonKind::Filled } else { ButtonKind::Tonal }, ui.ctx().theme() == egui::Theme::Dark, r);
        (fill, fg)
    } else {
        match kind {
            ButtonKind::Filled => (r.primary, r.on_primary),
            _ => (r.secondary_container, r.on_secondary_container),
        }
    };
    let height = density(ui.ctx()).button_height();
    let scale = density(ui.ctx()).font_scale();
    let font = TextStyle::Button.resolve(ui.style());
    let (pad, gap, icon_size) = ((height / 2.0).clamp(14.0, 20.0), 8.0 * scale, 20.0 * scale);
    let left_text = ui.painter().layout_no_wrap(left.to_string(), font.clone(), fg);
    let right_text = ui.painter().layout_no_wrap(right.to_string(), font, fg);
    let picture = icon_texture(ui.ctx(), icon);
    let icon_galley = ui.painter().layout_no_wrap(icon.codepoint.to_string(), FontId::new(icon_size, icon.font_family()), fg);
    let icon_slot = if picture.is_some() { icon_size } else { icon_galley.size().x };
    let left_w = 2.0 * pad + icon_slot + gap + left_text.size().x;
    let right_w = 2.0 * pad + right_text.size().x;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(left_w + right_w, height), egui::Sense::hover());
    let split = rect.left() + left_w;
    let (left_rect, right_rect) = (egui::Rect::from_min_max(rect.min, egui::pos2(split, rect.max.y)), egui::Rect::from_min_max(egui::pos2(split, rect.min.y), rect.max));
    let base = ui.scope_id().with(("split_button", left, right));
    let left_resp = ui.interact(left_rect, base.with("left"), egui::Sense::click());
    let right_resp = ui.interact(right_rect, base.with("right"), egui::Sense::click());
    let radius = radius(ui.ctx(), Shape::Pill, height);
    let painter = ui.painter();
    painter.rect_filled(rect, radius, fill);
    // The pressed or hovered half is shaded, with the pill's rounding on its outer corner only.
    let shade = |resp: &egui::Response, rect: egui::Rect, left_side: bool| {
        let opacity = if resp.is_pointer_button_down_on() { 0.12 } else if resp.hovered() { 0.08 } else { return };
        let rounding = if left_side { egui::CornerRadius { nw: radius, sw: radius, ne: 0, se: 0 } } else { egui::CornerRadius { ne: radius, se: radius, nw: 0, sw: 0 } };
        painter.rect_filled(rect, rounding, state_layer(Color32::TRANSPARENT, fg, opacity));
    };
    shade(&left_resp, left_rect, true);
    shade(&right_resp, right_rect, false);
    painter.line_segment([egui::pos2(split, rect.top() + height * 0.25), egui::pos2(split, rect.bottom() - height * 0.25)], Stroke::new(1.0, fg.gamma_multiply(0.4)));
    // Contents, placed as in a single pill.
    let mut x = left_rect.left() + pad;
    if let Some(tex) = &picture {
        let img_rect = egui::Rect::from_center_size(egui::pos2(x + icon_size / 2.0, rect.center().y), egui::Vec2::splat(icon_size));
        painter.image(tex.id(), img_rect, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), fg);
    } else {
        let ink = ink_centre_y(&icon_galley).unwrap_or(icon_galley.size().y / 2.0 - ICON_NUDGE);
        painter.galley(egui::pos2(x, rect.center().y - ink), icon_galley, fg);
    }
    x += icon_slot + gap;
    painter.galley(egui::pos2(x, rect.center().y - left_text.size().y / 2.0 + TEXT_NUDGE), left_text, fg);
    painter.galley(egui::pos2(right_rect.left() + pad, rect.center().y - right_text.size().y / 2.0 + TEXT_NUDGE), right_text, fg);
    left_resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, left));
    right_resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, right));
    (left_resp, right_resp)
}

/// Where, from the top of the galley, the ink of its first glyph is centred
/// vertically, when known.
fn ink_centre_y(g: &egui::Galley) -> Option<f32> {
    let row = g.rows.first()?;
    let glyph = row.row.glyphs.first()?;
    Some(row.pos.y + glyph.pos.y + glyph.uv_rect.offset.y + glyph.uv_rect.size.y / 2.0)
}

fn pill(ui: &mut egui::Ui, style: PillStyle, icon: Option<egui_material_icons::MaterialIcon>, label: &str) -> egui::Response {
    let r = roles(ui.ctx());
    let height = density(ui.ctx()).button_height();
    let font = TextStyle::Button.resolve(ui.style());
    let text = ui.painter().layout_no_wrap(label.to_string(), font, style.fg);
    let scale = density(ui.ctx()).font_scale();
    let (pad, gap, icon_size) = ((height / 2.0).clamp(14.0, 20.0), 8.0 * scale, 20.0 * scale);
    let picture = icon.and_then(|i| icon_texture(ui.ctx(), i));
    let icon_galley = icon.filter(|_| picture.is_none()).map(|i| ui.painter().layout_no_wrap(i.codepoint.to_string(), FontId::new(icon_size, i.font_family()), style.fg));
    let icon_w = icon_galley.as_ref().map_or(0.0, |g| g.size().x + gap) + if picture.is_some() { icon_size + gap } else { 0.0 };
    let content_w = icon_w + text.size().x;
    // Carbon: 16 px in from each edge, and room between the label and an icon at the right.
    let pad = if style.icon_right { 16.0 } else { pad };
    let right_icon = style.icon_right && icon.is_some();
    let width = if right_icon { pad + text.size().x + 2.0 * gap + icon_size + pad } else { content_w + 2.0 * pad };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width.max(if style.icon_right { 96.0 } else { 64.0 }), height), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), label));
    let radius = radius(ui.ctx(), Shape::Pill, height);
    let hot = resp.is_pointer_button_down_on() || resp.hovered();
    let fill = if resp.is_pointer_button_down_on() {
        style.press
    } else if resp.hovered() {
        style.hover
    } else {
        style.rest
    };
    let painter = ui.painter();
    painter.rect(rect, radius, fill, style.stroke, egui::StrokeKind::Inside);
    if resp.has_focus() {
        painter.rect_stroke(rect.expand(2.0), radius + 2, Stroke::new(2.0, r.primary), egui::StrokeKind::Outside);
    }
    if right_icon {
        // Label on the left, icon on the right.
        let colour = if hot { style.hover_fg.unwrap_or(style.fg) } else { style.fg };
        painter.galley_with_override_text_color(egui::pos2(rect.left() + pad, rect.center().y - text.size().y / 2.0 + TEXT_NUDGE), text, colour);
        let centre = egui::pos2(rect.right() - pad - icon_size / 2.0, rect.center().y);
        if let Some(tex) = &picture {
            painter.image(tex.id(), egui::Rect::from_center_size(centre, egui::Vec2::splat(icon_size)), egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), colour);
        } else if let Some(g) = icon_galley {
            let ink = ink_centre_y(&g).unwrap_or(g.size().y / 2.0 - ICON_NUDGE);
            painter.galley_with_override_text_color(egui::pos2(centre.x - g.size().x / 2.0, centre.y - ink), g, colour);
        }
        if let Some(line) = style.underline {
            painter.rect_filled(egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 2.0), rect.right_bottom()), 0.0, line);
        }
        return resp;
    }
    if style.icon_right {
        // No icon: just the label, from the left edge.
        let colour = if hot { style.hover_fg.unwrap_or(style.fg) } else { style.fg };
        painter.galley_with_override_text_color(egui::pos2(rect.left() + pad, rect.center().y - text.size().y / 2.0 + TEXT_NUDGE), text, colour);
        return resp;
    }
    let mut x = rect.center().x - content_w / 2.0;
    if let Some(tex) = &picture {
        let img_rect = egui::Rect::from_center_size(egui::pos2(x + icon_size / 2.0, rect.center().y), egui::Vec2::splat(icon_size));
        painter.image(tex.id(), img_rect, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), if hot { style.hover_fg.unwrap_or(style.fg) } else { style.fg });
        x += icon_size + gap;
    }
    if let Some(g) = icon_galley {
        let w = g.size().x;
        // Icons differ in how far up the em box they sit, so each is centred on its own ink.
        let ink = ink_centre_y(&g).unwrap_or(g.size().y / 2.0 - ICON_NUDGE);
        painter.galley_with_override_text_color(egui::pos2(x, rect.center().y - ink), g, if hot { style.hover_fg.unwrap_or(style.fg) } else { style.fg });
        x += w + gap;
    }
    painter.galley_with_override_text_color(egui::pos2(x, rect.center().y - text.size().y / 2.0 + TEXT_NUDGE), text, if hot { style.hover_fg.unwrap_or(style.fg) } else { style.fg });
    if let Some(line) = style.underline {
        painter.rect_filled(egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 2.0), rect.right_bottom()), 0.0, line);
    }
    resp
}

/// Carbon's button colours: (fill, hover fill, press fill, text, text on hover), from its
/// primary, secondary, tertiary and ghost buttons.
fn carbon_button(kind: ButtonKind, dark: bool, r: &Roles) -> (Color32, Color32, Color32, Color32, Option<Color32>) {
    let rgb = |hex: u32| Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8);
    let white = Color32::WHITE;
    match kind {
        ButtonKind::Filled => (rgb(0x0f62fe), rgb(0x0050e6), rgb(0x002d9c), white, None),
        ButtonKind::Tonal if dark => (rgb(0x6f6f6f), rgb(0x606060), rgb(0x393939), white, None),
        ButtonKind::Tonal => (rgb(0x393939), rgb(0x4c4c4c), rgb(0x6f6f6f), white, None),
        // Tertiary: an outline in the text colour that fills on hover.
        ButtonKind::Outlined if dark => (Color32::TRANSPARENT, rgb(0xf4f4f4), rgb(0xc6c6c6), white, Some(rgb(0x161616))),
        ButtonKind::Outlined => (Color32::TRANSPARENT, rgb(0x0f62fe), rgb(0x002d9c), rgb(0x0f62fe), Some(white)),
        // Ghost: just text until hovered.
        ButtonKind::Text if dark => (Color32::TRANSPARENT, rgb(0x333333), rgb(0x393939), r.link, None),
        ButtonKind::Text => (Color32::TRANSPARENT, rgb(0xe8e8e8), rgb(0xe0e0e0), r.link, None),
    }
}

/// A button, optionally with a leading icon: a pill in Material, a rectangle in
/// Carbon. Greys out when the `Ui` is disabled (wrap in `add_enabled_ui`).
pub fn button(ui: &mut egui::Ui, kind: ButtonKind, icon: Option<egui_material_icons::MaterialIcon>, label: &str) -> egui::Response {
    let r = roles(ui.ctx());
    if skin(ui.ctx()) == Skin::Carbon {
        let (fill, hover, press, fg, hover_fg) = carbon_button(kind, ui.ctx().theme() == egui::Theme::Dark, r);
        let stroke = if kind == ButtonKind::Outlined { Stroke::new(1.0, fg) } else { Stroke::NONE };
        return pill(ui, PillStyle { rest: fill, hover, press, fg, stroke, hover_fg, underline: None, icon_right: true }, icon, label);
    }
    let (fill, fg, outline) = match kind {
        ButtonKind::Filled => (r.primary, r.on_primary, None),
        ButtonKind::Tonal => (r.secondary_container, r.on_secondary_container, None),
        ButtonKind::Outlined => (Color32::TRANSPARENT, r.link, Some(r.outline)),
        ButtonKind::Text => (Color32::TRANSPARENT, r.link, None),
    };
    let layer = |opacity: f32| match kind {
        ButtonKind::Filled | ButtonKind::Tonal => state_layer(fill, fg, opacity),
        ButtonKind::Outlined | ButtonKind::Text => r.primary.gamma_multiply(opacity),
    };
    let stroke = outline.map_or(Stroke::NONE, |c| Stroke::new(1.0, c));
    pill(ui, PillStyle { rest: fill, hover: layer(0.08), press: layer(0.12), fg, stroke, hover_fg: None, underline: None, icon_right: false }, icon, label)
}

/// A chip, shaped and sized like the buttons: outlined at rest, filled when selected
/// (with a check mark in Material, an underline in Carbon). Use a row of them for a
/// choice among a few options.
pub fn chip(ui: &mut egui::Ui, selected: bool, icon: Option<egui_material_icons::MaterialIcon>, label: &str) -> egui::Response {
    let r = roles(ui.ctx());
    if skin(ui.ctx()) == Skin::Carbon {
        let dark = ui.ctx().theme() == egui::Theme::Dark;
        let hover = if dark { Color32::from_rgb(0x33, 0x33, 0x33) } else { Color32::from_rgb(0xe8, 0xe8, 0xe8) };
        let (fill, fg, stroke) = if selected {
            (r.secondary_container, r.on_surface, Stroke::NONE)
        } else {
            (Color32::TRANSPARENT, r.on_surface_variant, Stroke::new(1.0, r.outline))
        };
        let underline = selected.then_some(r.link);
        return pill(ui, PillStyle { rest: fill, hover, press: r.secondary_container, fg, stroke, hover_fg: None, underline, icon_right: false }, icon, label);
    }
    let (fill, fg, stroke) = if selected {
        (r.secondary_container, r.on_secondary_container, Stroke::NONE)
    } else {
        (Color32::TRANSPARENT, r.on_surface_variant, Stroke::new(1.0, r.outline))
    };
    let layer = |opacity: f32| if selected { state_layer(fill, fg, opacity) } else { r.on_surface.gamma_multiply(opacity) };
    let icon = if selected { Some(egui_material_icons::icons::ICON_CHECK) } else { icon };
    pill(ui, PillStyle { rest: fill, hover: layer(0.08), press: layer(0.12), fg, stroke, hover_fg: None, underline: None, icon_right: false }, icon, label)
}

/// A menu entry with a check mark column that is invisible when unchecked, so
/// every row has the identical layout whatever is selected.
pub fn check_label(ui: &egui::Ui, checked: bool, label: &str) -> egui::text::LayoutJob {
    let color = ui.visuals().text_color();
    let check = if checked { color } else { Color32::TRANSPARENT };
    let icon = egui_material_icons::icons::ICON_CHECK;
    let mut job = egui::text::LayoutJob::default();
    job.append(icon.codepoint, 0.0, centred(FontId::new(20.0, icon.font_family()), check));
    job.append(label, 12.0, centred(TextStyle::Body.resolve(ui.style()), color));
    job
}

/// Text with an optional leading Material icon, for menu entries. With no icon
/// the label is indented by the icon's width so a menu's labels line up.
pub fn icon_label(ui: &egui::Ui, icon: Option<egui_material_icons::MaterialIcon>, label: &str) -> egui::text::LayoutJob {
    let color = ui.visuals().text_color();
    let body = TextStyle::Body.resolve(ui.style());
    let mut job = egui::text::LayoutJob::default();
    match icon {
        Some(icon) => {
            job.append(icon.codepoint, 0.0, centred(FontId::new(20.0, icon.font_family()), color));
            job.append(label, 12.0, centred(body, color));
        }
        None => job.append(label, 32.0, centred(body, color)),
    }
    job
}

/// A Material snackbar: a short message near the bottom of the window.
/// Returns true when the user clicks it away.
pub fn snackbar(ctx: &egui::Context, text: &str, age: f32, hold: f32) -> bool {
    if skin(ctx) == Skin::Carbon {
        return toast(ctx, text, age, hold);
    }
    let r = roles(ctx);
    egui::Area::new(egui::Id::unique("snackbar"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -48.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let frame = egui::Frame::new()
                .fill(r.inverse_surface)
                .corner_radius(radius(ctx, Shape::Row, 0.0))
                .inner_margin(egui::Margin::symmetric(20, 14))
                .shadow(shadow(r.shadow, 14, 0.25));
            let shown = frame.show(ui, |ui| {
                ui.set_max_width(520.0);
                ui.label(egui::RichText::new(text).color(r.inverse_on_surface));
            });
            ui.interact(shown.response.rect, egui::Id::unique("snackbar-dismiss"), egui::Sense::click()).clicked()
        })
        .inner
}

/// How long Carbon's toast takes to slide in or out, in seconds.
pub const TOAST_SLIDE: f32 = 0.3;

/// What a message is about, for the colour of Carbon's toast: guessed from its wording.
fn toast_kind(text: &str) -> (&'static str, Color32) {
    let lower = text.to_lowercase();
    if ["could not", "failed", "error", "cannot", "no survey"].iter().any(|w| lower.contains(w)) {
        ("Error", Color32::from_rgb(0xda, 0x1e, 0x28))
    } else if ["saved", "added", "set.", "opened", "installed", "up to date"].iter().any(|w| lower.contains(w)) {
        ("Success", Color32::from_rgb(0x24, 0xa1, 0x48))
    } else {
        ("Notice", Color32::from_rgb(0x45, 0x89, 0xff))
    }
}

/// Carbon's toast notification: bottom right, an inverse-coloured box with a 3 px bar in the
/// colour of its kind, a title, the message and a close button. Returns true when dismissed.
fn toast(ctx: &egui::Context, text: &str, age: f32, hold: f32) -> bool {
    // Slides in from the right, waits, slides back out.
    let t = if age < hold { (age / TOAST_SLIDE).min(1.0) } else { 1.0 - ((age - hold) / TOAST_SLIDE).min(1.0) };
    let shift = (1.0 - t).powi(3) * 340.0;
    let r = roles(ctx);
    let (title, accent) = toast_kind(text);
    egui::Area::new(egui::Id::unique("toast"))
        .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0 + shift, -40.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let frame = egui::Frame::new()
                .fill(r.inverse_surface)
                .inner_margin(egui::Margin { left: 19, right: 12, top: 14, bottom: 14 })
                .shadow(shadow(r.shadow, 10, 0.30));
            let shown = frame.show(ui, |ui| {
                ui.set_width(300.0);
                let mut close = false;
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(252.0);
                        ui.label(medium(title).color(r.inverse_on_surface));
                        ui.add_space(2.0);
                        ui.label(egui::RichText::new(text).color(r.inverse_on_surface));
                    });
                    let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(24.0), egui::Sense::click());
                    if resp.hovered() {
                        ui.painter().rect_filled(rect, 0.0, r.inverse_on_surface.gamma_multiply(0.12));
                    }
                    paint_icon(ui.painter(), ui.ctx(), egui_material_icons::icons::ICON_CLOSE, rect.center(), 18.0, r.inverse_on_surface);
                    close = resp.on_hover_text("Close").clicked();
                });
                close
            });
            let rect = shown.response.rect;
            ui.painter().rect_filled(egui::Rect::from_min_max(rect.left_top(), egui::pos2(rect.left() + 3.0, rect.bottom())), 0.0, accent);
            shown.inner
        })
        .inner
}

fn hot_id() -> egui::Id {
    egui::Id::NULL.with("table-hot")
}

/// One cell of a [`data_table`]: in Carbon's rows the content is centred in the row's height.
pub fn cell<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    if skin(ui.ctx()) != Skin::Carbon {
        return add(ui);
    }
    let row_h = density(ui.ctx()).row_height();
    // The row under the pointer (as of the last frame) is light, so its text goes dark.
    let hot: Option<(f32, f32)> = ui.data(|d| d.get_temp(hot_id()));
    ui.horizontal(|ui| {
        ui.set_min_height(row_h);
        if let Some((top, bottom)) = hot
            && (top - 1.0..bottom - 1.0).contains(&ui.cursor().top())
        {
            ui.visuals_mut().override_text_color = Some(Color32::from_rgb(0x16, 0x16, 0x16));
        }
        add(ui)
    })
    .inner
}

/// A table with a header row, then whatever `body` lays out (it must `end_row` after each row, and
/// use [`cell`] for each cell). Material's is a striped grid. Carbon's has a tinted header,
/// rows on a layer with thin dividers, and a highlight under the pointer.
pub fn data_table(ui: &mut egui::Ui, id: &str, header: &[&str], gap: f32, body: impl FnOnce(&mut egui::Ui)) {
    let r = roles(ui.ctx());
    if skin(ui.ctx()) != Skin::Carbon {
        egui::Grid::new(id).striped(true).num_columns(header.len()).spacing([gap, ui.spacing().item_spacing.y * 0.5]).show(ui, |ui| {
            for h in header {
                ui.label(medium(*h).color(r.on_surface_variant));
            }
            ui.end_row();
            body(ui);
        });
        return;
    }
    let dark = ui.ctx().theme() == egui::Theme::Dark;
    let (accent, hover, divider) = if dark {
        (Color32::from_rgb(0x39, 0x39, 0x39), Color32::from_rgb(0x33, 0x33, 0x33), Color32::from_rgb(0x39, 0x39, 0x39))
    } else {
        (Color32::from_rgb(0xe0, 0xe0, 0xe0), Color32::from_rgb(0xe8, 0xe8, 0xe8), Color32::from_rgb(0xe0, 0xe0, 0xe0))
    };
    let row_h = density(ui.ctx()).row_height();
    let behind = ui.painter().add(egui::Shape::Noop);
    let table_key = hot_id().with(id);
    let hot: Option<(f32, f32)> = ui.data(|d| d.get_temp(table_key));
    ui.data_mut(|d| match hot {
        Some(h) => { d.insert_temp(hot_id(), h); }
        None => { d.remove_temp::<(f32, f32)>(hot_id()); }
    });
    let shown = ui.horizontal_top(|ui| {
        ui.add_space(12.0);
        egui::Grid::new(id).striped(false).min_row_height(row_h).spacing([gap + 16.0, 0.0]).show(ui, |ui| {
            for h in header {
                cell(ui, |ui| ui.label(medium(*h).color(r.on_surface)));
            }
            ui.end_row();
            body(ui);
        })
    });
    let shown = shown.inner;
    // The header band, the layer under the rows, the dividers and the hovered row, drawn behind the cells.
    ui.data_mut(|d| d.remove_temp::<(f32, f32)>(hot_id()));
    let grid = shown.response.rect;
    let right = (grid.right() + 12.0).max(ui.clip_rect().right());
    let rect = egui::Rect::from_min_max(grid.min - egui::vec2(12.0, 0.0), egui::pos2(right, grid.bottom()));
    let rows = (rect.height() / row_h).round().max(1.0) as usize;
    let mut shapes = vec![egui::Shape::rect_filled(rect, 0.0, r.surface_container)];
    shapes.push(egui::Shape::rect_filled(egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), row_h)), 0.0, accent));
    // Every second row a shade off the layer.
    let zebra = r.surface_container.lerp_to_gamma(hover, 0.6);
    let mut hot_now = None;
    for row in (2..rows).step_by(2) {
        let top = rect.top() + row as f32 * row_h;
        shapes.push(egui::Shape::rect_filled(egui::Rect::from_min_size(egui::pos2(rect.left(), top), egui::vec2(rect.width(), row_h)), 0.0, zebra));
    }
    // Only when the pointer is really over the table, not over a dialog or menu above it.
    if let Some(p) = ui.input(|i| i.pointer.hover_pos()).filter(|_| ui.rect_contains_pointer(rect)) {
        let row = ((p.y - rect.top()) / row_h) as usize;
        if row >= 1 && row < rows {
            let top = rect.top() + row as f32 * row_h;
            shapes.push(egui::Shape::rect_filled(egui::Rect::from_min_size(egui::pos2(rect.left(), top), egui::vec2(rect.width(), row_h)), 0.0, Color32::from_rgb(0xc6, 0xc6, 0xc6)));
            hot_now = Some((top, top + row_h));
        }
    }
    if hot_now != hot {
        ui.data_mut(|d| match hot_now {
            Some(h) => {
                d.insert_temp(table_key, h);
            }
            None => {
                d.remove_temp::<(f32, f32)>(table_key);
            }
        });
        ui.ctx().request_repaint();
    }
    for i in 1..=rows {
        let y = rect.top() + i as f32 * row_h - 0.5;
        shapes.push(egui::Shape::line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], Stroke::new(1.0, divider)));
    }
    ui.painter().set(behind, egui::Shape::Vec(shapes));
}

/// A centred empty state: a large icon, a headline, a sentence, and actions.
pub fn empty_state(
    ui: &mut egui::Ui,
    icon: egui_material_icons::MaterialIcon,
    headline: &str,
    body: &str,
    actions: impl FnOnce(&mut egui::Ui),
) {
    let r = roles(ui.ctx());
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.25).max(24.0));
        match icon_image(ui.ctx(), icon, 64.0, r.outline) {
            Some(img) => {
                ui.add(img);
            }
            None => {
                ui.label(icon.rich_text().size(64.0).color(r.outline));
            }
        }
        ui.add_space(8.0);
        ui.label(egui::RichText::new(headline).size(22.0).color(r.on_surface));
        ui.add_space(4.0);
        ui.label(egui::RichText::new(body).color(r.on_surface_variant));
        ui.add_space(16.0);
        actions(ui);
    });
}

/// A switch: Material's 52 x 32 track with a thumb that grows when on, or Carbon's
/// 48 x 24 toggle, green when on.
pub fn switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let r = roles(ui.ctx());
    let carbon = skin(ui.ctx()) == Skin::Carbon;
    let size = if carbon { egui::vec2(48.0, 24.0) } else { egui::vec2(52.0, 32.0) };
    let (rect, mut resp) = ui.allocate_exact_size(size, egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let fade = if ui.is_enabled() { 1.0 } else { 0.38 };
    let lerp = |a: Color32, b: Color32| a.lerp_to_gamma(b, t).gamma_multiply(fade);
    let painter = ui.painter();
    if carbon {
        let off = if ui.ctx().theme() == egui::Theme::Dark { Color32::from_rgb(0x6f, 0x6f, 0x6f) } else { Color32::from_rgb(0x8d, 0x8d, 0x8d) };
        painter.rect_filled(rect, 12.0, lerp(off, Color32::from_rgb(0x24, 0xa1, 0x48)));
        let x = egui::lerp((rect.left() + 12.0)..=(rect.right() - 12.0), t);
        painter.circle_filled(egui::pos2(x, rect.center().y), 9.0, Color32::WHITE.gamma_multiply(fade));
        return resp;
    }
    painter.rect_filled(rect, 16.0, lerp(r.surface_container_highest, r.primary));
    if t < 1.0 {
        painter.rect_stroke(rect, 16.0, Stroke::new(2.0 * (1.0 - t), r.outline.gamma_multiply(fade)), egui::StrokeKind::Inside);
    }
    let pressed = resp.is_pointer_button_down_on();
    let radius = egui::lerp(8.0..=12.0, t) + if pressed { 2.0 } else { 0.0 };
    let x = egui::lerp((rect.left() + 16.0)..=(rect.right() - 16.0), t);
    let centre = egui::pos2(x, rect.center().y);
    if resp.hovered() || pressed {
        painter.circle_filled(centre, 20.0, lerp(r.on_surface, r.primary).gamma_multiply(if pressed { 0.12 } else { 0.08 }));
    }
    painter.circle_filled(centre, radius, lerp(r.outline, r.on_primary));
    resp
}

/// A switch with its label beside it; clicking the label toggles too.
pub fn switch_row(ui: &mut egui::Ui, on: &mut bool, label: &str) -> egui::Response {
    ui.horizontal(|ui| {
        let mut resp = switch(ui, on);
        let text = ui.add(egui::Label::new(label).sense(egui::Sense::click()));
        if text.clicked() {
            *on = !*on;
            resp.mark_changed();
        }
        resp
    })
    .inner
}

/// An outlined Material card.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    let r = roles(ui.ctx());
    egui::Frame::new()
        .fill(r.surface_container_low)
        .stroke(Stroke::new(1.0, r.outline_variant))
        .corner_radius(radius(ui.ctx(), Shape::Card, 0.0))
        .inner_margin(16)
        .show(ui, add)
}

/// The frame for a page's content: Material's 16 dp margins.
pub fn page_frame(ctx: &egui::Context) -> egui::Frame {
    egui::Frame::new().fill(roles(ctx).surface).inner_margin(16)
}

/// A Material dialog: centred, 28 dp corners, a headline, then the content.
/// Returns what the content returned while it is open.
pub fn dialog<R>(ctx: &egui::Context, title: &str, width: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<Option<R>> {
    let r = roles(ctx);
    // Never wider than the window it sits in.
    let width = width.min((ctx.content_rect().width() - 80.0).max(280.0));
    let frame = egui::Frame::new()
        .fill(r.surface_container_high)
        .stroke(if skin(ctx) == Skin::Carbon { Stroke::new(1.0, r.outline_variant) } else { Stroke::NONE })
        .corner_radius(radius(ctx, Shape::Dialog, 0.0))
        .inner_margin(if skin(ctx) == Skin::Carbon { 24 } else { 24 })
        .shadow(shadow(r.shadow, 24, 0.20));
    // A layer over the window that takes the pointer, so nothing behind the dialog reacts to it.
    egui::Area::new(egui::Id::unique("dialog-blocker"))
        .order(egui::Order::Middle)
        .fixed_pos(ctx.content_rect().min)
        .show(ctx, |ui| {
            ui.allocate_rect(ctx.content_rect(), egui::Sense::hover());
        });
    egui::Window::new(title)
        .order(egui::Order::Foreground)
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.label(egui::RichText::new(title).size(24.0));
            ui.add_space(12.0);
            add(ui)
        })
        .expect("a dialog is always open when shown")
}

/// A right-aligned row for a dialog's buttons. Buttons are laid out right to
/// left, so add the main action first and the dismissive one last.
pub fn actions(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(16.0);
    // A fixed-height row: a right-to-left layout would otherwise take all the
    // height left in the window and leave a gap above the buttons.
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), 40.0),
        egui::Layout::right_to_left(egui::Align::Center),
        add,
    );
}

/// A navigation item (as in a navigation drawer): a pill that fills with the
/// secondary container when selected.
pub fn nav_item(ui: &mut egui::Ui, selected: bool, label: &str) -> egui::Response {
    let r = roles(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 48.0), egui::Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, radius(ui.ctx(), Shape::Pill, 48.0), r.secondary_container);
    } else if resp.hovered() || resp.is_pointer_button_down_on() {
        let opacity = if resp.is_pointer_button_down_on() { 0.12 } else { 0.08 };
        painter.rect_filled(rect, radius(ui.ctx(), Shape::Pill, 48.0), r.on_surface.gamma_multiply(opacity));
    }
    let color = if selected { r.on_secondary_container } else { r.on_surface_variant };
    painter.text(
        egui::pos2(rect.left() + 20.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        medium_style().resolve(ui.style()),
        color,
    );
    resp
}

/// A Material "primary tab": medium-weight label, a rounded indicator under
/// the active tab, and state layers on hover and press.
pub fn tab(ui: &mut egui::Ui, selected: bool, label: &str, height: f32) -> egui::Response {
    let r = roles(ui.ctx());
    let font = medium_style().resolve(ui.style());
    let color = if selected { r.link } else { r.on_surface_variant };
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, color);
    let size = egui::vec2(galley.size().x + 40.0, height);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let painter = ui.painter();
    if resp.hovered() || resp.is_pointer_button_down_on() {
        let opacity = if resp.is_pointer_button_down_on() { 0.12 } else { 0.08 };
        let inset = if skin(ui.ctx()) == Skin::Carbon { egui::Vec2::ZERO } else { egui::vec2(4.0, 4.0) };
        painter.rect_filled(rect.shrink2(inset), radius(ui.ctx(), Shape::Card, 0.0), r.on_surface.gamma_multiply(opacity));
    }
    painter.galley(egui::pos2(rect.center().x - galley.size().x / 2.0, rect.center().y - galley.size().y / 2.0), galley, color);
    if selected {
        if skin(ui.ctx()) == Skin::Carbon {
            // Carbon's selected tab: a 2 px line the width of the tab.
            painter.rect_filled(egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 2.0), rect.right_bottom()), 0.0, r.link);
        } else {
            let bar = egui::Rect::from_min_max(egui::pos2(rect.left() + 12.0, rect.bottom() - 3.0), egui::pos2(rect.right() - 12.0, rect.bottom()));
            painter.rect_filled(bar, CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 }, r.primary);
        }
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_seed_gives_a_teal_primary_in_both_themes() {
        let (light, dark) = (roles_for(egui::Theme::Light, Skin::Material), roles_for(egui::Theme::Dark, Skin::Material));
        // Teal: green and blue clearly above red.
        for p in [light.primary, dark.primary] {
            assert!(p.g() > p.r() + 20 && p.b() > p.r() + 20, "{p:?} is not teal");
        }
        assert!(light.surface.r() > 200 && dark.surface.r() < 60, "light is light, dark is dark");
        assert_ne!(light.primary, dark.primary);
    }

    #[test]
    fn text_has_enough_contrast_on_its_surface() {
        fn luminance(c: Color32) -> f32 {
            let f = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
        }
        fn ratio(a: Color32, b: Color32) -> f32 {
            let (la, lb) = (luminance(a), luminance(b));
            (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
        }
        for (theme, skin) in [(egui::Theme::Light, Skin::Material), (egui::Theme::Dark, Skin::Material), (egui::Theme::Light, Skin::Carbon), (egui::Theme::Dark, Skin::Carbon)] {
            let r = roles_for(theme, skin);
            assert!(ratio(r.on_surface, r.surface) >= 7.0, "{theme:?} body text");
            assert!(ratio(r.on_secondary_container, r.secondary_container) >= 4.5, "{theme:?} tonal buttons");
            assert!(ratio(r.on_primary, r.primary) >= 4.5, "{theme:?} filled buttons");
            assert!(ratio(r.link, r.surface) >= 3.0, "{theme:?} tab indicator against the surface");
            assert!(ratio(r.link, r.surface_container_high) >= 4.5, "{theme:?} text buttons in dialogs");
            assert!(ratio(r.link, r.secondary_container) >= 3.0, "{theme:?} text buttons in the update banner");
            assert!(ratio(r.on_surface_variant, r.surface) >= 4.5, "{theme:?} table headers and hints");
            assert!(ratio(r.on_surface_variant, r.surface_container_high) >= 4.5, "{theme:?} hints in dialogs");
            assert!(ratio(r.error, r.surface_container_high) >= 4.5, "{theme:?} error text in dialogs");
            assert!(ratio(r.error, r.surface) >= 4.5, "{theme:?} error text on pages");
            assert!(ratio(r.inverse_on_surface, r.inverse_surface) >= 7.0, "{theme:?} snackbar text");
            assert!(ratio(r.on_surface_variant, r.surface_container) >= 4.5, "{theme:?} status bar text");
        }
    }

    #[test]
    fn state_layers_move_toward_the_layer_colour() {
        let base = Color32::from_rgb(200, 200, 200);
        assert_eq!(state_layer(base, Color32::BLACK, 0.0), base);
        assert_eq!(state_layer(base, Color32::BLACK, 1.0), Color32::BLACK);
        assert!(state_layer(base, Color32::BLACK, 0.08).r() < 200);
    }

    #[test]
    fn density_levels_grow_in_row_height_text_and_gaps() {
        let all = Density::ALL;
        for pair in all.windows(2) {
            assert!(pair[0].row_height() < pair[1].row_height());
            assert!(pair[0].font_scale() < pair[1].font_scale());
            assert!(pair[0].item_gap() < pair[1].item_gap());
            assert!(pair[0].button_height() < pair[1].button_height());
            assert!(pair[0].bar_height() < pair[1].bar_height());
        }
        assert_eq!((Density::Default.row_height(), Density::Default.font_scale()), (40.0, 1.0), "default is unchanged");
        assert_eq!((Density::Default.button_height(), Density::Default.bar_height()), (40.0, 48.0), "default bars are unchanged");
        // Buttons are never taller than the bar they sit in.
        for d in Density::ALL {
            assert!(d.button_height() < d.bar_height());
        }
    }
}

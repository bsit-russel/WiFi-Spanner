// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! A table of scan results you can sort, filter and resize: click a heading to sort by it, click a
//! second one within a few seconds to sort by both, drag the line between headings to resize a
//! column and double-click it to fit the column to what it holds.

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};

use crate::theme::{self, Skin};

/// How long after a click on a heading a click on another heading adds to the sort instead of
/// replacing it.
const STACK_WINDOW: Duration = Duration::from_secs(5);
/// The most headings a sort is made of.
const MAX_TIERS: usize = 2;
/// Room either side of a cell's text.
const PAD: f32 = 10.0;
const MIN_WIDTH: f32 = 36.0;
const MAX_FIT: f32 = 420.0;
/// A press has to stay this still (pixels), and this long (seconds), to light the row.
const HOLD_MOVE: f32 = 8.0;
const HOLD_AFTER: f64 = 0.12;

pub struct Column {
    pub name: &'static str,
    /// The first click sorts the biggest first (signal, say) instead of A to Z.
    pub desc_first: bool,
}

/// One cell: what it shows, how, and what it sorts by.
#[derive(Clone, Default)]
pub struct Cell {
    pub text: String,
    pub colour: Option<Color32>,
    pub strong: bool,
    pub mono: bool,
    /// Sorts by this number when both cells in a column have one.
    pub num: Option<f64>,
}

impl Cell {
    pub fn text(text: impl Into<String>) -> Cell {
        Cell { text: text.into(), ..Cell::default() }
    }

    pub fn number(value: Option<f64>, text: impl Into<String>) -> Cell {
        Cell { text: text.into(), num: value, ..Cell::default() }
    }

    pub fn colour(mut self, c: Color32) -> Cell {
        self.colour = Some(c);
        self
    }

    pub fn strong(mut self) -> Cell {
        self.strong = true;
        self
    }

    pub fn mono(mut self) -> Cell {
        self.mono = true;
        self
    }
}

/// What the user has done to a table: the sort, the filter and the column widths.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Sort {
    /// Column and whether it runs biggest first, the most important first.
    pub keys: Vec<(usize, bool)>,
    last_click: Option<Instant>,
}

impl Sort {
    /// A click on heading `col` at `now`. A click within the window of the last one adds to the
    /// sort (the new heading first, the earlier one second); later it starts a new sort. Clicking
    /// the heading sorted on first again reverses it.
    pub fn click(&mut self, col: usize, desc_first: bool, now: Instant) {
        let stacking = self.last_click.is_some_and(|t| now.duration_since(t) <= STACK_WINDOW);
        self.last_click = Some(now);
        if let Some(first) = self.keys.first_mut()
            && first.0 == col
        {
            first.1 = !first.1;
            if !stacking {
                self.keys.truncate(1);
            }
            return;
        }
        if stacking {
            self.keys.retain(|k| k.0 != col);
            self.keys.insert(0, (col, desc_first));
            self.keys.truncate(MAX_TIERS);
        } else {
            self.keys = vec![(col, desc_first)];
        }
    }
}

/// Order `rows` by the sort, keeping the order they came in where it ties.
pub fn sort_rows(rows: &mut [Vec<Cell>], keys: &[(usize, bool)]) {
    use std::cmp::Ordering;
    rows.sort_by(|a, b| {
        for &(col, desc) in keys {
            let (x, y) = (a.get(col), b.get(col));
            let order = match (x.and_then(|c| c.num), y.and_then(|c| c.num)) {
                (Some(p), Some(q)) => p.partial_cmp(&q).unwrap_or(Ordering::Equal),
                // A number sorts before nothing, whichever way the sort runs.
                (Some(_), None) => return Ordering::Less,
                (None, Some(_)) => return Ordering::Greater,
                (None, None) => {
                    let (p, q) = (x.map_or(String::new(), |c| c.text.to_lowercase()), y.map_or(String::new(), |c| c.text.to_lowercase()));
                    p.cmp(&q)
                }
            };
            let order = if desc { order.reverse() } else { order };
            if order != Ordering::Equal {
                return order;
            }
        }
        Ordering::Equal
    });
}

#[derive(Clone, Default)]
struct State {
    sort: Sort,
    sorted_once: bool,
    filter: String,
    widths: Vec<f32>,
}

fn rich(c: &Cell) -> egui::RichText {
    let mut t = egui::RichText::new(&c.text);
    if c.mono {
        t = t.monospace();
    }
    if c.strong {
        t = t.strong();
    }
    t
}

fn text_width(ui: &egui::Ui, text: egui::RichText) -> f32 {
    egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body).size().x
}

/// The width that fits heading `col` and everything under it.
fn fit(ui: &egui::Ui, cols: &[Column], rows: &[Vec<Cell>], col: usize) -> f32 {
    let head = text_width(ui, theme::medium(cols[col].name)) + 22.0;
    let body = rows.iter().filter_map(|r| r.get(col)).map(|c| text_width(ui, rich(c))).fold(0.0, f32::max);
    (head.max(body) + 2.0 * PAD).clamp(MIN_WIDTH, MAX_FIT)
}

/// The filter box, the headings and the rows. `filter_col` is the column the filter looks in.
/// Scrolls as a whole, with its bar at the right edge of the space it is given.
pub fn show(ui: &mut egui::Ui, id: &str, cols: &[Column], filter_col: Option<usize>, default_sort: &[(usize, bool)], mut rows: Vec<Vec<Cell>>) {
    let key = egui::Id::NULL.with(("sortable-table", id));
    let mut st: State = ui.data(|d| d.get_temp(key)).unwrap_or_default();
    if !st.sorted_once {
        st.sort.keys = default_sort.to_vec();
        st.sorted_once = true;
    }
    if st.widths.len() != cols.len() {
        st.widths = (0..cols.len()).map(|i| fit(ui, cols, &rows, i)).collect();
    }
    let r = theme::roles(ui.ctx());
    let total = rows.len();
    if let Some(fc) = filter_col {
        let needle = st.filter.trim().to_lowercase();
        if !needle.is_empty() {
            rows.retain(|row| row.get(fc).is_some_and(|c| c.text.to_lowercase().contains(&needle)));
        }
    }
    let count = format!("Showing {} of {}", rows.len(), total);
    let mut filter = st.filter.clone();
    sort_rows(&mut rows, &st.sort.keys);

    let carbon = theme::skin(ui.ctx()) == Skin::Carbon;
    let row_h = if carbon { theme::density(ui.ctx()).row_height() } else { 26.0 * theme::density(ui.ctx()).font_scale() };
    let dark = ui.ctx().theme() == egui::Theme::Dark;
    let (head_fill, zebra, hover_fill) = if carbon {
        let (accent, zebra) = if dark { (Color32::from_rgb(0x39, 0x39, 0x39), Color32::from_rgb(0x2c, 0x2c, 0x2c)) } else { (Color32::from_rgb(0xe0, 0xe0, 0xe0), Color32::from_rgb(0xea, 0xea, 0xea)) };
        (accent, zebra, Color32::from_rgb(0xc6, 0xc6, 0xc6))
    } else {
        (Color32::TRANSPARENT, ui.visuals().faint_bg_color, theme::state_layer(Color32::TRANSPARENT, r.on_surface, 0.08))
    };
    let widths = st.widths.clone();
    let table_w: f32 = widths.iter().sum();
    let mut clicked: Option<usize> = None;
    let mut resized: Vec<(usize, f32)> = Vec::new();
    let mut autofit: Vec<usize> = Vec::new();

    egui::ScrollArea::both().id_salt(("table-scroll", id)).auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        // The headings.
        let (head, _) = ui.allocate_exact_size(Vec2::new(table_w.max(ui.available_width()), row_h), Sense::hover());
        ui.painter().rect_filled(head, 0.0, head_fill);
        let mut x = head.left();
        for (i, col) in cols.iter().enumerate() {
            let cell = Rect::from_min_size(Pos2::new(x, head.top()), Vec2::new(widths[i], row_h));
            // The filter symbol sits at the right of its heading; the rest of the heading sorts.
            let icon = (filter_col == Some(i)).then(|| Rect::from_center_size(Pos2::new(cell.right() - 18.0, cell.center().y), Vec2::splat(20.0)));
            let sort_area = Rect::from_min_max(cell.min, Pos2::new(icon.map_or(cell.right(), |r| r.left()) - 4.0, cell.bottom()));
            let resp = ui.interact(sort_area, egui::Id::NULL.with(("head", id, i)), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(cell, 0.0, theme::state_layer(Color32::TRANSPARENT, r.on_surface, 0.08));
            }
            if resp.clicked() {
                clicked = Some(i);
            }
            let tier = st.sort.keys.iter().position(|k| k.0 == i);
            let text_colour = if carbon { r.on_surface } else { r.on_surface_variant };
            let galley = egui::WidgetText::from(theme::medium(col.name).color(text_colour)).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body);
            let text_pos = Pos2::new(cell.left() + PAD, cell.center().y - galley.size().y / 2.0);
            ui.painter().with_clip_rect(sort_area).galley(text_pos, galley.clone(), text_colour);
            if let Some(t) = tier {
                // A small triangle: pointing up for A to Z or smallest first, down for the reverse.
                let desc = st.sort.keys[t].1;
                let c = Pos2::new((text_pos.x + galley.size().x + 12.0).min(cell.right() - 10.0), cell.center().y);
                let s = 4.0;
                let tri = if desc { vec![Pos2::new(c.x - s, c.y - s * 0.6), Pos2::new(c.x + s, c.y - s * 0.6), Pos2::new(c.x, c.y + s * 0.8)] } else { vec![Pos2::new(c.x - s, c.y + s * 0.6), Pos2::new(c.x + s, c.y + s * 0.6), Pos2::new(c.x, c.y - s * 0.8)] };
                ui.painter().add(egui::Shape::convex_polygon(tri, text_colour, Stroke::NONE));
                if st.sort.keys.len() > 1 {
                    ui.painter().text(Pos2::new(c.x + s + 5.0, c.y), egui::Align2::LEFT_CENTER, (t + 1).to_string(), egui::FontId::proportional(10.0), text_colour);
                }
            }
            if let Some(icon) = icon {
                let button = ui.interact(icon, egui::Id::NULL.with(("filter", id)), Sense::click());
                if button.hovered() {
                    ui.painter().rect_filled(icon, 3.0, theme::state_layer(Color32::TRANSPARENT, r.on_surface, 0.12));
                }
                paint_filter(ui.painter(), icon.center(), text_colour, !filter.is_empty());
                let button = button.on_hover_text(if filter.is_empty() { "Filter".to_string() } else { format!("Filtering: {filter}") });
                egui::Popup::menu(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                    ui.set_min_width(220.0);
                    // No ring round the box while typing: the cursor already shows that. The text
                    // sits at the left, centred up and down, over a thin line along the bottom.
                    let edit = ui
                        .scope(|ui| {
                            ui.visuals_mut().selection.stroke = Stroke::NONE;
                            ui.visuals_mut().widgets.hovered.bg_stroke = Stroke::NONE;
                            ui.visuals_mut().widgets.active.bg_stroke = Stroke::NONE;
                            let edit = ui.add(
                                egui::TextEdit::singleline(&mut filter)
                                    .hint_text(format!("Filter {} (for example Byte)", cols[i].name))
                                    .desired_width(210.0)
                                    .align(egui::Align2::LEFT_CENTER)
                                    .margin(egui::Margin::symmetric(8, 0))
                                    .min_size(Vec2::new(210.0, 32.0)),
                            );
                            ui.painter().line_segment([edit.rect.left_bottom() - Vec2::new(0.0, 0.5), edit.rect.right_bottom() - Vec2::new(0.0, 0.5)], Stroke::new(1.0, r.outline));
                            edit
                        })
                        .inner;
                    if !ui.memory(|m| m.has_focus(edit.id)) && filter.is_empty() {
                        edit.request_focus();
                    }
                    ui.horizontal(|ui| {
                        if theme::button(ui, theme::ButtonKind::Text, None, "Clear").clicked() {
                            filter.clear();
                        }
                        ui.label(egui::RichText::new(&count).color(r.on_surface_variant));
                    });
                });
            }
            // The line between this heading and the next: drag to resize, double-click to fit.
            let seam = ui.interact(Rect::from_min_max(Pos2::new(cell.right() - 4.0, cell.top()), Pos2::new(cell.right() + 4.0, cell.bottom())), egui::Id::NULL.with(("seam", id, i)), Sense::click_and_drag());
            if seam.hovered() || seam.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeColumn);
                ui.painter().line_segment([Pos2::new(cell.right(), cell.top() + 4.0), Pos2::new(cell.right(), cell.bottom() - 4.0)], Stroke::new(2.0, r.primary));
            } else {
                ui.painter().line_segment([Pos2::new(cell.right(), cell.top() + 6.0), Pos2::new(cell.right(), cell.bottom() - 6.0)], Stroke::new(1.0, r.outline_variant));
            }
            if seam.double_clicked() {
                autofit.push(i);
            } else if seam.dragged() {
                resized.push((i, seam.drag_delta().x));
            }
            x += widths[i];
        }
        // The rows.
        // A row lights only while it is held: pressed, kept still for a moment, and not yet
        // let go. Moving the pointer over the table, scrolling, or dragging the page with a
        // finger lights nothing.
        let held = ui.input(|i| {
            let (origin, now, start) = (i.pointer.press_origin()?, i.pointer.latest_pos()?, i.pointer.press_start_time()?);
            (i.pointer.primary_down() && origin.distance(now) < HOLD_MOVE).then_some((origin, i.time - start))
        });
        let pressed_on = held.map(|(origin, _)| origin).filter(|o| ui.rect_contains_pointer(ui.max_rect()) || ui.max_rect().contains(*o));
        if let Some((_, since)) = held
            && since < HOLD_AFTER
            && pressed_on.is_some()
        {
            ui.ctx().request_repaint_after(Duration::from_secs_f64(HOLD_AFTER - since + 0.01));
        }
        let held_at = held.filter(|(_, since)| *since >= HOLD_AFTER).and(pressed_on);
        for (n, row) in rows.iter().enumerate() {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(table_w.max(ui.available_width()), row_h), Sense::hover());
            if !ui.is_rect_visible(rect) {
                continue;
            }
            let hot = held_at.is_some_and(|p| rect.contains(p));
            if hot {
                ui.painter().rect_filled(rect, 0.0, hover_fill);
            } else if n % 2 == 1 {
                ui.painter().rect_filled(rect, 0.0, zebra);
            }
            let mut x = rect.left();
            for (i, cell) in row.iter().enumerate().take(cols.len()) {
                let area = Rect::from_min_size(Pos2::new(x, rect.top()), Vec2::new(widths[i], row_h));
                let colour = if carbon && hot { Color32::from_rgb(0x16, 0x16, 0x16) } else { cell.colour.unwrap_or(r.on_surface) };
                let galley = egui::WidgetText::from(rich(cell).color(colour)).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body);
                let pos = Pos2::new(area.left() + PAD, area.center().y - galley.size().y / 2.0);
                ui.painter().with_clip_rect(area.shrink2(Vec2::new(2.0, 0.0))).galley(pos, galley, colour);
                x += widths[i];
            }
        }
        if rows.is_empty() {
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Nothing to show.").color(r.on_surface_variant));
        }
    });

    if filter != st.filter {
        st.filter = filter;
        ui.ctx().request_repaint();
    }
    for (i, dx) in resized {
        st.widths[i] = (st.widths[i] + dx).max(MIN_WIDTH);
    }
    for i in autofit {
        st.widths[i] = fit(ui, cols, &rows, i);
    }
    if let Some(i) = clicked {
        st.sort.click(i, cols[i].desc_first, Instant::now());
    }
    ui.data_mut(|d| d.insert_temp(key, st));
}

/// A funnel: an outline, or filled while a filter is on.
fn paint_filter(painter: &egui::Painter, centre: Pos2, colour: Color32, on: bool) {
    let (w, h, stem) = (7.0, 6.5, 1.6);
    // The wide top and the narrow stem, each convex, so each fills cleanly.
    let top = vec![Pos2::new(centre.x - w, centre.y - h), Pos2::new(centre.x + w, centre.y - h), Pos2::new(centre.x + stem, centre.y + 0.5), Pos2::new(centre.x - stem, centre.y + 0.5)];
    let neck = vec![Pos2::new(centre.x - stem, centre.y + 0.5), Pos2::new(centre.x + stem, centre.y + 0.5), Pos2::new(centre.x + stem, centre.y + h), Pos2::new(centre.x - stem, centre.y + h - 1.5)];
    let stroke = Stroke::new(1.4, colour);
    if on {
        painter.add(egui::Shape::convex_polygon(top, colour, stroke));
        painter.add(egui::Shape::convex_polygon(neck, colour, stroke));
    } else {
        // One outline round both, without the line where they meet.
        let outline = vec![
            Pos2::new(centre.x - w, centre.y - h),
            Pos2::new(centre.x + w, centre.y - h),
            Pos2::new(centre.x + stem, centre.y + 0.5),
            Pos2::new(centre.x + stem, centre.y + h),
            Pos2::new(centre.x - stem, centre.y + h - 1.5),
            Pos2::new(centre.x - stem, centre.y + 0.5),
        ];
        painter.add(egui::Shape::closed_line(outline, stroke));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: u64, base: Instant) -> Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn a_second_click_within_five_seconds_adds_a_tier_and_a_later_one_starts_over() {
        let base = Instant::now();
        let mut s = Sort::default();
        s.click(7, true, base); // signal
        assert_eq!(s.keys, vec![(7, true)]);
        s.click(0, false, t(3, base)); // ssid, within 5 s: ssid first, signal second
        assert_eq!(s.keys, vec![(0, false), (7, true)]);
        s.click(2, false, t(20, base)); // long after: a fresh sort
        assert_eq!(s.keys, vec![(2, false)]);
    }

    #[test]
    fn clicking_the_sorted_heading_again_reverses_it_and_a_third_tier_is_not_kept() {
        let base = Instant::now();
        let mut s = Sort::default();
        s.click(1, false, base);
        s.click(1, false, t(1, base));
        assert_eq!(s.keys, vec![(1, true)]);
        s.click(2, false, t(2, base));
        s.click(3, false, t(3, base));
        assert_eq!(s.keys.len(), 2);
        assert_eq!(s.keys[0].0, 3);
        // The same heading again after the window reverses it and drops the second tier.
        s.click(3, false, t(30, base));
        assert_eq!(s.keys, vec![(3, true)]);
    }

    #[test]
    fn sorting_by_ssid_then_signal_lists_each_name_best_to_worst() {
        let row = |ssid: &str, dbm: f64| vec![Cell::text(ssid), Cell::number(Some(dbm), format!("{dbm} dBm"))];
        let mut rows = vec![row("beta", -70.0), row("alpha", -80.0), row("beta", -50.0), row("alpha", -60.0)];
        sort_rows(&mut rows, &[(0, false), (1, true)]);
        let got: Vec<(String, f64)> = rows.iter().map(|r| (r[0].text.clone(), r[1].num.unwrap())).collect();
        assert_eq!(got, vec![("alpha".into(), -60.0), ("alpha".into(), -80.0), ("beta".into(), -50.0), ("beta".into(), -70.0)]);
    }

    #[test]
    fn missing_numbers_sort_last_either_way() {
        let mut rows = vec![vec![Cell::number(None, "-")], vec![Cell::number(Some(1.0), "1")], vec![Cell::number(Some(5.0), "5")]];
        sort_rows(&mut rows, &[(0, true)]);
        assert_eq!(rows.iter().map(|r| r[0].text.as_str()).collect::<Vec<_>>(), ["5", "1", "-"]);
        sort_rows(&mut rows, &[(0, false)]);
        assert_eq!(rows.iter().map(|r| r[0].text.as_str()).collect::<Vec<_>>(), ["1", "5", "-"]);
    }
}

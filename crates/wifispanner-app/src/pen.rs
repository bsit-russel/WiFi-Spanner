// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! Pen and graphics-tablet input on Wayland.
//!
//! Wayland delivers pens through its own tablet protocol (`tablet-v2`), which
//! winit, and so egui, does not implement: mouse and touch work, a pen does
//! not. This module attaches a second event queue to the connection winit
//! already has (libwayland supports that), asks the compositor for tablet
//! events, and turns them into pointer events for egui.

use std::ffi::c_void;
use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui;
use wayland_backend::client::Backend;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::wp::tablet::zv2::client::{
    zwp_tablet_manager_v2::ZwpTabletManagerV2,
    zwp_tablet_pad_group_v2::{self, ZwpTabletPadGroupV2},
    zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2,
    zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2,
    zwp_tablet_pad_v2::{self, ZwpTabletPadV2},
    zwp_tablet_seat_v2::{self, ZwpTabletSeatV2},
    zwp_tablet_tool_v2::{self, ZwpTabletToolV2},
    zwp_tablet_v2::ZwpTabletV2,
};

/// Linux input codes for the pen's side buttons.
const BTN_STYLUS: u32 = 0x14b;
const BTN_STYLUS2: u32 = 0x14c;

/// What the pen did, in surface (egui) coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PenEvent {
    /// The pen moved (hovering or touching).
    Move(egui::Pos2),
    /// The tip touched or left the surface.
    Tip { down: bool },
    /// A side button: the lower one acts as a right click, the upper as middle.
    Side { button: egui::PointerButton, down: bool },
    /// The pen left the screen's reach.
    Gone,
}

/// Start listening for the pen on the Wayland connection behind `frame`.
/// Returns the stream of events, or `None` when not on Wayland or when the
/// compositor has no tablet support. Everything runs on its own thread.
pub fn start(frame: &eframe::Frame, ctx: egui::Context) -> Option<Receiver<PenEvent>> {
    use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
    let RawDisplayHandle::Wayland(handle) = frame.display_handle().ok()?.as_raw() else {
        return None;
    };
    let display = DisplayPtr(handle.display.as_ptr());
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("pen".into()).spawn(move || {
        if let Err(e) = run(display, tx, ctx) {
            eprintln!("pen: not available ({e})");
        }
    });
    spawned.ok().map(|_| rx)
}

/// The connection's `wl_display`, which outlives the window and this thread.
struct DisplayPtr(*mut c_void);
// SAFETY: libwayland-client's display is thread-safe, and the pointer stays
// valid for as long as the application's window (and so this thread) lives.
unsafe impl Send for DisplayPtr {}

struct State {
    tx: Sender<PenEvent>,
    ctx: egui::Context,
    debug: bool,
}

impl State {
    fn send(&self, event: PenEvent) {
        if self.debug {
            eprintln!("pen: {event:?}");
        }
        let _ = self.tx.send(event);
        self.ctx.request_repaint();
    }
}

fn run(display: DisplayPtr, tx: Sender<PenEvent>, ctx: egui::Context) -> Result<(), String> {
    // SAFETY: the pointer is the live wl_display winit connected with; the
    // backend creates its own event queue so it never steals winit's events.
    let backend = unsafe { Backend::from_foreign_display(display.0.cast()) };
    let conn = Connection::from_backend(backend);
    let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ()).map_err(|e| format!("no seat: {e}"))?;
    let manager: ZwpTabletManagerV2 = globals.bind(&qh, 1..=1, ()).map_err(|_| "the compositor has no tablet support".to_string())?;
    let _tablet_seat = manager.get_tablet_seat(&seat, &qh, ());
    let mut state = State { tx, ctx, debug: std::env::var_os("WS_PEN_DEBUG").is_some() };
    if state.debug {
        eprintln!("pen: listening for tablet events");
    }
    loop {
        queue.blocking_dispatch(&mut state).map_err(|e| e.to_string())?;
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(_: &mut Self, _: &wl_seat::WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwpTabletManagerV2, ()> for State {
    fn event(_: &mut Self, _: &ZwpTabletManagerV2, _: <ZwpTabletManagerV2 as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwpTabletSeatV2, ()> for State {
    fn event(_: &mut Self, _: &ZwpTabletSeatV2, _: zwp_tablet_seat_v2::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}

    wayland_client::event_created_child!(State, ZwpTabletSeatV2, [
        zwp_tablet_seat_v2::EVT_TABLET_ADDED_OPCODE => (ZwpTabletV2, ()),
        zwp_tablet_seat_v2::EVT_TOOL_ADDED_OPCODE => (ZwpTabletToolV2, ()),
        zwp_tablet_seat_v2::EVT_PAD_ADDED_OPCODE => (ZwpTabletPadV2, ()),
    ]);
}

impl Dispatch<ZwpTabletV2, ()> for State {
    fn event(_: &mut Self, _: &ZwpTabletV2, _: <ZwpTabletV2 as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

// Pads (the buttons and rings on a tablet) are not used, but the compositor
// announces them and their children, so they need handlers to exist.
impl Dispatch<ZwpTabletPadV2, ()> for State {
    fn event(_: &mut Self, _: &ZwpTabletPadV2, _: zwp_tablet_pad_v2::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}

    wayland_client::event_created_child!(State, ZwpTabletPadV2, [
        zwp_tablet_pad_v2::EVT_GROUP_OPCODE => (ZwpTabletPadGroupV2, ()),
    ]);
}

impl Dispatch<ZwpTabletPadGroupV2, ()> for State {
    fn event(_: &mut Self, _: &ZwpTabletPadGroupV2, _: zwp_tablet_pad_group_v2::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}

    wayland_client::event_created_child!(State, ZwpTabletPadGroupV2, [
        zwp_tablet_pad_group_v2::EVT_RING_OPCODE => (ZwpTabletPadRingV2, ()),
        zwp_tablet_pad_group_v2::EVT_STRIP_OPCODE => (ZwpTabletPadStripV2, ()),
    ]);
}

impl Dispatch<ZwpTabletPadRingV2, ()> for State {
    fn event(_: &mut Self, _: &ZwpTabletPadRingV2, _: <ZwpTabletPadRingV2 as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwpTabletPadStripV2, ()> for State {
    fn event(_: &mut Self, _: &ZwpTabletPadStripV2, _: <ZwpTabletPadStripV2 as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwpTabletToolV2, ()> for State {
    fn event(state: &mut Self, _: &ZwpTabletToolV2, event: zwp_tablet_tool_v2::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        use zwp_tablet_tool_v2::Event;
        match event {
            Event::Motion { x, y } => state.send(PenEvent::Move(egui::pos2(x as f32, y as f32))),
            Event::Down { .. } => state.send(PenEvent::Tip { down: true }),
            Event::Up => state.send(PenEvent::Tip { down: false }),
            Event::ProximityOut => state.send(PenEvent::Gone),
            Event::Button { button, state: pressed, .. } => {
                let down = matches!(pressed, WEnum::Value(zwp_tablet_tool_v2::ButtonState::Pressed));
                match button {
                    BTN_STYLUS => state.send(PenEvent::Side { button: egui::PointerButton::Secondary, down }),
                    BTN_STYLUS2 => state.send(PenEvent::Side { button: egui::PointerButton::Middle, down }),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// Turn pen events into egui pointer events, remembering where the pen is so
/// that a tip or button press lands on the right spot. `last` is that memory.
pub fn to_egui(events: impl IntoIterator<Item = PenEvent>, last: &mut egui::Pos2, modifiers: egui::Modifiers) -> Vec<egui::Event> {
    let button = |button, pressed, pos| egui::Event::PointerButton { pos, button, pressed, modifiers };
    let mut out = Vec::new();
    for event in events {
        match event {
            PenEvent::Move(pos) => {
                *last = pos;
                out.push(egui::Event::PointerMoved(pos));
            }
            PenEvent::Tip { down } => out.push(button(egui::PointerButton::Primary, down, *last)),
            PenEvent::Side { button: b, down } => out.push(button(b, down, *last)),
            PenEvent::Gone => out.push(egui::Event::PointerGone),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stroke_becomes_move_press_move_release() {
        let mut last = egui::Pos2::ZERO;
        let events = to_egui(
            [
                PenEvent::Move(egui::pos2(10.0, 20.0)),
                PenEvent::Tip { down: true },
                PenEvent::Move(egui::pos2(30.0, 40.0)),
                PenEvent::Tip { down: false },
                PenEvent::Gone,
            ],
            &mut last,
            egui::Modifiers::NONE,
        );
        assert_eq!(last, egui::pos2(30.0, 40.0));
        assert!(matches!(events[0], egui::Event::PointerMoved(p) if p == egui::pos2(10.0, 20.0)));
        assert!(matches!(events[1], egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, .. } if pos == egui::pos2(10.0, 20.0)));
        assert!(matches!(events[3], egui::Event::PointerButton { pos, pressed: false, .. } if pos == egui::pos2(30.0, 40.0)));
        assert!(matches!(events[4], egui::Event::PointerGone));
    }

    #[test]
    fn side_buttons_act_as_right_and_middle_click_at_the_pens_position() {
        let mut last = egui::pos2(5.0, 6.0);
        let events = to_egui(
            [PenEvent::Side { button: egui::PointerButton::Secondary, down: true }, PenEvent::Side { button: egui::PointerButton::Middle, down: false }],
            &mut last,
            egui::Modifiers::NONE,
        );
        assert!(matches!(events[0], egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed: true, .. } if pos == egui::pos2(5.0, 6.0)));
        assert!(matches!(events[1], egui::Event::PointerButton { button: egui::PointerButton::Middle, pressed: false, .. }));
    }
}

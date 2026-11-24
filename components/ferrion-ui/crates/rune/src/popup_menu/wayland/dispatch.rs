use std::os::fd::OwnedFd;
use std::os::unix::fs::FileExt;

use wayland_client::globals::GlobalListContents;
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::{self, WlSurface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::xdg::shell::client::xdg_popup::{self, XdgPopup};
use wayland_protocols::xdg::shell::client::xdg_positioner::{self, XdgPositioner};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};
use xkbcommon::xkb;

use crate::platform::layer_shell_machine::keyboard as ls_keyboard;

use super::MAX_POPUP_EVENTS_PER_TURN;

#[derive(Clone, Debug)]
pub(super) enum ProtoEvent {
    Key {
        keysym: u32,
        pressed: bool,
        serial: u32,
    },
    PointerEnter {
        surface_id: u32,
        x: f64,
        y: f64,
    },
    PointerLeave {
        surface_id: u32,
    },
    PointerMotion {
        x: f64,
        y: f64,
    },
    PointerButton {
        button: u32,
        pressed: bool,
        serial: u32,
    },
    PopupDone {
        level_id: u64,
    },
    PopupConfigured {
        level_id: u64,
        rect: (i32, i32, i32, i32),
    },
    SurfaceScale {
        level_id: u64,
        scale: i32,
    },
}

#[derive(Default)]
pub(super) struct ProtoState {
    pub(super) keyboard: Option<WlKeyboard>,
    pub(super) pointer: Option<WlPointer>,
    xkb: Option<xkb::State>,
    pending_popup_rect: Vec<(u64, (i32, i32, i32, i32))>,
    configured_levels: Vec<u64>,
    pub(super) events: Vec<ProtoEvent>,
    pub(super) released_buffers: Vec<u64>,
}

impl ProtoState {
    fn push_event(&mut self, ev: ProtoEvent) {
        if self.events.len() < MAX_POPUP_EVENTS_PER_TURN {
            self.events.push(ev);
        }
    }

    fn take_pending_rect(&mut self, level_id: u64) -> Option<(i32, i32, i32, i32)> {
        let pos = self
            .pending_popup_rect
            .iter()
            .position(|&(id, _)| id == level_id)?;
        Some(self.pending_popup_rect.remove(pos).1)
    }

    pub(super) fn take_configured(&mut self, level_id: u64) -> bool {
        if let Some(pos) = self.configured_levels.iter().position(|&id| id == level_id) {
            self.configured_levels.remove(pos);
            true
        } else {
            false
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for ProtoState {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlCompositor, ()> for ProtoState {
    fn event(
        _: &mut Self,
        _: &WlCompositor,
        _: <WlCompositor as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShm, ()> for ProtoState {
    fn event(
        _: &mut Self,
        _: &WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShmPool, ()> for ProtoState {
    fn event(
        _: &mut Self,
        _: &WlShmPool,
        _: <WlShmPool as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlBuffer, u64> for ProtoState {
    fn event(
        state: &mut Self,
        _: &WlBuffer,
        event: wl_buffer::Event,
        id: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_buffer::Event::Release)
            && state.released_buffers.len() < MAX_POPUP_EVENTS_PER_TURN
        {
            state.released_buffers.push(*id);
        }
    }
}

impl Dispatch<XdgWmBase, ()> for ProtoState {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgPositioner, ()> for ProtoState {
    fn event(
        _: &mut Self,
        _: &XdgPositioner,
        _: xdg_positioner::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSurface, u64> for ProtoState {
    fn event(
        state: &mut Self,
        _: &WlSurface,
        event: wl_surface::Event,
        level_id: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_surface::Event::PreferredBufferScale { factor } = event
            && factor > 0
        {
            state.push_event(ProtoEvent::SurfaceScale {
                level_id: *level_id,
                scale: factor,
            });
        }
    }
}

impl Dispatch<XdgSurface, u64> for ProtoState {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        level_id: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            state.configured_levels.push(*level_id);
            if let Some(rect) = state.take_pending_rect(*level_id) {
                state.push_event(ProtoEvent::PopupConfigured {
                    level_id: *level_id,
                    rect,
                });
            }
        }
    }
}

impl Dispatch<XdgPopup, u64> for ProtoState {
    fn event(
        state: &mut Self,
        _: &XdgPopup,
        event: xdg_popup::Event,
        level_id: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_popup::Event::Configure {
                x,
                y,
                width,
                height,
            } => {
                if let Some(slot) = state
                    .pending_popup_rect
                    .iter_mut()
                    .find(|(id, _)| *id == *level_id)
                {
                    slot.1 = (x, y, width, height);
                } else {
                    state
                        .pending_popup_rect
                        .push((*level_id, (x, y, width, height)));
                }
            }
            xdg_popup::Event::PopupDone => {
                state.push_event(ProtoEvent::PopupDone {
                    level_id: *level_id,
                });
            }
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for ProtoState {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        else {
            return;
        };
        let has_kb = caps.contains(wl_seat::Capability::Keyboard);
        let has_ptr = caps.contains(wl_seat::Capability::Pointer);
        if has_kb && state.keyboard.is_none() {
            state.keyboard = Some(seat.get_keyboard(qh, ()));
        } else if !has_kb
            && let Some(kb) = state.keyboard.take()
            && kb.version() >= 3
        {
            kb.release();
        }
        if has_ptr && state.pointer.is_none() {
            state.pointer = Some(seat.get_pointer(qh, ()));
        } else if !has_ptr
            && let Some(ptr) = state.pointer.take()
            && ptr.version() >= 3
        {
            ptr.release();
        }
    }
}

fn read_keymap_fd(fd: OwnedFd, size: u32) -> Option<String> {
    let file = std::fs::File::from(fd);
    ls_keyboard::decode_keymap(size, |buf, offset| file.read_exact_at(buf, offset))
}

impl Dispatch<WlKeyboard, ()> for ProtoState {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Keymap {
                format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1),
                fd,
                size,
            } => {
                if let Some(map_str) = read_keymap_fd(fd, size) {
                    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
                    state.xkb = xkb::Keymap::new_from_string(
                        &ctx,
                        map_str,
                        xkb::KEYMAP_FORMAT_TEXT_V1,
                        xkb::COMPILE_NO_FLAGS,
                    )
                    .map(|km| xkb::State::new(&km));
                }
            }
            wl_keyboard::Event::Keymap { fd, .. } => {
                drop(fd);
            }
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                if let Some(xkb_state) = &mut state.xkb {
                    xkb_state.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
            }
            wl_keyboard::Event::Key {
                serial,
                key,
                state: WEnum::Value(key_state),
                ..
            } => {
                let pressed = key_state == wl_keyboard::KeyState::Pressed;
                let keysym = state
                    .xkb
                    .as_ref()
                    .map_or(0, |x| x.key_get_one_sym(xkb::Keycode::new(key + 8)).raw());
                if keysym != 0 {
                    state.push_event(ProtoEvent::Key {
                        keysym,
                        pressed,
                        serial,
                    });
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for ProtoState {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface,
                surface_x,
                surface_y,
                ..
            } => {
                state.push_event(ProtoEvent::PointerEnter {
                    surface_id: surface.id().protocol_id(),
                    x: surface_x,
                    y: surface_y,
                });
            }
            wl_pointer::Event::Leave { surface, .. } => {
                state.push_event(ProtoEvent::PointerLeave {
                    surface_id: surface.id().protocol_id(),
                });
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                state.push_event(ProtoEvent::PointerMotion {
                    x: surface_x,
                    y: surface_y,
                });
            }
            wl_pointer::Event::Button {
                serial,
                button,
                state: WEnum::Value(btn_state),
                ..
            } => {
                state.push_event(ProtoEvent::PointerButton {
                    button,
                    pressed: btn_state == wl_pointer::ButtonState::Pressed,
                    serial,
                });
            }
            _ => {}
        }
    }
}

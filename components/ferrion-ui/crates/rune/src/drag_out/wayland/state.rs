use std::collections::HashMap;
use std::os::fd::AsFd;
use std::sync::{Arc, Mutex};

use wayland_backend::client::ObjectId;
use wayland_client::globals::GlobalListContents;
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};

use super::DndWaylandError;
use super::payload::{ActiveDrag, DropSession, MIME_URI_LIST, PendingDropRead, map_clip_err};
use crate::clipboard::wayland::MAX_PENDING_SENDS;
use crate::clipboard::wayland::io::{PendingSend, make_nonblocking_pipe, parse_uri_list};
use crate::drag_out::DragOutcome;
use crate::runtime::Waker;
use crate::window::FileStep;

pub(super) const MAX_OFFER_MIMES: usize = 64;
pub(super) const MAX_PENDING_OFFERS: usize = 16;
pub(super) const MAX_DROP_STEPS_PER_WINDOW: usize = 256;
pub(super) const MAX_REGISTERED_WINDOWS: usize = 64;
pub(super) const MAX_COMPLETED_DRAGS: usize = 64;

pub(super) struct RegisteredWindow {
    pub(super) steps: Vec<FileStep>,
    pub(super) wake: Waker,
}

pub(super) struct DndState {
    pub(super) pointer: Option<WlPointer>,
    pub(super) pointer_surface: Option<ObjectId>,
    pub(super) press_grab: Option<(ObjectId, u32)>,
    pub(super) pending_offers: Vec<(WlDataOffer, Vec<String>)>,
    pub(super) active_drag: Option<ActiveDrag>,
    pub(super) active_drop: Option<DropSession>,
    pub(super) pending_sends: Vec<(PendingSend, bool)>,
    pub(super) pending_drop_read: Option<PendingDropRead>,
    pub(super) logged_send_cap: bool,
    pub(super) windows: Arc<Mutex<HashMap<ObjectId, RegisteredWindow>>>,
    pub(super) completed: Arc<Mutex<Vec<(u64, DragOutcome)>>>,
}

impl DndState {
    pub(super) fn new(
        windows: Arc<Mutex<HashMap<ObjectId, RegisteredWindow>>>,
        completed: Arc<Mutex<Vec<(u64, DragOutcome)>>>,
    ) -> Self {
        Self {
            pointer: None,
            pointer_surface: None,
            press_grab: None,
            pending_offers: Vec::new(),
            active_drag: None,
            active_drop: None,
            pending_sends: Vec::new(),
            pending_drop_read: None,
            logged_send_cap: false,
            windows,
            completed,
        }
    }

    pub(super) fn push_step(&self, surface_id: &ObjectId, step: FileStep) {
        let mut map = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = map.get_mut(surface_id) else {
            return;
        };
        if entry.steps.len() < MAX_DROP_STEPS_PER_WINDOW {
            entry.steps.push(step);
        }
        let wake = entry.wake.clone();
        drop(map);
        wake();
    }

    fn wake_surface(&self, surface_id: &ObjectId) {
        let wake = {
            let map = self.windows.lock().unwrap_or_else(|e| e.into_inner());
            map.get(surface_id)
                .map(|w| w.wake.clone())
                .or_else(|| map.values().next().map(|w| w.wake.clone()))
        };
        if let Some(wake) = wake {
            wake();
        }
    }

    pub(super) fn resolve_active_drag(&mut self, outcome: DragOutcome) {
        let Some(drag) = self.active_drag.take() else {
            return;
        };
        let drag_id = drag.id;
        let origin = drag.origin_surface.clone();
        drag.teardown();
        {
            let mut done = self.completed.lock().unwrap_or_else(|e| e.into_inner());
            if done.len() >= MAX_COMPLETED_DRAGS {
                done.remove(0);
            }
            done.push((drag_id, outcome));
        }
        self.wake_surface(&origin);
    }

    pub(super) fn cancel_for_surface(&mut self, surface_id: &ObjectId) {
        if self
            .active_drag
            .as_ref()
            .is_some_and(|d| &d.origin_surface == surface_id)
        {
            self.pending_sends.clear();
            self.resolve_active_drag(DragOutcome::Cancelled);
        }
        if self
            .active_drop
            .as_ref()
            .is_some_and(|d| &d.target_surface == surface_id)
        {
            self.pending_drop_read = None;
            if let Some(drop_sess) = self.active_drop.take() {
                drop_sess.offer.destroy();
            }
        }
    }

    pub(super) fn start_drop_read(&mut self) {
        self.pending_drop_read = None;
        let Some(drop_sess) = self.active_drop.as_ref() else {
            return;
        };
        let Ok((read_fd, write_fd)) = make_nonblocking_pipe() else {
            eprintln!("rune: drop-in: failed to create transfer pipe");
            self.refuse_active_drop();
            return;
        };
        drop_sess
            .offer
            .receive(MIME_URI_LIST.to_owned(), write_fd.as_fd());
        drop(write_fd);
        self.pending_drop_read = Some(PendingDropRead::new(read_fd, crate::time::now()));
    }

    pub(super) fn refuse_active_drop(&mut self) {
        let Some(drop_sess) = self.active_drop.as_mut() else {
            return;
        };
        drop_sess.offer.accept(drop_sess.enter_serial, None);
        if drop_sess.offer_v3 {
            drop_sess
                .offer
                .set_actions(DndAction::None, DndAction::None);
        }
        drop_sess.refused = true;
        if drop_sess.dropped
            && let Some(finished) = self.active_drop.take()
        {
            finished.offer.destroy();
        }
    }

    pub(super) fn finish_drop_read(&mut self, res: Result<Vec<u8>, DndWaylandError>) {
        if self.active_drop.is_none() {
            return;
        }
        let parsed = res.and_then(|bytes| parse_uri_list(&bytes).map_err(map_clip_err));
        match parsed {
            Ok(paths) if !paths.is_empty() => {
                let (surface_id, pos, emit_enter, dropped, offer_v3) = {
                    let Some(d) = self.active_drop.as_mut() else {
                        return;
                    };
                    let emit_enter = !d.enter_emitted;
                    d.enter_emitted = true;
                    (
                        d.target_surface.clone(),
                        d.pos,
                        emit_enter,
                        d.dropped,
                        d.offer_v3,
                    )
                };
                if emit_enter {
                    self.push_step(&surface_id, FileStep::Enter(paths.clone(), Some(pos)));
                }
                if dropped {
                    self.push_step(&surface_id, FileStep::Drop(paths, Some(pos)));
                    if let Some(done) = self.active_drop.take() {
                        if offer_v3 {
                            done.offer.finish();
                        }
                        done.offer.destroy();
                    }
                } else if let Some(d) = self.active_drop.as_mut() {
                    d.paths = Some(paths);
                }
            }
            Ok(_) => {
                self.refuse_active_drop();
            }
            Err(err) => {
                eprintln!("rune: drop-in: {err}");
                self.refuse_active_drop();
            }
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for DndState {
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

impl Dispatch<WlDataDeviceManager, ()> for DndState {
    fn event(
        _: &mut Self,
        _: &WlDataDeviceManager,
        _: <WlDataDeviceManager as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlCompositor, ()> for DndState {
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

impl Dispatch<WlSurface, ()> for DndState {
    fn event(
        _: &mut Self,
        _: &WlSurface,
        _: <WlSurface as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShm, ()> for DndState {
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

impl Dispatch<WlShmPool, ()> for DndState {
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

impl Dispatch<WlBuffer, u64> for DndState {
    fn event(
        state: &mut Self,
        _: &WlBuffer,
        event: wl_buffer::Event,
        id: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_buffer::Event::Release)
            && let Some(drag) = state.active_drag.as_mut()
            && let Some(icon) = drag.icon.as_mut()
        {
            icon.mark_released(*id);
        }
    }
}

impl Dispatch<WlSeat, ()> for DndState {
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
        let has_ptr = caps.contains(wl_seat::Capability::Pointer);
        if has_ptr && state.pointer.is_none() {
            state.pointer = Some(seat.get_pointer(qh, ()));
        } else if !has_ptr && let Some(ptr) = state.pointer.take() {
            if ptr.version() >= 3 {
                ptr.release();
            }
            state.pointer_surface = None;
            state.press_grab = None;
        }
    }
}

impl Dispatch<WlPointer, ()> for DndState {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { surface, .. } => {
                state.pointer_surface = Some(surface.id());
                state.press_grab = None;
            }
            wl_pointer::Event::Leave { surface, .. } => {
                if state.pointer_surface.as_ref() == Some(&surface.id()) {
                    state.pointer_surface = None;
                    state.press_grab = None;
                }
            }
            wl_pointer::Event::Button {
                serial,
                state: WEnum::Value(btn_state),
                ..
            } => match btn_state {
                wl_pointer::ButtonState::Pressed => {
                    if serial != 0
                        && let Some(surface_id) = state.pointer_surface.clone()
                    {
                        state.press_grab = Some((surface_id, serial));
                    }
                }
                wl_pointer::ButtonState::Released => {
                    state.press_grab = None;
                }
                _ => {}
            },
            _ => {}
        }
    }
}

impl Dispatch<WlDataOffer, ()> for DndState {
    fn event(
        state: &mut Self,
        offer: &WlDataOffer,
        event: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let wl_data_offer::Event::Offer { mime_type } = event else {
            return;
        };
        if let Some((_, mimes)) = state
            .pending_offers
            .iter_mut()
            .find(|(existing, _)| existing == offer)
            && mimes.len() < MAX_OFFER_MIMES
            && !mimes.iter().any(|m| m == &mime_type)
        {
            mimes.push(mime_type);
        }
    }
}

impl Dispatch<WlDataDevice, ()> for DndState {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::DataOffer { id } => {
                if state.pending_offers.len() >= MAX_PENDING_OFFERS {
                    let (old, _) = state.pending_offers.remove(0);
                    old.destroy();
                }
                state.pending_offers.push((id, Vec::new()));
            }
            wl_data_device::Event::Selection { id: Some(offer) } => {
                if let Some(idx) = state
                    .pending_offers
                    .iter()
                    .position(|(pending, _)| pending == &offer)
                {
                    let (sel_offer, _) = state.pending_offers.remove(idx);
                    sel_offer.destroy();
                } else {
                    offer.destroy();
                }
            }
            wl_data_device::Event::Enter {
                serial,
                surface,
                x,
                y,
                id,
            } => {
                if let Some(prev) = state.active_drop.take() {
                    state.pending_drop_read = None;
                    if prev.enter_emitted && !prev.dropped {
                        state.push_step(&prev.target_surface, FileStep::Leave);
                    }
                    prev.offer.destroy();
                }
                let Some(offer) = id else {
                    return;
                };
                let mimes = state
                    .pending_offers
                    .iter()
                    .position(|(pending, _)| pending == &offer)
                    .map(|idx| state.pending_offers.remove(idx).1)
                    .unwrap_or_default();
                let target_surface = surface.id();
                let offer_v3 = offer.version() >= 3;
                let window_known = state
                    .windows
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains_key(&target_surface);
                let self_blocked = state
                    .active_drag
                    .as_ref()
                    .is_some_and(|d| !d.payload.within_app);
                let has_uri = mimes.iter().any(|m| m == MIME_URI_LIST);
                let pos = (x as f32, y as f32);
                if !window_known || self_blocked || !has_uri {
                    offer.accept(serial, None);
                    if offer_v3 {
                        offer.set_actions(DndAction::None, DndAction::None);
                    }
                    state.active_drop = Some(DropSession {
                        offer,
                        target_surface,
                        enter_serial: serial,
                        pos,
                        offer_v3,
                        paths: None,
                        enter_emitted: false,
                        dropped: false,
                        refused: true,
                    });
                    return;
                }
                offer.accept(serial, Some(MIME_URI_LIST.to_owned()));
                if offer_v3 {
                    offer.set_actions(DndAction::Copy, DndAction::Copy);
                }
                state.active_drop = Some(DropSession {
                    offer,
                    target_surface,
                    enter_serial: serial,
                    pos,
                    offer_v3,
                    paths: None,
                    enter_emitted: false,
                    dropped: false,
                    refused: false,
                });
                if offer_v3 {
                    state.start_drop_read();
                }
            }
            wl_data_device::Event::Motion { x, y, .. } => {
                let pos = (x as f32, y as f32);
                let emit = if let Some(drop_sess) = state.active_drop.as_mut() {
                    drop_sess.pos = pos;
                    (drop_sess.enter_emitted && !drop_sess.dropped && !drop_sess.refused)
                        .then(|| drop_sess.target_surface.clone())
                } else {
                    None
                };
                if let Some(surface_id) = emit {
                    state.push_step(&surface_id, FileStep::Move(pos));
                }
            }
            wl_data_device::Event::Drop => {
                let Some(drop_sess) = state.active_drop.as_mut() else {
                    return;
                };
                drop_sess.dropped = true;
                if drop_sess.refused {
                    if let Some(done) = state.active_drop.take() {
                        done.offer.destroy();
                    }
                    return;
                }
                if let Some(paths) = drop_sess.paths.take() {
                    let surface_id = drop_sess.target_surface.clone();
                    let pos = drop_sess.pos;
                    let offer_v3 = drop_sess.offer_v3;
                    state.push_step(&surface_id, FileStep::Drop(paths, Some(pos)));
                    if let Some(done) = state.active_drop.take() {
                        if offer_v3 {
                            done.offer.finish();
                        }
                        done.offer.destroy();
                    }
                } else if state.pending_drop_read.is_none() {
                    if !drop_sess.offer_v3 {
                        state.start_drop_read();
                    } else if let Some(done) = state.active_drop.take() {
                        done.offer.destroy();
                    }
                }
            }
            wl_data_device::Event::Leave
                if state.active_drop.as_ref().is_some_and(|d| !d.dropped) =>
            {
                state.pending_drop_read = None;
                if let Some(drop_sess) = state.active_drop.take() {
                    if drop_sess.enter_emitted {
                        state.push_step(&drop_sess.target_surface, FileStep::Leave);
                    }
                    drop_sess.offer.destroy();
                }
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(DndState, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataSource, ()> for DndState {
    fn event(
        state: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let matches_active = state
            .active_drag
            .as_ref()
            .is_some_and(|d| d.source == *source);
        if !matches_active {
            if matches!(event, wl_data_source::Event::Cancelled) {
                source.destroy();
            }
            return;
        }
        match event {
            wl_data_source::Event::Action {
                dnd_action: WEnum::Value(action),
            } => {
                if let Some(drag) = state.active_drag.as_mut() {
                    drag.selected_action = action;
                }
            }
            wl_data_source::Event::DndDropPerformed => {
                if let Some(drag) = state.active_drag.as_mut() {
                    drag.drop_performed = true;
                    drag.destroy_icon();
                }
            }
            wl_data_source::Event::DndFinished => {
                if let Some(outcome) = state.active_drag.as_ref().map(ActiveDrag::finish_outcome) {
                    state.resolve_active_drag(outcome);
                }
            }
            wl_data_source::Event::Cancelled => {
                state.resolve_active_drag(DragOutcome::Cancelled);
            }
            wl_data_source::Event::Send { mime_type, fd } => {
                if state.pending_sends.len() >= MAX_PENDING_SENDS {
                    if !state.logged_send_cap {
                        state.logged_send_cap = true;
                        eprintln!(
                            "rune: drag-out: concurrent send limit ({MAX_PENDING_SENDS}) reached, dropping send"
                        );
                    }
                    return;
                }
                let (bytes, v3, v2_outcome) = {
                    let Some(drag) = state.active_drag.as_ref() else {
                        return;
                    };
                    let Some(bytes) = drag.payload.bytes_for_mime(&mime_type) else {
                        return;
                    };
                    (bytes, drag.v3, drag.payload.v2_default_outcome())
                };
                match PendingSend::start(fd, bytes, crate::time::now()) {
                    Some(pending) => state.pending_sends.push((pending, !v3)),
                    None if !v3 => state.resolve_active_drag(v2_outcome),
                    None => {}
                }
            }
            _ => {}
        }
    }
}

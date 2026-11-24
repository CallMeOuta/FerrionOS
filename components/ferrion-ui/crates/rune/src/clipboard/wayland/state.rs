use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use wayland_client::globals::GlobalListContents;
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::WlDataDeviceManager;
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use zeroize::Zeroizing;

use super::io::{PendingSend, SendBytes, format_uri_list};
use super::{
    ClipboardWaylandError, MAX_OFFER_MIMES, MAX_PENDING_OFFERS, MAX_PENDING_SENDS, MIME_HTML,
    MIME_KDE_SECRET, MIME_PNG, MIME_URI_LIST, TEXT_MIMES, overlay_focus_serial, record_input_serial,
};
use crate::clipboard::ClipboardWrite;

pub(super) struct WritePayload {
    text: Option<Zeroizing<Vec<u8>>>,
    html: Option<Arc<[u8]>>,
    png: Option<Arc<[u8]>>,
    uri_list: Option<Arc<[u8]>>,
    is_secret: bool,
}

impl WritePayload {
    pub(super) fn from_write(copy: &ClipboardWrite) -> Result<Self, ClipboardWaylandError> {
        let content = copy.content();
        if copy.is_secret() {
            return Ok(Self {
                text: Some(Zeroizing::new(content.text().as_bytes().to_vec())),
                html: None,
                png: None,
                uri_list: None,
                is_secret: true,
            });
        }
        if !content.files().is_empty() {
            let list = format_uri_list(content.files())?;
            if !list.is_empty() {
                return Ok(Self {
                    text: None,
                    html: None,
                    png: None,
                    uri_list: Some(list.into()),
                    is_secret: false,
                });
            }
        }
        if let Some(image) = content.image() {
            return Ok(Self {
                text: None,
                html: None,
                png: Some(Arc::from(image.png())),
                uri_list: None,
                is_secret: false,
            });
        }
        let html_bytes = content.html().map(|h| Arc::from(h.as_bytes()));
        let text_bytes = Some(Zeroizing::new(content.text().as_bytes().to_vec()));
        Ok(Self {
            text: text_bytes,
            html: html_bytes,
            png: None,
            uri_list: None,
            is_secret: false,
        })
    }

    pub(super) fn offer_all(&self, source: &WlDataSource) {
        if self.text.is_some() {
            for mime in TEXT_MIMES {
                source.offer((*mime).to_owned());
            }
        }
        if self.html.is_some() {
            source.offer(MIME_HTML.to_owned());
        }
        if self.png.is_some() {
            source.offer(MIME_PNG.to_owned());
        }
        if self.uri_list.is_some() {
            source.offer(MIME_URI_LIST.to_owned());
        }
        if self.is_secret {
            source.offer(MIME_KDE_SECRET.to_owned());
        }
    }

    pub(super) fn bytes_for_mime(&self, mime: &str) -> Option<SendBytes> {
        if TEXT_MIMES.contains(&mime) {
            return self
                .text
                .as_ref()
                .map(|t| SendBytes::Secret(Zeroizing::new(t.to_vec())));
        }
        if mime == MIME_HTML {
            return self.html.as_ref().map(|b| SendBytes::Shared(b.clone()));
        }
        if mime == MIME_PNG {
            return self.png.as_ref().map(|b| SendBytes::Shared(b.clone()));
        }
        if mime == MIME_URI_LIST {
            return self.uri_list.as_ref().map(|b| SendBytes::Shared(b.clone()));
        }
        if mime == MIME_KDE_SECRET && self.is_secret {
            return Some(SendBytes::Shared(Arc::from(&b"secret"[..])));
        }
        None
    }
}

pub(super) struct State {
    pub(super) seat_focused: bool,
    pub(super) seat_serial: Option<u32>,
    pub(super) keyboard: Option<WlKeyboard>,
    pub(super) pointer: Option<WlPointer>,
    pub(super) pending_offers: Vec<(WlDataOffer, Vec<String>)>,
    pub(super) current_offer: Option<(WlDataOffer, Vec<String>)>,
    pub(super) active_source: Option<(WlDataSource, WritePayload)>,
    pub(super) pending_sends: Vec<PendingSend>,
    pub(super) logged_send_cap: bool,
    pub(super) revision: Arc<AtomicU64>,
}

impl State {
    pub(super) fn new(revision: Arc<AtomicU64>) -> Self {
        Self {
            seat_focused: false,
            seat_serial: None,
            keyboard: None,
            pointer: None,
            pending_offers: Vec::new(),
            current_offer: None,
            active_source: None,
            pending_sends: Vec::new(),
            logged_send_cap: false,
            revision,
        }
    }

    pub(super) fn focus_serial(&self) -> Option<u32> {
        let seat = if self.seat_focused {
            self.seat_serial.filter(|&s| s != 0)
        } else {
            None
        };
        seat.or_else(overlay_focus_serial)
    }

    pub(super) fn bump_revision(&self) {
        self.revision.fetch_add(1, Ordering::AcqRel);
    }

    pub(super) fn clear_active_source(&mut self, data_device: &WlDataDevice) {
        if let Some((source, _payload)) = self.active_source.take() {
            if let Some(serial) = self.focus_serial() {
                data_device.set_selection(None, serial);
            }
            source.destroy();
            self.bump_revision();
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
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

impl Dispatch<WlDataDeviceManager, ()> for State {
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

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_seat::Event::Capabilities {
            capabilities: wayland_client::WEnum::Value(caps),
        } = event
        else {
            return;
        };
        let has_kb = caps.contains(wl_seat::Capability::Keyboard);
        let has_ptr = caps.contains(wl_seat::Capability::Pointer);
        if has_kb && state.keyboard.is_none() {
            state.keyboard = Some(seat.get_keyboard(qh, ()));
        } else if !has_kb && let Some(kb) = state.keyboard.take() {
            if kb.version() >= 3 {
                kb.release();
            }
            state.seat_focused = false;
            state.seat_serial = None;
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

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { serial, .. } => {
                state.seat_focused = true;
                state.seat_serial = (serial != 0).then_some(serial);
                record_input_serial(serial);
            }
            wl_keyboard::Event::Leave { .. } => {
                state.seat_focused = false;
                state.seat_serial = None;
            }
            wl_keyboard::Event::Key { serial, .. } => {
                record_input_serial(serial);
                if state.seat_focused && serial != 0 {
                    state.seat_serial = Some(serial);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Button { serial, .. } = event {
            record_input_serial(serial);
            if state.seat_focused && serial != 0 {
                state.seat_serial = Some(serial);
            }
        }
    }
}

impl Dispatch<WlDataOffer, ()> for State {
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
        {
            if mimes.len() < MAX_OFFER_MIMES && !mimes.iter().any(|m| m == &mime_type) {
                mimes.push(mime_type);
            }
            return;
        }
        if let Some((current, mimes)) = &mut state.current_offer
            && current == offer
            && mimes.len() < MAX_OFFER_MIMES
            && !mimes.iter().any(|m| m == &mime_type)
        {
            mimes.push(mime_type);
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
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
            wl_data_device::Event::Selection { id } => {
                if let Some((old, _)) = state.current_offer.take() {
                    old.destroy();
                }
                if let Some(offer) = id {
                    let mimes = state
                        .pending_offers
                        .iter()
                        .position(|(pending, _)| pending == &offer)
                        .map(|idx| state.pending_offers.remove(idx).1)
                        .unwrap_or_default();
                    state.current_offer = Some((offer, mimes));
                }
                if state.active_source.is_none() {
                    state.bump_revision();
                }
            }
            wl_data_device::Event::Enter {
                id: Some(offer), ..
            } => {
                if let Some(idx) = state
                    .pending_offers
                    .iter()
                    .position(|(pending, _)| pending == &offer)
                {
                    let (dnd_offer, _) = state.pending_offers.remove(idx);
                    dnd_offer.destroy();
                } else {
                    offer.destroy();
                }
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataSource, ()> for State {
    fn event(
        state: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                let Some((active, payload)) = &state.active_source else {
                    return;
                };
                if active != source {
                    return;
                }
                if state.pending_sends.len() >= MAX_PENDING_SENDS {
                    if !state.logged_send_cap {
                        state.logged_send_cap = true;
                        eprintln!(
                            "rune: clipboard: concurrent send limit ({MAX_PENDING_SENDS}) reached, dropping send"
                        );
                    }
                    return;
                }
                let Some(bytes) = payload.bytes_for_mime(&mime_type) else {
                    return;
                };
                if let Some(pending) = PendingSend::start(fd, bytes, crate::time::now()) {
                    state.pending_sends.push(pending);
                }
            }
            wl_data_source::Event::Cancelled => {
                if let Some((active, payload)) = state.active_source.take() {
                    if active == *source {
                        active.destroy();
                        state.bump_revision();
                    } else {
                        state.active_source = Some((active, payload));
                        source.destroy();
                    }
                } else {
                    source.destroy();
                }
            }
            _ => {}
        }
    }
}

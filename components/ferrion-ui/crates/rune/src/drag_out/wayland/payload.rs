use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Arc;

use wayland_backend::client::ObjectId;
use wayland_client::protocol::wl_data_device_manager::DndAction;
use wayland_client::protocol::wl_data_offer::WlDataOffer;
use wayland_client::protocol::wl_data_source::WlDataSource;

use super::DndWaylandError;
use super::icon::DragIcon;
use crate::clipboard::wayland::io::{SendBytes, format_uri_list, read_fd_nonblocking};
use crate::clipboard::wayland::{ClipboardWaylandError, MAX_CLIPBOARD_BYTES, MAX_IO_TIMEOUT};
use crate::drag_out::{DragItem, DragOperation, DragOutcome, path_text};
use crate::time::Instant;

const READ_CHUNK_BYTES: usize = 16 * 1024;

pub(super) const MIME_URI_LIST: &str = "text/uri-list";
const MIME_PNG: &str = "image/png";
const MIME_TEXT_UTF8: &str = "text/plain;charset=utf-8";
const MIME_TEXT: &str = "text/plain";
const MIME_UTF8_STRING: &str = "UTF8_STRING";
const TEXT_MIMES: &[&str] = &[MIME_TEXT_UTF8, MIME_TEXT, MIME_UTF8_STRING];

pub(super) struct DragPayload {
    uri_list: Option<Arc<[u8]>>,
    png: Option<Arc<[u8]>>,
    text: Option<Arc<[u8]>>,
    pub(super) actions: DndAction,
    pub(super) within_app: bool,
}

impl DragPayload {
    pub(super) fn from_item(item: &DragItem) -> Result<Self, DndWaylandError> {
        if item.offers(DragOperation::Link) {
            return Err(DndWaylandError::UnsupportedOperation(DragOperation::Link));
        }
        let mut actions = DndAction::empty();
        if item.offers(DragOperation::Copy) {
            actions |= DndAction::Copy;
        }
        if item.offers(DragOperation::Move) {
            actions |= DndAction::Move;
        }
        if actions.is_empty() {
            return Err(DndWaylandError::NoOperations);
        }
        if item.is_empty() {
            return Err(DndWaylandError::EmptyDrag);
        }
        let uri_list = if !item.paths().is_empty() {
            let formatted = format_uri_list(item.paths()).map_err(map_clip_err)?;
            if formatted.len() > MAX_CLIPBOARD_BYTES {
                return Err(DndWaylandError::PayloadTooLarge);
            }
            (!formatted.is_empty()).then(|| Arc::<[u8]>::from(formatted))
        } else {
            None
        };
        let png = match item.png_data() {
            Some(bytes) => {
                if bytes.len() > MAX_CLIPBOARD_BYTES {
                    return Err(DndWaylandError::PayloadTooLarge);
                }
                Some(Arc::<[u8]>::from(bytes))
            }
            None => None,
        };
        let text_str = item
            .plain_text()
            .map(str::to_owned)
            .or_else(|| item.paths().first().map(|p| path_text(p)));
        let text = match text_str {
            Some(s) => {
                if s.len() > MAX_CLIPBOARD_BYTES {
                    return Err(DndWaylandError::PayloadTooLarge);
                }
                Some(Arc::<[u8]>::from(s.into_bytes()))
            }
            None => None,
        };
        if uri_list.is_none() && png.is_none() && text.is_none() {
            return Err(DndWaylandError::EmptyDrag);
        }
        Ok(Self {
            uri_list,
            png,
            text,
            actions,
            within_app: item.drops_within_app(),
        })
    }

    pub(super) fn offer_all(&self, source: &WlDataSource) {
        if self.uri_list.is_some() {
            source.offer(MIME_URI_LIST.to_owned());
        }
        if self.png.is_some() {
            source.offer(MIME_PNG.to_owned());
        }
        if self.text.is_some() {
            for mime in TEXT_MIMES {
                source.offer((*mime).to_owned());
            }
        }
    }

    pub(super) fn bytes_for_mime(&self, mime: &str) -> Option<SendBytes> {
        if mime == MIME_URI_LIST {
            return self.uri_list.as_ref().map(|b| SendBytes::Shared(b.clone()));
        }
        if mime == MIME_PNG {
            return self.png.as_ref().map(|b| SendBytes::Shared(b.clone()));
        }
        if TEXT_MIMES.contains(&mime) {
            return self.text.as_ref().map(|b| SendBytes::Shared(b.clone()));
        }
        None
    }

    pub(super) fn v2_default_outcome(&self) -> DragOutcome {
        if self.actions == DndAction::Move {
            DragOutcome::Moved
        } else {
            DragOutcome::Copied
        }
    }
}

pub(super) fn map_clip_err(err: ClipboardWaylandError) -> DndWaylandError {
    match err {
        ClipboardWaylandError::PayloadTooLarge => DndWaylandError::PayloadTooLarge,
        ClipboardWaylandError::TooManyUris => DndWaylandError::TooManyUris,
        ClipboardWaylandError::IoTimeout => DndWaylandError::IoTimeout,
        ClipboardWaylandError::PipeFailed(msg) => DndWaylandError::PipeFailed(msg),
        _ => DndWaylandError::ConnectionClosed,
    }
}

pub(super) struct ActiveDrag {
    pub(super) id: u64,
    pub(super) origin_surface: ObjectId,
    pub(super) source: WlDataSource,
    pub(super) payload: DragPayload,
    pub(super) icon: Option<DragIcon>,
    pub(super) v3: bool,
    pub(super) selected_action: DndAction,
    pub(super) drop_performed: bool,
}

impl ActiveDrag {
    pub(super) fn destroy_icon(&mut self) {
        self.icon.take();
    }

    pub(super) fn teardown(mut self) {
        self.icon.take();
        self.source.destroy();
    }

    pub(super) fn finish_outcome(&self) -> DragOutcome {
        if self.selected_action.contains(DndAction::Move) {
            DragOutcome::Moved
        } else if self.selected_action.contains(DndAction::Copy)
            || self.selected_action.contains(DndAction::Ask)
        {
            DragOutcome::Copied
        } else {
            DragOutcome::Cancelled
        }
    }
}

pub(super) struct DropSession {
    pub(super) offer: WlDataOffer,
    pub(super) target_surface: ObjectId,
    pub(super) enter_serial: u32,
    pub(super) pos: (f32, f32),
    pub(super) offer_v3: bool,
    pub(super) paths: Option<Vec<PathBuf>>,
    pub(super) enter_emitted: bool,
    pub(super) dropped: bool,
    pub(super) refused: bool,
}

pub(super) struct PendingDropRead {
    pub(super) fd: OwnedFd,
    pub(super) buf: Vec<u8>,
    pub(super) max_bytes: usize,
    pub(super) deadline: Instant,
}

impl PendingDropRead {
    pub(super) fn new(fd: OwnedFd, now: Instant) -> Self {
        Self {
            fd,
            buf: Vec::new(),
            max_bytes: MAX_CLIPBOARD_BYTES,
            deadline: now + MAX_IO_TIMEOUT,
        }
    }

    pub(super) fn step(&mut self) -> Option<Result<Vec<u8>, DndWaylandError>> {
        let mut chunk = [0u8; READ_CHUNK_BYTES];
        loop {
            match read_fd_nonblocking(self.fd.as_raw_fd(), &mut chunk) {
                Ok(0) => return Some(Ok(std::mem::take(&mut self.buf))),
                Ok(n) => {
                    if self.buf.len().saturating_add(n) > self.max_bytes {
                        self.buf.clear();
                        return Some(Err(DndWaylandError::PayloadTooLarge));
                    }
                    self.buf.extend_from_slice(&chunk[..n]);
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return None,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => {
                    self.buf.clear();
                    return Some(Err(DndWaylandError::PipeFailed(err.to_string())));
                }
            }
        }
    }
}

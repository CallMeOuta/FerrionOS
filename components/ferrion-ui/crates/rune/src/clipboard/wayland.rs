use std::ffi::c_void;
use std::fmt;
use std::io::ErrorKind;
use std::os::fd::{AsFd, AsRawFd};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use wayland_client::globals::{GlobalList, registry_queue_init};
use wayland_client::protocol::wl_data_device::WlDataDevice;
use wayland_client::protocol::wl_data_device_manager::WlDataDeviceManager;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, EventQueue, Proxy, QueueHandle};
use zeroize::Zeroizing;

use super::{Backend, ClipboardContent, ClipboardImage, ClipboardWrite, MAX_IMAGE_BYTES, html};
use crate::time::Instant;

mod io;
mod state;

use io::{
    MAX_POLL_FDS, PendingRead, WakeHandle, WakePipe, foreign_display_backend,
    make_nonblocking_pipe, parse_uri_list, poll_fds, poll_timeout_ms,
};
use state::{State, WritePayload};

pub(crate) const MAX_CLIPBOARD_BYTES: usize = MAX_IMAGE_BYTES;
pub(crate) const MAX_IO_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const MAX_PENDING_SENDS: usize = 16;
const MAX_OFFER_MIMES: usize = 64;
const MAX_PENDING_OFFERS: usize = 16;
pub(crate) const MAX_URI_FILES: usize = 1024;
const CMD_REPLY_TIMEOUT: Duration = Duration::from_secs(3);

const MIME_TEXT_UTF8: &str = "text/plain;charset=utf-8";
const MIME_TEXT: &str = "text/plain";
const MIME_UTF8_STRING: &str = "UTF8_STRING";
const MIME_HTML: &str = "text/html";
const MIME_PNG: &str = "image/png";
const MIME_URI_LIST: &str = "text/uri-list";
const MIME_KDE_SECRET: &str = "x-kde-passwordManagerHint";
const TEXT_MIMES: &[&str] = &[MIME_TEXT_UTF8, MIME_TEXT, MIME_UTF8_STRING];

static DISPLAY_PTR: AtomicUsize = AtomicUsize::new(0);
static OVERLAY_FOCUSED: AtomicBool = AtomicBool::new(false);
static OVERLAY_SERIAL: AtomicU32 = AtomicU32::new(0);
static LAST_INPUT_SERIAL: AtomicU32 = AtomicU32::new(0);

pub(crate) fn attach_display(display: NonNull<c_void>) {
    DISPLAY_PTR.store(display.as_ptr() as usize, Ordering::Release);
}

pub(crate) fn detach_display() {
    DISPLAY_PTR.store(0, Ordering::Release);
    OVERLAY_FOCUSED.store(false, Ordering::Release);
    OVERLAY_SERIAL.store(0, Ordering::Release);
    LAST_INPUT_SERIAL.store(0, Ordering::Release);
}

pub(crate) fn record_overlay_focus(focused: bool, serial: u32) {
    if focused {
        OVERLAY_SERIAL.store(serial, Ordering::Release);
        OVERLAY_FOCUSED.store(true, Ordering::Release);
        record_input_serial(serial);
    } else {
        OVERLAY_FOCUSED.store(false, Ordering::Release);
        OVERLAY_SERIAL.store(0, Ordering::Release);
    }
}

pub(crate) fn record_overlay_serial(serial: u32) {
    if OVERLAY_FOCUSED.load(Ordering::Acquire) && serial != 0 {
        OVERLAY_SERIAL.store(serial, Ordering::Release);
    }
    record_input_serial(serial);
}

pub(super) fn record_input_serial(serial: u32) {
    if serial != 0 {
        LAST_INPUT_SERIAL.store(serial, Ordering::Release);
    }
}

pub(crate) fn attached_display() -> Option<NonNull<c_void>> {
    current_display()
}

pub(crate) fn latest_input_serial() -> Option<u32> {
    let serial = LAST_INPUT_SERIAL.load(Ordering::Acquire);
    (serial != 0).then_some(serial)
}

fn current_display() -> Option<NonNull<c_void>> {
    NonNull::new(DISPLAY_PTR.load(Ordering::Acquire) as *mut c_void)
}

fn overlay_focus_serial() -> Option<u32> {
    if !OVERLAY_FOCUSED.load(Ordering::Acquire) {
        return None;
    }
    let serial = OVERLAY_SERIAL.load(Ordering::Acquire);
    (serial != 0).then_some(serial)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClipboardWaylandError {
    NoDisplay,
    DisplayConnection(String),
    MissingDataDeviceManager,
    MissingSeat,
    NoFocusSerial,
    ConnectionClosed,
    IoTimeout,
    PayloadTooLarge,
    TooManyUris,
    PipeFailed(String),
}

impl fmt::Display for ClipboardWaylandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClipboardWaylandError::NoDisplay => {
                f.write_str("no Wayland display attached to this process")
            }
            ClipboardWaylandError::DisplayConnection(reason) => {
                write!(f, "Wayland clipboard connection failed: {reason}")
            }
            ClipboardWaylandError::MissingDataDeviceManager => {
                f.write_str("Wayland compositor does not advertise wl_data_device_manager")
            }
            ClipboardWaylandError::MissingSeat => {
                f.write_str("Wayland compositor does not advertise wl_seat")
            }
            ClipboardWaylandError::NoFocusSerial => {
                f.write_str("Wayland clipboard write requires an input serial after keyboard focus")
            }
            ClipboardWaylandError::ConnectionClosed => {
                f.write_str("Wayland clipboard connection closed")
            }
            ClipboardWaylandError::IoTimeout => {
                write!(
                    f,
                    "Wayland clipboard transfer timed out after {} ms",
                    MAX_IO_TIMEOUT.as_millis()
                )
            }
            ClipboardWaylandError::PayloadTooLarge => {
                write!(
                    f,
                    "Wayland clipboard payload exceeded {MAX_CLIPBOARD_BYTES} bytes"
                )
            }
            ClipboardWaylandError::TooManyUris => {
                write!(
                    f,
                    "Wayland clipboard URI list exceeded {MAX_URI_FILES} files"
                )
            }
            ClipboardWaylandError::PipeFailed(reason) => {
                write!(f, "Wayland clipboard pipe failed: {reason}")
            }
        }
    }
}

enum IoCmd {
    Write {
        payload: WritePayload,
        reply: mpsc::Sender<Result<(), ClipboardWaylandError>>,
    },
    Read {
        mimes: &'static [&'static str],
        max_bytes: usize,
        reply: mpsc::Sender<Result<Zeroizing<Vec<u8>>, ClipboardWaylandError>>,
    },
    Clear {
        reply: mpsc::Sender<Result<(), ClipboardWaylandError>>,
    },
    Shutdown,
}

pub(super) struct WaylandClipboard {
    cmd_tx: mpsc::Sender<IoCmd>,
    wake: WakeHandle,
    revision: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl WaylandClipboard {
    pub(super) fn connect() -> Result<Self, ClipboardWaylandError> {
        let display = current_display().ok_or(ClipboardWaylandError::NoDisplay)?;
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (wake_pipe, wake) = WakePipe::new()?;
        let revision = Arc::new(AtomicU64::new(0));
        let thread_rev = revision.clone();
        let display_addr = display.as_ptr() as usize;
        let thread = std::thread::Builder::new()
            .name("rune-wl-clip".into())
            .spawn(move || {
                let ptr = NonNull::new(display_addr as *mut c_void);
                let Some(ptr) = ptr else {
                    ready_tx.send(Err(ClipboardWaylandError::NoDisplay)).ok();
                    return;
                };
                run_io_thread(ptr, cmd_rx, wake_pipe, thread_rev, ready_tx);
            })
            .map_err(|e| ClipboardWaylandError::PipeFailed(e.to_string()))?;
        match ready_rx.recv_timeout(CMD_REPLY_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                cmd_tx,
                wake,
                revision,
                thread: Some(thread),
            }),
            Ok(Err(err)) => {
                thread.join().ok();
                Err(err)
            }
            Err(_) => Err(ClipboardWaylandError::IoTimeout),
        }
    }

    fn read_mime(
        &self,
        mimes: &'static [&'static str],
        max_bytes: usize,
    ) -> Option<Zeroizing<Vec<u8>>> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.cmd_tx
            .send(IoCmd::Read {
                mimes,
                max_bytes,
                reply: reply_tx,
            })
            .ok()?;
        self.wake.wake();
        reply_rx
            .recv_timeout(CMD_REPLY_TIMEOUT)
            .ok()
            .and_then(Result::ok)
    }
}

impl Drop for WaylandClipboard {
    fn drop(&mut self) {
        self.cmd_tx.send(IoCmd::Shutdown).ok();
        self.wake.wake();
        if let Some(handle) = self.thread.take() {
            handle.join().ok();
        }
    }
}

fn zeroized_utf8(mut bytes: Zeroizing<Vec<u8>>) -> Option<Zeroizing<String>> {
    match String::from_utf8(std::mem::take(&mut *bytes)) {
        Ok(s) => Some(Zeroizing::new(s)),
        Err(err) => {
            drop(Zeroizing::new(err.into_bytes()));
            None
        }
    }
}

impl Backend for WaylandClipboard {
    fn write(&mut self, copy: &ClipboardWrite) -> Result<(), String> {
        let payload = WritePayload::from_write(copy).map_err(|e| e.to_string())?;
        let (reply_tx, reply_rx) = mpsc::channel();
        self.cmd_tx
            .send(IoCmd::Write {
                payload,
                reply: reply_tx,
            })
            .map_err(|_| ClipboardWaylandError::ConnectionClosed.to_string())?;
        self.wake.wake();
        match reply_rx.recv_timeout(CMD_REPLY_TIMEOUT) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => Err(err.to_string()),
            Err(_) => Err(ClipboardWaylandError::IoTimeout.to_string()),
        }
    }

    fn text(&mut self) -> Option<Zeroizing<String>> {
        let bytes = self.read_mime(TEXT_MIMES, MAX_CLIPBOARD_BYTES)?;
        zeroized_utf8(bytes)
    }

    fn content(&mut self, all: bool) -> Option<ClipboardContent> {
        let text = self.text();
        let html = self
            .read_mime(&[MIME_HTML], html::MAX_RICH_BYTES)
            .and_then(zeroized_utf8)
            .filter(|h| !h.is_empty() && h.len() <= html::MAX_RICH_BYTES);
        let image = if all {
            self.read_mime(&[MIME_PNG], MAX_IMAGE_BYTES)
                .and_then(|mut b| ClipboardImage::from_png(std::mem::take(&mut *b)))
        } else {
            None
        };
        let mut uri_error: Option<String> = None;
        let files = if all {
            match self.read_mime(&[MIME_URI_LIST], MAX_CLIPBOARD_BYTES) {
                Some(bytes) => match parse_uri_list(&bytes) {
                    Ok(paths) => paths,
                    Err(err) => {
                        eprintln!("rune: clipboard: {err}");
                        uri_error = Some(err.to_string());
                        Vec::new()
                    }
                },
                None => Vec::new(),
            }
        } else {
            Vec::new()
        };
        if text.is_none()
            && html.is_none()
            && image.is_none()
            && files.is_empty()
            && uri_error.is_none()
        {
            return None;
        }
        let mut content = ClipboardContent::from(text.unwrap_or_default()).with_files(files);
        if let Some(mut html) = html {
            content = content.with_html(std::mem::take(&mut *html));
        }
        if let Some(image) = image {
            content = content.with_image(image);
        }
        if let Some(err) = uri_error {
            content = content.with_error(err);
        }
        Some(content)
    }

    fn revision(&self) -> Option<u64> {
        Some(self.revision.load(Ordering::Acquire))
    }

    fn clear(&mut self) -> Result<(), String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.cmd_tx
            .send(IoCmd::Clear { reply: reply_tx })
            .map_err(|_| ClipboardWaylandError::ConnectionClosed.to_string())?;
        self.wake.wake();
        match reply_rx.recv_timeout(CMD_REPLY_TIMEOUT) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => Err(err.to_string()),
            Err(_) => Err(ClipboardWaylandError::IoTimeout.to_string()),
        }
    }
}

fn destroy_globals(conn: &Connection, globals: GlobalList) {
    let id = globals.registry().id();
    drop(globals);
    conn.backend().destroy_object(&id).ok();
}

fn run_io_thread(
    display: NonNull<c_void>,
    cmd_rx: mpsc::Receiver<IoCmd>,
    wake_pipe: WakePipe,
    revision: Arc<AtomicU64>,
    ready_tx: mpsc::Sender<Result<(), ClipboardWaylandError>>,
) {
    let backend = foreign_display_backend(display);
    let conn = Connection::from_backend(backend);
    let (globals, mut queue) = match registry_queue_init::<State>(&conn) {
        Ok(pair) => pair,
        Err(err) => {
            ready_tx
                .send(Err(ClipboardWaylandError::DisplayConnection(
                    err.to_string(),
                )))
                .ok();
            return;
        }
    };
    let qh = queue.handle();
    let Ok(manager) = globals.bind::<WlDataDeviceManager, _, _>(&qh, 1..=3, ()) else {
        destroy_globals(&conn, globals);
        queue.flush().ok();
        ready_tx
            .send(Err(ClipboardWaylandError::MissingDataDeviceManager))
            .ok();
        return;
    };
    let Ok(seat) = globals.bind::<WlSeat, _, _>(&qh, 1..=8, ()) else {
        conn.backend().destroy_object(&manager.id()).ok();
        destroy_globals(&conn, globals);
        queue.flush().ok();
        ready_tx.send(Err(ClipboardWaylandError::MissingSeat)).ok();
        return;
    };
    let data_device = manager.get_data_device(&seat, &qh, ());
    let mut state = State::new(revision);
    queue.roundtrip(&mut state).ok();
    queue.roundtrip(&mut state).ok();
    ready_tx.send(Ok(())).ok();

    let mut pending_read: Option<PendingRead> = None;
    let mut running = true;

    while running {
        queue.dispatch_pending(&mut state).ok();
        queue.flush().ok();

        let now = crate::time::now();
        service_io(&mut state, &mut pending_read, now);

        while let Ok(cmd) = cmd_rx.try_recv() {
            if !handle_cmd(
                cmd,
                &mut state,
                &mut pending_read,
                &manager,
                &data_device,
                &qh,
                &mut queue,
            ) {
                running = false;
                break;
            }
        }
        if !running {
            break;
        }
        queue.flush().ok();

        let read_guard = match queue.prepare_read() {
            Some(guard) => guard,
            None => {
                if queue.dispatch_pending(&mut state).is_err() {
                    break;
                }
                continue;
            }
        };

        let wl_fd = read_guard.connection_fd().as_raw_fd();
        let mut pfds = Vec::with_capacity(MAX_POLL_FDS);
        pfds.push(libc::pollfd {
            fd: wl_fd,
            events: libc::POLLIN,
            revents: 0,
        });
        pfds.push(libc::pollfd {
            fd: wake_pipe.fd(),
            events: libc::POLLIN,
            revents: 0,
        });
        let read_slot = pending_read.as_ref().map(|r| {
            let idx = pfds.len();
            pfds.push(libc::pollfd {
                fd: r.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            idx
        });
        let send_base = pfds.len();
        for send in &state.pending_sends {
            pfds.push(libc::pollfd {
                fd: send.fd.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            });
        }

        let now = crate::time::now();
        let timeout_ms = poll_timeout_ms(
            state
                .pending_sends
                .iter()
                .map(|s| s.deadline)
                .chain(pending_read.iter().map(|r| r.deadline)),
            now,
        );

        let poll_res = poll_fds(&mut pfds, timeout_ms);
        match poll_res {
            Ok(_) => {
                let wl_revents = pfds[0].revents;
                if wl_revents & libc::POLLIN != 0 {
                    if let Err(err) = read_guard.read()
                        && !matches!(
                            err,
                            wayland_backend::client::WaylandError::Io(ref e)
                                if e.kind() == ErrorKind::WouldBlock
                        )
                    {
                        break;
                    }
                } else {
                    drop(read_guard);
                    if wl_revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                        break;
                    }
                }
                if pfds[1].revents & libc::POLLIN != 0 {
                    wake_pipe.drain();
                }
                if let Some(idx) = read_slot
                    && pfds[idx].revents != 0
                {
                    step_read(&mut pending_read);
                }
                for i in (0..state.pending_sends.len()).rev() {
                    let rev = pfds[send_base + i].revents;
                    let drop_send = if rev & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                        true
                    } else if rev & libc::POLLOUT != 0 {
                        state.pending_sends[i].try_write()
                    } else {
                        false
                    };
                    if drop_send {
                        state.pending_sends.swap_remove(i);
                    }
                }
            }
            Err(err) if err.kind() == ErrorKind::Interrupted => {
                drop(read_guard);
            }
            Err(_) => {
                drop(read_guard);
                break;
            }
        }
    }

    cleanup(
        &conn,
        globals,
        &mut queue,
        &mut state,
        &mut pending_read,
        &manager,
        &seat,
        &data_device,
    );
}

#[allow(clippy::too_many_arguments)]
fn cleanup(
    conn: &Connection,
    globals: GlobalList,
    queue: &mut EventQueue<State>,
    state: &mut State,
    pending_read: &mut Option<PendingRead>,
    manager: &WlDataDeviceManager,
    seat: &WlSeat,
    data_device: &WlDataDevice,
) {
    if let Some(read) = pending_read.take() {
        read.reply
            .send(Err(ClipboardWaylandError::ConnectionClosed))
            .ok();
    }
    state.pending_sends.clear();
    state.clear_active_source(data_device);
    for (offer, _) in state.pending_offers.drain(..) {
        offer.destroy();
    }
    if let Some((offer, _)) = state.current_offer.take() {
        offer.destroy();
    }
    if let Some(offer) = state.active_dnd_offer.take() {
        offer.destroy();
    }
    if let Some(kb) = state.keyboard.take() {
        if kb.version() >= 3 {
            kb.release();
        } else {
            conn.backend().destroy_object(&kb.id()).ok();
        }
    }
    if let Some(ptr) = state.pointer.take() {
        if ptr.version() >= 3 {
            ptr.release();
        } else {
            conn.backend().destroy_object(&ptr.id()).ok();
        }
    }
    if data_device.version() >= 2 {
        data_device.release();
    } else {
        conn.backend().destroy_object(&data_device.id()).ok();
    }
    if seat.version() >= 5 {
        seat.release();
    } else {
        conn.backend().destroy_object(&seat.id()).ok();
    }
    conn.backend().destroy_object(&manager.id()).ok();
    destroy_globals(conn, globals);
    queue.flush().ok();
}

fn service_io(state: &mut State, pending_read: &mut Option<PendingRead>, now: Instant) {
    let before = state.pending_sends.len();
    state.pending_sends.retain(|send| now < send.deadline);
    if state.pending_sends.len() < before {
        eprintln!(
            "rune: clipboard: send timed out after {} ms",
            MAX_IO_TIMEOUT.as_millis()
        );
    }
    if let Some(read) = pending_read.as_ref()
        && now >= read.deadline
        && let Some(expired) = pending_read.take()
    {
        eprintln!("rune: clipboard: {}", ClipboardWaylandError::IoTimeout);
        expired
            .reply
            .send(Err(ClipboardWaylandError::IoTimeout))
            .ok();
    }
}

fn step_read(pending_read: &mut Option<PendingRead>) {
    let outcome = pending_read.as_mut().and_then(PendingRead::step);
    if let Some(res) = outcome
        && let Some(done) = pending_read.take()
    {
        if let Err(err) = &res {
            eprintln!("rune: clipboard: {err}");
        }
        done.reply.send(res).ok();
    }
}

fn handle_cmd(
    cmd: IoCmd,
    state: &mut State,
    pending_read: &mut Option<PendingRead>,
    manager: &WlDataDeviceManager,
    data_device: &WlDataDevice,
    qh: &QueueHandle<State>,
    queue: &mut EventQueue<State>,
) -> bool {
    match cmd {
        IoCmd::Shutdown => false,
        IoCmd::Write { payload, reply } => {
            let Some(serial) = state.focus_serial() else {
                reply.send(Err(ClipboardWaylandError::NoFocusSerial)).ok();
                return true;
            };
            let source = manager.create_data_source(qh, ());
            payload.offer_all(&source);
            data_device.set_selection(Some(&source), serial);
            if let Some((old, _)) = state.active_source.replace((source, payload)) {
                old.destroy();
            }
            state.bump_revision();
            queue.flush().ok();
            reply.send(Ok(())).ok();
            true
        }
        IoCmd::Clear { reply } => {
            state.clear_active_source(data_device);
            queue.flush().ok();
            reply.send(Ok(())).ok();
            true
        }
        IoCmd::Read {
            mimes,
            max_bytes,
            reply,
        } => {
            if let Some(prev) = pending_read.take() {
                prev.reply.send(Err(ClipboardWaylandError::IoTimeout)).ok();
            }
            let chosen = state.current_offer.as_ref().and_then(|(offer, offered)| {
                mimes
                    .iter()
                    .find(|&&want| offered.iter().any(|m| m == want))
                    .map(|&mime| (offer.clone(), mime))
            });
            let Some((offer, mime)) = chosen else {
                reply
                    .send(Err(ClipboardWaylandError::ConnectionClosed))
                    .ok();
                return true;
            };
            let (read_fd, write_fd) = match make_nonblocking_pipe() {
                Ok(pair) => pair,
                Err(err) => {
                    reply.send(Err(err)).ok();
                    return true;
                }
            };
            offer.receive(mime.to_owned(), write_fd.as_fd());
            drop(write_fd);
            queue.flush().ok();
            *pending_read = Some(PendingRead::new(
                read_fd,
                max_bytes,
                crate::time::now(),
                reply,
            ));
            true
        }
    }
}

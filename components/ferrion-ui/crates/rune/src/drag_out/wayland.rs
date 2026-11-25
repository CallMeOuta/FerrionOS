use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::fmt;
use std::io::ErrorKind;
use std::os::fd::AsRawFd;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use wayland_backend::client::ObjectId;
use wayland_client::globals::{GlobalList, registry_queue_init};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_data_device::WlDataDevice;
use wayland_client::protocol::wl_data_device_manager::WlDataDeviceManager;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_shm::WlShm;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, EventQueue, Proxy, QueueHandle};
use winit::window::Window;

mod icon;
mod payload;
mod state;

use crate::clipboard::wayland::io::{
    MAX_POLL_FDS, WakeHandle, WakePipe, foreign_display_backend, poll_fds, poll_timeout_ms,
};
use crate::clipboard::wayland::{MAX_CLIPBOARD_BYTES, MAX_IO_TIMEOUT, MAX_URI_FILES};
use crate::drag_out::{DragImage, DragOperation, DragOutStart, DragOutcome};
use crate::platform::wayland_shm::foreign_surface_id;
use crate::runtime::Waker;
use crate::time::Instant;
use crate::window::FileStep;
use icon::DragIcon;
use payload::{ActiveDrag, DragPayload, PendingDropRead, map_clip_err};
use state::{DndState, MAX_REGISTERED_WINDOWS, RegisteredWindow};

const CMD_REPLY_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_PENDING_DONE: usize = 64;

static ATTACHED: AtomicBool = AtomicBool::new(false);
static NEXT_DRAG_ID: AtomicU64 = AtomicU64::new(1);
static ENGINE: Mutex<Option<WaylandDnd>> = Mutex::new(None);

type DoneCallback = Box<dyn FnOnce(DragOutcome)>;

thread_local! {
    static PENDING_DONE: RefCell<Vec<(u64, DoneCallback)>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DndWaylandError {
    NoDisplay,
    DisplayConnection(String),
    MissingDataDeviceManager,
    MissingSeat,
    NotWaylandWindow,
    NoPressSerial,
    UnsupportedOperation(DragOperation),
    NoOperations,
    EmptyDrag,
    DragAlreadyActive,
    PayloadTooLarge,
    TooManyUris,
    PipeFailed(String),
    ConnectionClosed,
    IoTimeout,
}

impl fmt::Display for DndWaylandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DndWaylandError::NoDisplay => f.write_str("no Wayland display attached to this process"),
            DndWaylandError::DisplayConnection(reason) => {
                write!(f, "Wayland drag-and-drop connection failed: {reason}")
            }
            DndWaylandError::MissingDataDeviceManager => {
                f.write_str("Wayland compositor does not advertise wl_data_device_manager")
            }
            DndWaylandError::MissingSeat => {
                f.write_str("Wayland compositor does not advertise wl_seat")
            }
            DndWaylandError::NotWaylandWindow => {
                f.write_str("Wayland drag-and-drop requires a Wayland window surface")
            }
            DndWaylandError::NoPressSerial => {
                f.write_str("NoPressSerial: Wayland drag requires an active pointer button press on the origin surface")
            }
            DndWaylandError::UnsupportedOperation(op) => {
                write!(f, "Wayland drag-and-drop does not support DragOperation::{op:?}")
            }
            DndWaylandError::NoOperations => {
                f.write_str("Wayland drag-and-drop requires at least one supported drag operation")
            }
            DndWaylandError::EmptyDrag => f.write_str("Wayland drag-and-drop item is empty"),
            DndWaylandError::DragAlreadyActive => {
                f.write_str("a Wayland drag-out session is already active")
            }
            DndWaylandError::PayloadTooLarge => {
                write!(
                    f,
                    "Wayland drag-and-drop payload exceeded {MAX_CLIPBOARD_BYTES} bytes"
                )
            }
            DndWaylandError::TooManyUris => {
                write!(
                    f,
                    "Wayland drag-and-drop URI list exceeded {MAX_URI_FILES} files"
                )
            }
            DndWaylandError::PipeFailed(reason) => {
                write!(f, "Wayland drag-and-drop pipe failed: {reason}")
            }
            DndWaylandError::ConnectionClosed => {
                f.write_str("Wayland drag-and-drop connection closed")
            }
            DndWaylandError::IoTimeout => {
                write!(
                    f,
                    "Wayland drag-and-drop transfer timed out after {} ms",
                    MAX_IO_TIMEOUT.as_millis()
                )
            }
        }
    }
}

enum DndCmd {
    StartDrag {
        id: u64,
        origin_surface: ObjectId,
        payload: DragPayload,
        picture: Option<DragImage>,
        reply: mpsc::Sender<Result<(), DndWaylandError>>,
    },
    CancelSurface {
        surface_id: ObjectId,
        reply: mpsc::Sender<()>,
    },
    Shutdown,
}

struct WaylandDnd {
    cmd_tx: mpsc::Sender<DndCmd>,
    wake: WakeHandle,
    windows: Arc<Mutex<HashMap<ObjectId, RegisteredWindow>>>,
    completed: Arc<Mutex<Vec<(u64, DragOutcome)>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl WaylandDnd {
    fn connect(display: NonNull<c_void>) -> Result<Self, DndWaylandError> {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (wake_pipe, wake) = WakePipe::new().map_err(map_clip_err)?;
        let windows = Arc::new(Mutex::new(HashMap::new()));
        let completed = Arc::new(Mutex::new(Vec::new()));
        let thread_windows = windows.clone();
        let thread_completed = completed.clone();
        let display_addr = display.as_ptr() as usize;
        let thread = std::thread::Builder::new()
            .name("rune-wl-dnd".into())
            .spawn(move || {
                let Some(ptr) = NonNull::new(display_addr as *mut c_void) else {
                    ready_tx.send(Err(DndWaylandError::NoDisplay)).ok();
                    return;
                };
                run_dnd_thread(
                    ptr,
                    cmd_rx,
                    wake_pipe,
                    thread_windows,
                    thread_completed,
                    ready_tx,
                );
            })
            .map_err(|e| DndWaylandError::PipeFailed(e.to_string()))?;
        match ready_rx.recv_timeout(CMD_REPLY_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                cmd_tx,
                wake,
                windows,
                completed,
                thread: Some(thread),
            }),
            Ok(Err(err)) => {
                thread.join().ok();
                Err(err)
            }
            Err(_) => Err(DndWaylandError::IoTimeout),
        }
    }

    fn shutdown(&mut self) {
        self.cmd_tx.send(DndCmd::Shutdown).ok();
        self.wake.wake();
        if let Some(handle) = self.thread.take() {
            handle.join().ok();
        }
    }
}

pub(crate) struct DropRegistration {
    surface_id: ObjectId,
}

impl Drop for DropRegistration {
    fn drop(&mut self) {
        unregister_surface(&self.surface_id);
    }
}

pub(crate) fn attach_display(display: NonNull<c_void>) {
    detach_display();
    match WaylandDnd::connect(display) {
        Ok(engine) => {
            *ENGINE.lock().unwrap_or_else(|e| e.into_inner()) = Some(engine);
            ATTACHED.store(true, Ordering::Release);
        }
        Err(err) => {
            eprintln!("rune: drag-and-drop unavailable: {err}");
            ATTACHED.store(false, Ordering::Release);
        }
    }
}

pub(crate) fn detach_display() {
    ATTACHED.store(false, Ordering::Release);
    let engine = ENGINE.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(mut engine) = engine {
        engine.shutdown();
    }
    PENDING_DONE.with(|pending| {
        for (_, done) in pending.borrow_mut().drain(..) {
            done(DragOutcome::Cancelled);
        }
    });
}

pub(crate) fn is_attached() -> bool {
    ATTACHED.load(Ordering::Acquire)
}

fn window_surface_id(window: &Window) -> Result<ObjectId, DndWaylandError> {
    let handle = window
        .window_handle()
        .map_err(|_| DndWaylandError::NotWaylandWindow)?;
    let RawWindowHandle::Wayland(wl) = handle.as_raw() else {
        return Err(DndWaylandError::NotWaylandWindow);
    };
    foreign_surface_id(wl.surface.as_ptr()).map_err(|_| DndWaylandError::NotWaylandWindow)
}

pub(crate) fn register_window(window: &Window, wake: Waker) -> Option<DropRegistration> {
    if !is_attached() {
        return None;
    }
    let surface_id = window_surface_id(window).ok()?;
    let windows = {
        let guard = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref()?.windows.clone()
    };
    let mut map = windows.lock().unwrap_or_else(|e| e.into_inner());
    if map.len() >= MAX_REGISTERED_WINDOWS && !map.contains_key(&surface_id) {
        return None;
    }
    map.insert(
        surface_id.clone(),
        RegisteredWindow {
            steps: Vec::new(),
            wake,
        },
    );
    Some(DropRegistration { surface_id })
}

fn unregister_surface(surface_id: &ObjectId) {
    cancel_surface_sync(surface_id);
    let windows = {
        let guard = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref().map(|e| e.windows.clone())
    };
    if let Some(windows) = windows {
        windows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(surface_id);
    }
    poll_completed();
}

pub(crate) fn cancel_for_window(window: &Window) {
    if !is_attached() {
        return;
    }
    if let Ok(surface_id) = window_surface_id(window) {
        cancel_surface_sync(&surface_id);
        poll_completed();
    }
}

fn cancel_surface_sync(surface_id: &ObjectId) {
    let pair = {
        let guard = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref().map(|e| (e.cmd_tx.clone(), e.wake.clone()))
    };
    let Some((cmd_tx, wake)) = pair else {
        return;
    };
    let (reply_tx, reply_rx) = mpsc::channel();
    if cmd_tx
        .send(DndCmd::CancelSurface {
            surface_id: surface_id.clone(),
            reply: reply_tx,
        })
        .is_ok()
    {
        wake.wake();
        reply_rx.recv_timeout(CMD_REPLY_TIMEOUT).ok();
    }
}

pub(crate) fn take_drop_steps(window: &Window) -> Vec<FileStep> {
    poll_completed();
    if !is_attached() {
        return Vec::new();
    }
    let Ok(surface_id) = window_surface_id(window) else {
        return Vec::new();
    };
    let windows = {
        let guard = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref().map(|e| e.windows.clone())
    };
    let Some(windows) = windows else {
        return Vec::new();
    };
    let mut map = windows.lock().unwrap_or_else(|e| e.into_inner());
    map.get_mut(&surface_id)
        .map(|entry| std::mem::take(&mut entry.steps))
        .unwrap_or_default()
}

pub(crate) fn poll_completed() {
    let completed = {
        let guard = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref().map(|e| e.completed.clone())
    };
    let Some(completed) = completed else {
        return;
    };
    let ready: Vec<(u64, DragOutcome)> = {
        let mut list = completed.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *list)
    };
    if ready.is_empty() {
        return;
    }
    for (drag_id, outcome) in ready {
        let cb = PENDING_DONE.with(|pending| {
            let mut list = pending.borrow_mut();
            list.iter()
                .position(|(id, _)| *id == drag_id)
                .map(|idx| list.remove(idx).1)
        });
        if let Some(done) = cb {
            done(outcome);
        }
    }
}

pub(super) fn begin(
    window: &Window,
    start: DragOutStart,
    picture: Option<DragImage>,
    done: Box<dyn FnOnce(DragOutcome)>,
) {
    match start_drag_inner(window, &start, picture) {
        Ok(drag_id) => {
            PENDING_DONE.with(|pending| {
                let mut list = pending.borrow_mut();
                if list.len() >= MAX_PENDING_DONE {
                    let (_, stale) = list.remove(0);
                    stale(DragOutcome::Cancelled);
                }
                list.push((drag_id, done));
            });
            poll_completed();
        }
        Err(err) => {
            eprintln!("rune: drag-out: {err}");
            done(DragOutcome::Cancelled);
        }
    }
}

fn start_drag_inner(
    window: &Window,
    start: &DragOutStart,
    picture: Option<DragImage>,
) -> Result<u64, DndWaylandError> {
    let payload = DragPayload::from_item(&start.item)?;
    let origin_surface = window_surface_id(window)?;
    let pair = {
        let guard = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref().map(|e| (e.cmd_tx.clone(), e.wake.clone()))
    };
    let Some((cmd_tx, wake)) = pair else {
        return Err(DndWaylandError::NoDisplay);
    };
    let id = NEXT_DRAG_ID.fetch_add(1, Ordering::Relaxed);
    let (reply_tx, reply_rx) = mpsc::channel();
    cmd_tx
        .send(DndCmd::StartDrag {
            id,
            origin_surface,
            payload,
            picture,
            reply: reply_tx,
        })
        .map_err(|_| DndWaylandError::ConnectionClosed)?;
    wake.wake();
    match reply_rx.recv_timeout(CMD_REPLY_TIMEOUT) {
        Ok(Ok(())) => Ok(id),
        Ok(Err(err)) => Err(err),
        Err(_) => Err(DndWaylandError::IoTimeout),
    }
}

fn destroy_globals(conn: &Connection, globals: GlobalList) {
    let id = globals.registry().id();
    drop(globals);
    conn.backend().destroy_object(&id).ok();
}

fn run_dnd_thread(
    display: NonNull<c_void>,
    cmd_rx: mpsc::Receiver<DndCmd>,
    wake_pipe: WakePipe,
    windows: Arc<Mutex<HashMap<ObjectId, RegisteredWindow>>>,
    completed: Arc<Mutex<Vec<(u64, DragOutcome)>>>,
    ready_tx: mpsc::Sender<Result<(), DndWaylandError>>,
) {
    let backend = foreign_display_backend(display);
    let conn = Connection::from_backend(backend);
    let (globals, mut queue) = match registry_queue_init::<DndState>(&conn) {
        Ok(pair) => pair,
        Err(err) => {
            ready_tx
                .send(Err(DndWaylandError::DisplayConnection(err.to_string())))
                .ok();
            return;
        }
    };
    let qh = queue.handle();
    let Ok(manager) = globals.bind::<WlDataDeviceManager, _, _>(&qh, 1..=3, ()) else {
        destroy_globals(&conn, globals);
        queue.flush().ok();
        ready_tx
            .send(Err(DndWaylandError::MissingDataDeviceManager))
            .ok();
        return;
    };
    let Ok(seat) = globals.bind::<WlSeat, _, _>(&qh, 1..=8, ()) else {
        conn.backend().destroy_object(&manager.id()).ok();
        destroy_globals(&conn, globals);
        queue.flush().ok();
        ready_tx.send(Err(DndWaylandError::MissingSeat)).ok();
        return;
    };
    let compositor = globals.bind::<WlCompositor, _, _>(&qh, 1..=6, ()).ok();
    let shm = globals.bind::<WlShm, _, _>(&qh, 1..=2, ()).ok();
    let data_device = manager.get_data_device(&seat, &qh, ());
    let mut state = DndState::new(windows, completed);
    queue.roundtrip(&mut state).ok();
    queue.roundtrip(&mut state).ok();
    ready_tx.send(Ok(())).ok();

    let mut running = true;
    while running {
        queue.dispatch_pending(&mut state).ok();
        queue.flush().ok();

        let now = crate::time::now();
        service_timeouts(&mut state, now);

        while let Ok(cmd) = cmd_rx.try_recv() {
            if !handle_cmd(
                cmd,
                &conn,
                &mut state,
                &manager,
                &data_device,
                compositor.as_ref(),
                shm.as_ref(),
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
        let read_slot = state.pending_drop_read.as_ref().map(|r| {
            let idx = pfds.len();
            pfds.push(libc::pollfd {
                fd: r.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            idx
        });
        let send_base = pfds.len();
        for (send, _) in &state.pending_sends {
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
                .map(|(s, _)| s.deadline)
                .chain(state.pending_drop_read.iter().map(|r| r.deadline)),
            now,
        );

        match poll_fds(&mut pfds, timeout_ms) {
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
                    && let Some(res) = state
                        .pending_drop_read
                        .as_mut()
                        .and_then(PendingDropRead::step)
                {
                    state.pending_drop_read = None;
                    state.finish_drop_read(res);
                }
                let mut v2_complete = false;
                for i in (0..state.pending_sends.len()).rev() {
                    let rev = pfds[send_base + i].revents;
                    let drop_send = if rev & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                        true
                    } else if rev & libc::POLLOUT != 0 {
                        state.pending_sends[i].0.try_write()
                    } else {
                        false
                    };
                    if drop_send {
                        let (_, completes_v2) = state.pending_sends.swap_remove(i);
                        v2_complete |= completes_v2;
                    }
                }
                if v2_complete
                    && let Some(outcome) = state
                        .active_drag
                        .as_ref()
                        .filter(|d| !d.v3)
                        .map(|d| d.payload.v2_default_outcome())
                {
                    state.resolve_active_drag(outcome);
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

    cleanup_dnd(
        &conn,
        globals,
        &mut queue,
        &mut state,
        &manager,
        &seat,
        &data_device,
        compositor,
        shm,
    );
}

fn service_timeouts(state: &mut DndState, now: Instant) {
    let before = state.pending_sends.len();
    state.pending_sends.retain(|(send, _)| now < send.deadline);
    if state.pending_sends.len() < before {
        eprintln!(
            "rune: drag-out: send timed out after {} ms",
            MAX_IO_TIMEOUT.as_millis()
        );
    }
    if state
        .pending_drop_read
        .as_ref()
        .is_some_and(|r| now >= r.deadline)
    {
        state.pending_drop_read = None;
        state.finish_drop_read(Err(DndWaylandError::IoTimeout));
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_cmd(
    cmd: DndCmd,
    conn: &Connection,
    state: &mut DndState,
    manager: &WlDataDeviceManager,
    data_device: &WlDataDevice,
    compositor: Option<&WlCompositor>,
    shm: Option<&WlShm>,
    qh: &QueueHandle<DndState>,
    queue: &mut EventQueue<DndState>,
) -> bool {
    match cmd {
        DndCmd::Shutdown => false,
        DndCmd::CancelSurface { surface_id, reply } => {
            state.cancel_for_surface(&surface_id);
            queue.flush().ok();
            reply.send(()).ok();
            true
        }
        DndCmd::StartDrag {
            id,
            origin_surface,
            payload,
            picture,
            reply,
        } => {
            queue.dispatch_pending(state).ok();
            if state.active_drag.is_some() {
                reply.send(Err(DndWaylandError::DragAlreadyActive)).ok();
                return true;
            }
            let grab_matches = state
                .press_grab
                .as_ref()
                .is_some_and(|(sid, s)| sid == &origin_surface && *s != 0);
            if !grab_matches {
                queue.roundtrip(state).ok();
            }
            let serial = match state.press_grab.take() {
                Some((sid, serial)) if sid == origin_surface && serial != 0 => serial,
                _ => {
                    reply.send(Err(DndWaylandError::NoPressSerial)).ok();
                    return true;
                }
            };
            let Ok(origin) = WlSurface::from_id(conn, origin_surface.clone()) else {
                reply.send(Err(DndWaylandError::NotWaylandWindow)).ok();
                return true;
            };
            let source = manager.create_data_source(qh, ());
            payload.offer_all(&source);
            let v3 = source.version() >= 3;
            if v3 {
                source.set_actions(payload.actions);
            }
            let icon = match (compositor, shm, picture.as_ref()) {
                (Some(comp), Some(wl_shm), Some(img)) => {
                    DragIcon::from_image(comp, wl_shm, qh, img)
                }
                _ => None,
            };
            data_device.start_drag(
                Some(&source),
                &origin,
                icon.as_ref().map(DragIcon::surface),
                serial,
            );
            state.active_drag = Some(ActiveDrag {
                id,
                origin_surface,
                source,
                payload,
                icon,
                v3,
                selected_action: wayland_client::protocol::wl_data_device_manager::DndAction::None,
                drop_performed: false,
            });
            queue.flush().ok();
            reply.send(Ok(())).ok();
            true
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn cleanup_dnd(
    conn: &Connection,
    globals: GlobalList,
    queue: &mut EventQueue<DndState>,
    state: &mut DndState,
    manager: &WlDataDeviceManager,
    seat: &WlSeat,
    data_device: &WlDataDevice,
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
) {
    state.pending_sends.clear();
    state.pending_drop_read = None;
    state.resolve_active_drag(DragOutcome::Cancelled);
    if let Some(drop_sess) = state.active_drop.take() {
        drop_sess.offer.destroy();
    }
    for (offer, _) in state.pending_offers.drain(..) {
        offer.destroy();
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
    if let Some(comp) = compositor {
        conn.backend().destroy_object(&comp.id()).ok();
    }
    if let Some(wl_shm) = shm {
        conn.backend().destroy_object(&wl_shm.id()).ok();
    }
    conn.backend().destroy_object(&manager.id()).ok();
    destroy_globals(conn, globals);
    queue.flush().ok();
}

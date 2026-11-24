use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, DispatchError, EventQueue, Proxy, QueueHandle};
use winit::window::Window;

use crate::runtime::Waker;
use crate::time::Instant;

pub(crate) mod slots;
pub(crate) mod watch;
pub(crate) use slots::{CpuPresentError, ShmRing, SlotAcquire};
use watch::ShmWatch;

pub(crate) const MAX_DRAIN_PASSES: usize = 64;

#[derive(Default)]
struct ShmState {
    released: Vec<u64>,
    frame_done_id: Option<u64>,
    frame_callback: Option<(u64, WlCallback)>,
    next_frame_id: u64,
}

impl Dispatch<WlRegistry, GlobalListContents> for ShmState {
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

impl Dispatch<WlShm, ()> for ShmState {
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

impl Dispatch<WlShmPool, ()> for ShmState {
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

impl Dispatch<WlBuffer, u64> for ShmState {
    fn event(
        state: &mut Self,
        _: &WlBuffer,
        event: wl_buffer::Event,
        id: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_buffer::Event::Release) && state.released.len() < MAX_DRAIN_PASSES {
            state.released.push(*id);
        }
    }
}

impl Dispatch<WlCallback, u64> for ShmState {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        id: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_callback::Event::Done { .. }) {
            state.frame_done_id = Some(*id);
            if state
                .frame_callback
                .as_ref()
                .is_some_and(|(cur, _)| *cur == *id)
            {
                state.frame_callback = None;
            }
        }
    }
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn foreign_display_backend(display: *mut std::ffi::c_void) -> Backend {
    assert!(!display.is_null());
    unsafe { Backend::from_foreign_display(display.cast()) }
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn foreign_surface_id(surface: *mut std::ffi::c_void) -> Result<ObjectId, CpuPresentError> {
    assert!(!surface.is_null());
    unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.cast()) }
        .map_err(|_| CpuPresentError::NotWayland)
}

pub(crate) struct WaylandShm {
    conn: Connection,
    globals: Option<GlobalList>,
    shm: WlShm,
    surface: WlSurface,
    queue: EventQueue<ShmState>,
    state: ShmState,
    ring: ShmRing,
    watch: Option<ShmWatch>,
}

impl WaylandShm {
    pub(crate) fn new(window: &Window, wake: Waker) -> Result<Self, CpuPresentError> {
        let display_raw = window
            .display_handle()
            .map_err(|_| CpuPresentError::NotWayland)?
            .as_raw();
        let window_raw = window
            .window_handle()
            .map_err(|_| CpuPresentError::NotWayland)?
            .as_raw();
        let (RawDisplayHandle::Wayland(display), RawWindowHandle::Wayland(handle)) =
            (display_raw, window_raw)
        else {
            return Err(CpuPresentError::NotWayland);
        };
        let backend = foreign_display_backend(display.display.as_ptr());
        let conn = Connection::from_backend(backend);
        let (globals, queue) = registry_queue_init::<ShmState>(&conn)
            .map_err(|e| CpuPresentError::DisplayConnection(e.to_string()))?;
        let shm = globals
            .bind::<WlShm, _, _>(&queue.handle(), 1..=2, ())
            .or_else(|_| globals.bind::<WlShm, _, _>(&queue.handle(), 1..=1, ()))
            .map_err(|_| CpuPresentError::MissingWlShm)?;
        let id = foreign_surface_id(handle.surface.as_ptr())?;
        let surface = WlSurface::from_id(&conn, id).map_err(|_| CpuPresentError::NotWayland)?;
        let watch = ShmWatch::spawn(conn.clone(), wake);
        Ok(Self {
            conn,
            globals: Some(globals),
            shm,
            surface,
            queue,
            state: ShmState::default(),
            ring: ShmRing::new(wl_shm::Format::Xrgb8888),
            watch,
        })
    }

    pub(crate) fn dispatch(&mut self) -> bool {
        for _ in 0..MAX_DRAIN_PASSES {
            match self.queue.dispatch_pending(&mut self.state) {
                Ok(0) => break,
                Ok(_) | Err(DispatchError::BadMessage { .. }) => {}
                Err(_) => break,
            }
        }
        let released_any = self.ring.mark_released(self.state.released.drain(..));
        let frame_done = self.state.frame_done_id.take().is_some();
        if let Some(watch) = &self.watch {
            watch.turned();
        }
        (released_any || frame_done) && self.ring.deferred_dirty()
    }

    pub(crate) fn waiting_for_slot(&self) -> bool {
        self.ring.waiting_for_slot()
    }

    pub(crate) fn stall_deadline(&self) -> Option<Instant> {
        self.ring.stall_deadline()
    }

    pub(crate) fn has_presented(&self) -> bool {
        self.ring.has_presented()
    }

    pub(crate) fn acquire_slot(
        &mut self,
        width: u32,
        height: u32,
        now: Instant,
    ) -> Result<SlotAcquire, CpuPresentError> {
        let _ = self.dispatch();
        let qh = self.queue.handle();
        self.ring.acquire_slot(&self.shm, &qh, width, height, now)
    }

    pub(crate) fn slot_target_mut(&mut self, slot: usize) -> Option<(&mut [u8], u32, u32, usize)> {
        self.ring.slot_target_mut(slot)
    }

    pub(crate) fn present(
        &mut self,
        slot: usize,
        rects: &[[u32; 4]],
        scale: i32,
        force_full_damage: bool,
    ) -> Result<(), CpuPresentError> {
        self.ring
            .attach_and_damage(&self.surface, slot, rects, scale, force_full_damage)?;
        self.state.next_frame_id = self.state.next_frame_id.wrapping_add(1).max(1);
        let frame_id = self.state.next_frame_id;
        if let Some((_, prev_cb)) = self.state.frame_callback.take() {
            self.conn.backend().destroy_object(&prev_cb.id()).ok();
        }
        let cb = self.surface.frame(&self.queue.handle(), frame_id);
        self.state.frame_callback = Some((frame_id, cb));
        self.surface.commit();
        self.queue
            .flush()
            .map_err(|e| CpuPresentError::DisplayConnection(e.to_string()))?;
        self.ring.mark_presented(slot);
        Ok(())
    }
}

impl Drop for WaylandShm {
    fn drop(&mut self) {
        self.watch = None;
        for _ in 0..MAX_DRAIN_PASSES {
            match self.queue.dispatch_pending(&mut self.state) {
                Ok(0) => break,
                Ok(_) | Err(DispatchError::BadMessage { .. }) => {}
                Err(_) => break,
            }
        }
        let backend = self.conn.backend();
        if let Some((_, cb)) = self.state.frame_callback.take() {
            backend.destroy_object(&cb.id()).ok();
        }
        self.ring.clear_slots();
        backend.destroy_object(&self.shm.id()).ok();
        if let Some(globals) = self.globals.take() {
            let reg_id = globals.registry().id();
            globals.destroy();
            backend.destroy_object(&reg_id).ok();
        }
        for _ in 0..MAX_DRAIN_PASSES {
            match self.queue.dispatch_pending(&mut self.state) {
                Ok(0) => break,
                Ok(_) | Err(DispatchError::BadMessage { .. }) => {}
                Err(_) => break,
            }
        }
        self.queue.flush().ok();
    }
}

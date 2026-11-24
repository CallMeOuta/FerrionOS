use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::Duration;

use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::globals::{GlobalList, registry_queue_init};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_shm::WlShm;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, EventQueue, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::xdg_popup::XdgPopup;
use wayland_protocols::xdg::shell::client::xdg_positioner::{
    Anchor, ConstraintAdjustment, Gravity,
};
use wayland_protocols::xdg::shell::client::xdg_surface::XdgSurface;
use wayland_protocols::xdg::shell::client::xdg_wm_base::XdgWmBase;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::ZwlrLayerSurfaceV1;

use crate::platform::wayland_shm::watch::ShmWatch;
use crate::runtime::Waker;

use super::dispatch::{ProtoEvent, ProtoState};
use super::{MAX_CONFIGURE_WAIT, PopupWaylandError};

const CONFIGURE_POLL_STEP: Duration = Duration::from_millis(5);
const MAX_CONFIGURE_ITERS: usize = 512;

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
pub(super) fn foreign_display_backend(display: NonNull<c_void>) -> Backend {
    assert!(display.as_ptr().addr() != 0);
    unsafe { Backend::from_foreign_display(display.as_ptr().cast()) }
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn foreign_xdg_surface_id(ptr: NonNull<c_void>) -> Result<ObjectId, PopupWaylandError> {
    assert!(ptr.as_ptr().addr() != 0);
    unsafe { ObjectId::from_ptr(XdgSurface::interface(), ptr.as_ptr().cast()) }
        .map_err(|_| PopupWaylandError::MissingParentSurface)
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn foreign_layer_surface_id(ptr: NonNull<c_void>) -> Result<ObjectId, PopupWaylandError> {
    assert!(ptr.as_ptr().addr() != 0);
    unsafe { ObjectId::from_ptr(ZwlrLayerSurfaceV1::interface(), ptr.as_ptr().cast()) }
        .map_err(|_| PopupWaylandError::MissingParentSurface)
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
pub(super) fn create_wgpu_surface(
    instance: &wgpu::Instance,
    display: RawDisplayHandle,
    window: RawWindowHandle,
) -> Result<wgpu::Surface<'static>, PopupWaylandError> {
    assert!(matches!(display, RawDisplayHandle::Wayland(_)));
    assert!(matches!(window, RawWindowHandle::Wayland(_)));
    unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(display),
            raw_window_handle: window,
        })
    }
    .map_err(|e| PopupWaylandError::SurfaceCreate(e.to_string()))
}

pub(super) fn probe_xdg_wm_base(display: NonNull<c_void>) -> bool {
    let backend = foreign_display_backend(display);
    let conn = Connection::from_backend(backend);
    let Ok((globals, queue)) = registry_queue_init::<ProtoState>(&conn) else {
        return false;
    };
    let qh = queue.handle();
    let has_wm_base = globals.bind::<XdgWmBase, _, _>(&qh, 1..=6, ()).ok();
    let has_compositor = globals.bind::<WlCompositor, _, _>(&qh, 1..=6, ()).ok();
    let has_seat = globals.bind::<WlSeat, _, _>(&qh, 1..=9, ()).ok();
    let ok = has_wm_base.is_some() && has_compositor.is_some() && has_seat.is_some();
    if let Some(wm) = has_wm_base {
        wm.destroy();
    }
    if let Some(comp) = has_compositor {
        conn.backend().destroy_object(&comp.id()).ok();
    }
    if let Some(seat) = has_seat {
        if seat.version() >= 5 {
            seat.release();
        } else {
            conn.backend().destroy_object(&seat.id()).ok();
        }
    }
    let reg_id = globals.registry().id();
    drop(globals);
    conn.backend().destroy_object(&reg_id).ok();
    queue.flush().ok();
    ok
}

pub(super) enum PopupParent<'a> {
    WindowXdg(NonNull<c_void>),
    PopupXdg(&'a XdgSurface),
    Layer(NonNull<c_void>),
}

pub(super) struct PopupPlacement {
    pub(super) size: (i32, i32),
    pub(super) anchor_rect: [i32; 4],
    pub(super) anchor: Anchor,
    pub(super) gravity: Gravity,
}

pub(super) struct PopupSurfaceHandles {
    pub(super) level_id: u64,
    pub(super) wl_surface: WlSurface,
    pub(super) xdg_surface: XdgSurface,
    pub(super) xdg_popup: XdgPopup,
    pub(super) configured_size: (i32, i32),
}

impl PopupSurfaceHandles {
    pub(super) fn raw_handles(
        &self,
        display: NonNull<c_void>,
    ) -> (RawDisplayHandle, RawWindowHandle) {
        let surface =
            NonNull::new(self.wl_surface.id().as_ptr().cast()).unwrap_or_else(|| unreachable!());
        (
            RawDisplayHandle::Wayland(WaylandDisplayHandle::new(display)),
            RawWindowHandle::Wayland(WaylandWindowHandle::new(surface)),
        )
    }

    pub(super) fn surface_protocol_id(&self) -> u32 {
        self.wl_surface.id().protocol_id()
    }

    pub(super) fn destroy(self) {
        self.xdg_popup.destroy();
        self.xdg_surface.destroy();
        self.wl_surface.destroy();
    }
}

pub(super) struct PopupConn {
    pub(super) display: NonNull<c_void>,
    pub(super) conn: Connection,
    globals: Option<GlobalList>,
    pub(super) queue: EventQueue<ProtoState>,
    pub(super) state: ProtoState,
    compositor: WlCompositor,
    pub(super) shm: Option<WlShm>,
    seat: WlSeat,
    wm_base: XdgWmBase,
    watch: Option<ShmWatch>,
    next_level_id: u64,
}

impl PopupConn {
    pub(super) fn new(
        display: NonNull<c_void>,
        wake: Option<Waker>,
    ) -> Result<Self, PopupWaylandError> {
        let backend = foreign_display_backend(display);
        let conn = Connection::from_backend(backend);
        let (globals, mut queue) = registry_queue_init::<ProtoState>(&conn)
            .map_err(|e| PopupWaylandError::SurfaceCreate(e.to_string()))?;
        let qh = queue.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&qh, 1..=6, ())
            .map_err(|_| PopupWaylandError::MissingGlobal("wl_compositor"))?;
        let wm_base = globals
            .bind::<XdgWmBase, _, _>(&qh, 1..=6, ())
            .map_err(|_| PopupWaylandError::MissingGlobal("xdg_wm_base"))?;
        let seat = globals
            .bind::<WlSeat, _, _>(&qh, 1..=9, ())
            .map_err(|_| PopupWaylandError::MissingGlobal("wl_seat"))?;
        let shm = globals.bind::<WlShm, _, _>(&qh, 1..=2, ()).ok();
        let mut state = ProtoState::default();
        queue.roundtrip(&mut state).ok();
        if state.keyboard.is_some() {
            queue.roundtrip(&mut state).ok();
        }
        let watch = wake.and_then(|w| ShmWatch::spawn(conn.clone(), w));
        Ok(Self {
            display,
            conn,
            globals: Some(globals),
            queue,
            state,
            compositor,
            shm,
            seat,
            wm_base,
            watch,
            next_level_id: 1,
        })
    }

    pub(super) fn qh(&self) -> QueueHandle<ProtoState> {
        self.queue.handle()
    }

    pub(super) fn create_popup(
        &mut self,
        parent: PopupParent<'_>,
        placement: PopupPlacement,
        grab_serial: u32,
        scale: i32,
    ) -> Result<PopupSurfaceHandles, PopupWaylandError> {
        if grab_serial == 0 {
            return Err(PopupWaylandError::NoInputSerial);
        }
        let qh = self.queue.handle();
        let level_id = self.next_level_id;
        self.next_level_id = self.next_level_id.wrapping_add(1).max(1);

        let positioner = self.wm_base.create_positioner(&qh, ());
        positioner.set_size(placement.size.0.max(1), placement.size.1.max(1));
        positioner.set_anchor_rect(
            placement.anchor_rect[0],
            placement.anchor_rect[1],
            placement.anchor_rect[2].max(1),
            placement.anchor_rect[3].max(1),
        );
        positioner.set_anchor(placement.anchor);
        positioner.set_gravity(placement.gravity);
        positioner.set_constraint_adjustment(
            ConstraintAdjustment::FlipX
                | ConstraintAdjustment::FlipY
                | ConstraintAdjustment::SlideX
                | ConstraintAdjustment::SlideY,
        );

        let wl_surface = self.compositor.create_surface(&qh, level_id);
        let xdg_surface = self.wm_base.get_xdg_surface(&wl_surface, &qh, level_id);
        let xdg_popup = match parent {
            PopupParent::WindowXdg(ptr) => {
                let id = foreign_xdg_surface_id(ptr)?;
                let parent_xdg = XdgSurface::from_id(&self.conn, id)
                    .map_err(|_| PopupWaylandError::MissingParentSurface)?;
                xdg_surface.get_popup(Some(&parent_xdg), &positioner, &qh, level_id)
            }
            PopupParent::PopupXdg(parent_xdg) => {
                xdg_surface.get_popup(Some(parent_xdg), &positioner, &qh, level_id)
            }
            PopupParent::Layer(ptr) => {
                let id = foreign_layer_surface_id(ptr)?;
                let parent_layer = ZwlrLayerSurfaceV1::from_id(&self.conn, id)
                    .map_err(|_| PopupWaylandError::MissingParentSurface)?;
                let popup = xdg_surface.get_popup(None, &positioner, &qh, level_id);
                parent_layer.get_popup(&popup);
                popup
            }
        };
        positioner.destroy();

        xdg_popup.grab(&self.seat, grab_serial);
        if wl_surface.version() >= 3 {
            wl_surface.set_buffer_scale(scale.max(1));
        }
        wl_surface.commit();
        self.queue.flush().ok();

        let deadline = crate::time::now() + MAX_CONFIGURE_WAIT;
        let mut configured = false;
        let mut configured_size = placement.size;
        for _ in 0..MAX_CONFIGURE_ITERS {
            if self.watch.is_none() {
                self.queue.roundtrip(&mut self.state).ok();
            }
            self.dispatch_once();
            if self.state.take_configured(level_id) {
                configured = true;
                for ev in &self.state.events {
                    if let ProtoEvent::PopupConfigured {
                        level_id: id,
                        rect: (_, _, w, h),
                    } = *ev
                        && id == level_id
                        && w > 0
                        && h > 0
                    {
                        configured_size = (w, h);
                    }
                }
                break;
            }
            let dismissed =
                self.state.events.iter().any(
                    |ev| matches!(ev, ProtoEvent::PopupDone { level_id: id } if *id == level_id),
                );
            if dismissed {
                xdg_popup.destroy();
                xdg_surface.destroy();
                wl_surface.destroy();
                self.queue.flush().ok();
                return Err(PopupWaylandError::DismissedBeforeConfigure);
            }
            if crate::time::now() >= deadline {
                xdg_popup.destroy();
                xdg_surface.destroy();
                wl_surface.destroy();
                self.queue.flush().ok();
                return Err(PopupWaylandError::ConfigureTimeout);
            }
            std::thread::sleep(CONFIGURE_POLL_STEP);
        }
        if !configured {
            xdg_popup.destroy();
            xdg_surface.destroy();
            wl_surface.destroy();
            self.queue.flush().ok();
            return Err(PopupWaylandError::ConfigureTimeout);
        }

        xdg_surface.set_window_geometry(0, 0, configured_size.0.max(1), configured_size.1.max(1));
        self.queue.flush().ok();

        Ok(PopupSurfaceHandles {
            level_id,
            wl_surface,
            xdg_surface,
            xdg_popup,
            configured_size,
        })
    }

    pub(super) fn dispatch_once(&mut self) {
        self.queue.dispatch_pending(&mut self.state).ok();
        self.queue.flush().ok();
        if let Some(watch) = &self.watch {
            watch.turned();
        }
    }

    pub(super) fn flush(&mut self) {
        self.queue.flush().ok();
    }
}

impl Drop for PopupConn {
    fn drop(&mut self) {
        self.watch = None;
        self.queue.dispatch_pending(&mut self.state).ok();
        let backend = self.conn.backend();
        if let Some(ptr) = self.state.pointer.take() {
            if ptr.version() >= 3 {
                ptr.release();
            } else {
                backend.destroy_object(&ptr.id()).ok();
            }
        }
        if let Some(kb) = self.state.keyboard.take() {
            if kb.version() >= 3 {
                kb.release();
            } else {
                backend.destroy_object(&kb.id()).ok();
            }
        }
        if self.seat.version() >= 5 {
            self.seat.release();
        } else {
            backend.destroy_object(&self.seat.id()).ok();
        }
        if let Some(shm) = self.shm.take() {
            backend.destroy_object(&shm.id()).ok();
        }
        self.wm_base.destroy();
        backend.destroy_object(&self.compositor.id()).ok();
        if let Some(globals) = self.globals.take() {
            let id = globals.registry().id();
            drop(globals);
            backend.destroy_object(&id).ok();
        }
        self.queue.dispatch_pending(&mut self.state).ok();
        self.queue.flush().ok();
    }
}

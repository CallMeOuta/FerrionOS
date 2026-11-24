use std::fmt;
use std::time::Duration;

use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Anchor;

use super::super::layer_shell_machine::{
    connection, frame_callback, readiness, repeat, resolved_exclusive_zone,
};
use super::super::wayland_shm::{CpuPresentError, MAX_DRAIN_PASSES, ShmRing, SlotAcquire};
use super::{LayerSurface, State, classify_turn_error};
use crate::el::Align;
use crate::overlay::{Placement, ScreenEdge};
use crate::time::Instant;
use crate::{OverlayLayer, OverlayPlacement};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LayerSurfaceError {
    NotWayland,
    DisplayConnection(String),
    MissingCompositor,
    MissingLayerShell,
    ConfigureTimeout(Duration),
    Closed,
}

impl fmt::Display for LayerSurfaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LayerSurfaceError::NotWayland => {
                f.write_str("Wayland layer surface requires a Wayland display")
            }
            LayerSurfaceError::DisplayConnection(reason) => {
                write!(f, "Wayland display connection failed: {reason}")
            }
            LayerSurfaceError::MissingCompositor => {
                f.write_str("Wayland compositor does not advertise wl_compositor")
            }
            LayerSurfaceError::MissingLayerShell => {
                f.write_str("Wayland compositor does not advertise zwlr_layer_shell_v1")
            }
            LayerSurfaceError::ConfigureTimeout(timeout) => write!(
                f,
                "Wayland layer surface timed out waiting for initial configure after {} ms",
                timeout.as_millis()
            ),
            LayerSurfaceError::Closed => f.write_str("Wayland compositor closed the layer surface"),
        }
    }
}

pub(super) fn protocol_layer(layer: OverlayLayer) -> Layer {
    match layer {
        OverlayLayer::Background => Layer::Background,
        OverlayLayer::Bottom => Layer::Bottom,
        OverlayLayer::Top => Layer::Top,
        OverlayLayer::Overlay => Layer::Overlay,
    }
}

impl Dispatch<WlShm, ()> for State {
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

impl Dispatch<WlShmPool, ()> for State {
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

impl Dispatch<WlBuffer, u64> for State {
    fn event(
        state: &mut Self,
        _: &WlBuffer,
        event: wl_buffer::Event,
        id: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_buffer::Event::Release)
            && state.released_buffers.len() < MAX_DRAIN_PASSES
        {
            state.released_buffers.push(*id);
        }
    }
}

impl LayerSurface {
    pub fn set_placement(&mut self, placement: Option<OverlayPlacement>) {
        self.options.placement = placement;
    }

    pub fn place(&mut self, at: Placement) {
        self.layer.set_exclusive_zone(resolved_exclusive_zone(
            self.options.placement,
            self.exclusive_zone,
        ));
        match self.options.placement {
            Some(OverlayPlacement::FullScreen) => {
                self.layer.set_size(0, 0);
                self.layer
                    .set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
                self.layer.set_margin(0, 0, 0, 0);
                return;
            }
            Some(OverlayPlacement::Rect {
                x,
                y,
                width,
                height,
            }) => {
                self.layer
                    .set_size(width.ceil().max(1.0) as u32, height.ceil().max(1.0) as u32);
                self.layer.set_anchor(Anchor::Top | Anchor::Left);
                self.layer
                    .set_margin(y.round() as i32, 0, 0, x.round() as i32);
                return;
            }
            _ => {}
        }
        let at = match self.options.placement {
            Some(OverlayPlacement::Edge {
                edge,
                margin,
                align,
                inset,
            }) => Placement {
                size: at.size,
                edge,
                margin,
                align,
                inset,
            },
            _ => at,
        };
        let (margin, inset) = (at.margin.round() as i32, at.inset.round() as i32);
        let (vertical, top, bottom) = match at.edge {
            ScreenEdge::Top => (Anchor::Top, margin, 0),
            ScreenEdge::Bottom => (Anchor::Bottom, 0, margin),
        };
        if self.span_width {
            self.layer.set_size(0, at.size.1.ceil().max(1.0) as u32);
            self.layer
                .set_anchor(vertical | Anchor::Left | Anchor::Right);
            self.layer.set_margin(top, inset, bottom, inset);
            return;
        }
        let (horizontal, left, right) = match at.align {
            Align::Start => (Anchor::Left, inset, 0),
            Align::Center => (Anchor::empty(), 0, 0),
            Align::End => (Anchor::Right, 0, inset),
        };
        self.layer
            .set_size(at.size.0.ceil() as u32, at.size.1.ceil() as u32);
        self.layer.set_anchor(vertical | horizontal);
        self.layer.set_margin(top, right, bottom, left);
    }

    pub fn configured_size(&self) -> Option<(u32, u32)> {
        self.state.readiness.size()
    }

    pub(super) fn drain_released_buffers(&mut self) {
        if let Some(ring) = &mut self.shm_ring {
            let _ = ring.mark_released(self.state.released_buffers.drain(..));
        } else {
            self.state.released_buffers.clear();
        }
    }

    pub(crate) fn enable_cpu_shm(&mut self, format: wl_shm::Format) -> Result<(), CpuPresentError> {
        if self.shm.is_none() {
            let Some(globals) = &self.globals else {
                return Err(CpuPresentError::MissingWlShm);
            };
            let qh = self.queue.handle();
            let shm = globals
                .bind::<WlShm, _, _>(&qh, 1..=2, ())
                .or_else(|_| globals.bind::<WlShm, _, _>(&qh, 1..=1, ()))
                .map_err(|_| CpuPresentError::MissingWlShm)?;
            self.shm = Some(shm);
        }
        self.shm_ring = Some(ShmRing::new(format));
        Ok(())
    }

    pub(crate) fn acquire_cpu_slot(
        &mut self,
        width: u32,
        height: u32,
        now: Instant,
    ) -> Result<SlotAcquire, CpuPresentError> {
        self.drain_released_buffers();
        let Some(shm) = &self.shm else {
            return Err(CpuPresentError::MissingWlShm);
        };
        let Some(ring) = &mut self.shm_ring else {
            return Err(CpuPresentError::MissingWlShm);
        };
        let qh = self.queue.handle();
        ring.acquire_slot(shm, &qh, width, height, now)
    }

    pub(crate) fn cpu_slot_target_mut(
        &mut self,
        slot: usize,
    ) -> Option<(&mut [u8], u32, u32, usize)> {
        self.shm_ring.as_mut()?.slot_target_mut(slot)
    }

    pub(crate) fn present_cpu(
        &mut self,
        slot: usize,
        rects: &[[u32; 4]],
        scale: i32,
        force_full_damage: bool,
    ) -> Result<(), CpuPresentError> {
        let Some(ring) = &mut self.shm_ring else {
            return Err(CpuPresentError::MissingWlShm);
        };
        ring.attach_and_damage(&self.surface, slot, rects, scale, force_full_damage)?;
        self.request_frame();
        self.surface.commit();
        self.flush_checked();
        if let Some(ring) = &mut self.shm_ring {
            ring.mark_presented(slot);
        }
        Ok(())
    }

    pub(crate) fn shm_stall_deadline(&self) -> Option<Instant> {
        self.shm_ring.as_ref().and_then(ShmRing::stall_deadline)
    }

    pub(crate) fn shm_has_presented(&self) -> bool {
        self.shm_ring.as_ref().is_some_and(ShmRing::has_presented)
    }

    pub(super) fn turn(&mut self) {
        let was_closed = self.state.readiness.closed();
        if was_closed {
            self.watch = None;
            return;
        }
        let dispatch_err = self.queue.dispatch_pending(&mut self.state).err();
        let flush_err = self.queue.flush().err();
        let last_err = self.conn.backend().last_error();
        let (kind, reason) = classify_turn_error(dispatch_err, flush_err, last_err);
        match connection::turn_action(was_closed, kind, self.state.readiness.closed()) {
            connection::TurnAction::Continue => {
                if let Some(watch) = &self.watch {
                    watch.turned();
                }
            }
            connection::TurnAction::StopClosed | connection::TurnAction::AlreadyClosed => {
                self.watch = None;
            }
            connection::TurnAction::FailAndClose => self.fail_connection(reason),
        }
    }

    pub(super) fn flush_checked(&mut self) {
        if self.state.readiness.closed() {
            return;
        }
        let flush_err = self.queue.flush().err();
        let (kind, reason) = classify_turn_error(None, flush_err, self.conn.backend().last_error());
        if kind.is_some_and(connection::is_fatal) {
            self.fail_connection(reason);
        }
    }

    pub(super) fn fail_connection(&mut self, reason: Option<String>) {
        let was_closed = self.state.readiness.closed();
        readiness::step(&mut self.state.readiness, readiness::Event::Closed);
        frame_callback::step(&mut self.state.frame, frame_callback::Event::Reset);
        if let Some((_, cb)) = self.state.frame_callback.take() {
            self.conn.backend().destroy_object(&cb.id()).ok();
        }
        self.state.step_repeat(repeat::Event::Stop);
        self.state.input.clear();
        self.watch = None;
        if !was_closed {
            match reason {
                Some(reason) => eprintln!(
                    "rune: Wayland layer surface '{}' closed: the display connection failed: {reason}",
                    self.namespace
                ),
                None => eprintln!(
                    "rune: Wayland layer surface '{}' closed: the display connection failed",
                    self.namespace
                ),
            }
        }
    }
}

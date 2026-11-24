use wayland_client::protocol::wl_shm;

use super::app::OverlayApp;
use super::frame::{idle_after, step};
use super::surface::overlay_rescale_size;
use super::{Backing, Live, OverlayCore, OverlayRenderTarget, frame};
use crate::platform::layer_shell::LayerSurface;
use crate::platform::wayland_shm::{CpuPresentError, SlotAcquire};
use crate::render::cpu_boot::{CpuPainter, NeedsGpu, Scene, Target};
use crate::window::wayland_cpu::{needs_gpu_kind, painter_threads};

pub(super) struct OverlayCpu {
    painter: CpuPainter,
    logged_refusals: u16,
}

impl OverlayCpu {
    pub(super) fn new() -> Self {
        Self {
            painter: CpuPainter::new(painter_threads()),
            logged_refusals: 0,
        }
    }

    fn record_refusal(&mut self, why: NeedsGpu, name: &str) -> &'static str {
        let (bit, kind) = needs_gpu_kind(why);
        if self.logged_refusals & bit == 0 {
            self.logged_refusals |= bit;
            eprintln!("rune: cpu-present:needs-gpu:{kind} (overlay {name}, keeping last frame)");
        }
        kind
    }
}

impl OverlayCore {
    pub(super) fn attach_layer_cpu(
        &mut self,
        app: &mut dyn OverlayApp,
        mut layer: LayerSurface,
        scale: i32,
    ) -> Option<Live> {
        if let Err(err) = layer.enable_cpu_shm(wl_shm::Format::Argb8888) {
            self.fail(err.to_string());
            return None;
        }
        let a11y = self
            .shared
            .wake
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .and_then(crate::a11y::Adapter::windowless);
        let scale_f = scale.max(1) as f32;
        let (w, h) =
            overlay_rescale_size(self.at.size, scale_f, crate::window::CPU_MAX_SURFACE_DIM);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST,
            format: wgpu::TextureFormat::Bgra8Unorm,
            width: w,
            height: h,
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: crate::window::surface::FRAME_LATENCY,
            alpha_mode: wgpu::CompositeAlphaMode::PreMultiplied,
            view_formats: vec![],
            color_space: Default::default(),
        };
        let rt = app.rt_mut();
        crate::render::cpu_boot::attach_text(&mut rt.text.atlas);
        rt.transparent = true;
        rt.set_viewport(self.at.size, scale_f);
        *self
            .shared
            .surface
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some("CPU wl_shm Argb8888".into());
        Some(Live {
            a11y,
            target: OverlayRenderTarget::Cpu(Box::new(OverlayCpu::new())),
            config,
            backing: Backing::Layer(layer),
            scale: scale_f,
        })
    }

    pub(super) fn rescale_cpu(&mut self, app: &mut dyn OverlayApp, scale: f32) {
        let Some(live) = self.surface.live_mut() else {
            return;
        };
        let scale = scale.max(1.0);
        if (live.scale - scale).abs() < f32::EPSILON {
            return;
        }
        live.scale = scale;
        (live.config.width, live.config.height) =
            overlay_rescale_size(self.at.size, scale, crate::window::CPU_MAX_SURFACE_DIM);
        app.rt_mut().set_viewport(self.at.size, scale);
        self.dirty = true;
    }

    pub(super) fn draw_wayland_cpu(&mut self, app: &mut dyn OverlayApp, prewarm: bool) -> bool {
        let _ = self.quiet.step(frame::QuietEvent::FrameStarts);
        if let Some(Live {
            backing: Backing::Layer(layer),
            ..
        }) = self.surface.live_mut()
        {
            let changes = layer.dispatch();
            if changes.closed {
                self.fail("Wayland compositor closed the layer surface".into());
                self.hide(app);
                return false;
            }
            for input in changes.input {
                self.surface_input(app, input);
            }
            if let Some((w, h)) = changes.size {
                let size = (
                    if w == 0 { self.at.size.0 } else { w as f32 },
                    if h == 0 { self.at.size.1 } else { h as f32 },
                );
                if size != self.at.size {
                    self.at.size = size;
                    if let Some(live) = self.surface.live_mut() {
                        live.scale = 0.0;
                    }
                }
            }
            if let Some(scale) = changes.scale {
                if let Some(Live {
                    backing: Backing::Layer(layer),
                    ..
                }) = self.surface.live_mut()
                {
                    layer.set_buffer_scale(scale);
                }
                self.rescale_cpu(app, scale as f32);
            }
            if self.surface.live().is_some_and(|l| l.scale == 0.0) {
                let scale = app.rt().window_scale();
                self.rescale_cpu(app, scale);
            }
        }
        self.apply_input(app.rt());
        if self.surface.live().is_none() {
            return false;
        }
        let gate = frame::Gate {
            prewarm,
            occluded: false,
            awaiting_frame: self.awaiting_layer_frame(),
            dirty: self.dirty,
            idle: self.idle,
            retry: self.backoff.deadline(),
            missed_this_turn: self.turn_frame.missed(),
        };
        match frame::turn(gate, crate::time::now) {
            frame::Turn::Occluded { .. } | frame::Turn::Rest | frame::Turn::BackedOff => {
                return false;
            }
            frame::Turn::AwaitingFrame => {
                if app.rt().has_pending_input() {
                    self.commit_at(app, crate::time::now());
                }
                self.meter.skipped();
                self.meter.set_pacing(self.pacing_state(true));
                return false;
            }
            frame::Turn::Draw => {
                if !prewarm {
                    self.backoff_step(crate::window::BackoffEvent::Lapsed);
                }
            }
        }
        if !prewarm {
            self.meter.redraw(true);
        }
        let now = crate::time::now();
        let (animating, _dt) = if !prewarm && frame::rests(app, self.runtime_idle, self.dirty) {
            frame::rest(app, &mut self.clock, now)
        } else {
            step(app, &mut self.clock, now, prewarm)
        };
        super::input::drop_window_requests(app.rt_mut());
        self.clipboard.flush_core(app.rt_mut());
        let regions_changed = self.publish_regions(app.rt());
        let has_comp = app.has_compositor();
        let name = self.name;
        let Some(live) = self.surface.live_mut() else {
            return false;
        };
        let (width, height) = (live.config.width, live.config.height);
        let scale_i32 = live.scale.round().max(1.0) as i32;
        let (OverlayRenderTarget::Cpu(cpu), Backing::Layer(layer)) =
            (&mut live.target, &mut live.backing)
        else {
            return false;
        };
        let acquired = match layer.acquire_cpu_slot(width, height, now) {
            Ok(SlotAcquire::Ready {
                slot,
                needs_full_paint,
            }) => (slot, needs_full_paint),
            Ok(SlotAcquire::Deferred) => {
                self.dirty = true;
                return false;
            }
            Err(err) => {
                eprintln!("rune: cpu-present:{name}: {err}");
                *self
                    .shared
                    .failure
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(err.to_string());
                return false;
            }
        };
        let (slot, needs_full_paint) = acquired;
        if needs_full_paint {
            cpu.painter.forget();
        }
        let rt = app.rt_mut();
        let _ = cpu.painter.fill_masks(&mut rt.text.atlas);
        let scene = Scene::new(
            &rt.instances,
            &rt.text.atlas,
            &rt.images,
            (width, height),
            rt.clear_color(),
        )
        .srgb_blend(rt.srgb_blend())
        .fade_stops(rt.fade_stops())
        .native_surfaces(!rt.surfaces.is_empty())
        .compositor(has_comp)
        .font_smoothing(rt.font_smoothing());
        let Some((pixels, w, h, stride)) = layer.cpu_slot_target_mut(slot) else {
            return false;
        };
        let mut target = Target::with_stride(pixels, w, h, stride);
        match cpu.painter.paint(&scene, &mut target) {
            Ok(painted) => {
                let commit = !painted.rects.is_empty()
                    || !layer.shm_has_presented()
                    || needs_full_paint
                    || regions_changed;
                if commit
                    && let Err(err) =
                        layer.present_cpu(slot, &painted.rects, scale_i32, needs_full_paint)
                {
                    eprintln!("rune: cpu-present:{name}: {err}");
                    *self
                        .shared
                        .failure
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = Some(err.to_string());
                }
            }
            Err(why) => {
                let err = CpuPresentError::NeedsGpu(why);
                let kind = cpu.record_refusal(why, name);
                *self
                    .shared
                    .failure
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) =
                    Some(format!("cpu-present:needs-gpu:{kind} ({err})"));
                return false;
            }
        }
        frame::record_presented(prewarm);
        if !prewarm {
            self.meter.presented();
            self.backoff.step(crate::window::BackoffEvent::Presented);
        }
        self.dirty = false;
        self.idle = idle_after(app.rt(), animating, None);
        self.runtime_idle = !prewarm && idle_after(app.rt(), animating, None);
        if !prewarm {
            self.meter.set_pacing(self.pacing_state(!self.idle));
        }
        frame::wants_more(self.idle, self.awaiting_layer_frame())
    }
}

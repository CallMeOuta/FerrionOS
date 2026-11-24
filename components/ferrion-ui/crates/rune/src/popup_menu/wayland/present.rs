use std::sync::Arc;

use wayland_client::Proxy;
use wayland_client::protocol::wl_shm;

use crate::frame::host_step::draw_ui;
use crate::platform::wayland_shm::{ShmRing, SlotAcquire};
use crate::render::cpu_boot::{CpuPainter, Scene, Target, attach_text as attach_cpu_text};
use crate::render::{Gpu, Renderer};
use crate::runtime::Runtime;
use crate::window::wayland_cpu::painter_threads;

use super::menu_view::MenuViewState;
use super::protocol::{PopupConn, PopupSurfaceHandles, create_wgpu_surface};
use super::{MAX_POPUP_DIM, PopupWaylandError};

pub(super) enum LevelPresenter {
    Cpu {
        ring: Box<ShmRing>,
        painter: Box<CpuPainter>,
    },
    Gpu {
        gpu: Arc<Gpu>,
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
        renderer: Box<Renderer>,
    },
}

impl LevelPresenter {
    pub(super) fn new(
        conn: &PopupConn,
        handles: &PopupSurfaceHandles,
        rt: &mut Runtime<MenuViewState>,
        scale: i32,
        shared_gpu: Option<&Arc<Gpu>>,
    ) -> Result<Self, PopupWaylandError> {
        let scale = scale.max(1);
        let (lw, lh) = handles.configured_size;
        let wp = ((lw.max(1) as u32).saturating_mul(scale as u32)).min(MAX_POPUP_DIM);
        let hp = ((lh.max(1) as u32).saturating_mul(scale as u32)).min(MAX_POPUP_DIM);
        if crate::gpu::backend::uses_cpu() {
            if conn.shm.is_none() {
                return Err(PopupWaylandError::MissingGlobal("wl_shm"));
            }
            attach_cpu_text(&mut rt.core_mut().text.atlas);
            return Ok(Self::Cpu {
                ring: Box::new(ShmRing::new(wl_shm::Format::Argb8888)),
                painter: Box::new(CpuPainter::new(painter_threads())),
            });
        }
        let (raw_display, raw_window) = handles.raw_handles(conn.display);
        let (gpu, surface) = if let Some(existing) = shared_gpu.filter(|g| !g.is_lost()) {
            let surface = create_wgpu_surface(&existing.instance, raw_display, raw_window)?;
            (existing.clone(), surface)
        } else {
            let (gpu, surface, _) = Gpu::for_surface(wgpu::Features::empty(), true, |instance| {
                create_wgpu_surface(instance, raw_display, raw_window).map_err(|e| e.to_string())
            })
            .map_err(PopupWaylandError::SurfaceCreate)?;
            (Arc::new(gpu), surface)
        };
        let caps = surface.get_capabilities(&gpu.adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|&f| f == wgpu::TextureFormat::Bgra8Unorm || f == wgpu::TextureFormat::Rgba8Unorm)
            .or_else(|| caps.formats.first().copied())
            .ok_or_else(|| PopupWaylandError::SurfaceCreate("no surface format".into()))?;
        let alpha_mode = if caps
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
        {
            wgpu::CompositeAlphaMode::PreMultiplied
        } else {
            caps.alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Auto)
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: wp.max(1),
            height: hp.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode,
            view_formats: vec![],
            color_space: Default::default(),
        };
        crate::window::surface::configure(&surface, &gpu, &config)
            .map_err(PopupWaylandError::SurfaceCreate)?;
        let mut renderer = Renderer::new(&gpu.device, format);
        renderer.set_srgb_blend(rt.core().srgb_blend());
        renderer.attach_text(&mut rt.core_mut().text.atlas);
        Ok(Self::Gpu {
            gpu,
            surface,
            config,
            renderer: Box::new(renderer),
        })
    }

    pub(super) fn mark_released(&mut self, ids: &[u64]) {
        if let Self::Cpu { ring, .. } = self {
            ring.mark_released(ids.iter().copied());
        }
    }

    pub(super) fn paint(
        &mut self,
        conn: &mut PopupConn,
        handles: &PopupSurfaceHandles,
        rt: &mut Runtime<MenuViewState>,
        scale: i32,
    ) -> Result<(), PopupWaylandError> {
        let scale = scale.max(1);
        let (lw, lh) = handles.configured_size;
        let wp = ((lw.max(1) as u32).saturating_mul(scale as u32)).min(MAX_POPUP_DIM);
        let hp = ((lh.max(1) as u32).saturating_mul(scale as u32)).min(MAX_POPUP_DIM);
        rt.core_mut()
            .set_viewport((lw.max(1) as f32, lh.max(1) as f32), scale as f32);
        rt.frame(0.0);
        match self {
            Self::Cpu { ring, painter } => {
                let shm = conn
                    .shm
                    .as_ref()
                    .ok_or(PopupWaylandError::MissingGlobal("wl_shm"))?;
                let qh = conn.qh();
                let now = crate::time::now();
                let (slot, needs_full) = match ring
                    .acquire_slot(shm, &qh, wp, hp, now)
                    .map_err(|e| PopupWaylandError::Present(e.to_string()))?
                {
                    SlotAcquire::Ready {
                        slot,
                        needs_full_paint,
                    } => (slot, needs_full_paint),
                    SlotAcquire::Deferred => return Ok(()),
                };
                if needs_full {
                    painter.forget();
                }
                let core = rt.core_mut();
                let _ = painter.fill_masks(&mut core.text.atlas);
                let scene = Scene::new(
                    &core.instances,
                    &core.text.atlas,
                    &core.images,
                    (wp, hp),
                    core.clear_color(),
                )
                .srgb_blend(core.srgb_blend())
                .fade_stops(core.fade_stops())
                .font_smoothing(core.font_smoothing());
                let Some((pixels, w, h, stride)) = ring.slot_target_mut(slot) else {
                    return Err(PopupWaylandError::Present("shm slot unavailable".into()));
                };
                let mut target = Target::with_stride(pixels, w, h, stride);
                let painted = painter
                    .paint(&scene, &mut target)
                    .map_err(|why| PopupWaylandError::Present(format!("cpu refused: {why:?}")))?;
                ring.attach_and_damage(
                    &handles.wl_surface,
                    slot,
                    &painted.rects,
                    scale,
                    needs_full || !ring.has_presented(),
                )
                .map_err(|e| PopupWaylandError::Present(e.to_string()))?;
                handles.wl_surface.commit();
                ring.mark_presented(slot);
                conn.flush();
                Ok(())
            }
            Self::Gpu {
                gpu,
                surface,
                config,
                renderer,
            } => {
                if config.width != wp || config.height != hp {
                    config.width = wp.max(1);
                    config.height = hp.max(1);
                    crate::window::surface::configure(surface, gpu, config)
                        .map_err(PopupWaylandError::Present)?;
                }
                if handles.wl_surface.version() >= 3 {
                    handles.wl_surface.set_buffer_scale(scale);
                }
                let frame = match surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(f)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
                    other => {
                        return Err(PopupWaylandError::Present(format!(
                            "surface acquire: {other:?}"
                        )));
                    }
                };
                let view = frame.texture.create_view(&Default::default());
                let mut encoder =
                    gpu.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("popup-gpu"),
                        });
                let core = rt.core_mut();
                let clear = core.clear_color();
                draw_ui(renderer, gpu, &mut encoder, &view, core, (wp, hp), clear);
                let _ = renderer.submit(&gpu.queue, encoder);
                gpu.queue.present(frame);
                conn.flush();
                Ok(())
            }
        }
    }

    pub(super) fn clear_slots(&mut self) {
        if let Self::Cpu { ring, .. } = self {
            ring.clear_slots();
        }
    }
}

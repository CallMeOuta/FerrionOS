use std::sync::Arc;
use std::sync::atomic::Ordering;

use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use crate::frame::clock::FrameClock;
use crate::frame::host_step::{self, Acquire, FrameHost, Gate, RenderPrep, StepInfo};
use crate::platform::wayland_shm::{CpuPresentError, SlotAcquire, WaylandShm};
use crate::render::cpu_boot::{CpuPainter, NeedsGpu, Scene, Target};
use crate::runtime::RuntimeCore;
use crate::time::Instant;
use crate::window::chrome::{sync_chrome_core, sync_platform_core};
use crate::window::handle::native_window_number;
use crate::window::host::Host;
use crate::window::host::stepping::FrameMove;
use crate::window::state::{FirstFrame, RestEvent, rest};
use crate::window::surface::{
    Backoff, BackoffEvent, FRAME_LATENCY, HiddenPace, LoopFrame, Native, Presented, Redraw,
    SurfaceRetries, TimerFrame, Win, drawable_size, max_surface_dim,
};
use crate::window::{WakeEvent, sync_screen_origin_core};

pub(crate) const CPU_PAINTER_THREADS_MAX: usize = 6;
pub(crate) const CPU_PAINTER_THREADS_DEFAULT: usize = 4;

pub(crate) fn painter_threads() -> usize {
    let requested = rune_core::env::var("RUNE_CPU_THREADS")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(CPU_PAINTER_THREADS_DEFAULT, |n| n.get())
        });
    requested.clamp(1, CPU_PAINTER_THREADS_MAX)
}

pub(crate) fn needs_gpu_kind(why: NeedsGpu) -> (u16, &'static str) {
    match why {
        NeedsGpu::LinearBlend => (1 << 0, "linear-blend"),
        NeedsGpu::NativeSurface => (1 << 1, "native-surface"),
        NeedsGpu::SlugText => (1 << 2, "slug-text"),
        NeedsGpu::LayerBlur => (1 << 3, "layer-blur"),
        NeedsGpu::TurnedLayer => (1 << 4, "turned-layer"),
        NeedsGpu::BackdropBlur => (1 << 5, "backdrop-blur"),
        NeedsGpu::Effect => (1 << 6, "effect"),
        NeedsGpu::OutlineGlyph => (1 << 7, "outline-glyph"),
        NeedsGpu::UnknownKind => (1 << 8, "unknown-kind"),
        NeedsGpu::Target => (1 << 9, "target"),
    }
}

pub(in crate::window) struct WaylandCpu {
    shm: WaylandShm,
    painter: CpuPainter,
    logged_refusals: u16,
}

impl WaylandCpu {
    pub(in crate::window) fn dispatch(&mut self) -> bool {
        self.shm.dispatch()
    }

    pub(in crate::window) fn waiting_for_slot(&self) -> bool {
        self.shm.waiting_for_slot()
    }

    pub(in crate::window) fn stall_deadline(&self) -> Option<Instant> {
        self.shm.stall_deadline()
    }

    fn record_refusal(&mut self, why: NeedsGpu, key: &str) -> &'static str {
        let (bit, kind) = needs_gpu_kind(why);
        if self.logged_refusals & bit == 0 {
            self.logged_refusals |= bit;
            eprintln!("rune: cpu-present:needs-gpu:{kind} (window {key}, keeping last frame)");
        }
        kind
    }
}

struct PreparedCpuFrame {
    slot: usize,
    rects: Vec<[u32; 4]>,
    scale: i32,
    force_full_damage: bool,
    commit: bool,
}

impl Host {
    pub(in crate::window) fn open_on_wayland_cpu(
        &mut self,
        event_loop: &ActiveEventLoop,
        window: Arc<Window>,
        a11y: Option<crate::a11y::Adapter>,
        visible: bool,
        built: bool,
    ) -> Result<(), String> {
        if self.options.transparent {
            return Err(CpuPresentError::TransparentUnsupported.to_string());
        }
        let handle = self.native_handle.clone();
        let wake = Arc::new(move || {
            handle.send(WakeEvent::FrameSlot);
        });
        let shm = WaylandShm::new(&window, wake).map_err(|e| e.to_string())?;
        crate::render::cpu_boot::attach_text(&mut self.app.rt_mut().text.atlas);
        let painter = CpuPainter::new(painter_threads());
        let phys = window.inner_size();
        let size = drawable_size(None, (phys.width, phys.height), max_surface_dim(None));
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST,
            format: wgpu::TextureFormat::Bgra8Unorm,
            width: size.0,
            height: size.1,
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: FRAME_LATENCY,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: vec![],
            color_space: Default::default(),
        };
        let scale = window.scale_factor() as f32;
        let launcher = self.launcher();
        {
            let rt = self.app.rt_mut();
            rt.set_system_dark(crate::platform::preferences::window_dark(
                &window,
                self.args.theme.as_deref(),
            ));
            rt.set_viewport((size.0 as f32 / scale, size.1 as f32 / scale), scale);
            if (self.shown || launcher) && !built {
                rt.settle_all();
            }
        }
        self.shown = true;
        if visible {
            self.life.show();
        }
        self.idle = true;
        self.app.set_live_visible(visible);
        *self
            .native_handle
            .native
            .window
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(&window);
        self.native_handle.native.number.store(
            native_window_number(&window).unwrap_or(0),
            Ordering::Release,
        );
        #[cfg(not(feature = "x11"))]
        let drop_target = {
            let handle = self.native_handle.clone();
            crate::drag_out::wayland::register_window(
                &window,
                Arc::new(move || handle.send(WakeEvent::App)),
            )
        };
        let native = Native {
            activator: None,
            cpu: Some(WaylandCpu {
                shm,
                painter,
                logged_refusals: 0,
            }),
            #[cfg(not(feature = "x11"))]
            _drop_target: drop_target,
        };
        self.win = Some(Win {
            a11y,
            native,
            first_frame: FirstFrame::opened(crate::time::now()),
            surface: None,
            config,
            surface_ok: true,
            surface_retries: SurfaceRetries::default(),
            window,
            backoff: Backoff::default(),
            hidden_pace: HiddenPace::default(),
            cursor: crate::cursor::os::OsCursor::new(),
            alpha: None,
            corners: None,
            timer_frame: TimerFrame::default(),
            loop_frame: LoopFrame::default(),
            presented: None,
            redraw: std::cell::Cell::new(Redraw::Idle),
        });
        if launcher && let Some(w) = self.win.as_mut() {
            w.first_frame.prepare(crate::time::now());
        }
        self.apply_placement(event_loop);
        crate::marks::mark("window.created");
        if let Some(created) = self.on_created.take() {
            created(self.native_handle.clone());
        }
        if let Some(on_window) = self.on_window.as_mut() {
            on_window(&self.native_handle);
        }
        if let Some(w) = self.win.as_mut() {
            let size = w.window.inner_size();
            let scale = w.window.scale_factor() as f32;
            let rt = self.app.rt_mut();
            if size.width > 0 && size.height > 0 {
                w.resize_surface(None, size);
                rt.set_viewport(
                    (
                        w.config.width as f32 / scale,
                        w.config.height as f32 / scale,
                    ),
                    scale,
                );
            }
            sync_chrome_core(rt, w, self.chrome, self.titlebar);
            sync_screen_origin_core(rt, &w.window);
            if w.window.has_focus() {
                self.app.window_focus(true);
            }
        }
        if (visible || launcher)
            && let Some(w) = &self.win
        {
            w.request_redraw();
        }
        Ok(())
    }

    pub(in crate::window) fn draw_wayland_cpu(&mut self, resizing: bool) {
        let mut frame = WaylandCpuFrame {
            host: self,
            resizing,
            hidden: false,
            now: None,
            encoded: None,
        };
        let _ = host_step::run(&mut frame);
    }
}

struct WaylandCpuFrame<'a> {
    host: &'a mut Host,
    resizing: bool,
    hidden: bool,
    now: Option<Instant>,
    encoded: Option<Instant>,
}

impl FrameHost for WaylandCpuFrame<'_> {
    type Drawable = ();
    type Prepared = PreparedCpuFrame;

    fn gate(&mut self) -> Gate {
        let launcher = self.host.launcher();
        let Some(l) = self.host.win.as_ref() else {
            return Gate::Skip;
        };
        let hidden = launcher && !self.host.life.shown();
        let covered = (!launcher && !self.host.life.uncovered()) || l.minimized();
        if covered {
            self.host.covered_frame();
            return Gate::Skip;
        }
        if !self.resizing && l.backing_off(crate::time::now()) {
            return Gate::Skip;
        }
        let Some(l) = self.host.win.as_mut() else {
            return Gate::Skip;
        };
        l.timer_frame.drawn();
        if self
            .host
            .native_handle
            .native
            .number
            .load(Ordering::Relaxed)
            == 0
        {
            self.host.native_handle.native.number.store(
                native_window_number(&l.window).unwrap_or(0),
                Ordering::Release,
            );
        }
        if !hidden {
            self.host.meter.redraw(l.paced_for_stats());
        }
        self.hidden = hidden;
        Gate::Draw
    }

    fn before_acquire(&mut self) {
        if !self.hidden && !self.resizing {
            self.host.step_frame(crate::time::now());
        }
        let Some(l) = self.host.win.as_mut() else {
            return;
        };
        l.backoff_step(BackoffEvent::Lapsed);
        l.backoff_step(BackoffEvent::Presented);
        if self.host.frame_reports.wanted() {
            let hz = l
                .window
                .current_monitor()
                .and_then(|m| m.refresh_rate_millihertz())
                .map(|hz| hz as f64 / 1000.0);
            self.host.frame_reports.set_refresh(hz);
        }
    }

    fn acquire(&mut self) -> Acquire<()> {
        Acquire::Ready(())
    }

    fn step_runtime(&mut self) -> StepInfo {
        let now = crate::time::now();
        self.now = Some(now);
        if self.hidden
            && self
                .host
                .win
                .as_ref()
                .is_some_and(|l| l.first_frame.settles())
        {
            self.host.app.rt_mut().settle_all();
        }
        let (animating, dt) = if self.hidden {
            self.host.clock.hold(now);
            (self.host.app.frame(0.0), 0.0)
        } else {
            self.host
                .app
                .clock_advance(&mut self.host.clock, now, false)
        };
        if self.hidden
            && let Some(l) = self.host.win.as_mut()
        {
            l.hidden_pace.rendered(now);
        }
        StepInfo {
            runtime_animating: animating,
            animating,
            dt,
            update_ms: 0.0,
            presents_at: None,
        }
    }

    fn sync_after_step(&mut self, info: &mut StepInfo) {
        let now = self.now.unwrap_or_else(crate::time::now);
        info.update_ms = now.elapsed().as_secs_f64() * 1000.0;
        let Some(l) = self.host.win.as_mut() else {
            return;
        };
        let rt = self.host.app.rt_mut();
        sync_platform_core(
            l,
            rt,
            &mut self.host.secure_input,
            &mut self.host.ime,
            self.host.ime_waits,
            &self.host.clipboard,
        );
    }

    fn prepare_render(
        &mut self,
        _drawable: (),
        _info: &mut StepInfo,
    ) -> RenderPrep<PreparedCpuFrame> {
        let has_comp = self.host.app.has_compositor();
        let now = self.now.unwrap_or_else(crate::time::now);
        let key = self.host.native_handle.key().to_owned();
        let Some(l) = self.host.win.as_mut() else {
            return RenderPrep::Abort;
        };
        let (width, height) = (l.config.width, l.config.height);
        let scale = l.window.scale_factor().round().max(1.0) as i32;
        let Some(cpu) = l.native.cpu.as_mut() else {
            return RenderPrep::Abort;
        };
        let acquired = match cpu.shm.acquire_slot(width, height, now) {
            Ok(SlotAcquire::Ready {
                slot,
                needs_full_paint,
            }) => (slot, needs_full_paint),
            Ok(SlotAcquire::Deferred) => {
                l.presented = None;
                self.host.frame_reports.skipped();
                return RenderPrep::Abort;
            }
            Err(err) => {
                eprintln!("rune: cpu-present:{key}: {err}");
                self.host.shared.set_window_error(Some(err.to_string()));
                l.presented = None;
                self.host.frame_reports.skipped();
                return RenderPrep::Abort;
            }
        };
        let (slot, needs_full_paint) = acquired;
        if needs_full_paint {
            cpu.painter.forget();
        }
        let rt = self.host.app.rt_mut();
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
        let Some((pixels, w, h, stride)) = cpu.shm.slot_target_mut(slot) else {
            l.presented = None;
            self.host.frame_reports.skipped();
            return RenderPrep::Abort;
        };
        let mut target = Target::with_stride(pixels, w, h, stride);
        match cpu.painter.paint(&scene, &mut target) {
            Ok(painted) => {
                let commit =
                    !painted.rects.is_empty() || !cpu.shm.has_presented() || needs_full_paint;
                RenderPrep::Ready(PreparedCpuFrame {
                    slot,
                    rects: painted.rects,
                    scale,
                    force_full_damage: needs_full_paint,
                    commit,
                })
            }
            Err(why) => {
                let err = CpuPresentError::NeedsGpu(why);
                let kind = cpu.record_refusal(why, &key);
                self.host
                    .shared
                    .set_window_error(Some(format!("cpu-present:needs-gpu:{kind} ({err})")));
                l.presented = None;
                self.host.frame_reports.skipped();
                RenderPrep::Abort
            }
        }
    }

    fn render(&mut self, _prepared: &mut PreparedCpuFrame, _info: &StepInfo) -> bool {
        false
    }

    fn submit_and_present(&mut self, prepared: PreparedCpuFrame, _info: &StepInfo) -> Instant {
        self.encoded = self.host.frame_reports.stamp();
        if let Some(l) = self.host.win.as_mut() {
            if prepared.commit
                && let Some(cpu) = l.native.cpu.as_mut()
                && let Err(err) = cpu.shm.present(
                    prepared.slot,
                    &prepared.rects,
                    prepared.scale,
                    prepared.force_full_damage,
                )
            {
                eprintln!("rune: cpu-present:{}: {err}", self.host.native_handle.key());
                self.host.shared.set_window_error(Some(err.to_string()));
            }
            if !l.first_frame.ready() {
                l.first_frame.gpu_done();
                let now = *self.now.get_or_insert_with(crate::time::now);
                if let Some(script) = &mut self.host.window_script {
                    script.start(now);
                }
            }
        }
        crate::window::frame_presented();
        crate::time::now()
    }

    fn present_targets(&mut self) -> (Option<&mut FrameClock>, &mut RuntimeCore) {
        (Some(&mut self.host.clock), self.host.app.rt_mut())
    }

    fn settle(&mut self, submitted: Instant, info: &StepInfo) {
        let now = self.now.unwrap_or(submitted);
        self.host.meter.presented();
        self.host
            .frame_reports
            .drawn(self.encoded, None, now, info.update_ms);
        self.host.app.inspect_frame();
        let Some(l) = self.host.win.as_mut() else {
            return;
        };
        let next_tick = l.next_tick(now).filter(|_| !self.hidden);
        self.host.idle = Presented::settle_core(
            &mut l.presented,
            self.host.app.rt(),
            &l.config,
            info.animating || FrameMove::moving(&self.host.frame_move),
            next_tick,
            !self.hidden,
            false,
        );
        if !self.hidden {
            let _ = rest::step(
                &mut self.host.rest,
                RestEvent::Drew {
                    now,
                    low: rest::low_memory(),
                },
            );
        }
        if !self.hidden && !self.host.idle {
            l.request_redraw();
            self.host.meter.set_pacing(l.pacing_state(true));
        } else {
            l.stop_pacing();
            self.host.meter.set_pacing(l.pacing_state(false));
        }
    }
}

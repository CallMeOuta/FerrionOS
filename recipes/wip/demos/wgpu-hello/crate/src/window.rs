//! Opens a winit window and shows the triangle turning. wgpu renders each frame into a texture,
//! and softbuffer copies the pixels into the window: Mesa's Vulkan has no surface for Orbital
//! windows, so a wgpu surface is only tried and its result printed.
//! Usage: wgpu-hello-window [--frames N]  (exits after N frames; default: runs until closed)

mod triangle;

use std::num::NonZeroU32;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

struct Shown {
    window: Arc<Window>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
}

struct App {
    gpu: triangle::Gpu,
    shown: Option<Shown>,
    start: Instant,
    frames: u64,
    max_frames: Option<u64>,
    error: Option<String>,
}

impl App {
    fn fail(&mut self, event_loop: &ActiveEventLoop, err: String) {
        self.error = Some(err);
        event_loop.exit();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.shown.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("wgpu-hello")
            .with_inner_size(LogicalSize::new(480, 360));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(err) => return self.fail(event_loop, format!("create window: {err}")),
        };

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        match instance.create_surface(window.clone()) {
            Ok(_) => println!("wgpu surface: created"),
            Err(err) => println!("wgpu surface: {err}"),
        }

        let context = match softbuffer::Context::new(window.clone()) {
            Ok(context) => context,
            Err(err) => return self.fail(event_loop, format!("softbuffer context: {err}")),
        };
        let surface = match softbuffer::Surface::new(&context, window.clone()) {
            Ok(surface) => surface,
            Err(err) => return self.fail(event_loop, format!("softbuffer surface: {err}")),
        };
        window.request_redraw();
        self.shown = Some(Shown { window, surface });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                let Some(shown) = self.shown.as_mut() else {
                    return;
                };
                let size = shown.window.inner_size();
                let (Some(width), Some(height)) =
                    (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                else {
                    return;
                };
                let angle = self.start.elapsed().as_secs_f32();
                let pixels = self.gpu.render(width.get(), height.get(), angle);
                if let Err(err) = shown.surface.resize(width, height) {
                    return self.fail(event_loop, format!("softbuffer resize: {err}"));
                }
                let mut buffer = match shown.surface.buffer_mut() {
                    Ok(buffer) => buffer,
                    Err(err) => return self.fail(event_loop, format!("softbuffer buffer: {err}")),
                };
                for (dst, src) in buffer.iter_mut().zip(pixels.chunks_exact(4)) {
                    *dst = u32::from(src[0]) << 16 | u32::from(src[1]) << 8 | u32::from(src[2]);
                }
                if let Err(err) = buffer.present() {
                    return self.fail(event_loop, format!("softbuffer present: {err}"));
                }
                self.frames += 1;
                if self.frames == 1 || self.frames % 60 == 0 {
                    let secs = self.start.elapsed().as_secs_f64();
                    println!(
                        "frame {} at {}x{}, {:.1} frames/s",
                        self.frames,
                        width,
                        height,
                        self.frames as f64 / secs
                    );
                }
                if self.max_frames.is_some_and(|max| self.frames >= max) {
                    event_loop.exit();
                } else {
                    shown.window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let max_frames = match (args.next().as_deref(), args.next()) {
        (None, _) => None,
        (Some("--frames"), Some(n)) if n.parse::<u64>().is_ok() => n.parse().ok(),
        _ => {
            eprintln!("usage: wgpu-hello-window [--frames N]");
            return ExitCode::FAILURE;
        }
    };

    let gpu = match triangle::Gpu::new() {
        Ok(gpu) => gpu,
        Err(err) => {
            eprintln!("wgpu-hello-window: {err}");
            return ExitCode::FAILURE;
        }
    };
    let info = &gpu.adapter_info;
    println!("adapter: {} ({:?}, {:?})", info.name, info.device_type, info.backend);

    let event_loop = match EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(err) => {
            eprintln!("wgpu-hello-window: event loop: {err}");
            return ExitCode::FAILURE;
        }
    };
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        gpu,
        shown: None,
        start: Instant::now(),
        frames: 0,
        max_frames,
        error: None,
    };
    if let Err(err) = event_loop.run_app(&mut app) {
        eprintln!("wgpu-hello-window: event loop: {err}");
        return ExitCode::FAILURE;
    }
    if let Some(err) = app.error {
        eprintln!("wgpu-hello-window: {err}");
        return ExitCode::FAILURE;
    }
    println!("presented {} frames", app.frames);
    ExitCode::SUCCESS
}

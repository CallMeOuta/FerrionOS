// Smithay protocol/rendering setup follows smallvil; see state.rs and LICENSE.
mod state;
use std::{error::Error, process::{Child, Command}, time::{Duration, Instant}};
use ferrion_wm_backend::{Backend, BackendEvent, FrameScheduler};
use smithay::{backend::renderer::{Bind, damage::OutputDamageTracker, element::surface::WaylandSurfaceRenderElement, pixman::PixmanRenderer}, output::{Mode, Output, PhysicalProperties, Subpixel}, reexports::wayland_server::{Display, ListeningSocket}, utils::Transform};
use state::State;

struct Clients(Vec<Child>);
impl Drop for Clients {
    fn drop(&mut self) {
        for child in &mut self.0 { let _ = child.kill(); let _ = child.wait(); }
    }
}

fn framebuffer(backend: &mut Backend) -> Result<pixman::Image<'static, 'static>, Box<dyn Error>> {
    let (width, height) = backend.dimensions();
    let stride = backend.stride() as usize;
    let pixels = backend.framebuffer_mut();
    let image = unsafe { pixman::Image::from_raw_mut(pixman::FormatCode::A8R8G8B8, width as usize, height as usize, pixels.as_mut_ptr().cast(), stride, false) }?;
    Ok(image)
}

fn monotonic_ns() -> std::io::Result<u64> {
    let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64)
}

fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    use std::os::unix::fs::DirBuilderExt;
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR is required")?;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(runtime_dir)?;
    let mut backend = Backend::open()?;
    let mut image = framebuffer(&mut backend)?;
    let mut renderer = PixmanRenderer::new()?;
    let mut display = Display::<State>::new()?;
    let mut state = State::new(display.handle());
    let socket = ListeningSocket::bind("wayland-0")?;
    let (width, height) = backend.dimensions();
    let mode = Mode { size: (width as i32, height as i32).into(), refresh: 60_000 };
    let output = Output::new("ferrion-drm".into(), PhysicalProperties { size: (0, 0).into(), subpixel: Subpixel::Unknown, make: "FerrionOS".into(), model: "CPU display".into(), serial_number: String::new() });
    let _global = output.create_global::<State>(&state.dh);
    output.change_current_state(Some(mode), Some(Transform::Normal), None, Some((0, 0).into()));
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));
    let mut damage = OutputDamageTracker::from_output(&output);
    let mut clients = Clients(Vec::new());
    clients.0.push(Command::new("wayland-probe").env("WAYLAND_DISPLAY", "wayland-0").spawn()?);
    clients.0.push(Command::new("wayland-probe").arg("--slow").env("WAYLAND_DISPLAY", "wayland-0").spawn()?);
    let mut scheduler = FrameScheduler::new(Duration::from_micros(16_666), Duration::from_micros(300));
    let mut next_frame = backend.now_us();
    println!("DD1_COMPOSITOR_READY width={width} height={height} renderer=pixman transport=rust");
    loop {
        while let Some(stream) = socket.accept()? { state.accept(stream)?; }
        display.dispatch_clients(&mut state)?;
        while let Some(event) = backend.try_event()? {
            match event {
                BackendEvent::Input(input) => state.input(input.into_smithay()),
                BackendEvent::CursorTiming(timing) => println!("DD1_POINTER read_us={} completed_us={} elapsed_us={} x={} y={}", timing.read_us, timing.completed_us, timing.elapsed_us(), timing.x, timing.y),
                BackendEvent::DisplayChanged => {
                    drop(image);
                    backend.reconfigure()?;
                    image = framebuffer(&mut backend)?;
                    let (width, height) = backend.dimensions();
                    output.change_current_state(Some(Mode { size: (width as i32, height as i32).into(), refresh: 60_000 }), None, None, None);
                    damage = OutputDamageTracker::from_output(&output);
                }
            }
        }
        if backend.now_us() >= next_frame {
            let started = Instant::now();
            {
                let mut target = renderer.bind(&mut image)?;
                smithay::desktop::space::render_output::<_, WaylandSurfaceRenderElement<PixmanRenderer>, _, _>(&output, &mut renderer, &mut target, 1.0, 0, [&state.space], &[], &mut damage, [15.0 / 255.0, 14.0 / 255.0, 12.0 / 255.0, 1.0])?;
            }
            let presentation = backend.present()?;
            let handed = Instant::now();
            let presented_ns = monotonic_ns()?;
            scheduler.record_render(handed.duration_since(started));
            for (client, sequence, received) in state.commits.drain(..) {
                println!("DD1_FRAME client={client} frame={sequence} dispatch_to_present_us={} monotonic_ns={} present_us={} present_started_us={}", handed.duration_since(received).as_micros(), presented_ns, presentation.completed_us, presentation.started_us);
            }
            for window in state.space.elements() {
                window.send_frame(&output, state.start.elapsed(), Some(Duration::ZERO), |_, _| Some(output.clone()));
            }
            state.space.refresh();
            next_frame = scheduler.next_start(backend.now_us());
        }
        state.dh.flush_clients()?;
        let remaining = next_frame.saturating_sub(backend.now_us());
        if remaining > 0 { std::thread::sleep(Duration::from_micros(remaining.min(500))); }
    }
}

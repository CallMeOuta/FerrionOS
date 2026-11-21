// SPDX-License-Identifier: MIT

use std::{
    error::Error,
    fs::File,
    io::{self, Write},
    os::fd::AsFd,
    process::ExitCode,
    thread,
    time::Duration,
};

use wayland_client::{
    delegate_noop,
    protocol::{
        wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_registry, wl_seat, wl_shm,
        wl_shm_pool, wl_surface,
    },
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

const WIDTH: i32 = 320;
const HEIGHT: i32 = 240;
const STRIDE: i32 = WIDTH * 4;
const BUFFER_BYTES: i32 = STRIDE * HEIGHT;
const BUFFER_COUNT: usize = 2;

fn monotonic_ns() -> io::Result<u64> {
    let mut timestamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut timestamp) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let nanoseconds = timestamp.tv_sec as i128 * 1_000_000_000 + timestamp.tv_nsec as i128;
    u64::try_from(nanoseconds).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "monotonic clock is outside the nanosecond range")
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    Slow,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Slow => "slow",
        }
    }

    fn app_id(self) -> &'static str {
        match self {
            Self::Normal => "ferrion-wayland-probe",
            Self::Slow => "ferrion-wayland-probe-slow",
        }
    }

    fn pixel(self) -> [u8; 4] {
        match self {
            Self::Normal => 0xffe68e0d_u32.to_le_bytes(),
            Self::Slow => 0xff3a9fd8_u32.to_le_bytes(),
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("wayland-probe: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut mode = Mode::Normal;
    for argument in std::env::args_os().skip(1) {
        if argument == "--slow" && mode == Mode::Normal {
            mode = Mode::Slow;
        } else if argument == "--help" || argument == "-h" {
            println!("Usage: wayland-probe [--slow]\n\n--slow  Delay each frame by 100 ms in this client.\n\nRun both modes as separate processes. Press Escape or close the window to exit.");
            return Ok(());
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unexpected argument {argument:?}; use wayland-probe --help"),
            )
            .into());
        }
    }

    let connection = Connection::connect_to_env()?;
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    let mut state = State::new(mode);
    connection.display().get_registry(&qh, ());
    queue.roundtrip(&mut state)?;
    queue.roundtrip(&mut state)?;
    state.create_window(&qh)?;
    connection.flush()?;

    let result = (|| -> Result<(), Box<dyn Error>> {
        while state.running {
            queue.blocking_dispatch(&mut state)?;
            if state.running {
                state.submit_if_ready(&connection, &qh)?;
            }
        }
        Ok(())
    })();

    drop(state);
    let cleanup = connection.flush();
    result?;
    cleanup?;
    Ok(())
}

struct Buffer {
    proxy: wl_buffer::WlBuffer,
    busy: bool,
}

struct Window {
    surface: wl_surface::WlSurface,
    xdg_surface: xdg_surface::XdgSurface,
    toplevel: xdg_toplevel::XdgToplevel,
    buffers: [Buffer; BUFFER_COUNT],
    _shm_file: File,
}

impl Drop for Window {
    fn drop(&mut self) {
        self.toplevel.destroy();
        self.xdg_surface.destroy();
        self.surface.destroy();
        for buffer in &self.buffers {
            buffer.proxy.destroy();
        }
    }
}

struct State {
    mode: Mode,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm_base: Option<xdg_wm_base::XdgWmBase>,
    seat: Option<wl_seat::WlSeat>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    window: Option<Window>,
    running: bool,
    configured: bool,
    frame_ready: bool,
    submitted: u64,
}

impl State {
    fn new(mode: Mode) -> Self {
        Self {
            mode,
            compositor: None,
            shm: None,
            wm_base: None,
            seat: None,
            keyboard: None,
            window: None,
            running: true,
            configured: false,
            frame_ready: true,
            submitted: 0,
        }
    }

    fn create_window(&mut self, qh: &QueueHandle<Self>) -> Result<(), Box<dyn Error>> {
        let missing = |name| io::Error::new(io::ErrorKind::NotFound, name);
        let compositor = self.compositor.as_ref().ok_or_else(|| missing("wl_compositor is unavailable"))?;
        let shm = self.shm.as_ref().ok_or_else(|| missing("wl_shm is unavailable"))?;
        let wm_base = self.wm_base.as_ref().ok_or_else(|| missing("xdg_wm_base is unavailable"))?;
        if self.keyboard.is_none() {
            return Err(missing("a Wayland keyboard is unavailable").into());
        }

        let mut file = tempfile::tempfile()?;
        write_solid_pool(&mut file, self.mode)?;
        let pool = shm.create_pool(file.as_fd(), BUFFER_BYTES * BUFFER_COUNT as i32, qh, ());
        let buffers = std::array::from_fn(|index| Buffer {
            proxy: pool.create_buffer(
                index as i32 * BUFFER_BYTES,
                WIDTH,
                HEIGHT,
                STRIDE,
                wl_shm::Format::Argb8888,
                qh,
                index,
            ),
            busy: false,
        });
        pool.destroy();

        let surface = compositor.create_surface(qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, qh, ());
        let toplevel = xdg_surface.get_toplevel(qh, ());
        toplevel.set_app_id(self.mode.app_id().into());
        toplevel.set_title(match self.mode {
            Mode::Normal => "Wayland probe".into(),
            Mode::Slow => "Wayland probe (slow)".into(),
        });
        toplevel.set_min_size(WIDTH, HEIGHT);
        toplevel.set_max_size(WIDTH, HEIGHT);
        xdg_surface.set_window_geometry(0, 0, WIDTH, HEIGHT);
        surface.commit();
        self.window = Some(Window {
            surface,
            xdg_surface,
            toplevel,
            buffers,
            _shm_file: file,
        });
        Ok(())
    }

    fn submit_if_ready(
        &mut self,
        connection: &Connection,
        qh: &QueueHandle<Self>,
    ) -> Result<(), Box<dyn Error>> {
        if !self.configured || !self.frame_ready {
            return Ok(());
        }
        let Some(window) = self.window.as_mut() else {
            return Ok(());
        };
        let Some(buffer) = window.buffers.iter_mut().find(|buffer| !buffer.busy) else {
            return Ok(());
        };
        if self.mode == Mode::Slow {
            thread::sleep(Duration::from_millis(100));
        }
        window.surface.attach(Some(&buffer.proxy), 0, 0);
        window.surface.damage(0, 0, WIDTH, HEIGHT);
        window.surface.frame(qh, ());
        buffer.busy = true;
        self.frame_ready = false;
        let commit_ns = monotonic_ns()?;
        window.surface.commit();
        connection.flush()?;

        self.submitted += 1;
        let mut output = io::stdout().lock();
        if self.submitted == 1 {
            writeln!(output, "{}", match self.mode {
                Mode::Normal => "DD1_CLIENT_READY",
                Mode::Slow => "DD1_SLOW_CLIENT_READY",
            })?;
        }
        writeln!(
            output,
            "DD1_CLIENT_COMMIT client={} frame={} monotonic_ns={}",
            self.mode.name(),
            self.submitted,
            commit_ns,
        )?;
        writeln!(
            output,
            "DD1_FRAME_SUBMITTED client={} frame={}",
            self.mode.name(),
            self.submitted,
        )?;
        output.flush()?;
        Ok(())
    }

    fn release_keyboard(&mut self) {
        if let Some(keyboard) = self.keyboard.take() {
            if keyboard.version() >= 3 {
                keyboard.release();
            }
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        drop(self.window.take());
        self.release_keyboard();
        if let Some(seat) = self.seat.take() {
            if seat.version() >= 5 {
                seat.release();
            }
        }
        if let Some(wm_base) = self.wm_base.take() {
            wm_base.destroy();
        }
    }
}

fn write_solid_pool(output: &mut impl Write, mode: Mode) -> io::Result<()> {
    let pixel = mode.pixel();
    let mut row = [0; STRIDE as usize];
    for destination in row.chunks_exact_mut(4) {
        destination.copy_from_slice(&pixel);
    }
    for _ in 0..HEIGHT as usize * BUFFER_COUNT {
        output.write_all(&row)?;
    }
    Ok(())
}

fn pressed_probe_key(key: u32, state: WEnum<wl_keyboard::KeyState>) -> Option<char> {
    match (state, key) {
        (WEnum::Value(wl_keyboard::KeyState::Pressed), 30) => Some('a'),
        (WEnum::Value(wl_keyboard::KeyState::Pressed), 48) => Some('b'),
        (WEnum::Value(wl_keyboard::KeyState::Pressed), 46) => Some('c'),
        _ => None,
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_compositor" if state.compositor.is_none() => {
                    state.compositor = Some(registry.bind(name, 1, qh, ()));
                }
                "wl_shm" if state.shm.is_none() => {
                    state.shm = Some(registry.bind(name, 1, qh, ()));
                }
                "xdg_wm_base" if state.wm_base.is_none() => {
                    state.wm_base = Some(registry.bind(name, 1, qh, ()));
                }
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, version.min(5), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for State {
    fn event(
        _: &mut Self,
        wm_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for State {
    fn event(
        state: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            state.configured = true;
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for State {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Close = event {
            state.running = false;
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.frame_ready = true;
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, usize> for State {
    fn event(
        state: &mut Self,
        _: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            if let Some(window) = state.window.as_mut() {
                window.buffers[*index].busy = false;
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(capabilities) } = event {
            if capabilities.contains(wl_seat::Capability::Keyboard) {
                if state.keyboard.is_none() {
                    state.keyboard = Some(seat.get_keyboard(qh, ()));
                }
            } else {
                state.release_keyboard();
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key { key, state: key_state, .. } = event {
            if let Some(key) = pressed_probe_key(key, key_state) {
                println!("DD1_KEY {key}");
            }
            if key == 1 && key_state == WEnum::Value(wl_keyboard::KeyState::Pressed) {
                state.running = false;
            }
        }
    }
}

delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_surface::WlSurface);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shm_pool_contains_two_complete_opaque_little_endian_images() {
        for (mode, expected_pixel) in [
            (Mode::Normal, [0x0d, 0x8e, 0xe6, 0xff]),
            (Mode::Slow, [0xd8, 0x9f, 0x3a, 0xff]),
        ] {
            let mut bytes = Vec::new();
            write_solid_pool(&mut bytes, mode).unwrap();
            assert_eq!(bytes.len(), 2 * 320 * 240 * 4);
            assert!(bytes.chunks_exact(4).all(|pixel| pixel == expected_pixel));
        }
    }

    #[test]
    fn keyboard_reports_only_pressed_evdev_abc_without_xkb_offset() {
        for (code, expected) in [(30, 'a'), (48, 'b'), (46, 'c')] {
            assert_eq!(
                pressed_probe_key(code, WEnum::Value(wl_keyboard::KeyState::Pressed)),
                Some(expected),
            );
            assert_eq!(
                pressed_probe_key(code, WEnum::Value(wl_keyboard::KeyState::Released)),
                None,
            );
            assert_eq!(pressed_probe_key(code, WEnum::Unknown(2)), None);
            assert_eq!(
                pressed_probe_key(code + 8, WEnum::Value(wl_keyboard::KeyState::Pressed)),
                None,
            );
        }
        assert_eq!(
            pressed_probe_key(1, WEnum::Value(wl_keyboard::KeyState::Pressed)),
            None,
        );
    }
}

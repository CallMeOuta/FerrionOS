// SPDX-License-Identifier: MIT
// Three ordinary xdg-shell clients for the DD2 layout check. Their rectangles
// are managed by the compositor; no fixture image is pasted onto the display.
mod shm_storage;
use std::{error::Error, time::Duration};
use wayland_client::{
    delegate_noop,
    protocol::{
        wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_registry, wl_seat, wl_shm,
        wl_shm_pool, wl_surface,
    },
    Connection, Dispatch, QueueHandle, WEnum,
};
use wayland_protocols::wp::presentation_time::client::{wp_presentation, wp_presentation_feedback};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

const SLOT_BYTES: usize = 1280 * 800 * 4;
const SLOT_COUNT: usize = 3;

type Result<T> = std::result::Result<T, Box<dyn Error>>;
struct State {
    role: String,
    slow: bool,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    shell: Option<xdg_wm_base::XdgWmBase>,
    presentation: Option<wp_presentation::WpPresentation>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    window: Option<(
        wl_surface::WlSurface,
        xdg_surface::XdgSurface,
        xdg_toplevel::XdgToplevel,
    )>,
    pool: Option<(wl_shm_pool::WlShmPool, shm_storage::Storage)>,
    buffers: Vec<(u64, wl_buffer::WlBuffer, (i32, i32), bool)>,
    size: (i32, i32),
    configured: bool,
    ready: bool,
    running: bool,
    frame: u64,
    key: u64,
}
fn now_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) },
        0
    );
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}
fn pixels(role: &str, width: i32, height: i32, key: u64) -> Vec<u8> {
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let mut rgb = if y < 34 {
                0x0c0b09
            } else if role != "terminal" && x < 180 {
                0x0b0a09
            } else {
                0x0f0e0c
            };
            // Client decorations have the same three round controls in each size.
            for (cx, color) in [(17, 0xd35f5f), (35, 0xd0a44f), (53, 0x58a56e)] {
                if (x - cx) * (x - cx) + (y - 15) * (y - 15) <= 25 {
                    rgb = color;
                }
            }
            // A keyboard-visible patch away from the layout's color samples.
            if x >= 200 && x < 232 && y >= 50 && y < 82 {
                rgb = if key % 2 == 0 { 0x3a9fd8 } else { 0xe68e0d };
            }
            data.extend_from_slice(&(0xff000000_u32 | rgb).to_le_bytes());
        }
    }
    data
}
impl State {
    fn draw(&mut self, qh: &QueueHandle<Self>, connection: &Connection) -> Result<()> {
        if !self.configured || !self.ready {
            return Ok(());
        }
        let slot = match self.buffers.iter().position(|b| !b.3) {
            Some(slot) => slot,
            None if self.buffers.len() < SLOT_COUNT => self.buffers.len(),
            None => return Ok(()),
        };
        if self.slow {
            std::thread::sleep(Duration::from_millis(100));
        }
        let (width, height) = self.size;
        let bytes = pixels(&self.role, width, height, self.key);
        if bytes.len() > SLOT_BYTES {
            return Err("window exceeds probe buffer capacity".into());
        }
        // Reuse three slots, writing only after wl_buffer.release. Keeping one
        // pool per window also avoids allocation churn in the timing series.
        let (pool, file) = self.pool.as_mut().unwrap();
        // Ordinary file writes and shared mappings disagree on the current OS;
        // shm-reuse-probe keeps that failure independently reproducible.
        file.write(slot * SLOT_BYTES, &bytes);
        if self.buffers.get(slot).is_none_or(|b| b.2 != self.size) {
            let buffer = pool.create_buffer(
                (slot * SLOT_BYTES) as i32,
                width,
                height,
                width * 4,
                wl_shm::Format::Argb8888,
                qh,
                slot as u64,
            );
            let entry = (slot as u64, buffer, self.size, false);
            if slot == self.buffers.len() {
                self.buffers.push(entry);
            } else {
                self.buffers[slot].1.destroy();
                self.buffers[slot] = entry;
            }
        }
        self.buffers[slot].3 = true;
        let buffer = &self.buffers[slot].1;
        self.frame += 1;
        let (surface, xdg, _) = self.window.as_ref().unwrap();
        xdg.set_window_geometry(0, 0, width, height);
        surface.attach(Some(buffer), 0, 0);
        surface.damage(0, 0, width, height);
        surface.frame(qh, ());
        self.ready = false;
        let commit_ns = now_ns();
        self.presentation
            .as_ref()
            .ok_or("no presentation feedback")?
            .feedback(surface, qh, (self.frame, commit_ns, self.key));
        surface.commit();
        connection.flush()?;
        println!(
            "DD2_CLIENT role={} frame={} key={} commit_ns={} width={} height={}",
            self.role, self.frame, self.key, commit_ns, width, height
        );
        Ok(())
    }
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let role = args
        .first()
        .filter(|r| ["files", "editor", "terminal"].contains(&r.as_str()))
        .ok_or("usage: desktop-layout-probe files|editor|terminal [--slow]")?
        .clone();
    if args.len() > 2 || args.get(1).is_some_and(|s| s != "--slow") {
        return Err("unexpected argument".into());
    }
    let connection = Connection::connect_to_env()?;
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    let mut state = State {
        role,
        slow: args.len() == 2,
        compositor: None,
        shm: None,
        shell: None,
        presentation: None,
        keyboard: None,
        window: None,
        pool: None,
        buffers: Vec::new(),
        size: (400, 300),
        configured: false,
        ready: true,
        running: true,
        frame: 0,
        key: 0,
    };
    connection.display().get_registry(&qh, ());
    queue.roundtrip(&mut state)?;
    queue.roundtrip(&mut state)?;
    let file = shm_storage::Storage::new(SLOT_BYTES * SLOT_COUNT)?;
    let pool = state.shm.as_ref().ok_or("no shared memory")?.create_pool(
        file.fd(),
        (SLOT_BYTES * SLOT_COUNT) as i32,
        &qh,
        (),
    );
    state.pool = Some((pool, file));
    let surface = state
        .compositor
        .as_ref()
        .ok_or("no compositor")?
        .create_surface(&qh, ());
    let xdg = state
        .shell
        .as_ref()
        .ok_or("no xdg-shell")?
        .get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id(format!("ferrion-desktop-{}", state.role));
    toplevel.set_title(format!("FerrionOS {} probe", state.role));
    surface.commit();
    state.window = Some((surface, xdg, toplevel));
    while state.running {
        queue.blocking_dispatch(&mut state)?;
        state.draw(&qh, &connection)?;
    }
    Ok(())
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
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => state.compositor = Some(registry.bind(name, 1, qh, ())),
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => state.shell = Some(registry.bind(name, 1, qh, ())),
                "wp_presentation" => state.presentation = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" => {
                    let _: wl_seat::WlSeat = registry.bind(name, version.min(5), qh, ());
                }
                _ => (),
            }
        }
    }
}
impl Dispatch<xdg_wm_base::XdgWmBase, ()> for State {
    fn event(
        _: &mut Self,
        shell: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            shell.pong(serial);
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
            state.ready = true;
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
        match event {
            xdg_toplevel::Event::Close => state.running = false,
            xdg_toplevel::Event::Configure { width, height, .. } => {
                if width > 0 {
                    state.size.0 = width;
                }
                if height > 0 {
                    state.size.1 = height;
                }
                assert!(state.size.0 <= 4096 && state.size.1 <= 4096);
            }
            _ => (),
        }
    }
}
impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.ready = true;
    }
}
impl Dispatch<wl_buffer::WlBuffer, u64> for State {
    fn event(
        state: &mut Self,
        buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        frame: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            if let Some(slot) = state
                .buffers
                .iter_mut()
                .find(|b| b.0 == *frame && b.1 == *buffer)
            {
                slot.3 = false;
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
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(c),
        } = event
        {
            if c.contains(wl_seat::Capability::Keyboard) && state.keyboard.is_none() {
                state.keyboard = Some(seat.get_keyboard(qh, ()));
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
        if let wl_keyboard::Event::Key {
            key,
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
        {
            if key == 1 {
                state.running = false;
            } else {
                state.key += 1;
                println!(
                    "DD2_KEY role={} key={} received_ns={}",
                    state.role,
                    state.key,
                    now_ns()
                );
            }
        }
    }
}
delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_surface::WlSurface);

impl Dispatch<wp_presentation::WpPresentation, ()> for State {
    fn event(
        _: &mut Self,
        _: &wp_presentation::WpPresentation,
        event: wp_presentation::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            assert_eq!(
                clk_id,
                libc::CLOCK_MONOTONIC as u32,
                "presentation clock must be monotonic"
            );
        }
    }
}
impl Dispatch<wp_presentation_feedback::WpPresentationFeedback, (u64, u64, u64)> for State {
    fn event(
        state: &mut Self,
        _: &wp_presentation_feedback::WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        data: &(u64, u64, u64),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                seq_hi,
                seq_lo,
                ..
            } => {
                let presented_ns = ((u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo))
                    * 1_000_000_000
                    + u64::from(tv_nsec);
                let output_frame = (u64::from(seq_hi) << 32) | u64::from(seq_lo);
                println!("DD2_PRESENT role={} frame={} key={} commit_ns={} presented_ns={} output_frame={}", state.role, data.0, data.2, data.1, presented_ns, output_frame);
            }
            wp_presentation_feedback::Event::Discarded => {
                println!("DD2_DISCARDED role={} frame={}", state.role, data.0)
            }
            _ => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::pixels;

    #[test]
    fn odd_width_buffers_keep_their_row_stride_and_controls() {
        for (role, width) in [("files", 409), ("editor", 620), ("terminal", 209)] {
            let bytes = pixels(role, width, 730, 0);
            assert_eq!(bytes.len(), width as usize * 730 * 4);
            let at = ((15 * width + 17) * 4) as usize;
            assert_eq!(&bytes[at..at + 4], &0xffd35f5fu32.to_le_bytes());
            let at = ((50 * width + 200) * 4) as usize;
            assert_eq!(&bytes[at..at + 4], &0xff3a9fd8u32.to_le_bytes());
        }
    }
}

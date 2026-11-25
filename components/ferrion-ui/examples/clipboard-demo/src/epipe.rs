use std::fs::OpenOptions;
use std::io::{Read, Write, pipe};
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::time::Duration;

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::WlDataDeviceManager;
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, QueueHandle, event_created_child};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};

const MAX_EPIPE_DISPATCHES: usize = 400;
const MAX_EPIPE_WAIT: Duration = Duration::from_secs(10);
const TARGET_MIME: &str = "text/plain;charset=utf-8";

struct EpipeState {
    configured: bool,
    buffer: Option<WlBuffer>,
    surface: Option<WlSurface>,
    offered_utf8: bool,
    selection: Option<WlDataOffer>,
    closed: bool,
}

impl Dispatch<WlRegistry, GlobalListContents> for EpipeState {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wayland_client::protocol::wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlCompositor, ()> for EpipeState {
    fn event(
        _: &mut Self,
        _: &WlCompositor,
        _: wayland_client::protocol::wl_compositor::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSurface, ()> for EpipeState {
    fn event(
        _: &mut Self,
        _: &WlSurface,
        _: wayland_client::protocol::wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShm, ()> for EpipeState {
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

impl Dispatch<WlShmPool, ()> for EpipeState {
    fn event(
        _: &mut Self,
        _: &WlShmPool,
        _: wayland_client::protocol::wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlBuffer, ()> for EpipeState {
    fn event(
        _: &mut Self,
        _: &WlBuffer,
        _: wayland_client::protocol::wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgWmBase, ()> for EpipeState {
    fn event(
        _: &mut Self,
        wm: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for EpipeState {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            state.configured = true;
            if let (Some(surface), Some(buffer)) = (&state.surface, &state.buffer) {
                surface.attach(Some(buffer), 0, 0);
                surface.damage_buffer(0, 0, 1, 1);
                surface.commit();
            }
        }
    }
}

impl Dispatch<XdgToplevel, ()> for EpipeState {
    fn event(
        state: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, xdg_toplevel::Event::Close) {
            state.closed = true;
        }
    }
}

impl Dispatch<WlSeat, ()> for EpipeState {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: wayland_client::protocol::wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataDeviceManager, ()> for EpipeState {
    fn event(
        _: &mut Self,
        _: &WlDataDeviceManager,
        _: wayland_client::protocol::wl_data_device_manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataOffer, ()> for EpipeState {
    fn event(
        state: &mut Self,
        _: &WlDataOffer,
        event: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event
            && mime_type == TARGET_MIME
        {
            state.offered_utf8 = true;
        }
    }
}

impl Dispatch<WlDataDevice, ()> for EpipeState {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_device::Event::Selection { id } = event {
            state.selection = id;
        }
    }

    event_created_child!(EpipeState, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

pub(crate) fn run() -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("connect: {e}"))?;
    let (globals, mut queue) =
        registry_queue_init::<EpipeState>(&conn).map_err(|e| format!("registry: {e}"))?;
    let qh = queue.handle();

    let compositor = globals
        .bind::<WlCompositor, _, _>(&qh, 1..=6, ())
        .map_err(|e| format!("wl_compositor: {e}"))?;
    let shm = globals
        .bind::<WlShm, _, _>(&qh, 1..=2, ())
        .or_else(|_| globals.bind::<WlShm, _, _>(&qh, 1..=1, ()))
        .map_err(|e| format!("wl_shm: {e}"))?;
    let wm_base = globals
        .bind::<XdgWmBase, _, _>(&qh, 1..=6, ())
        .map_err(|e| format!("xdg_wm_base: {e}"))?;
    let seat = globals
        .bind::<WlSeat, _, _>(&qh, 1..=8, ())
        .map_err(|e| format!("wl_seat: {e}"))?;
    let ddm = globals
        .bind::<WlDataDeviceManager, _, _>(&qh, 1..=3, ())
        .map_err(|e| format!("wl_data_device_manager: {e}"))?;

    let _data_device = ddm.get_data_device(&seat, &qh, ());

    let dir = rune::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let shm_path = dir.join(format!("rune-epipe-{}.shm", std::process::id()));
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&shm_path)
        .map_err(|e| format!("shm file: {e}"))?;
    file.write_all(&[0x20, 0x20, 0x20, 0xff])
        .map_err(|e| format!("shm write: {e}"))?;
    file.flush().map_err(|e| format!("shm flush: {e}"))?;
    std::fs::remove_file(&shm_path).ok();

    let pool = shm.create_pool(file.as_fd(), 4, &qh, ());
    let buffer = pool.create_buffer(0, 1, 1, 4, wl_shm::Format::Xrgb8888, &qh, ());
    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title("EpipeReader".to_owned());
    toplevel.set_app_id("rune-epipe-reader".to_owned());
    surface.commit();

    let mut state = EpipeState {
        configured: false,
        buffer: Some(buffer),
        surface: Some(surface),
        offered_utf8: false,
        selection: None,
        closed: false,
    };

    let deadline = rune::time::now() + MAX_EPIPE_WAIT;
    for _ in 0..MAX_EPIPE_DISPATCHES {
        queue
            .roundtrip(&mut state)
            .map_err(|e| format!("roundtrip: {e}"))?;
        if let Some(offer) = state.selection.take()
            && state.offered_utf8
        {
            let (mut reader, writer) = pipe().map_err(|e| format!("pipe: {e}"))?;
            offer.receive(TARGET_MIME.to_owned(), writer.as_fd());
            conn.flush().map_err(|e| format!("flush: {e}"))?;
            drop(writer);
            let mut one = [0u8; 1];
            let got = reader.read(&mut one).unwrap_or(0);
            drop(reader);
            offer.destroy();
            conn.flush().ok();
            let mut out = std::io::stdout().lock();
            if writeln!(out, "EPIPE_READER_CLOSED bytes={got}").is_ok() {
                out.flush().ok();
            }
            return Ok(());
        }
        if state.closed || rune::time::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    Err("timed out waiting for Wayland selection offer".to_owned())
}

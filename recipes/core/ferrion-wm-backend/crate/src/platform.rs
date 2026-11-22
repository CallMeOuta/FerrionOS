// SPDX-License-Identifier: MIT
// DRM and inputd integration follows Orbital src/core.rs and
// src/compositor/display.rs, and base graphics-ipc. Their notices are in LICENSE.
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use drm::buffer::{Buffer as _, DrmFourcc};
use drm::control::{ClipRect, Device as _, connector, crtc, dumbbuffer::DumbBuffer, framebuffer};
use drm::{ClientCapability, Device as _, DriverCapability};
use graphics_ipc::redox_uapi_exts::REDOX_DRM_CLIENT_CAP_HOTPLUG_EVENTS;
use graphics_ipc::{CpuBackedBuffer, DrmHandle, RedoxDrmEvent};
use inputd::{ConsumerHandle, ConsumerHandleEvent, ControlHandle};

use crate::{BackendEvent, CursorTiming, Input, Presentation, input::InputState};

pub struct Backend {
    input: Arc<ConsumerHandle>,
    display: Display,
    shared: Arc<Shared>,
    receiver: mpsc::Receiver<BackendEvent>,
    stop_writer: File,
    reader: Option<JoinHandle<()>>,
    epoch: Instant,
    vt: usize,
}

enum DisplayPhase {
    Active { cursor: Cursor },
    ChangeQueued { cursor: Cursor },
    ChangePending,
    Stopped,
}

fn display_change_pending_err() -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, "display change pending")
}

struct Shared {
    phase: Mutex<DisplayPhase>,
    changed: Condvar,
    failure: Mutex<Option<io::Error>>,
    wakeup: Box<dyn Fn() + Send + Sync>,
}

struct Display {
    drm: Arc<DrmHandle>,
    framebuffer: framebuffer::Handle,
    buffer: Option<CpuBackedBuffer>,
    dimensions: (u32, u32),
    refresh: Option<Duration>,
}

struct Cursor {
    drm: Arc<DrmHandle>,
    crtc: crtc::Handle,
    buffer: DumbBuffer,
    dimensions: (u32, u32),
}

impl Backend {
    pub fn open() -> io::Result<Self> {
        Self::open_with_wakeup(|| {})
    }

    pub fn open_with_wakeup(wakeup: impl Fn() + Send + Sync + 'static) -> io::Result<Self> {
        let epoch = Instant::now();
        let input = Arc::new(ConsumerHandle::new_vt()?);
        let vt = consumer_vt(&input)?;
        let (display, cursor) = Display::open(&input)?;
        let (stop_reader, stop_writer) = stop_pipe()?;
        let shared = Arc::new(Shared {
            phase: Mutex::new(DisplayPhase::Active { cursor }),
            changed: Condvar::new(),
            failure: Mutex::new(None),
            wakeup: Box::new(wakeup),
        });
        // Bounded storage: never block the reader behind rendering. Overflow is
        // a visible terminal error rather than dropping a key release silently.
        let (sender, receiver) = mpsc::sync_channel(1024);
        let thread_input = Arc::clone(&input);
        let thread_shared = Arc::clone(&shared);
        let reader = thread::Builder::new()
            .name("ferrion-wm-input".into())
            .spawn(move || {
                if let Err(error) =
                    read_input(thread_input, &thread_shared, sender, stop_reader, epoch)
                {
                    *thread_shared.failure.lock().unwrap() = Some(error);
                    (thread_shared.wakeup)();
                }
            })?;
        let backend = Self {
            input,
            display,
            shared,
            receiver,
            stop_writer,
            reader: Some(reader),
            epoch,
            vt,
        };
        // Activate only the VT that inputd allocated to this consumer. A VT
        // environment variable cannot establish ownership and is not trusted.
        ControlHandle::new()?.activate_vt(vt)?;
        Ok(backend)
    }

    pub fn vt(&self) -> usize {
        self.vt
    }
    pub fn dimensions(&self) -> (u32, u32) {
        self.display.dimensions
    }
    pub fn stride(&self) -> u32 {
        self.display.buffer.as_ref().unwrap().buffer().pitch()
    }
    pub fn now_us(&self) -> u64 {
        elapsed_us(self.epoch)
    }

    /// Reported mode period, if the driver supplied a nonzero refresh rate.
    /// This is not a measured vblank clock.
    pub fn refresh_period(&self) -> Option<Duration> {
        self.display.refresh
    }

    pub fn framebuffer_mut(&mut self) -> &mut [u8] {
        let phase = self.shared.phase.lock().unwrap();
        assert!(matches!(
            *phase,
            DisplayPhase::Active { .. } | DisplayPhase::ChangeQueued { .. }
        ));
        drop(phase);
        self.display.buffer.as_mut().unwrap().shadow_buf()
    }

    pub fn present(&mut self) -> io::Result<Presentation> {
        self.check_failure()?;
        {
            let phase = self.shared.phase.lock().unwrap();
            match *phase {
                DisplayPhase::Active { .. } => {}
                DisplayPhase::ChangeQueued { .. }
                | DisplayPhase::ChangePending
                | DisplayPhase::Stopped => {
                    return Err(display_change_pending_err());
                }
            }
        }
        let started_us = self.now_us();
        let (width, height) = self.display.dimensions;
        self.display
            .buffer
            .as_mut()
            .unwrap()
            .sync_rect(0, 0, width, height);
        let result = self.display.drm.dirty_framebuffer(
            self.display.framebuffer,
            &[ClipRect::new(0, 0, width as u16, height as u16)],
        );
        match result {
            Ok(()) => Ok(Presentation {
                started_us,
                completed_us: self.now_us(),
            }),
            Err(error) => {
                let phase = self.shared.phase.lock().unwrap();
                if matches!(*phase, DisplayPhase::Active { .. }) {
                    Err(error)
                } else {
                    Err(display_change_pending_err())
                }
            }
        }
    }

    pub fn try_event(&mut self) -> io::Result<Option<BackendEvent>> {
        self.check_failure()?;
        match self.receiver.try_recv() {
            Ok(BackendEvent::DisplayChanged) => {
                let mut phase = self.shared.phase.lock().unwrap();
                let prev = std::mem::replace(&mut *phase, DisplayPhase::ChangePending);
                assert!(matches!(prev, DisplayPhase::ChangeQueued { .. }));
                drop(prev);
                Ok(Some(BackendEvent::DisplayChanged))
            }
            Ok(event) => Ok(Some(event)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                self.check_failure()?;
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "input reader stopped",
                ))
            }
        }
    }

    pub fn cursor_size(&self) -> io::Result<(u32, u32)> {
        let phase = self.shared.phase.lock().unwrap();
        match &*phase {
            DisplayPhase::Active { cursor } | DisplayPhase::ChangeQueued { cursor } => {
                Ok((cursor.buffer.size().0, cursor.buffer.size().1))
            }
            DisplayPhase::ChangePending | DisplayPhase::Stopped => {
                Err(display_change_pending_err())
            }
        }
    }

    pub fn set_cursor(
        &mut self,
        pixels: Option<&[u8]>,
        size: (u32, u32),
        hotspot: (i32, i32),
    ) -> io::Result<()> {
        self.check_failure()?;
        let mut phase = self.shared.phase.lock().unwrap();
        let cursor = match &mut *phase {
            DisplayPhase::Active { cursor } => cursor,
            DisplayPhase::ChangeQueued { .. }
            | DisplayPhase::ChangePending
            | DisplayPhase::Stopped => {
                return Err(display_change_pending_err());
            }
        };
        let Some(pixels) = pixels else {
            #[allow(deprecated)]
            return cursor
                .drm
                .set_cursor2(cursor.crtc, None::<&DumbBuffer>, (0, 0));
        };
        let (width, height) = size;
        let bytes = u64::from(width) * u64::from(height) * 4;
        if width == 0
            || height == 0
            || width > cursor.buffer.size().0
            || height > cursor.buffer.size().1
            || bytes != pixels.len() as u64
            || hotspot.0 < 0
            || hotspot.1 < 0
            || hotspot.0 as u32 >= width
            || hotspot.1 as u32 >= height
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cursor image does not fit its plane",
            ));
        }
        let pitch = cursor.buffer.pitch() as usize;
        {
            let mut mapping = cursor.drm.map_dumb_buffer(&mut cursor.buffer)?;
            mapping.fill(0);
            let row_bytes = width as usize * 4;
            for (row, source) in pixels.chunks_exact(row_bytes).enumerate() {
                mapping[row * pitch..row * pitch + row_bytes].copy_from_slice(source);
            }
        }
        #[allow(deprecated)]
        cursor
            .drm
            .set_cursor2(cursor.crtc, Some(&cursor.buffer), hotspot)
    }

    pub fn reconfigure(&mut self) -> io::Result<()> {
        self.check_failure()?;
        let mut phase = self.shared.phase.lock().unwrap();
        assert!(matches!(*phase, DisplayPhase::ChangePending));
        let (display, replacement) = Display::open(&self.input)?;
        self.display = display;
        *phase = DisplayPhase::Active {
            cursor: replacement,
        };
        self.shared.changed.notify_one();
        Ok(())
    }

    fn check_failure(&self) -> io::Result<()> {
        let failure = self.shared.failure.lock().unwrap();
        if let Some(error) = failure.as_ref() {
            return Err(io::Error::new(error.kind(), error.to_string()));
        }
        Ok(())
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        let prev = {
            let mut phase = self.shared.phase.lock().unwrap();
            let prev = std::mem::replace(&mut *phase, DisplayPhase::Stopped);
            self.shared.changed.notify_one();
            prev
        };
        let _ = self.stop_writer.write_all(&[1]);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if let DisplayPhase::Active { cursor } = prev {
            #[allow(deprecated)]
            let _ = cursor
                .drm
                .set_cursor2(cursor.crtc, None::<&DumbBuffer>, (0, 0));
        }
    }
}

impl Display {
    fn open(input: &ConsumerHandle) -> io::Result<(Self, Cursor)> {
        let drm = Arc::new(DrmHandle::from_file(input.open_display()?)?);
        drm.set_client_capability(ClientCapability::UniversalPlanes, true)?;
        // Virtual devices use hotspots; physical devices may legitimately omit
        // this capability while still providing a hardware cursor.
        let _ = drm.set_client_capability(ClientCapability::CursorPlaneHotspot, true);
        drm_ffi::set_capability(
            drm.as_fd(),
            u64::from(REDOX_DRM_CLIENT_CAP_HOTPLUG_EVENTS),
            true,
        )?;
        let resources = drm.resource_handles()?;
        for &connector in resources.connectors() {
            let info = drm.get_connector(connector, true)?;
            if info.state() != connector::State::Connected {
                continue;
            }
            let Some(&mode) = info.modes().first() else {
                continue;
            };
            let dimensions = (u32::from(mode.size().0), u32::from(mode.size().1));
            if dimensions.0 == 0 || dimensions.1 == 0 {
                continue;
            }
            for &encoder in info.encoders() {
                for crtc in resources.filter_crtcs(drm.get_encoder(encoder)?.possible_crtcs()) {
                    if !has_cursor(&drm, crtc)? {
                        continue;
                    }
                    let buffer = CpuBackedBuffer::new(&drm, dimensions, DrmFourcc::Argb8888, 32)?;
                    let framebuffer = match drm.add_framebuffer(buffer.buffer(), 32, 32) {
                        Ok(fb) => fb,
                        Err(error) => {
                            let _ = buffer.destroy(&drm);
                            return Err(error);
                        }
                    };
                    let mut display = Self {
                        drm: Arc::clone(&drm),
                        framebuffer,
                        buffer: Some(buffer),
                        dimensions,
                        refresh: (mode.vrefresh() > 0)
                            .then(|| Duration::from_secs_f64(1.0 / f64::from(mode.vrefresh()))),
                    };
                    display.buffer.as_mut().unwrap().shadow_buf().fill(0);
                    display
                        .buffer
                        .as_mut()
                        .unwrap()
                        .sync_rect(0, 0, dimensions.0, dimensions.1);
                    drm.set_crtc(crtc, Some(framebuffer), (0, 0), &[connector], Some(mode))?;
                    let cursor = Cursor::new(Arc::clone(&drm), crtc, dimensions)?;
                    return Ok((display, cursor));
                }
            }
        }
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no connected DRM output with a hardware cursor plane",
        ))
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        let _ = self.drm.destroy_framebuffer(self.framebuffer);
        if let Some(buffer) = self.buffer.take() {
            let _ = buffer.destroy(&self.drm);
        }
    }
}

fn has_cursor(drm: &DrmHandle, crtc: crtc::Handle) -> io::Result<bool> {
    let resources = drm.resource_handles()?;
    for plane in drm.plane_handles()? {
        let info = drm.get_plane(plane)?;
        if !resources
            .filter_crtcs(info.possible_crtcs())
            .contains(&crtc)
        {
            continue;
        }
        for (&property, &value) in drm.get_properties(plane)?.iter() {
            if drm.get_property(property)?.name() == c"type"
                && value == u64::from(drm_ffi::DRM_PLANE_TYPE_CURSOR)
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

impl Cursor {
    fn new(drm: Arc<DrmHandle>, crtc: crtc::Handle, dimensions: (u32, u32)) -> io::Result<Self> {
        let width = u32::try_from(drm.get_driver_capability(DriverCapability::CursorWidth)?)
            .map_err(|_| io::Error::other("cursor width overflows"))?;
        let height = u32::try_from(drm.get_driver_capability(DriverCapability::CursorHeight)?)
            .map_err(|_| io::Error::other("cursor height overflows"))?;
        if width < 16 || height < 24 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "hardware cursor is smaller than the pointer image",
            ));
        }
        let mut cursor = Self {
            buffer: drm.create_dumb_buffer((width, height), DrmFourcc::Argb8888, 32)?,
            drm,
            crtc,
            dimensions,
        };
        let pitch = cursor.buffer.pitch() as usize;
        {
            let mut mapping = cursor.drm.map_dumb_buffer(&mut cursor.buffer)?;
            mapping.fill(0);
            // Original small arrow, opaque white interior and dark outline.
            for y in 0..24_usize {
                for x in 0..=(y / 2).min(15) {
                    let edge = x == 0 || x == y / 2 || y == 23;
                    let pixel = if edge { 0xff101010_u32 } else { 0xffeeeeee_u32 };
                    let offset = y * pitch + x * 4;
                    mapping[offset..offset + 4].copy_from_slice(&pixel.to_ne_bytes());
                }
            }
        }
        #[allow(deprecated)]
        cursor.drm.set_cursor2(crtc, Some(&cursor.buffer), (0, 0))?;
        #[allow(deprecated)]
        cursor.drm.move_cursor(crtc, (0, 0))?;
        Ok(cursor)
    }
}

impl Drop for Cursor {
    fn drop(&mut self) {
        // Do not disable a replacement cursor installed on this same VT.
        let _ = self.drm.destroy_dumb_buffer(self.buffer);
    }
}

fn queue_display_change(shared: &Shared) -> bool {
    let mut phase = shared.phase.lock().unwrap();
    let prev = std::mem::replace(&mut *phase, DisplayPhase::Stopped);
    match prev {
        DisplayPhase::Active { cursor } => {
            *phase = DisplayPhase::ChangeQueued { cursor };
            true
        }
        DisplayPhase::Stopped => false,
        DisplayPhase::ChangeQueued { .. } | DisplayPhase::ChangePending => {
            unreachable!("reader queued a second display change before reconfigure")
        }
    }
}

fn read_input(
    input: Arc<ConsumerHandle>,
    shared: &Shared,
    sender: mpsc::SyncSender<BackendEvent>,
    stop: File,
    epoch: Instant,
) -> io::Result<()> {
    let mut state = InputState::new();
    let mut events = [unsafe { std::mem::zeroed() }; 32];
    loop {
        let drm = {
            let mut phase = shared.phase.lock().unwrap();
            loop {
                match &*phase {
                    DisplayPhase::Active { cursor } => break Arc::clone(&cursor.drm),
                    DisplayPhase::ChangeQueued { .. } | DisplayPhase::ChangePending => {
                        phase = shared.changed.wait(phase).unwrap();
                    }
                    DisplayPhase::Stopped => return Ok(()),
                }
            }
        };
        let mut fds = [
            libc::pollfd {
                fd: input.event_handle().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: stop.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: drm.as_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if fds[1].revents != 0 || matches!(*shared.phase.lock().unwrap(), DisplayPhase::Stopped) {
            return Ok(());
        }
        if fds
            .iter()
            .any(|fd| fd.revents & (libc::POLLERR | libc::POLLNVAL | libc::POLLHUP) != 0)
        {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "input/display poll descriptor closed",
            ));
        }
        if fds[2].revents & libc::POLLIN != 0 {
            let mut changed = false;
            for event in drm.redox_receive_events()? {
                changed |= matches!(event, RedoxDrmEvent::RedoxConnectorHotplug(_));
            }
            if changed {
                if !queue_display_change(shared) {
                    return Ok(());
                }
                send(&sender, shared, BackendEvent::DisplayChanged)?;
                continue;
            }
        }
        if fds[0].revents & libc::POLLIN == 0 {
            continue;
        }
        loop {
            if matches!(*shared.phase.lock().unwrap(), DisplayPhase::Stopped) {
                return Ok(());
            }
            let event = match input.read_events(&mut events) {
                Ok(event) => event,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            let read_us = elapsed_us(epoch);
            match event {
                ConsumerHandleEvent::Handoff => {
                    if !queue_display_change(shared) {
                        return Ok(());
                    }
                    send(&sender, shared, BackendEvent::DisplayChanged)?;
                    break;
                }
                ConsumerHandleEvent::Events(&[]) => break,
                ConsumerHandleEvent::Events(events) => {
                    for wire in events {
                        let phase = shared.phase.lock().unwrap();
                        let cursor = match &*phase {
                            DisplayPhase::Active { cursor } => cursor,
                            DisplayPhase::Stopped => return Ok(()),
                            DisplayPhase::ChangeQueued { .. } | DisplayPhase::ChangePending => {
                                unreachable!("reader converted input outside Active phase")
                            }
                        };
                        for event in state
                            .convert(wire.code, wire.a, wire.b, read_us, cursor.dimensions)
                            .into_iter()
                            .flatten()
                        {
                            if let Input::Motion(motion) = event {
                                assert!(matches!(&*phase, DisplayPhase::Active { .. }));
                                #[allow(deprecated)]
                                cursor.drm.move_cursor(cursor.crtc, (motion.x, motion.y))?;
                                let timing = CursorTiming {
                                    read_us,
                                    completed_us: elapsed_us(epoch),
                                    x: motion.x,
                                    y: motion.y,
                                };
                                send(&sender, shared, BackendEvent::CursorTiming(timing))?;
                            }
                            send(&sender, shared, BackendEvent::Input(event))?;
                        }
                    }
                }
            }
        }
    }
}

fn send(
    sender: &mpsc::SyncSender<BackendEvent>,
    shared: &Shared,
    event: BackendEvent,
) -> io::Result<()> {
    sender.try_send(event).map_err(|error| match error {
        mpsc::TrySendError::Full(_) => {
            io::Error::other("input queue overflow; compositor is not draining events")
        }
        mpsc::TrySendError::Disconnected(_) => io::Error::new(
            io::ErrorKind::BrokenPipe,
            "compositor event receiver closed",
        ),
    })?;
    (shared.wakeup)();
    Ok(())
}

fn elapsed_us(epoch: Instant) -> u64 {
    epoch.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

fn consumer_vt(input: &ConsumerHandle) -> io::Result<usize> {
    let mut path = [0_u8; 4096];
    let length = libredox::call::fpath(input.event_handle().as_raw_fd() as usize, &mut path)?;
    let path = std::str::from_utf8(&path[..length]).map_err(io::Error::other)?;
    path.rsplit('/')
        .next()
        .and_then(|number| number.parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "inputd returned no VT number"))
}

fn stop_pipe() -> io::Result<(File, File)> {
    let mut fds = [-1; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // A successful pipe2 transfers exactly these two newly allocated fds.
    Ok(unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) })
}

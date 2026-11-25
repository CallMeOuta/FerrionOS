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
use drm::control::{Device as _, connector, crtc, dumbbuffer::DumbBuffer};
use drm::{ClientCapability, Device as _, DriverCapability};
use graphics_ipc::redox_uapi_exts::REDOX_DRM_CLIENT_CAP_HOTPLUG_EVENTS;
use graphics_ipc::{DrmHandle, RedoxDrmEvent};
use inputd::ControlHandle;

use crate::presenter::{self, Presenter};
use crate::vt::{Consumer, ConsumerEvent};
use crate::{
    BackendEvent, CursorTiming, FlipCompletion, Input, PresentMode, PresentPreference,
    Presentation, input::InputState,
};

pub struct Backend {
    input: Arc<Consumer>,
    display: Display,
    preference: PresentPreference,
    shared: Arc<Shared>,
    receiver: mpsc::Receiver<BackendEvent>,
    stop_writer: File,
    reader: Option<JoinHandle<()>>,
    epoch: Instant,
    vt: usize,
}

struct ActiveOutput {
    drm: Arc<DrmHandle>,
    dimensions: (u32, u32),
    cursor: Option<Cursor>,
}

enum DisplayPhase {
    Active(ActiveOutput),
    ChangeQueued(ActiveOutput),
    ChangePending,
    Stopped,
}

fn display_change_pending_err() -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, "display change pending")
}

fn no_cursor_plane_err() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "no hardware cursor plane")
}

struct Shared {
    phase: Mutex<DisplayPhase>,
    changed: Condvar,
    failure: Mutex<Option<io::Error>>,
    wakeup: Box<dyn Fn() + Send + Sync>,
}

struct Display {
    presenter: Presenter,
    present_mode: PresentMode,
    dimensions: (u32, u32),
    refresh: Option<Duration>,
}

struct Cursor {
    drm: Arc<DrmHandle>,
    crtc: crtc::Handle,
    buffer: DumbBuffer,
}

impl Backend {
    pub fn open() -> io::Result<Self> {
        Self::open_with_presenter(|| {}, PresentPreference::DirtyFb)
    }

    pub fn open_with_wakeup(wakeup: impl Fn() + Send + Sync + 'static) -> io::Result<Self> {
        Self::open_with_presenter(wakeup, PresentPreference::DirtyFb)
    }

    pub fn open_with_presenter(
        wakeup: impl Fn() + Send + Sync + 'static,
        preference: PresentPreference,
    ) -> io::Result<Self> {
        let epoch = Instant::now();
        let (consumer, inherited) = Consumer::open_or_inherited()?;
        let vt = consumer.vt();
        let input = Arc::new(consumer);
        let (display, output) = Display::open(&input, preference)?;
        let (stop_reader, stop_writer) = stop_pipe()?;
        let shared = Arc::new(Shared {
            phase: Mutex::new(DisplayPhase::Active(output)),
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
            preference,
            shared,
            receiver,
            stop_writer,
            reader: Some(reader),
            epoch,
            vt,
        };
        // Activate only the VT that inputd allocated to this consumer. A VT
        // environment variable cannot establish ownership and is not trusted.
        if !inherited {
            ControlHandle::new()?.activate_vt(vt)?;
        }
        Ok(backend)
    }

    pub fn vt(&self) -> usize {
        self.vt
    }
    pub fn dimensions(&self) -> (u32, u32) {
        self.display.dimensions
    }
    pub fn stride(&self) -> u32 {
        self.display.presenter.stride()
    }
    pub fn present_mode(&self) -> PresentMode {
        debug_assert_eq!(
            self.display.present_mode,
            self.display.presenter.present_mode()
        );
        self.display.present_mode
    }
    pub fn flip_in_flight(&self) -> bool {
        self.display.presenter.flip_in_flight()
    }
    pub fn complete_flip(&mut self) -> io::Result<()> {
        self.check_failure()?;
        self.display.presenter.complete_flip()
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
            DisplayPhase::Active(_) | DisplayPhase::ChangeQueued(_)
        ));
        drop(phase);
        self.display.presenter.framebuffer_mut()
    }

    pub fn present(&mut self) -> io::Result<Presentation> {
        self.check_failure()?;
        {
            let phase = self.shared.phase.lock().unwrap();
            match *phase {
                DisplayPhase::Active(_) => {}
                DisplayPhase::ChangeQueued(_)
                | DisplayPhase::ChangePending
                | DisplayPhase::Stopped => {
                    return Err(display_change_pending_err());
                }
            }
        }
        let started_us = self.now_us();
        let epoch = self.epoch;
        let result = self
            .display
            .presenter
            .present(started_us, || elapsed_us(epoch));
        match result {
            Ok(presentation) => Ok(presentation),
            Err(error) => {
                let phase = self.shared.phase.lock().unwrap();
                if matches!(*phase, DisplayPhase::Active(_)) {
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
                assert!(matches!(prev, DisplayPhase::ChangeQueued(_)));
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
            DisplayPhase::Active(output) | DisplayPhase::ChangeQueued(output) => {
                let Some(cursor) = &output.cursor else {
                    return Err(no_cursor_plane_err());
                };
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
        let output = match &mut *phase {
            DisplayPhase::Active(output) => output,
            DisplayPhase::ChangeQueued(_) | DisplayPhase::ChangePending | DisplayPhase::Stopped => {
                return Err(display_change_pending_err());
            }
        };
        let Some(cursor) = output.cursor.as_mut() else {
            return Err(no_cursor_plane_err());
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
        let (display, replacement) = Display::open(&self.input, self.preference)?;
        self.display = display;
        *phase = DisplayPhase::Active(replacement);
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
        if let DisplayPhase::Active(ActiveOutput {
            cursor: Some(cursor),
            ..
        }) = prev
        {
            #[allow(deprecated)]
            let _ = cursor
                .drm
                .set_cursor2(cursor.crtc, None::<&DumbBuffer>, (0, 0));
        }
    }
}

const MAX_DISPLAY_OPEN_ATTEMPTS: usize = 4;

impl Display {
    fn open(input: &Consumer, preference: PresentPreference) -> io::Result<(Self, ActiveOutput)> {
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
        for _ in 0..MAX_DISPLAY_OPEN_ATTEMPTS {
            let resources = drm.resource_handles()?;
            let mut fallback = None;
            let mut chosen = None;
            'scan: for &connector in resources.connectors() {
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
                        if has_cursor(&drm, crtc)? {
                            chosen = Some((connector, mode, dimensions, crtc, true));
                            break 'scan;
                        }
                        if fallback.is_none() {
                            fallback = Some((connector, mode, dimensions, crtc, false));
                        }
                    }
                }
            }
            let Some((connector, mode, dimensions, crtc, with_cursor)) = chosen.or(fallback) else {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "no connected DRM output",
                ));
            };
            let (display, output, hotplug_seen) = Self::setup_output(
                Arc::clone(&drm),
                connector,
                mode,
                dimensions,
                crtc,
                with_cursor,
                preference,
                None,
            )?;
            if !hotplug_seen {
                return Ok((display, output));
            }
        }
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "display changed repeatedly during startup probe",
        ))
    }

    fn setup_output(
        drm: Arc<DrmHandle>,
        connector: connector::Handle,
        mode: drm::control::Mode,
        dimensions: (u32, u32),
        crtc: crtc::Handle,
        with_cursor: bool,
        preference: PresentPreference,
        chosen_mode: Option<PresentMode>,
    ) -> io::Result<(Self, ActiveOutput, bool)> {
        let (presenter, present_mode, hotplug_seen) = match chosen_mode {
            Some(mode_choice) => (
                presenter::open_presenter_with_mode(
                    Arc::clone(&drm),
                    crtc,
                    connector,
                    mode,
                    mode_choice,
                )?,
                mode_choice,
                false,
            ),
            None => presenter::open_presenter_probing(
                Arc::clone(&drm),
                crtc,
                connector,
                mode,
                preference,
            )?,
        };
        let display = Self {
            presenter,
            present_mode,
            dimensions,
            refresh: (mode.vrefresh() > 0)
                .then(|| Duration::from_secs_f64(1.0 / f64::from(mode.vrefresh()))),
        };
        let cursor = if with_cursor {
            Some(Cursor::new(Arc::clone(&drm), crtc)?)
        } else {
            None
        };
        Ok((
            display,
            ActiveOutput {
                drm,
                dimensions,
                cursor,
            },
            hotplug_seen,
        ))
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
    fn new(drm: Arc<DrmHandle>, crtc: crtc::Handle) -> io::Result<Self> {
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
        DisplayPhase::Active(output) => {
            *phase = DisplayPhase::ChangeQueued(output);
            true
        }
        DisplayPhase::Stopped => false,
        DisplayPhase::ChangeQueued(_) | DisplayPhase::ChangePending => {
            unreachable!("reader queued a second display change before reconfigure")
        }
    }
}

fn read_input(
    input: Arc<Consumer>,
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
                    DisplayPhase::Active(output) => break Arc::clone(&output.drm),
                    DisplayPhase::ChangeQueued(_) | DisplayPhase::ChangePending => {
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
                match event {
                    RedoxDrmEvent::RedoxConnectorHotplug(_) => {
                        changed = true;
                    }
                    RedoxDrmEvent::PageFlip(flip) => {
                        let completion = FlipCompletion {
                            crtc: u32::from(flip.crtc),
                            completed_us: elapsed_us(epoch),
                            sequence: flip.frame,
                            tv_us: flip.duration.as_micros().min(u128::from(u64::MAX)) as u64,
                        };
                        send(&sender, shared, BackendEvent::FlipComplete(completion))?;
                    }
                    _ => {}
                }
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
                ConsumerEvent::Handoff => {
                    if !queue_display_change(shared) {
                        return Ok(());
                    }
                    send(&sender, shared, BackendEvent::DisplayChanged)?;
                    break;
                }
                ConsumerEvent::Events(&[]) => break,
                ConsumerEvent::Events(events) => {
                    for wire in events {
                        let phase = shared.phase.lock().unwrap();
                        let output = match &*phase {
                            DisplayPhase::Active(output) => output,
                            DisplayPhase::Stopped => return Ok(()),
                            DisplayPhase::ChangeQueued(_) | DisplayPhase::ChangePending => {
                                unreachable!("reader converted input outside Active phase")
                            }
                        };
                        for event in state
                            .convert(wire.code, wire.a, wire.b, read_us, output.dimensions)
                            .into_iter()
                            .flatten()
                        {
                            if let Input::Motion(motion) = event {
                                if let Some(cursor) = &output.cursor {
                                    assert!(matches!(&*phase, DisplayPhase::Active(_)));
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

fn stop_pipe() -> io::Result<(File, File)> {
    let mut fds = [-1; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // A successful pipe2 transfers exactly these two newly allocated fds.
    Ok(unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) })
}

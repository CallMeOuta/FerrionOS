// SPDX-License-Identifier: MIT
// DRM and inputd integration follows Orbital src/core.rs and
// src/compositor/display.rs, and base graphics-ipc. Their notices are in LICENSE.
use std::collections::HashMap;
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

use crate::input::{InputState, OutputRect};
use crate::presenter::{self, Presenter};
use crate::vt::{Consumer, ConsumerEvent};
use crate::{
    BackendEvent, CursorTiming, FlipCompletion, Input, MAX_OUTPUTS, OutputInfo, PresentMode,
    PresentPreference, Presentation,
};

pub struct Backend {
    input: Arc<Consumer>,
    display: Display,
    preference: PresentPreference,
    shared: Arc<Shared>,
    sender: mpsc::SyncSender<BackendEvent>,
    receiver: mpsc::Receiver<BackendEvent>,
    stop_writer: File,
    reader: Option<JoinHandle<()>>,
    epoch: Instant,
    vt: usize,
    display_generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChangeReason {
    Hotplug,
    Handoff,
}

struct ActiveHead {
    id: u32,
    rect: OutputRect,
    cursor: Option<Cursor>,
}

struct ActiveOutput {
    drm: Arc<DrmHandle>,
    bounds: (u32, u32),
    heads: Vec<ActiveHead>,
    active_cursor_head: Option<u32>,
}

impl ActiveOutput {
    fn rects(&self) -> [OutputRect; MAX_OUTPUTS] {
        let mut rects = [OutputRect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        }; MAX_OUTPUTS];
        for (slot, head) in rects.iter_mut().zip(self.heads.iter()) {
            *slot = head.rect;
        }
        rects
    }

    fn head_count(&self) -> usize {
        self.heads.len().min(MAX_OUTPUTS)
    }
}

enum DisplayPhase {
    Active(ActiveOutput),
    ChangeQueued(ActiveOutput, ChangeReason),
    ChangePending(Option<ActiveOutput>, ChangeReason),
    Stopped,
}

fn display_change_pending_err() -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, "display change pending")
}

fn no_cursor_plane_err() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "no hardware cursor plane")
}

fn unknown_output_err() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "unknown DRM output")
}

struct Shared {
    phase: Mutex<DisplayPhase>,
    changed: Condvar,
    failure: Mutex<Option<io::Error>>,
    wakeup: Box<dyn Fn() + Send + Sync>,
}

struct Head {
    info: OutputInfo,
    connector: connector::Handle,
    crtc: crtc::Handle,
    mode: drm::control::Mode,
    presenter: Presenter,
}

struct Display {
    drm: Arc<DrmHandle>,
    present_mode: PresentMode,
    heads: Vec<Head>,
    output_infos: Vec<OutputInfo>,
}

struct Cursor {
    drm: Arc<DrmHandle>,
    crtc: crtc::Handle,
    buffer: DumbBuffer,
    hotspot: (i32, i32),
    shown: bool,
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
        let (sender, receiver) = mpsc::sync_channel(1024);
        let thread_input = Arc::clone(&input);
        let thread_shared = Arc::clone(&shared);
        let thread_sender = sender.clone();
        let reader = thread::Builder::new()
            .name("ferrion-wm-input".into())
            .spawn(move || {
                if let Err(error) = read_input(
                    thread_input,
                    &thread_shared,
                    thread_sender,
                    stop_reader,
                    epoch,
                ) {
                    *thread_shared.failure.lock().unwrap() = Some(error);
                    (thread_shared.wakeup)();
                }
            })?;
        let backend = Self {
            input,
            display,
            preference,
            shared,
            sender,
            receiver,
            stop_writer,
            reader: Some(reader),
            epoch,
            vt,
            display_generation: 0,
        };
        if !inherited {
            ControlHandle::new()?.activate_vt(vt)?;
        }
        Ok(backend)
    }

    pub fn vt(&self) -> usize {
        self.vt
    }

    pub fn display_generation(&self) -> u64 {
        self.display_generation
    }

    pub fn outputs(&self) -> &[OutputInfo] {
        &self.display.output_infos
    }

    pub fn dimensions(&self) -> (u32, u32) {
        let primary = &self.display.heads[0].info;
        (primary.width, primary.height)
    }

    pub fn stride(&self) -> u32 {
        self.display.heads[0].presenter.stride()
    }

    pub fn stride_for(&self, output_id: u32) -> io::Result<u32> {
        let head = self
            .display
            .heads
            .iter()
            .find(|head| head.info.id == output_id)
            .ok_or_else(unknown_output_err)?;
        Ok(head.presenter.stride())
    }

    pub fn present_mode(&self) -> PresentMode {
        self.display.present_mode
    }

    pub fn flip_in_flight(&self) -> bool {
        self.display.heads[0].presenter.flip_in_flight()
    }

    pub fn flip_in_flight_for(&self, output_id: u32) -> bool {
        self.display
            .heads
            .iter()
            .find(|head| head.info.id == output_id)
            .is_some_and(|head| head.presenter.flip_in_flight())
    }

    pub fn complete_flip(&mut self) -> io::Result<()> {
        self.check_failure()?;
        self.display.heads[0].presenter.complete_flip()
    }

    pub fn complete_flip_for(&mut self, crtc: u32) -> io::Result<bool> {
        self.check_failure()?;
        let Some(head) = self
            .display
            .heads
            .iter_mut()
            .find(|head| head.info.crtc == crtc)
        else {
            return Ok(false);
        };
        if !head.presenter.flip_in_flight() {
            return Ok(false);
        }
        head.presenter.complete_flip()?;
        Ok(true)
    }

    pub fn now_us(&self) -> u64 {
        elapsed_us(self.epoch)
    }

    pub fn refresh_period(&self) -> Option<Duration> {
        refresh_period_from_mhz(self.display.heads[0].info.refresh_mhz)
    }

    pub fn framebuffer_mut(&mut self) -> &mut [u8] {
        let phase = self.shared.phase.lock().unwrap();
        assert!(matches!(
            *phase,
            DisplayPhase::Active(_) | DisplayPhase::ChangeQueued(_, _)
        ));
        drop(phase);
        self.display.heads[0].presenter.framebuffer_mut()
    }

    pub fn framebuffer_mut_for(&mut self, output_id: u32) -> io::Result<&mut [u8]> {
        let phase = self.shared.phase.lock().unwrap();
        assert!(matches!(
            *phase,
            DisplayPhase::Active(_) | DisplayPhase::ChangeQueued(_, _)
        ));
        drop(phase);
        let head = self
            .display
            .heads
            .iter_mut()
            .find(|head| head.info.id == output_id)
            .ok_or_else(unknown_output_err)?;
        Ok(head.presenter.framebuffer_mut())
    }

    pub fn present(&mut self) -> io::Result<Presentation> {
        let primary_id = self.display.heads[0].info.id;
        self.present_for(primary_id)
    }

    pub fn present_for(&mut self, output_id: u32) -> io::Result<Presentation> {
        self.check_failure()?;
        {
            let phase = self.shared.phase.lock().unwrap();
            match *phase {
                DisplayPhase::Active(_) => {}
                DisplayPhase::ChangeQueued(_, _)
                | DisplayPhase::ChangePending(_, _)
                | DisplayPhase::Stopped => {
                    return Err(display_change_pending_err());
                }
            }
        }
        let started_us = self.now_us();
        let epoch = self.epoch;
        let head = self
            .display
            .heads
            .iter_mut()
            .find(|head| head.info.id == output_id)
            .ok_or_else(unknown_output_err)?;
        let result = head.presenter.present(started_us, || elapsed_us(epoch));
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
                let prev = std::mem::replace(&mut *phase, DisplayPhase::Stopped);
                match prev {
                    DisplayPhase::ChangeQueued(output, reason) => {
                        *phase = DisplayPhase::ChangePending(Some(output), reason);
                    }
                    _ => unreachable!("DisplayChanged received outside ChangeQueued"),
                }
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
        let primary_id = self.display.heads[0].info.id;
        self.cursor_size_for(primary_id)
    }

    pub fn cursor_size_for(&self, output_id: u32) -> io::Result<(u32, u32)> {
        let phase = self.shared.phase.lock().unwrap();
        match &*phase {
            DisplayPhase::Active(output) | DisplayPhase::ChangeQueued(output, _) => {
                let head = output
                    .heads
                    .iter()
                    .find(|head| head.id == output_id)
                    .ok_or_else(unknown_output_err)?;
                let Some(cursor) = &head.cursor else {
                    return Err(no_cursor_plane_err());
                };
                Ok((cursor.buffer.size().0, cursor.buffer.size().1))
            }
            DisplayPhase::ChangePending(_, _) | DisplayPhase::Stopped => {
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
        let primary_id = self.display.heads[0].info.id;
        self.set_cursor_for(primary_id, pixels, size, hotspot)
    }

    pub fn set_cursor_for(
        &mut self,
        output_id: u32,
        pixels: Option<&[u8]>,
        size: (u32, u32),
        hotspot: (i32, i32),
    ) -> io::Result<()> {
        self.check_failure()?;
        let mut phase = self.shared.phase.lock().unwrap();
        let output = match &mut *phase {
            DisplayPhase::Active(output) => output,
            DisplayPhase::ChangeQueued(_, _)
            | DisplayPhase::ChangePending(_, _)
            | DisplayPhase::Stopped => {
                return Err(display_change_pending_err());
            }
        };
        let active_head = output.active_cursor_head;
        let head = output
            .heads
            .iter_mut()
            .find(|head| head.id == output_id)
            .ok_or_else(unknown_output_err)?;
        let Some(cursor) = head.cursor.as_mut() else {
            return Err(no_cursor_plane_err());
        };
        let Some(pixels) = pixels else {
            cursor.shown = false;
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
        cursor.hotspot = hotspot;
        cursor.shown = true;
        if active_head == Some(output_id) || active_head.is_none() {
            #[allow(deprecated)]
            cursor
                .drm
                .set_cursor2(cursor.crtc, Some(&cursor.buffer), hotspot)?;
            if active_head.is_none() {
                output.active_cursor_head = Some(output_id);
            }
        }
        Ok(())
    }

    pub fn reconfigure(&mut self) -> io::Result<()> {
        self.check_failure()?;
        let mut phase = self.shared.phase.lock().unwrap();
        let (prev_output, reason) = match &mut *phase {
            DisplayPhase::ChangePending(prev_output, reason) => (prev_output.take(), *reason),
            _ => unreachable!("reconfigure requires ChangePending"),
        };
        let sender = self.sender.clone();
        let shared = Arc::clone(&self.shared);
        let epoch = self.epoch;
        let mut on_other_flip = move |flip: FlipCompletion| {
            let _ = send(&sender, &shared, BackendEvent::FlipComplete(flip));
        };
        let (replacement, hotplug_seen) = match reason {
            ChangeReason::Hotplug => {
                self.display
                    .reconfigure_hotplug(prev_output, epoch, &mut on_other_flip)?
            }
            ChangeReason::Handoff => {
                for head in &mut self.display.heads {
                    head.presenter.drain_flip(false, epoch, |_| {});
                }
                drop(prev_output);
                let (display, replacement) = Display::open(&self.input, self.preference)?;
                self.display = display;
                self.display_generation = self.display_generation.wrapping_add(1);
                (replacement, false)
            }
        };
        if hotplug_seen {
            *phase = DisplayPhase::ChangeQueued(replacement, ChangeReason::Hotplug);
            drop(phase);
            send(&self.sender, &self.shared, BackendEvent::DisplayChanged)?;
        } else {
            *phase = DisplayPhase::Active(replacement);
            self.shared.changed.notify_one();
        }
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
        if let DisplayPhase::Active(active) = prev {
            for head in active.heads {
                if let Some(cursor) = head.cursor {
                    #[allow(deprecated)]
                    let _ = cursor
                        .drm
                        .set_cursor2(cursor.crtc, None::<&DumbBuffer>, (0, 0));
                }
            }
        }
    }
}

const MAX_DISPLAY_OPEN_ATTEMPTS: usize = 4;

struct ConnectedCandidate {
    connector: connector::Handle,
    name: String,
    mode: drm::control::Mode,
    dimensions: (u32, u32),
    refresh_mhz: u32,
    mm_size: (u32, u32),
    possible_crtcs: Vec<crtc::Handle>,
}

fn scan_connected(drm: &DrmHandle) -> io::Result<Vec<ConnectedCandidate>> {
    let resources = drm.resource_handles()?;
    let mut candidates = Vec::new();
    let mut overflow_logged = false;
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
        if candidates.len() >= MAX_OUTPUTS {
            if !overflow_logged {
                eprintln!(
                    "ferrion-wm-backend: more than {MAX_OUTPUTS} connected displays; extra connectors stay off"
                );
                overflow_logged = true;
            }
            continue;
        }
        let mut possible_crtcs = Vec::new();
        for &encoder in info.encoders() {
            for crtc in resources.filter_crtcs(drm.get_encoder(encoder)?.possible_crtcs()) {
                if !possible_crtcs.contains(&crtc) {
                    possible_crtcs.push(crtc);
                }
            }
        }
        if possible_crtcs.is_empty() {
            continue;
        }
        let name = format!("{}-{}", info.interface().as_str(), info.interface_id());
        let refresh_mhz = mode_refresh_mhz(&mode);
        let mm_size = info.size().unwrap_or((0, 0));
        candidates.push(ConnectedCandidate {
            connector,
            name,
            mode,
            dimensions,
            refresh_mhz,
            mm_size,
            possible_crtcs,
        });
    }
    assert!(candidates.len() <= MAX_OUTPUTS);
    Ok(candidates)
}

fn pick_crtc(
    drm: &DrmHandle,
    possible: &[crtc::Handle],
    used: &[crtc::Handle],
) -> io::Result<Option<(crtc::Handle, bool)>> {
    let mut fallback = None;
    for &crtc in possible {
        if used.contains(&crtc) {
            continue;
        }
        if has_cursor(drm, crtc)? {
            return Ok(Some((crtc, true)));
        }
        if fallback.is_none() {
            fallback = Some((crtc, false));
        }
    }
    Ok(fallback)
}

fn mode_refresh_mhz(mode: &drm::control::Mode) -> u32 {
    let htotal = u64::from(mode.hsync().2);
    let vtotal = u64::from(mode.vsync().2);
    let clock_khz = u64::from(mode.clock());
    if clock_khz > 0 && htotal > 0 && vtotal > 0 {
        let denom = htotal.saturating_mul(vtotal);
        let mhz = (clock_khz.saturating_mul(1_000_000) + denom / 2) / denom;
        if mhz > 0 && mhz <= u64::from(u32::MAX) {
            return mhz as u32;
        }
    }
    mode.vrefresh().saturating_mul(1000)
}

fn refresh_period_from_mhz(refresh_mhz: u32) -> Option<Duration> {
    if refresh_mhz == 0 {
        return None;
    }
    let nanos = (1_000_000_000_000_u64 + u64::from(refresh_mhz) / 2) / u64::from(refresh_mhz);
    Some(Duration::from_nanos(nanos.max(1)))
}

fn assign_layout(heads: &mut [Head]) -> ((u32, u32), Vec<OutputInfo>) {
    let mut next_x: i32 = 0;
    let mut max_height: u32 = 0;
    let mut infos = Vec::with_capacity(heads.len());
    for head in heads.iter_mut() {
        head.info.x = next_x;
        head.info.y = 0;
        next_x = next_x.saturating_add(head.info.width as i32);
        max_height = max_height.max(head.info.height);
        infos.push(head.info.clone());
    }
    let total_width = next_x.max(0) as u32;
    ((total_width, max_height), infos)
}

fn modes_equal(a: &drm::control::Mode, b: &drm::control::Mode) -> bool {
    a.size() == b.size()
        && a.clock() == b.clock()
        && a.hsync() == b.hsync()
        && a.vsync() == b.vsync()
        && a.vrefresh() == b.vrefresh()
}

impl Display {
    fn open(input: &Consumer, preference: PresentPreference) -> io::Result<(Self, ActiveOutput)> {
        let drm = Arc::new(DrmHandle::from_file(input.open_display()?)?);
        drm.set_client_capability(ClientCapability::UniversalPlanes, true)?;
        let _ = drm.set_client_capability(ClientCapability::CursorPlaneHotspot, true);
        drm_ffi::set_capability(
            drm.as_fd(),
            u64::from(REDOX_DRM_CLIENT_CAP_HOTPLUG_EVENTS),
            true,
        )?;
        for _ in 0..MAX_DISPLAY_OPEN_ATTEMPTS {
            let candidates = scan_connected(&drm)?;
            if candidates.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "no connected DRM output",
                ));
            }
            let mut used_crtcs = Vec::with_capacity(candidates.len());
            let mut chosen_mode: Option<PresentMode> = None;
            let mut heads = Vec::with_capacity(candidates.len());
            let mut cursors: Vec<Option<Cursor>> = Vec::with_capacity(candidates.len());
            let mut hotplug_seen = false;
            for candidate in candidates {
                let Some((crtc, with_cursor)) =
                    pick_crtc(&drm, &candidate.possible_crtcs, &used_crtcs)?
                else {
                    continue;
                };
                let (presenter, mode_choice, seen) = match chosen_mode {
                    None => presenter::open_presenter_probing(
                        Arc::clone(&drm),
                        crtc,
                        candidate.connector,
                        candidate.mode,
                        preference,
                    )?,
                    Some(fixed_mode) => (
                        presenter::open_presenter_with_mode(
                            Arc::clone(&drm),
                            crtc,
                            candidate.connector,
                            candidate.mode,
                            fixed_mode,
                        )?,
                        fixed_mode,
                        false,
                    ),
                };
                chosen_mode = Some(mode_choice);
                if seen {
                    hotplug_seen = true;
                }
                let is_primary = heads.is_empty();
                let cursor = if with_cursor {
                    Some(Cursor::new(Arc::clone(&drm), crtc, is_primary)?)
                } else {
                    None
                };
                used_crtcs.push(crtc);
                let info = OutputInfo {
                    id: u32::from(candidate.connector),
                    crtc: u32::from(crtc),
                    name: candidate.name,
                    width: candidate.dimensions.0,
                    height: candidate.dimensions.1,
                    refresh_mhz: candidate.refresh_mhz,
                    mm_width: candidate.mm_size.0,
                    mm_height: candidate.mm_size.1,
                    x: 0,
                    y: 0,
                    present_mode: mode_choice,
                    has_cursor: cursor.is_some(),
                };
                heads.push(Head {
                    info,
                    connector: candidate.connector,
                    crtc,
                    mode: candidate.mode,
                    presenter,
                });
                cursors.push(cursor);
            }
            if heads.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "no connected DRM output",
                ));
            }
            if hotplug_seen {
                for head in &heads {
                    head.presenter.disable_crtc();
                }
                drop(cursors);
                drop(heads);
                continue;
            }
            let present_mode = chosen_mode.unwrap_or(PresentMode::DirtyFb);
            let (bounds, output_infos) = assign_layout(&mut heads);
            let active_cursor_head = heads
                .iter()
                .zip(cursors.iter())
                .find_map(|(head, cursor)| cursor.as_ref().map(|_| head.info.id));
            let active_heads = heads
                .iter()
                .zip(cursors)
                .map(|(head, cursor)| ActiveHead {
                    id: head.info.id,
                    rect: OutputRect {
                        x: head.info.x,
                        y: head.info.y,
                        width: head.info.width,
                        height: head.info.height,
                    },
                    cursor,
                })
                .collect();
            return Ok((
                Self {
                    drm: Arc::clone(&drm),
                    present_mode,
                    heads,
                    output_infos,
                },
                ActiveOutput {
                    drm,
                    bounds,
                    heads: active_heads,
                    active_cursor_head,
                },
            ));
        }
        Err(io::Error::new(
            io::ErrorKind::Other,
            "display changed repeatedly during startup probe",
        ))
    }

    fn reconfigure_hotplug(
        &mut self,
        prev_output: Option<ActiveOutput>,
        epoch: Instant,
        on_other_flip: &mut impl FnMut(FlipCompletion),
    ) -> io::Result<(ActiveOutput, bool)> {
        let candidates = scan_connected(&self.drm)?;
        if candidates.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no connected DRM output",
            ));
        }
        let prev_active_cursor = prev_output.as_ref().and_then(|o| o.active_cursor_head);
        let mut old_cursors: HashMap<u32, Option<Cursor>> = prev_output
            .map(|o| {
                o.heads
                    .into_iter()
                    .map(|head| (head.id, head.cursor))
                    .collect()
            })
            .unwrap_or_default();
        let mut old_heads: HashMap<connector::Handle, Head> = self
            .heads
            .drain(..)
            .map(|head| (head.connector, head))
            .collect();

        let candidate_ids: Vec<connector::Handle> =
            candidates.iter().map(|c| c.connector).collect();
        let mut removed = Vec::new();
        let mut hotplug_seen = false;
        old_heads.retain(|connector, head| {
            if candidate_ids.contains(connector) {
                true
            } else {
                removed.push(head.info.id);
                let outcome = head
                    .presenter
                    .drain_flip(true, epoch, |flip| on_other_flip(flip));
                hotplug_seen |= outcome.hotplug_seen;
                head.presenter.disable_crtc();
                false
            }
        });
        for removed_id in removed {
            if let Some(Some(cursor)) = old_cursors.remove(&removed_id) {
                #[allow(deprecated)]
                let _ = cursor
                    .drm
                    .set_cursor2(cursor.crtc, None::<&DumbBuffer>, (0, 0));
            }
        }

        let mut used_crtcs: Vec<crtc::Handle> = old_heads.values().map(|h| h.crtc).collect();
        let mut new_heads = Vec::with_capacity(candidates.len());
        let mut new_cursors: Vec<Option<Cursor>> = Vec::with_capacity(candidates.len());

        for candidate in candidates {
            if let Some(mut existing) = old_heads.remove(&candidate.connector) {
                let old_id = existing.info.id;
                if modes_equal(&existing.mode, &candidate.mode) {
                    existing.info.name = candidate.name;
                    existing.info.mm_width = candidate.mm_size.0;
                    existing.info.mm_height = candidate.mm_size.1;
                    new_heads.push(existing);
                    new_cursors.push(old_cursors.remove(&old_id).flatten());
                    continue;
                }
                let outcome = existing
                    .presenter
                    .drain_flip(true, epoch, |flip| on_other_flip(flip));
                hotplug_seen |= outcome.hotplug_seen;
                let presenter = match presenter::open_presenter_with_mode(
                    Arc::clone(&self.drm),
                    existing.crtc,
                    candidate.connector,
                    candidate.mode,
                    self.present_mode,
                ) {
                    Ok(presenter) => presenter,
                    Err(error) => {
                        existing.presenter.disable_crtc();
                        return Err(error);
                    }
                };
                let existing_cursor = old_cursors.remove(&old_id).flatten();
                let info = OutputInfo {
                    id: u32::from(candidate.connector),
                    crtc: u32::from(existing.crtc),
                    name: candidate.name,
                    width: candidate.dimensions.0,
                    height: candidate.dimensions.1,
                    refresh_mhz: candidate.refresh_mhz,
                    mm_width: candidate.mm_size.0,
                    mm_height: candidate.mm_size.1,
                    x: 0,
                    y: 0,
                    present_mode: self.present_mode,
                    has_cursor: existing_cursor.is_some(),
                };
                new_heads.push(Head {
                    info,
                    connector: candidate.connector,
                    crtc: existing.crtc,
                    mode: candidate.mode,
                    presenter,
                });
                new_cursors.push(existing_cursor);
                continue;
            }
            let Some((crtc, with_cursor)) =
                pick_crtc(&self.drm, &candidate.possible_crtcs, &used_crtcs)?
            else {
                continue;
            };
            let presenter = presenter::open_presenter_with_mode(
                Arc::clone(&self.drm),
                crtc,
                candidate.connector,
                candidate.mode,
                self.present_mode,
            )?;
            let cursor = if with_cursor {
                Some(Cursor::new(Arc::clone(&self.drm), crtc, false)?)
            } else {
                None
            };
            used_crtcs.push(crtc);
            let info = OutputInfo {
                id: u32::from(candidate.connector),
                crtc: u32::from(crtc),
                name: candidate.name,
                width: candidate.dimensions.0,
                height: candidate.dimensions.1,
                refresh_mhz: candidate.refresh_mhz,
                mm_width: candidate.mm_size.0,
                mm_height: candidate.mm_size.1,
                x: 0,
                y: 0,
                present_mode: self.present_mode,
                has_cursor: cursor.is_some(),
            };
            new_heads.push(Head {
                info,
                connector: candidate.connector,
                crtc,
                mode: candidate.mode,
                presenter,
            });
            new_cursors.push(cursor);
        }

        if new_heads.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no connected DRM output",
            ));
        }

        let (bounds, output_infos) = assign_layout(&mut new_heads);
        let active_cursor_head = prev_active_cursor
            .filter(|id| {
                new_heads
                    .iter()
                    .zip(new_cursors.iter())
                    .any(|(head, cursor)| head.info.id == *id && cursor.is_some())
            })
            .or_else(|| {
                new_heads
                    .iter()
                    .zip(new_cursors.iter())
                    .find_map(|(head, cursor)| cursor.as_ref().map(|_| head.info.id))
            });
        let active_heads = new_heads
            .iter()
            .zip(new_cursors)
            .map(|(head, cursor)| ActiveHead {
                id: head.info.id,
                rect: OutputRect {
                    x: head.info.x,
                    y: head.info.y,
                    width: head.info.width,
                    height: head.info.height,
                },
                cursor,
            })
            .collect();
        self.heads = new_heads;
        self.output_infos = output_infos;
        Ok((
            ActiveOutput {
                drm: Arc::clone(&self.drm),
                bounds,
                heads: active_heads,
                active_cursor_head,
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
    fn new(drm: Arc<DrmHandle>, crtc: crtc::Handle, show_initial: bool) -> io::Result<Self> {
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
            hotspot: (0, 0),
            shown: show_initial,
        };
        let pitch = cursor.buffer.pitch() as usize;
        {
            let mut mapping = cursor.drm.map_dumb_buffer(&mut cursor.buffer)?;
            mapping.fill(0);
            for y in 0..24_usize {
                for x in 0..=(y / 2).min(15) {
                    let edge = x == 0 || x == y / 2 || y == 23;
                    let pixel = if edge { 0xff101010_u32 } else { 0xffeeeeee_u32 };
                    let offset = y * pitch + x * 4;
                    mapping[offset..offset + 4].copy_from_slice(&pixel.to_ne_bytes());
                }
            }
        }
        if show_initial {
            #[allow(deprecated)]
            cursor.drm.set_cursor2(crtc, Some(&cursor.buffer), (0, 0))?;
            #[allow(deprecated)]
            cursor.drm.move_cursor(crtc, (0, 0))?;
        }
        Ok(cursor)
    }
}

impl Drop for Cursor {
    fn drop(&mut self) {
        let _ = self.drm.destroy_dumb_buffer(self.buffer);
    }
}

fn queue_display_change(shared: &Shared, reason: ChangeReason) -> bool {
    let mut phase = shared.phase.lock().unwrap();
    let prev = std::mem::replace(&mut *phase, DisplayPhase::Stopped);
    match prev {
        DisplayPhase::Active(output) => {
            *phase = DisplayPhase::ChangeQueued(output, reason);
            true
        }
        DisplayPhase::Stopped => false,
        DisplayPhase::ChangeQueued(_, _) | DisplayPhase::ChangePending(_, _) => {
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
                    DisplayPhase::ChangeQueued(_, _) | DisplayPhase::ChangePending(_, _) => {
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
            match drm.redox_receive_events() {
                Ok(drm_events) => {
                    for event in drm_events {
                        match event {
                            RedoxDrmEvent::RedoxConnectorHotplug(_) => {
                                changed = true;
                            }
                            RedoxDrmEvent::PageFlip(flip) => {
                                let completion = FlipCompletion {
                                    crtc: u32::from(flip.crtc),
                                    completed_us: elapsed_us(epoch),
                                    sequence: flip.frame,
                                    tv_us: flip.duration.as_micros().min(u128::from(u64::MAX))
                                        as u64,
                                };
                                send(&sender, shared, BackendEvent::FlipComplete(completion))?;
                            }
                            _ => {}
                        }
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
            if changed {
                if !queue_display_change(shared, ChangeReason::Hotplug) {
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
                    if !queue_display_change(shared, ChangeReason::Handoff) {
                        return Ok(());
                    }
                    send(&sender, shared, BackendEvent::DisplayChanged)?;
                    break;
                }
                ConsumerEvent::Events(&[]) => break,
                ConsumerEvent::Events(events) => {
                    for wire in events {
                        let mut phase = shared.phase.lock().unwrap();
                        let output = match &mut *phase {
                            DisplayPhase::Active(output) => output,
                            DisplayPhase::Stopped => return Ok(()),
                            DisplayPhase::ChangeQueued(_, _)
                            | DisplayPhase::ChangePending(_, _) => {
                                unreachable!("reader converted input outside Active phase")
                            }
                        };
                        let rects = output.rects();
                        let rect_slice = &rects[..output.head_count()];
                        let bounds = output.bounds;
                        for event in state
                            .convert(wire.code, wire.a, wire.b, read_us, bounds, rect_slice)
                            .into_iter()
                            .flatten()
                        {
                            if let Input::Motion(motion) = event {
                                if let Some(target_idx) = output
                                    .heads
                                    .iter()
                                    .position(|h| h.rect.contains(motion.x, motion.y))
                                {
                                    let target_id = output.heads[target_idx].id;
                                    let prev_id = output.active_cursor_head;
                                    if prev_id != Some(target_id) {
                                        if let Some(prev_head_id) = prev_id {
                                            if let Some(prev_head) =
                                                output.heads.iter().find(|h| h.id == prev_head_id)
                                            {
                                                if let Some(prev_cursor) = &prev_head.cursor {
                                                    #[allow(deprecated)]
                                                    let _ = prev_cursor.drm.set_cursor2(
                                                        prev_cursor.crtc,
                                                        None::<&DumbBuffer>,
                                                        (0, 0),
                                                    );
                                                }
                                            }
                                        }
                                        output.active_cursor_head = Some(target_id);
                                        if let Some(cursor) = &output.heads[target_idx].cursor {
                                            if cursor.shown {
                                                #[allow(deprecated)]
                                                cursor.drm.set_cursor2(
                                                    cursor.crtc,
                                                    Some(&cursor.buffer),
                                                    cursor.hotspot,
                                                )?;
                                            }
                                        }
                                    }
                                    let target = &output.heads[target_idx];
                                    if let Some(cursor) = &target.cursor {
                                        let local_x = motion.x - target.rect.x;
                                        let local_y = motion.y - target.rect.y;
                                        #[allow(deprecated)]
                                        cursor.drm.move_cursor(cursor.crtc, (local_x, local_y))?;
                                        let timing = CursorTiming {
                                            read_us,
                                            completed_us: elapsed_us(epoch),
                                            x: motion.x,
                                            y: motion.y,
                                        };
                                        send(&sender, shared, BackendEvent::CursorTiming(timing))?;
                                    }
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
    Ok(unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) })
}

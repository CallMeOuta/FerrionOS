use std::io;
use std::num::NonZeroU32;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::sync::Arc;
use std::time::Instant;

use drm::buffer::{Buffer as _, DrmFourcc};
use drm::control::{ClipRect, Device as _, Mode, PageFlipFlags, connector, crtc, framebuffer};
use graphics_ipc::{CpuBackedBuffer, DrmHandle, RedoxDrmEvent};

use crate::{
    DmabufFb, DmabufFbDesc, FlipCompletion, MAX_DMABUF_FBS_PER_HEAD, MAX_IN_FLIGHT_FLIPS,
    PresentMode, PresentPreference, Presentation, SWAP_BUFFERS,
};

const DRM_FORMAT_MOD_LINEAR: u64 = 0;

const FLIP_PROBE_TIMEOUT_MS: i32 = 2_000;
const MAX_PROBE_POLL_ROUNDS: usize = 16;
const DRAIN_POLL_TIMEOUT_MS: i32 = 100;
const MAX_DRAIN_POLL_ROUNDS: usize = 16;

pub(crate) struct DrainOutcome {
    pub(crate) hotplug_seen: bool,
}

struct SwapSlot {
    framebuffer: framebuffer::Handle,
    buffer: Option<CpuBackedBuffer>,
}

impl SwapSlot {
    fn create(drm: &DrmHandle, dimensions: (u32, u32)) -> io::Result<Self> {
        let mut buffer = CpuBackedBuffer::new(drm, dimensions, DrmFourcc::Argb8888, 32)?;
        buffer.shadow_buf().fill(0);
        buffer.sync_rect(0, 0, dimensions.0, dimensions.1);
        let framebuffer = match drm.add_framebuffer(buffer.buffer(), 32, 32) {
            Ok(fb) => fb,
            Err(error) => {
                let _ = buffer.destroy(drm);
                return Err(error);
            }
        };
        Ok(Self {
            framebuffer,
            buffer: Some(buffer),
        })
    }

    fn stride(&self) -> u32 {
        self.buffer.as_ref().unwrap().buffer().pitch()
    }

    fn shadow_buf(&mut self) -> &mut [u8] {
        self.buffer.as_mut().unwrap().shadow_buf()
    }

    fn sync_full(&mut self, dimensions: (u32, u32)) {
        self.buffer
            .as_mut()
            .unwrap()
            .sync_rect(0, 0, dimensions.0, dimensions.1);
    }

    fn destroy(&mut self, drm: &DrmHandle) {
        let _ = drm.destroy_framebuffer(self.framebuffer);
        if let Some(buffer) = self.buffer.take() {
            let _ = buffer.destroy(drm);
        }
    }
}

enum PresenterKind {
    Cpu {
        slot: SwapSlot,
    },
    Flip {
        slots: [SwapSlot; SWAP_BUFFERS],
        back: usize,
        in_flight: usize,
    },
}

#[derive(Clone, Copy)]
struct DmabufEntry {
    id: DmabufFb,
    framebuffer: framebuffer::Handle,
    gem_handle: u32,
}

pub(crate) struct Presenter {
    drm: Arc<DrmHandle>,
    crtc: crtc::Handle,
    connector: connector::Handle,
    mode: Mode,
    dimensions: (u32, u32),
    kind: PresenterKind,
    dmabuf_fbs: [Option<DmabufEntry>; MAX_DMABUF_FBS_PER_HEAD],
    active_dmabuf_fb: Option<DmabufFb>,
    crtc_fb_active: bool,
}

impl Presenter {
    pub(crate) fn stride(&self) -> u32 {
        match &self.kind {
            PresenterKind::Cpu { slot } => slot.stride(),
            PresenterKind::Flip { slots, back, .. } => {
                assert!(*back < SWAP_BUFFERS);
                slots[*back].stride()
            }
        }
    }

    pub(crate) fn present_mode(&self) -> PresentMode {
        match self.kind {
            PresenterKind::Cpu { .. } => PresentMode::DirtyFb,
            PresenterKind::Flip { .. } => PresentMode::PageFlip,
        }
    }

    pub(crate) fn flip_in_flight(&self) -> bool {
        match self.kind {
            PresenterKind::Cpu { .. } => false,
            PresenterKind::Flip { in_flight, .. } => {
                assert!(in_flight <= MAX_IN_FLIGHT_FLIPS);
                in_flight > 0
            }
        }
    }

    pub(crate) fn framebuffer_mut(&mut self) -> &mut [u8] {
        match &mut self.kind {
            PresenterKind::Cpu { slot } => slot.shadow_buf(),
            PresenterKind::Flip { slots, back, .. } => {
                assert!(*back < SWAP_BUFFERS);
                slots[*back].shadow_buf()
            }
        }
    }

    pub(crate) fn present(
        &mut self,
        started_us: u64,
        now_us: impl FnOnce() -> u64,
    ) -> io::Result<Presentation> {
        let (width, height) = self.dimensions;
        let crtc = self.crtc;
        let connector = self.connector;
        let mode = self.mode;
        let crtc_fb_active = &mut self.crtc_fb_active;
        let active_dmabuf_fb = &mut self.active_dmabuf_fb;
        match &mut self.kind {
            PresenterKind::Cpu { slot } => {
                assert!(*crtc_fb_active);
                slot.sync_full((width, height));
                self.drm.dirty_framebuffer(
                    slot.framebuffer,
                    &[ClipRect::new(0, 0, width as u16, height as u16)],
                )?;
                Ok(Presentation {
                    started_us,
                    completed_us: now_us(),
                    mode: PresentMode::DirtyFb,
                    pending: false,
                })
            }
            PresenterKind::Flip {
                slots,
                back,
                in_flight,
            } => {
                assert!(*back < SWAP_BUFFERS);
                assert!(*in_flight <= MAX_IN_FLIGHT_FLIPS);
                if *in_flight >= MAX_IN_FLIGHT_FLIPS {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "page flip already in flight",
                    ));
                }
                let slot = &mut slots[*back];
                slot.sync_full((width, height));
                let fb = slot.framebuffer;
                if !*crtc_fb_active {
                    self.drm
                        .set_crtc(crtc, Some(fb), (0, 0), &[connector], Some(mode))?;
                    *crtc_fb_active = true;
                    *active_dmabuf_fb = None;
                }
                if let Err(error) = self.drm.page_flip(crtc, fb, PageFlipFlags::EVENT, None) {
                    if error.raw_os_error() == Some(libc::EBUSY) {
                        return Err(io::Error::new(io::ErrorKind::WouldBlock, "page flip busy"));
                    }
                    return Err(error);
                }
                *active_dmabuf_fb = None;
                *back = (*back + 1) % SWAP_BUFFERS;
                *in_flight += 1;
                assert!(*in_flight <= MAX_IN_FLIGHT_FLIPS);
                Ok(Presentation {
                    started_us,
                    completed_us: started_us,
                    mode: PresentMode::PageFlip,
                    pending: true,
                })
            }
        }
    }

    pub(crate) fn complete_flip(&mut self) -> io::Result<()> {
        match &mut self.kind {
            PresenterKind::Cpu { .. } => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "page flip completion received in dirtyfb mode",
            )),
            PresenterKind::Flip { in_flight, .. } => {
                assert!(*in_flight <= MAX_IN_FLIGHT_FLIPS);
                if *in_flight == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "unexpected page flip completion",
                    ));
                }
                *in_flight -= 1;
                Ok(())
            }
        }
    }

    pub(crate) fn drain_flip(
        &mut self,
        blocking: bool,
        epoch: Instant,
        mut on_other_flip: impl FnMut(FlipCompletion),
    ) -> DrainOutcome {
        let PresenterKind::Flip { in_flight, .. } = &mut self.kind else {
            return DrainOutcome {
                hotplug_seen: false,
            };
        };
        assert!(*in_flight <= MAX_IN_FLIGHT_FLIPS);
        if *in_flight == 0 {
            return DrainOutcome {
                hotplug_seen: false,
            };
        }
        let max_rounds = if blocking { MAX_DRAIN_POLL_ROUNDS } else { 1 };
        let timeout_ms = if blocking { DRAIN_POLL_TIMEOUT_MS } else { 0 };
        let mut hotplug_seen = false;
        for _ in 0..max_rounds {
            let mut pfd = libc::pollfd {
                fd: self.drm.as_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                eprintln!(
                    "ferrion-wm-backend: drain poll failed on crtc {}: {error}",
                    u32::from(self.crtc)
                );
                *in_flight = 0;
                return DrainOutcome { hotplug_seen };
            }
            if ready == 0 {
                continue;
            }
            if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                *in_flight = 0;
                return DrainOutcome { hotplug_seen };
            }
            if pfd.revents & libc::POLLIN != 0 {
                let events = match self.drm.redox_receive_events() {
                    Ok(events) => events,
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) =>
                    {
                        continue;
                    }
                    Err(error) => {
                        eprintln!(
                            "ferrion-wm-backend: drain receive failed on crtc {}: {error}",
                            u32::from(self.crtc)
                        );
                        *in_flight = 0;
                        return DrainOutcome { hotplug_seen };
                    }
                };
                for event in events {
                    match event {
                        RedoxDrmEvent::PageFlip(flip) => {
                            if flip.crtc == self.crtc {
                                *in_flight = in_flight.saturating_sub(1);
                            } else {
                                on_other_flip(FlipCompletion {
                                    crtc: u32::from(flip.crtc),
                                    completed_us: epoch
                                        .elapsed()
                                        .as_micros()
                                        .min(u128::from(u64::MAX))
                                        as u64,
                                    sequence: flip.frame,
                                    tv_us: flip.duration.as_micros().min(u128::from(u64::MAX))
                                        as u64,
                                });
                            }
                        }
                        RedoxDrmEvent::RedoxConnectorHotplug(_) => {
                            hotplug_seen = true;
                        }
                        _ => {}
                    }
                }
                if *in_flight == 0 {
                    return DrainOutcome { hotplug_seen };
                }
            }
        }
        if blocking {
            eprintln!(
                "ferrion-wm-backend: timed out draining page flip on crtc {}",
                u32::from(self.crtc)
            );
        }
        *in_flight = 0;
        DrainOutcome { hotplug_seen }
    }

    pub(crate) fn import_dmabuf_fb(
        &mut self,
        dmabuf_fd: BorrowedFd<'_>,
        desc: DmabufFbDesc,
    ) -> io::Result<DmabufFb> {
        if !matches!(self.kind, PresenterKind::Flip { .. }) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "dma-buf scanout requires page-flip presenter",
            ));
        }
        let min_stride = desc.width.checked_mul(4).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "dma-buf width overflows stride",
            )
        })?;
        if desc.width != self.dimensions.0
            || desc.height != self.dimensions.1
            || desc.stride < min_stride
            || desc.modifier != DRM_FORMAT_MOD_LINEAR
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "dma-buf geometry or modifier does not match output",
            ));
        }
        let Some(slot_index) = self.dmabuf_fbs.iter().position(Option::is_none) else {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "per-head dma-buf framebuffer limit reached",
            ));
        };
        let prime = drm_ffi::gem::fd_to_handle(self.drm.as_fd(), dmabuf_fd)?;
        let gem_handle = prime.handle;
        if gem_handle == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "PRIME import returned zero GEM handle",
            ));
        }
        let fb_cmd = match drm_ffi::mode::add_fb2(
            self.drm.as_fd(),
            desc.width,
            desc.height,
            desc.fourcc,
            &[gem_handle, 0, 0, 0],
            &[desc.stride, 0, 0, 0],
            &[desc.offset, 0, 0, 0],
            &[desc.modifier, 0, 0, 0],
            drm_ffi::DRM_MODE_FB_MODIFIERS,
        ) {
            Ok(cmd) => cmd,
            Err(error) => {
                let _ = drm_ffi::gem::close(self.drm.as_fd(), gem_handle);
                return Err(error);
            }
        };
        let Some(fb_nz) = NonZeroU32::new(fb_cmd.fb_id) else {
            let _ = drm_ffi::gem::close(self.drm.as_fd(), gem_handle);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ADDFB2 returned zero framebuffer id",
            ));
        };
        let id = DmabufFb::new(fb_cmd.fb_id);
        self.dmabuf_fbs[slot_index] = Some(DmabufEntry {
            id,
            framebuffer: framebuffer::Handle::from(fb_nz),
            gem_handle,
        });
        assert!(self.dmabuf_fbs.iter().flatten().count() <= MAX_DMABUF_FBS_PER_HEAD);
        Ok(id)
    }

    pub(crate) fn present_dmabuf(
        &mut self,
        fb: DmabufFb,
        started_us: u64,
    ) -> io::Result<Presentation> {
        let Some(entry) = self
            .dmabuf_fbs
            .iter()
            .flatten()
            .find(|entry| entry.id == fb)
            .copied()
        else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "unknown dma-buf framebuffer",
            ));
        };
        let crtc = self.crtc;
        let connector = self.connector;
        let mode = self.mode;
        let crtc_fb_active = &mut self.crtc_fb_active;
        let active_dmabuf_fb = &mut self.active_dmabuf_fb;
        match &mut self.kind {
            PresenterKind::Cpu { .. } => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "dma-buf scanout requires page-flip presenter",
            )),
            PresenterKind::Flip { in_flight, .. } => {
                assert!(*in_flight <= MAX_IN_FLIGHT_FLIPS);
                if *in_flight >= MAX_IN_FLIGHT_FLIPS {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "page flip already in flight",
                    ));
                }
                if !*crtc_fb_active {
                    self.drm.set_crtc(
                        crtc,
                        Some(entry.framebuffer),
                        (0, 0),
                        &[connector],
                        Some(mode),
                    )?;
                    *crtc_fb_active = true;
                    *active_dmabuf_fb = Some(entry.id);
                }
                if let Err(error) =
                    self.drm
                        .page_flip(crtc, entry.framebuffer, PageFlipFlags::EVENT, None)
                {
                    if error.raw_os_error() == Some(libc::EBUSY) {
                        return Err(io::Error::new(io::ErrorKind::WouldBlock, "page flip busy"));
                    }
                    return Err(error);
                }
                *active_dmabuf_fb = Some(entry.id);
                *in_flight += 1;
                assert!(*in_flight <= MAX_IN_FLIGHT_FLIPS);
                Ok(Presentation {
                    started_us,
                    completed_us: started_us,
                    mode: PresentMode::PageFlip,
                    pending: true,
                })
            }
        }
    }

    pub(crate) fn destroy_dmabuf_fb(&mut self, fb: DmabufFb) -> io::Result<()> {
        let Some(slot) = self
            .dmabuf_fbs
            .iter_mut()
            .find(|slot| slot.is_some_and(|entry| entry.id == fb))
        else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "unknown dma-buf framebuffer",
            ));
        };
        let entry = slot.take().unwrap();
        if self.active_dmabuf_fb == Some(entry.id) {
            self.active_dmabuf_fb = None;
            self.crtc_fb_active = false;
        }
        let _ = self.drm.destroy_framebuffer(entry.framebuffer);
        let _ = drm_ffi::gem::close(self.drm.as_fd(), entry.gem_handle);
        Ok(())
    }

    pub(crate) fn destroy_all_dmabuf_fbs(&mut self) {
        for slot in &mut self.dmabuf_fbs {
            if let Some(entry) = slot.take() {
                if self.active_dmabuf_fb == Some(entry.id) {
                    self.active_dmabuf_fb = None;
                    self.crtc_fb_active = false;
                }
                let _ = self.drm.destroy_framebuffer(entry.framebuffer);
                let _ = drm_ffi::gem::close(self.drm.as_fd(), entry.gem_handle);
            }
        }
    }

    pub(crate) fn disable_crtc(&self) {
        let _ = self.drm.set_crtc(self.crtc, None, (0, 0), &[], None);
    }
}

impl Drop for Presenter {
    fn drop(&mut self) {
        self.destroy_all_dmabuf_fbs();
        match &mut self.kind {
            PresenterKind::Cpu { slot } => slot.destroy(&self.drm),
            PresenterKind::Flip { slots, .. } => {
                for slot in slots {
                    slot.destroy(&self.drm);
                }
            }
        }
    }
}

pub(crate) fn open_presenter_probing(
    drm: Arc<DrmHandle>,
    crtc: crtc::Handle,
    connector: connector::Handle,
    mode: Mode,
    preference: PresentPreference,
) -> io::Result<(Presenter, PresentMode, bool)> {
    let dimensions = (u32::from(mode.size().0), u32::from(mode.size().1));
    assert!(dimensions.0 > 0 && dimensions.1 > 0);
    let mut slot0 = SwapSlot::create(&drm, dimensions)?;
    if let Err(error) = drm.set_crtc(
        crtc,
        Some(slot0.framebuffer),
        (0, 0),
        &[connector],
        Some(mode),
    ) {
        slot0.destroy(&drm);
        return Err(error);
    }
    if preference == PresentPreference::DirtyFb {
        return Ok((
            Presenter {
                drm,
                crtc,
                connector,
                mode,
                dimensions,
                kind: PresenterKind::Cpu { slot: slot0 },
                dmabuf_fbs: [None; MAX_DMABUF_FBS_PER_HEAD],
                active_dmabuf_fb: None,
                crtc_fb_active: true,
            },
            PresentMode::DirtyFb,
            false,
        ));
    }

    let mut slot1 = match SwapSlot::create(&drm, dimensions) {
        Ok(slot) => slot,
        Err(error) => {
            slot0.destroy(&drm);
            return Err(error);
        }
    };
    match drm.page_flip(crtc, slot1.framebuffer, PageFlipFlags::EVENT, None) {
        Ok(()) => {
            let hotplug_seen = match wait_probe_flip(&drm, crtc) {
                Ok(hotplug) => hotplug,
                Err(error) => {
                    let _ = drm.set_crtc(crtc, None, (0, 0), &[], None);
                    slot1.destroy(&drm);
                    slot0.destroy(&drm);
                    return Err(error);
                }
            };
            Ok((
                Presenter {
                    drm,
                    crtc,
                    connector,
                    mode,
                    dimensions,
                    kind: PresenterKind::Flip {
                        slots: [slot0, slot1],
                        back: 0,
                        in_flight: 0,
                    },
                    dmabuf_fbs: [None; MAX_DMABUF_FBS_PER_HEAD],
                    active_dmabuf_fb: None,
                    crtc_fb_active: true,
                },
                PresentMode::PageFlip,
                hotplug_seen,
            ))
        }
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(libc::EINVAL | libc::EOPNOTSUPP | libc::ENOSYS)
            ) =>
        {
            slot1.destroy(&drm);
            Ok((
                Presenter {
                    drm,
                    crtc,
                    connector,
                    mode,
                    dimensions,
                    kind: PresenterKind::Cpu { slot: slot0 },
                    dmabuf_fbs: [None; MAX_DMABUF_FBS_PER_HEAD],
                    active_dmabuf_fb: None,
                    crtc_fb_active: true,
                },
                PresentMode::DirtyFb,
                false,
            ))
        }
        Err(error) => {
            let _ = drm.set_crtc(crtc, None, (0, 0), &[], None);
            slot1.destroy(&drm);
            slot0.destroy(&drm);
            Err(error)
        }
    }
}

pub(crate) fn open_presenter_with_mode(
    drm: Arc<DrmHandle>,
    crtc: crtc::Handle,
    connector: connector::Handle,
    mode: Mode,
    chosen_mode: PresentMode,
) -> io::Result<Presenter> {
    let dimensions = (u32::from(mode.size().0), u32::from(mode.size().1));
    assert!(dimensions.0 > 0 && dimensions.1 > 0);
    let mut slot0 = SwapSlot::create(&drm, dimensions)?;
    if let Err(error) = drm.set_crtc(
        crtc,
        Some(slot0.framebuffer),
        (0, 0),
        &[connector],
        Some(mode),
    ) {
        slot0.destroy(&drm);
        return Err(error);
    }
    match chosen_mode {
        PresentMode::DirtyFb => Ok(Presenter {
            drm,
            crtc,
            connector,
            mode,
            dimensions,
            kind: PresenterKind::Cpu { slot: slot0 },
            dmabuf_fbs: [None; MAX_DMABUF_FBS_PER_HEAD],
            active_dmabuf_fb: None,
            crtc_fb_active: true,
        }),
        PresentMode::PageFlip => {
            let slot1 = match SwapSlot::create(&drm, dimensions) {
                Ok(slot) => slot,
                Err(error) => {
                    let _ = drm.set_crtc(crtc, None, (0, 0), &[], None);
                    slot0.destroy(&drm);
                    return Err(error);
                }
            };
            Ok(Presenter {
                drm,
                crtc,
                connector,
                mode,
                dimensions,
                kind: PresenterKind::Flip {
                    slots: [slot0, slot1],
                    back: 1,
                    in_flight: 0,
                },
                dmabuf_fbs: [None; MAX_DMABUF_FBS_PER_HEAD],
                active_dmabuf_fb: None,
                crtc_fb_active: true,
            })
        }
    }
}

fn wait_probe_flip(drm: &DrmHandle, crtc: crtc::Handle) -> io::Result<bool> {
    let mut hotplug_seen = false;
    for _ in 0..MAX_PROBE_POLL_ROUNDS {
        let mut pfd = libc::pollfd {
            fd: drm.as_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut pfd, 1, FLIP_PROBE_TIMEOUT_MS) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "probe page flip timed out",
            ));
        }
        if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "display descriptor closed during probe page flip",
            ));
        }
        if pfd.revents & libc::POLLIN != 0 {
            let events = match drm.redox_receive_events() {
                Ok(events) => events,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            let mut probe_completed = false;
            for event in events {
                match event {
                    RedoxDrmEvent::PageFlip(flip) if flip.crtc == crtc => {
                        probe_completed = true;
                    }
                    RedoxDrmEvent::RedoxConnectorHotplug(_) => hotplug_seen = true,
                    _ => {}
                }
            }
            if probe_completed {
                return Ok(hotplug_seen);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "probe page flip did not complete within poll limit",
    ))
}

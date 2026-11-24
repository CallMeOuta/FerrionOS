use std::io;
use std::os::fd::{AsFd, AsRawFd};
use std::sync::Arc;

use drm::buffer::{Buffer as _, DrmFourcc};
use drm::control::{ClipRect, Device as _, Mode, PageFlipFlags, connector, crtc, framebuffer};
use graphics_ipc::{CpuBackedBuffer, DrmHandle, RedoxDrmEvent};

use crate::{MAX_IN_FLIGHT_FLIPS, PresentMode, PresentPreference, Presentation, SWAP_BUFFERS};

const FLIP_PROBE_TIMEOUT_MS: i32 = 2_000;
const MAX_PROBE_POLL_ROUNDS: usize = 16;
const MAX_PROBE_EVENTS: usize = 64;

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
        crtc: crtc::Handle,
        slots: [SwapSlot; SWAP_BUFFERS],
        back: usize,
        in_flight: usize,
    },
}

pub(crate) struct Presenter {
    drm: Arc<DrmHandle>,
    dimensions: (u32, u32),
    kind: PresenterKind,
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
        match &mut self.kind {
            PresenterKind::Cpu { slot } => {
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
                crtc,
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
                if let Err(error) = self.drm.page_flip(*crtc, fb, PageFlipFlags::EVENT, None) {
                    if error.raw_os_error() == Some(libc::EBUSY) {
                        return Err(io::Error::new(io::ErrorKind::WouldBlock, "page flip busy"));
                    }
                    return Err(error);
                }
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
}

impl Drop for Presenter {
    fn drop(&mut self) {
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

pub(crate) fn open_presenter(
    drm: Arc<DrmHandle>,
    crtc: crtc::Handle,
    connector: connector::Handle,
    mode: Mode,
    preference: PresentPreference,
) -> io::Result<(Presenter, bool)> {
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
                dimensions,
                kind: PresenterKind::Cpu { slot: slot0 },
            },
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
            let hotplug_seen = match wait_probe_flip(&drm) {
                Ok(hotplug) => hotplug,
                Err(error) => {
                    slot1.destroy(&drm);
                    slot0.destroy(&drm);
                    return Err(error);
                }
            };
            Ok((
                Presenter {
                    drm,
                    dimensions,
                    kind: PresenterKind::Flip {
                        crtc,
                        slots: [slot0, slot1],
                        back: 0,
                        in_flight: 0,
                    },
                },
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
                    dimensions,
                    kind: PresenterKind::Cpu { slot: slot0 },
                },
                false,
            ))
        }
        Err(error) => {
            slot1.destroy(&drm);
            slot0.destroy(&drm);
            Err(error)
        }
    }
}

fn wait_probe_flip(drm: &DrmHandle) -> io::Result<bool> {
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
            for (count, event) in drm.redox_receive_events()?.enumerate() {
                if count >= MAX_PROBE_EVENTS {
                    return Err(io::Error::other("probe page flip event limit exceeded"));
                }
                match event {
                    RedoxDrmEvent::PageFlip(_) => return Ok(hotplug_seen),
                    RedoxDrmEvent::RedoxConnectorHotplug(_) => hotplug_seen = true,
                    _ => {}
                }
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "probe page flip did not complete within poll limit",
    ))
}

// SPDX-License-Identifier: MIT
//! CPU DRM presentation and inputd input for FerrionOS.
//!
//! The renderer writes the buffer returned by `Backend::framebuffer_mut` directly.
//! Only drivers requesting a CPU shadow buffer incur a copy at presentation. A raw
//! pixman image borrowing that memory must be destroyed before reconfigure/drop and
//! must not access it concurrently with presentation or another mutable borrow.
//!
//! Inputd owns VT selection and the graphics driver saves/restores per-VT planes.
//! Consumers receive no VT enter/leave notifications at the pinned base revision;
//! consequently this crate does not invent active-state or key-reset events.
//! A display handoff is a driver replacement, not an ordinary VT switch.
//!
//! Timings use one `Instant` epoch. Cursor timing ends at the successful cursor
//! ioctl return, not at physical scanout. Presentation timing ends at dirty-fb
//! return. Neither is a vblank timestamp or an input-device hardware timestamp.

mod input;
#[cfg(target_os = "redox")]
mod platform;
#[cfg(target_os = "redox")]
mod presenter;
mod scheduler;
#[cfg(feature = "smithay")]
mod smithay_input;
#[cfg(target_os = "redox")]
mod vt;

pub const SWAP_BUFFERS: usize = 2;
pub const MAX_IN_FLIGHT_FLIPS: usize = 1;
pub const MAX_DMABUF_FBS_PER_HEAD: usize = 3;
pub const MAX_OUTPUTS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DmabufFb(u32);

impl DmabufFb {
    #[cfg(target_os = "redox")]
    pub(crate) fn new(id: u32) -> Self {
        assert!(id != 0);
        Self(id)
    }

    pub fn id(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DmabufFbDesc {
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    pub stride: u32,
    pub offset: u32,
    pub modifier: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputInfo {
    pub id: u32,
    pub crtc: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub refresh_mhz: u32,
    pub mm_width: u32,
    pub mm_height: u32,
    pub x: i32,
    pub y: i32,
    pub present_mode: PresentMode,
    pub has_cursor: bool,
}

pub use input::{AxisEvent, ButtonEvent, Input, KeyEvent, MotionEvent};
#[cfg(target_os = "redox")]
pub use platform::Backend;
pub use scheduler::FrameScheduler;

/// Type-level input backend on hosts; hardware operations only exist on FerrionOS.
#[cfg(not(target_os = "redox"))]
#[derive(Debug)]
pub enum Backend {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorTiming {
    pub read_us: u64,
    pub completed_us: u64,
    pub x: i32,
    pub y: i32,
}

impl CursorTiming {
    pub fn elapsed_us(self) -> u64 {
        self.completed_us.saturating_sub(self.read_us)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentMode {
    DirtyFb,
    PageFlip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentPreference {
    DirtyFb,
    PageFlip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlipCompletion {
    pub crtc: u32,
    pub completed_us: u64,
    pub sequence: u32,
    pub tv_us: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Presentation {
    pub started_us: u64,
    pub completed_us: u64,
    pub mode: PresentMode,
    pub pending: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendEvent {
    Input(Input),
    CursorTiming(CursorTiming),
    FlipComplete(FlipCompletion),
    /// Release renderer images, call `Backend::reconfigure`, then rebind.
    DisplayChanged,
}

#[cfg(not(target_os = "redox"))]
impl Backend {
    pub fn open() -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "inputd/DRM backend requires FerrionOS",
        ))
    }
    pub fn open_with_wakeup(_wakeup: impl Fn() + Send + Sync + 'static) -> std::io::Result<Self> {
        Self::open()
    }
    pub fn open_with_presenter(
        _wakeup: impl Fn() + Send + Sync + 'static,
        _preference: PresentPreference,
    ) -> std::io::Result<Self> {
        Self::open()
    }
    pub fn cursor_size(&self) -> std::io::Result<(u32, u32)> {
        match *self {}
    }
    pub fn cursor_size_for(&self, _output_id: u32) -> std::io::Result<(u32, u32)> {
        match *self {}
    }
    pub fn set_cursor(
        &mut self,
        _pixels: Option<&[u8]>,
        _size: (u32, u32),
        _hotspot: (i32, i32),
    ) -> std::io::Result<()> {
        match *self {}
    }
    pub fn set_cursor_for(
        &mut self,
        _output_id: u32,
        _pixels: Option<&[u8]>,
        _size: (u32, u32),
        _hotspot: (i32, i32),
    ) -> std::io::Result<()> {
        match *self {}
    }
    pub fn vt(&self) -> usize {
        match *self {}
    }
    pub fn outputs(&self) -> &[OutputInfo] {
        match *self {}
    }
    pub fn dimensions(&self) -> (u32, u32) {
        match *self {}
    }
    pub fn stride(&self) -> u32 {
        match *self {}
    }
    pub fn stride_for(&self, _output_id: u32) -> std::io::Result<u32> {
        match *self {}
    }
    pub fn present_mode(&self) -> PresentMode {
        match *self {}
    }
    pub fn flip_in_flight(&self) -> bool {
        match *self {}
    }
    pub fn flip_in_flight_for(&self, _output_id: u32) -> bool {
        match *self {}
    }
    pub fn complete_flip(&mut self) -> std::io::Result<()> {
        match *self {}
    }
    pub fn complete_flip_for(&mut self, _crtc: u32) -> std::io::Result<bool> {
        match *self {}
    }
    pub fn now_us(&self) -> u64 {
        match *self {}
    }
    pub fn refresh_period(&self) -> Option<std::time::Duration> {
        match *self {}
    }
    pub fn framebuffer_mut(&mut self) -> &mut [u8] {
        match *self {}
    }
    pub fn framebuffer_mut_for(&mut self, _output_id: u32) -> std::io::Result<&mut [u8]> {
        match *self {}
    }
    pub fn present(&mut self) -> std::io::Result<Presentation> {
        match *self {}
    }
    pub fn present_for(&mut self, _output_id: u32) -> std::io::Result<Presentation> {
        match *self {}
    }
    pub fn import_dmabuf_fb_for(
        &mut self,
        _output_id: u32,
        _dmabuf_fd: std::os::fd::BorrowedFd<'_>,
        _desc: DmabufFbDesc,
    ) -> std::io::Result<DmabufFb> {
        match *self {}
    }
    pub fn destroy_dmabuf_fb_for(&mut self, _output_id: u32, _fb: DmabufFb) -> std::io::Result<()> {
        match *self {}
    }
    pub fn destroy_all_dmabuf_fbs(&mut self) {
        match *self {}
    }
    pub fn present_dmabuf_for(
        &mut self,
        _output_id: u32,
        _fb: DmabufFb,
    ) -> std::io::Result<Presentation> {
        match *self {}
    }
    pub fn try_event(&mut self) -> std::io::Result<Option<BackendEvent>> {
        match *self {}
    }
    pub fn reconfigure(&mut self) -> std::io::Result<()> {
        match *self {}
    }
}

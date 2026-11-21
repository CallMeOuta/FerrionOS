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
mod scheduler;
#[cfg(feature = "smithay")]
mod smithay_input;
#[cfg(target_os = "redox")]
mod platform;

pub use input::{AxisEvent, ButtonEvent, Input, KeyEvent, MotionEvent};
pub use scheduler::FrameScheduler;
#[cfg(target_os = "redox")]
pub use platform::Backend;

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
pub struct Presentation {
    pub started_us: u64,
    pub completed_us: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendEvent {
    Input(Input),
    CursorTiming(CursorTiming),
    /// Release renderer images, call `Backend::reconfigure`, then rebind.
    DisplayChanged,
}

#[cfg(not(target_os = "redox"))]
impl Backend {
    pub fn open() -> std::io::Result<Self> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "inputd/DRM backend requires FerrionOS"))
    }
    pub fn vt(&self) -> usize { match *self {} }
    pub fn dimensions(&self) -> (u32, u32) { match *self {} }
    pub fn stride(&self) -> u32 { match *self {} }
    pub fn now_us(&self) -> u64 { match *self {} }
    pub fn refresh_period(&self) -> Option<std::time::Duration> { match *self {} }
    pub fn framebuffer_mut(&mut self) -> &mut [u8] { match *self {} }
    pub fn present(&mut self) -> std::io::Result<Presentation> { match *self {} }
    pub fn try_event(&mut self) -> std::io::Result<Option<BackendEvent>> { match *self {} }
    pub fn reconfigure(&mut self) -> std::io::Result<()> { match *self {} }
}

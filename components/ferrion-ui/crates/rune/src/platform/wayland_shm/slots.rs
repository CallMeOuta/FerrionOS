use std::fmt;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::ptr::NonNull;
use std::time::Duration;

use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Dispatch, Proxy, QueueHandle};

use crate::render::cpu_boot::{MAX_RECTS, NeedsGpu};
use crate::time::Instant;
use crate::window::CPU_MAX_SURFACE_DIM;

pub(crate) const MAX_SHM_SLOTS: usize = 2;
pub(crate) const MAX_SHM_BYTES: usize = 128 * 1024 * 1024;
pub(crate) const MAX_SURFACE_DIM: u32 = CPU_MAX_SURFACE_DIM;
pub(crate) const MAX_RELEASE_WAIT: Duration = Duration::from_secs(1);
#[cfg(not(target_os = "linux"))]
const MAX_SHM_OPEN_RETRIES: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CpuPresentError {
    NotWayland,
    TransparentUnsupported,
    MissingWlShm,
    DisplayConnection(String),
    BufferTooLarge {
        width: u32,
        height: u32,
        bytes: usize,
    },
    ShmAllocFailed(String),
    CompositorStalled,
    NeedsGpu(NeedsGpu),
}

impl fmt::Display for CpuPresentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CpuPresentError::NotWayland => {
                f.write_str("Wayland CPU presenter requires a Wayland surface")
            }
            CpuPresentError::TransparentUnsupported => f.write_str(
                "transparent windows are not supported by the Wayland CPU presenter yet",
            ),
            CpuPresentError::MissingWlShm => {
                f.write_str("Wayland compositor does not advertise wl_shm")
            }
            CpuPresentError::DisplayConnection(msg) => {
                write!(f, "Wayland display connection failed: {msg}")
            }
            CpuPresentError::BufferTooLarge {
                width,
                height,
                bytes,
            } => {
                write!(
                    f,
                    "Wayland CPU surface {width}x{height} ({bytes} bytes) exceeds limits"
                )
            }
            CpuPresentError::ShmAllocFailed(msg) => {
                write!(f, "Wayland wl_shm allocation failed: {msg}")
            }
            CpuPresentError::CompositorStalled => {
                write!(
                    f,
                    "Wayland compositor held all wl_shm buffers past {} ms",
                    MAX_RELEASE_WAIT.as_millis()
                )
            }
            CpuPresentError::NeedsGpu(why) => write!(f, "frame requires GPU: {why:?}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlotAcquire {
    Ready { slot: usize, needs_full_paint: bool },
    Deferred,
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn mmap_shm(fd: RawFd, bytes: usize) -> Result<NonNull<u8>, CpuPresentError> {
    assert!(fd >= 0);
    assert!(bytes > 0 && bytes <= MAX_SHM_BYTES);
    let map = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            bytes,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if map == libc::MAP_FAILED {
        return Err(CpuPresentError::ShmAllocFailed(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    NonNull::new(map.cast::<u8>())
        .ok_or_else(|| CpuPresentError::ShmAllocFailed("mmap returned null".into()))
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn munmap_shm(ptr: NonNull<u8>, bytes: usize) {
    assert!(bytes > 0 && bytes <= MAX_SHM_BYTES);
    unsafe {
        libc::munmap(ptr.as_ptr().cast(), bytes);
    }
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn memfd_create_shm() -> RawFd {
    unsafe { libc::memfd_create(c"ferrion-rune-shm".as_ptr(), libc::MFD_CLOEXEC) }
}

#[cfg(not(target_os = "linux"))]
#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn shm_open_exclusive(name: &[u8]) -> RawFd {
    assert!(name.ends_with(b"\0"));
    unsafe {
        libc::shm_open(
            name.as_ptr().cast(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )
    }
}

#[cfg(not(target_os = "linux"))]
#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn shm_unlink_name(name: &[u8]) {
    assert!(name.ends_with(b"\0"));
    unsafe {
        libc::shm_unlink(name.as_ptr().cast());
    }
}

#[cfg(not(target_os = "linux"))]
#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn set_fd_cloexec(fd: RawFd) {
    assert!(fd >= 0);
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
    }
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn own_fd(raw: RawFd) -> OwnedFd {
    assert!(raw >= 0);
    unsafe { OwnedFd::from_raw_fd(raw) }
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn ftruncate_fd(fd: RawFd, size: libc::off_t) -> i32 {
    assert!(fd >= 0);
    assert!(size > 0);
    unsafe { libc::ftruncate(fd, size) }
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn copy_slot_bytes(
    src: NonNull<u8>,
    dst: NonNull<u8>,
    offset: usize,
    len: usize,
    src_len: usize,
    dst_len: usize,
) {
    assert!(src != dst);
    assert!(
        offset
            .checked_add(len)
            .is_some_and(|end| end <= src_len && end <= dst_len)
    );
    unsafe {
        std::ptr::copy_nonoverlapping(src.as_ptr().add(offset), dst.as_ptr().add(offset), len);
    }
}

struct ShmSlot {
    id: u64,
    _fd: OwnedFd,
    ptr: NonNull<u8>,
    len: usize,
    width: u32,
    height: u32,
    stride: usize,
    pool: WlShmPool,
    buffer: WlBuffer,
    busy: bool,
    generation: u64,
}

impl ShmSlot {
    fn new<S>(
        shm: &WlShm,
        qh: &QueueHandle<S>,
        id: u64,
        width: u32,
        height: u32,
        format: wl_shm::Format,
    ) -> Result<Self, CpuPresentError>
    where
        S: Dispatch<WlShmPool, ()> + Dispatch<WlBuffer, u64> + 'static,
    {
        let (stride, bytes) = checked_buffer_size(width, height)?;
        let fd = create_shm_fd(bytes)?;
        let ptr = mmap_shm(fd.as_raw_fd(), bytes)?;
        let pool_size = i32::try_from(bytes).map_err(|_| CpuPresentError::BufferTooLarge {
            width,
            height,
            bytes,
        })?;
        let w_i32 = i32::try_from(width).map_err(|_| CpuPresentError::BufferTooLarge {
            width,
            height,
            bytes,
        })?;
        let h_i32 = i32::try_from(height).map_err(|_| CpuPresentError::BufferTooLarge {
            width,
            height,
            bytes,
        })?;
        let stride_i32 = i32::try_from(stride).map_err(|_| CpuPresentError::BufferTooLarge {
            width,
            height,
            bytes,
        })?;
        let pool = shm.create_pool(fd.as_fd(), pool_size, qh, ());
        let buffer = pool.create_buffer(0, w_i32, h_i32, stride_i32, format, qh, id);
        Ok(Self {
            id,
            _fd: fd,
            ptr,
            len: bytes,
            width,
            height,
            stride,
            pool,
            buffer,
            busy: false,
            generation: 0,
        })
    }

    #[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
    fn bytes_mut(&mut self) -> &mut [u8] {
        assert!(!self.busy);
        assert!(self.len > 0 && self.len <= MAX_SHM_BYTES);
        assert_eq!(
            self.stride.checked_mul(self.height as usize),
            Some(self.len)
        );
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl Drop for ShmSlot {
    fn drop(&mut self) {
        self.buffer.destroy();
        self.pool.destroy();
        munmap_shm(self.ptr, self.len);
    }
}

pub(crate) fn checked_buffer_size(
    width: u32,
    height: u32,
) -> Result<(usize, usize), CpuPresentError> {
    if width == 0 || height == 0 || width > MAX_SURFACE_DIM || height > MAX_SURFACE_DIM {
        return Err(CpuPresentError::BufferTooLarge {
            width,
            height,
            bytes: 0,
        });
    }
    let stride = (width as usize)
        .checked_mul(4)
        .ok_or(CpuPresentError::BufferTooLarge {
            width,
            height,
            bytes: usize::MAX,
        })?;
    let bytes = stride
        .checked_mul(height as usize)
        .ok_or(CpuPresentError::BufferTooLarge {
            width,
            height,
            bytes: usize::MAX,
        })?;
    if bytes == 0 || bytes > MAX_SHM_BYTES || i32::try_from(bytes).is_err() {
        return Err(CpuPresentError::BufferTooLarge {
            width,
            height,
            bytes,
        });
    }
    Ok((stride, bytes))
}

fn create_shm_fd(bytes: usize) -> Result<OwnedFd, CpuPresentError> {
    #[cfg(target_os = "linux")]
    let raw = memfd_create_shm();
    #[cfg(not(target_os = "linux"))]
    let raw = {
        let mut opened = -1;
        let pid = std::process::id();
        for attempt in 0..MAX_SHM_OPEN_RETRIES {
            let name = format!("/ferrion-rune-shm-{pid}-{attempt}\0");
            let fd = shm_open_exclusive(name.as_bytes());
            if fd >= 0 {
                shm_unlink_name(name.as_bytes());
                set_fd_cloexec(fd);
                opened = fd;
                break;
            }
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists {
                break;
            }
        }
        opened
    };
    if raw < 0 {
        return Err(CpuPresentError::ShmAllocFailed(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    let fd = own_fd(raw);
    let size = libc::off_t::try_from(bytes)
        .map_err(|_| CpuPresentError::ShmAllocFailed("buffer size overflow".into()))?;
    if ftruncate_fd(fd.as_raw_fd(), size) < 0 {
        return Err(CpuPresentError::ShmAllocFailed(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    Ok(fd)
}

fn copy_slot_rects(src: &ShmSlot, dst: &mut ShmSlot, rects: &[[u32; 4]]) {
    assert_eq!(
        (src.width, src.height, src.stride, src.len),
        (dst.width, dst.height, dst.stride, dst.len)
    );
    assert!(!dst.busy);
    if rects.is_empty() || rects.len() > MAX_RECTS {
        copy_slot_bytes(src.ptr, dst.ptr, 0, src.len, src.len, dst.len);
        return;
    }
    for &[x0, y0, x1, y1] in rects {
        let x0 = x0.min(src.width);
        let x1 = x1.min(src.width);
        let y0 = y0.min(src.height);
        let y1 = y1.min(src.height);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        assert!(x1 <= src.width && y1 <= src.height);
        let row_bytes = (x1 - x0) as usize * 4;
        let col_offset = x0 as usize * 4;
        for y in y0..y1 {
            let offset = y as usize * src.stride + col_offset;
            copy_slot_bytes(src.ptr, dst.ptr, offset, row_bytes, src.len, dst.len);
        }
    }
}

fn damage_surface(surface: &WlSurface, rect: [u32; 4], scale: i32) {
    let [x0, y0, x1, y1] = rect;
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    if surface.version() >= 4 {
        surface.damage_buffer(x0 as i32, y0 as i32, (x1 - x0) as i32, (y1 - y0) as i32);
    } else {
        let s = scale.max(1) as u32;
        let sx0 = (x0 / s) as i32;
        let sy0 = (y0 / s) as i32;
        let sx1 = x1.div_ceil(s) as i32;
        let sy1 = y1.div_ceil(s) as i32;
        surface.damage(sx0, sy0, (sx1 - sx0).max(1), (sy1 - sy0).max(1));
    }
}

pub(crate) struct ShmRing {
    format: wl_shm::Format,
    slots: [Option<ShmSlot>; MAX_SHM_SLOTS],
    next_alloc_id: u64,
    current_gen: u64,
    last_presented: Option<usize>,
    last_rects: Vec<[u32; 4]>,
    last_scale: i32,
    stalled_since: Option<Instant>,
    stall_logged: bool,
    deferred_dirty: bool,
}

impl ShmRing {
    pub(crate) fn new(format: wl_shm::Format) -> Self {
        Self {
            format,
            slots: [None, None],
            next_alloc_id: 1,
            current_gen: 0,
            last_presented: None,
            last_rects: Vec::new(),
            last_scale: 0,
            stalled_since: None,
            stall_logged: false,
            deferred_dirty: false,
        }
    }

    pub(crate) fn mark_released(&mut self, released: impl IntoIterator<Item = u64>) -> bool {
        let mut released_any = false;
        for released_id in released {
            for slot in self.slots.iter_mut().flatten() {
                if slot.id == released_id {
                    slot.busy = false;
                    released_any = true;
                }
            }
        }
        if released_any {
            self.stalled_since = None;
            self.stall_logged = false;
        }
        released_any
    }

    pub(crate) fn deferred_dirty(&self) -> bool {
        self.deferred_dirty
    }

    pub(crate) fn waiting_for_slot(&self) -> bool {
        self.deferred_dirty && self.stalled_since.is_some()
    }

    pub(crate) fn stall_deadline(&self) -> Option<Instant> {
        self.stalled_since.map(|since| since + MAX_RELEASE_WAIT)
    }

    pub(crate) fn has_presented(&self) -> bool {
        self.last_presented.is_some()
    }

    pub(crate) fn acquire_slot<S>(
        &mut self,
        shm: &WlShm,
        qh: &QueueHandle<S>,
        width: u32,
        height: u32,
        now: Instant,
    ) -> Result<SlotAcquire, CpuPresentError>
    where
        S: Dispatch<WlShmPool, ()> + Dispatch<WlBuffer, u64> + 'static,
    {
        let _ = checked_buffer_size(width, height)?;
        let preferred = self.last_presented.filter(|&idx| {
            self.slots[idx]
                .as_ref()
                .is_some_and(|s| !s.busy && s.width == width && s.height == height)
        });
        let chosen = preferred.or_else(|| {
            (0..MAX_SHM_SLOTS).find(|&idx| self.slots[idx].as_ref().is_none_or(|s| !s.busy))
        });
        let Some(idx) = chosen else {
            self.deferred_dirty = true;
            let since = *self.stalled_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= MAX_RELEASE_WAIT {
                if !self.stall_logged {
                    self.stall_logged = true;
                    return Err(CpuPresentError::CompositorStalled);
                }
                return Ok(SlotAcquire::Deferred);
            }
            return Ok(SlotAcquire::Deferred);
        };
        self.deferred_dirty = false;
        self.stalled_since = None;
        self.stall_logged = false;
        let realloc = self.slots[idx]
            .as_ref()
            .is_none_or(|s| s.width != width || s.height != height);
        if realloc {
            let alloc_id = self.next_alloc_id;
            self.next_alloc_id = self.next_alloc_id.wrapping_add(1).max(1);
            self.slots[idx] = Some(ShmSlot::new(shm, qh, alloc_id, width, height, self.format)?);
        }
        let needs_full_paint = match self.last_presented {
            Some(prev_idx) if prev_idx == idx => self.slots[idx]
                .as_ref()
                .is_none_or(|slot| slot.generation != self.current_gen),
            Some(prev_idx) => {
                let can_copy = self.slots[prev_idx].as_ref().is_some_and(|p| {
                    p.width == width && p.height == height && p.generation == self.current_gen
                });
                if can_copy {
                    let (first, second) = self.slots.split_at_mut(1);
                    let pair = if prev_idx == 0 {
                        first[0].as_ref().zip(second[0].as_mut())
                    } else {
                        second[0].as_ref().zip(first[0].as_mut())
                    };
                    if let Some((src, dst)) = pair {
                        if dst.generation + 1 == self.current_gen && !self.last_rects.is_empty() {
                            copy_slot_rects(src, dst, &self.last_rects);
                        } else {
                            copy_slot_rects(src, dst, &[]);
                        }
                        dst.generation = self.current_gen;
                        false
                    } else {
                        true
                    }
                } else {
                    true
                }
            }
            None => true,
        };
        Ok(SlotAcquire::Ready {
            slot: idx,
            needs_full_paint,
        })
    }

    pub(crate) fn slot_target_mut(&mut self, slot: usize) -> Option<(&mut [u8], u32, u32, usize)> {
        let s = self.slots.get_mut(slot)?.as_mut()?;
        let (w, h, stride) = (s.width, s.height, s.stride);
        Some((s.bytes_mut(), w, h, stride))
    }

    pub(crate) fn attach_and_damage(
        &mut self,
        surface: &WlSurface,
        slot: usize,
        rects: &[[u32; 4]],
        scale: i32,
        force_full_damage: bool,
    ) -> Result<(), CpuPresentError> {
        let scale = scale.max(1);
        let (w, h) = {
            let Some(s) = self.slots.get(slot).and_then(|s| s.as_ref()) else {
                return Err(CpuPresentError::ShmAllocFailed("missing slot".into()));
            };
            assert!(!s.busy);
            (s.width, s.height)
        };
        let scale_changed = self.last_scale != scale;
        if surface.version() >= 3 && scale_changed {
            surface.set_buffer_scale(scale);
            self.last_scale = scale;
        }
        let Some(s) = self.slots.get(slot).and_then(|s| s.as_ref()) else {
            return Err(CpuPresentError::ShmAllocFailed("missing slot".into()));
        };
        surface.attach(Some(&s.buffer), 0, 0);
        let full = force_full_damage
            || scale_changed
            || self.last_presented.is_none()
            || rects.len() > MAX_RECTS;
        if full {
            damage_surface(surface, [0, 0, w, h], scale);
            self.last_rects.clear();
            self.last_rects.push([0, 0, w, h]);
        } else {
            for &r in rects {
                damage_surface(surface, r, scale);
            }
            self.last_rects.clear();
            self.last_rects.extend_from_slice(rects);
        }
        Ok(())
    }

    pub(crate) fn mark_presented(&mut self, slot: usize) {
        self.current_gen = self.current_gen.wrapping_add(1).max(1);
        if let Some(s) = self.slots.get_mut(slot).and_then(|s| s.as_mut()) {
            s.busy = true;
            s.generation = self.current_gen;
        }
        self.last_presented = Some(slot);
    }

    pub(crate) fn clear_slots(&mut self) {
        for slot in &mut self.slots {
            *slot = None;
        }
    }
}

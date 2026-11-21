//! Asks the display driver, through a display handle, for dumb buffers and mappings outside a
//! buffer or outside the driver's limits, then checks that the driver still answers. Run it as
//! `user` in a login shell.
//!
//! Each line reads `graphics-bounds-probe: pass <check>: ...` or `graphics-bounds-probe: FAIL
//! <check>: ...`. A refused request must fail with the error named in its check, and a fresh
//! handle must still create and map a small buffer afterwards. The exit status is 0
//! only when every line passes. With scheme names as arguments (`display.virtio-gpu`) it checks
//! only those, else every `display.*` scheme. `scripts/vm-test.py graphics-bounds` runs it on a
//! booted image.

use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::mem;
use std::os::fd::AsRawFd;
use std::process::ExitCode;
use std::ptr;

/// The virtual terminal whose display the handles open. It exists on every booted desktop.
const VT: u32 = 1;
const PAGE: usize = 4096;
/// The display scheme's map offsets: a buffer number times this, plus a position in the buffer.
const MAP_STRIDE: u64 = 0x1000_0000;
/// A buffer number no handle reaches by creating buffers.
const UNUSED_BUFFER: u64 = 0xFFFF;

#[repr(C)]
#[derive(Default)]
struct CardRes {
    fb_id_ptr: u64,
    crtc_id_ptr: u64,
    connector_id_ptr: u64,
    encoder_id_ptr: u64,
    count_fbs: u32,
    count_crtcs: u32,
    count_connectors: u32,
    count_encoders: u32,
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
}

#[repr(C)]
#[derive(Default)]
struct CreateDumb {
    height: u32,
    width: u32,
    bpp: u32,
    flags: u32,
    handle: u32,
    pitch: u32,
    size: u64,
}

#[repr(C)]
#[derive(Default)]
struct MapDumb {
    handle: u32,
    pad: u32,
    offset: u64,
}

#[repr(C)]
#[derive(Default)]
struct DestroyDumb {
    handle: u32,
}

/// Linux's `DRM_IOWR(nr, T)`, which the C library turns into a call on the display scheme.
fn drm_iowr<T>(nr: u8) -> libc::c_ulong {
    ((3 << 30) | ((mem::size_of::<T>() as u64) << 16) | ((b'd' as u64) << 8) | nr as u64)
        as libc::c_ulong
}

struct Display {
    file: File,
}

struct Mapping {
    ptr: *mut c_void,
    len: usize,
}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr, self.len) };
    }
}

impl Display {
    fn open(path: &str) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self { file })
    }

    fn ioctl<T>(&self, nr: u8, data: &mut T) -> io::Result<()> {
        let fd = self.file.as_raw_fd();
        if unsafe { libc::ioctl(fd, drm_iowr::<T>(nr), data as *mut T) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The largest width and height the driver says a buffer may have.
    fn max_size(&self) -> io::Result<(u32, u32)> {
        let mut ids = [[0u32; 32]; 4];
        let mut res = CardRes {
            fb_id_ptr: ids[0].as_mut_ptr() as u64,
            crtc_id_ptr: ids[1].as_mut_ptr() as u64,
            connector_id_ptr: ids[2].as_mut_ptr() as u64,
            encoder_id_ptr: ids[3].as_mut_ptr() as u64,
            count_fbs: 32,
            count_crtcs: 32,
            count_connectors: 32,
            count_encoders: 32,
            ..CardRes::default()
        };
        self.ioctl(0xA0, &mut res)?;
        Ok((res.max_width, res.max_height))
    }

    fn create(&self, width: u32, height: u32) -> io::Result<CreateDumb> {
        let mut data = CreateDumb {
            width,
            height,
            bpp: 32,
            ..CreateDumb::default()
        };
        self.ioctl(0xB2, &mut data)?;
        Ok(data)
    }

    fn map_offset(&self, handle: u32) -> io::Result<u64> {
        let mut data = MapDumb {
            handle,
            ..MapDumb::default()
        };
        self.ioctl(0xB3, &mut data)?;
        Ok(data.offset)
    }

    fn destroy(&self, handle: u32) -> io::Result<()> {
        self.ioctl(0xB4, &mut DestroyDumb { handle })
    }

    fn mmap(&self, offset: u64, len: usize) -> io::Result<Mapping> {
        let ptr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                self.file.as_raw_fd(),
                offset as libc::off_t,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Mapping { ptr, len })
    }
}

fn describe(err: &io::Error) -> String {
    match err.raw_os_error() {
        Some(errno) => format!("errno {errno} ({err})"),
        None => err.to_string(),
    }
}

fn errno_in(err: &io::Error, allowed: &[i32]) -> bool {
    err.raw_os_error()
        .is_some_and(|errno| allowed.contains(&errno))
}

/// The display schemes named on the command line, or else every one in the caller's namespace,
/// as paths to the probe's virtual terminal.
fn display_paths() -> io::Result<Vec<String>> {
    let named: Vec<String> = std::env::args().skip(1).collect();
    if !named.is_empty() {
        return Ok(named
            .iter()
            .map(|name| format!("/scheme/{name}/{VT}"))
            .collect());
    }
    let mut paths: Vec<String> = fs::read_dir("/scheme")?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with("display."))
        .map(|name| format!("/scheme/{name}/{VT}"))
        .collect();
    paths.sort();
    Ok(paths)
}

struct Report {
    failed: bool,
}

impl Report {
    fn line(&mut self, check: &str, ok: bool, detail: String) {
        let verdict = if ok { "pass" } else { "FAIL" };
        println!("graphics-bounds-probe: {verdict} {check}: {detail}");
        self.failed |= !ok;
    }

    /// A request that must be refused with one of `allowed`.
    fn refused<T>(&mut self, check: &str, what: &str, result: io::Result<T>, allowed: &[i32]) {
        match result {
            Ok(_) => self.line(check, false, format!("{what} was accepted")),
            Err(err) => self.line(
                check,
                errno_in(&err, allowed),
                format!("{what}: {}", describe(&err)),
            ),
        }
    }
}

/// Mappings that run past the end of a small buffer, or start past it.
fn check_mappings(report: &mut Report, display: &Display) {
    let buffer = match display.create(64, 64) {
        Ok(buffer) => buffer,
        Err(err) => {
            return report.line("small-buffer", false, format!("64x64: {}", describe(&err)))
        }
    };
    let size = buffer.size as usize;
    let offset = match display.map_offset(buffer.handle) {
        Ok(offset) => offset,
        Err(err) => {
            let _ = display.destroy(buffer.handle);
            return report.line(
                "small-buffer",
                false,
                format!("map offset: {}", describe(&err)),
            );
        }
    };
    match display.mmap(offset, size) {
        Ok(_) => report.line(
            "small-buffer",
            true,
            format!("64x64, {size} bytes, maps whole"),
        ),
        Err(err) => report.line(
            "small-buffer",
            false,
            format!("mapping its {size} bytes: {}", describe(&err)),
        ),
    }
    report.refused(
        "past-end-length",
        &format!("a mapping of {} bytes", size + PAGE),
        display.mmap(offset, size + PAGE),
        &[libc::EINVAL],
    );
    report.refused(
        "past-end-offset",
        &format!("a page at position {size}"),
        display.mmap(offset + size as u64, PAGE),
        &[libc::EINVAL],
    );
    let far = MAP_STRIDE - PAGE as u64;
    report.refused(
        "far-offset",
        &format!("a page at position {far:#x}"),
        display.mmap(offset + far, PAGE),
        &[libc::EINVAL],
    );
    let _ = display.destroy(buffer.handle);
}

/// Buffers beyond the driver's own limits, one pixel too wide or too tall.
fn check_over_max(report: &mut Report, display: &Display, max: (u32, u32)) {
    for (check, width, height) in [
        ("over-max-width", max.0.saturating_add(1), 1),
        ("over-max-height", 1, max.1.saturating_add(1)),
    ] {
        let result = display.create(width, height);
        if let Ok(buffer) = &result {
            let _ = display.destroy(buffer.handle);
        }
        report.refused(
            check,
            &format!("{width}x{height} against a maximum of {}x{}", max.0, max.1),
            result,
            &[libc::EINVAL],
        );
    }
}

/// The largest buffer within the driver's limits that the map offset layout still holds: it
/// may be created or refused for memory, and mapped or refused, but must not stop the driver.
fn check_largest(report: &mut Report, display: &Display, max: (u32, u32)) {
    let width = max.0.max(1);
    let height = (MAP_STRIDE / (u64::from(width) * 4)).clamp(1, u64::from(max.1.max(1))) as u32;
    let what = format!("{width}x{height}");
    match display.create(width, height) {
        Ok(buffer) => {
            let mapped = display.map_offset(buffer.handle);
            let _ = display.destroy(buffer.handle);
            match mapped {
                Ok(_) => report.line("largest", true, format!("{what} created and mapped")),
                Err(err) => report.line(
                    "largest",
                    errno_in(&err, &[libc::EINVAL]),
                    format!("{what} created, map offset: {}", describe(&err)),
                ),
            }
        }
        Err(err) => report.line(
            "largest",
            errno_in(&err, &[libc::ENOMEM, libc::EINVAL]),
            format!("{what}: {}", describe(&err)),
        ),
    }
}

/// A fresh handle still reads the driver's limits and creates, maps and destroys a small buffer.
fn check_still_answers(report: &mut Report, path: &str) {
    let result = (|| -> io::Result<(u32, u32)> {
        let display = Display::open(path)?;
        let max = display.max_size()?;
        let buffer = display.create(32, 32)?;
        drop(display.mmap(display.map_offset(buffer.handle)?, buffer.size as usize)?);
        display.destroy(buffer.handle)?;
        Ok(max)
    })();
    match result {
        Ok(max) => report.line(
            "still-answers",
            true,
            format!(
                "a fresh handle reads the maximum {}x{} and creates, maps and destroys a 32x32 buffer",
                max.0, max.1
            ),
        ),
        Err(err) => report.line("still-answers", false, describe(&err)),
    }
}

fn main() -> ExitCode {
    let mut report = Report { failed: false };

    let paths = match display_paths() {
        Ok(paths) if !paths.is_empty() => paths,
        Ok(_) => {
            report.line(
                "display",
                false,
                "no display.* scheme in /scheme".to_owned(),
            );
            return ExitCode::FAILURE;
        }
        Err(err) => {
            report.line("display", false, format!("/scheme: {err}"));
            return ExitCode::FAILURE;
        }
    };

    for path in &paths {
        let display = match Display::open(path) {
            Ok(display) => display,
            Err(err) => {
                report.line("display", false, format!("{path}: {}", describe(&err)));
                continue;
            }
        };
        let max = match display.max_size() {
            Ok(max) => max,
            Err(err) => {
                report.line(
                    "display",
                    false,
                    format!("{path}: resources: {}", describe(&err)),
                );
                continue;
            }
        };
        report.line(
            "display",
            true,
            format!("{path}, maximum {}x{}", max.0, max.1),
        );

        // The checks that an unchecked driver survives come first.
        check_mappings(&mut report, &display);
        check_over_max(&mut report, &display, max);
        check_largest(&mut report, &display, max);
        report.refused(
            "overflow",
            "4294967295x4294967295",
            display.create(u32::MAX, u32::MAX),
            &[libc::EINVAL],
        );
        report.refused(
            "unknown-buffer",
            &format!("a page of buffer {UNUSED_BUFFER:#x}"),
            display.mmap(UNUSED_BUFFER * MAP_STRIDE, PAGE),
            &[libc::ENOENT, libc::EBADF],
        );
        drop(display);

        check_still_answers(&mut report, path);
    }

    if report.failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

//! Native counter descriptors. On Redox, fdobjd owns the counter and readiness.
use crate::{
    error::{Errno, ResultExt},
    header::{errno::{EINVAL, EIO}, fcntl::{O_CLOEXEC, O_NONBLOCK}, unistd},
    platform::types::{c_int, c_uint},
};

pub type eventfd_t = u64;
pub const EFD_SEMAPHORE: c_int = 1;
pub const EFD_CLOEXEC: c_int = O_CLOEXEC;
pub const EFD_NONBLOCK: c_int = O_NONBLOCK;

#[unsafe(no_mangle)]
pub extern "C" fn eventfd(initval: c_uint, flags: c_int) -> c_int {
    if flags & !(EFD_SEMAPHORE | EFD_CLOEXEC | EFD_NONBLOCK) != 0 {
        return Err::<c_int, _>(Errno(EINVAL)).or_minus_one_errno();
    }
    #[cfg(target_os = "linux")]
    return unsafe {
        crate::platform::sys::e_raw(syscall!(EVENTFD2, initval, flags))
            .map(|fd| fd as c_int).or_minus_one_errno()
    };
    #[cfg(target_os = "redox")]
    {
        // Format the bounded u32 without allocating a pathname.
        let mut path = [0_u8; 28];
        let prefix = b"/scheme/eventfd/";
        path[..prefix.len()].copy_from_slice(prefix);
        let mut digits = [0_u8; 10];
        let mut start = digits.len();
        let mut value = initval;
        loop {
            start -= 1;
            digits[start] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 { break; }
        }
        let end = prefix.len() + digits.len() - start;
        path[prefix.len()..end].copy_from_slice(&digits[start..]);
        path[end] = b'/';
        path[end + 1] = b'0' + (flags & EFD_SEMAPHORE) as u8;
        redox_rt::sys::open(core::str::from_utf8(&path[..end + 2]).unwrap(),
            (crate::header::fcntl::O_RDWR | (flags & (EFD_CLOEXEC | EFD_NONBLOCK))) as usize)
            .map(|fd| fd as c_int).map_err(Errno::from).or_minus_one_errno()
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn eventfd_read(fd: c_int, value: *mut eventfd_t) -> c_int {
    let count = unsafe { unistd::read(fd, value.cast(), core::mem::size_of::<eventfd_t>()) };
    if count == 8 { 0 } else if count < 0 { -1 } else {
        Err::<c_int, _>(Errno(EIO)).or_minus_one_errno()
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn eventfd_write(fd: c_int, value: eventfd_t) -> c_int {
    let count = unsafe { unistd::write(fd, (&raw const value).cast(), core::mem::size_of::<eventfd_t>()) };
    if count == 8 { 0 } else if count < 0 { -1 } else {
        Err::<c_int, _>(Errno(EIO)).or_minus_one_errno()
    }
}

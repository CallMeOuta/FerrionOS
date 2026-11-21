//! Native timer descriptors, with timer state and readiness owned by fdobjd on Redox.
use crate::{
    error::{Errno, ResultExt},
    header::{errno::{EFAULT, EINVAL}, fcntl::{O_CLOEXEC, O_NONBLOCK}, time::itimerspec},
    platform::types::{c_int, clockid_t},
};

pub const TFD_CLOEXEC: c_int = O_CLOEXEC;
pub const TFD_NONBLOCK: c_int = O_NONBLOCK;
pub const TFD_TIMER_ABSTIME: c_int = 1;
/// Redox currently rejects this flag with EINVAL: clock-set notifications are unavailable.
pub const TFD_TIMER_CANCEL_ON_SET: c_int = 2;

#[unsafe(no_mangle)]
pub extern "C" fn timerfd_create(clockid: clockid_t, flags: c_int) -> c_int {
    if flags & !(TFD_CLOEXEC | TFD_NONBLOCK) != 0 {
        return Err::<c_int, _>(Errno(EINVAL)).or_minus_one_errno();
    }
    #[cfg(target_os = "linux")]
    return unsafe {
        crate::platform::sys::e_raw(syscall!(TIMERFD_CREATE, clockid, flags))
            .map(|fd| fd as c_int).or_minus_one_errno()
    };
    #[cfg(target_os = "redox")]
    {
        use crate::header::time::{CLOCK_MONOTONIC, CLOCK_REALTIME};
        let path = match clockid {
            CLOCK_REALTIME => "/scheme/timerfd/1",
            CLOCK_MONOTONIC => "/scheme/timerfd/4",
            _ => return Err::<c_int, _>(Errno(EINVAL)).or_minus_one_errno(),
        };
        redox_rt::sys::open(path, (crate::header::fcntl::O_RDWR | flags) as usize)
            .map(|fd| fd as c_int).map_err(Errno::from).or_minus_one_errno()
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn timerfd_settime(
    fd: c_int, flags: c_int, new_value: *const itimerspec, old_value: *mut itimerspec,
) -> c_int {
    if flags & !(TFD_TIMER_ABSTIME | TFD_TIMER_CANCEL_ON_SET) != 0 {
        return Err::<c_int, _>(Errno(EINVAL)).or_minus_one_errno();
    }
    if new_value.is_null() {
        return Err::<c_int, _>(Errno(EFAULT)).or_minus_one_errno();
    }
    #[cfg(target_os = "linux")]
    return unsafe {
        crate::platform::sys::e_raw(syscall!(TIMERFD_SETTIME, fd, flags, new_value, old_value))
            .map(|_| 0).or_minus_one_errno()
    };
    #[cfg(target_os = "redox")]
    {
        let value = unsafe { &*new_value };
        let fields = [value.it_interval.tv_sec, value.it_interval.tv_nsec,
            value.it_value.tv_sec, value.it_value.tv_nsec];
        let mut payload = [0_u8; 32];
        for (chunk, field) in payload.chunks_exact_mut(8).zip(fields) {
            chunk.copy_from_slice(&field.to_ne_bytes());
        }
        // The daemon rejects unsupported cancellation semantics, never silently ignores them.
        match redox_rt::sys::sys_call_rw(fd as usize, &mut payload,
            syscall::CallFlags::empty(), &[1, flags as u64]) {
            Ok(_) => {
                if !old_value.is_null() { unsafe { old_value.write(decode(&payload)); } }
                0
            }
            Err(error) => Err::<c_int, _>(Errno::from(error)).or_minus_one_errno(),
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn timerfd_gettime(fd: c_int, value: *mut itimerspec) -> c_int {
    if value.is_null() {
        return Err::<c_int, _>(Errno(EFAULT)).or_minus_one_errno();
    }
    #[cfg(target_os = "linux")]
    return unsafe {
        crate::platform::sys::e_raw(syscall!(TIMERFD_GETTIME, fd, value))
            .map(|_| 0).or_minus_one_errno()
    };
    #[cfg(target_os = "redox")]
    {
        let mut payload = [0_u8; 32];
        match redox_rt::sys::sys_call_rw(fd as usize, &mut payload,
            syscall::CallFlags::empty(), &[2]) {
            Ok(_) => { unsafe { value.write(decode(&payload)); } 0 }
            Err(error) => Err::<c_int, _>(Errno::from(error)).or_minus_one_errno(),
        }
    }
}

#[cfg(target_os = "redox")]
fn decode(payload: &[u8; 32]) -> itimerspec {
    use crate::header::time::timespec;
    let field = |offset| i64::from_ne_bytes(payload[offset..offset + 8].try_into().unwrap());
    itimerspec {
        it_interval: timespec { tv_sec: field(0), tv_nsec: field(8) },
        it_value: timespec { tv_sec: field(16), tv_nsec: field(24) },
    }
}

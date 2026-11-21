//! Linux error numbers, and relibc's turned into Linux's. relibc numbers its errors as Linux does
//! except for two names Linux aliases (EWOULDBLOCK, EDEADLOCK), which relibc gives numbers of
//! their own.

pub const EPERM: i64 = 1;
pub const ESRCH: i64 = 3;
pub const EAGAIN: i64 = 11;
pub const ENOMEM: i64 = 12;
pub const EFAULT: i64 = 14;
pub const EEXIST: i64 = 17;
pub const EINVAL: i64 = 22;
pub const ENOTTY: i64 = 25;
pub const EDEADLK: i64 = 35;
pub const ENAMETOOLONG: i64 = 36;
pub const ENOSYS: i64 = 38;
pub const ENOEXEC: i64 = 8;
pub const E2BIG: i64 = 7;

pub fn linux_errno(native: i32) -> i64 {
    match native {
        libc::EWOULDBLOCK if libc::EWOULDBLOCK != libc::EAGAIN => EAGAIN,
        libc::EDEADLOCK if libc::EDEADLOCK != libc::EDEADLK => EDEADLK,
        e => e as i64,
    }
}

pub fn last_errno() -> i64 {
    linux_errno(
        std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO),
    )
}

/// A libredox raw result: values in the top 4096 are negated error numbers.
pub fn raw(r: usize) -> Result<usize, i64> {
    if r > usize::MAX - 4096 {
        Err(linux_errno((r as isize).wrapping_neg() as i32))
    } else {
        Ok(r)
    }
}

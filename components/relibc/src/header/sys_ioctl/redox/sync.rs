use redox_ioctl::{IoctlData, sync::*};
use syscall::flag::F_SETFD;

use crate::{
    error::{Errno, Result},
    header::{
        errno::{EFAULT, EINVAL, ENOTTY},
        fcntl::sys::FD_CLOEXEC,
    },
    platform::types::c_int,
};

use super::IoctlBuffer;

fn monotonic_remaining_ns(abs_timeout_nsec: i64) -> i64 {
    if abs_timeout_nsec <= 0 {
        return 0;
    }
    if abs_timeout_nsec == i64::MAX {
        return i64::MAX;
    }
    let mut ts = syscall::TimeSpec::default();
    if syscall::clock_gettime(syscall::flag::CLOCK_MONOTONIC, &mut ts).is_err() {
        return 0;
    }
    let now = i128::from(ts.tv_sec)
        .saturating_mul(1_000_000_000)
        .saturating_add(i128::from(ts.tv_nsec));
    let diff = i128::from(abs_timeout_nsec).saturating_sub(now);
    if diff <= 0 {
        0
    } else if diff > i128::from(i64::MAX) {
        i64::MAX
    } else {
        diff as i64
    }
}

pub(super) fn map_two_fd_err(err: syscall::Error) -> Errno {
    if err.errno == syscall::EXDEV || err.errno == syscall::EBADF {
        Errno(EINVAL)
    } else {
        Errno::from(err)
    }
}

unsafe fn rw_fixed<T: IoctlData>(fd: c_int, mut buf: IoctlBuffer, func: u64) -> Result<c_int> {
    let mut data = unsafe { buf.read::<T>() }?;
    let mut wire = unsafe { data.write() };
    let res = redox_rt::sys::sys_call_rw(
        fd as usize,
        &mut wire,
        syscall::CallFlags::empty(),
        &[func],
    )?;
    unsafe { data.read_from(&wire) };
    unsafe { buf.write(data) }?;
    Ok(res as c_int)
}

unsafe fn wo_fixed<T: IoctlData>(fd: c_int, buf: IoctlBuffer, func: u64) -> Result<c_int> {
    let data = unsafe { buf.read::<T>() }?;
    let wire = unsafe { data.write() };
    let res = redox_rt::sys::sys_call_wo(
        fd as usize,
        &wire,
        syscall::CallFlags::empty(),
        &[func],
    )?;
    Ok(res as c_int)
}

unsafe fn syncobj_handle_to_fd(fd: c_int, mut buf: IoctlBuffer) -> Result<c_int> {
    let mut data = unsafe { buf.read::<drm_syncobj_handle>() }?;
    if data.pad != 0 {
        return Err(Errno(EINVAL));
    }
    let payload = encode_handle_to_fd_dup(data.handle, data.flags, data.point);
    let new_fd = redox_rt::sys::dup(fd as usize, &payload)?;
    let _ = redox_rt::sys::fcntl(new_fd, F_SETFD, FD_CLOEXEC as usize);
    data.fd = new_fd as i32;
    unsafe { buf.write(data) }?;
    Ok(0)
}

unsafe fn syncobj_fd_to_handle(fd: c_int, mut buf: IoctlBuffer) -> Result<c_int> {
    let mut data = unsafe { buf.read::<drm_syncobj_handle>() }?;
    if data.pad != 0 || data.fd < 0 {
        return Err(Errno(EINVAL));
    }
    let mut wire = unsafe { data.write() };
    redox_rt::sys::sys_call_rw(
        &[fd as usize, data.fd as usize][..],
        &mut wire,
        syscall::CallFlags::empty(),
        &[SYNCOBJ_FD_TO_HANDLE],
    )
    .map_err(map_two_fd_err)?;
    unsafe { data.read_from(&wire) };
    unsafe { buf.write(data) }?;
    Ok(0)
}

unsafe fn syncobj_wait(fd: c_int, mut buf: IoctlBuffer) -> Result<c_int> {
    let mut data = unsafe { buf.read::<drm_syncobj_wait>() }?;
    if data.count_handles > SYNCOBJ_MAX_HANDLES {
        return Err(Errno(EINVAL));
    }
    if data.count_handles != 0 && data.handles == 0 {
        return Err(Errno(EFAULT));
    }
    let orig_timeout = data.timeout_nsec;
    let orig_deadline = data.deadline_nsec;
    data.timeout_nsec = monotonic_remaining_ns(orig_timeout);
    if data.flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_DEADLINE != 0 {
        data.deadline_nsec =
            monotonic_remaining_ns(orig_deadline.min(i64::MAX as u64) as i64) as u64;
    }
    let mut wire = unsafe { data.write() };
    let res = redox_rt::sys::sys_call_rw(
        fd as usize,
        &mut wire,
        syscall::CallFlags::empty(),
        &[SYNCOBJ_WAIT],
    )?;
    unsafe { data.read_from(&wire) };
    data.timeout_nsec = orig_timeout;
    data.deadline_nsec = orig_deadline;
    unsafe { buf.write(data) }?;
    Ok(res as c_int)
}

unsafe fn syncobj_timeline_wait(fd: c_int, mut buf: IoctlBuffer) -> Result<c_int> {
    let mut data = unsafe { buf.read::<drm_syncobj_timeline_wait>() }?;
    if data.count_handles > SYNCOBJ_MAX_HANDLES {
        return Err(Errno(EINVAL));
    }
    if data.count_handles != 0 && (data.handles == 0 || data.points == 0) {
        return Err(Errno(EFAULT));
    }
    let orig_timeout = data.timeout_nsec;
    let orig_deadline = data.deadline_nsec;
    data.timeout_nsec = monotonic_remaining_ns(orig_timeout);
    if data.flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_DEADLINE != 0 {
        data.deadline_nsec =
            monotonic_remaining_ns(orig_deadline.min(i64::MAX as u64) as i64) as u64;
    }
    let mut wire = unsafe { data.write() };
    let res = redox_rt::sys::sys_call_rw(
        fd as usize,
        &mut wire,
        syscall::CallFlags::empty(),
        &[SYNCOBJ_TIMELINE_WAIT],
    )?;
    unsafe { data.read_from(&wire) };
    data.timeout_nsec = orig_timeout;
    data.deadline_nsec = orig_deadline;
    unsafe { buf.write(data) }?;
    Ok(res as c_int)
}

unsafe fn syncobj_array(fd: c_int, buf: IoctlBuffer, func: u64) -> Result<c_int> {
    let data = unsafe { buf.read::<drm_syncobj_array>() }?;
    if data.count_handles > SYNCOBJ_MAX_HANDLES {
        return Err(Errno(EINVAL));
    }
    if data.count_handles != 0 && data.handles == 0 {
        return Err(Errno(EFAULT));
    }
    let wire = unsafe { data.write() };
    let res = redox_rt::sys::sys_call_wo(
        fd as usize,
        &wire,
        syscall::CallFlags::empty(),
        &[func],
    )?;
    Ok(res as c_int)
}

unsafe fn syncobj_query(fd: c_int, mut buf: IoctlBuffer) -> Result<c_int> {
    let mut data = unsafe { buf.read::<drm_syncobj_query_array>() }?;
    if data.count_handles > SYNCOBJ_MAX_HANDLES {
        return Err(Errno(EINVAL));
    }
    if data.count_handles != 0 && (data.handles == 0 || data.points == 0) {
        return Err(Errno(EFAULT));
    }
    let mut wire = unsafe { data.write() };
    let res = redox_rt::sys::sys_call_rw(
        fd as usize,
        &mut wire,
        syscall::CallFlags::empty(),
        &[SYNCOBJ_QUERY],
    )?;
    unsafe { data.read_from(&wire) };
    unsafe { buf.write(data) }?;
    Ok(res as c_int)
}

unsafe fn syncobj_timeline_signal(fd: c_int, buf: IoctlBuffer) -> Result<c_int> {
    let data = unsafe { buf.read::<drm_syncobj_timeline_array>() }?;
    if data.count_handles > SYNCOBJ_MAX_HANDLES {
        return Err(Errno(EINVAL));
    }
    if data.count_handles != 0 && (data.handles == 0 || data.points == 0) {
        return Err(Errno(EFAULT));
    }
    let wire = unsafe { data.write() };
    let res = redox_rt::sys::sys_call_wo(
        fd as usize,
        &wire,
        syscall::CallFlags::empty(),
        &[SYNCOBJ_TIMELINE_SIGNAL],
    )?;
    Ok(res as c_int)
}

pub(super) unsafe fn drm_syncobj_ioctl(fd: c_int, func: u8, buf: IoctlBuffer) -> Result<c_int> {
    match func {
        0xBF => unsafe { rw_fixed::<drm_syncobj_create>(fd, buf, SYNCOBJ_CREATE) },
        0xC0 => unsafe { wo_fixed::<drm_syncobj_destroy>(fd, buf, SYNCOBJ_DESTROY) },
        0xC1 => unsafe { syncobj_handle_to_fd(fd, buf) },
        0xC2 => unsafe { syncobj_fd_to_handle(fd, buf) },
        0xC3 => unsafe { syncobj_wait(fd, buf) },
        0xC4 => unsafe { syncobj_array(fd, buf, SYNCOBJ_RESET) },
        0xC5 => unsafe { syncobj_array(fd, buf, SYNCOBJ_SIGNAL) },
        0xCA => unsafe { syncobj_timeline_wait(fd, buf) },
        0xCB => unsafe { syncobj_query(fd, buf) },
        0xCC => unsafe { wo_fixed::<drm_syncobj_transfer>(fd, buf, SYNCOBJ_TRANSFER) },
        0xCD => unsafe { syncobj_timeline_signal(fd, buf) },
        0xCF => unsafe { wo_fixed::<drm_syncobj_eventfd>(fd, buf, SYNCOBJ_EVENTFD) },
        _ => Err(Errno(EINVAL)),
    }
}

unsafe fn sync_merge(fd: c_int, mut buf: IoctlBuffer) -> Result<c_int> {
    let mut data = unsafe { buf.read::<sync_merge_data>() }?;
    if data.flags != 0 || data.pad != 0 || data.fd2 < 0 {
        return Err(Errno(EINVAL));
    }
    let call_wire = encode_sync_merge_call(&data.name, data.flags, data.pad);
    let token = redox_rt::sys::sys_call_wo(
        &[fd as usize, data.fd2 as usize][..],
        &call_wire,
        syscall::CallFlags::empty(),
        &[SYNC_MERGE],
    )
    .map_err(map_two_fd_err)?;
    let dup_wire = encode_sync_merge_dup(token as u64);
    let new_fd = redox_rt::sys::dup(fd as usize, &dup_wire)?;
    let _ = redox_rt::sys::fcntl(new_fd, F_SETFD, FD_CLOEXEC as usize);
    data.fence = new_fd as i32;
    unsafe { buf.write(data) }?;
    Ok(0)
}

unsafe fn sync_file_info_ioctl(fd: c_int, mut buf: IoctlBuffer) -> Result<c_int> {
    let mut data = unsafe { buf.read::<sync_file_info>() }?;
    if data.flags != 0 || data.pad != 0 || data.num_fences > SYNC_MAX_FENCES {
        return Err(Errno(EINVAL));
    }
    if data.num_fences != 0 && data.sync_fence_info == 0 {
        return Err(Errno(EFAULT));
    }
    let mut wire = unsafe { data.write() };
    let res = redox_rt::sys::sys_call_rw(
        fd as usize,
        &mut wire,
        syscall::CallFlags::empty(),
        &[SYNC_FILE_INFO],
    )?;
    unsafe { data.read_from(&wire) };
    unsafe { buf.write(data) }?;
    Ok(res as c_int)
}

pub(super) unsafe fn sync_file_ioctl(fd: c_int, func: u8, buf: IoctlBuffer) -> Result<c_int> {
    match func {
        0x03 => unsafe { sync_merge(fd, buf) },
        0x04 => unsafe { sync_file_info_ioctl(fd, buf) },
        0x05 => unsafe { wo_fixed::<sync_set_deadline>(fd, buf, SYNC_SET_DEADLINE) },
        _ => Err(Errno(ENOTTY)),
    }
}

pub(super) unsafe fn dmabuf_ioctl(func: u8, buf: IoctlBuffer) -> Result<c_int> {
    match func {
        0x00 => {
            let _ = unsafe { buf.read::<u64>() }?;
            Ok(0)
        }
        _ => Err(Errno(ENOTTY)),
    }
}

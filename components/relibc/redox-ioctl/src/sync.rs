//! DRM sync objects, timelines and sync files (FerrionOS).
//!
//! DRM syncobj structures and constants come from Linux's `include/uapi/drm/drm.h`
//! (MIT) via `drm_sys`. Sync-file structures and constants come from Linux's
//! `include/uapi/linux/sync_file.h` (GPL-1.0+ WITH Linux-syscall-note,
//! Copyright (C) 2012 Google, Inc.).

use alloc::vec::Vec;
use core::{cmp, ffi::c_char, iter, mem, slice};

pub use drm_sys::{
    DRM_CAP_SYNCOBJ, DRM_CAP_SYNCOBJ_TIMELINE, DRM_SYNCOBJ_CREATE_SIGNALED,
    DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE, DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_TIMELINE,
    DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE, DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_TIMELINE,
    DRM_SYNCOBJ_QUERY_FLAGS_LAST_SUBMITTED, DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL,
    DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE, DRM_SYNCOBJ_WAIT_FLAGS_WAIT_DEADLINE,
    DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT, drm_syncobj_array, drm_syncobj_create,
    drm_syncobj_destroy, drm_syncobj_eventfd, drm_syncobj_handle, drm_syncobj_timeline_array,
    drm_syncobj_timeline_wait, drm_syncobj_transfer, drm_syncobj_wait,
};

pub const SYNCOBJ_CREATE: u64 = 0xBF;
pub const SYNCOBJ_DESTROY: u64 = 0xC0;
pub const SYNCOBJ_HANDLE_TO_FD: u64 = 0xC1;
pub const SYNCOBJ_FD_TO_HANDLE: u64 = 0xC2;
pub const SYNCOBJ_WAIT: u64 = 0xC3;
pub const SYNCOBJ_RESET: u64 = 0xC4;
pub const SYNCOBJ_SIGNAL: u64 = 0xC5;
pub const SYNCOBJ_TIMELINE_WAIT: u64 = 0xCA;
pub const SYNCOBJ_QUERY: u64 = 0xCB;
pub const SYNCOBJ_TRANSFER: u64 = 0xCC;
pub const SYNCOBJ_TIMELINE_SIGNAL: u64 = 0xCD;
pub const SYNCOBJ_EVENTFD: u64 = 0xCF;

pub const SYNCOBJ_MAX_HANDLES: u32 = 4096;
pub const SYNC_MAX_FENCES: u32 = 1024;

pub const SYNC_IOC_MAGIC: u8 = b'>';
pub const SYNC_MERGE: u64 = 0x03;
pub const SYNC_FILE_INFO: u64 = 0x04;
pub const SYNC_SET_DEADLINE: u64 = 0x05;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct sync_merge_data {
    pub name: [c_char; 32],
    pub fd2: i32,
    pub fence: i32,
    pub flags: u32,
    pub pad: u32,
}

impl Default for sync_merge_data {
    fn default() -> Self {
        Self {
            name: [0; 32],
            fd2: 0,
            fence: 0,
            flags: 0,
            pad: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct sync_fence_info {
    pub obj_name: [c_char; 32],
    pub driver_name: [c_char; 32],
    pub status: i32,
    pub flags: u32,
    pub timestamp_ns: u64,
}

impl Default for sync_fence_info {
    fn default() -> Self {
        Self {
            obj_name: [0; 32],
            driver_name: [0; 32],
            status: 0,
            flags: 0,
            timestamp_ns: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct sync_file_info {
    pub name: [c_char; 32],
    pub status: i32,
    pub flags: u32,
    pub num_fences: u32,
    pub pad: u32,
    pub sync_fence_info: u64,
}

impl Default for sync_file_info {
    fn default() -> Self {
        Self {
            name: [0; 32],
            status: 0,
            flags: 0,
            num_fences: 0,
            pad: 0,
            sync_fence_info: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct sync_set_deadline {
    pub deadline_ns: u64,
    pub pad: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct drm_syncobj_query_array {
    pub handles: u64,
    pub points: u64,
    pub count_handles: u32,
    pub flags: u32,
}

define_ioctl_data! {
    struct drm_syncobj_create, DrmSyncobjCreate {
        handle: u32,
        flags: u32,
    }
}

define_ioctl_data! {
    struct drm_syncobj_destroy, DrmSyncobjDestroy {
        handle: u32,
        pad: u32,
    }
}

define_ioctl_data! {
    struct drm_syncobj_handle, DrmSyncobjHandle {
        handle: u32,
        flags: u32,
        fd: i32,
        pad: u32,
        point: u64,
    }
}

define_ioctl_data! {
    struct drm_syncobj_transfer, DrmSyncobjTransfer {
        src_handle: u32,
        dst_handle: u32,
        src_point: u64,
        dst_point: u64,
        flags: u32,
        pad: u32,
    }
}

define_ioctl_data! {
    struct drm_syncobj_wait, DrmSyncobjWait {
        timeout_nsec: i64,
        count_handles: u32,
        flags: u32,
        first_signaled: u32,
        pad: u32,
        deadline_nsec: u64,
        handles: u64 [in_array<u32, count_handles>],
    }
}

define_ioctl_data! {
    struct drm_syncobj_timeline_wait, DrmSyncobjTimelineWait {
        timeout_nsec: i64,
        count_handles: u32,
        flags: u32,
        first_signaled: u32,
        pad: u32,
        deadline_nsec: u64,
        points: u64 [in_array<u64, count_handles>],
        handles: u64 [in_array<u32, count_handles>],
    }
}

define_ioctl_data! {
    struct drm_syncobj_eventfd, DrmSyncobjEventfd {
        handle: u32,
        flags: u32,
        point: u64,
        fd: i32,
        pad: u32,
    }
}

define_ioctl_data! {
    struct drm_syncobj_array, DrmSyncobjArray {
        count_handles: u32,
        pad: u32,
        handles: u64 [in_array<u32, count_handles>],
    }
}

define_ioctl_data! {
    struct drm_syncobj_timeline_array, DrmSyncobjTimelineSignal {
        count_handles: u32,
        flags: u32,
        points: u64 [in_array<u64, count_handles>],
        handles: u64 [in_array<u32, count_handles>],
    }
}

define_ioctl_data! {
    struct drm_syncobj_query_array, DrmSyncobjQuery {
        count_handles: u32,
        flags: u32,
        points: u64 [out_array<u64, count_handles>],
        handles: u64 [in_array<u32, count_handles>],
    }
}

define_ioctl_data! {
    struct sync_file_info, SyncFileInfo {
        name: [c_char; 32],
        status: i32,
        flags: u32,
        num_fences: u32,
        pad: u32,
        sync_fence_info: u64 [out_array<sync_fence_info, num_fences>],
    }
}

define_ioctl_data! {
    struct sync_set_deadline, SyncSetDeadline {
        deadline_ns: u64,
        pad: u64,
    }
}

pub fn encode_handle_to_fd_dup(handle: u32, flags: u32, point: u64) -> [u8; 24] {
    let mut buf = [0u8; 24];
    buf[0..8].copy_from_slice(&SYNCOBJ_HANDLE_TO_FD.to_ne_bytes());
    buf[8..12].copy_from_slice(&handle.to_ne_bytes());
    buf[12..16].copy_from_slice(&flags.to_ne_bytes());
    buf[16..24].copy_from_slice(&point.to_ne_bytes());
    buf
}

pub fn decode_handle_to_fd_dup(buf: &[u8]) -> Option<(u32, u32, u64)> {
    if buf.len() != 24 {
        return None;
    }
    let cmd = u64::from_ne_bytes(buf[0..8].try_into().ok()?);
    if cmd != SYNCOBJ_HANDLE_TO_FD {
        return None;
    }
    let handle = u32::from_ne_bytes(buf[8..12].try_into().ok()?);
    let flags = u32::from_ne_bytes(buf[12..16].try_into().ok()?);
    let point = u64::from_ne_bytes(buf[16..24].try_into().ok()?);
    Some((handle, flags, point))
}

pub fn encode_sync_merge_call(name: &[c_char; 32], flags: u32, pad: u32) -> [u8; 40] {
    let mut buf = [0u8; 40];
    for (i, &c) in name.iter().enumerate() {
        buf[i] = c as u8;
    }
    buf[32..36].copy_from_slice(&flags.to_ne_bytes());
    buf[36..40].copy_from_slice(&pad.to_ne_bytes());
    buf
}

pub fn decode_sync_merge_call(buf: &[u8]) -> Option<([u8; 32], u32, u32)> {
    if buf.len() != 40 {
        return None;
    }
    let mut name = [0u8; 32];
    name.copy_from_slice(&buf[0..32]);
    let flags = u32::from_ne_bytes(buf[32..36].try_into().ok()?);
    let pad = u32::from_ne_bytes(buf[36..40].try_into().ok()?);
    Some((name, flags, pad))
}

pub fn encode_sync_merge_dup(token: u64) -> [u8; 16] {
    let mut buf = [0u8; 16];
    buf[0..8].copy_from_slice(&SYNC_MERGE.to_ne_bytes());
    buf[8..16].copy_from_slice(&token.to_ne_bytes());
    buf
}

pub fn decode_sync_merge_dup(buf: &[u8]) -> Option<u64> {
    if buf.len() != 16 {
        return None;
    }
    let cmd = u64::from_ne_bytes(buf[0..8].try_into().ok()?);
    if cmd != SYNC_MERGE {
        return None;
    }
    Some(u64::from_ne_bytes(buf[8..16].try_into().ok()?))
}

const _: () = {
    assert!(size_of::<drm_syncobj_create>() == 8);
    assert!(size_of::<drm_syncobj_destroy>() == 8);
    assert!(size_of::<drm_syncobj_handle>() == 24);
    assert!(size_of::<drm_syncobj_transfer>() == 32);
    assert!(size_of::<drm_syncobj_wait>() == 40);
    assert!(size_of::<drm_syncobj_timeline_wait>() == 48);
    assert!(size_of::<drm_syncobj_eventfd>() == 24);
    assert!(size_of::<drm_syncobj_array>() == 16);
    assert!(size_of::<drm_syncobj_timeline_array>() == 24);
    assert!(size_of::<drm_syncobj_query_array>() == 24);
    assert!(size_of::<sync_merge_data>() == 48);
    assert!(size_of::<sync_fence_info>() == 80);
    assert!(size_of::<sync_file_info>() == 56);
    assert!(size_of::<sync_set_deadline>() == 16);

    assert!(DrmSyncobjWait::LAYOUT.is_packed() && DrmSyncobjWait::LAYOUT.is_aligned());
    assert!(
        DrmSyncobjTimelineWait::LAYOUT.is_packed() && DrmSyncobjTimelineWait::LAYOUT.is_aligned()
    );
    assert!(DrmSyncobjArray::LAYOUT.is_packed() && DrmSyncobjArray::LAYOUT.is_aligned());
    assert!(
        DrmSyncobjTimelineSignal::LAYOUT.is_packed()
            && DrmSyncobjTimelineSignal::LAYOUT.is_aligned()
    );
    assert!(DrmSyncobjQuery::LAYOUT.is_packed() && DrmSyncobjQuery::LAYOUT.is_aligned());
    assert!(SyncFileInfo::LAYOUT.is_packed() && SyncFileInfo::LAYOUT.is_aligned());
};

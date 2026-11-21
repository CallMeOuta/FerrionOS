//! The DRM requests of virtio-gpu, and DRM's own close request, which Mesa's
//! Venus driver makes on the same device.
//!
//! Numbers, flags and structures are those of Linux's
//! `include/uapi/drm/virtgpu_drm.h` at the tag v7.3 (commit
//! c03c1a14507dfaf8b856c2608f9ac216021fc222), whose notice follows. A program
//! compiled against that header hands these structures to `ioctl` as they
//! are, so every size, offset and number here stays equal to the header's.
//! The close request and its structure are those of `include/uapi/drm/drm.h`
//! at the same commit, under the same MIT terms. FerrionOS's
//! `scripts/virtgpu-layout-check.py` compares them.
//!
//! The payload types, `VirtgpuMap` and the others, are what the driver
//! receives for a request: the structure without its pointers, then each
//! array a pointer names. An array the request only reads is `in_array`,
//! sent and not copied back; one it only writes is `out_array`, sent as
//! zeros. Arrays are listed from the most strictly aligned down, so each
//! starts aligned whatever the counts, which the assertion at the end checks.
//! Transfers, waits and the older resource creation have structures only:
//! no payload is defined for them.
//!
//! Copyright 2013 Red Hat
//! All Rights Reserved.
//!
//! Permission is hereby granted, free of charge, to any person obtaining a
//! copy of this software and associated documentation files (the "Software"),
//! to deal in the Software without restriction, including without limitation
//! the rights to use, copy, modify, merge, publish, distribute, sublicense,
//! and/or sell copies of the Software, and to permit persons to whom the
//! Software is furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice (including the next
//! paragraph) shall be included in all copies or substantial portions of the
//! Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.  IN NO EVENT SHALL
//! THE AUTHORS AND/OR ITS SUPPLIERS BE LIABLE FOR ANY CLAIM, DAMAGES OR
//! OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE,
//! ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR
//! OTHER DEALINGS IN THE SOFTWARE.

use alloc::vec::Vec;
use core::{
    cmp,
    ffi::c_int,
    iter,
    mem::{self, offset_of},
    slice,
};

use crate::{IoctlData, WireArray, WireDirection, WireField, WireLayout};

/// `DRM_COMMAND_BASE` of Linux's `drm.h`: driver requests start here.
pub const DRM_COMMAND_BASE: u64 = 0x40;

pub const DRM_VIRTGPU_MAP: u64 = 0x01;
pub const DRM_VIRTGPU_EXECBUFFER: u64 = 0x02;
pub const DRM_VIRTGPU_GETPARAM: u64 = 0x03;
pub const DRM_VIRTGPU_RESOURCE_CREATE: u64 = 0x04;
pub const DRM_VIRTGPU_RESOURCE_INFO: u64 = 0x05;
pub const DRM_VIRTGPU_TRANSFER_FROM_HOST: u64 = 0x06;
pub const DRM_VIRTGPU_TRANSFER_TO_HOST: u64 = 0x07;
pub const DRM_VIRTGPU_WAIT: u64 = 0x08;
pub const DRM_VIRTGPU_GET_CAPS: u64 = 0x09;
pub const DRM_VIRTGPU_RESOURCE_CREATE_BLOB: u64 = 0x0a;
pub const DRM_VIRTGPU_CONTEXT_INIT: u64 = 0x0b;

/// The function numbers the driver receives: the low byte of each request,
/// as for the requests of `drm`.
pub const MAP: u64 = DRM_COMMAND_BASE + DRM_VIRTGPU_MAP;
pub const EXECBUFFER: u64 = DRM_COMMAND_BASE + DRM_VIRTGPU_EXECBUFFER;
pub const GETPARAM: u64 = DRM_COMMAND_BASE + DRM_VIRTGPU_GETPARAM;
pub const RESOURCE_CREATE: u64 = DRM_COMMAND_BASE + DRM_VIRTGPU_RESOURCE_CREATE;
pub const RESOURCE_INFO: u64 = DRM_COMMAND_BASE + DRM_VIRTGPU_RESOURCE_INFO;
pub const TRANSFER_FROM_HOST: u64 = DRM_COMMAND_BASE + DRM_VIRTGPU_TRANSFER_FROM_HOST;
pub const TRANSFER_TO_HOST: u64 = DRM_COMMAND_BASE + DRM_VIRTGPU_TRANSFER_TO_HOST;
pub const WAIT: u64 = DRM_COMMAND_BASE + DRM_VIRTGPU_WAIT;
pub const GET_CAPS: u64 = DRM_COMMAND_BASE + DRM_VIRTGPU_GET_CAPS;
pub const RESOURCE_CREATE_BLOB: u64 = DRM_COMMAND_BASE + DRM_VIRTGPU_RESOURCE_CREATE_BLOB;
pub const CONTEXT_INIT: u64 = DRM_COMMAND_BASE + DRM_VIRTGPU_CONTEXT_INIT;

pub const VIRTGPU_EXECBUF_FENCE_FD_IN: u32 = 0x01;
pub const VIRTGPU_EXECBUF_FENCE_FD_OUT: u32 = 0x02;
pub const VIRTGPU_EXECBUF_RING_IDX: u32 = 0x04;
pub const VIRTGPU_EXECBUF_FLAGS: u32 =
    VIRTGPU_EXECBUF_FENCE_FD_IN | VIRTGPU_EXECBUF_FENCE_FD_OUT | VIRTGPU_EXECBUF_RING_IDX;

pub const VIRTGPU_EXECBUF_SYNCOBJ_RESET: u32 = 0x01;
pub const VIRTGPU_EXECBUF_SYNCOBJ_FLAGS: u32 = VIRTGPU_EXECBUF_SYNCOBJ_RESET;

pub const VIRTGPU_PARAM_3D_FEATURES: u64 = 1;
pub const VIRTGPU_PARAM_CAPSET_QUERY_FIX: u64 = 2;
pub const VIRTGPU_PARAM_RESOURCE_BLOB: u64 = 3;
pub const VIRTGPU_PARAM_HOST_VISIBLE: u64 = 4;
pub const VIRTGPU_PARAM_CROSS_DEVICE: u64 = 5;
pub const VIRTGPU_PARAM_CONTEXT_INIT: u64 = 6;
pub const VIRTGPU_PARAM_SUPPORTED_CAPSET_IDs: u64 = 7;
pub const VIRTGPU_PARAM_EXPLICIT_DEBUG_NAME: u64 = 8;
pub const VIRTGPU_PARAM_BLOB_ALIGNMENT: u64 = 9;

pub const VIRTGPU_WAIT_NOWAIT: u32 = 1;

pub const VIRTGPU_DRM_CAPSET_VIRGL: u32 = 1;
pub const VIRTGPU_DRM_CAPSET_VIRGL2: u32 = 2;
pub const VIRTGPU_DRM_CAPSET_GFXSTREAM_VULKAN: u32 = 3;
pub const VIRTGPU_DRM_CAPSET_VENUS: u32 = 4;
pub const VIRTGPU_DRM_CAPSET_CROSS_DOMAIN: u32 = 5;
pub const VIRTGPU_DRM_CAPSET_DRM: u32 = 6;

pub const VIRTGPU_BLOB_MEM_GUEST: u32 = 0x0001;
pub const VIRTGPU_BLOB_MEM_HOST3D: u32 = 0x0002;
pub const VIRTGPU_BLOB_MEM_HOST3D_GUEST: u32 = 0x0003;

pub const VIRTGPU_BLOB_FLAG_USE_MAPPABLE: u32 = 0x0001;
pub const VIRTGPU_BLOB_FLAG_USE_SHAREABLE: u32 = 0x0002;
pub const VIRTGPU_BLOB_FLAG_USE_CROSS_DEVICE: u32 = 0x0004;

pub const DRM_VIRTGPU_BLOB_FLAG_HINT_DEFER_MAPPING: u32 = 0x0001;

pub const VIRTGPU_CONTEXT_PARAM_CAPSET_ID: u64 = 0x0001;
pub const VIRTGPU_CONTEXT_PARAM_NUM_RINGS: u64 = 0x0002;
pub const VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK: u64 = 0x0003;
pub const VIRTGPU_CONTEXT_PARAM_DEBUG_NAME: u64 = 0x0004;

pub const VIRTGPU_EVENT_FENCE_SIGNALED: u32 = 0x9000_0000;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct drm_virtgpu_map {
    pub offset: u64,
    pub handle: u32,
    pub pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct drm_virtgpu_execbuffer_syncobj {
    pub handle: u32,
    pub flags: u32,
    pub point: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct drm_virtgpu_execbuffer {
    pub flags: u32,
    pub size: u32,
    pub command: u64,
    pub bo_handles: u64,
    pub num_bo_handles: u32,
    pub fence_fd: i32,
    pub ring_idx: u32,
    pub syncobj_stride: u32,
    pub num_in_syncobjs: u32,
    pub num_out_syncobjs: u32,
    pub in_syncobjs: u64,
    pub out_syncobjs: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct drm_virtgpu_getparam {
    pub param: u64,
    pub value: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct drm_virtgpu_resource_create {
    pub target: u32,
    pub format: u32,
    pub bind: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub array_size: u32,
    pub last_level: u32,
    pub nr_samples: u32,
    pub flags: u32,
    pub bo_handle: u32,
    pub res_handle: u32,
    pub size: u32,
    pub stride: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct drm_virtgpu_resource_info {
    pub bo_handle: u32,
    pub res_handle: u32,
    pub size: u32,
    pub blob_mem: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct drm_virtgpu_3d_box {
    pub x: u32,
    pub y: u32,
    pub z: u32,
    pub w: u32,
    pub h: u32,
    pub d: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct drm_virtgpu_3d_transfer_to_host {
    pub bo_handle: u32,
    pub r#box: drm_virtgpu_3d_box,
    pub level: u32,
    pub offset: u32,
    pub stride: u32,
    pub layer_stride: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct drm_virtgpu_3d_transfer_from_host {
    pub bo_handle: u32,
    pub r#box: drm_virtgpu_3d_box,
    pub level: u32,
    pub offset: u32,
    pub stride: u32,
    pub layer_stride: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct drm_virtgpu_3d_wait {
    pub handle: u32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct drm_virtgpu_get_caps {
    pub cap_set_id: u32,
    pub cap_set_ver: u32,
    pub addr: u64,
    pub size: u32,
    pub pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct drm_virtgpu_resource_create_blob {
    pub blob_mem: u32,
    pub blob_flags: u32,
    pub bo_handle: u32,
    pub res_handle: u32,
    pub size: u64,
    pub pad: u32,
    pub cmd_size: u32,
    pub cmd: u64,
    pub blob_id: u64,
    pub blob_hints: u32,
    pub pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct drm_virtgpu_context_set_param {
    pub param: u64,
    pub value: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct drm_virtgpu_context_init {
    pub num_params: u32,
    pub pad: u32,
    pub ctx_set_params: u64,
}

/// `DRM_IOWR(nr, T)` of Linux's `drm.h` for a structure of `size` bytes:
/// `_IOC(_IOC_READ | _IOC_WRITE, 'd', nr, size)` in the encoding of
/// `asm-generic/ioctl.h`, which x86_64, aarch64 and riscv64 share.
pub const fn drm_iowr(nr: u64, size: usize) -> u64 {
    assert!(nr < 1 << 8 && size < 1 << 14);
    (3 << 30) | ((size as u64) << 16) | (0x64 << 8) | nr
}

/// `DRM_IOW(nr, T)` of Linux's `drm.h`: `_IOC(_IOC_WRITE, 'd', nr, size)`.
pub const fn drm_iow(nr: u64, size: usize) -> u64 {
    assert!(nr < 1 << 8 && size < 1 << 14);
    (1 << 30) | ((size as u64) << 16) | (0x64 << 8) | nr
}

pub use drm_sys::drm_gem_close;

/// `DRM_IOCTL_GEM_CLOSE` of Linux's `drm.h`: drops a buffer number. Its
/// function number is not one of the driver's own, which start at
/// `DRM_COMMAND_BASE`.
pub const GEM_CLOSE: u64 = 0x09;
pub const DRM_IOCTL_GEM_CLOSE: u64 = drm_iow(GEM_CLOSE, size_of::<drm_gem_close>());

pub const DRM_IOCTL_VIRTGPU_MAP: u64 = drm_iowr(MAP, size_of::<drm_virtgpu_map>());
pub const DRM_IOCTL_VIRTGPU_EXECBUFFER: u64 =
    drm_iowr(EXECBUFFER, size_of::<drm_virtgpu_execbuffer>());
pub const DRM_IOCTL_VIRTGPU_GETPARAM: u64 = drm_iowr(GETPARAM, size_of::<drm_virtgpu_getparam>());
pub const DRM_IOCTL_VIRTGPU_RESOURCE_CREATE: u64 =
    drm_iowr(RESOURCE_CREATE, size_of::<drm_virtgpu_resource_create>());
pub const DRM_IOCTL_VIRTGPU_RESOURCE_INFO: u64 =
    drm_iowr(RESOURCE_INFO, size_of::<drm_virtgpu_resource_info>());
pub const DRM_IOCTL_VIRTGPU_TRANSFER_FROM_HOST: u64 = drm_iowr(
    TRANSFER_FROM_HOST,
    size_of::<drm_virtgpu_3d_transfer_from_host>(),
);
pub const DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST: u64 = drm_iowr(
    TRANSFER_TO_HOST,
    size_of::<drm_virtgpu_3d_transfer_to_host>(),
);
pub const DRM_IOCTL_VIRTGPU_WAIT: u64 = drm_iowr(WAIT, size_of::<drm_virtgpu_3d_wait>());
pub const DRM_IOCTL_VIRTGPU_GET_CAPS: u64 = drm_iowr(GET_CAPS, size_of::<drm_virtgpu_get_caps>());
pub const DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB: u64 = drm_iowr(
    RESOURCE_CREATE_BLOB,
    size_of::<drm_virtgpu_resource_create_blob>(),
);
pub const DRM_IOCTL_VIRTGPU_CONTEXT_INIT: u64 =
    drm_iowr(CONTEXT_INIT, size_of::<drm_virtgpu_context_init>());

define_ioctl_data! {
    struct drm_virtgpu_map, VirtgpuMap {
        offset: u64,
        handle: u32,
        pad: u32,
    }
}

// The arrays assume `syncobj_stride` equals the size of
// `drm_virtgpu_execbuffer_syncobj` whenever `num_in_syncobjs` or
// `num_out_syncobjs` is nonzero; the caller of `write` checks it first. With
// both counts zero the stride is not read, as in Linux: Venus sends 0 then.
// `fence_fd` carries a fence's number in the handle's table, never a
// descriptor of the calling process. The sync objects come first and the
// command last, so that a command of any length leaves the others aligned.
define_ioctl_data! {
    struct drm_virtgpu_execbuffer, VirtgpuExecbuffer {
        flags: u32,
        size: u32,
        num_bo_handles: u32,
        fence_fd: i32,
        ring_idx: u32,
        syncobj_stride: u32,
        num_in_syncobjs: u32,
        num_out_syncobjs: u32,
        in_syncobjs: u64 [in_array<drm_virtgpu_execbuffer_syncobj, num_in_syncobjs>],
        out_syncobjs: u64 [in_array<drm_virtgpu_execbuffer_syncobj, num_out_syncobjs>],
        bo_handles: u64 [in_array<u32, num_bo_handles>],
        command: u64 [in_array<u8, size>],
    }
}

define_ioctl_data! {
    struct drm_virtgpu_resource_info, VirtgpuResourceInfo {
        bo_handle: u32,
        res_handle: u32,
        size: u32,
        blob_mem: u32,
    }
}

define_ioctl_data! {
    struct drm_virtgpu_get_caps, VirtgpuGetCaps {
        cap_set_id: u32,
        cap_set_ver: u32,
        addr: u64 [out_array<u8, size>],
        size: u32,
        pad: u32,
    }
}

define_ioctl_data! {
    struct drm_virtgpu_resource_create_blob, VirtgpuResourceCreateBlob {
        blob_mem: u32,
        blob_flags: u32,
        bo_handle: u32,
        res_handle: u32,
        size: u64,
        pad: u32,
        cmd_size: u32,
        cmd: u64 [in_array<u8, cmd_size>],
        blob_id: u64,
        blob_hints: u32,
        pad2: u32,
    }
}

/// The most parameters context-init takes: Linux refuses more before it reads
/// any (`virtgpu_ioctl.c`, "Number of unique parameters supported at this
/// time").
pub const CONTEXT_INIT_MAX_PARAMS: u32 = 4;

/// The longest name a context is given: the 64-byte name of virtio-gpu's
/// context-create command less its terminating zero, as many as Linux's
/// `strscpy` into that field leaves.
pub const CONTEXT_NAME_MAX: usize = 63;

#[repr(C)]
pub struct __VirtgpuContextInitNoncounted {
    num_params: u32,
    pad: u32,
}

/// The payload of context-init: the structure without its pointer, the
/// parameters `ctx_set_params` points at (sent, not copied back), then the
/// context's name, which Linux's structure does not hold.
///
/// A `DEBUG_NAME` parameter holds the address of a string inside an array
/// element, which no payload can carry. So relibc reads that string itself and
/// sends its bytes after the parameters, at most `CONTEXT_NAME_MAX` of them and
/// without a terminating zero. When the program gives no name, as Mesa's Venus
/// driver does not, relibc sends the name the program runs under, which is what
/// Linux names a context after. The `DEBUG_NAME` parameter itself still arrives,
/// its value being the caller's address, which means nothing to the driver.
/// This payload is written by hand for that name; its `LAYOUT` describes the
/// part Linux's structure has.
pub struct VirtgpuContextInit<'a> {
    num_params: u32,
    params: &'a [u8],
    name: &'a [u8],
}

impl<'a> VirtgpuContextInit<'a> {
    pub const LAYOUT: WireLayout = WireLayout {
        fixed_size: size_of::<__VirtgpuContextInitNoncounted>(),
        fixed: &[
            WireField {
                name: "num_params",
                offset: offset_of!(__VirtgpuContextInitNoncounted, num_params),
                size: 4,
            },
            WireField {
                name: "pad",
                offset: offset_of!(__VirtgpuContextInitNoncounted, pad),
                size: 4,
            },
        ],
        arrays: &[WireArray {
            pointer: "ctx_set_params",
            count: "num_params",
            element: "drm_virtgpu_context_set_param",
            size: size_of::<drm_virtgpu_context_set_param>(),
            align: align_of::<drm_virtgpu_context_set_param>(),
            direction: WireDirection::In,
        }],
    };

    /// The payload relibc sends: `pad` as the caller gave it, the parameters
    /// read from the caller's array, then `name` cut to `CONTEXT_NAME_MAX`.
    pub fn wire(pad: u32, params: &[drm_virtgpu_context_set_param], name: &[u8]) -> Vec<u8> {
        let name = &name[..cmp::min(name.len(), CONTEXT_NAME_MAX)];
        let mut data = Vec::with_capacity(
            size_of::<__VirtgpuContextInitNoncounted>() + mem::size_of_val(params) + name.len(),
        );
        data.extend_from_slice(&(params.len() as u32).to_ne_bytes());
        data.extend_from_slice(&pad.to_ne_bytes());
        for param in params {
            data.extend_from_slice(&param.param.to_ne_bytes());
            data.extend_from_slice(&param.value.to_ne_bytes());
        }
        data.extend_from_slice(name);
        data
    }

    /// Splits a payload the driver received. `EINVAL` when it holds fewer
    /// parameters than `num_params` says, or a name longer than
    /// `CONTEXT_NAME_MAX`. Reads every field by value, so the payload may have
    /// any alignment.
    pub fn with(
        buf: &'a [u8],
        f: impl FnOnce(VirtgpuContextInit<'a>) -> syscall::Result<usize>,
    ) -> syscall::Result<usize> {
        let invalid = || syscall::Error::new(syscall::EINVAL);
        let (fixed, rest) = buf
            .split_at_checked(size_of::<__VirtgpuContextInitNoncounted>())
            .ok_or_else(invalid)?;
        let num_params = u32::from_ne_bytes(fixed[..4].try_into().unwrap());
        let len = (num_params as usize)
            .checked_mul(size_of::<drm_virtgpu_context_set_param>())
            .ok_or_else(invalid)?;
        let (params, name) = rest.split_at_checked(len).ok_or_else(invalid)?;
        if name.len() > CONTEXT_NAME_MAX {
            return Err(invalid());
        }
        f(VirtgpuContextInit {
            num_params,
            params,
            name,
        })
    }

    pub fn num_params(&self) -> u32 {
        self.num_params
    }

    pub fn params(&self) -> impl Iterator<Item = drm_virtgpu_context_set_param> + 'a {
        self.params
            .chunks_exact(size_of::<drm_virtgpu_context_set_param>())
            .map(|param| drm_virtgpu_context_set_param {
                param: u64::from_ne_bytes(param[..8].try_into().unwrap()),
                value: u64::from_ne_bytes(param[8..].try_into().unwrap()),
            })
    }

    /// The bytes relibc sent as the context's name, untrusted: any bytes, at
    /// most `CONTEXT_NAME_MAX` of them.
    pub fn name(&self) -> &'a [u8] {
        self.name
    }
}

/// The payload of get-parameter. `value` points at the one `int` Linux
/// writes and has no count, so this payload is written by hand: it holds the
/// parameter and, on return, the answer, and `read_from` writes the answer's
/// low 32 bits through `value`, as Linux writes `sizeof(int)` bytes there.
#[repr(C)]
pub struct __VirtgpuGetparamNoncounted {
    param: u64,
    value: u64,
}

impl IoctlData for drm_virtgpu_getparam {
    unsafe fn write(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(size_of::<__VirtgpuGetparamNoncounted>());
        data.extend_from_slice(&self.param.to_ne_bytes());
        data.extend_from_slice(&0u64.to_ne_bytes());
        data
    }

    unsafe fn read_from(&mut self, buf: &[u8]) {
        let Some(answer) = buf.get(8..16) else {
            return;
        };
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(answer);
        let answer = u64::from_ne_bytes(bytes) as c_int;
        if self.value != 0 {
            unsafe { (self.value as usize as *mut c_int).write_unaligned(answer) };
        }
    }
}

pub struct VirtgpuGetparam<'a> {
    payload: &'a mut [u8; 16],
}

impl<'a> VirtgpuGetparam<'a> {
    pub const LAYOUT: WireLayout = WireLayout {
        fixed_size: size_of::<__VirtgpuGetparamNoncounted>(),
        fixed: &[
            WireField {
                name: "param",
                offset: offset_of!(__VirtgpuGetparamNoncounted, param),
                size: 8,
            },
            WireField {
                name: "value",
                offset: offset_of!(__VirtgpuGetparamNoncounted, value),
                size: 8,
            },
        ],
        arrays: &[],
    };

    pub fn with(
        buf: &'a mut [u8],
        f: impl FnOnce(VirtgpuGetparam<'a>) -> syscall::Result<usize>,
    ) -> syscall::Result<usize> {
        let payload = buf
            .try_into()
            .map_err(|_| syscall::Error::new(syscall::EINVAL))?;
        f(VirtgpuGetparam { payload })
    }

    pub fn param(&self) -> u64 {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&self.payload[..8]);
        u64::from_ne_bytes(bytes)
    }

    pub fn set_value(&mut self, value: c_int) {
        self.payload[8..].copy_from_slice(&u64::from(value as u32).to_ne_bytes());
    }
}

define_ioctl_data! {
    struct drm_gem_close, GemClose {
        handle: u32,
        pad: u32,
    }
}

/// Each request that has a payload: its function number, the name of its
/// structure in Linux's headers, and the payload's layout.
pub const PAYLOADS: &[(u64, &str, &WireLayout)] = &[
    (MAP, "drm_virtgpu_map", &VirtgpuMap::LAYOUT),
    (
        EXECBUFFER,
        "drm_virtgpu_execbuffer",
        &VirtgpuExecbuffer::LAYOUT,
    ),
    (GETPARAM, "drm_virtgpu_getparam", &VirtgpuGetparam::LAYOUT),
    (
        RESOURCE_INFO,
        "drm_virtgpu_resource_info",
        &VirtgpuResourceInfo::LAYOUT,
    ),
    (GET_CAPS, "drm_virtgpu_get_caps", &VirtgpuGetCaps::LAYOUT),
    (
        RESOURCE_CREATE_BLOB,
        "drm_virtgpu_resource_create_blob",
        &VirtgpuResourceCreateBlob::LAYOUT,
    ),
    (
        CONTEXT_INIT,
        "drm_virtgpu_context_init",
        &VirtgpuContextInit::LAYOUT,
    ),
    (GEM_CLOSE, "drm_gem_close", &GemClose::LAYOUT),
];

const _: () = {
    let mut i = 0;
    while i < PAYLOADS.len() {
        assert!(PAYLOADS[i].2.is_packed() && PAYLOADS[i].2.is_aligned());
        i += 1;
    }
};

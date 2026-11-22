use core::slice;

use redox_ioctl::{
    IoctlData,
    drm::*,
    virtgpu::{self, GEM_CLOSE, VirtgpuContextInit, drm_gem_close, drm_virtgpu_context_set_param},
};
use syscall::flag::{F_SETFD, O_RDWR};

use crate::{
    error::{Errno, Result},
    header::{
        errno::{EFAULT, EINVAL},
        fcntl::sys::FD_CLOEXEC,
    },
    platform::{self, types::c_int},
};

use super::IoctlBuffer;

const DRM_FORMAT_ARGB8888: u32 = 0x34325241; // 'AR24' fourcc code, for ARGB8888

fn id_index(id: u32) -> u32 {
    id & 0xFF
}

fn conn_id(i: u32) -> u32 {
    id_index(i) | (1 << 8)
}

fn crtc_id(i: u32) -> u32 {
    id_index(i) | (1 << 9)
}

fn enc_id(i: u32) -> u32 {
    id_index(i) | (1 << 10)
}

fn fb_id(i: u32) -> u32 {
    id_index(i) | (1 << 11)
}

fn fb_handle_id(i: u32) -> u32 {
    id_index(i) | (1 << 12)
}

fn plane_id(i: u32) -> u32 {
    id_index(i) | (1 << 13)
}

unsafe fn copy_array<T: Copy>(src: &[T], dst_ptr: *mut T, dst_len: usize) -> usize {
    let dst = unsafe { slice::from_raw_parts_mut(dst_ptr, dst_len) };
    dst.copy_from_slice(&src[..src.len().min(dst_len)]);
    src.len()
}

struct Dev {
    fd: c_int,
}

impl Dev {
    fn new(fd: c_int) -> Result<Self> {
        //TODO: check display scheme using fpath?
        Ok(Self { fd })
    }

    unsafe fn read_write_ioctl<T: IoctlData>(
        &self,
        mut buf: IoctlBuffer,
        func: u64,
    ) -> Result<c_int> {
        let mut data = unsafe { buf.read::<T>() }?;
        let mut wire = unsafe { data.write() };
        let res = redox_rt::sys::sys_call_rw(
            self.fd as usize,
            &mut wire,
            syscall::CallFlags::empty(),
            &[func],
        )?;
        unsafe { data.read_from(&wire) };
        (unsafe { buf.write(data) })?;
        Ok(res as c_int)
    }

    unsafe fn write_ioctl<T: IoctlData>(&self, buf: IoctlBuffer, func: u64) -> Result<c_int> {
        let data = unsafe { buf.read::<T>() }?;
        let wire = unsafe { data.write() };
        let res = redox_rt::sys::sys_call_wo(
            self.fd as usize,
            &wire,
            syscall::CallFlags::empty(),
            &[func],
        )?;
        Ok(res as c_int)
    }

    /// Context-init. The parameters are read from the caller's array and sent
    /// by value, then the context's name: the string a `DEBUG_NAME` parameter
    /// points at, or else the name the program runs under, as Linux names a
    /// context after the process. See `VirtgpuContextInit`.
    unsafe fn virtgpu_context_init(&self, buf: IoctlBuffer) -> Result<c_int> {
        let init = unsafe { buf.read::<virtgpu::drm_virtgpu_context_init>() }?;
        // Linux refuses more parameters than it knows before it reads any.
        if init.num_params > virtgpu::CONTEXT_INIT_MAX_PARAMS {
            return Err(Errno(EINVAL));
        }
        let count = init.num_params as usize;
        if count != 0 && init.ctx_set_params == 0 {
            return Err(Errno(EFAULT));
        }
        let array = init.ctx_set_params as usize as *const drm_virtgpu_context_set_param;
        let mut params =
            [drm_virtgpu_context_set_param::default(); virtgpu::CONTEXT_INIT_MAX_PARAMS as usize];
        for (i, param) in params[..count].iter_mut().enumerate() {
            *param = unsafe { array.add(i).read_unaligned() };
        }
        let params = &params[..count];

        let mut name = [0u8; virtgpu::CONTEXT_NAME_MAX];
        let source = match params
            .iter()
            .find(|param| param.param == virtgpu::VIRTGPU_CONTEXT_PARAM_DEBUG_NAME)
        {
            Some(param) if param.value == 0 => return Err(Errno(EFAULT)),
            Some(param) => param.value as usize as *const u8,
            None => unsafe { platform::program_invocation_short_name }
                .cast_const()
                .cast::<u8>(),
        };
        let mut len = 0;
        if !source.is_null() {
            while len < name.len() {
                let byte = unsafe { source.add(len).read() };
                if byte == 0 {
                    break;
                }
                name[len] = byte;
                len += 1;
            }
        }

        let wire = VirtgpuContextInit::wire(init.pad, params, &name[..len]);
        let res = redox_rt::sys::sys_call_wo(
            self.fd as usize,
            &wire,
            syscall::CallFlags::empty(),
            &[virtgpu::CONTEXT_INIT],
        )?;
        Ok(res as c_int)
    }

    unsafe fn virtgpu_resource_create_blob(&self, mut buf: IoctlBuffer) -> Result<c_int> {
        let mut data = unsafe { buf.read::<virtgpu::drm_virtgpu_resource_create_blob>() }?;
        if data.cmd_size != 0 && data.cmd == 0 {
            return Err(Errno(EFAULT));
        }
        let mut wire = unsafe { data.write() };
        let res = redox_rt::sys::sys_call_rw(
            self.fd as usize,
            &mut wire,
            syscall::CallFlags::empty(),
            &[virtgpu::RESOURCE_CREATE_BLOB],
        )?;
        unsafe { data.read_from(&wire) };
        (unsafe { buf.write(data) })?;
        Ok(res as c_int)
    }

    unsafe fn virtgpu_execbuffer(&self, mut buf: IoctlBuffer) -> Result<c_int> {
        let mut data = unsafe { buf.read::<virtgpu::drm_virtgpu_execbuffer>() }?;
        if data.num_in_syncobjs != 0 || data.num_out_syncobjs != 0 {
            if data.syncobj_stride
                != core::mem::size_of::<virtgpu::drm_virtgpu_execbuffer_syncobj>() as u32
            {
                return Err(Errno(EINVAL));
            }
            if data.num_in_syncobjs > virtgpu::EXECBUF_MAX_SYNCOBJS
                || data.num_out_syncobjs > virtgpu::EXECBUF_MAX_SYNCOBJS
            {
                return Err(Errno(EINVAL));
            }
            if (data.num_in_syncobjs != 0 && data.in_syncobjs == 0)
                || (data.num_out_syncobjs != 0 && data.out_syncobjs == 0)
            {
                return Err(Errno(EFAULT));
            }
        }
        if data.num_bo_handles > virtgpu::EXECBUF_MAX_BO_HANDLES {
            return Err(Errno(EINVAL));
        }
        if data.num_bo_handles != 0 && data.bo_handles == 0 {
            return Err(Errno(EFAULT));
        }
        if data.size > virtgpu::EXECBUF_MAX_CMD_SIZE {
            return Err(Errno(EINVAL));
        }
        if data.size != 0 && data.command == 0 {
            return Err(Errno(EFAULT));
        }

        let mut wire = unsafe { data.write() };
        let res = if (data.flags & virtgpu::VIRTGPU_EXECBUF_FENCE_FD_IN) != 0 {
            if data.fence_fd < 0 {
                return Err(Errno(EINVAL));
            }
            redox_rt::sys::sys_call_rw(
                &[self.fd as usize, data.fence_fd as usize][..],
                &mut wire,
                syscall::CallFlags::empty(),
                &[virtgpu::EXECBUFFER],
            )
            .map_err(super::sync::map_two_fd_err)?
        } else {
            redox_rt::sys::sys_call_rw(
                self.fd as usize,
                &mut wire,
                syscall::CallFlags::empty(),
                &[virtgpu::EXECBUFFER],
            )?
        };
        unsafe { data.read_from(&wire) };

        if (data.flags & virtgpu::VIRTGPU_EXECBUF_FENCE_FD_OUT) != 0 {
            let mut metadata = [0u8; 16];
            metadata[0..8].copy_from_slice(&virtgpu::FENCE_DUP_EXECBUFFER.to_ne_bytes());
            metadata[8..12].copy_from_slice(&(data.fence_fd as u32).to_ne_bytes());
            metadata[12..16].copy_from_slice(&0u32.to_ne_bytes());
            let new_fd = redox_rt::sys::dup(self.fd as usize, &metadata)?;
            if let Err(err) = redox_rt::sys::fcntl(new_fd, F_SETFD, FD_CLOEXEC as usize) {
                let _ = redox_rt::sys::close(new_fd);
                return Err(err.into());
            }
            data.fence_fd = new_fd as c_int;
        }

        unsafe { buf.write(data) }?;
        Ok(res as c_int)
    }

    unsafe fn prime_handle_to_fd(&self, mut buf: IoctlBuffer) -> Result<c_int> {
        let mut data = unsafe { buf.read::<drm_prime_handle>() }?;
        let writable = (data.flags & O_RDWR as u32) != 0;
        let mut metadata = [0u8; 16];
        metadata[0..8].copy_from_slice(&PRIME_HANDLE_TO_FD.to_ne_bytes());
        metadata[8..12].copy_from_slice(&data.handle.to_ne_bytes());
        metadata[12..16].copy_from_slice(&(if writable { 1u32 } else { 0u32 }).to_ne_bytes());
        let new_fd = redox_rt::sys::dup(self.fd as usize, &metadata)?;
        if (data.flags & FD_CLOEXEC as u32) != 0 {
            redox_rt::sys::fcntl(new_fd as usize, F_SETFD, FD_CLOEXEC as usize)?;
        }
        data.fd = new_fd as i32;
        unsafe { buf.write(data) }?;
        Ok(0)
    }

    unsafe fn prime_fd_to_handle(&self, mut buf: IoctlBuffer) -> Result<c_int> {
        let mut data = unsafe { buf.read::<drm_prime_handle>() }?;
        let metadata = [PRIME_FD_TO_HANDLE];
        let handle = redox_rt::sys::sys_call_wo(
            &[self.fd as usize, data.fd as usize][..],
            &[],
            syscall::CallFlags::empty(),
            &metadata,
        )?;
        data.handle = handle as u32;
        unsafe { buf.write(data) }?;
        Ok(0)
    }
}

pub(super) unsafe fn ioctl(fd: c_int, func: u8, buf: IoctlBuffer) -> Result<c_int> {
    let dev = Dev::new(fd)?;
    match func {
        0x00 => unsafe { dev.read_write_ioctl::<drm_version>(buf, VERSION) },
        0x01 => unsafe { dev.read_write_ioctl::<drm_unique>(buf, GET_UNIQUE) },
        0x07 => unsafe { dev.read_write_ioctl::<drm_set_version>(buf, SET_VERSION) },
        0x09 => unsafe { dev.write_ioctl::<drm_gem_close>(buf, GEM_CLOSE) },
        0x0C => unsafe { dev.read_write_ioctl::<drm_get_cap>(buf, GET_CAP) },
        0x0D => unsafe { dev.write_ioctl::<drm_set_client_cap>(buf, SET_CLIENT_CAP) },
        0x2D => unsafe { dev.prime_handle_to_fd(buf) },
        0x2E => unsafe { dev.prime_fd_to_handle(buf) },
        0x3A => unsafe { dev.read_write_ioctl::<drm_wait_vblank>(buf, WAIT_VBLANK) },
        0x41 => unsafe { dev.read_write_ioctl::<virtgpu::drm_virtgpu_map>(buf, virtgpu::MAP) },
        0x42 => unsafe { dev.virtgpu_execbuffer(buf) },
        0x43 => unsafe {
            dev.read_write_ioctl::<virtgpu::drm_virtgpu_getparam>(buf, virtgpu::GETPARAM)
        },
        0x45 => unsafe {
            dev.read_write_ioctl::<virtgpu::drm_virtgpu_resource_info>(buf, virtgpu::RESOURCE_INFO)
        },
        0x48 => unsafe {
            dev.read_write_ioctl::<virtgpu::drm_virtgpu_3d_wait>(buf, virtgpu::WAIT)
        },
        0x49 => unsafe {
            dev.read_write_ioctl::<virtgpu::drm_virtgpu_get_caps>(buf, virtgpu::GET_CAPS)
        },
        0x4A => unsafe { dev.virtgpu_resource_create_blob(buf) },
        0x4B => unsafe { dev.virtgpu_context_init(buf) },
        0xA0 => unsafe { dev.read_write_ioctl::<drm_mode_card_res>(buf, MODE_CARD_RES) },
        0xA1 => unsafe { dev.read_write_ioctl::<drm_mode_crtc>(buf, MODE_GET_CRTC) },
        0xA2 => unsafe { dev.read_write_ioctl::<drm_mode_crtc>(buf, MODE_SET_CRTC) },
        0xA3 => unsafe { dev.write_ioctl::<drm_mode_cursor>(buf, MODE_CURSOR) },
        0xA4 => unsafe { dev.write_ioctl::<drm_mode_crtc_lut>(buf, MODE_GETGAMMA) },
        0xA5 => unsafe { dev.write_ioctl::<drm_mode_crtc_lut>(buf, MODE_SETGAMMA) },
        0xA6 => unsafe { dev.read_write_ioctl::<drm_mode_get_encoder>(buf, MODE_GET_ENCODER) },
        0xA7 => unsafe { dev.read_write_ioctl::<drm_mode_get_connector>(buf, MODE_GET_CONNECTOR) },
        0xAA => unsafe { dev.read_write_ioctl::<drm_mode_get_property>(buf, MODE_GET_PROPERTY) },
        0xAB => unsafe {
            dev.read_write_ioctl::<drm_mode_connector_set_property>(buf, MODE_SET_PROPERTY)
        },
        0xAC => unsafe { dev.read_write_ioctl::<drm_mode_get_blob>(buf, MODE_GET_PROP_BLOB) },
        0xAD => unsafe { dev.read_write_ioctl::<drm_mode_fb_cmd>(buf, MODE_GET_FB) },
        0xAE => unsafe { dev.read_write_ioctl::<drm_mode_fb_cmd>(buf, MODE_ADD_FB) },
        0xAF => unsafe { dev.read_write_ioctl::<standin_for_uint>(buf, MODE_RM_FB) },
        0xB0 => unsafe {
            dev.read_write_ioctl::<drm_mode_crtc_page_flip_target>(buf, MODE_PAGE_FLIP)
        },
        0xB1 => unsafe { dev.read_write_ioctl::<drm_mode_fb_dirty_cmd>(buf, MODE_DIRTYFB) },
        0xB2 => unsafe { dev.read_write_ioctl::<drm_mode_create_dumb>(buf, MODE_CREATE_DUMB) },
        0xB3 => unsafe { dev.read_write_ioctl::<drm_mode_map_dumb>(buf, MODE_MAP_DUMB) },
        0xB4 => unsafe { dev.read_write_ioctl::<drm_mode_destroy_dumb>(buf, MODE_DESTROY_DUMB) },
        0xB5 => unsafe { dev.read_write_ioctl::<drm_mode_get_plane_res>(buf, MODE_GET_PLANE_RES) },
        0xB6 => unsafe { dev.read_write_ioctl::<drm_mode_get_plane>(buf, MODE_GET_PLANE) },
        0xB7 => unsafe { dev.read_write_ioctl::<drm_mode_set_plane>(buf, MODE_SET_PLANE) },
        0xB8 => unsafe { dev.read_write_ioctl::<drm_mode_fb_cmd2>(buf, MODE_ADD_FB2) },
        0xB9 => unsafe {
            dev.read_write_ioctl::<drm_mode_obj_get_properties>(buf, MODE_OBJ_GET_PROPERTIES)
        },
        0xBB => unsafe { dev.write_ioctl::<drm_mode_cursor2>(buf, MODE_CURSOR2) },
        0xBF..=0xC5 | 0xCA..=0xCD | 0xCF => unsafe { super::sync::drm_syncobj_ioctl(fd, func, buf) },
        0xCE => unsafe { dev.read_write_ioctl::<drm_mode_fb_cmd2>(buf, MODE_GET_FB2) },
        0xD0 => unsafe { dev.read_write_ioctl::<drm_mode_closefb>(buf, MODE_CLOSE_FB) },
        0xD1 => unsafe { dev.write_ioctl::<drm_set_client_name>(buf, SET_CLIENT_NAME) },
        _ => {
            todo_skip!(
                0,
                "unimplemented DRM ioctl({}, 0x{:02x}, {:?})",
                fd,
                func,
                buf
            );
            Err(Errno(EINVAL))
        }
    }
}

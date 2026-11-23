use core::{
    cell::SyncUnsafeCell,
    cmp,
    fmt::Debug,
    ops::Range,
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

use crate::{
    DYNAMIC_PROC_INFO, RtTcb, StaticProcInfo,
    arch::*,
    auxv_defs::*,
    read_proc_meta,
    sys::{FdTblInner, fstat, open, sys_call, sys_call_wo},
};
use redox_protocols::protocol::{O_CLOEXEC, ProcCall, ThreadCall};
#[cfg(feature = "proc")]
use redox_protocols::protocol::{ProcKillTarget, SIGKILL, WaitFlags};

use alloc::{boxed::Box, vec};

use goblin::elf::header::ET_DYN;
//TODO: allow use of either 32-bit or 64-bit programs
#[cfg(target_pointer_width = "32")]
use goblin::elf32::{
    header::Header,
    program_header::program_header32::{PF_R, PF_W, PF_X, PT_INTERP, PT_LOAD, ProgramHeader},
};
#[cfg(target_pointer_width = "64")]
use goblin::elf64::{
    header::Header,
    program_header::program_header64::{PF_R, PF_W, PF_X, PT_INTERP, PT_LOAD, ProgramHeader},
};

use syscall::{
    CallFlags, GrantDesc, GrantFlags, MAP_FIXED_NOREPLACE, MAP_SHARED, Map, PAGE_SIZE, PROT_EXEC,
    PROT_READ, PROT_WRITE, SetSighandlerData,
    error::*,
    flag::{MapFlags, SEEK_SET},
};

pub enum FexecResult {
    Interp {
        path: Box<[u8]>,
        interp_override: InterpOverride,
    },
}
pub struct InterpOverride {
    phdrs_vaddr: usize,
    at_entry: usize,
    at_phnum: usize,
    at_phent: usize,
    name: Box<[u8]>,
    min_mmap_addr: usize,
    grants_fd: usize,
}

pub struct ExtraInfo<'a> {
    pub cwd: Option<&'a [u8]>,
    // POSIX states that while sigactions are reset, ignored sigactions will remain ignored.
    pub sigignmask: u64,
    // POSIX also states that the sigprocmask must be preserved across execs.
    pub sigprocmask: u64,
    /// File mode creation mask (POSIX)
    pub umask: u32,
    /// Thread handle
    pub thr_fd: usize,
    /// Process handle
    pub proc_fd: usize,
    /// Namespace handle
    pub ns_fd: Option<usize>,
    /// CWD handle
    pub cwd_fd: Option<usize>,
    /// Filetable handle
    pub filetable_fd: Option<usize>,
    /// If the process for which the image is to be loaded the same as the currently running process
    pub same_process: bool,
}

pub const AT_RANDOM_BYTES: usize = 16;
const EXEC_RANDOM_MAX_BYTES: usize = 256;
const EXEC_RANDOM_MAX_READS: usize = 64;

pub fn exec_random_bytes(buf: &mut [u8]) -> Result<()> {
    if buf.is_empty() {
        return Ok(());
    }
    if buf.len() > EXEC_RANDOM_MAX_BYTES {
        return Err(Error::new(EINVAL));
    }
    let rand_fd = FdGuard::open_into_upper("/scheme/rand", syscall::O_RDONLY | O_CLOEXEC)?;
    let mut filled = 0;
    for _ in 0..EXEC_RANDOM_MAX_READS {
        if filled == buf.len() {
            return Ok(());
        }
        match rand_fd.read(&mut buf[filled..]) {
            Ok(0) => return Err(Error::new(EIO)),
            Ok(n) => {
                assert!(n <= buf.len() - filled);
                filled += n;
            }
            Err(Error { errno: EINTR }) => continue,
            Err(err) => return Err(err),
        }
    }
    if filled == buf.len() {
        Ok(())
    } else {
        Err(Error::new(EIO))
    }
}

#[cfg(feature = "proc")]
fn is_early_boot_spawner() -> bool {
    let resugid = crate::sys::posix_getresugid();
    if resugid.ruid != 0 || resugid.euid != 0 {
        return false;
    }
    let pid = crate::sys::posix_getpid();
    pid == 1 || crate::sys::posix_getppid() == 1
}

#[cfg(not(feature = "proc"))]
fn is_early_boot_spawner() -> bool {
    true
}

#[expect(clippy::too_many_arguments)]
pub fn fexec_impl(
    image_file: FdGuardUpper,
    thread_fd: &FdGuardUpper,
    proc_fd: &FdGuardUpper,
    path: &[u8],
    args: &[&[u8]],
    envs: &[&[u8]],
    extrainfo: &ExtraInfo,
    interp_override: Option<InterpOverride>,
) -> Result<Option<FexecResult>> {
    // Here, we do the minimum part of loading an application, which is what the kernel used to do.
    // We load the executable into memory (albeit at different offsets in this executable), fix
    // some misalignments, and then switch address space.

    let mut header_bytes = [0_u8; size_of::<Header>()];
    pread_all(&image_file, 0, &mut header_bytes)?;
    let header = Header::from_bytes(&header_bytes);

    if header.e_ident[..4] != [0x7F, 0x45, 0x4C, 0x46] {
        // Not an ELF, according to posix_spawn() just throw error here
        return Err(Error::new(ENOEXEC));
    }

    let grants_fd = if let Some(interp) = interp_override.as_ref() {
        FdGuard::new(interp.grants_fd).to_upper()?
    } else {
        let current_addrspace_fd = thread_fd.dup_into_upper(b"addrspace")?;
        current_addrspace_fd.dup_into_upper(b"empty")?
    };
    grants_fd.fcntl(redox_protocols::protocol::F_SETFD, O_CLOEXEC)?;

    // Never allow more than 1 MiB of program headers.
    const MAX_PH_SIZE: usize = 1024 * 1024;
    let phentsize = u64::from(header.e_phentsize) as usize;
    let phnum = u64::from(header.e_phnum) as usize;
    let pheaders_size = phentsize
        .saturating_mul(phnum)
        .saturating_add(size_of::<Header>());

    if pheaders_size > MAX_PH_SIZE {
        return Err(Error::new(E2BIG));
    }
    let mut phs_raw = vec![0_u8; pheaders_size];
    phs_raw[..size_of::<Header>()].copy_from_slice(&header_bytes);
    let phs = &mut phs_raw[size_of::<Header>()..];

    // TODO: Remove clone, but this would require more as_refs and as_muts
    let mut min_mmap_addr = interp_override
        .as_ref()
        .map(|interp| interp.min_mmap_addr)
        .unwrap_or(PAGE_SIZE);
    let mut update_min_mmap_addr = |addr: usize, size: usize| {
        min_mmap_addr = cmp::max(min_mmap_addr, (addr + size).next_multiple_of(PAGE_SIZE));
    };

    #[cfg(target_pointer_width = "64")]
    let header_e_phoff = header.e_phoff;
    #[cfg(target_pointer_width = "32")]
    let header_e_phoff = u64::from(header.e_phoff);
    pread_all(&image_file, header_e_phoff, phs).map_err(|_| Error::new(EIO))?;

    let mut span: Option<Range<usize>> = None;
    for ph_idx in 0..phnum {
        let ph_bytes = &phs[ph_idx * phentsize..(ph_idx + 1) * phentsize];
        let segment: &ProgramHeader =
            plain::from_bytes(ph_bytes).map_err(|_| Error::new(EINVAL))?;
        if segment.p_type != PT_LOAD {
            continue;
        }

        let voff = segment.p_vaddr as usize % PAGE_SIZE;
        let vaddr = segment.p_vaddr as usize - voff;
        let vsize = (segment.p_memsz as usize + voff).next_multiple_of(segment.p_align as usize);
        let b = vaddr..vaddr + vsize;

        span = Some(if let Some(a) = span {
            a.start.min(b.start)..a.end.max(b.end)
        } else {
            b
        });
    }
    let span = span.expect("ELF executables must contain at least one `PT_LOAD` segment");
    let span_size = (span.end - span.start).next_multiple_of(PAGE_SIZE);

    let base_addr = if header.e_type == ET_DYN {
        // PIE
        let addr = mmap_anon_remote(&grants_fd, 0, 0, span_size, MapFlags::PROT_NONE)?;
        update_min_mmap_addr(addr, span_size);
        addr
    } else {
        mmap_anon_remote(
            &grants_fd,
            0,
            span.start,
            span_size,
            MapFlags::MAP_FIXED_NOREPLACE,
        )?;
        update_min_mmap_addr(span.start, span_size);
        0
    };

    let mut phdrs_vaddr = 0;
    let mut interpreter = None;

    for ph_idx in 0..phnum {
        let ph_bytes = &phs[ph_idx * phentsize..(ph_idx + 1) * phentsize];
        let segment: &ProgramHeader =
            plain::from_bytes(ph_bytes).map_err(|_| Error::new(EINVAL))?;
        let mut flags = if segment.p_flags & PF_R == PF_R {
            syscall::PROT_READ
        } else {
            syscall::PROT_NONE
        };

        // W ^ X. If it is executable, do not allow it to be writable, even if requested
        if segment.p_flags & PF_X == PF_X {
            flags |= syscall::PROT_EXEC;
        } else if segment.p_flags & PF_W == PF_W {
            flags |= syscall::PROT_WRITE;
        }

        match segment.p_type {
            // PT_INTERP must come before any PT_LOAD, so we don't have to iterate twice.
            PT_INTERP => {
                let mut interp = vec![0_u8; segment.p_filesz as usize];
                #[cfg(target_pointer_width = "64")]
                let segment_p_offset = segment.p_offset;
                #[cfg(target_pointer_width = "32")]
                let segment_p_offset = u64::from(segment.p_offset);
                pread_all(&image_file, segment_p_offset, &mut interp)?;

                interpreter = Some(interp.into_boxed_slice());
            }
            PT_LOAD => {
                let voff = segment.p_vaddr as usize % PAGE_SIZE;
                let vaddr = segment.p_vaddr as usize - voff;
                let filesz = segment.p_filesz as usize;

                let total_page_count = (segment.p_memsz as usize + voff).div_ceil(PAGE_SIZE);

                // The case where segments overlap so that they share one page, is not handled.
                // TODO: Should it be?

                if segment.p_filesz > segment.p_memsz {
                    return Err(Error::new(ENOEXEC));
                }

                mmap_anon_remote(
                    &grants_fd,
                    0,
                    base_addr + vaddr,
                    total_page_count * PAGE_SIZE,
                    flags | MapFlags::MAP_FIXED,
                )?;

                if segment.p_offset <= header.e_phoff
                    && header.e_phoff < segment.p_offset + segment.p_filesz
                {
                    phdrs_vaddr =
                        (header.e_phoff - segment.p_offset + segment.p_vaddr) as usize + base_addr;
                }

                // TODO: Attempt to mmap with MAP_PRIVATE directly from the image file instead.

                if filesz > 0 {
                    let (_guard, dst_memory) = unsafe {
                        MmapGuard::map_mut_anywhere(
                            &grants_fd,
                            base_addr + vaddr,                           // offset
                            (voff + filesz).next_multiple_of(PAGE_SIZE), // size
                        )?
                    };
                    #[cfg(target_pointer_width = "64")]
                    let segment_p_offset = segment.p_offset;
                    #[cfg(target_pointer_width = "32")]
                    let segment_p_offset = u64::from(segment.p_offset);
                    pread_all(
                        &image_file,
                        segment_p_offset,
                        &mut dst_memory[voff..voff + filesz],
                    )?;
                }
            }
            _ => continue,
        }
    }

    if let Some(interpreter_path) = interpreter {
        return Ok(Some(FexecResult::Interp {
            path: interpreter_path,
            interp_override: InterpOverride {
                at_entry: base_addr + header.e_entry as usize,
                at_phnum: phnum,
                at_phent: phentsize,
                phdrs_vaddr,
                name: path.into(),
                min_mmap_addr,
                grants_fd: grants_fd.take(),
            },
        }));
    }

    mmap_anon_remote(
        &grants_fd,
        0,
        STACK_TOP - STACK_SIZE,
        STACK_SIZE,
        MapFlags::PROT_READ | MapFlags::PROT_WRITE | MapFlags::MAP_FIXED_NOREPLACE,
    )?;

    let mut sp = STACK_TOP;
    let mut stack_page = Option::<MmapGuard>::None;

    let mut push = |word: usize| -> Result<()> {
        let old_page_no = sp / PAGE_SIZE;
        sp -= size_of::<usize>();
        let new_page_no = sp / PAGE_SIZE;
        let new_page_off = sp % PAGE_SIZE;

        let page = if let Some(ref mut page) = stack_page
            && old_page_no == new_page_no
        {
            page
        } else if let Some(ref mut stack_page) = stack_page {
            stack_page.remap(new_page_no * PAGE_SIZE, PROT_READ | PROT_WRITE)?;
            stack_page
        } else {
            let new = MmapGuard::map(
                &grants_fd,
                &Map {
                    offset: new_page_no * PAGE_SIZE,
                    size: PAGE_SIZE,
                    flags: PROT_READ | PROT_WRITE,
                    address: 0, // let kernel decide
                },
            )?;

            stack_page.insert(new)
        };

        unsafe {
            page.as_mut_ptr_slice()
                .as_mut_ptr()
                .add(new_page_off)
                .cast::<usize>()
                .write(word);
        }

        Ok(())
    };

    push(0)?;
    push(AT_NULL)?;
    if let Some(ref r#override) = interp_override {
        push(r#override.at_entry)?;
        push(AT_ENTRY)?;
        push(base_addr)?;
        push(AT_BASE)?;
        push(r#override.phdrs_vaddr)?;
        push(AT_PHDR)?;
    } else {
        push(base_addr + header.e_entry as usize)?;
        push(AT_ENTRY)?;
        push(phdrs_vaddr)?;
        push(AT_PHDR)?;
    }
    push(
        interp_override
            .as_ref()
            .map_or(header.e_phnum as usize, |o| o.at_phnum),
    )?;
    push(AT_PHNUM)?;
    push(
        interp_override
            .as_ref()
            .map_or(header.e_phentsize as usize, |o| o.at_phent),
    )?;
    push(AT_PHENT)?;

    let mut random_bytes = [0_u8; AT_RANDOM_BYTES];
    let has_random = match exec_random_bytes(&mut random_bytes) {
        Ok(()) => true,
        Err(err)
            if (err == Error::new(ENODEV) || cfg!(not(feature = "proc")))
                && is_early_boot_spawner() =>
        {
            false
        }
        Err(err) => return Err(err),
    };

    let total_args_envs_auxvpointee_size = args.iter().map(|arg| arg.len() + 1).sum::<usize>()
        + envs.iter().map(|env| env.len() + 1).sum::<usize>()
        + extrainfo.cwd.map_or(0, |s| s.len() + 1)
        + if has_random { AT_RANDOM_BYTES + 1 } else { 0 };
    let args_envs_size_aligned = total_args_envs_auxvpointee_size.next_multiple_of(PAGE_SIZE);
    let target_args_env_address = mmap_anon_remote(
        &grants_fd,
        0,
        0,
        args_envs_size_aligned,
        MapFlags::PROT_READ | MapFlags::PROT_WRITE,
    )?;
    update_min_mmap_addr(target_args_env_address, args_envs_size_aligned);

    let mut offset = 0;

    let mut argc = 0;

    {
        let mut append = |source_slice: &[u8]| -> Result<usize> {
            // TODO
            let address = target_args_env_address + offset;

            if !source_slice.is_empty() {
                let containing_page = address.div_floor(PAGE_SIZE) * PAGE_SIZE;
                let displacement = address - containing_page;
                let size = source_slice.len() + displacement;
                let aligned_size = size.next_multiple_of(PAGE_SIZE);

                let (_guard, memory) = unsafe {
                    MmapGuard::map_mut_anywhere(&grants_fd, containing_page, aligned_size)?
                };
                memory[displacement..][..source_slice.len()].copy_from_slice(source_slice);
            }

            offset += source_slice.len() + 1;
            Ok(address)
        };

        if has_random {
            push(append(&random_bytes)?)?;
            push(AT_RANDOM)?;
        }

        if let Some(cwd) = extrainfo.cwd {
            push(append(cwd)?)?;
            push(AT_REDOX_INITIAL_CWD_PTR)?;
            push(cwd.len())?;
            push(AT_REDOX_INITIAL_CWD_LEN)?;
        }

        #[cfg(target_pointer_width = "32")]
        {
            push((extrainfo.sigignmask >> 32) as usize)?;
            push(AT_REDOX_INHERITED_SIGIGNMASK_HI)?;
        }
        push(extrainfo.sigignmask as usize)?;
        push(AT_REDOX_INHERITED_SIGIGNMASK)?;
        #[cfg(target_pointer_width = "32")]
        {
            push((extrainfo.sigprocmask >> 32) as usize)?;
            push(AT_REDOX_INHERITED_SIGPROCMASK_HI)?;
        }
        push(extrainfo.sigprocmask as usize)?;
        push(AT_REDOX_INHERITED_SIGPROCMASK)?;

        push(extrainfo.umask as usize)?;
        push(AT_REDOX_UMASK)?;

        push(extrainfo.thr_fd)?;
        push(AT_REDOX_THR_FD)?;
        push(extrainfo.proc_fd)?;
        push(AT_REDOX_PROC_FD)?;
        push(extrainfo.ns_fd.unwrap_or(usize::MAX))?;
        push(AT_REDOX_NS_FD)?;
        push(extrainfo.cwd_fd.unwrap_or(usize::MAX))?;
        push(AT_REDOX_CWD_FD)?;
        push(extrainfo.filetable_fd.unwrap_or(usize::MAX))?;
        push(AT_REDOX_FILETABLE_FD)?;

        push(0)?;

        for env in envs.iter().rev() {
            push(append(env)?)?;
        }

        push(0)?;

        for arg in args.iter().rev() {
            push(append(arg)?)?;
            argc += 1;
        }
    }

    push(argc)?;

    if let Ok(sighandler_fd) = thread_fd.dup_into_upper(b"sighandler") {
        let _ = sighandler_fd.write(&SetSighandlerData {
            user_handler: 0,
            excp_handler: 0,
            thread_control_addr: 0,
            proc_control_addr: 0,
        });
        // TODO: sync with procmgr
    }

    // TODO: Restore old name if exec failed?
    {
        let mut buf = [0; 32];
        let new_name = interp_override.as_ref().map_or(path, |o| &o.name);
        let len = new_name.len().min(32);
        buf[..len].copy_from_slice(&new_name[..len]);
        let _ = sys_call_wo(
            proc_fd.as_raw_fd(),
            &buf,
            CallFlags::empty(),
            &[ProcCall::Rename as u64],
        );
    }

    // TODO: Error handling
    let _ = sys_call(
        proc_fd.as_raw_fd(),
        CallFlags::empty(),
        &[ProcCall::DisableSetpgid as u64],
    );

    if interp_override.is_some() {
        let mmap_min_fd = grants_fd.dup_into_upper(b"mmap-min-addr")?;
        let _ = mmap_min_fd.write(&usize::to_ne_bytes(min_mmap_addr));
    }

    let filetable_fd = if extrainfo.same_process {
        Some(thread_fd.dup_into_upper(b"filetable-binary")?)
    } else {
        None
    };

    let addrspace_selection_fd = thread_fd.dup_into_upper(b"current-addrspace")?;

    let _ = addrspace_selection_fd.write(&create_set_addr_space_buf(
        grants_fd.as_raw_fd(),
        base_addr + header.e_entry as usize,
        sp,
    ));

    // Close all O_CLOEXEC file descriptors. TODO: close_range?
    if extrainfo.same_process {
        // NOTE: This approach of implementing O_CLOEXEC will not work in multithreaded
        // scenarios. While execve() is undefined according to POSIX if there exist sibling
        // threads, it could still be allowed by keeping certain file descriptors and instead
        // set the active file table.
        let fail_closed = || -> ! {
            #[cfg(feature = "proc")]
            crate::sys::posix_exit(127);
            #[cfg(not(feature = "proc"))]
            core::intrinsics::abort();
        };
        let _siglock = crate::signal::tmp_disable_signals();
        let filetable_fd = filetable_fd.unwrap();
        {
            let fdtbl = crate::current_filetable();
            let mut guard = fdtbl.inner.lock();
            let fd_guard = fdtbl.fd.lock();
            let keep_upper = [
                extrainfo.thr_fd,
                extrainfo.proc_fd,
                extrainfo.ns_fd.unwrap_or(usize::MAX),
                extrainfo.cwd_fd.unwrap_or(usize::MAX),
                extrainfo.filetable_fd.unwrap_or(usize::MAX),
                addrspace_selection_fd.as_raw_fd(),
                filetable_fd.as_raw_fd(),
                fd_guard.as_ref().map_or(usize::MAX, |g| g.as_raw_fd()),
            ];
            let mut fds_to_close = alloc::vec::Vec::new();
            for (fd, flags) in guard.iter() {
                if keep_upper.contains(&fd) {
                    continue;
                }
                if flags & O_CLOEXEC == O_CLOEXEC
                    || fd == image_file.as_raw_fd()
                    || FdTblInner::is_upper(fd)
                {
                    fds_to_close.push(fd);
                }
            }
            if collect_upper_fds_to_close(&filetable_fd, &keep_upper, &mut fds_to_close).is_err() {
                fail_closed();
            }
            if !fds_to_close.is_empty() {
                let fds_to_close_bytes: &[u8] = unsafe {
                    core::slice::from_raw_parts(
                        fds_to_close.as_ptr() as *const u8,
                        fds_to_close.len() * core::mem::size_of::<usize>(),
                    )
                };
                if filetable_fd
                    .call_wo(
                        fds_to_close_bytes,
                        CallFlags::empty(),
                        &[syscall::FileTableVerb::Close as u64],
                    )
                    .is_err()
                {
                    assert!(fds_to_close.len() <= FdTblInner::CONTEXT_MAX_FILES as usize);
                    for &fd in &fds_to_close {
                        match unsafe { syscall::syscall1(syscall::SYS_CLOSE, fd) } {
                            Ok(_) | Err(Error { errno: EBADF }) => {}
                            Err(_) => fail_closed(),
                        }
                    }
                }
                for fd in fds_to_close {
                    let _ = guard.remove(fd);
                }
            }
        }
        drop(filetable_fd);

        unsafe {
            deactivate_tcb(thread_fd)?;
        }

        drop(crate::current_filetable().take());

        // Dropping this FD will cause the address space switch.
        drop(addrspace_selection_fd);
        unreachable!();
    } else {
        Ok(None)
    }
}

pub fn collect_upper_fds_to_close(
    filetable_fd: &FdGuardUpper,
    keep: &[usize],
    out: &mut alloc::vec::Vec<usize>,
) -> Result<()> {
    filetable_fd.call_wo(
        &[],
        CallFlags::empty(),
        &[syscall::FileTableVerb::Refresh as u64],
    )?;
    let _ = filetable_fd.lseek(0, syscall::SEEK_SET)?;
    let mut reader = FileBufReader::from_fd(filetable_fd.as_raw_fd());
    let mut seen = 0usize;
    for _ in 0..(FdTblInner::CONTEXT_MAX_FILES as usize) {
        let Some(raw_fd) = reader.read_le_u64()? else {
            break;
        };
        seen += 1;
        let fd = raw_fd as usize;
        if FdTblInner::is_upper(fd) && !keep.contains(&fd) && !out.contains(&fd) {
            out.push(fd);
        }
    }
    if seen == FdTblInner::CONTEXT_MAX_FILES as usize && reader.read_le_u64()?.is_some() {
        return Err(Error::new(EMFILE));
    }
    Ok(())
}
fn write_usizes<const N: usize>(fd: &FdGuardUpper, usizes: [usize; N]) -> Result<usize> {
    fd.write(unsafe { plain::as_bytes(&usizes) })
}
pub fn mmap_remote(
    addrspace_fd: &FdGuardUpper,
    fd: &FdGuardUpper,
    offset: usize,
    dst_addr: usize,
    len: usize,
    flags: MapFlags,
) -> Result<usize> {
    write_usizes(
        addrspace_fd,
        [
            // op
            syscall::flag::ADDRSPACE_OP_MMAP,
            // fd
            fd.as_raw_fd(),
            // "offset"
            offset,
            // address
            dst_addr,
            // size
            len,
            // flags
            flags.bits(),
        ],
    )
}
pub fn mmap_anon_remote(
    addrspace_fd: &FdGuardUpper,
    offset: usize,
    dst_addr: usize,
    len: usize,
    flags: MapFlags,
) -> Result<usize> {
    write_usizes(
        addrspace_fd,
        [
            // op
            syscall::flag::ADDRSPACE_OP_MMAP,
            // fd
            !0,
            // "offset"
            offset,
            // address
            dst_addr,
            // size
            len,
            // flags
            flags.bits(),
        ],
    )
}
pub fn mprotect_remote(
    addrspace_fd: &FdGuardUpper,
    addr: usize,
    len: usize,
    flags: MapFlags,
) -> Result<()> {
    write_usizes(
        addrspace_fd,
        [
            // op
            syscall::flag::ADDRSPACE_OP_MPROTECT,
            // address
            addr,
            // size
            len,
            // flags
            flags.bits(),
        ],
    )?;
    Ok(())
}
pub fn munmap_remote(addrspace_fd: &FdGuardUpper, addr: usize, len: usize) -> Result<()> {
    write_usizes(
        addrspace_fd,
        [
            // op
            syscall::flag::ADDRSPACE_OP_MUNMAP,
            // address
            addr,
            // size
            len,
        ],
    )?;
    Ok(())
}
pub fn munmap_transfer(
    src: &FdGuardUpper,
    dst: &FdGuardUpper,
    src_addr: usize,
    dst_addr: usize,
    len: usize,
    flags: MapFlags,
) -> Result<()> {
    write_usizes(
        dst,
        [
            // op
            syscall::flag::ADDRSPACE_OP_TRANSFER,
            // fd
            src.as_raw_fd(),
            // "offset" (source address)
            src_addr,
            // address
            dst_addr,
            // size
            len,
            // flags
            (flags | MapFlags::MAP_FIXED_NOREPLACE).bits(),
        ],
    )?;
    Ok(())
}
fn pread_all(fd: &FdGuardUpper, offset: u64, buf: &mut [u8]) -> Result<()> {
    fd.lseek(offset as isize, SEEK_SET)?;

    let mut total_bytes_read = 0;

    while total_bytes_read < buf.len() {
        total_bytes_read += match fd.read(&mut buf[total_bytes_read..])? {
            0 => return Err(Error::new(ENOEXEC)),
            bytes_read => bytes_read,
        }
    }
    Ok(())
}

pub struct MmapGuard<'a> {
    fd: &'a FdGuardUpper,
    base: usize,
    size: usize,
}
impl<'a> MmapGuard<'a> {
    pub fn map(fd: &'a FdGuardUpper, map: &Map) -> Result<Self> {
        let base = unsafe { syscall::fmap(fd.as_raw_fd(), map)? };
        Ok(Self {
            fd,
            size: map.size,
            base,
        })
    }
    pub fn remap(&mut self, offset: usize, mut flags: MapFlags) -> Result<()> {
        flags.remove(MapFlags::MAP_FIXED_NOREPLACE);
        flags.insert(MapFlags::MAP_FIXED);

        let _new_base = unsafe {
            syscall::fmap(
                self.fd.as_raw_fd(),
                &Map {
                    offset,
                    size: self.size,
                    flags,
                    address: self.base,
                },
            )?
        };

        Ok(())
    }
    #[expect(clippy::mut_from_ref)]
    pub unsafe fn map_mut_anywhere(
        fd: &'a FdGuardUpper,
        offset: usize,
        size: usize,
    ) -> Result<(Self, &'a mut [u8])> {
        let mut this = Self::map(
            fd,
            &Map {
                size,
                offset,
                address: 0,
                flags: PROT_READ | PROT_WRITE,
            },
        )?;
        let slice = unsafe { &mut *this.as_mut_ptr_slice() };

        Ok((this, slice))
    }
    pub fn addr(&self) -> usize {
        self.base
    }
    #[expect(clippy::len_without_is_empty, reason = "this len() is not costly")]
    pub fn len(&self) -> usize {
        self.size
    }
    pub fn as_mut_ptr_slice(&mut self) -> *mut [u8] {
        core::ptr::slice_from_raw_parts_mut(self.base as *mut u8, self.size)
    }
    pub fn take(mut self) {
        self.size = 0;
    }
}
impl<'a> Drop for MmapGuard<'a> {
    fn drop(&mut self) {
        if self.size != 0 {
            let _ = unsafe { syscall::funmap(self.base, self.size) };
        }
    }
}

pub(crate) struct FileBufReader {
    fd: usize,
    buf: [u8; 8192],
    pos: usize,
    cap: usize,
}

impl FileBufReader {
    pub fn from_fd(fd: usize) -> FileBufReader {
        FileBufReader {
            fd,
            buf: [0; 8192],
            pos: 0,
            cap: 0,
        }
    }
}

impl FileBufReader {
    pub fn read_le_u64(&mut self) -> Result<Option<u64>> {
        if self.pos >= self.cap {
            debug_assert!(self.pos == self.cap);
            self.cap = syscall::read(self.fd, &mut self.buf)?;
            self.pos = 0;
        }

        if self.cap == 0 {
            return Ok(None);
        }

        if self.cap - self.pos < 8 {
            unreachable!();
        }

        let num = u64::from_le_bytes(self.buf[self.pos..self.pos + 8].try_into().unwrap());

        self.pos += 8;

        Ok(Some(num))
    }
}
#[repr(transparent)]
pub struct FdGuard<const UPPER: bool = false> {
    fd: usize,
}
pub type FdGuardUpper = FdGuard<true>;
impl FdGuard<false> {
    #[inline]
    pub fn new(fd: usize) -> Self {
        Self { fd }
    }
    #[inline]
    pub fn open<T: AsRef<str>>(path: T, flags: usize) -> Result<Self> {
        open(path, flags).map(Self::new)
    }

    #[inline]
    pub fn open_into_upper<T: AsRef<str>>(path: T, flags: usize) -> Result<FdGuardUpper> {
        crate::sys::open_into_upper(path, flags)
            .map(FdGuard::new)?
            .to_upper()
    }

    #[inline]
    pub fn to_upper(self) -> Result<FdGuardUpper> {
        // Move to upper table if necessary
        let fd = if self.fd & syscall::UPPER_FDTBL_TAG == 0 {
            let fd = crate::sys::fdcntl(
                self.fd,
                None,
                syscall::FileTableVerb::Move,
                syscall::UPPER_FDTBL_TAG,
            )?;
            self.take();
            fd
        } else {
            self.take()
        };
        Ok(FdGuard::<true> { fd })
    }

    // Not implemented for UPPER to prevent misuse
    #[inline]
    pub fn as_c_fd(&self) -> Option<i32> {
        i32::try_from(self.fd).ok()
    }
}
impl<const UPPER: bool> FdGuard<UPPER> {
    #[inline]
    pub fn openat<T: AsRef<str>>(
        &self,
        path: T,
        flags: usize,
        fcntl_flags: usize,
    ) -> Result<FdGuard<false>> {
        crate::sys::openat(self.fd, path, flags, fcntl_flags).map(FdGuard::new)
    }
    #[inline]
    pub fn openat_into_upper<T: AsRef<str>>(
        &self,
        path: T,
        flags: usize,
        fcntl_flags: usize,
    ) -> Result<FdGuardUpper> {
        crate::sys::openat_into_upper(self.fd, path, flags, fcntl_flags)
            .map(FdGuard::new)?
            .to_upper()
    }

    #[inline]
    pub fn dup(&self, buf: &[u8]) -> Result<FdGuard<false>> {
        crate::sys::dup(self.fd, buf).map(FdGuard::new)
    }

    #[inline]
    pub fn dup_into_upper(&self, buf: &[u8]) -> Result<FdGuardUpper> {
        crate::sys::dup_into_upper(self.fd, buf)
            .map(FdGuard::new)?
            .to_upper()
    }

    #[inline]
    pub fn fcntl(&self, cmd: usize, arg: usize) -> Result<usize> {
        crate::sys::fcntl(self.fd, cmd, arg)
    }

    #[inline]
    pub fn fstat(&self, stat: &mut syscall::Stat) -> Result<usize> {
        fstat(self.fd, stat)
    }

    #[inline]
    pub fn lseek(&self, offset: isize, whence: usize) -> Result<usize> {
        syscall::lseek(self.fd, offset, whence)
    }

    #[inline]
    pub fn read(&self, buf: &mut [u8]) -> Result<usize> {
        syscall::read(self.fd, buf)
    }

    #[inline]
    pub fn write(&self, buf: &[u8]) -> Result<usize> {
        syscall::write(self.fd, buf)
    }

    #[inline]
    pub fn call_ro(&self, payload: &mut [u8], flags: CallFlags, metadata: &[u64]) -> Result<usize> {
        crate::sys::sys_call_ro(self.fd, payload, flags, metadata)
    }

    #[inline]
    pub fn call_wo(&self, payload: &[u8], flags: CallFlags, metadata: &[u64]) -> Result<usize> {
        crate::sys::sys_call_wo(self.fd, payload, flags, metadata)
    }

    #[inline]
    pub fn call_rw(&self, payload: &mut [u8], flags: CallFlags, metadata: &[u64]) -> Result<usize> {
        crate::sys::sys_call_rw(self.fd, payload, flags, metadata)
    }

    #[inline]
    pub fn as_raw_fd(&self) -> usize {
        self.fd
    }

    #[inline]
    pub fn take(self) -> usize {
        let fd = self.fd;
        core::mem::forget(self);
        fd
    }
}
impl<const UPPER: bool> Drop for FdGuard<UPPER> {
    #[inline]
    fn drop(&mut self) {
        let _ = crate::sys::close(self.fd);
    }
}
impl Debug for FdGuard<false> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "[fd {}]", self.fd)
    }
}
impl Debug for FdGuardUpper {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "[fd upper {}]", self.fd & !syscall::UPPER_FDTBL_TAG)
    }
}
pub fn create_set_addr_space_buf(
    space: usize,
    ip: usize,
    sp: usize,
) -> [u8; size_of::<usize>() * 3] {
    let mut buf = [0u8; size_of::<usize>() * 3];
    buf.copy_from_slice([space, sp, ip].map(usize::to_ne_bytes).as_flattened());
    buf
}
pub fn create_set_addr_space_buf_for_fork(
    space: usize,
    ip: usize,
    sp: usize,
    arg1: usize,
) -> [u8; size_of::<usize>() * 4] {
    let mut buf = [0u8; size_of::<usize>() * 4];
    buf.copy_from_slice([space, sp, ip, arg1].map(usize::to_ne_bytes).as_flattened());
    buf
}

/// Spawns a new context which will not share the same address space as the current one. File
/// descriptors from other schemes are reobtained with `dup`, and grants referencing such file
/// descriptors are reobtained through `fmap`. Other mappings are kept but duplicated using CoW.
pub fn fork_impl(args: &ForkArgs<'_>) -> Result<usize> {
    let old_mask = crate::signal::get_sigmask()?;
    let pid = unsafe {
        Error::demux(__relibc_internal_fork_wrapper(
            core::ptr::from_ref::<ForkArgs>(args) as usize,
        ))?
    };

    if pid == 0 {
        crate::signal::set_sigmask(Some(old_mask), None)?;
    }
    Ok(pid)
}

pub enum ForkArgs<'a> {
    Init {
        this_thr_fd: &'a FdGuardUpper,
        auth: &'a FdGuard,
        name: &'a str,
    },
    Managed,
}

pub struct ForkAllocHooks {
    pub lock: fn(),
    pub unlock_parent: fn(),
    pub unlock_child: fn(),
}

static FORK_ALLOC_HOOKS: AtomicPtr<ForkAllocHooks> = AtomicPtr::new(ptr::null_mut());

pub fn set_fork_alloc_hooks(hooks: &'static ForkAllocHooks) {
    FORK_ALLOC_HOOKS.store(hooks as *const ForkAllocHooks as *mut ForkAllocHooks, Ordering::Release);
}

pub(crate) unsafe fn unlock_fork_alloc_child() {
    let ptr = FORK_ALLOC_HOOKS.load(Ordering::Acquire);
    if !ptr.is_null() {
        let hooks = unsafe { &*ptr };
        (hooks.unlock_child)();
    }
}

struct ForkAllocGuard {
    hooks: *const ForkAllocHooks,
}

impl ForkAllocGuard {
    fn lock() -> Self {
        let ptr = FORK_ALLOC_HOOKS.load(Ordering::Acquire);
        if !ptr.is_null() {
            let hooks = unsafe { &*ptr };
            (hooks.lock)();
            Self { hooks: ptr }
        } else {
            Self { hooks: ptr::null() }
        }
    }
}

impl Drop for ForkAllocGuard {
    fn drop(&mut self) {
        if !self.hooks.is_null() {
            let hooks = unsafe { &*self.hooks };
            (hooks.unlock_parent)();
        }
    }
}

pub fn raw_dup_into(fd: usize, buf: &[u8], out: usize) -> Result<usize> {
    unsafe {
        syscall::syscall4(
            syscall::SYS_DUP_INTO,
            fd,
            buf.as_ptr() as usize,
            buf.len(),
            out,
        )
    }
}

const FORK_MAX_REAP_POLLS: usize = 200;
const FORK_REAP_SLEEP_NS: i32 = 1_000_000;

pub struct ChildCleanupGuard {
    pid: usize,
    proc_fd: usize,
    armed: bool,
}

impl ChildCleanupGuard {
    pub fn disarmed() -> Self {
        Self {
            pid: 0,
            proc_fd: usize::MAX,
            armed: false,
        }
    }

    pub fn arm(&mut self, pid: usize, proc_fd: usize) {
        self.pid = pid;
        self.proc_fd = proc_fd;
        self.armed = true;
    }

    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ChildCleanupGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        #[cfg(feature = "proc")]
        {
            let exited = if self.proc_fd != usize::MAX {
                sys_call_wo(
                    self.proc_fd,
                    &[],
                    CallFlags::empty(),
                    &[ProcCall::Exit as u64, (SIGKILL << 8) as u64],
                )
                .is_ok()
            } else {
                false
            };
            if !exited {
                let _ = crate::sys::posix_kill(ProcKillTarget::SingleProc(self.pid), SIGKILL);
            }
            let mut status = 0usize;
            let mut reaped = false;
            for _ in 0..FORK_MAX_REAP_POLLS {
                match crate::sys::sys_waitpid(
                    crate::sys::WaitpidTarget::SingleProc { pid: self.pid },
                    &mut status,
                    WaitFlags::WNOHANG,
                ) {
                    Ok(p) if p == self.pid => {
                        reaped = true;
                        break;
                    }
                    Err(Error { errno: ECHILD }) => {
                        reaped = true;
                        break;
                    }
                    Err(Error { errno: EINTR }) => continue,
                    Ok(_) => {
                        let req = syscall::TimeSpec {
                            tv_sec: 0,
                            tv_nsec: FORK_REAP_SLEEP_NS,
                        };
                        let mut rem = syscall::TimeSpec::default();
                        let _ = syscall::nanosleep(&req, &mut rem);
                    }
                    Err(_) => break,
                }
            }
            if !reaped {
                let _ = syscall::write(2, b"[redox-rt] fork cleanup: unable to reap child\n");
            }
        }
    }
}

const FORK_MAX_GRANTS: usize = 16_384;
const FORK_GRANT_READ_CHUNK: usize = 16;
const FORK_MAX_GRANT_READS: usize = (FORK_MAX_GRANTS / FORK_GRANT_READ_CHUNK) + 1;
const FORK_MAX_GRANT_SNAPSHOT_PASSES: usize = 32;

fn grant_desc_eq(a: &GrantDesc, b: &GrantDesc) -> bool {
    a.base == b.base && a.size == b.size && a.flags == b.flags && a.offset == b.offset
}

fn read_grants_once(
    cur_addr_space_fd: &FdGuardUpper,
    out: &mut alloc::vec::Vec<GrantDesc>,
) -> Result<()> {
    out.clear();
    cur_addr_space_fd.lseek(0, SEEK_SET)?;
    let mut grant_desc_buf = [GrantDesc::default(); FORK_GRANT_READ_CHUNK];
    for _ in 0..FORK_MAX_GRANT_READS {
        let bytes_read = {
            let buf = unsafe {
                core::slice::from_raw_parts_mut(
                    grant_desc_buf.as_mut_ptr().cast(),
                    grant_desc_buf.len() * size_of::<GrantDesc>(),
                )
            };
            cur_addr_space_fd.read(buf)?
        };
        if bytes_read == 0 {
            return Ok(());
        }
        let count = bytes_read / size_of::<GrantDesc>();
        if out.len().saturating_add(count) > FORK_MAX_GRANTS {
            return Err(Error::new(E2BIG));
        }
        out.extend_from_slice(&grant_desc_buf[..count]);
    }
    Err(Error::new(E2BIG))
}

fn read_stable_grants(cur_addr_space_fd: &FdGuardUpper) -> Result<alloc::vec::Vec<GrantDesc>> {
    let mut prev = alloc::vec::Vec::new();
    let mut curr = alloc::vec::Vec::new();
    read_grants_once(cur_addr_space_fd, &mut prev)?;
    for _ in 0..FORK_MAX_GRANT_SNAPSHOT_PASSES {
        read_grants_once(cur_addr_space_fd, &mut curr)?;
        if prev.len() == curr.len()
            && prev
                .iter()
                .zip(curr.iter())
                .all(|(a, b)| grant_desc_eq(a, b))
        {
            return Ok(curr);
        }
        core::mem::swap(&mut prev, &mut curr);
    }
    Err(Error::new(EAGAIN))
}

fn format_grant_fd_name(base: usize, out: &mut [u8]) -> &[u8] {
    const PREFIX: &[u8] = b"grant-fd-";
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let hex_digits = size_of::<usize>() * 2;
    let total = PREFIX.len() + hex_digits;
    assert!(out.len() >= total);
    out[..PREFIX.len()].copy_from_slice(PREFIX);
    for i in 0..hex_digits {
        let shift = (hex_digits - 1 - i) * 4;
        out[PREFIX.len() + i] = HEX[(base >> shift) & 0xf];
    }
    &out[..total]
}

pub fn fork_inner(initial_rsp: *mut usize, args: &ForkArgs) -> Result<usize> {
    let (cur_filetable_fd, new_filetable_fd, new_proc_fd, new_thr_fd, new_pid);
    let mut child_cleanup = ChildCleanupGuard::disarmed();

    {
        let cur_thr_fd = match args {
            ForkArgs::Init { this_thr_fd, .. } => this_thr_fd,
            ForkArgs::Managed => RtTcb::current().thread_fd(),
        };
        let NewChildProc {
            proc_fd,
            thr_fd,
            pid,
        } = new_child_process(args)?;
        new_proc_fd = proc_fd;
        new_thr_fd = thr_fd;
        new_pid = pid;
        if matches!(args, ForkArgs::Managed) {
            child_cleanup.arm(
                new_pid,
                new_proc_fd.as_ref().map_or(usize::MAX, |p| p.as_raw_fd()),
            );
        }

        cur_filetable_fd = cur_thr_fd.dup_into_upper(b"filetable-binary")?;
        let proc_fd_raw = new_proc_fd.as_ref().map_or(usize::MAX, |p| p.as_raw_fd());

        let mut scratchpad = ForkScratchpad {
            cur_filetable_fd: cur_filetable_fd.as_raw_fd(),
            new_filetable_fd: usize::MAX,
            new_proc_fd: proc_fd_raw,
            new_thr_fd: new_thr_fd.as_raw_fd(),
        };

        let (cur_addr_space_fd, new_addr_space_fd) = {
            let _siglock = crate::signal::tmp_disable_signals();
            let _dyn_guard = DYNAMIC_PROC_INFO.lock();
            let fdtbl = crate::current_filetable();
            let mut fdtbl_guard = fdtbl.inner.lock();
            let fd_guard = fdtbl.fd.lock();

            let new_filetable_slot = fdtbl_guard.reserve_upper_slot_raw(0, fd_guard.as_ref())?;
            let cur_addr_space_slot =
                match fdtbl_guard.reserve_upper_slot_raw(0, fd_guard.as_ref()) {
                    Ok(slot) => slot,
                    Err(err) => {
                        let _ = fdtbl_guard.remove(new_filetable_slot);
                        return Err(err);
                    }
                };
            let new_addr_space_slot =
                match fdtbl_guard.reserve_upper_slot_raw(0, fd_guard.as_ref()) {
                    Ok(slot) => slot,
                    Err(err) => {
                        let _ = fdtbl_guard.remove(cur_addr_space_slot);
                        let _ = fdtbl_guard.remove(new_filetable_slot);
                        return Err(err);
                    }
                };

            scratchpad.new_filetable_fd = new_filetable_slot;
            #[cfg(target_arch = "x86")]
            unsafe {
                let scratchpad_ptr = initial_rsp.cast::<ForkScratchpad>();
                scratchpad_ptr.write(scratchpad);
            }

            let _alloc_guard = ForkAllocGuard::lock();
            let _sigact_guard = crate::signal::lock_sigactions();

            if let Err(err) = raw_dup_into(cur_filetable_fd.as_raw_fd(), b"copy", new_filetable_slot)
            {
                let _ = fdtbl_guard.remove(new_addr_space_slot);
                let _ = fdtbl_guard.remove(cur_addr_space_slot);
                let _ = fdtbl_guard.remove(new_filetable_slot);
                return Err(err);
            }
            if let Err(err) = raw_dup_into(cur_thr_fd.as_raw_fd(), b"addrspace", cur_addr_space_slot)
            {
                let _ = unsafe { syscall::syscall1(syscall::SYS_CLOSE, new_filetable_slot) };
                let _ = fdtbl_guard.remove(new_addr_space_slot);
                let _ = fdtbl_guard.remove(cur_addr_space_slot);
                let _ = fdtbl_guard.remove(new_filetable_slot);
                return Err(err);
            }
            if let Err(err) = raw_dup_into(cur_addr_space_slot, b"exclusive", new_addr_space_slot) {
                let _ = unsafe { syscall::syscall1(syscall::SYS_CLOSE, cur_addr_space_slot) };
                let _ = unsafe { syscall::syscall1(syscall::SYS_CLOSE, new_filetable_slot) };
                let _ = fdtbl_guard.remove(new_addr_space_slot);
                let _ = fdtbl_guard.remove(cur_addr_space_slot);
                let _ = fdtbl_guard.remove(new_filetable_slot);
                return Err(err);
            }

            drop(_sigact_guard);
            drop(_alloc_guard);
            drop(fd_guard);
            drop(fdtbl_guard);
            drop(_dyn_guard);

            new_filetable_fd = FdGuard::new(new_filetable_slot).to_upper().unwrap();
            (
                FdGuard::new(cur_addr_space_slot).to_upper().unwrap(),
                FdGuard::new(new_addr_space_slot).to_upper().unwrap(),
            )
        };

        #[cfg(any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        ))]
        let arg1 = {
            let scratchpad_ptr: *const ForkScratchpad = &raw const scratchpad;
            scratchpad_ptr as usize
        };

        // CoW-duplicate address space.
        {
            let new_addr_space_sel_fd = new_thr_fd.dup_into_upper(b"current-addrspace")?;
            let grants = read_stable_grants(&cur_addr_space_fd)?;
            let mut name_buf = [0u8; 32];

            for grant in &grants {
                if !grant.flags.contains(GrantFlags::GRANT_SCHEME)
                    || !grant.flags.contains(GrantFlags::GRANT_SHARED)
                {
                    continue;
                }

                let name = format_grant_fd_name(grant.base, &mut name_buf);
                let grant_fd = match cur_addr_space_fd.dup_into_upper(name) {
                    Ok(fd) => fd,
                    Err(Error { errno: EINVAL }) => continue,
                    Err(err) => return Err(err),
                };

                let mut flags = MAP_SHARED | MAP_FIXED_NOREPLACE;

                flags.set(PROT_READ, grant.flags.contains(GrantFlags::GRANT_READ));
                flags.set(PROT_WRITE, grant.flags.contains(GrantFlags::GRANT_WRITE));
                flags.set(PROT_EXEC, grant.flags.contains(GrantFlags::GRANT_EXEC));

                mmap_remote(
                    &new_addr_space_fd,
                    &grant_fd,
                    grant.offset as usize,
                    grant.base,
                    grant.size,
                    flags,
                )?;
            }
            #[cfg(any(
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "riscv64"
            ))]
            let buf = create_set_addr_space_buf_for_fork(
                new_addr_space_fd.as_raw_fd(),
                __relibc_internal_fork_ret as *const () as usize,
                initial_rsp as usize,
                arg1,
            );
            #[cfg(target_arch = "x86")]
            let buf = create_set_addr_space_buf(
                new_addr_space_fd.as_raw_fd(),
                __relibc_internal_fork_ret as *const () as usize,
                initial_rsp as usize,
            );
            new_addr_space_sel_fd.write(&buf)?;
        }
        {
            // Reuse the same sigaltstack and signal entry (all memory will be re-mapped CoW later).
            //
            // Do this after the address space is cloned, since the kernel will get a shared
            // reference to the TCB and whatever pages stores the signal proc control struct.
            {
                let new_sighandler_fd = new_thr_fd.dup_into_upper(b"sighandler")?;
                new_sighandler_fd.write(&crate::signal::current_setsighandler_struct())?;
            }
            if let Some(ref proc_fd) = new_proc_fd {
                sys_call(
                    proc_fd.as_raw_fd(),
                    CallFlags::empty(),
                    &[ProcCall::SyncSigPctl as u64],
                )?;
                sys_call(
                    new_thr_fd.as_raw_fd(),
                    CallFlags::empty(),
                    &[ThreadCall::SyncSigTctl as u64],
                )?;
            }
        }
        {
            // Copy environment registers.
            let cur_env_regs_fd = cur_thr_fd.dup_into_upper(b"regs/env")?;
            let new_env_regs_fd = new_thr_fd.dup_into_upper(b"regs/env")?;

            let mut env_regs = syscall::EnvRegisters::default();
            cur_env_regs_fd.read(&mut env_regs)?;
            new_env_regs_fd.write(&env_regs)?;
        }
    }
    // TODO: This shouldn't be opt-in, make sure in the kernel that userspace can't create cyclic
    // references to leak fds.
    let (new_filetable_weak_fd, new_filetable_fd_idx) = {
        // TODO: Use file descriptor forwarding or something similar to avoid copying the file
        // table in the kernel.
        let new_filetable_sel_fd = new_thr_fd.dup_into_upper(b"current-filetable")?;
        new_filetable_sel_fd.write(&usize::to_ne_bytes(new_filetable_fd.as_raw_fd()))?;
        // downgrade to a kernel handle with only a Weak reference to the FdTbl
        let weak = new_filetable_fd.dup_into_upper(b"refresh")?;
        let new_filetable_fd_idx = new_filetable_fd.as_raw_fd();

        drop(new_filetable_fd);
        (weak, new_filetable_fd_idx)
    };

    new_filetable_weak_fd.call_wo(
        &new_filetable_weak_fd.as_raw_fd().to_ne_bytes(),
        syscall::CallFlags::FD,
        &[new_filetable_fd_idx as u64],
    )?;
    let _ = new_filetable_weak_fd.take();

    let start_fd = new_thr_fd.dup_into_upper(b"start")?;
    start_fd.write(&[0])?;
    child_cleanup.disarm();
    Ok(new_pid)
}

pub struct NewChildProc {
    pub proc_fd: Option<FdGuardUpper>,

    pub thr_fd: FdGuardUpper,
    pub pid: usize,
}

pub fn new_child_process(args: &ForkArgs<'_>) -> Result<NewChildProc> {
    match *args {
        ForkArgs::Managed => {
            let proc_info = crate::static_proc_info();
            let this_proc_fd = proc_info
                .proc_fd
                .as_ref()
                .expect("cannot use ForkArgs::Managed without an existing proc info");
            let child_proc_fd = this_proc_fd.dup_into_upper(b"fork")?;
            let only_thread_fd = child_proc_fd.dup_into_upper(b"thread-0")?;
            let meta = read_proc_meta(&child_proc_fd)?;
            Ok(NewChildProc {
                proc_fd: Some(child_proc_fd),
                thr_fd: only_thread_fd,
                pid: meta.pid as usize,
            })
        }
        #[cfg(feature = "proc")]
        ForkArgs::Init { .. } => unreachable!(),

        #[cfg(not(feature = "proc"))]
        ForkArgs::Init {
            this_thr_fd: _,
            auth,
            name,
        } => {
            let thr_fd = auth.dup_into_upper(b"new-context")?;
            let buf = syscall::ProcSchemeAttrs {
                pid: 0,
                euid: 0,
                egid: 0,
                prio: !0, // Value is overwritten later
                debug_name: {
                    let mut buf = [0; 32];
                    let len = name.len().min(buf.len());
                    buf[..len].copy_from_slice(&name.as_bytes()[..len]);
                    buf
                },
            };
            let attr_fd = thr_fd
                .dup_into_upper(alloc::format!("auth-{}-attrs", auth.as_raw_fd()).as_bytes())?;
            attr_fd.write(&buf)?;
            Ok(NewChildProc {
                thr_fd,
                pid: 1, // dummy fd to distinguish child from parent
                proc_fd: None,
            })
        }
    }
}

pub unsafe fn make_init(proc_cap: usize) -> (&'static FdGuardUpper, &'static FdGuardUpper) {
    let proc_fd = FdGuard::new(
        crate::sys::openat_into_upper(proc_cap, "init", 0, 0).expect("failed to create init"),
    )
    .to_upper()
    .unwrap();

    crate::sys::sys_call_wo(
        proc_fd.as_raw_fd(),
        &RtTcb::current()
            .thread_fd()
            .dup_into_upper(&[])
            .unwrap()
            .take()
            .to_ne_bytes(),
        syscall::CallFlags::FD,
        &[],
    )
    .expect("failed to assign current thread to init process");

    let managed_thr_fd = proc_fd
        .dup_into_upper(b"thread-0")
        .expect("failed to get managed thread for init");

    let managed_thr_fd = unsafe { (*RtTcb::current().thr_fd.get()).insert(managed_thr_fd) };

    unsafe {
        STATIC_PROC_INFO.get().write(crate::StaticProcInfo {
            pid: 1,
            proc_fd: Some(proc_fd),
        })
    };
    *DYNAMIC_PROC_INFO.lock() = crate::DynamicProcInfo {
        pgid: 1,
        ruid: 0,
        euid: 0,
        suid: 0,
        rgid: 0,
        egid: 0,
        sgid: 0,
        ns_fd: None,
    };
    (
        unsafe { (*STATIC_PROC_INFO.get()).proc_fd.as_ref().unwrap() },
        managed_thr_fd,
    )
}
pub(crate) static STATIC_PROC_INFO: SyncUnsafeCell<StaticProcInfo> =
    SyncUnsafeCell::new(StaticProcInfo {
        pid: 0,
        proc_fd: None,
    });

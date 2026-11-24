use std::os::fd::IntoRawFd;
use std::ptr::NonNull;

use libredox::{Fd, flag};
use redox_buffer_pool::{AllocationStrategy, NoHandle};
use redox_rings::op::{DiskOpCqe, DiskOpKind, DiskOpSqe, RING_MAX_CQ_ENTRIES, RING_MAX_SQ_ENTRIES};
use redox_rings::raw::{RingPopError, RingPushError};
use redox_rings::sync::{BlockingConsumer, BlockingProducer, FutexWaitResult, WaitNotify};
use syscall::CallFlags;
use syscall::EIO;
use syscall::ENOMEM;
use syscall::TimeSpec;
use syscall::error::{Error, Result};
use zerocopy::IntoBytes;

use super::Disk;
use super::ring_op::{
    RingCompletion, RingOp, RingOpState, RingOps, RingPopOutcome, RingPushOutcome, RingSubmission,
    execute_ring_op,
};
use crate::{BLOCK_SIZE, DiskFile};

const POOL_SIZE: u32 = 4 * 1024 * 1024; // 4 MiB pool
const _: () = assert!(super::ring_op::RING_MAX_SQ_ENTRIES == RING_MAX_SQ_ENTRIES);
const _: () = assert!(super::ring_op::RING_MAX_CQ_ENTRIES == RING_MAX_CQ_ENTRIES);

type BufferPool = redox_buffer_pool::BufferPool<'static, u32, NoHandle, ()>;
type BufferSlice<'pool> = redox_buffer_pool::BufferSlice<'pool, u32, NoHandle, ()>;

struct Pipe(Fd);

impl WaitNotify for Pipe {
    fn wait_on_tail(
        &self,
        _expected_tail: u32,
        _deadline_opt: Option<&TimeSpec>,
    ) -> FutexWaitResult {
        unimplemented!("notify_on_tail is not implemented for Pipe")
    }
    fn notify_on_tail(&self) {
        if let Err(err) = self.0.write(&[0]) {
            log::error!("Pipe::notify_on_tail() failed: {err}");
        }
    }
    fn wait_on_head(
        &self,
        _expected_head: u32,
        _deadline_opt: Option<&TimeSpec>,
    ) -> FutexWaitResult {
        unimplemented!("wait_on_head is not implemented for Pipe")
    }
    fn notify_on_head(&self) {
        unimplemented!("notify_on_head is not implemented for Pipe")
    }
}

struct DiskRingOps<'a> {
    allocator: &'a BufferPool,
    sq: &'a mut BlockingProducer<DiskOpSqe>,
    cq: &'a mut BlockingConsumer<DiskOpCqe>,
    pipe: &'a Pipe,
}

impl<'a> RingOps<'a> for DiskRingOps<'a> {
    type Buffer = BufferSlice<'a>;

    fn allocate(&mut self, len: u32) -> Option<Self::Buffer> {
        self.allocator
            .acquire_borrowed_slice(len, 1, AllocationStrategy::Optimal)
    }

    fn buffer_offset(buf: &Self::Buffer) -> u32 {
        buf.offset()
    }

    fn release(&mut self, buf: Self::Buffer) {
        drop(buf);
    }

    fn quarantine(&mut self, buf: Self::Buffer) {
        core::mem::forget(buf);
    }

    fn push_sqe(&mut self, sqe: RingSubmission) -> RingPushOutcome {
        let opcode = match sqe.op {
            RingOp::Read => DiskOpKind::Read as u8,
            RingOp::Write => DiskOpKind::Write as u8,
        };
        let raw = DiskOpSqe {
            opcode,
            block: sqe.block,
            buf_offset: sqe.buf_offset,
            buf_len: sqe.buf_len,
            user_data: sqe.user_data,
            file_idx: 0,
            pad: [0; 3],
        };
        match self.sq.inner.push_back(raw) {
            Ok(_) => RingPushOutcome::Pushed,
            Err(RingPushError::Full(_)) => RingPushOutcome::Full,
            Err(RingPushError::Broken(_)) => RingPushOutcome::Broken,
        }
    }

    fn notify_if_waiting(&mut self) {
        if self.sq.inner.header.is_wait_head() {
            self.pipe.notify_on_tail();
        }
    }

    fn pop_cqe(&mut self) -> RingPopOutcome {
        match self.cq.pop(None) {
            Ok(cqe) => RingPopOutcome::Completion(RingCompletion {
                user_data: cqe.user_data,
                count: cqe.count,
                status: cqe.status,
            }),
            Err(RingPopError::Empty) => RingPopOutcome::Empty,
            Err(RingPopError::Broken) => RingPopOutcome::Broken,
        }
    }
}

pub struct DiskRing {
    sq: BlockingProducer<DiskOpSqe>,
    cq: BlockingConsumer<DiskOpCqe>,
    shm_base: *mut u8,
    pool_size: usize,
    allocator: BufferPool,
    op_state: RingOpState,
    sectors_per_block: u64,
    disk_size: u64,
    #[allow(unused, reason = "closed on drop")]
    ring_fd: Fd,
    disk_fd: Fd,
    pipe: Pipe,
}

impl DiskRing {
    pub fn from_fd(ring_fd: Fd, df: DiskFile, disk_size: u64) -> Result<Self> {
        use libredox::call::{MmapArgs, mmap};
        use redox_rings::op::{RingCallVerb, RingSetupFlags, RingSetupParams};

        let disk_fd = Fd::new(df.file.into_raw_fd() as usize);
        let sector_size =
            u64::try_from(disk_fd.stat()?.st_blksize).map_err(|_| Error::new(syscall::EINVAL))?;

        if !BLOCK_SIZE.is_multiple_of(sector_size) {
            log::error!(
                "block size must be a multiple of sector size: BLOCK_SIZE={BLOCK_SIZE}, sector_size={sector_size}"
            );
            return Err(Error::new(syscall::EINVAL));
        }

        let sectors_per_block = BLOCK_SIZE / sector_size;

        let mut params = RingSetupParams {
            nr_sq_entries: 256,
            // The `CQSIZE` flag is not set, so the scheme will fill in this field for us.
            nr_cq_entries: 0,
            flags: RingSetupFlags::empty().bits(),
            pool_size: POOL_SIZE,
        };

        ring_fd.call_rw(
            params.as_mut_bytes(),
            CallFlags::empty(),
            &[RingCallVerb::Setup as u64],
        )?;

        let op_state =
            RingOpState::new(params.nr_sq_entries, params.nr_cq_entries, params.pool_size)?;
        let pool_size = params.pool_size as usize;

        let fixed_rtbl_req = [ring_fd.raw(), disk_fd.raw()];
        libredox::call::call_wo(
            fixed_rtbl_req.as_slice(),
            &[],
            CallFlags::empty(),
            &[RingCallVerb::SetFileTable as u64],
        )?;

        // mmap the shared DMA/shm pool
        let shm_ptr = unsafe {
            mmap(MmapArgs {
                addr: std::ptr::null_mut(),
                length: pool_size,
                prot: flag::PROT_READ | flag::PROT_WRITE,
                flags: flag::MAP_SHARED,
                fd: ring_fd.raw(),
                offset: 0,
            })
            .map_err(|_| Error::new(EIO))? as *mut u8
        };

        let mut fd_buf = [usize::MAX; 3]; // [sq_shm_fd, cq_shm_fd, pipe_fd]
        ring_fd
            .call_ro(
                fd_buf.as_mut_bytes(),
                CallFlags::FD | CallFlags::FD_UPPER,
                &[],
            )
            .map_err(|_| Error::new(EIO))?;

        let (sq_shm_fd, cq_shm_fd, pipe) = (
            Fd::new(fd_buf[0]),
            Fd::new(fd_buf[1]),
            Fd::new(fd_buf[2]).openat("write", 0, 0)?,
        );

        let sq =
            BlockingProducer::<DiskOpSqe>::from_fd(sq_shm_fd, false, Some(params.nr_sq_entries))
                .map_err(|_| Error::new(EIO))?;

        let cq =
            BlockingConsumer::<DiskOpCqe>::from_fd(cq_shm_fd, false, Some(params.nr_cq_entries))
                .map_err(|_| Error::new(EIO))?;

        let allocator = {
            let pool = BufferPool::new(None);
            let handle = pool
                .begin_expand(params.pool_size)
                .map_err(|_| Error::new(EIO))?;

            unsafe {
                handle.initialize(NonNull::new(shm_ptr).ok_or(Error::new(ENOMEM))?, ());
            }
            pool
        };

        Ok(Self {
            sq,
            cq,
            ring_fd,
            disk_fd,
            shm_base: shm_ptr,
            pool_size,
            allocator,
            op_state,
            sectors_per_block,
            disk_size,
            pipe: Pipe(pipe),
        })
    }

    fn disk_op<'a, B, I, P, C>(
        &'a mut self,
        op: RingOp,
        batch: B,
        prepare: P,
        complete: C,
    ) -> Result<usize>
    where
        B: Iterator<Item = (u64, I)>,
        I: AsRef<[u8]>,
        P: Fn(&I, &mut BufferSlice<'a>),
        C: Fn(&mut I, &BufferSlice<'a>),
    {
        let mut ops = DiskRingOps {
            allocator: &self.allocator,
            sq: &mut self.sq,
            cq: &mut self.cq,
            pipe: &self.pipe,
        };
        execute_ring_op(&mut self.op_state, &mut ops, op, batch, prepare, complete)
    }
}

impl Disk for DiskRing {
    unsafe fn read_at(&mut self, block: u64, buffer: &mut [u8]) -> Result<usize> {
        self.disk_op(
            RingOp::Read,
            core::iter::once((block * self.sectors_per_block, buffer)),
            |_buf, _cmd_buf| {},
            |buf, cmd_buf| buf.copy_from_slice(cmd_buf),
        )
    }

    unsafe fn write_at(&mut self, block: u64, buffer: &[u8]) -> Result<usize> {
        unsafe { self.write_at_batched(&[(block, buffer)]) }
    }

    unsafe fn write_at_batched(&mut self, batch: &[(u64, &[u8])]) -> Result<usize> {
        let sectors_per_block = self.sectors_per_block;
        self.disk_op(
            RingOp::Write,
            batch
                .iter()
                .map(|(block, buf)| (*block * sectors_per_block, *buf)),
            |buf, cmd_buf| cmd_buf.copy_from_slice(buf),
            |_buf, _cmd_buf| {},
        )
    }

    fn size(&mut self) -> Result<u64> {
        Ok(self.disk_size)
    }

    fn flush(&mut self) -> Result<()> {
        self.disk_fd.fsync()?;
        Ok(())
    }
}

impl Drop for DiskRing {
    fn drop(&mut self) {
        unsafe {
            let _ = libredox::call::munmap(self.shm_base.cast(), self.pool_size);
        }
    }
}

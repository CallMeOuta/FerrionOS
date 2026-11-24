use std::collections::HashMap;
use syscall::error::{EINVAL, EIO, ENOMEM, Error, Result};

use crate::BLOCK_SIZE;

pub const RING_MAX_SQ_ENTRIES: u32 = 1 << 15;
pub const RING_MAX_CQ_ENTRIES: u32 = 2 * RING_MAX_SQ_ENTRIES;
pub const MAX_RING_POOL_SIZE: u32 = 64 * 1024 * 1024;
pub const MAX_EMPTY_POP_RETRIES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingState {
    Live,
    Broken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingOp {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingSubmission {
    pub op: RingOp,
    pub block: u64,
    pub buf_offset: u32,
    pub buf_len: u32,
    pub user_data: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingCompletion {
    pub user_data: u64,
    pub count: u32,
    pub status: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingPushOutcome {
    Pushed,
    Full,
    Broken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingPopOutcome {
    Completion(RingCompletion),
    Empty,
    Broken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingOpState {
    state: RingState,
    max_inflight: usize,
    next_id: u64,
}

impl RingOpState {
    pub fn new(nr_sq_entries: u32, nr_cq_entries: u32, pool_size: u32) -> Result<Self> {
        let max_inflight = validate_ring_params(nr_sq_entries, nr_cq_entries, pool_size)?;
        Ok(Self {
            state: RingState::Live,
            max_inflight,
            next_id: 0,
        })
    }

    pub fn state(&self) -> RingState {
        self.state
    }

    pub fn max_inflight(&self) -> usize {
        self.max_inflight
    }

    fn mark_broken(&mut self) {
        self.state = RingState::Broken;
    }

    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }
}

pub fn validate_ring_params(
    nr_sq_entries: u32,
    nr_cq_entries: u32,
    pool_size: u32,
) -> Result<usize> {
    if nr_sq_entries == 0 || nr_sq_entries > RING_MAX_SQ_ENTRIES || !nr_sq_entries.is_power_of_two()
    {
        return Err(Error::new(EINVAL));
    }
    if nr_cq_entries == 0 || nr_cq_entries > RING_MAX_CQ_ENTRIES || !nr_cq_entries.is_power_of_two()
    {
        return Err(Error::new(EINVAL));
    }
    if pool_size == 0 || pool_size > MAX_RING_POOL_SIZE {
        return Err(Error::new(EINVAL));
    }
    let max_pool_blocks = (pool_size as usize) / (BLOCK_SIZE as usize);
    let max_inflight = (nr_cq_entries as usize).min(max_pool_blocks);
    if max_inflight == 0 {
        return Err(Error::new(EINVAL));
    }
    Ok(max_inflight)
}

pub trait RingOps<'a> {
    type Buffer;

    fn allocate(&mut self, len: u32) -> Option<Self::Buffer>;
    fn buffer_offset(buf: &Self::Buffer) -> u32;
    fn release(&mut self, buf: Self::Buffer);
    fn quarantine(&mut self, buf: Self::Buffer);
    fn push_sqe(&mut self, sqe: RingSubmission) -> RingPushOutcome;
    fn notify_if_waiting(&mut self);
    fn pop_cqe(&mut self) -> RingPopOutcome;
}

fn fail_broken<'a, O: RingOps<'a>, I>(
    ring: &mut RingOpState,
    ops: &mut O,
    pending: &mut HashMap<u64, (I, usize, O::Buffer)>,
) -> Error {
    ring.mark_broken();
    for (_, (_, _, buf)) in pending.drain() {
        ops.quarantine(buf);
    }
    Error::new(EIO)
}

fn pop_one_cqe<'a, O: RingOps<'a>>(ops: &mut O) -> Option<RingCompletion> {
    for _ in 0..MAX_EMPTY_POP_RETRIES {
        match ops.pop_cqe() {
            RingPopOutcome::Completion(cqe) => return Some(cqe),
            RingPopOutcome::Empty => continue,
            RingPopOutcome::Broken => return None,
        }
    }
    None
}

fn process_cqe<'a, O: RingOps<'a>, I, C: Fn(&mut I, &O::Buffer)>(
    ops: &mut O,
    cqe: RingCompletion,
    pending: &mut HashMap<u64, (I, usize, O::Buffer)>,
    total_processed: &mut usize,
    first_err: &mut Option<Error>,
    complete: &C,
) -> bool {
    let Some((mut item, expected_len, buf)) = pending.remove(&cqe.user_data) else {
        return false;
    };
    if cqe.status != 0 {
        if first_err.is_none() {
            *first_err = Some(Error::new(cqe.status as i32));
        }
    } else if cqe.count as usize != expected_len {
        if first_err.is_none() {
            *first_err = Some(Error::new(EIO));
        }
    } else {
        complete(&mut item, &buf);
        *total_processed += cqe.count as usize;
    }
    ops.release(buf);
    true
}

pub fn execute_ring_op<'a, O, B, I, P, C>(
    ring: &mut RingOpState,
    ops: &mut O,
    op: RingOp,
    batch: B,
    prepare: P,
    complete: C,
) -> Result<usize>
where
    O: RingOps<'a>,
    B: Iterator<Item = (u64, I)>,
    I: AsRef<[u8]>,
    P: Fn(&I, &mut O::Buffer),
    C: Fn(&mut I, &O::Buffer),
{
    if ring.state() == RingState::Broken {
        return Err(Error::new(EIO));
    }
    assert!(ring.max_inflight() > 0);

    let mut total_processed = 0usize;
    let mut first_err: Option<Error> = None;
    let mut pending: HashMap<u64, (I, usize, O::Buffer)> = HashMap::new();
    let mut notified = true;

    for (lba, chunk) in batch {
        if first_err.is_some() {
            break;
        }
        let len = chunk.as_ref().len();
        assert!(len >= BLOCK_SIZE as usize && len % (BLOCK_SIZE as usize) == 0);
        let req_len = match u32::try_from(len) {
            Ok(req_len) => req_len,
            Err(_) => {
                first_err = Some(Error::new(ENOMEM));
                break;
            }
        };

        if pending.len() >= ring.max_inflight() {
            assert_eq!(pending.len(), ring.max_inflight());
            if !notified {
                ops.notify_if_waiting();
                notified = true;
            }
            let Some(cqe) = pop_one_cqe(ops) else {
                return Err(fail_broken(ring, ops, &mut pending));
            };
            if !process_cqe(
                ops,
                cqe,
                &mut pending,
                &mut total_processed,
                &mut first_err,
                &complete,
            ) {
                return Err(fail_broken(ring, ops, &mut pending));
            }
            if first_err.is_some() {
                break;
            }
        }
        assert!(pending.len() < ring.max_inflight());

        let mut buf_opt = None;
        let alloc_wait_bound = pending.len().min(ring.max_inflight());
        for _ in 0..=alloc_wait_bound {
            assert_eq!(ring.state(), RingState::Live);
            if let Some(buf) = ops.allocate(req_len) {
                buf_opt = Some(buf);
                break;
            }
            if pending.is_empty() {
                first_err = Some(Error::new(ENOMEM));
                break;
            }
            if !notified {
                ops.notify_if_waiting();
                notified = true;
            }
            let Some(cqe) = pop_one_cqe(ops) else {
                return Err(fail_broken(ring, ops, &mut pending));
            };
            if !process_cqe(
                ops,
                cqe,
                &mut pending,
                &mut total_processed,
                &mut first_err,
                &complete,
            ) {
                return Err(fail_broken(ring, ops, &mut pending));
            }
            if first_err.is_some() {
                break;
            }
        }
        let Some(mut buf) = buf_opt else {
            if first_err.is_none() {
                return Err(fail_broken(ring, ops, &mut pending));
            }
            break;
        };

        prepare(&chunk, &mut buf);

        let new_id = ring.alloc_id();
        let sqe = RingSubmission {
            op,
            block: lba,
            buf_offset: O::buffer_offset(&buf),
            buf_len: req_len,
            user_data: new_id,
        };

        let push_wait_bound = pending.len();
        let mut pushed = false;
        for _ in 0..=push_wait_bound {
            match ops.push_sqe(sqe) {
                RingPushOutcome::Pushed => {
                    pushed = true;
                    break;
                }
                RingPushOutcome::Full => {
                    if pending.is_empty() {
                        ops.release(buf);
                        ring.mark_broken();
                        return Err(Error::new(EIO));
                    }
                    if !notified {
                        ops.notify_if_waiting();
                        notified = true;
                    }
                    let Some(cqe) = pop_one_cqe(ops) else {
                        ops.release(buf);
                        return Err(fail_broken(ring, ops, &mut pending));
                    };
                    if !process_cqe(
                        ops,
                        cqe,
                        &mut pending,
                        &mut total_processed,
                        &mut first_err,
                        &complete,
                    ) {
                        ops.release(buf);
                        return Err(fail_broken(ring, ops, &mut pending));
                    }
                    if first_err.is_some() {
                        break;
                    }
                }
                RingPushOutcome::Broken => {
                    ops.release(buf);
                    return Err(fail_broken(ring, ops, &mut pending));
                }
            }
        }
        if !pushed {
            ops.release(buf);
            if first_err.is_some() {
                break;
            }
            return Err(fail_broken(ring, ops, &mut pending));
        }

        pending.insert(new_id, (chunk, len, buf));
        assert!(pending.len() <= ring.max_inflight());
        notified = false;
    }

    if !notified {
        ops.notify_if_waiting();
    }

    let drain_bound = pending.len().min(ring.max_inflight());
    assert_eq!(drain_bound, pending.len());
    let mut drained = 0usize;
    while !pending.is_empty() {
        if drained >= drain_bound {
            return Err(fail_broken(ring, ops, &mut pending));
        }
        drained += 1;
        let Some(cqe) = pop_one_cqe(ops) else {
            return Err(fail_broken(ring, ops, &mut pending));
        };
        if !process_cqe(
            ops,
            cqe,
            &mut pending,
            &mut total_processed,
            &mut first_err,
            &complete,
        ) {
            return Err(fail_broken(ring, ops, &mut pending));
        }
    }
    assert_eq!(ring.state(), RingState::Live);
    assert!(pending.is_empty());

    if let Some(err) = first_err {
        return Err(err);
    }

    Ok(total_processed)
}

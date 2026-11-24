use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

use redoxfs::BLOCK_SIZE;
use redoxfs::ring_op::{
    MAX_EMPTY_POP_RETRIES, MAX_RING_POOL_SIZE, RING_MAX_CQ_ENTRIES, RING_MAX_SQ_ENTRIES,
    RingCompletion, RingOp, RingOpState, RingOps, RingPopOutcome, RingPushOutcome, RingState,
    RingSubmission, execute_ring_op,
};
use syscall::error::{EINVAL, EIO, ENOMEM};

use crate::rng::Rng;

const MAX_SCRIPTED_POP_FAULTS: usize = 4096;
const BLOCK_SIZE_USIZE: usize = BLOCK_SIZE as usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotState {
    Free,
    InUse,
    Quarantined,
}

#[derive(Default)]
struct PoolTracker {
    slots: Vec<SlotState>,
    inflight_offsets: BTreeMap<u32, u64>,
    alloc_calls: usize,
    released_slices: usize,
    quarantined_slices: usize,
    implicit_drops: usize,
    released_while_inflight: usize,
}

impl PoolTracker {
    fn new(pool_size: u32) -> Self {
        let count = (pool_size as usize) / BLOCK_SIZE_USIZE;
        Self {
            slots: vec![SlotState::Free; count],
            ..Self::default()
        }
    }

    fn alloc_slots(&mut self, len: u32) -> Option<(usize, usize)> {
        self.alloc_calls += 1;
        let blocks = (len as usize).div_ceil(BLOCK_SIZE_USIZE).max(1);
        if blocks > self.slots.len() {
            return None;
        }
        let mut run = 0usize;
        for idx in 0..self.slots.len() {
            if self.slots[idx] == SlotState::Free {
                run += 1;
                if run == blocks {
                    let start = idx + 1 - blocks;
                    for slot in &mut self.slots[start..start + blocks] {
                        *slot = SlotState::InUse;
                    }
                    return Some((start, blocks));
                }
            } else {
                run = 0;
            }
        }
        None
    }

    fn release_slots(&mut self, start: usize, blocks: usize, offset: u32, implicit: bool) {
        if self.inflight_offsets.contains_key(&offset) {
            self.released_while_inflight += 1;
        }
        for slot in &mut self.slots[start..start + blocks] {
            assert_eq!(*slot, SlotState::InUse);
            *slot = SlotState::Free;
        }
        if implicit {
            self.implicit_drops += 1;
        } else {
            self.released_slices += 1;
        }
    }

    fn quarantine_slots(&mut self, start: usize, blocks: usize) {
        for slot in &mut self.slots[start..start + blocks] {
            assert_eq!(*slot, SlotState::InUse);
            *slot = SlotState::Quarantined;
        }
        self.quarantined_slices += 1;
    }

    fn in_use_slots(&self) -> usize {
        self.slots
            .iter()
            .filter(|&&s| s == SlotState::InUse)
            .count()
    }

    fn quarantined_slot_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|&&s| s == SlotState::Quarantined)
            .count()
    }
}

struct SimBuf {
    tracker: Rc<RefCell<PoolTracker>>,
    start: usize,
    blocks: usize,
    offset: u32,
    data: Vec<u8>,
    handled: bool,
}

impl Drop for SimBuf {
    fn drop(&mut self) {
        if !self.handled {
            self.tracker
                .borrow_mut()
                .release_slots(self.start, self.blocks, self.offset, true);
        }
    }
}

#[derive(Clone, Debug)]
enum FaultAction {
    SpuriousCqe(u64),
    DuplicateLastCqe,
    PopEmpty,
    PopBroken,
    SqFullOnce,
    SqBroken,
    CqeStatus(u16),
    CqeShortCount,
}

struct SimRing {
    tracker: Rc<RefCell<PoolTracker>>,
    nr_sq: usize,
    nr_cq: usize,
    sq_queue: VecDeque<RingSubmission>,
    driver_inflight: Vec<RingSubmission>,
    cq_queue: VecDeque<RingCompletion>,
    disk: BTreeMap<u64, Vec<u8>>,
    buf_mem: BTreeMap<u32, Vec<u8>>,
    faults: VecDeque<FaultAction>,
    last_cqe: Option<RingCompletion>,
    reverse_cqe_order: bool,
    buffer_until_notify: bool,
    force_sq_full: bool,
    rng: Option<Rng>,
    push_calls: usize,
    pop_calls: usize,
    pop_when_empty: usize,
    max_inflight_seen: usize,
}

impl SimRing {
    fn new(nr_sq: u32, nr_cq: u32, pool_size: u32) -> Self {
        Self {
            tracker: Rc::new(RefCell::new(PoolTracker::new(pool_size))),
            nr_sq: nr_sq as usize,
            nr_cq: nr_cq as usize,
            sq_queue: VecDeque::new(),
            driver_inflight: Vec::new(),
            cq_queue: VecDeque::new(),
            disk: BTreeMap::new(),
            buf_mem: BTreeMap::new(),
            faults: VecDeque::new(),
            last_cqe: None,
            reverse_cqe_order: false,
            buffer_until_notify: false,
            force_sq_full: false,
            rng: None,
            push_calls: 0,
            pop_calls: 0,
            pop_when_empty: 0,
            max_inflight_seen: 0,
        }
    }

    fn total_inflight(&self) -> usize {
        self.sq_queue.len() + self.driver_inflight.len() + self.cq_queue.len()
    }

    fn drain_sq_to_driver(&mut self) {
        while let Some(sqe) = self.sq_queue.pop_front() {
            self.driver_inflight.push(sqe);
        }
    }

    fn complete_one_from_driver(&mut self) -> bool {
        if self.driver_inflight.is_empty() {
            self.drain_sq_to_driver();
        }
        if self.driver_inflight.is_empty() {
            return false;
        }
        if self.cq_queue.len() >= self.nr_cq {
            return false;
        }
        let idx = if let Some(rng) = self.rng.as_mut() {
            rng.below(self.driver_inflight.len() as u64) as usize
        } else if self.reverse_cqe_order {
            self.driver_inflight.len() - 1
        } else {
            0
        };
        let sqe = self.driver_inflight.remove(idx);
        let mut status = 0u16;
        let mut count = sqe.buf_len;
        if let Some(FaultAction::CqeStatus(err)) = self.faults.front().cloned() {
            self.faults.pop_front();
            status = err;
            count = 0;
        } else if matches!(self.faults.front(), Some(FaultAction::CqeShortCount)) {
            self.faults.pop_front();
            count = sqe.buf_len / 2;
        } else if sqe.op == RingOp::Write {
            if let Some(bytes) = self.buf_mem.get(&sqe.buf_offset).cloned() {
                self.disk.insert(sqe.block, bytes);
            }
        } else if let Some(stored) = self.disk.get(&sqe.block).cloned() {
            self.buf_mem.insert(sqe.buf_offset, stored);
        }
        self.tracker
            .borrow_mut()
            .inflight_offsets
            .remove(&sqe.buf_offset);
        self.cq_queue.push_back(RingCompletion {
            user_data: sqe.user_data,
            count,
            status,
        });
        true
    }
}

impl<'a> RingOps<'a> for SimRing {
    type Buffer = SimBuf;

    fn allocate(&mut self, len: u32) -> Option<Self::Buffer> {
        let (start, blocks) = self.tracker.borrow_mut().alloc_slots(len)?;
        let offset = (start * BLOCK_SIZE_USIZE) as u32;
        Some(SimBuf {
            tracker: Rc::clone(&self.tracker),
            start,
            blocks,
            offset,
            data: vec![0u8; len as usize],
            handled: false,
        })
    }

    fn buffer_offset(buf: &Self::Buffer) -> u32 {
        buf.offset
    }

    fn release(&mut self, mut buf: Self::Buffer) {
        buf.handled = true;
        self.tracker
            .borrow_mut()
            .release_slots(buf.start, buf.blocks, buf.offset, false);
        drop(buf);
    }

    fn quarantine(&mut self, mut buf: Self::Buffer) {
        buf.handled = true;
        self.tracker
            .borrow_mut()
            .quarantine_slots(buf.start, buf.blocks);
        core::mem::forget(buf);
    }

    fn push_sqe(&mut self, sqe: RingSubmission) -> RingPushOutcome {
        self.push_calls += 1;
        if self.force_sq_full {
            return RingPushOutcome::Full;
        }
        if matches!(self.faults.front(), Some(FaultAction::SqBroken)) {
            self.faults.pop_front();
            return RingPushOutcome::Broken;
        }
        if matches!(self.faults.front(), Some(FaultAction::SqFullOnce)) {
            self.faults.pop_front();
            return RingPushOutcome::Full;
        }
        if self.sq_queue.len() >= self.nr_sq {
            if self.buffer_until_notify {
                self.drain_sq_to_driver();
            } else {
                return RingPushOutcome::Full;
            }
        }
        self.tracker
            .borrow_mut()
            .inflight_offsets
            .insert(sqe.buf_offset, sqe.user_data);
        self.sq_queue.push_back(sqe);
        if self.buffer_until_notify {
            self.drain_sq_to_driver();
        }
        let inflight = self.total_inflight();
        if inflight > self.max_inflight_seen {
            self.max_inflight_seen = inflight;
        }
        RingPushOutcome::Pushed
    }

    fn notify_if_waiting(&mut self) {
        self.drain_sq_to_driver();
        if !self.buffer_until_notify {
            while self.cq_queue.len() < self.nr_cq && self.complete_one_from_driver() {}
        }
    }

    fn pop_cqe(&mut self) -> RingPopOutcome {
        self.pop_calls += 1;
        if self.total_inflight() == 0 {
            self.pop_when_empty += 1;
        }
        if self.pop_calls > MAX_SCRIPTED_POP_FAULTS + self.nr_cq * 4 {
            return RingPopOutcome::Broken;
        }
        match self.faults.front().cloned() {
            Some(FaultAction::SpuriousCqe(id)) => {
                self.faults.pop_front();
                return RingPopOutcome::Completion(RingCompletion {
                    user_data: id,
                    count: BLOCK_SIZE as u32,
                    status: 0,
                });
            }
            Some(FaultAction::DuplicateLastCqe) => {
                if let Some(cqe) = self.last_cqe {
                    self.faults.pop_front();
                    return RingPopOutcome::Completion(cqe);
                }
            }
            Some(FaultAction::PopEmpty) => {
                self.faults.pop_front();
                return RingPopOutcome::Empty;
            }
            Some(FaultAction::PopBroken) => {
                self.faults.pop_front();
                return RingPopOutcome::Broken;
            }
            _ => {}
        }
        if self.cq_queue.is_empty() {
            self.complete_one_from_driver();
        }
        match self.cq_queue.pop_front() {
            Some(cqe) => {
                self.last_cqe = Some(cqe);
                RingPopOutcome::Completion(cqe)
            }
            None => RingPopOutcome::Empty,
        }
    }
}

fn make_blocks(count: usize, seed_byte: u8) -> Vec<(u64, Vec<u8>)> {
    let mut batch = Vec::with_capacity(count);
    for i in 0..count {
        let mut block = vec![0u8; BLOCK_SIZE_USIZE];
        block[0] = seed_byte;
        block[1] = (i & 0xff) as u8;
        block[2] = ((i >> 8) & 0xff) as u8;
        batch.push((i as u64, block));
    }
    batch
}

fn check_case_a() -> Result<String, String> {
    let nr_sq = 1024;
    let nr_cq = 2048;
    let pool_size = 16 * 1024 * 1024;
    let count = 1500usize;
    let mut state = RingOpState::new(nr_sq, nr_cq, pool_size)
        .map_err(|err| format!("case(a): RingOpState::new failed: {err}"))?;
    if state.max_inflight() != 2048 {
        return Err(format!(
            "case(a): expected max_inflight=2048, got {}",
            state.max_inflight()
        ));
    }
    let mut sim = SimRing::new(nr_sq, nr_cq, pool_size);
    sim.buffer_until_notify = true;
    let batch = make_blocks(count, 0xA5);
    let expected_bytes = count * BLOCK_SIZE_USIZE;
    let res = execute_ring_op(
        &mut state,
        &mut sim,
        RingOp::Write,
        batch.into_iter(),
        |src, dst| dst.data.copy_from_slice(src),
        |_, _| {},
    );
    let processed = res.map_err(|err| {
        format!(
            "case(a): expected Ok({expected_bytes}), got Err({err}) after {} pops (max_inflight_seen={})",
            sim.pop_calls, sim.max_inflight_seen
        )
    })?;
    if processed != expected_bytes {
        return Err(format!(
            "case(a): processed {processed} != expected {expected_bytes}"
        ));
    }
    let tracker = sim.tracker.borrow();
    if state.state() != RingState::Live
        || tracker.in_use_slots() != 0
        || tracker.quarantined_slices != 0
        || tracker.implicit_drops != 0
        || sim.pop_when_empty != 0
    {
        return Err(format!(
            "case(a): invariant mismatch state={:?} in_use={} quarantined={} implicit_drops={} pop_empty={}",
            state.state(),
            tracker.in_use_slots(),
            tracker.quarantined_slices,
            tracker.implicit_drops,
            sim.pop_when_empty
        ));
    }
    Ok(format!(
        "case(a) constructed 1500-block drain (nr_sq={nr_sq}, nr_cq={nr_cq}, pool=16MiB): PASS ({processed} bytes, pops={}, max_inflight_seen={})",
        sim.pop_calls, sim.max_inflight_seen
    ))
}

fn check_case_b_spurious() -> Result<String, String> {
    let mut state = RingOpState::new(16, 32, 256 * 1024)
        .map_err(|err| format!("case(b1): init failed: {err}"))?;
    let mut sim = SimRing::new(16, 32, 256 * 1024);
    sim.buffer_until_notify = true;
    for _ in 0..2048 {
        sim.faults.push_back(FaultAction::SpuriousCqe(0xDEAD_BEEF));
    }
    let res = execute_ring_op(
        &mut state,
        &mut sim,
        RingOp::Write,
        make_blocks(4, 0xB1).into_iter(),
        |src, dst| dst.data.copy_from_slice(src),
        |_, _| {},
    );
    match res {
        Err(err) if err.errno == EIO => {}
        other => return Err(format!("case(b1): expected EIO, got {other:?}")),
    }
    if sim.pop_calls != 1 {
        return Err(format!(
            "case(b1): expected immediate EIO on pop 1, got {} pops",
            sim.pop_calls
        ));
    }
    if state.state() != RingState::Broken {
        return Err(format!(
            "case(b1): expected RingState::Broken, got {:?}",
            state.state()
        ));
    }
    {
        let tracker = sim.tracker.borrow();
        if tracker.quarantined_slices != 4
            || tracker.released_while_inflight != 0
            || tracker.implicit_drops != 0
            || tracker.quarantined_slot_count() != 4
        {
            return Err(format!(
                "case(b1): quarantine failed quarantined={} released_inflight={} implicit_drops={} q_slots={}",
                tracker.quarantined_slices,
                tracker.released_while_inflight,
                tracker.implicit_drops,
                tracker.quarantined_slot_count()
            ));
        }
    }
    let allocs_before = sim.tracker.borrow().alloc_calls;
    let pushes_before = sim.push_calls;
    let follow = execute_ring_op(
        &mut state,
        &mut sim,
        RingOp::Write,
        make_blocks(1, 0xB9).into_iter(),
        |src, dst| dst.data.copy_from_slice(src),
        |_, _| {},
    );
    match follow {
        Err(err) if err.errno == EIO => {}
        other => return Err(format!("case(b1): follow-up expected EIO, got {other:?}")),
    }
    if sim.tracker.borrow().alloc_calls != allocs_before || sim.push_calls != pushes_before {
        return Err("case(b1): Broken ring allocated or submitted on follow-up op".to_string());
    }
    Ok("case(b1) unknown user_data CQE: PASS (immediate EIO at pop 1, Broken, 4/4 slices quarantined, 0 released in-flight, follow-up rejected)".to_string())
}

fn check_case_b_duplicate() -> Result<String, String> {
    let mut state = RingOpState::new(16, 32, 256 * 1024)
        .map_err(|err| format!("case(b2): init failed: {err}"))?;
    let mut sim = SimRing::new(16, 32, 256 * 1024);
    sim.buffer_until_notify = true;
    for _ in 0..2048 {
        sim.faults.push_back(FaultAction::DuplicateLastCqe);
    }
    let res = execute_ring_op(
        &mut state,
        &mut sim,
        RingOp::Write,
        make_blocks(4, 0xB2).into_iter(),
        |src, dst| dst.data.copy_from_slice(src),
        |_, _| {},
    );
    match res {
        Err(err) if err.errno == EIO => {}
        other => return Err(format!("case(b2): expected EIO, got {other:?}")),
    }
    if sim.pop_calls != 2 {
        return Err(format!(
            "case(b2): expected immediate EIO on duplicate pop 2, got {} pops",
            sim.pop_calls
        ));
    }
    if state.state() != RingState::Broken {
        return Err(format!(
            "case(b2): expected RingState::Broken, got {:?}",
            state.state()
        ));
    }
    let tracker = sim.tracker.borrow();
    if tracker.released_slices != 1
        || tracker.quarantined_slices != 3
        || tracker.released_while_inflight != 0
        || tracker.implicit_drops != 0
    {
        return Err(format!(
            "case(b2): expected released=1 quarantined=3 released_inflight=0 implicit=0, got released={} quarantined={} released_inflight={} implicit={}",
            tracker.released_slices,
            tracker.quarantined_slices,
            tracker.released_while_inflight,
            tracker.implicit_drops
        ));
    }
    Ok("case(b2) duplicate user_data CQE: PASS (immediate EIO at pop 2, Broken, 1 completed released, 3/3 remaining quarantined)".to_string())
}

fn check_case_c() -> Result<String, String> {
    let mut state = RingOpState::new(16, 32, 4 * BLOCK_SIZE as u32)
        .map_err(|err| format!("case(c1): init failed: {err}"))?;
    let mut sim = SimRing::new(16, 32, 4 * BLOCK_SIZE as u32);
    let oversized = vec![(0u64, vec![0u8; 8 * BLOCK_SIZE_USIZE])];
    let res = execute_ring_op(
        &mut state,
        &mut sim,
        RingOp::Write,
        oversized.into_iter(),
        |src, dst| dst.data.copy_from_slice(src),
        |_, _| {},
    );
    match res {
        Err(err) if err.errno == ENOMEM => {}
        other => return Err(format!("case(c1): expected ENOMEM, got {other:?}")),
    }
    if state.state() != RingState::Live || sim.pop_calls != 0 || sim.pop_when_empty != 0 {
        return Err(format!(
            "case(c1): expected Live with 0 pops, got {:?} pops={}",
            state.state(),
            sim.pop_calls
        ));
    }
    let ok = execute_ring_op(
        &mut state,
        &mut sim,
        RingOp::Write,
        make_blocks(2, 0xC1).into_iter(),
        |src, dst| dst.data.copy_from_slice(src),
        |_, _| {},
    );
    if ok != Ok(2 * BLOCK_SIZE_USIZE) {
        return Err(format!("case(c1): follow-up after ENOMEM failed: {ok:?}"));
    }

    let mut state = RingOpState::new(16, 32, 256 * 1024)
        .map_err(|err| format!("case(c2): init failed: {err}"))?;
    let mut sim = SimRing::new(16, 32, 256 * 1024);
    sim.force_sq_full = true;
    let res = execute_ring_op(
        &mut state,
        &mut sim,
        RingOp::Write,
        make_blocks(1, 0xC2).into_iter(),
        |src, dst| dst.data.copy_from_slice(src),
        |_, _| {},
    );
    match res {
        Err(err) if err.errno == EIO => {}
        other => {
            return Err(format!(
                "case(c2): expected EIO on empty-pending SQ Full, got {other:?}"
            ));
        }
    }
    if state.state() != RingState::Broken
        || sim.pop_calls != 0
        || sim.tracker.borrow().released_slices != 1
        || sim.tracker.borrow().quarantined_slices != 0
    {
        return Err("case(c2): empty-pending SQ Full invariant failed".to_string());
    }

    let mut state = RingOpState::new(16, 32, 256 * 1024)
        .map_err(|err| format!("case(c3): init failed: {err}"))?;
    let mut sim = SimRing::new(16, 32, 256 * 1024);
    sim.buffer_until_notify = true;
    let mut batch = make_blocks(4, 0xC3);
    sim.faults.clear();
    let res = execute_ring_op(
        &mut state,
        &mut sim,
        RingOp::Write,
        batch.drain(..3),
        |src, dst| dst.data.copy_from_slice(src),
        |_, _| {},
    );
    if res != Ok(3 * BLOCK_SIZE_USIZE) {
        return Err(format!("case(c3): warm-up failed: {res:?}"));
    }
    let mut state = RingOpState::new(16, 32, 256 * 1024)
        .map_err(|err| format!("case(c3b): init failed: {err}"))?;
    let mut sim = SimRing::new(16, 32, 256 * 1024);
    sim.buffer_until_notify = true;
    let mut broken_sim = SqBrokenAt {
        inner: sim,
        fail_on_push: 4,
    };
    let res = execute_ring_op(
        &mut state,
        &mut broken_sim,
        RingOp::Write,
        make_blocks(4, 0xC3).into_iter(),
        |src, dst| dst.data.copy_from_slice(src),
        |_, _| {},
    );
    match res {
        Err(err) if err.errno == EIO => {}
        other => {
            return Err(format!(
                "case(c3): expected EIO on SQ Broken, got {other:?}"
            ));
        }
    }
    let t = broken_sim.inner.tracker.borrow();
    if state.state() != RingState::Broken
        || t.released_slices != 1
        || t.quarantined_slices != 3
        || t.released_while_inflight != 0
        || t.implicit_drops != 0
    {
        return Err(format!(
            "case(c3): SQ Broken expected released=1 quarantined=3, got released={} quarantined={} inflight_rel={} implicit={}",
            t.released_slices, t.quarantined_slices, t.released_while_inflight, t.implicit_drops
        ));
    }

    let mut state = RingOpState::new(16, 32, 256 * 1024)
        .map_err(|err| format!("case(c4): init failed: {err}"))?;
    let mut sim = SimRing::new(16, 32, 256 * 1024);
    sim.buffer_until_notify = true;
    for _ in 0..3 {
        sim.faults.push_back(FaultAction::PopEmpty);
    }
    let res = execute_ring_op(
        &mut state,
        &mut sim,
        RingOp::Write,
        make_blocks(2, 0xC4).into_iter(),
        |src, dst| dst.data.copy_from_slice(src),
        |_, _| {},
    );
    if res != Ok(2 * BLOCK_SIZE_USIZE) || state.state() != RingState::Live {
        return Err(format!(
            "case(c4): transient Empty retry failed: {res:?} {:?}",
            state.state()
        ));
    }

    let mut state = RingOpState::new(16, 32, 256 * 1024)
        .map_err(|err| format!("case(c5): init failed: {err}"))?;
    let mut sim = SimRing::new(16, 32, 256 * 1024);
    sim.buffer_until_notify = true;
    for _ in 0..MAX_EMPTY_POP_RETRIES {
        sim.faults.push_back(FaultAction::PopEmpty);
    }
    let res = execute_ring_op(
        &mut state,
        &mut sim,
        RingOp::Write,
        make_blocks(2, 0xC5).into_iter(),
        |src, dst| dst.data.copy_from_slice(src),
        |_, _| {},
    );
    match res {
        Err(err) if err.errno == EIO => {}
        other => {
            return Err(format!(
                "case(c5): expected EIO at empty bound, got {other:?}"
            ));
        }
    }
    if state.state() != RingState::Broken
        || sim.pop_calls != MAX_EMPTY_POP_RETRIES
        || sim.tracker.borrow().quarantined_slices != 2
        || sim.tracker.borrow().released_while_inflight != 0
    {
        return Err(format!(
            "case(c5): empty bound invariant failed pops={} state={:?} q={}",
            sim.pop_calls,
            state.state(),
            sim.tracker.borrow().quarantined_slices
        ));
    }

    let mut state = RingOpState::new(16, 32, 256 * 1024)
        .map_err(|err| format!("case(c6): init failed: {err}"))?;
    let mut sim = SimRing::new(16, 32, 256 * 1024);
    sim.buffer_until_notify = true;
    sim.faults.push_back(FaultAction::PopBroken);
    let res = execute_ring_op(
        &mut state,
        &mut sim,
        RingOp::Write,
        make_blocks(2, 0xC6).into_iter(),
        |src, dst| dst.data.copy_from_slice(src),
        |_, _| {},
    );
    match res {
        Err(err) if err.errno == EIO => {}
        other => {
            return Err(format!(
                "case(c6): expected EIO on PopBroken, got {other:?}"
            ));
        }
    }
    if state.state() != RingState::Broken
        || sim.pop_calls != 1
        || sim.tracker.borrow().quarantined_slices != 2
        || sim.tracker.borrow().released_while_inflight != 0
    {
        return Err("case(c6): PopBroken invariant failed".to_string());
    }

    let bad_params = [
        (0, 32, 4 * 1024 * 1024),
        (3, 32, 4 * 1024 * 1024),
        (RING_MAX_SQ_ENTRIES * 2, 32, 4 * 1024 * 1024),
        (16, 0, 4 * 1024 * 1024),
        (16, 30, 4 * 1024 * 1024),
        (16, RING_MAX_CQ_ENTRIES * 2, 4 * 1024 * 1024),
        (16, 32, 0),
        (16, 32, BLOCK_SIZE as u32 - 1),
        (16, 32, MAX_RING_POOL_SIZE + BLOCK_SIZE as u32),
    ];
    for (sq, cq, pool) in bad_params {
        match RingOpState::new(sq, cq, pool) {
            Err(err) if err.errno == EINVAL => {}
            other => {
                return Err(format!(
                    "case(c7): bad params ({sq}, {cq}, {pool}) expected EINVAL, got {other:?}"
                ));
            }
        }
    }
    let today = RingOpState::new(256, 512, 4 * 1024 * 1024)
        .map_err(|err| format!("case(c7): today params failed: {err}"))?;
    if today.max_inflight() != 512 {
        return Err(format!(
            "case(c7): today max_inflight expected 512, got {}",
            today.max_inflight()
        ));
    }
    let pool_bound = RingOpState::new(256, 512, 1024 * 1024)
        .map_err(|err| format!("case(c7): pool-limited params failed: {err}"))?;
    if pool_bound.max_inflight() != 256 {
        return Err(format!(
            "case(c7): pool-limited max_inflight expected 256, got {}",
            pool_bound.max_inflight()
        ));
    }

    Ok("case(c) edge cases (empty-pending ENOMEM/Live, empty-pending SQ Full/Broken, SQ Broken release+quarantine, transient/exhausted PopEmpty, PopBroken, param validation): PASS".to_string())
}

struct SqBrokenAt {
    inner: SimRing,
    fail_on_push: usize,
}

impl<'a> RingOps<'a> for SqBrokenAt {
    type Buffer = SimBuf;

    fn allocate(&mut self, len: u32) -> Option<Self::Buffer> {
        self.inner.allocate(len)
    }

    fn buffer_offset(buf: &Self::Buffer) -> u32 {
        SimRing::buffer_offset(buf)
    }

    fn release(&mut self, buf: Self::Buffer) {
        self.inner.release(buf);
    }

    fn quarantine(&mut self, buf: Self::Buffer) {
        self.inner.quarantine(buf);
    }

    fn push_sqe(&mut self, sqe: RingSubmission) -> RingPushOutcome {
        if self.inner.push_calls + 1 == self.fail_on_push {
            self.inner.push_calls += 1;
            return RingPushOutcome::Broken;
        }
        self.inner.push_sqe(sqe)
    }

    fn notify_if_waiting(&mut self) {
        self.inner.notify_if_waiting();
    }

    fn pop_cqe(&mut self) -> RingPopOutcome {
        self.inner.pop_cqe()
    }
}

fn run_seeded_case(seed: u64) -> Result<(), String> {
    let mut rng = Rng::new(seed);
    let sq_opts = [1u32, 2, 4, 8, 16, 64, 256, 1024];
    let nr_sq = *rng.pick(&sq_opts);
    let cq_mult = *rng.pick(&[1u32, 2, 4]);
    let nr_cq = (nr_sq * cq_mult).min(2048);
    let pool_blocks = *rng.pick(&[1u32, 2, 4, 8, 32, 128, 512, 1024, 2048, 4096]);
    let pool_size = pool_blocks * (BLOCK_SIZE as u32);
    let mut state = RingOpState::new(nr_sq, nr_cq, pool_size)
        .map_err(|err| format!("seed {seed}: init failed: {err}"))?;

    let mut sim = SimRing::new(nr_sq, nr_cq, pool_size);
    sim.rng = Some(Rng::new(seed ^ 0xA11CE));
    sim.buffer_until_notify = rng.chance(1, 2);

    let batch_len = rng.range(0, 64) as usize;
    let mut batch: Vec<(u64, Vec<u8>)> = Vec::with_capacity(batch_len);
    let mut total_expected = 0usize;
    let block_mults = [1usize, 1, 1, 2, 4, 8];
    for i in 0..batch_len {
        let mult = *rng.pick(&block_mults);
        let len = mult * BLOCK_SIZE_USIZE;
        let mut data = vec![0u8; len];
        data[0] = (seed & 0xff) as u8;
        data[1] = (i & 0xff) as u8;
        total_expected += len;
        batch.push((i as u64, data));
    }

    let fault_mode = rng.below(9);
    match fault_mode {
        0 | 1 | 2 => {}
        3 => {
            sim.faults.push_back(FaultAction::SqFullOnce);
        }
        4 => {
            sim.faults.push_back(FaultAction::CqeStatus(EIO as u16));
        }
        5 => {
            sim.faults.push_back(FaultAction::CqeShortCount);
        }
        6 => {
            for _ in 0..rng.range(1, 5) {
                sim.faults.push_back(FaultAction::PopEmpty);
            }
        }
        7 => {
            sim.faults
                .push_back(FaultAction::SpuriousCqe(0xFFFF_0000 + seed));
        }
        8 => {
            if rng.chance(1, 2) {
                sim.faults.push_back(FaultAction::PopBroken);
            } else {
                sim.faults.push_back(FaultAction::SqBroken);
            }
        }
        _ => unreachable!(),
    }

    let res = execute_ring_op(
        &mut state,
        &mut sim,
        RingOp::Write,
        batch.into_iter(),
        |src, dst| dst.data.copy_from_slice(src),
        |_, _| {},
    );

    if sim.pop_when_empty != 0 {
        return Err(format!("seed {seed}: pop_cqe called while inflight == 0"));
    }
    if sim.max_inflight_seen > state.max_inflight() {
        return Err(format!(
            "seed {seed}: max_inflight_seen {} > max_inflight {}",
            sim.max_inflight_seen,
            state.max_inflight()
        ));
    }
    {
        let tracker = sim.tracker.borrow();
        if tracker.released_while_inflight != 0 || tracker.implicit_drops != 0 {
            return Err(format!(
                "seed {seed}: released_while_inflight={} implicit_drops={}",
                tracker.released_while_inflight, tracker.implicit_drops
            ));
        }
        match state.state() {
            RingState::Live => {
                if tracker.in_use_slots() != 0 || tracker.quarantined_slices != 0 {
                    return Err(format!(
                        "seed {seed}: Live ring left in_use={} quarantined={}",
                        tracker.in_use_slots(),
                        tracker.quarantined_slices
                    ));
                }
                if fault_mode <= 2 && res.is_ok() && res != Ok(total_expected) {
                    return Err(format!(
                        "seed {seed}: Ok({:?}) != expected {total_expected}",
                        res
                    ));
                }
            }
            RingState::Broken => {
                if res.is_ok() {
                    return Err(format!("seed {seed}: Broken ring returned Ok"));
                }
                if tracker.in_use_slots() != 0 {
                    return Err(format!(
                        "seed {seed}: Broken ring left {} unquarantined in_use slots",
                        tracker.in_use_slots()
                    ));
                }
            }
        }
    }

    sim.faults.clear();
    sim.force_sq_full = false;
    let allocs_before = sim.tracker.borrow().alloc_calls;
    let pushes_before = sim.push_calls;
    let follow = execute_ring_op(
        &mut state,
        &mut sim,
        RingOp::Write,
        make_blocks(1, 0xD0).into_iter(),
        |src, dst| dst.data.copy_from_slice(src),
        |_, _| {},
    );
    match state.state() {
        RingState::Live => {
            if follow != Ok(BLOCK_SIZE_USIZE) {
                return Err(format!(
                    "seed {seed}: healthy follow-up on Live ring failed: {follow:?}"
                ));
            }
        }
        RingState::Broken => {
            match follow {
                Err(err) if err.errno == EIO => {}
                other => {
                    return Err(format!(
                        "seed {seed}: follow-up on Broken ring expected EIO, got {other:?}"
                    ));
                }
            }
            if sim.tracker.borrow().alloc_calls != allocs_before || sim.push_calls != pushes_before
            {
                return Err(format!(
                    "seed {seed}: Broken ring allocated or pushed on follow-up"
                ));
            }
        }
    }
    Ok(())
}

pub fn run_ringdrain(seeds: u64) -> usize {
    let mut failed = 0usize;
    for (name, res) in [
        ("case(a)", check_case_a()),
        ("case(b1)", check_case_b_spurious()),
        ("case(b2)", check_case_b_duplicate()),
        ("case(c)", check_case_c()),
    ] {
        match res {
            Ok(msg) => println!("ringdrain: {msg}"),
            Err(err) => {
                eprintln!("ringdrain: {name} FAIL: {err}");
                failed += 1;
            }
        }
    }
    let mut seed_failures = 0usize;
    for seed in 1..=seeds {
        if let Err(err) = run_seeded_case(seed) {
            if seed_failures < 5 {
                eprintln!("ringdrain: case(d) FAIL: {err}");
            }
            seed_failures += 1;
        }
    }
    if seed_failures == 0 {
        println!("ringdrain: case(d) {seeds}/{seeds} seeded fault-injection runs: PASS");
    } else {
        eprintln!("ringdrain: case(d) {seed_failures}/{seeds} seeded runs FAILED");
        failed += seed_failures;
    }
    failed
}

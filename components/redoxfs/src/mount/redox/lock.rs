use std::collections::BTreeMap;

use redox_scheme::{Id, Response, Tag, scheme::IntoTag};
use syscall::error::{
    EAGAIN, EBADF, EDEADLK, EINTR, EINVAL, ENOLCK, EOVERFLOW, ESRCH, Error, Result,
};
use syscall::flag::{O_ACCMODE, O_RDONLY, O_RDWR, O_WRONLY};

pub const OFFSET_MAX: u64 = i64::MAX as u64;
pub const LOCK_WAIT: u64 = 1;
pub const LOCK_FLOCK: u64 = 2;
const OFD_BIT: u64 = 1 << 63;
const EXCLUSIVE_BIT: u64 = 1 << 63;

const MAX_LOCKED_FILES: usize = 1024;
const MAX_RANGES_PER_FILE: usize = 512;
const MAX_FLOCKS_PER_FILE: usize = 512;
const MAX_RANGES_PER_PROCESS: usize = 1024;
const MAX_WAITERS: usize = 1024;
const MAX_WAITERS_PER_FILE: usize = 128;
const MAX_WAITERS_PER_PROCESS: usize = 64;
const MAX_DEADLOCK_STEPS: usize = 64;
const RANGE_GROWTH_PER_SET: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Process(usize),
    Handle(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Range {
        owner: Owner,
        exclusive: bool,
        start: u64,
        end: u64,
    },
    Flock {
        handle: usize,
        exclusive: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub owner: Owner,
    pub exclusive: bool,
    pub start: u64,
    pub end: u64,
}

impl Range {
    fn overlaps(&self, start: u64, end: u64) -> bool {
        self.start <= end && start <= self.end
    }

    fn conflicts(&self, owner: Owner, exclusive: bool, start: u64, end: u64) -> bool {
        self.owner != owner && self.overlaps(start, end) && (self.exclusive || exclusive)
    }

    fn touches(&self, start: u64, end: u64) -> bool {
        self.start <= end.saturating_add(1) && start <= self.end.saturating_add(1)
    }

    pub fn length(&self) -> u64 {
        if self.end == OFFSET_MAX {
            0
        } else {
            self.end - self.start + 1
        }
    }
}

#[derive(Default)]
struct FileLocks {
    ranges: Vec<Range>,
    flocks: Vec<(usize, bool)>,
}

impl FileLocks {
    fn is_empty(&self) -> bool {
        self.ranges.is_empty() && self.flocks.is_empty()
    }

    fn owned(&self, owner: Owner) -> usize {
        self.ranges
            .iter()
            .filter(|range| range.owner == owner)
            .count()
    }

    fn range_conflict(&self, owner: Owner, exclusive: bool, start: u64, end: u64) -> Option<Range> {
        self.ranges
            .iter()
            .find(|range| range.conflicts(owner, exclusive, start, end))
            .copied()
    }

    fn blocked(&self, request: &Request) -> bool {
        match *request {
            Request::Range {
                owner,
                exclusive,
                start,
                end,
            } => self.range_conflict(owner, exclusive, start, end).is_some(),
            Request::Flock { handle, exclusive } => self
                .flocks
                .iter()
                .any(|&(other, held)| other != handle && (held || exclusive)),
        }
    }

    fn set_range(&mut self, owner: Owner, kind: Option<bool>, start: u64, end: u64) {
        assert!(start <= end && end <= OFFSET_MAX, "lock range out of order");
        let mut kept = Vec::with_capacity(self.ranges.len() + RANGE_GROWTH_PER_SET);
        let (mut merged_start, mut merged_end) = (start, end);
        for range in self.ranges.drain(..) {
            if range.owner != owner {
                kept.push(range);
                continue;
            }
            if kind == Some(range.exclusive) && range.touches(merged_start, merged_end) {
                merged_start = merged_start.min(range.start);
                merged_end = merged_end.max(range.end);
                continue;
            }
            if !range.overlaps(start, end) {
                kept.push(range);
                continue;
            }
            if range.start < start {
                kept.push(Range {
                    end: start - 1,
                    ..range
                });
            }
            if range.end > end {
                kept.push(Range {
                    start: end + 1,
                    ..range
                });
            }
        }
        if let Some(exclusive) = kind {
            kept.push(Range {
                owner,
                exclusive,
                start: merged_start,
                end: merged_end,
            });
        }
        self.ranges = kept;
    }

    fn check(&self) {
        assert!(
            self.ranges.len() <= MAX_RANGES_PER_FILE,
            "file range table over its bound"
        );
        assert!(
            self.flocks.len() <= MAX_FLOCKS_PER_FILE,
            "file flock table over its bound"
        );
        for (index, a) in self.ranges.iter().enumerate() {
            assert!(
                a.start <= a.end && a.end <= OFFSET_MAX,
                "lock range out of order"
            );
            for b in &self.ranges[index + 1..] {
                if a.owner == b.owner {
                    assert!(!a.overlaps(b.start, b.end), "one owner's ranges overlap");
                    assert!(
                        a.exclusive != b.exclusive || !a.touches(b.start, b.end),
                        "one owner's ranges of one type left unmerged"
                    );
                } else {
                    assert!(
                        !a.conflicts(b.owner, b.exclusive, b.start, b.end),
                        "conflicting range locks granted together"
                    );
                }
            }
        }
        for (index, &(handle, exclusive)) in self.flocks.iter().enumerate() {
            assert!(
                self.flocks[index + 1..]
                    .iter()
                    .all(|&(other, _)| other != handle),
                "one handle holds two flocks"
            );
            assert!(
                !exclusive || self.flocks.len() == 1,
                "exclusive flock shared"
            );
        }
    }
}

struct Waiter {
    tag: Tag,
    node: u32,
    pid: usize,
    handle: usize,
    request: Request,
}

pub struct Reply {
    tag: Tag,
    result: Result<usize>,
    notify_on_detach: bool,
}

impl Reply {
    pub fn into_response(self) -> Response {
        if self.notify_on_detach {
            Response::new_notify_on_detach(self.result, self.tag)
        } else {
            Response::new(self.result, self.tag)
        }
    }
}

pub struct Call {
    pub node: u32,
    pub flags: usize,
    pub handle: usize,
    pub pid: usize,
}

#[derive(Default)]
pub struct LockTable {
    files: BTreeMap<u32, FileLocks>,
    process_ranges: BTreeMap<usize, usize>,
    waiters: Vec<Waiter>,
}

pub fn span(start: u64, len: u64) -> Result<(u64, u64)> {
    if start > OFFSET_MAX {
        return Err(Error::new(EINVAL));
    }
    if len == 0 {
        return Ok((start, OFFSET_MAX));
    }
    if len - 1 > OFFSET_MAX - start {
        return Err(Error::new(EOVERFLOW));
    }
    Ok((start, start + len - 1))
}

pub fn read_flags(payload: &[u8]) -> u64 {
    payload
        .get(..8)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u64::from_ne_bytes)
        .unwrap_or(0)
}

fn owner_of(call: &Call, arg1: u64) -> Owner {
    if arg1 & OFD_BIT == OFD_BIT {
        Owner::Handle(call.handle)
    } else {
        Owner::Process(call.pid)
    }
}

impl LockTable {
    pub fn has_process_locks(&self, node: u32) -> bool {
        self.files.get(&node).is_some_and(|file| {
            file.ranges
                .iter()
                .any(|range| matches!(range.owner, Owner::Process(_)))
        })
    }

    fn prune(&mut self, node: u32) {
        if self.files.get(&node).is_some_and(FileLocks::is_empty) {
            self.files.remove(&node);
        }
    }

    fn blocked(&self, node: u32, request: &Request) -> bool {
        self.files
            .get(&node)
            .is_some_and(|file| file.blocked(request))
    }

    fn admit(&self, node: u32, request: &Request, growth: usize) -> Result<()> {
        let file = self.files.get(&node);
        if file.is_none() && self.files.len() >= MAX_LOCKED_FILES {
            return Err(Error::new(ENOLCK));
        }
        match *request {
            Request::Range { owner, .. } => {
                if file.map_or(0, |file| file.ranges.len()) + growth > MAX_RANGES_PER_FILE {
                    return Err(Error::new(ENOLCK));
                }
                if let Owner::Process(pid) = owner
                    && self.process_ranges.get(&pid).copied().unwrap_or(0) + growth
                        > MAX_RANGES_PER_PROCESS
                {
                    return Err(Error::new(ENOLCK));
                }
            }
            Request::Flock { handle, .. } => {
                let others = file.map_or(0, |file| {
                    file.flocks
                        .iter()
                        .filter(|&&(other, _)| other != handle)
                        .count()
                });
                if others >= MAX_FLOCKS_PER_FILE {
                    return Err(Error::new(ENOLCK));
                }
            }
        }
        Ok(())
    }

    fn admit_waiter(&self, node: u32, pid: usize) -> Result<()> {
        let on_file = self
            .waiters
            .iter()
            .filter(|waiter| waiter.node == node)
            .count();
        let of_process = self
            .waiters
            .iter()
            .filter(|waiter| waiter.pid == pid)
            .count();
        if self.waiters.len() >= MAX_WAITERS
            || on_file >= MAX_WAITERS_PER_FILE
            || of_process >= MAX_WAITERS_PER_PROCESS
        {
            return Err(Error::new(ENOLCK));
        }
        Ok(())
    }

    fn edit(&mut self, node: u32, owner: Owner, kind: Option<bool>, start: u64, end: u64) {
        let file = self.files.entry(node).or_default();
        let before = file.owned(owner);
        file.set_range(owner, kind, start, end);
        let after = file.owned(owner);
        file.check();
        if let Owner::Process(pid) = owner {
            self.count_process(pid, before, after);
        }
    }

    fn count_process(&mut self, pid: usize, before: usize, after: usize) {
        let held = self.process_ranges.get(&pid).copied().unwrap_or(0);
        assert!(
            held >= before,
            "process range count below its ranges in one file"
        );
        let held = held - before + after;
        assert!(
            held <= MAX_RANGES_PER_PROCESS,
            "process range count over its bound"
        );
        if held == 0 {
            self.process_ranges.remove(&pid);
        } else {
            self.process_ranges.insert(pid, held);
        }
    }

    fn apply(&mut self, node: u32, request: &Request) {
        assert!(!self.blocked(node, request), "granting a blocked lock");
        match *request {
            Request::Range {
                owner,
                exclusive,
                start,
                end,
            } => self.edit(node, owner, Some(exclusive), start, end),
            Request::Flock { handle, exclusive } => {
                let file = self.files.entry(node).or_default();
                file.flocks.retain(|&(other, _)| other != handle);
                file.flocks.push((handle, exclusive));
                file.check();
            }
        }
        assert!(
            self.files.len() <= MAX_LOCKED_FILES,
            "lock table over its bound"
        );
    }

    fn release(&mut self, node: u32, owner: Owner) {
        let Some(file) = self.files.get_mut(&node) else {
            return;
        };
        let before = file.owned(owner);
        file.ranges.retain(|range| range.owner != owner);
        if let Owner::Handle(handle) = owner {
            file.flocks.retain(|&(other, _)| other != handle);
        }
        file.check();
        if let Owner::Process(pid) = owner {
            self.count_process(pid, before, 0);
        }
    }

    fn blocking_processes(&self, node: u32, request: &Request, found: &mut Vec<usize>) {
        let Request::Range {
            owner,
            exclusive,
            start,
            end,
        } = *request
        else {
            return;
        };
        let Some(file) = self.files.get(&node) else {
            return;
        };
        for range in &file.ranges {
            if let Owner::Process(pid) = range.owner
                && range.conflicts(owner, exclusive, start, end)
                && !found.contains(&pid)
            {
                found.push(pid);
            }
        }
    }

    fn deadlocks(&self, node: u32, pid: usize, request: &Request) -> bool {
        let mut pending = Vec::new();
        self.blocking_processes(node, request, &mut pending);
        let mut seen = Vec::new();
        for _ in 0..MAX_DEADLOCK_STEPS {
            let Some(holder) = pending.pop() else {
                return false;
            };
            if holder == pid {
                return true;
            }
            if seen.contains(&holder) {
                continue;
            }
            seen.push(holder);
            for waiter in &self.waiters {
                if let Request::Range {
                    owner: Owner::Process(waiting),
                    ..
                } = waiter.request
                    && waiting == holder
                {
                    self.blocking_processes(waiter.node, &waiter.request, &mut pending);
                }
            }
        }
        false
    }

    fn reply(&self, tag: Tag, node: u32, result: Result<usize>) -> Reply {
        Reply {
            tag,
            result,
            notify_on_detach: self.has_process_locks(node),
        }
    }

    pub fn reply_for(&self, tag: Tag, node: Option<u32>, result: Result<usize>) -> Reply {
        Reply {
            tag,
            result,
            notify_on_detach: node.is_some_and(|node| self.has_process_locks(node)),
        }
    }

    fn request_for(&self, call: &Call, arg1: u64, arg2: u64, flags: u64) -> Result<Request> {
        let exclusive = arg2 & EXCLUSIVE_BIT == EXCLUSIVE_BIT;
        if flags & LOCK_FLOCK == LOCK_FLOCK {
            return Ok(Request::Flock {
                handle: call.handle,
                exclusive,
            });
        }
        let readable = matches!(call.flags & O_ACCMODE, O_RDONLY | O_RDWR);
        let writable = matches!(call.flags & O_ACCMODE, O_WRONLY | O_RDWR);
        if (exclusive && !writable) || (!exclusive && !readable) {
            return Err(Error::new(EBADF));
        }
        let (start, end) = span(arg1 & !OFD_BIT, arg2 & !EXCLUSIVE_BIT)?;
        Ok(Request::Range {
            owner: owner_of(call, arg1),
            exclusive,
            start,
            end,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn lock(
        &mut self,
        call: &Call,
        tag: Tag,
        arg1: u64,
        arg2: u64,
        flags: u64,
        alive: &dyn Fn(usize) -> bool,
        replies: &mut Vec<Reply>,
    ) {
        let request = match self.request_for(call, arg1, arg2, flags) {
            Ok(request) => request,
            Err(err) => {
                replies.push(self.reply(tag, call.node, Err(err)));
                return;
            }
        };
        if let Request::Flock { handle, exclusive } = request
            && let Some(file) = self.files.get_mut(&call.node)
        {
            if file.flocks.contains(&(handle, exclusive)) {
                replies.push(self.reply(tag, call.node, Ok(0)));
                return;
            }
            let held = file.flocks.len();
            file.flocks.retain(|&(other, _)| other != handle);
            if file.flocks.len() != held {
                self.wake(call.node, alive, replies);
            }
        }
        if !self.blocked(call.node, &request) {
            let result = self.admit(call.node, &request, RANGE_GROWTH_PER_SET);
            if result.is_ok() {
                self.apply(call.node, &request);
            }
            replies.push(self.reply(tag, call.node, result.map(|()| 0)));
            return;
        }
        let refusal = if flags & LOCK_WAIT == 0 {
            Some(EAGAIN)
        } else if matches!(
            request,
            Request::Range {
                owner: Owner::Process(_),
                ..
            }
        ) && self.deadlocks(call.node, call.pid, &request)
        {
            Some(EDEADLK)
        } else if self.admit_waiter(call.node, call.pid).is_err() {
            Some(ENOLCK)
        } else {
            None
        };
        if let Some(errno) = refusal {
            self.prune(call.node);
            replies.push(self.reply(tag, call.node, Err(Error::new(errno))));
            return;
        }
        self.waiters.push(Waiter {
            tag,
            node: call.node,
            pid: call.pid,
            handle: call.handle,
            request,
        });
        assert!(
            self.waiters.len() <= MAX_WAITERS,
            "waiter list over its bound"
        );
    }

    pub fn unlock(
        &mut self,
        call: &Call,
        arg1: u64,
        arg2: u64,
        flags: u64,
        alive: &dyn Fn(usize) -> bool,
        replies: &mut Vec<Reply>,
    ) -> Result<usize> {
        if flags & LOCK_FLOCK == LOCK_FLOCK {
            if let Some(file) = self.files.get_mut(&call.node) {
                file.flocks.retain(|&(other, _)| other != call.handle);
            }
        } else {
            let (start, end) = span(arg1 & !OFD_BIT, arg2)?;
            let owner = owner_of(call, arg1);
            if self
                .files
                .get(&call.node)
                .is_some_and(|file| file.owned(owner) > 0)
            {
                let split = Request::Range {
                    owner,
                    exclusive: false,
                    start,
                    end,
                };
                self.admit(call.node, &split, 1)?;
                self.edit(call.node, owner, None, start, end);
            }
        }
        self.wake(call.node, alive, replies);
        Ok(0)
    }

    pub fn get_lock(&self, call: &Call, payload: &mut [u8]) -> Result<usize> {
        let words: &mut [u8; 16] = payload
            .get_mut(..16)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(Error::new(EINVAL))?;
        let arg1 = u64::from_ne_bytes(words[..8].try_into().unwrap());
        let arg2 = u64::from_ne_bytes(words[8..].try_into().unwrap());
        let (start, end) = span(arg1 & !OFD_BIT, arg2 & !EXCLUSIVE_BIT)?;
        let exclusive = arg2 & EXCLUSIVE_BIT == EXCLUSIVE_BIT;
        let holder = self
            .files
            .get(&call.node)
            .and_then(|file| file.range_conflict(owner_of(call, arg1), exclusive, start, end))
            .ok_or(Error::new(ESRCH))?;
        let len = holder.length() | if holder.exclusive { EXCLUSIVE_BIT } else { 0 };
        words[..8].copy_from_slice(&holder.start.to_ne_bytes());
        words[8..].copy_from_slice(&len.to_ne_bytes());
        Ok(match holder.owner {
            Owner::Process(pid) => pid,
            Owner::Handle(_) => 0,
        })
    }

    pub fn cancel(&mut self, id: Id, replies: &mut Vec<Reply>) {
        if let Some(index) = self
            .waiters
            .iter()
            .position(|waiter| waiter.tag.req_id() == id)
        {
            let waiter = self.waiters.remove(index);
            replies.push(self.reply(waiter.tag, waiter.node, Err(Error::new(EINTR))));
        }
    }

    pub fn on_detach(
        &mut self,
        node: u32,
        pid: usize,
        alive: &dyn Fn(usize) -> bool,
        replies: &mut Vec<Reply>,
    ) {
        self.release(node, Owner::Process(pid));
        self.wake(node, alive, replies);
    }

    pub fn on_close(
        &mut self,
        node: u32,
        handle: usize,
        alive: &dyn Fn(usize) -> bool,
        replies: &mut Vec<Reply>,
    ) {
        self.release(node, Owner::Handle(handle));
        let mut index = 0;
        while index < self.waiters.len() {
            if self.waiters[index].handle == handle && self.waiters[index].node == node {
                let waiter = self.waiters.remove(index);
                replies.push(self.reply(waiter.tag, waiter.node, Err(Error::new(EINTR))));
            } else {
                index += 1;
            }
        }
        self.wake(node, alive, replies);
    }

    fn wake(&mut self, node: u32, alive: &dyn Fn(usize) -> bool, replies: &mut Vec<Reply>) {
        let mut index = 0;
        while index < self.waiters.len() {
            let waiter = &self.waiters[index];
            if waiter.node != node || self.blocked(node, &waiter.request) {
                index += 1;
                continue;
            }
            let waiter = self.waiters.remove(index);
            let result = if alive(waiter.pid) {
                self.admit(node, &waiter.request, RANGE_GROWTH_PER_SET)
            } else {
                Err(Error::new(EINTR))
            };
            if result.is_ok() {
                self.apply(node, &waiter.request);
            }
            replies.push(self.reply(waiter.tag, node, result.map(|()| 0)));
        }
        self.prune(node);
    }
}

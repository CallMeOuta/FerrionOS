use std::collections::{BTreeMap, VecDeque};

use libredox::Fd;
use redox_scheme::scheme::{Op, SchemeResponse, SchemeState, SchemeSync, register_sync_scheme};
use redox_scheme::{CallerCtx, OpenResult, RequestKind, Response, SignalBehavior, Socket};
use syscall::error::*;
use syscall::flag::*;
use syscall::schemev2::NewFdFlags;
const O_CLOEXEC: usize = libredox::flag::O_CLOEXEC as usize;
use syscall::{Event, TimeSpec};

const MAX_OBJECTS: usize = 4096;
const MAX_PER_UID: usize = 1024;
const MAX_ALARMS: usize = 4096;
const MAX_ALARMS_PER_UID: usize = 1024;
const MAX_RETRY_PASSES: usize = MAX_OBJECTS + 1;
const COUNTER_MAX: u64 = u64::MAX - 1;
const NANO: u128 = 1_000_000_000;
const TOKEN_COUNTERS: usize = 1;
const TOKEN_TIMERS: usize = 2;
const FIRST_ALARM_TOKEN: usize = 3;

#[derive(Clone, Copy)]
enum Kind {
    Event,
    Timer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arm {
    Disarmed,
    Armed { deadline: u128, alarm: usize },
}

struct Timer {
    clock: usize,
    arm: Arm,
    interval: u128,
    count: u64,
}

impl Timer {
    fn expire_at(&mut self, deadline: u128, now: u128) -> Option<u128> {
        if now < deadline {
            return Some(deadline);
        }
        let (count, next) = if self.interval == 0 {
            (1u128, None)
        } else {
            let count = 1 + (now - deadline) / self.interval;
            let next = deadline.checked_add(count.saturating_mul(self.interval));
            (count, next)
        };
        self.count = self
            .count
            .saturating_add(count.min(COUNTER_MAX as u128) as u64);
        next
    }

    fn settings(&self, now: u128, buf: &mut [u8]) {
        let remaining = match self.arm {
            Arm::Disarmed => 0,
            Arm::Armed { deadline, .. } => deadline.saturating_sub(now),
        };
        for (chunk, nanos) in buf.chunks_exact_mut(16).zip([self.interval, remaining]) {
            chunk[..8].copy_from_slice(&((nanos / NANO) as i64).to_ne_bytes());
            chunk[8..].copy_from_slice(&((nanos % NANO) as i64).to_ne_bytes());
        }
    }
}

enum Value {
    Counter { count: u64, semaphore: bool },
    Timer(Timer),
}

struct Object {
    uid: u32,
    value: Value,
}

struct Alarm {
    object: usize,
    uid: u32,
    _fd: Fd,
}

struct Objects<'a> {
    kind: Kind,
    socket: &'a Socket,
    events: &'a Fd,
    time_root: &'a Fd,
    clocks: &'a [Fd; 2],
    objects: BTreeMap<usize, Object>,
    alarms: BTreeMap<usize, Alarm>,
    next_id: usize,
    next_alarm: usize,
}

impl<'a> Objects<'a> {
    fn new(
        kind: Kind,
        socket: &'a Socket,
        events: &'a Fd,
        time_root: &'a Fd,
        clocks: &'a [Fd; 2],
    ) -> Self {
        Self {
            kind,
            socket,
            events,
            time_root,
            clocks,
            objects: BTreeMap::new(),
            alarms: BTreeMap::new(),
            next_id: 1,
            next_alarm: FIRST_ALARM_TOKEN,
        }
    }

    fn assert_alarm_invariants(&self, uid: Option<u32>) {
        let armed = self
            .objects
            .values()
            .filter(|object| {
                matches!(
                    object.value,
                    Value::Timer(Timer {
                        arm: Arm::Armed { .. },
                        ..
                    })
                )
            })
            .count();
        assert_eq!(self.alarms.len(), armed);
        assert!(self.alarms.len() <= MAX_ALARMS);
        if let Some(uid) = uid {
            let per_uid = self
                .alarms
                .values()
                .filter(|alarm| alarm.uid == uid)
                .count();
            assert!(per_uid <= MAX_ALARMS_PER_UID);
        }
    }

    fn now(&self, clock: usize) -> Result<u128> {
        let index = if clock == CLOCK_REALTIME { 0 } else { 1 };
        let mut time = TimeSpec::default();
        if syscall::read(self.clocks[index].raw(), &mut time)? != size_of::<TimeSpec>() {
            return Err(Error::new(EIO));
        }
        Ok(time.tv_sec as u128 * NANO + time.tv_nsec as u128)
    }

    fn notify(&self, id: usize, flags: EventFlags) -> Result<()> {
        if self.socket.write_response(
            Response::post_fevent(id, flags.bits()),
            SignalBehavior::Restart,
        )? {
            Ok(())
        } else {
            Err(Error::new(EIO))
        }
    }

    fn rearm(&mut self, id: usize, deadline: Option<u128>) -> Result<()> {
        let (uid, clock, old_arm) = {
            let object = self.objects.get(&id).ok_or(Error::new(EBADF))?;
            let Value::Timer(timer) = &object.value else {
                return Err(Error::new(EINVAL));
            };
            (object.uid, timer.clock, timer.arm)
        };
        let old_token = match old_arm {
            Arm::Disarmed => None,
            Arm::Armed { alarm, .. } => Some(alarm),
        };
        let Some(deadline) = deadline else {
            if let Some(token) = old_token {
                let removed = self.alarms.remove(&token);
                assert!(removed.is_some());
            }
            if let Some(Object {
                value: Value::Timer(timer),
                ..
            }) = self.objects.get_mut(&id)
            {
                timer.arm = Arm::Disarmed;
            }
            self.assert_alarm_invariants(Some(uid));
            return Ok(());
        };
        let reused = usize::from(old_token.is_some());
        let uid_count = self
            .alarms
            .values()
            .filter(|alarm| alarm.uid == uid)
            .count();
        if self.alarms.len() - reused >= MAX_ALARMS || uid_count - reused >= MAX_ALARMS_PER_UID {
            return Err(Error::new(EAGAIN));
        }
        let time = TimeSpec {
            tv_sec: (deadline / NANO)
                .try_into()
                .map_err(|_| Error::new(EOVERFLOW))?,
            tv_nsec: (deadline % NANO) as i32,
        };
        let token = self.next_alarm;
        let next_alarm = token.checked_add(1).ok_or(Error::new(EOVERFLOW))?;
        let fd = self.time_root.openat(
            if clock == CLOCK_REALTIME {
                "1/timer"
            } else {
                "4/timer"
            },
            (O_RDWR | O_CLOEXEC) as i32,
            0,
        )?;
        subscribe(self.events, fd.raw(), token)?;
        if syscall::write(fd.raw(), &time)? != size_of::<TimeSpec>() {
            return Err(Error::new(EIO));
        }
        self.next_alarm = next_alarm;
        if let Some(old) = old_token {
            let removed = self.alarms.remove(&old);
            assert!(removed.is_some());
        }
        self.alarms.insert(
            token,
            Alarm {
                object: id,
                uid,
                _fd: fd,
            },
        );
        if let Some(Object {
            value: Value::Timer(timer),
            ..
        }) = self.objects.get_mut(&id)
        {
            timer.arm = Arm::Armed {
                deadline,
                alarm: token,
            };
        }
        self.assert_alarm_invariants(Some(uid));
        Ok(())
    }

    fn refresh(&mut self, id: usize) -> Result<()> {
        let (clock, armed_deadline) = match &self.objects.get(&id).ok_or(Error::new(EBADF))?.value {
            Value::Timer(timer) => match timer.arm {
                Arm::Disarmed => return Ok(()),
                Arm::Armed { deadline, .. } => (timer.clock, deadline),
            },
            Value::Counter { .. } => return Ok(()),
        };
        let now = self.now(clock)?;
        if now < armed_deadline {
            return Ok(());
        }
        let next = match &mut self.objects.get_mut(&id).unwrap().value {
            Value::Timer(timer) => timer.expire_at(armed_deadline, now),
            Value::Counter { .. } => return Ok(()),
        };
        self.rearm(id, next)
    }

    fn wake(&mut self, token: usize) -> Result<()> {
        let Some(alarm) = self.alarms.remove(&token) else {
            return Ok(());
        };
        let id = alarm.object;
        let uid = alarm.uid;
        let (clock, deadline) = {
            let object = self
                .objects
                .get_mut(&id)
                .expect("alarm points to live object");
            let Value::Timer(timer) = &mut object.value else {
                panic!("alarm points to timer");
            };
            let Arm::Armed {
                deadline,
                alarm: armed_token,
            } = timer.arm
            else {
                panic!("alarm points to armed timer");
            };
            assert_eq!(armed_token, token);
            timer.arm = Arm::Disarmed;
            (timer.clock, deadline)
        };
        self.assert_alarm_invariants(Some(uid));
        let Ok(now) = self.now(clock) else {
            return Ok(());
        };
        let (next, readable) = {
            let Value::Timer(timer) = &mut self.objects.get_mut(&id).unwrap().value else {
                unreachable!();
            };
            let next = timer.expire_at(deadline, now);
            (next, timer.count != 0)
        };
        if let Err(error) = self.rearm(id, next) {
            eprintln!("fdobjd: rearm failed on wake for timer {id}: {error}");
        }
        if readable {
            self.notify(id, EVENT_READ)?;
        }
        Ok(())
    }
}

impl SchemeSync for Objects<'_> {
    fn scheme_root(&mut self) -> Result<usize> {
        Ok(0)
    }

    fn openat(
        &mut self,
        dir: usize,
        path: &str,
        _flags: usize,
        _fcntl_flags: u32,
        ctx: &CallerCtx,
    ) -> Result<OpenResult> {
        if dir != 0 {
            return Err(Error::new(ENOTDIR));
        }
        if self.objects.len() >= MAX_OBJECTS
            || self
                .objects
                .values()
                .filter(|object| object.uid == ctx.uid)
                .count()
                >= MAX_PER_UID
        {
            return Err(Error::new(EMFILE));
        }
        let value = match self.kind {
            Kind::Event => {
                let (initial, mode) = path.split_once('/').ok_or(Error::new(EINVAL))?;
                let count = initial.parse::<u32>().map_err(|_| Error::new(EINVAL))? as u64;
                let semaphore = match mode {
                    "0" => false,
                    "1" => true,
                    _ => return Err(Error::new(EINVAL)),
                };
                Value::Counter { count, semaphore }
            }
            Kind::Timer => {
                let clock = path.parse::<usize>().map_err(|_| Error::new(EINVAL))?;
                if clock != CLOCK_MONOTONIC && clock != CLOCK_REALTIME {
                    return Err(Error::new(EINVAL));
                }
                Value::Timer(Timer {
                    clock,
                    arm: Arm::Disarmed,
                    interval: 0,
                    count: 0,
                })
            }
        };
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or(Error::new(EOVERFLOW))?;
        self.objects.insert(
            id,
            Object {
                uid: ctx.uid,
                value,
            },
        );
        Ok(OpenResult::ThisScheme {
            number: id,
            flags: NewFdFlags::empty(),
        })
    }

    fn read(
        &mut self,
        id: usize,
        buf: &mut [u8],
        _offset: u64,
        flags: u32,
        _ctx: &CallerCtx,
    ) -> Result<usize> {
        if buf.len() < 8 {
            return Err(Error::new(EINVAL));
        }
        self.refresh(id)?;
        let object = self.objects.get_mut(&id).ok_or(Error::new(EBADF))?;
        let (count, semaphore) = match &mut object.value {
            Value::Counter { count, semaphore } => (count, *semaphore),
            Value::Timer(timer) => (&mut timer.count, false),
        };
        if *count == 0 {
            return Err(blocked(flags));
        }
        let value = if semaphore { 1 } else { *count };
        *count -= value;
        buf[..8].copy_from_slice(&value.to_ne_bytes());
        let remaining = *count != 0;
        let mut ready = EventFlags::empty();
        if matches!(object.value, Value::Counter { .. }) {
            ready |= EVENT_WRITE;
        }
        if remaining {
            ready |= EVENT_READ;
        }
        self.notify(id, ready)?;
        Ok(8)
    }

    fn write(
        &mut self,
        id: usize,
        buf: &[u8],
        _offset: u64,
        flags: u32,
        _ctx: &CallerCtx,
    ) -> Result<usize> {
        if buf.len() != 8 {
            return Err(Error::new(EINVAL));
        }
        let value = u64::from_ne_bytes(buf.try_into().unwrap());
        if value == u64::MAX {
            return Err(Error::new(EINVAL));
        }
        let object = self.objects.get_mut(&id).ok_or(Error::new(EBADF))?;
        let Value::Counter { count, .. } = &mut object.value else {
            return Err(Error::new(EINVAL));
        };
        if value > COUNTER_MAX - *count {
            return Err(blocked(flags));
        }
        *count += value;
        if *count != 0 {
            self.notify(id, EVENT_READ)?;
        }
        Ok(8)
    }

    fn call(
        &mut self,
        id: usize,
        payload: &mut [u8],
        metadata: &[u64],
        _ctx: &CallerCtx,
    ) -> Result<usize> {
        if payload.len() != 32 {
            return Err(Error::new(EINVAL));
        }
        self.refresh(id)?;
        let clock = match &self.objects.get(&id).ok_or(Error::new(EBADF))?.value {
            Value::Timer(timer) => timer.clock,
            Value::Counter { .. } => return Err(Error::new(EINVAL)),
        };
        let now = self.now(clock)?;
        match metadata {
            [2, 0, 0] => {
                if let Value::Timer(current) = &self.objects.get(&id).unwrap().value {
                    current.settings(now, payload);
                }
                Ok(0)
            }
            [1, flags, 0] if *flags <= 1 => {
                let interval = decode_time(&payload[..16])?;
                let value = decode_time(&payload[16..])?;
                let initial_deadline = if value == 0 {
                    None
                } else if *flags == 1 {
                    Some(value)
                } else {
                    Some(now.checked_add(value).ok_or(Error::new(EOVERFLOW))?)
                };
                if let Some(deadline) = initial_deadline {
                    if deadline / NANO > i64::MAX as u128
                        || deadline
                            .checked_add(interval)
                            .is_none_or(|d| d / NANO > i64::MAX as u128)
                    {
                        return Err(Error::new(EOVERFLOW));
                    }
                }
                let mut old = [0; 32];
                let mut pending = Timer {
                    clock,
                    arm: Arm::Disarmed,
                    interval,
                    count: 0,
                };
                let next_deadline = match initial_deadline {
                    Some(deadline) => pending.expire_at(deadline, now),
                    None => None,
                };
                if let Value::Timer(current) = &self.objects.get(&id).unwrap().value {
                    current.settings(now, &mut old);
                }
                self.rearm(id, next_deadline)?;
                let readable = pending.count != 0;
                if let Value::Timer(timer) = &mut self.objects.get_mut(&id).unwrap().value {
                    timer.interval = pending.interval;
                    timer.count = pending.count;
                }
                payload.copy_from_slice(&old);
                if readable {
                    self.notify(id, EVENT_READ)?;
                }
                Ok(0)
            }
            _ => Err(Error::new(EINVAL)),
        }
    }

    fn fevent(&mut self, id: usize, _flags: EventFlags, _ctx: &CallerCtx) -> Result<EventFlags> {
        self.refresh(id)?;
        Ok(
            match &self.objects.get(&id).ok_or(Error::new(EBADF))?.value {
                Value::Counter { count, .. } => {
                    let mut flags = EventFlags::empty();
                    if *count != 0 {
                        flags |= EVENT_READ;
                    }
                    if *count < COUNTER_MAX {
                        flags |= EVENT_WRITE;
                    }
                    flags
                }
                Value::Timer(timer) if timer.count != 0 => EVENT_READ,
                Value::Timer(_) => EventFlags::empty(),
            },
        )
    }

    fn fpath(&mut self, id: usize, buf: &mut [u8], _ctx: &CallerCtx) -> Result<usize> {
        if id != 0 && !self.objects.contains_key(&id) {
            return Err(Error::new(EBADF));
        }
        let name: &[u8] = match self.kind {
            Kind::Event => b"/scheme/eventfd",
            Kind::Timer => b"/scheme/timerfd",
        };
        let len = buf.len().min(name.len());
        buf[..len].copy_from_slice(&name[..len]);
        Ok(len)
    }

    fn fcntl(&mut self, id: usize, cmd: usize, _arg: usize, _ctx: &CallerCtx) -> Result<usize> {
        if id != 0 && !self.objects.contains_key(&id) {
            return Err(Error::new(EBADF));
        }
        match cmd {
            F_GETFL | F_SETFL => Ok(0),
            _ => Err(Error::new(EINVAL)),
        }
    }

    fn on_close(&mut self, id: usize) {
        if let Some(object) = self.objects.remove(&id) {
            if let Value::Timer(Timer {
                arm: Arm::Armed { alarm, .. },
                ..
            }) = object.value
            {
                let removed = self.alarms.remove(&alarm);
                assert!(removed.is_some());
            }
            self.assert_alarm_invariants(Some(object.uid));
        }
    }
}

fn decode_time(bytes: &[u8]) -> Result<u128> {
    let sec = i64::from_ne_bytes(bytes[..8].try_into().unwrap());
    let nsec = i64::from_ne_bytes(bytes[8..].try_into().unwrap());
    if sec < 0 || !(0..1_000_000_000).contains(&nsec) {
        return Err(Error::new(EINVAL));
    }
    Ok(sec as u128 * NANO + nsec as u128)
}

fn blocked(flags: u32) -> Error {
    Error::new(if flags as usize & O_NONBLOCK != 0 {
        EAGAIN
    } else {
        EWOULDBLOCK
    })
}

fn subscribe(queue: &Fd, fd: usize, data: usize) -> Result<()> {
    syscall::write(
        queue.raw(),
        &Event {
            id: fd,
            flags: EVENT_READ,
            data,
        },
    )?;
    Ok(())
}

struct Requests {
    state: SchemeState,
    pending: VecDeque<(CallerCtx, Op)>,
}

impl Requests {
    fn new() -> Self {
        Self {
            state: SchemeState::new(),
            pending: VecDeque::new(),
        }
    }

    fn dispatch(&mut self, caller: CallerCtx, mut op: Op, scheme: &mut Objects<'_>) -> Result<()> {
        let response = match op.handle_sync_dont_consume(&caller, scheme, &mut self.state) {
            SchemeResponse::Regular(Err(Error { errno: EWOULDBLOCK }))
                if !op.is_explicitly_nonblock() && self.pending.len() < MAX_OBJECTS =>
            {
                self.pending.push_back((caller, op));
                return Ok(());
            }
            SchemeResponse::Regular(result) => Response::new(result, op),
            SchemeResponse::Opened(result) => Response::open_dup_like(result, op),
            SchemeResponse::RegularAndNotifyOnDetach(result) => {
                Response::new_notify_on_detach(result, op)
            }
        };
        if !scheme
            .socket
            .write_response(response, SignalBehavior::Restart)?
        {
            return Err(Error::new(EIO));
        }
        Ok(())
    }

    fn receive(&mut self, scheme: &mut Objects<'_>) -> Result<()> {
        loop {
            let request = match scheme.socket.next_request(SignalBehavior::Interrupt) {
                Ok(Some(request)) => request,
                Ok(None) => return Err(Error::new(EIO)),
                Err(Error {
                    errno: EAGAIN | EWOULDBLOCK | EINTR,
                }) => break,
                Err(error) => return Err(error),
            };
            match request.kind() {
                RequestKind::Call(call) => {
                    let caller = call.caller();
                    match call.op() {
                        Ok(op) => self.dispatch(caller, op, scheme)?,
                        Err(call) => {
                            scheme.socket.write_response(
                                Response::err(EOPNOTSUPP, call),
                                SignalBehavior::Restart,
                            )?;
                        }
                    }
                }
                RequestKind::Cancellation(cancel) => {
                    if let Some(index) = self
                        .pending
                        .iter()
                        .position(|(caller, _)| caller.id == cancel.id)
                    {
                        let (_, op) = self.pending.remove(index).unwrap();
                        scheme.socket.write_response(
                            Response::err(ECANCELED, op),
                            SignalBehavior::Restart,
                        )?;
                    }
                }
                RequestKind::OnClose { id } => scheme.on_close(id),
                RequestKind::SendFd(request) => {
                    scheme.socket.write_response(
                        Response::err(EOPNOTSUPP, request),
                        SignalBehavior::Restart,
                    )?;
                }
                RequestKind::RecvFd(request) => {
                    scheme.socket.write_response(
                        Response::open_dup_like(Err(Error::new(EOPNOTSUPP)), request),
                        SignalBehavior::Restart,
                    )?;
                }
                _ => {}
            }
        }
        self.retry(scheme)
    }

    fn retry(&mut self, scheme: &mut Objects<'_>) -> Result<()> {
        for _ in 0..MAX_RETRY_PASSES {
            let before = self.pending.len();
            for _ in 0..before {
                let (caller, op) = self.pending.pop_front().unwrap();
                self.dispatch(caller, op, scheme)?;
            }
            if self.pending.len() == before {
                return Ok(());
            }
        }
        Err(Error::new(EIO))
    }
}

fn run() -> Result<()> {
    let notify = std::env::var("INIT_NOTIFY")
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or(Error::new(EINVAL))?;
    let notify = Fd::new(notify);
    let events = Fd::open("/scheme/event", (O_RDWR | O_CLOEXEC) as i32, 0)?;
    let time_root = Fd::new(libredox::call::openat(
        libredox::call::getns()?,
        "/scheme/time",
        (O_DIRECTORY | O_CLOEXEC) as i32,
        0,
    )?);
    const {
        assert!(CLOCK_REALTIME == 1 && CLOCK_MONOTONIC == 4);
    }
    let clocks = [
        time_root.openat("1/gettime", (O_RDONLY | O_CLOEXEC) as i32, 0)?,
        time_root.openat("4/gettime", (O_RDONLY | O_CLOEXEC) as i32, 0)?,
    ];
    let event_socket = Socket::nonblock()?;
    let timer_socket = Socket::nonblock()?;
    let mut counters = Objects::new(Kind::Event, &event_socket, &events, &time_root, &clocks);
    let mut timers = Objects::new(Kind::Timer, &timer_socket, &events, &time_root, &clocks);
    register_sync_scheme(&event_socket, "eventfd", &mut counters)?;
    register_sync_scheme(&timer_socket, "timerfd", &mut timers)?;
    subscribe(&events, event_socket.inner().raw(), TOKEN_COUNTERS)?;
    subscribe(&events, timer_socket.inner().raw(), TOKEN_TIMERS)?;
    libredox::call::setrens(0, 0)?;
    notify.write(&[0])?;
    drop(notify);
    let mut counter_requests = Requests::new();
    let mut timer_requests = Requests::new();
    loop {
        let mut event = Event::default();
        match syscall::read(events.raw(), &mut event) {
            Err(Error { errno: EINTR }) => continue,
            Err(error) => return Err(error),
            Ok(len) if len != size_of::<Event>() => return Err(Error::new(EIO)),
            Ok(_) => {}
        }
        match event.data {
            TOKEN_COUNTERS => counter_requests.receive(&mut counters)?,
            TOKEN_TIMERS => timer_requests.receive(&mut timers)?,
            token => {
                timers.wake(token)?;
                timer_requests.retry(&mut timers)?;
            }
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("fdobjd: {error}");
        std::process::exit(1);
    }
}

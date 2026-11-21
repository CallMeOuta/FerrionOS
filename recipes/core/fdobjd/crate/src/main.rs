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
const COUNTER_MAX: u64 = u64::MAX - 1;
const NANO: u128 = 1_000_000_000;

#[derive(Clone, Copy)]
enum Kind {
    Event,
    Timer,
}

struct Timer {
    clock: usize,
    deadline: u128,
    interval: u128,
    count: u64,
    generation: u64,
}

impl Timer {
    fn advance(&mut self, now: u128) {
        if self.deadline == 0 || now < self.deadline {
            return;
        }
        let count = if self.interval == 0 {
            self.deadline = 0;
            1
        } else {
            let count = 1 + (now - self.deadline) / self.interval;
            self.deadline += count * self.interval;
            count
        };
        self.count = self
            .count
            .saturating_add(count.min(COUNTER_MAX as u128) as u64);
    }

    fn settings(&self, now: u128, buf: &mut [u8]) {
        for (chunk, nanos) in buf
            .chunks_exact_mut(16)
            .zip([self.interval, self.deadline.saturating_sub(now)])
        {
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
    generation: u64,
    uid: u32,
    // Retain obsolete generations until they fire: closing cannot cancel a kernel timeout.
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
            next_alarm: 3,
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

    fn refresh(&mut self, id: usize) -> Result<()> {
        let clock = match &self.objects.get(&id).ok_or(Error::new(EBADF))?.value {
            Value::Timer(timer) => timer.clock,
            Value::Counter { .. } => return Ok(()),
        };
        let now = self.now(clock)?;
        if let Value::Timer(timer) = &mut self.objects.get_mut(&id).unwrap().value {
            timer.advance(now);
        }
        Ok(())
    }

    fn schedule(&mut self, id: usize, timer: &Timer, uid: u32) -> Result<()> {
        if timer.deadline == 0 {
            return Ok(());
        }
        if self.alarms.len() >= MAX_ALARMS
            || self
                .alarms
                .values()
                .filter(|alarm| alarm.uid == uid)
                .count()
                >= MAX_ALARMS_PER_UID
        {
            return Err(Error::new(EAGAIN));
        }
        let token = self.next_alarm;
        self.next_alarm = token.checked_add(1).ok_or(Error::new(EOVERFLOW))?;
        let fd = self.time_root.openat(
            if timer.clock == CLOCK_REALTIME {
                "1/timer"
            } else {
                "4/timer"
            },
            (O_RDWR | O_CLOEXEC) as i32,
            0,
        )?;
        subscribe(self.events, fd.raw(), token)?;
        let time = TimeSpec {
            tv_sec: (timer.deadline / NANO)
                .try_into()
                .map_err(|_| Error::new(EOVERFLOW))?,
            tv_nsec: (timer.deadline % NANO) as i32,
        };
        if syscall::write(fd.raw(), &time)? != size_of::<TimeSpec>() {
            return Err(Error::new(EIO));
        }
        self.alarms.insert(
            token,
            Alarm {
                object: id,
                generation: timer.generation,
                uid,
                _fd: fd,
            },
        );
        Ok(())
    }

    fn wake(&mut self, token: usize) -> Result<()> {
        let Some(alarm) = self.alarms.remove(&token) else {
            return Ok(());
        };
        let Some(object) = self.objects.remove(&alarm.object) else {
            return Ok(());
        };
        let Object { uid, mut value } = object;
        let result = if let Value::Timer(timer) = &mut value {
            if timer.generation == alarm.generation {
                timer.advance(self.now(timer.clock)?);
                self.schedule(alarm.object, timer, uid)?;
                if timer.count != 0 {
                    self.notify(alarm.object, EVENT_READ)
                } else {
                    Ok(())
                }
            } else {
                Ok(())
            }
        } else {
            Ok(())
        };
        self.objects.insert(alarm.object, Object { uid, value });
        result
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
                    deadline: 0,
                    interval: 0,
                    count: 0,
                    generation: 0,
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
        let object = self.objects.get(&id).ok_or(Error::new(EBADF))?;
        let Value::Timer(current) = &object.value else {
            return Err(Error::new(EINVAL));
        };
        let now = self.now(current.clock)?;
        match metadata {
            [2, 0, 0] => {
                current.settings(now, payload);
                Ok(0)
            }
            [1, flags, 0] if *flags <= 1 => {
                let interval = decode_time(&payload[..16])?;
                let value = decode_time(&payload[16..])?;
                let deadline = if value == 0 {
                    0
                } else if *flags == 1 {
                    value
                } else {
                    now + value
                };
                // Reserve a representable next period before publishing an armed timer.
                // A hostile interval must not make the asynchronous rearm terminate the daemon.
                if deadline / NANO > i64::MAX as u128
                    || (deadline != 0 && (deadline + interval) / NANO > i64::MAX as u128)
                {
                    return Err(Error::new(EOVERFLOW));
                }
                let mut timer = Timer {
                    clock: current.clock,
                    deadline,
                    interval,
                    count: 0,
                    generation: current
                        .generation
                        .checked_add(1)
                        .ok_or(Error::new(EOVERFLOW))?,
                };
                timer.advance(now);
                let mut old = [0; 32];
                current.settings(now, &mut old);
                let uid = object.uid;
                self.schedule(id, &timer, uid)?;
                let readable = timer.count != 0;
                self.objects.get_mut(&id).unwrap().value = Value::Timer(timer);
                payload.copy_from_slice(&old);
                if readable {
                    self.notify(id, EVENT_READ)?;
                }
                Ok(0)
            }
            // CANCEL_ON_SET needs a kernel clock-change notification, which does not exist yet.
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
        self.objects.remove(&id);
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
        // Retrying may unblock an earlier writer after a later pending reader drains the counter.
        loop {
            let before = self.pending.len();
            for _ in 0..before {
                let (caller, op) = self.pending.pop_front().unwrap();
                self.dispatch(caller, op, scheme)?;
            }
            if self.pending.len() == before {
                return Ok(());
            }
        }
    }
}

fn run() -> Result<()> {
    let notify = std::env::var("INIT_NOTIFY")
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or(Error::new(EINVAL))?;
    let notify = Fd::new(notify);
    let events = Fd::open("/scheme/event", (O_RDWR | O_CLOEXEC) as i32, 0)?;
    // Opening the scheme pathname itself would open an empty clock name. Obtain
    // its root capability from the namespace before dropping namespace access.
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
    subscribe(&events, event_socket.inner().raw(), 1)?;
    subscribe(&events, timer_socket.inner().raw(), 2)?;
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
            1 => counter_requests.receive(&mut counters)?,
            2 => timer_requests.receive(&mut timers)?,
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

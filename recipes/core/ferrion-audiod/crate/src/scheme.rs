use redox_scheme::scheme::SchemeSync;
use redox_scheme::{CallerCtx, OpenResult};
use serde_json::json;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use syscall::schemev2::NewFdFlags;
use syscall::{
    Error, Result, Stat, EACCES, EBADF, EBUSY, EINVAL, ENOENT, EOVERFLOW, EWOULDBLOCK, MODE_CHR,
    MODE_DIR, MODE_FILE,
};

const PERIOD: usize = 512;
const CAPACITY: usize = 2048;
/// Frames a record stream keeps for a reader that is late; older frames are dropped.
const RECORD_CAPACITY: usize = 8192;
/// Present only on test images (config/audio-snoop.toml): enables the loopback input and grants
/// that hold for the server's lifetime. Read before the server enters its namespace.
pub const TEST_CONFIG: &str = "/etc/ferrion-audiod/test.conf";

#[derive(Clone, Copy)]
struct Gain {
    db: f32,
    factor: f32,
}
impl Gain {
    const UNITY: Self = Self {
        db: 0.0,
        factor: 1.0,
    };
    fn parse(bytes: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| Error::new(EINVAL))?
            .trim();
        if text == "mute" {
            return Ok(Self {
                db: f32::NEG_INFINITY,
                factor: 0.0,
            });
        }
        let db = text.parse::<f32>().map_err(|_| Error::new(EINVAL))?;
        if !db.is_finite() || db > 0.0 {
            return Err(Error::new(EINVAL));
        }
        Ok(Self {
            db,
            factor: 10.0f32.powf(db / 20.0),
        })
    }
    fn text(self) -> String {
        if self.factor == 0.0 {
            "mute\n".into()
        } else {
            format!("{}\n", self.db)
        }
    }
}
/// Who a grant is for: a program identity or a session, as the factory that made the stream names it.
#[derive(Clone, PartialEq)]
enum Holder {
    Program(String),
    Session(String),
}
/// What a grant lets its holder hear: the input device, or the streams of one named program.
#[derive(Clone, PartialEq)]
enum Hearing {
    Microphone,
    Tap(String),
}
#[derive(Clone, PartialEq)]
struct Grant {
    holder: Holder,
    hearing: Hearing,
}
impl Grant {
    /// `microphone program:<id>`, `microphone session:<id>`, `tap program:<id> <program>` or
    /// `tap session:<id> <program>`.
    fn parse(line: &str) -> Result<Self> {
        let words: Vec<_> = line.split_whitespace().collect();
        let holder = |word: &str| match word.split_once(':') {
            Some(("program", id)) if identity(id) => Ok(Holder::Program(id.into())),
            Some(("session", id)) if identity(id) => Ok(Holder::Session(id.into())),
            _ => Err(Error::new(EINVAL)),
        };
        match words.as_slice() {
            ["microphone", who] => Ok(Self {
                holder: holder(who)?,
                hearing: Hearing::Microphone,
            }),
            ["tap", who, target] if identity(target) => Ok(Self {
                holder: holder(who)?,
                hearing: Hearing::Tap((*target).into()),
            }),
            _ => Err(Error::new(EINVAL)),
        }
    }
    fn allows(&self, program: &str, session: &str, hearing: &Hearing) -> bool {
        self.hearing == *hearing
            && match &self.holder {
                Holder::Program(id) => id == program,
                Holder::Session(id) => id == session,
            }
    }
    fn json(&self, lease: &str) -> serde_json::Value {
        let (kind, holder) = match &self.holder {
            Holder::Program(id) => ("program", id),
            Holder::Session(id) => ("session", id),
        };
        let (hearing, target) = match &self.hearing {
            Hearing::Microphone => ("microphone", None),
            Hearing::Tap(target) => ("tap", Some(target)),
        };
        json!({"holder":format!("{kind}:{holder}"),"hearing":hearing,"target":target,"lease":lease})
    }
}
/// What the test configuration sets: the loopback input and grants without a lease.
pub struct TestConfig {
    loopback: bool,
    grants: Vec<Grant>,
}
impl TestConfig {
    pub fn parse(text: &str) -> std::result::Result<Self, String> {
        let mut config = Self {
            loopback: false,
            grants: Vec::new(),
        };
        for (number, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if line == "loopback" {
                config.loopback = true;
            } else {
                config
                    .grants
                    .push(Grant::parse(line).map_err(|_| format!("line {}: {line}", number + 1))?);
            }
        }
        Ok(config)
    }
}
/// A record stream: the input device's frames, or the frames of one program's streams, each
/// delivered only while a grant allows it and as zeros otherwise.
struct Record {
    program: String,
    session: String,
    hearing: Hearing,
    allowed: bool,
    cycle: Box<[[f32; 2]; PERIOD]>,
    samples: VecDeque<[f32; 2]>,
    dropped: u64,
}
struct Stream {
    program: String,
    session: String,
    gain: Gain,
    samples: VecDeque<[f32; 2]>,
    float: bool,
    closed: bool,
}
#[derive(Clone)]
enum Handle {
    Stream(usize),
    Record(usize),
    Loopback,
    Grants,
    StreamGain(usize),
    Factory(String, String),
    ProgramGain(String),
    SessionGain(String),
    Volume,
    Status,
    Stats,
    Apps,
}
pub struct State {
    handles: BTreeMap<usize, Handle>,
    streams: BTreeMap<usize, Stream>,
    records: BTreeMap<usize, Record>,
    /// The input of test images: what root writes to `loopback`. None on every other image.
    loopback: Option<VecDeque<[f32; 2]>>,
    /// Grants with the handle that leased them; None for the test configuration's.
    grants: Vec<(Grant, Option<usize>)>,
    /// The capture service's handle, the one that issues grants.
    capture: Option<usize>,
    programs: BTreeMap<String, Gain>,
    sessions: BTreeMap<String, Gain>,
    next: usize,
    volume: u32,
    device: Gain,
    cycles: u64,
    limited: u64,
}
impl State {
    pub fn new(test: Option<TestConfig>) -> Self {
        let (loopback, grants) = match test {
            Some(test) => (
                test.loopback.then(|| VecDeque::with_capacity(CAPACITY)),
                test.grants.into_iter().map(|grant| (grant, None)).collect(),
            ),
            None => (None, Vec::new()),
        };
        Self {
            handles: BTreeMap::new(),
            streams: BTreeMap::new(),
            records: BTreeMap::new(),
            loopback,
            grants,
            capture: None,
            programs: BTreeMap::new(),
            sessions: BTreeMap::new(),
            next: 1,
            volume: 50,
            device: Gain {
                db: 20.0 * 0.125f32.log10(),
                factor: 0.125,
            },
            cycles: 0,
            limited: 0,
        }
    }
    fn insert(&mut self, handle: Handle) -> Result<OpenResult> {
        let id = self.next;
        self.next = id.checked_add(1).ok_or(Error::new(EOVERFLOW))?;
        let flags = if matches!(
            handle,
            Handle::Stream(_) | Handle::Record(_) | Handle::Loopback
        ) {
            NewFdFlags::empty()
        } else {
            NewFdFlags::POSITIONED
        };
        self.handles.insert(id, handle);
        Ok(OpenResult::ThisScheme { number: id, flags })
    }
    fn stream(&mut self, program: String, session: String, float: bool) -> Result<OpenResult> {
        let id = self.next;
        let result = self.insert(Handle::Stream(id))?;
        self.streams.insert(
            id,
            Stream {
                program,
                session,
                gain: Gain::UNITY,
                samples: VecDeque::with_capacity(CAPACITY),
                float,
                closed: false,
            },
        );
        Ok(result)
    }
    /// Opens a record stream. It always opens: without a grant it delivers zeros.
    fn record(&mut self, program: String, session: String, hearing: Hearing) -> Result<OpenResult> {
        let id = self.next;
        let result = self.insert(Handle::Record(id))?;
        self.records.insert(
            id,
            Record {
                program,
                session,
                hearing,
                allowed: false,
                cycle: Box::new([[0.0; 2]; PERIOD]),
                samples: VecDeque::with_capacity(RECORD_CAPACITY),
                dropped: 0,
            },
        );
        Ok(result)
    }
    pub fn mix(&mut self) -> [[i16; 2]; PERIOD] {
        for record in self.records.values_mut() {
            record.allowed = self
                .grants
                .iter()
                .any(|(grant, _)| grant.allows(&record.program, &record.session, &record.hearing));
        }
        let mut sum = [[0.0f32; 2]; PERIOD];
        for stream in self.streams.values_mut() {
            let tapped = stream.gain.factor
                * self
                    .programs
                    .get(&stream.program)
                    .unwrap_or(&Gain::UNITY)
                    .factor
                * self
                    .sessions
                    .get(&stream.session)
                    .unwrap_or(&Gain::UNITY)
                    .factor;
            let gain = tapped * self.device.factor;
            let mut frames = [[0.0f32; 2]; PERIOD];
            let mut count = 0;
            while count < PERIOD {
                let Some(sample) = stream.samples.pop_front() else {
                    break;
                };
                frames[count] = sample;
                count += 1;
            }
            for (frame, sample) in sum.iter_mut().zip(&frames[..count]) {
                frame[0] += sample[0] * gain;
                frame[1] += sample[1] * gain;
            }
            // A tap hears the program as it plays, before the device's volume.
            for record in self.records.values_mut().filter(|record| {
                record.allowed
                    && matches!(&record.hearing, Hearing::Tap(target) if *target == stream.program)
            }) {
                for (frame, sample) in record.cycle.iter_mut().zip(&frames[..count]) {
                    frame[0] += sample[0] * tapped;
                    frame[1] += sample[1] * tapped;
                }
            }
        }
        let mut input = [[0.0f32; 2]; PERIOD];
        if let Some(loopback) = &mut self.loopback {
            for frame in &mut input {
                let Some(sample) = loopback.pop_front() else {
                    break;
                };
                *frame = sample;
            }
        }
        for record in self.records.values_mut() {
            if record.hearing == Hearing::Microphone && record.allowed {
                *record.cycle = input;
            }
            let overflow = (record.samples.len() + PERIOD).saturating_sub(RECORD_CAPACITY);
            record.samples.drain(..overflow);
            record.dropped += overflow as u64;
            // Without a grant the cycle stays zeros: silence, not an error.
            record.samples.extend(record.cycle.iter().copied());
            *record.cycle = [[0.0; 2]; PERIOD];
        }
        self.streams
            .retain(|_, stream| !stream.closed || !stream.samples.is_empty());
        let mut output = [[0i16; 2]; PERIOD];
        for (frame, dest) in sum.iter().zip(&mut output) {
            let mut peak = 1.0f32;
            for sample in frame {
                peak = peak.max(if *sample >= 0.0 {
                    *sample / 32767.0
                } else {
                    -*sample / 32768.0
                });
            }
            if peak > 1.0 {
                self.limited += 1;
            }
            dest[0] = (frame[0] / peak) as i16;
            dest[1] = (frame[1] / peak) as i16;
        }
        self.cycles += 1;
        output
    }
    fn text(&self, handle: &Handle) -> Result<String> {
        Ok(match handle {
            Handle::Volume => format!("{}\n", self.volume),
            Handle::StreamGain(id) => self.streams.get(id).ok_or(Error::new(EBADF))?.gain.text(),
            Handle::ProgramGain(id) => self.programs.get(id).unwrap_or(&Gain::UNITY).text(),
            Handle::SessionGain(id) => self.sessions.get(id).unwrap_or(&Gain::UNITY).text(),
            Handle::Status => json!({"rate":44100,"format":"s16le","channels":2,"volume":self.volume,"bit_perfect":false,"reason":"format","limiter_frames":self.limited,"streams":self.streams.len()}).to_string(),
            Handle::Stats => json!({"records":self.records.len(),"record_dropped_frames":self.records.values().map(|r| r.dropped).sum::<u64>(),"grants":self.grants.len(),"streams":self.streams.len(),"handles":self.handles.len(),"queued_frames":self.streams.values().map(|s| s.samples.len()).sum::<usize>(),"cycles":self.cycles,"limiter_frames":self.limited}).to_string(),
            Handle::Apps => {
                let mut apps: BTreeMap<&str, usize> = self.programs.keys().map(|key| (key.as_str(), 0)).collect();
                for stream in self.streams.values() { *apps.entry(&stream.program).or_default() += 1; }
                let rows: Vec<_> = apps.into_iter().map(|(identity, streams)| { let gain = self.programs.get(identity).unwrap_or(&Gain::UNITY); json!({"identity":identity,"gain_db":if gain.db.is_finite() {Some(gain.db)} else {None},"streams":streams}) }).collect();
                json!({"apps":rows}).to_string()
            }
            Handle::Grants => {
                let rows: Vec<_> = self.grants.iter().map(|(grant, lease)| grant.json(if lease.is_some() { "capture" } else { "test-config" })).collect();
                json!({"grants":rows}).to_string()
            }
            _ => return Err(Error::new(EBADF)),
        })
    }
}

/// Appends whole stereo frames, f32le (`float`) or s16le, at 16-bit scale to a queue of at most
/// CAPACITY frames. Returns the bytes taken, or EWOULDBLOCK while none fit.
fn push_frames(queue: &mut VecDeque<[f32; 2]>, buf: &[u8], float: bool) -> Result<usize> {
    let width = if float { 8 } else { 4 };
    if buf.len() % width != 0 {
        return Err(Error::new(EINVAL));
    }
    if buf.is_empty() {
        return Ok(0);
    }
    let count = CAPACITY.min(buf.len() / width);
    if CAPACITY - queue.len() < count {
        return Err(Error::new(EWOULDBLOCK));
    }
    let buf = &buf[..count * width];
    if float
        && buf.chunks_exact(4).any(|s| {
            let value = f32::from_le_bytes(s.try_into().unwrap());
            !value.is_finite() || value.abs() > 1.0
        })
    {
        return Err(Error::new(EINVAL));
    }
    for frame in buf.chunks_exact(width) {
        queue.push_back(if float {
            [
                f32::from_le_bytes(frame[..4].try_into().unwrap()) * 32768.0,
                f32::from_le_bytes(frame[4..].try_into().unwrap()) * 32768.0,
            ]
        } else {
            [
                i16::from_le_bytes(frame[..2].try_into().unwrap()) as f32,
                i16::from_le_bytes(frame[2..].try_into().unwrap()) as f32,
            ]
        });
    }
    Ok(count * width)
}

fn identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
}
/// `record` hears the input device; `tap/<program>` hears that program's streams.
fn hearing(path: &str) -> Option<Hearing> {
    match path.split_once('/') {
        None if path == "record" => Some(Hearing::Microphone),
        Some(("tap", target)) if identity(target) => Some(Hearing::Tap(target.into())),
        _ => None,
    }
}
fn admin(ctx: &CallerCtx) -> Result<()> {
    if ctx.uid == 0 {
        Ok(())
    } else {
        Err(Error::new(EACCES))
    }
}

pub struct AudioScheme(pub Arc<Mutex<State>>);
impl SchemeSync for AudioScheme {
    fn scheme_root(&mut self) -> Result<usize> {
        Ok(0)
    }
    fn openat(
        &mut self,
        dir: usize,
        path: &str,
        _flags: usize,
        _fcntl: u32,
        ctx: &CallerCtx,
    ) -> Result<OpenResult> {
        let mut state = self.0.lock().unwrap();
        let path = path.trim_matches('/');
        if dir != 0 {
            return match state.handles.get(&dir).cloned().ok_or(Error::new(EBADF))? {
                Handle::Stream(id) if path == "gain" => state.insert(Handle::StreamGain(id)),
                Handle::Factory(program, session) if path == "stream" || path.is_empty() => {
                    state.stream(program, session, path == "stream")
                }
                Handle::Factory(program, session) => match hearing(path) {
                    Some(hearing) => state.record(program, session, hearing),
                    None => Err(Error::new(ENOENT)),
                },
                _ => Err(Error::new(ENOENT)),
            };
        }
        match path {
            "" | "stream" => {
                return state.stream(
                    format!("pid-{}", ctx.pid),
                    format!("uid-{}", ctx.uid),
                    path == "stream",
                )
            }
            "volume" => return state.insert(Handle::Volume),
            "loopback" if state.loopback.is_none() => return Err(Error::new(ENOENT)),
            _ => {
                if let Some(hearing) = hearing(path) {
                    return state.record(
                        format!("pid-{}", ctx.pid),
                        format!("uid-{}", ctx.uid),
                        hearing,
                    );
                }
                admin(ctx)?
            }
        }
        let handle = match path {
            "loopback" => Handle::Loopback,
            "grants" if state.capture.is_some() => return Err(Error::new(EBUSY)),
            "grants" => {
                let result = state.insert(Handle::Grants)?;
                if let OpenResult::ThisScheme { number, .. } = result {
                    state.capture = Some(number);
                }
                return Ok(result);
            }
            "status" => Handle::Status,
            "stats" => Handle::Stats,
            "apps" => Handle::Apps,
            _ => {
                let parts: Vec<_> = path.split('/').collect();
                match parts.as_slice() {
                    ["factory", program, session] if identity(program) && identity(session) => {
                        Handle::Factory((*program).into(), (*session).into())
                    }
                    ["program", id, "gain"] if identity(id) => Handle::ProgramGain((*id).into()),
                    ["session", id, "gain"] if identity(id) => Handle::SessionGain((*id).into()),
                    _ => return Err(Error::new(ENOENT)),
                }
            }
        };
        state.insert(handle)
    }
    fn read(
        &mut self,
        id: usize,
        buf: &mut [u8],
        offset: u64,
        _flags: u32,
        _ctx: &CallerCtx,
    ) -> Result<usize> {
        let mut state = self.0.lock().unwrap();
        let record = match state.handles.get(&id).ok_or(Error::new(EBADF))? {
            Handle::Record(record) => Some(*record),
            _ => None,
        };
        if let Some(record) = record {
            let record = state.records.get_mut(&record).ok_or(Error::new(EBADF))?;
            if buf.len() % 8 != 0 {
                return Err(Error::new(EINVAL));
            }
            if buf.is_empty() {
                return Ok(0);
            }
            if record.samples.is_empty() {
                return Err(Error::new(EWOULDBLOCK));
            }
            let count = record.samples.len().min(buf.len() / 8);
            for (frame, out) in record.samples.drain(..count).zip(buf.chunks_exact_mut(8)) {
                out[..4].copy_from_slice(&(frame[0] / 32768.0).clamp(-1.0, 1.0).to_le_bytes());
                out[4..].copy_from_slice(&(frame[1] / 32768.0).clamp(-1.0, 1.0).to_le_bytes());
            }
            return Ok(count * 8);
        }
        let text = state.text(&state.handles[&id])?;
        let source = usize::try_from(offset)
            .ok()
            .and_then(|off| text.as_bytes().get(off..))
            .unwrap_or(&[]);
        let count = source.len().min(buf.len());
        buf[..count].copy_from_slice(&source[..count]);
        Ok(count)
    }
    fn write(
        &mut self,
        id: usize,
        buf: &[u8],
        offset: u64,
        _flags: u32,
        _ctx: &CallerCtx,
    ) -> Result<usize> {
        let mut state = self.0.lock().unwrap();
        let handle = state.handles.get(&id).cloned().ok_or(Error::new(EBADF))?;
        match handle {
            Handle::Stream(stream_id) => {
                let stream = state.streams.get_mut(&stream_id).ok_or(Error::new(EBADF))?;
                return push_frames(&mut stream.samples, buf, stream.float);
            }
            Handle::Loopback => {
                let loopback = state.loopback.as_mut().ok_or(Error::new(EBADF))?;
                return push_frames(loopback, buf, true);
            }
            Handle::Grants => {
                let text = std::str::from_utf8(buf).map_err(|_| Error::new(EINVAL))?;
                let mut lines = Vec::new();
                for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
                    lines.push(match line.strip_prefix("revoke ") {
                        Some(rest) => (Grant::parse(rest)?, true),
                        None => (Grant::parse(line)?, false),
                    });
                }
                for (grant, revoke) in lines {
                    if revoke {
                        state
                            .grants
                            .retain(|(held, lease)| !(*held == grant && *lease == Some(id)));
                    } else if !state.grants.contains(&(grant.clone(), Some(id))) {
                        state.grants.push((grant, Some(id)));
                    }
                }
                return Ok(buf.len());
            }
            _ => {}
        }
        if offset != 0 {
            return Err(Error::new(EINVAL));
        }
        match handle {
            Handle::Volume => {
                let volume = std::str::from_utf8(buf)
                    .map_err(|_| Error::new(EINVAL))?
                    .trim()
                    .parse::<u32>()
                    .map_err(|_| Error::new(EINVAL))?;
                if volume > 100 {
                    return Err(Error::new(EINVAL));
                }
                state.volume = volume;
                let factor = (volume as f32 / 100.0).powi(3);
                state.device = Gain {
                    db: 20.0 * factor.log10(),
                    factor,
                };
            }
            Handle::StreamGain(stream) => {
                state
                    .streams
                    .get_mut(&stream)
                    .ok_or(Error::new(EBADF))?
                    .gain = Gain::parse(buf)?
            }
            Handle::ProgramGain(program) => {
                let gain = Gain::parse(buf)?;
                if gain.db == 0.0 {
                    state.programs.remove(&program);
                } else {
                    state.programs.insert(program, gain);
                }
            }
            Handle::SessionGain(session) => {
                let gain = Gain::parse(buf)?;
                if gain.db == 0.0 {
                    state.sessions.remove(&session);
                } else {
                    state.sessions.insert(session, gain);
                }
            }
            _ => return Err(Error::new(EBADF)),
        }
        Ok(buf.len())
    }
    fn fstat(&mut self, id: usize, stat: &mut Stat, _ctx: &CallerCtx) -> Result<()> {
        let state = self.0.lock().unwrap();
        *stat = Stat::default();
        stat.st_mode = if id == 0 {
            MODE_DIR | 0o755
        } else {
            match state.handles.get(&id).ok_or(Error::new(EBADF))? {
                Handle::Factory(..) => MODE_DIR | 0o700,
                Handle::Stream(_) => MODE_CHR | 0o600,
                Handle::Loopback => MODE_CHR | 0o200,
                Handle::Record(_) => MODE_CHR | 0o400,
                handle => {
                    stat.st_size = state.text(handle)?.len() as u64;
                    MODE_FILE | 0o600
                }
            }
        };
        Ok(())
    }
    fn on_close(&mut self, id: usize) {
        let mut state = self.0.lock().unwrap();
        let handle = state.handles.remove(&id);
        if let Some(Handle::Record(record)) = handle {
            state.records.remove(&record);
        }
        if let Some(Handle::Grants) = handle {
            // The capture service's grants end with its handle: no holder, no samples.
            state.capture = None;
            state.grants.retain(|(_, lease)| *lease != Some(id));
        }
        if let Some(Handle::Stream(stream)) = handle {
            if let Some(node) = state.streams.get_mut(&stream) {
                node.closed = true;
            }
            if state
                .streams
                .get(&stream)
                .is_some_and(|node| node.samples.is_empty())
            {
                state.streams.remove(&stream);
            }
        }
    }
}

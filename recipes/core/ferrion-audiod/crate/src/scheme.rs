use redox_scheme::scheme::SchemeSync;
use redox_scheme::{CallerCtx, OpenResult};
use serde_json::json;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use syscall::schemev2::NewFdFlags;
use syscall::{
    Error, Result, Stat, EACCES, EBADF, EINVAL, ENOENT, EOVERFLOW, EWOULDBLOCK, MODE_CHR, MODE_DIR,
    MODE_FILE,
};

const PERIOD: usize = 512;
const CAPACITY: usize = 2048;

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
    programs: BTreeMap<String, Gain>,
    sessions: BTreeMap<String, Gain>,
    next: usize,
    volume: u32,
    device: Gain,
    cycles: u64,
    limited: u64,
}
impl State {
    pub fn new() -> Self {
        Self {
            handles: BTreeMap::new(),
            streams: BTreeMap::new(),
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
        let flags = if matches!(handle, Handle::Stream(_)) {
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
    pub fn mix(&mut self) -> [[i16; 2]; PERIOD] {
        let mut sum = [[0.0f32; 2]; PERIOD];
        for stream in self.streams.values_mut() {
            let gain = stream.gain.factor
                * self
                    .programs
                    .get(&stream.program)
                    .unwrap_or(&Gain::UNITY)
                    .factor
                * self
                    .sessions
                    .get(&stream.session)
                    .unwrap_or(&Gain::UNITY)
                    .factor
                * self.device.factor;
            for frame in &mut sum {
                let Some(sample) = stream.samples.pop_front() else {
                    break;
                };
                frame[0] += sample[0] * gain;
                frame[1] += sample[1] * gain;
            }
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
            Handle::Stats => json!({"streams":self.streams.len(),"handles":self.handles.len(),"queued_frames":self.streams.values().map(|s| s.samples.len()).sum::<usize>(),"cycles":self.cycles,"limiter_frames":self.limited}).to_string(),
            Handle::Apps => {
                let mut apps: BTreeMap<&str, usize> = self.programs.keys().map(|key| (key.as_str(), 0)).collect();
                for stream in self.streams.values() { *apps.entry(&stream.program).or_default() += 1; }
                let rows: Vec<_> = apps.into_iter().map(|(identity, streams)| { let gain = self.programs.get(identity).unwrap_or(&Gain::UNITY); json!({"identity":identity,"gain_db":if gain.db.is_finite() {Some(gain.db)} else {None},"streams":streams}) }).collect();
                json!({"apps":rows}).to_string()
            }
            _ => return Err(Error::new(EBADF)),
        })
    }
}

fn identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
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
            _ => admin(ctx)?,
        }
        let handle = match path {
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
        let state = self.0.lock().unwrap();
        let text = state.text(state.handles.get(&id).ok_or(Error::new(EBADF))?)?;
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
        if let Handle::Stream(stream_id) = handle {
            let stream = state.streams.get_mut(&stream_id).ok_or(Error::new(EBADF))?;
            let width = if stream.float { 8 } else { 4 };
            if buf.len() % width != 0 {
                return Err(Error::new(EINVAL));
            }
            if buf.is_empty() {
                return Ok(0);
            }
            let count = CAPACITY.min(buf.len() / width);
            if CAPACITY - stream.samples.len() < count {
                return Err(Error::new(EWOULDBLOCK));
            }
            if stream.float
                && buf[..count * width].chunks_exact(4).any(|s| {
                    let value = f32::from_le_bytes(s.try_into().unwrap());
                    !value.is_finite() || value.abs() > 1.0
                })
            {
                return Err(Error::new(EINVAL));
            }
            for frame in buf[..count * width].chunks_exact(width) {
                let sample = if stream.float {
                    [
                        f32::from_le_bytes(frame[..4].try_into().unwrap()) * 32768.0,
                        f32::from_le_bytes(frame[4..].try_into().unwrap()) * 32768.0,
                    ]
                } else {
                    [
                        i16::from_le_bytes(frame[..2].try_into().unwrap()) as f32,
                        i16::from_le_bytes(frame[2..].try_into().unwrap()) as f32,
                    ]
                };
                stream.samples.push_back(sample);
            }
            return Ok(count * width);
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
        if let Some(Handle::Stream(stream)) = state.handles.remove(&id) {
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

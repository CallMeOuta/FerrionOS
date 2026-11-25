use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use redox_scheme::scheme::SchemeSync;
use redox_scheme::{CallerCtx, OpenResult};
use syscall::schemev2::NewFdFlags;
use syscall::{
    Error, Result, Stat, EACCES, EBADF, EINVAL, EMFILE, ENOENT, EROFS, MODE_DIR, MODE_FILE,
};

pub const MAX_STATUS_BYTES: usize = 16 * 1024;
pub const MAX_IFADDRS_BYTES: usize = 4096;
pub const MAX_STATUS_HANDLES: usize = 256;
pub const MAX_CTL_HANDLES: usize = 32;
pub const MAX_CTL_WRITE_BYTES: usize = 128;

#[derive(Clone, Default)]
pub struct StatusSnapshot {
    pub status_json: String,
    pub ifaddrs_text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CtlCommand {
    Up,
    Down,
    Release,
    Reload,
}

pub fn parse_ctl_command(text: &str) -> Option<CtlCommand> {
    match text.trim() {
        "up" | "up eth0" => Some(CtlCommand::Up),
        "down" | "down eth0" => Some(CtlCommand::Down),
        "release" | "release eth0" => Some(CtlCommand::Release),
        "reload" => Some(CtlCommand::Reload),
        _ => None,
    }
}

enum StatusHandle {
    Root,
    Dir(Vec<u8>),
    File(Vec<u8>),
}

pub struct NetStatusScheme {
    snapshot: Rc<RefCell<StatusSnapshot>>,
    handles: BTreeMap<usize, StatusHandle>,
    next_id: usize,
}

impl NetStatusScheme {
    pub fn new(snapshot: Rc<RefCell<StatusSnapshot>>) -> Self {
        Self {
            snapshot,
            handles: BTreeMap::new(),
            next_id: 1,
        }
    }

    fn alloc(&mut self, handle: StatusHandle) -> Result<usize> {
        if self.handles.len() >= MAX_STATUS_HANDLES {
            return Err(Error::new(EMFILE));
        }
        for _ in 0..MAX_STATUS_HANDLES.saturating_add(4) {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1).max(1);
            if !self.handles.contains_key(&id) {
                self.handles.insert(id, handle);
                return Ok(id);
            }
        }
        Err(Error::new(EMFILE))
    }
}

impl SchemeSync for NetStatusScheme {
    fn scheme_root(&mut self) -> Result<usize> {
        self.alloc(StatusHandle::Root)
    }

    fn openat(
        &mut self,
        dirfd: usize,
        path: &str,
        _flags: usize,
        _fcntl_flags: u32,
        _ctx: &CallerCtx,
    ) -> Result<OpenResult> {
        if !matches!(self.handles.get(&dirfd), Some(StatusHandle::Root)) {
            return Err(Error::new(EACCES));
        }
        let clean = path.trim_matches('/');
        let handle = match clean {
            "" => StatusHandle::Dir(b"status\nifaddrs\n".to_vec()),
            "status" => {
                let mut bytes = self.snapshot.borrow().status_json.as_bytes().to_vec();
                if bytes.len() > MAX_STATUS_BYTES {
                    bytes.truncate(MAX_STATUS_BYTES);
                }
                StatusHandle::File(bytes)
            }
            "ifaddrs" => {
                let mut bytes = self.snapshot.borrow().ifaddrs_text.as_bytes().to_vec();
                if bytes.len() > MAX_IFADDRS_BYTES {
                    bytes.truncate(MAX_IFADDRS_BYTES);
                }
                StatusHandle::File(bytes)
            }
            _ => return Err(Error::new(ENOENT)),
        };
        let id = self.alloc(handle)?;
        Ok(OpenResult::ThisScheme {
            number: id,
            flags: NewFdFlags::POSITIONED,
        })
    }

    fn read(
        &mut self,
        id: usize,
        buf: &mut [u8],
        offset: u64,
        _fcntl_flags: u32,
        _ctx: &CallerCtx,
    ) -> Result<usize> {
        let data = match self.handles.get(&id) {
            Some(StatusHandle::File(b)) | Some(StatusHandle::Dir(b)) => b,
            _ => return Err(Error::new(EBADF)),
        };
        let Ok(off) = usize::try_from(offset) else {
            return Ok(0);
        };
        if off >= data.len() {
            return Ok(0);
        }
        let slice = &data[off..];
        let n = buf.len().min(slice.len());
        buf[..n].copy_from_slice(&slice[..n]);
        Ok(n)
    }

    fn write(
        &mut self,
        _id: usize,
        _buf: &[u8],
        _offset: u64,
        _fcntl_flags: u32,
        _ctx: &CallerCtx,
    ) -> Result<usize> {
        Err(Error::new(EROFS))
    }

    fn fstat(&mut self, id: usize, stat: &mut Stat, _ctx: &CallerCtx) -> Result<()> {
        match self.handles.get(&id) {
            Some(StatusHandle::Dir(b)) => {
                stat.st_mode = MODE_DIR | 0o555;
                stat.st_size = b.len() as u64;
                Ok(())
            }
            Some(StatusHandle::File(b)) => {
                stat.st_mode = MODE_FILE | 0o444;
                stat.st_size = b.len() as u64;
                Ok(())
            }
            _ => Err(Error::new(EBADF)),
        }
    }

    fn on_close(&mut self, id: usize) {
        self.handles.remove(&id);
    }
}

enum CtlHandle {
    Root,
    Ctl { buf: Vec<u8> },
}

pub struct NetCtlScheme<F>
where
    F: FnMut(CtlCommand) -> Result<()>,
{
    handler: F,
    handles: BTreeMap<usize, CtlHandle>,
    next_id: usize,
}

impl<F> NetCtlScheme<F>
where
    F: FnMut(CtlCommand) -> Result<()>,
{
    pub fn new(handler: F) -> Self {
        Self {
            handler,
            handles: BTreeMap::new(),
            next_id: 1,
        }
    }

    fn alloc(&mut self, handle: CtlHandle) -> Result<usize> {
        if self.handles.len() >= MAX_CTL_HANDLES {
            return Err(Error::new(EMFILE));
        }
        for _ in 0..MAX_CTL_HANDLES.saturating_add(4) {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1).max(1);
            if !self.handles.contains_key(&id) {
                self.handles.insert(id, handle);
                return Ok(id);
            }
        }
        Err(Error::new(EMFILE))
    }
}

impl<F> SchemeSync for NetCtlScheme<F>
where
    F: FnMut(CtlCommand) -> Result<()>,
{
    fn scheme_root(&mut self) -> Result<usize> {
        self.alloc(CtlHandle::Root)
    }

    fn openat(
        &mut self,
        dirfd: usize,
        path: &str,
        _flags: usize,
        _fcntl_flags: u32,
        ctx: &CallerCtx,
    ) -> Result<OpenResult> {
        if ctx.uid != 0 {
            return Err(Error::new(EACCES));
        }
        if !matches!(self.handles.get(&dirfd), Some(CtlHandle::Root)) {
            return Err(Error::new(EACCES));
        }
        if path.trim_matches('/') != "ctl" {
            return Err(Error::new(ENOENT));
        }
        let id = self.alloc(CtlHandle::Ctl { buf: Vec::new() })?;
        Ok(OpenResult::ThisScheme {
            number: id,
            flags: NewFdFlags::empty(),
        })
    }

    fn read(
        &mut self,
        _id: usize,
        _buf: &mut [u8],
        _offset: u64,
        _fcntl_flags: u32,
        ctx: &CallerCtx,
    ) -> Result<usize> {
        if ctx.uid != 0 {
            return Err(Error::new(EACCES));
        }
        Ok(0)
    }

    fn write(
        &mut self,
        id: usize,
        buf: &[u8],
        _offset: u64,
        _fcntl_flags: u32,
        ctx: &CallerCtx,
    ) -> Result<usize> {
        if ctx.uid != 0 {
            return Err(Error::new(EACCES));
        }
        let cmd = {
            let Some(CtlHandle::Ctl { buf: acc }) = self.handles.get_mut(&id) else {
                return Err(Error::new(EBADF));
            };
            if acc.len().saturating_add(buf.len()) > MAX_CTL_WRITE_BYTES {
                return Err(Error::new(EINVAL));
            }
            acc.extend_from_slice(buf);
            if acc.contains(&b'\n') {
                let text = std::str::from_utf8(acc)
                    .map_err(|_| Error::new(EINVAL))?
                    .to_owned();
                acc.clear();
                Some(parse_ctl_command(&text).ok_or_else(|| Error::new(EINVAL))?)
            } else {
                None
            }
        };
        if let Some(cmd) = cmd {
            (self.handler)(cmd)?;
        }
        Ok(buf.len())
    }

    fn fstat(&mut self, id: usize, stat: &mut Stat, ctx: &CallerCtx) -> Result<()> {
        if ctx.uid != 0 {
            return Err(Error::new(EACCES));
        }
        match self.handles.get(&id) {
            Some(CtlHandle::Ctl { .. }) => {
                stat.st_mode = MODE_FILE | 0o200;
                stat.st_size = 0;
                Ok(())
            }
            _ => Err(Error::new(EBADF)),
        }
    }

    fn on_close(&mut self, id: usize) {
        if let Some(CtlHandle::Ctl { buf }) = self.handles.remove(&id) {
            if !buf.is_empty() {
                if let Ok(text) = std::str::from_utf8(&buf) {
                    if let Some(cmd) = parse_ctl_command(text) {
                        let _ = (self.handler)(cmd);
                    }
                }
            }
        }
    }
}

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{PermissionsExt, symlink};

use redox_scheme::scheme::{SchemeState, SchemeSync};
use redox_scheme::{CallerCtx, OpenResult, RequestKind, Response, SignalBehavior, Socket};
use syscall::data::{Stat, StatVfs, StdFsCallMeta, TimeSpec};
use syscall::dirent::{DirEntry, DirentBuf, DirentHeader, DirentIter, DirentKind};
use syscall::error::{
    EBADF, EBUSY, EINVAL, EIO, EISDIR, EMFILE, ENOENT, ENOTDIR, ENOTEMPTY, EOPNOTSUPP, EPERM,
    EROFS, ESTALE, EXDEV, Error, Result,
};
use syscall::flag::{
    CallFlags, EventFlags, MODE_DIR, MODE_SYMLINK, MODE_TYPE, MapFlags, MunmapFlags, O_ACCMODE,
    O_CREAT, O_DIRECTORY, O_EXCL, O_NOFOLLOW, O_RDONLY, O_RDWR, O_STAT, O_SYMLINK, O_TRUNC,
    O_WRONLY, StdFsCallKind,
};
use syscall::schemev2::NewFdFlags;

use crate::overlay::{Lookup, Overlay, WHITEOUTS, rel_parent};
use crate::scope::{
    Access, DirCache, Kind, MAX_LINKS, MAX_PATH, Place, Scope, View, Walked, join, parent, retarget,
};

pub const CONTROL: usize = 1;
pub const END_SESSION: &[u8] = b"end";
const MAX_SESSIONS: usize = 64;
const MAX_HANDLES: usize = 1 << 16;
const MAX_LISTING: usize = 1 << 16;
const MAX_MAPS: usize = 4096;
const MAX_CREATE_TRIES: usize = 4;
const MAX_RECORDED_LINKS: usize = 4096;
const AT_REMOVEDIR: usize = 0x200;

fn err(e: i32) -> Error {
    Error::new(e)
}

fn last_error() -> Error {
    err(std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(EIO))
}

fn cpath(path: &str) -> Result<CString> {
    CString::new(path).map_err(|_| err(EINVAL))
}

fn check(r: i32) -> Result<()> {
    if r < 0 { Err(last_error()) } else { Ok(()) }
}

fn sys_open(real: &str, flags: usize) -> Result<OwnedFd> {
    let c = cpath(real)?;
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            (flags & 0xFFFF_0000) as libc::c_int | libc::O_CLOEXEC,
            (flags & 0xFFFF) as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(last_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn raw_stat(fd: usize, stat: &mut Stat) -> Result<()> {
    let bytes =
        unsafe { std::slice::from_raw_parts_mut(stat as *mut Stat as *mut u8, size_of::<Stat>()) };
    syscall::std_fs_call(fd, bytes, &StdFsCallMeta::new(StdFsCallKind::Fstat, 0, 0))?;
    Ok(())
}

fn lkind(real: &str) -> Result<Kind> {
    let c = cpath(real)?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    check(unsafe { libc::lstat(c.as_ptr(), &mut st) })?;
    Ok(match st.st_mode & libc::S_IFMT {
        libc::S_IFDIR => Kind::Dir,
        libc::S_IFLNK => Kind::Symlink,
        _ => Kind::File,
    })
}

fn readlink(real: &str) -> Result<String> {
    let c = cpath(real)?;
    let mut buf = vec![0u8; MAX_PATH];
    let n = unsafe { libc::readlink(c.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        return Err(last_error());
    }
    buf.truncate(n as usize);
    String::from_utf8(buf).map_err(|_| err(ENOENT))
}

fn exists(real: &str) -> bool {
    lkind(real).is_ok()
}

fn dirent_kind(kind: Kind) -> DirentKind {
    match kind {
        Kind::Dir => DirentKind::Directory,
        Kind::File => DirentKind::Regular,
        Kind::Symlink => DirentKind::Symlink,
    }
}

fn list_real(real: &str, out: &mut BTreeMap<String, (DirentKind, u64)>) -> Result<()> {
    use std::os::unix::fs::DirEntryExt;
    for entry in fs::read_dir(real).map_err(|e| err(e.raw_os_error().unwrap_or(EIO)))? {
        let entry = entry.map_err(|e| err(e.raw_os_error().unwrap_or(EIO)))?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if out.len() >= MAX_LISTING {
            return Err(err(EIO));
        }
        let kind = lkind(&join(real, &name))
            .map(dirent_kind)
            .unwrap_or(DirentKind::Unspecified);
        out.insert(name, (kind, entry.ino()));
    }
    Ok(())
}

struct Node {
    session: u32,
    vpath: String,
    fd: Option<OwnedFd>,
    listing: Option<Vec<(String, DirentKind, u64)>>,
    grant: Option<usize>,
    upper: bool,
    flags: usize,
}

enum Handle {
    Control,
    Ctl {
        session: u32,
        spec: Vec<u8>,
        read: usize,
    },
    Root {
        session: u32,
    },
    External {
        session: u32,
        path: String,
    },
    Node(Node),
}

impl Handle {
    fn session(&self) -> Option<u32> {
        match self {
            Handle::Control => None,
            Handle::Ctl { session, .. }
            | Handle::Root { session }
            | Handle::External { session, .. } => Some(*session),
            Handle::Node(node) => Some(node.session),
        }
    }
}

struct Session {
    scope: Option<Scope>,
    overlays: Vec<Option<Overlay>>,
    handles: usize,
    ended: bool,
    links: BTreeSet<String>,
    links_dropped: usize,
}

impl Session {
    fn record_link(&mut self, real: String) {
        if self.links.len() >= MAX_RECORDED_LINKS && !self.links.contains(&real) {
            self.links_dropped += 1;
            return;
        }
        self.links.insert(real);
        debug_assert!(self.links.len() <= MAX_RECORDED_LINKS);
    }

    fn moved_links(&mut self, from: &str, to: &str) {
        let moved: Vec<String> = self
            .links
            .iter()
            .filter(|p| crate::scope::within(p, from))
            .cloned()
            .collect();
        for p in moved {
            self.links.remove(&p);
            self.links.insert(format!("{to}{}", &p[from.len()..]));
        }
    }

    fn links_report(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for p in &self.links {
            out.extend_from_slice(p.as_bytes());
            out.push(b'\n');
        }
        if self.links_dropped > 0 {
            out.extend_from_slice(format!("+{}\n", self.links_dropped).as_bytes());
        }
        out
    }
}

struct RealView<'a> {
    dirs: &'a mut DirCache,
    overlays: &'a [Option<Overlay>],
}

enum Located {
    Real(String, bool),
    Missing,
}

fn locate(scope: &Scope, overlays: &[Option<Overlay>], grant: usize, rel: &str) -> Located {
    let g = &scope.grants[grant];
    let lower = join(&g.real, rel);
    match (&overlays[grant], &g.layer) {
        (Some(ov), Some(layer)) => match ov.lookup(rel, || exists(&lower)) {
            Lookup::Upper => Located::Real(join(layer, rel), true),
            Lookup::Lower => Located::Real(lower, false),
            Lookup::Missing => Located::Missing,
        },
        _ => Located::Real(lower, false),
    }
}

impl View for RealView<'_> {
    fn kind(&mut self, scope: &Scope, grant: usize, rel: &str) -> std::result::Result<Kind, i32> {
        let (real, cacheable) = match locate(scope, self.overlays, grant, rel) {
            Located::Missing => return Err(ENOENT),
            Located::Real(real, _) => (real, scope.grants[grant].access != Access::Clone),
        };
        if cacheable && self.dirs.contains(&real) {
            return Ok(Kind::Dir);
        }
        let kind = lkind(&real).map_err(|e| e.errno)?;
        if cacheable && kind == Kind::Dir {
            self.dirs.insert(real);
        }
        Ok(kind)
    }

    fn readlink(
        &mut self,
        scope: &Scope,
        grant: usize,
        rel: &str,
    ) -> std::result::Result<String, i32> {
        match locate(scope, self.overlays, grant, rel) {
            Located::Missing => Err(ENOENT),
            Located::Real(real, _) => readlink(&real).map_err(|e| e.errno),
        }
    }
}

enum Opened {
    Node(Node),
    Symlink(String),
}

pub struct Proxy {
    uid: u32,
    handles: BTreeMap<usize, Handle>,
    next: usize,
    sessions: BTreeMap<u32, Session>,
    next_session: u32,
    dirs: DirCache,
    maps: BTreeMap<(usize, u64), (usize, usize)>,
}

fn skeleton(
    scope: &Scope,
    session: u32,
    vpath: &str,
    flags: usize,
    follow: bool,
) -> Result<Opened> {
    if flags & (O_CREAT | O_EXCL) == O_CREAT | O_EXCL {
        return Err(err(syscall::error::EEXIST));
    }
    if accmode_writes(flags) || flags & O_TRUNC != 0 {
        return Err(err(EISDIR));
    }
    if flags & O_SYMLINK != 0 && !follow {
        return Err(err(EINVAL));
    }
    let listing = scope
        .skeleton_children(vpath)
        .into_iter()
        .map(|name| (name, DirentKind::Directory, 0))
        .collect();
    let fd = sys_open(vpath, O_STAT | O_DIRECTORY).ok();
    Ok(Opened::Node(Node {
        session,
        vpath: vpath.to_owned(),
        fd,
        listing: Some(listing),
        grant: None,
        upper: false,
        flags,
    }))
}

fn accmode_writes(flags: usize) -> bool {
    matches!(flags & O_ACCMODE, O_WRONLY | O_RDWR)
}

const COPY_CHUNK: usize = 1 << 16;

fn copy_new(from: &str, to: &str, mode: u32) -> Result<()> {
    let src = sys_open(from, O_RDONLY | O_NOFOLLOW)?;
    let mut st = Stat::default();
    raw_stat(src.as_raw_fd() as usize, &mut st)?;
    if st.st_mode & MODE_TYPE == MODE_DIR || st.st_mode & MODE_TYPE == MODE_SYMLINK {
        return Err(err(EIO));
    }
    let dst = sys_open(to, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | 0o600)?;
    let mut buf = vec![0u8; COPY_CHUNK];
    let max_chunks = (st.st_size as usize).div_ceil(COPY_CHUNK) + 1;
    let mut offset = 0usize;
    let mut done = false;
    for _ in 0..max_chunks {
        let n = unsafe {
            libc::pread(
                src.as_raw_fd(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                offset as libc::off_t,
            )
        };
        if n < 0 {
            return Err(last_error());
        }
        if n == 0 {
            done = true;
            break;
        }
        let n = n as usize;
        let mut written = 0;
        while written < n {
            let w = unsafe {
                libc::pwrite(
                    dst.as_raw_fd(),
                    buf[written..n].as_ptr().cast(),
                    n - written,
                    (offset + written) as libc::off_t,
                )
            };
            if w <= 0 {
                return Err(last_error());
            }
            written += w as usize;
        }
        offset += n;
    }
    if !done {
        return Err(err(EIO));
    }
    check(unsafe { libc::fchmod(dst.as_raw_fd(), mode as libc::mode_t) })
}

fn save_overlay(layer: &str, overlay: &Overlay) -> Result<()> {
    let state = parent(layer);
    let tmp = join(state, ".whiteouts.new");
    fs::write(&tmp, overlay.encode()).map_err(|e| err(e.raw_os_error().unwrap_or(EIO)))?;
    fs::rename(&tmp, join(state, WHITEOUTS)).map_err(|e| err(e.raw_os_error().unwrap_or(EIO)))
}

impl Proxy {
    pub fn new(uid: u32) -> Proxy {
        let mut handles = BTreeMap::new();
        handles.insert(CONTROL, Handle::Control);
        Proxy {
            uid,
            handles,
            next: CONTROL + 1,
            sessions: BTreeMap::new(),
            next_session: 1,
            dirs: DirCache::default(),
            maps: BTreeMap::new(),
        }
    }

    fn insert(&mut self, handle: Handle) -> Result<usize> {
        if self.handles.len() >= MAX_HANDLES {
            return Err(err(EMFILE));
        }
        if let Some(s) = handle.session() {
            self.sessions.get_mut(&s).ok_or(err(EBADF))?.handles += 1;
        }
        let id = self.next;
        self.next += 1;
        self.handles.insert(id, handle);
        Ok(id)
    }

    fn live(&self, session: u32) -> Result<&Session> {
        match self.sessions.get(&session) {
            Some(s) if !s.ended && s.scope.is_some() => Ok(s),
            _ => Err(err(EBADF)),
        }
    }

    fn start_of(&self, dirfd: usize) -> Result<(u32, String)> {
        match self.handles.get(&dirfd).ok_or(err(EBADF))? {
            Handle::Root { session } => Ok((*session, "/".to_owned())),
            Handle::Node(node) => Ok((node.session, node.vpath.clone())),
            _ => Err(err(EBADF)),
        }
    }

    fn walk(&mut self, session: u32, start: &str, path: &str, links: &mut usize) -> Result<Walked> {
        let s = self.sessions.get(&session).ok_or(err(EBADF))?;
        let scope = s.scope.as_ref().ok_or(err(EBADF))?;
        let mut view = RealView {
            dirs: &mut self.dirs,
            overlays: &s.overlays,
        };
        scope
            .resolve(&mut view, start, path, false, links)
            .map_err(err)
    }

    fn scope(&self, session: u32) -> Result<&Scope> {
        self.live(session)?.scope.as_ref().ok_or(err(EBADF))
    }

    fn open_in(
        &mut self,
        session: u32,
        start: &str,
        path: &str,
        flags: usize,
    ) -> Result<OpenResult> {
        self.live(session)?;
        let follow = flags & O_NOFOLLOW == 0
            && !(flags & O_SYMLINK != 0 && (flags & O_ACCMODE == O_RDONLY || flags & O_CREAT != 0));
        let mut links = 0;
        let mut walked = self.walk(session, start, path, &mut links)?;
        loop {
            let vpath = match walked {
                Walked::External(target) => {
                    if flags & O_SYMLINK != 0 && flags & O_CREAT == 0 {
                        let id = self.insert(Handle::External {
                            session,
                            path: target,
                        })?;
                        return Ok(OpenResult::ThisScheme {
                            number: id,
                            flags: NewFdFlags::POSITIONED,
                        });
                    }
                    return Err(err(EXDEV));
                }
                Walked::Path(vpath) => vpath,
            };
            match self.open_place(session, &vpath, flags, follow)? {
                Opened::Node(node) => {
                    let id = self.insert(Handle::Node(node))?;
                    return Ok(OpenResult::ThisScheme {
                        number: id,
                        flags: NewFdFlags::POSITIONED,
                    });
                }
                Opened::Symlink(target) => {
                    links += 1;
                    if links > MAX_LINKS {
                        return Err(err(syscall::error::ELOOP));
                    }
                    walked = match retarget(&target) {
                        Walked::External(e) => Walked::External(e),
                        Walked::Path(t) => self.walk(session, parent(&vpath), &t, &mut links)?,
                    };
                }
            }
        }
    }

    fn open_place(
        &mut self,
        session: u32,
        vpath: &str,
        flags: usize,
        follow: bool,
    ) -> Result<Opened> {
        let scope = self.scope(session)?;
        match scope.classify(vpath) {
            Place::Outside => Err(err(ENOENT)),
            Place::Skeleton => skeleton(scope, session, vpath, flags, follow),
            Place::Inside { grant, rel } => {
                let access = scope.grants[grant].access;
                let wants_write = accmode_writes(flags) || flags & O_TRUNC != 0;
                if access == Access::ReadOnly && wants_write {
                    let real = scope.real_path(grant, &rel);
                    if follow && lkind(&real).is_ok_and(|k| k == Kind::Symlink) {
                        return Ok(Opened::Symlink(readlink(&real)?));
                    }
                    return Err(err(EROFS));
                }
                if rel.is_empty() && flags & (O_CREAT | O_EXCL) == O_CREAT | O_EXCL {
                    return Err(err(syscall::error::EEXIST));
                }
                let ancestor = scope.ancestor_of_grant(vpath);
                let opened = if access == Access::Clone {
                    self.open_clone(session, vpath, grant, &rel, flags, follow)
                } else {
                    let real = scope.real_path(grant, &rel);
                    let read_only = access == Access::ReadOnly;
                    let opened = self.open_real(
                        session, vpath, grant, &real, false, flags, follow, read_only,
                    );
                    if matches!(opened, Ok(Opened::Node(_)))
                        && !read_only
                        && flags & O_SYMLINK != 0
                        && accmode_writes(flags)
                        && let Some(s) = self.sessions.get_mut(&session)
                    {
                        s.record_link(real);
                    }
                    opened
                };
                match opened {
                    Err(e) if e.errno == ENOENT && ancestor => {
                        skeleton(self.scope(session)?, session, vpath, flags, follow)
                    }
                    other => other,
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn open_real(
        &mut self,
        session: u32,
        vpath: &str,
        grant: usize,
        real: &str,
        upper: bool,
        flags: usize,
        follow: bool,
        read_only: bool,
    ) -> Result<Opened> {
        let keeps_link = flags & O_SYMLINK != 0 && flags & O_ACCMODE == O_RDONLY;
        let base =
            (flags & !(O_CREAT | O_EXCL | O_TRUNC)) | if keeps_link { 0 } else { O_NOFOLLOW };
        let node = |fd: OwnedFd| Node {
            session,
            vpath: vpath.to_owned(),
            fd: Some(fd),
            listing: None,
            grant: Some(grant),
            upper,
            flags,
        };
        for _ in 0..MAX_CREATE_TRIES {
            match sys_open(real, base) {
                Ok(fd) => {
                    if flags & (O_CREAT | O_EXCL) == O_CREAT | O_EXCL {
                        return Err(err(syscall::error::EEXIST));
                    }
                    if follow && flags & (O_STAT | O_SYMLINK) != 0 {
                        let mut st = Stat::default();
                        raw_stat(fd.as_raw_fd() as usize, &mut st)?;
                        if st.st_mode & MODE_TYPE == MODE_SYMLINK {
                            return Ok(Opened::Symlink(readlink(real)?));
                        }
                    }
                    if flags & O_TRUNC != 0 && accmode_writes(flags) {
                        check(unsafe { libc::ftruncate(fd.as_raw_fd(), 0) })?;
                    }
                    return Ok(Opened::Node(node(fd)));
                }
                Err(e) if e.errno == syscall::error::ELOOP && follow => {
                    return Ok(Opened::Symlink(readlink(real)?));
                }
                Err(e) if e.errno == ENOENT && flags & O_CREAT != 0 => {
                    if read_only {
                        return Err(err(EROFS));
                    }
                    match sys_open(real, (flags & !O_TRUNC) | O_CREAT | O_EXCL | O_NOFOLLOW) {
                        Ok(fd) => return Ok(Opened::Node(node(fd))),
                        Err(e) if e.errno == syscall::error::EEXIST && flags & O_EXCL == 0 => {
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Err(err(syscall::error::EEXIST))
    }

    fn overlay_parts(
        &mut self,
        session: u32,
        grant: usize,
    ) -> Result<(&mut Overlay, String, String)> {
        let s = self.sessions.get_mut(&session).ok_or(err(EBADF))?;
        let scope = s.scope.as_ref().ok_or(err(EBADF))?;
        let g = &scope.grants[grant];
        let layer = g.layer.clone().ok_or(err(EINVAL))?;
        let lower = g.real.clone();
        let ov = s.overlays[grant].as_mut().ok_or(err(EINVAL))?;
        Ok((ov, lower, layer))
    }

    fn copy_up_dirs(&mut self, session: u32, grant: usize, rel: &str) -> Result<()> {
        let (ov, lower, layer) = self.overlay_parts(session, grant)?;
        let mut chain: Vec<&str> = Vec::new();
        let mut cur = rel;
        while !cur.is_empty() && !ov.upper.contains(cur) {
            chain.push(cur);
            cur = rel_parent(cur);
        }
        for dir in chain.into_iter().rev() {
            let mode = fs::symlink_metadata(join(&lower, dir))
                .map(|m| m.permissions().mode() & 0o7777)
                .unwrap_or(0o755);
            let target = join(&layer, dir);
            match fs::create_dir(&target) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lkind(&target)? != Kind::Dir {
                        return Err(err(EIO));
                    }
                }
                Err(e) => return Err(err(e.raw_os_error().unwrap_or(EIO))),
            }
            let _ = fs::set_permissions(&target, fs::Permissions::from_mode(mode));
            ov.created(dir, true).map_err(err)?;
        }
        Ok(())
    }

    fn copy_up(&mut self, session: u32, grant: usize, rel: &str) -> Result<()> {
        {
            let (ov, _, _) = self.overlay_parts(session, grant)?;
            if ov.upper.contains(rel) {
                return Ok(());
            }
        }
        self.copy_up_dirs(session, grant, rel_parent(rel))?;
        let (ov, lower, layer) = self.overlay_parts(session, grant)?;
        let (from, to) = (join(&lower, rel), join(&layer, rel));
        let io = |e: std::io::Error| err(e.raw_os_error().unwrap_or(EIO));
        let meta = fs::symlink_metadata(&from).map_err(io)?;
        if meta.file_type().is_symlink() {
            symlink(fs::read_link(&from).map_err(io)?, &to).map_err(io)?;
        } else if meta.is_dir() {
            fs::create_dir(&to).map_err(io)?;
            fs::set_permissions(&to, meta.permissions()).map_err(io)?;
        } else {
            copy_new(&from, &to, meta.permissions().mode() & 0o7777)?;
        }
        ov.upper.insert(rel.to_owned());
        save_overlay(&layer, ov)
    }

    fn open_clone(
        &mut self,
        session: u32,
        vpath: &str,
        grant: usize,
        rel: &str,
        flags: usize,
        follow: bool,
    ) -> Result<Opened> {
        let located = {
            let s = self.sessions.get(&session).ok_or(err(EBADF))?;
            locate(s.scope.as_ref().ok_or(err(EBADF))?, &s.overlays, grant, rel)
        };
        let wants_write = accmode_writes(flags) || flags & O_TRUNC != 0;
        match located {
            Located::Real(real, true) => {
                let opened =
                    self.open_real(session, vpath, grant, &real, true, flags, follow, false)?;
                self.listing_for(session, grant, rel, opened)
            }
            Located::Real(real, false) => {
                if flags & (O_CREAT | O_EXCL) == O_CREAT | O_EXCL {
                    return Err(err(syscall::error::EEXIST));
                }
                let kind = lkind(&real)?;
                if wants_write && !(kind == Kind::Symlink && follow) {
                    if kind == Kind::Dir {
                        return Err(err(EISDIR));
                    }
                    self.copy_up(session, grant, rel)?;
                    let (_, _, layer) = self.overlay_parts(session, grant)?;
                    return self.open_real(
                        session,
                        vpath,
                        grant,
                        &join(&layer, rel),
                        true,
                        flags,
                        follow,
                        false,
                    );
                }
                let opened = self.open_real(
                    session,
                    vpath,
                    grant,
                    &real,
                    false,
                    flags & !O_CREAT,
                    follow,
                    true,
                )?;
                self.listing_for(session, grant, rel, opened)
            }
            Located::Missing => {
                if flags & O_CREAT == 0 {
                    return Err(err(ENOENT));
                }
                self.copy_up_dirs(session, grant, rel_parent(rel))?;
                let (_, _, layer) = self.overlay_parts(session, grant)?;
                let opened = self.open_real(
                    session,
                    vpath,
                    grant,
                    &join(&layer, rel),
                    true,
                    flags | O_EXCL,
                    follow,
                    false,
                )?;
                let (ov, _, layer) = self.overlay_parts(session, grant)?;
                ov.created(rel, flags & O_DIRECTORY != 0).map_err(err)?;
                save_overlay(&layer, ov)?;
                Ok(opened)
            }
        }
    }

    fn merged(
        &mut self,
        session: u32,
        grant: usize,
        rel: &str,
    ) -> Result<Vec<(String, DirentKind, u64)>> {
        let s = self.sessions.get(&session).ok_or(err(EBADF))?;
        let scope = s.scope.as_ref().ok_or(err(EBADF))?;
        let g = &scope.grants[grant];
        let ov = s.overlays[grant].as_ref().ok_or(err(EINVAL))?;
        let mut names = BTreeMap::new();
        if ov.lower_listed(rel) {
            let lower = join(&g.real, rel);
            if lkind(&lower).is_ok_and(|k| k == Kind::Dir) {
                list_real(&lower, &mut names)?;
            }
            let prefix = if rel.is_empty() {
                String::new()
            } else {
                format!("{rel}/")
            };
            names.retain(|name, _| !ov.lower_hidden(&format!("{prefix}{name}")));
        }
        if rel.is_empty() || ov.upper.contains(rel) {
            let upper = join(g.layer.as_ref().ok_or(err(EINVAL))?, rel);
            if lkind(&upper).is_ok_and(|k| k == Kind::Dir) {
                list_real(&upper, &mut names)?;
            }
        }
        Ok(names.into_iter().map(|(n, (k, i))| (n, k, i)).collect())
    }

    fn listing_for(
        &mut self,
        session: u32,
        grant: usize,
        rel: &str,
        opened: Opened,
    ) -> Result<Opened> {
        let Opened::Node(mut node) = opened else {
            return Ok(opened);
        };
        let is_dir = match &node.fd {
            Some(fd) => {
                let mut st = Stat::default();
                raw_stat(fd.as_raw_fd() as usize, &mut st)?;
                st.st_mode & MODE_TYPE == MODE_DIR
            }
            None => false,
        };
        if is_dir && node.flags & O_ACCMODE == O_RDONLY {
            node.listing = Some(self.merged(session, grant, rel)?);
        }
        Ok(Opened::Node(node))
    }

    fn node(&self, id: usize) -> Result<&Node> {
        match self.handles.get(&id) {
            Some(Handle::Node(node)) => {
                self.live(node.session)?;
                Ok(node)
            }
            _ => Err(err(EBADF)),
        }
    }

    fn node_fd(&self, id: usize) -> Result<usize> {
        self.node(id)?
            .fd
            .as_ref()
            .map(|fd| fd.as_raw_fd() as usize)
            .ok_or(err(EISDIR))
    }

    fn mutable_fd(&mut self, id: usize) -> Result<usize> {
        let (session, grant, vpath, upper, flags) = {
            let node = self.node(id)?;
            let grant = node.grant.ok_or(err(EROFS))?;
            (
                node.session,
                grant,
                node.vpath.clone(),
                node.upper,
                node.flags,
            )
        };
        let scope = self.scope(session)?;
        let g = &scope.grants[grant];
        match g.access {
            Access::ReadOnly => return Err(err(EROFS)),
            Access::ReadWrite => return self.node_fd(id),
            Access::Clone if upper => return self.node_fd(id),
            Access::Clone => {}
        }
        let (located_session, located_grant, rel, _, located_upper) = self.located_node(id)?;
        if located_session != session || located_grant != grant || located_upper {
            return Err(err(ESTALE));
        }
        debug_assert!(vpath.ends_with(&rel));
        self.copy_up(session, grant, &rel)?;
        let (_, _, layer) = self.overlay_parts(session, grant)?;
        let fd = sys_open(
            &join(&layer, &rel),
            (flags & !(O_CREAT | O_EXCL | O_TRUNC)) | O_NOFOLLOW,
        )?;
        let raw = fd.as_raw_fd() as usize;
        if let Some(Handle::Node(node)) = self.handles.get_mut(&id) {
            node.fd = Some(fd);
            node.upper = true;
        }
        Ok(raw)
    }

    fn parent_rel_of(&self, session: u32, vpath: &str) -> Result<(usize, String)> {
        match self.scope(session)?.classify(vpath) {
            Place::Inside { grant, rel } => Ok((grant, rel)),
            Place::Skeleton => Err(err(EROFS)),
            Place::Outside => Err(err(ENOENT)),
        }
    }

    fn target(&mut self, dirfd: usize, path: &str) -> Result<(u32, String)> {
        let (session, start) = self.start_of(dirfd)?;
        self.live(session)?;
        let mut links = 0;
        match self.walk(session, &start, path, &mut links)? {
            Walked::Path(vpath) => Ok((session, vpath)),
            Walked::External(_) => Err(err(EXDEV)),
        }
    }

    fn writable_place(&self, session: u32, vpath: &str) -> Result<(usize, String, Access)> {
        let (grant, rel) = self.parent_rel_of(session, vpath)?;
        let access = self.scope(session)?.grants[grant].access;
        if access == Access::ReadOnly {
            return Err(err(EROFS));
        }
        if rel.is_empty() {
            return Err(err(EBUSY));
        }
        if access == Access::ReadWrite {
            let scope = self.scope(session)?;
            if scope.protects(&scope.real_path(grant, &rel)) {
                return Err(err(EBUSY));
            }
        }
        Ok((grant, rel, access))
    }

    fn located_node(&mut self, id: usize) -> Result<(u32, usize, String, String, bool)> {
        let (session, vpath, fd) = {
            let node = self.node(id)?;
            (
                node.session,
                node.vpath.clone(),
                node.fd.as_ref().map(|f| f.as_raw_fd() as usize),
            )
        };
        let fd = fd.ok_or(err(EROFS))?;
        let mut links = 0;
        match self.walk(session, "/", &vpath, &mut links)? {
            Walked::Path(walked) if walked == vpath => {}
            _ => return Err(err(ESTALE)),
        }
        let (grant, rel) = self.parent_rel_of(session, &vpath)?;
        let located = {
            let s = self.sessions.get(&session).ok_or(err(EBADF))?;
            locate(
                s.scope.as_ref().ok_or(err(EBADF))?,
                &s.overlays,
                grant,
                &rel,
            )
        };
        let Located::Real(real, upper) = located else {
            return Err(err(ESTALE));
        };
        let mut held = Stat::default();
        raw_stat(fd, &mut held)?;
        let c = cpath(&real)?;
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        check(unsafe { libc::lstat(c.as_ptr(), &mut st) })?;
        if st.st_ino as u64 != held.st_ino
            || (st.st_mode as u16 & MODE_TYPE) != (held.st_mode & MODE_TYPE)
        {
            return Err(err(ESTALE));
        }
        Ok((session, grant, rel, real, upper))
    }

    fn unlink(&mut self, dirfd: usize, path: &str, flags: usize) -> Result<()> {
        let (session, vpath) = self.target(dirfd, path)?;
        let (grant, rel, access) = self.writable_place(session, &vpath)?;
        let rmdir = flags & AT_REMOVEDIR != 0;
        if access == Access::ReadWrite {
            let real = self.scope(session)?.real_path(grant, &rel);
            let c = cpath(&real)?;
            check(unsafe {
                if rmdir {
                    libc::rmdir(c.as_ptr())
                } else {
                    libc::unlink(c.as_ptr())
                }
            })?;
            self.dirs.forget(&real);
            if let Some(s) = self.sessions.get_mut(&session) {
                s.links.remove(&real);
            }
            return Ok(());
        }
        let located = {
            let s = self.sessions.get(&session).ok_or(err(EBADF))?;
            locate(
                s.scope.as_ref().ok_or(err(EBADF))?,
                &s.overlays,
                grant,
                &rel,
            )
        };
        let Located::Real(real, upper) = located else {
            return Err(err(ENOENT));
        };
        let kind = lkind(&real)?;
        if rmdir && kind != Kind::Dir {
            return Err(err(ENOTDIR));
        }
        if !rmdir && kind == Kind::Dir {
            return Err(err(EISDIR));
        }
        if rmdir && !self.merged(session, grant, &rel)?.is_empty() {
            return Err(err(ENOTEMPTY));
        }
        let (ov, lower, layer) = self.overlay_parts(session, grant)?;
        let lower_had = !ov.lower_hidden(&rel) && exists(&join(&lower, &rel));
        if upper {
            let c = cpath(&real)?;
            check(unsafe {
                if rmdir {
                    libc::rmdir(c.as_ptr())
                } else {
                    libc::unlink(c.as_ptr())
                }
            })?;
        }
        ov.removed(&rel, lower_had).map_err(err)?;
        save_overlay(&layer, ov)
    }

    fn rename(
        &mut self,
        old_dir: usize,
        old_path: &str,
        new_dir: usize,
        new_path: &str,
        flags: u32,
    ) -> Result<()> {
        let (session, old) = self.target(old_dir, old_path)?;
        let (new_session, new) = self.target(new_dir, new_path)?;
        if session != new_session {
            return Err(err(EXDEV));
        }
        let (grant, old_rel, access) = self.writable_place(session, &old)?;
        let (new_grant, new_rel, _) = match self.writable_place(session, &new) {
            Err(e) if e.errno == ENOENT || e.errno == EROFS => return Err(err(EXDEV)),
            other => other?,
        };
        if grant != new_grant {
            return Err(err(EXDEV));
        }
        if flags & 1 != 0 && self.exists_in_view(session, grant, &new_rel) {
            return Err(err(syscall::error::EEXIST));
        }
        if access == Access::ReadWrite {
            let scope = self.scope(session)?;
            let (from, to) = (
                scope.real_path(grant, &old_rel),
                scope.real_path(grant, &new_rel),
            );
            let (a, b) = (cpath(&from)?, cpath(&to)?);
            check(unsafe { libc::rename(a.as_ptr(), b.as_ptr()) })?;
            self.dirs.forget(&from);
            self.dirs.forget(&to);
            let moved_link = lkind(&to).is_ok_and(|k| k == Kind::Symlink);
            if let Some(s) = self.sessions.get_mut(&session) {
                s.links.remove(&to);
                s.moved_links(&from, &to);
                if moved_link {
                    s.record_link(to.clone());
                }
            }
            return Ok(());
        }
        let located = {
            let s = self.sessions.get(&session).ok_or(err(EBADF))?;
            locate(
                s.scope.as_ref().ok_or(err(EBADF))?,
                &s.overlays,
                grant,
                &old_rel,
            )
        };
        let Located::Real(real, _) = located else {
            return Err(err(ENOENT));
        };
        let is_dir = lkind(&real)? == Kind::Dir;
        let (lower_had_old, lower_has_new, new_lower_dir) = {
            let (ov, lower, _) = self.overlay_parts(session, grant)?;
            let new_lower = join(&lower, &new_rel);
            (
                !ov.lower_hidden(&old_rel) && exists(&join(&lower, &old_rel)),
                !ov.lower_hidden(&new_rel) && exists(&new_lower),
                !ov.lower_hidden(&new_rel) && lkind(&new_lower).is_ok_and(|k| k == Kind::Dir),
            )
        };
        if is_dir && lower_had_old {
            return Err(err(EXDEV));
        }
        if new_lower_dir && !self.merged(session, grant, &new_rel)?.is_empty() {
            return Err(err(ENOTEMPTY));
        }
        self.copy_up(session, grant, &old_rel)?;
        self.copy_up_dirs(session, grant, rel_parent(&new_rel))?;
        let (ov, _, layer) = self.overlay_parts(session, grant)?;
        let (from, to) = (join(&layer, &old_rel), join(&layer, &new_rel));
        if is_dir && lower_has_new && !new_lower_dir {
            return Err(err(ENOTDIR));
        }
        let (a, b) = (cpath(&from)?, cpath(&to)?);
        check(unsafe { libc::rename(a.as_ptr(), b.as_ptr()) })?;
        ov.moved(&old_rel, &new_rel);
        ov.removed(&old_rel, lower_had_old).map_err(err)?;
        ov.created(&new_rel, is_dir).map_err(err)?;
        if new_lower_dir {
            ov.opaque.insert(new_rel.clone());
        }
        save_overlay(&layer, ov)
    }

    fn exists_in_view(&mut self, session: u32, grant: usize, rel: &str) -> bool {
        let Ok(s) = self.live(session) else {
            return false;
        };
        let Some(scope) = s.scope.as_ref() else {
            return false;
        };
        match locate(scope, &s.overlays, grant, rel) {
            Located::Real(real, _) => exists(&real),
            Located::Missing => false,
        }
    }

    fn link(&mut self, id: usize, dirfd: usize, path: &str) -> Result<()> {
        let vpath = self.node(id)?.vpath.clone();
        let (session, located_grant, located_rel, _, _) = self.located_node(id)?;
        let (new_session, new) = self.target(dirfd, path)?;
        if session != new_session {
            return Err(err(EXDEV));
        }
        let (grant, old_rel, access) = self.writable_place(session, &vpath)?;
        if grant != located_grant || old_rel != located_rel {
            return Err(err(ESTALE));
        }
        let (new_grant, new_rel, _) = match self.writable_place(session, &new) {
            Err(e) if e.errno == ENOENT || e.errno == EROFS => return Err(err(EXDEV)),
            other => other?,
        };
        if grant != new_grant {
            return Err(err(EXDEV));
        }
        if self.exists_in_view(session, grant, &new_rel) {
            return Err(err(syscall::error::EEXIST));
        }
        let (from, to) = if access == Access::ReadWrite {
            let scope = self.scope(session)?;
            (
                scope.real_path(grant, &old_rel),
                scope.real_path(grant, &new_rel),
            )
        } else {
            self.copy_up(session, grant, &old_rel)?;
            self.copy_up_dirs(session, grant, rel_parent(&new_rel))?;
            let (_, _, layer) = self.overlay_parts(session, grant)?;
            (join(&layer, &old_rel), join(&layer, &new_rel))
        };
        let (a, b) = (cpath(&from)?, cpath(&to)?);
        check(unsafe { libc::link(a.as_ptr(), b.as_ptr()) })?;
        if access == Access::ReadWrite
            && lkind(&to)? == Kind::Symlink
            && let Some(s) = self.sessions.get_mut(&session)
        {
            s.record_link(to.clone());
        }
        if access == Access::Clone {
            let (ov, _, layer) = self.overlay_parts(session, grant)?;
            ov.created(&new_rel, false).map_err(err)?;
            save_overlay(&layer, ov)?;
        }
        Ok(())
    }

    fn configure(&mut self, ctl: usize) -> Result<OpenResult> {
        let (session, spec) = match self.handles.get_mut(&ctl) {
            Some(Handle::Ctl { session, spec, .. }) => (*session, std::mem::take(spec)),
            _ => return Err(err(EBADF)),
        };
        let s = self.sessions.get(&session).ok_or(err(EBADF))?;
        if s.scope.is_some() || s.ended {
            return Err(err(EBUSY));
        }
        let scope = Scope::parse(&spec).map_err(|reason| {
            eprintln!("ferrion-scope-proxy: {reason}");
            err(EINVAL)
        })?;
        let mut overlays = Vec::with_capacity(scope.grants.len());
        for g in &scope.grants {
            if !exists(&g.real) {
                eprintln!("ferrion-scope-proxy: {} does not exist", g.real);
                return Err(err(ENOENT));
            }
            overlays.push(match &g.layer {
                Some(layer) => {
                    if lkind(&g.real)? != Kind::Dir || lkind(layer)? != Kind::Dir {
                        return Err(err(ENOTDIR));
                    }
                    let ov = Overlay::default();
                    save_overlay(layer, &ov)?;
                    Some(ov)
                }
                None => None,
            });
        }
        let s = self.sessions.get_mut(&session).ok_or(err(EBADF))?;
        s.scope = Some(scope);
        s.overlays = overlays;
        let id = self.insert(Handle::Root { session })?;
        Ok(OpenResult::ThisScheme {
            number: id,
            flags: NewFdFlags::empty(),
        })
    }

    fn end_session(&mut self, session: u32) {
        let Some(s) = self.sessions.get_mut(&session) else {
            return;
        };
        s.ended = true;
        let ids: Vec<usize> = self
            .handles
            .iter()
            .filter(|(_, h)| h.session() == Some(session))
            .map(|(id, _)| *id)
            .collect();
        for other in ids {
            if let Some(Handle::Node(node)) = self.handles.get_mut(&other) {
                node.fd = None;
                node.listing = None;
            }
        }
    }

    pub fn close(&mut self, id: usize) {
        if id == CONTROL {
            return;
        }
        let Some(handle) = self.handles.remove(&id) else {
            return;
        };
        if let Handle::Node(_) = handle {
            let keys: Vec<(usize, u64)> = self
                .maps
                .range((id, 0)..=(id, u64::MAX))
                .map(|(k, _)| *k)
                .collect();
            for k in keys {
                if let Some((addr, size)) = self.maps.remove(&k) {
                    unsafe { libc::munmap(addr as *mut libc::c_void, size) };
                }
            }
        }
        if let Handle::Ctl { session, .. } = &handle {
            self.end_session(*session);
        }
        if let Some(session) = handle.session()
            && let Some(s) = self.sessions.get_mut(&session)
        {
            s.handles -= 1;
            if s.handles == 0 {
                self.sessions.remove(&session);
            }
        }
    }

    pub fn inode_of(&self, id: usize) -> Option<usize> {
        let fd = self.node_fd(id).ok()?;
        let mut st = Stat::default();
        raw_stat(fd, &mut st).ok()?;
        Some(st.st_ino as usize)
    }
}

fn map_prot(flags: MapFlags) -> (i32, i32) {
    let mut prot = 0;
    if flags.contains(MapFlags::PROT_READ) {
        prot |= libc::PROT_READ;
    }
    if flags.contains(MapFlags::PROT_WRITE) {
        prot |= libc::PROT_WRITE;
    }
    if flags.contains(MapFlags::PROT_EXEC) {
        prot |= libc::PROT_EXEC;
    }
    let kind = if flags.contains(MapFlags::MAP_SHARED) {
        libc::MAP_SHARED
    } else {
        libc::MAP_PRIVATE
    };
    (prot, kind)
}

impl SchemeSync for Proxy {
    fn scheme_root(&mut self) -> Result<usize> {
        Err(err(EOPNOTSUPP))
    }

    fn openat(
        &mut self,
        dirfd: usize,
        path: &str,
        flags: usize,
        fcntl_flags: u32,
        ctx: &CallerCtx,
    ) -> Result<OpenResult> {
        let _ = fcntl_flags;
        match self.handles.get(&dirfd).ok_or(err(EBADF))? {
            Handle::Control => {
                if ctx.uid != self.uid || path != "session" {
                    return Err(err(syscall::error::EACCES));
                }
                if self.sessions.len() >= MAX_SESSIONS {
                    return Err(err(EMFILE));
                }
                let session = self.next_session;
                self.next_session += 1;
                self.sessions.insert(
                    session,
                    Session {
                        scope: None,
                        overlays: Vec::new(),
                        handles: 0,
                        ended: false,
                        links: BTreeSet::new(),
                        links_dropped: 0,
                    },
                );
                let id = self.insert(Handle::Ctl {
                    session,
                    spec: Vec::new(),
                    read: 0,
                })?;
                Ok(OpenResult::ThisScheme {
                    number: id,
                    flags: NewFdFlags::empty(),
                })
            }
            Handle::Ctl { .. } if path == "root" => self.configure(dirfd),
            Handle::Ctl { .. } | Handle::External { .. } => Err(err(EINVAL)),
            Handle::Node(node)
                if node.listing.is_none()
                    && node.fd.is_some()
                    && !path.is_empty()
                    && !path.starts_with('/') =>
            {
                let mut st = Stat::default();
                raw_stat(
                    node.fd
                        .as_ref()
                        .map(|f| f.as_raw_fd() as usize)
                        .unwrap_or(0),
                    &mut st,
                )?;
                if st.st_mode & MODE_TYPE != MODE_DIR {
                    return Err(err(ENOTDIR));
                }
                let (session, start) = self.start_of(dirfd)?;
                self.open_in(session, &start, path, flags)
            }
            Handle::Root { .. } | Handle::Node(_) => {
                let (session, start) = self.start_of(dirfd)?;
                self.open_in(session, &start, path, flags)
            }
        }
    }

    fn unlinkat(&mut self, dirfd: usize, path: &str, flags: usize, _ctx: &CallerCtx) -> Result<()> {
        self.unlink(dirfd, path, flags)
    }

    fn dup(&mut self, _old_id: usize, _buf: &[u8], _ctx: &CallerCtx) -> Result<OpenResult> {
        Err(err(EOPNOTSUPP))
    }

    fn read(
        &mut self,
        id: usize,
        buf: &mut [u8],
        offset: u64,
        _fcntl_flags: u32,
        _ctx: &CallerCtx,
    ) -> Result<usize> {
        if let Some(Handle::Ctl { session, read, .. }) = self.handles.get_mut(&id) {
            let report = self.sessions.get(session).ok_or(err(EBADF))?.links_report();
            let start = (*read).min(report.len());
            let n = (report.len() - start).min(buf.len());
            buf[..n].copy_from_slice(&report[start..start + n]);
            *read = start + n;
            return Ok(n);
        }
        let fd = self.node_fd(id)?;
        let n = unsafe {
            libc::pread(
                fd as i32,
                buf.as_mut_ptr().cast(),
                buf.len(),
                offset as libc::off_t,
            )
        };
        if n < 0 {
            Err(last_error())
        } else {
            Ok(n as usize)
        }
    }

    fn write(
        &mut self,
        id: usize,
        buf: &[u8],
        offset: u64,
        _fcntl_flags: u32,
        _ctx: &CallerCtx,
    ) -> Result<usize> {
        let ctl = match self.handles.get(&id) {
            Some(Handle::Ctl { session, .. }) => Some(*session),
            _ => None,
        };
        if let Some(session) = ctl
            && self
                .sessions
                .get(&session)
                .is_some_and(|s| s.scope.is_some())
        {
            if buf != END_SESSION {
                return Err(err(EBUSY));
            }
            self.end_session(session);
            return Ok(buf.len());
        }
        match self.handles.get_mut(&id) {
            Some(Handle::Ctl { spec, .. }) => {
                if spec.len() + buf.len() > crate::scope::MAX_SPEC {
                    return Err(err(EINVAL));
                }
                spec.extend_from_slice(buf);
                return Ok(buf.len());
            }
            Some(Handle::Node(_)) => {}
            _ => return Err(err(EBADF)),
        }
        let fd = self.node_fd(id)?;
        let n = unsafe {
            libc::pwrite(
                fd as i32,
                buf.as_ptr().cast(),
                buf.len(),
                offset as libc::off_t,
            )
        };
        if n < 0 {
            Err(last_error())
        } else {
            Ok(n as usize)
        }
    }

    fn fsize(&mut self, id: usize, ctx: &CallerCtx) -> Result<u64> {
        let mut st = Stat::default();
        self.fstat(id, &mut st, ctx)?;
        Ok(st.st_size)
    }

    fn fchmod(&mut self, id: usize, mode: u16, _ctx: &CallerCtx) -> Result<()> {
        let fd = self.mutable_fd(id)?;
        check(unsafe { libc::fchmod(fd as i32, mode as libc::mode_t) })
    }

    fn fchown(&mut self, id: usize, uid: u32, gid: u32, _ctx: &CallerCtx) -> Result<()> {
        let fd = self.mutable_fd(id)?;
        check(unsafe { libc::fchown(fd as i32, uid, gid) })
    }

    fn fcntl(&mut self, id: usize, cmd: usize, arg: usize, _ctx: &CallerCtx) -> Result<usize> {
        match self.node(id)?.fd.as_ref() {
            Some(fd) => syscall::fcntl(fd.as_raw_fd() as usize, cmd, arg),
            None => Ok(0),
        }
    }

    fn fevent(&mut self, _id: usize, _flags: EventFlags, _ctx: &CallerCtx) -> Result<EventFlags> {
        Err(err(EPERM))
    }

    fn fpath(&mut self, id: usize, buf: &mut [u8], _ctx: &CallerCtx) -> Result<usize> {
        let path = match self.handles.get(&id).ok_or(err(EBADF))? {
            Handle::Root { .. } => "/scheme/file".to_owned(),
            Handle::Node(node) if node.vpath == "/" => "/scheme/file".to_owned(),
            Handle::Node(node) => format!("/scheme/file{}", node.vpath),
            Handle::External { path, .. } => path.clone(),
            _ => return Err(err(EBADF)),
        };
        let n = path.len().min(buf.len());
        buf[..n].copy_from_slice(&path.as_bytes()[..n]);
        Ok(n)
    }

    fn flinkat(
        &mut self,
        id: usize,
        dirid: usize,
        path: &str,
        _flags: u32,
        _ctx: &CallerCtx,
    ) -> Result<usize> {
        self.link(id, dirid, path).map(|()| 0)
    }

    fn frenameat(
        &mut self,
        old_dir: usize,
        old_path: &str,
        new_dir: usize,
        new_path: &str,
        flags: u32,
        _ctx: &CallerCtx,
    ) -> Result<usize> {
        self.rename(old_dir, old_path, new_dir, new_path, flags)
            .map(|()| 0)
    }

    fn fstat(&mut self, id: usize, stat: &mut Stat, _ctx: &CallerCtx) -> Result<()> {
        match self.handles.get(&id).ok_or(err(EBADF))? {
            Handle::Root { .. } => {
                *stat = Stat {
                    st_mode: MODE_DIR | 0o755,
                    ..Stat::default()
                };
                Ok(())
            }
            Handle::External { .. } => {
                *stat = Stat {
                    st_mode: MODE_SYMLINK | 0o777,
                    ..Stat::default()
                };
                Ok(())
            }
            Handle::Node(node) => {
                self.live(node.session)?;
                match &node.fd {
                    Some(fd) => raw_stat(fd.as_raw_fd() as usize, stat),
                    None => {
                        *stat = Stat {
                            st_mode: MODE_DIR | 0o755,
                            ..Stat::default()
                        };
                        Ok(())
                    }
                }
            }
            _ => Err(err(EBADF)),
        }
    }

    fn fstatvfs(&mut self, id: usize, stat: &mut StatVfs, _ctx: &CallerCtx) -> Result<()> {
        let fd = self.node_fd(id)?;
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(stat as *mut StatVfs as *mut u8, size_of::<StatVfs>())
        };
        syscall::std_fs_call(
            fd,
            bytes,
            &StdFsCallMeta::new(StdFsCallKind::Fstatvfs, 0, 0),
        )?;
        Ok(())
    }

    fn fsync(&mut self, id: usize, _ctx: &CallerCtx) -> Result<()> {
        match self.node_fd(id) {
            Ok(fd) => check(unsafe { libc::fsync(fd as i32) }),
            Err(e) if e.errno == EISDIR => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn ftruncate(&mut self, id: usize, len: u64, _ctx: &CallerCtx) -> Result<()> {
        let fd = self.mutable_fd(id)?;
        check(unsafe { libc::ftruncate(fd as i32, len as libc::off_t) })
    }

    fn futimens(&mut self, id: usize, times: &[TimeSpec], _ctx: &CallerCtx) -> Result<()> {
        let fd = self.mutable_fd(id)?;
        let mut ts = [libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        }; 2];
        for (t, s) in ts.iter_mut().zip(times) {
            t.tv_sec = s.tv_sec as _;
            t.tv_nsec = s.tv_nsec as _;
        }
        check(unsafe { libc::futimens(fd as i32, ts.as_ptr()) })
    }

    fn getdents<'buf>(
        &mut self,
        id: usize,
        mut buf: DirentBuf<&'buf mut [u8]>,
        opaque: u64,
    ) -> Result<DirentBuf<&'buf mut [u8]>> {
        let node = self.node(id)?;
        if let Some(listing) = &node.listing {
            for (i, (name, kind, inode)) in listing.iter().enumerate().skip(opaque as usize) {
                let entry = DirEntry {
                    inode: *inode,
                    next_opaque_id: i as u64 + 1,
                    name,
                    kind: *kind,
                };
                if let Err(e) = buf.entry(entry) {
                    if e.errno == EINVAL && i as u64 > opaque {
                        break;
                    }
                    return Err(e);
                }
            }
            return Ok(buf);
        }
        let fd = node.fd.as_ref().ok_or(err(EBADF))?.as_raw_fd() as usize;
        let session = node.session;
        let vpath = node.vpath.clone();
        let mut raw = vec![0u8; 4096];
        let header = size_of::<DirentHeader>() as u64;
        let scope = self.scope(session)?;
        let mut cursor = opaque;
        let mut first = true;
        for _ in 0..MAX_LISTING {
            let n = syscall::std_fs_call(
                fd,
                &mut raw,
                &StdFsCallMeta::new(StdFsCallKind::Getdents, cursor, header),
            )?;
            if n == 0 {
                break;
            }
            for item in DirentIter::new(&raw[..n]) {
                let Ok((h, name)) = item else {
                    return Err(err(EIO));
                };
                let next = h.next_opaque_id;
                let Ok(name) = std::str::from_utf8(name) else {
                    cursor = next;
                    continue;
                };
                let visible = name == "."
                    || name == ".."
                    || scope.classify(&join(&vpath, name)) != Place::Outside;
                if visible {
                    let entry = DirEntry {
                        inode: h.inode,
                        next_opaque_id: next,
                        name,
                        kind: DirentKind::try_from_raw(h.kind).unwrap_or(DirentKind::Unspecified),
                    };
                    if let Err(e) = buf.entry(entry) {
                        if e.errno == EINVAL && !first {
                            return Ok(buf);
                        }
                        return Err(e);
                    }
                    first = false;
                }
                cursor = next;
            }
        }
        Ok(buf)
    }

    fn mmap_prep(
        &mut self,
        id: usize,
        offset: u64,
        size: usize,
        flags: MapFlags,
        _ctx: &CallerCtx,
    ) -> Result<usize> {
        if self.maps.len() >= MAX_MAPS {
            return Err(err(EMFILE));
        }
        let fd = self.node_fd(id)?;
        let (prot, kind) = map_prot(flags);
        let addr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                prot,
                kind,
                fd as i32,
                offset as libc::off_t,
            )
        };
        if addr == libc::MAP_FAILED {
            return Err(last_error());
        }
        if let Some((old, old_size)) = self.maps.insert((id, offset), (addr as usize, size)) {
            unsafe { libc::munmap(old as *mut libc::c_void, old_size) };
        }
        Ok(addr as usize)
    }

    fn munmap(
        &mut self,
        id: usize,
        offset: u64,
        _size: usize,
        _flags: MunmapFlags,
        _ctx: &CallerCtx,
    ) -> Result<()> {
        if let Some((addr, size)) = self.maps.remove(&(id, offset)) {
            unsafe { libc::munmap(addr as *mut libc::c_void, size) };
        }
        Ok(())
    }

    fn inode(&self, id: usize) -> Result<usize> {
        self.inode_of(id).ok_or(err(EBADF))
    }
}

pub const PROXY_SCHEMES: &[&str] = &["file", "memory", "pipe", "rand", "null", "zero"];

pub fn start(uid: u32, gid: u32, ready: usize) -> Result<Socket> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(err(EPERM));
    }
    let socket = Socket::create()?;
    let control = socket.create_this_scheme_fd(0, CONTROL, 0, 0)?;
    let sent = libredox::call::call_wo(ready, &control.to_ne_bytes(), CallFlags::FD, &[]);
    let _ = libredox::call::close(ready);
    sent?;
    let names: Vec<ioslice::IoSlice> = PROXY_SCHEMES
        .iter()
        .map(|s| ioslice::IoSlice::new(s.as_bytes()))
        .collect();
    let namespace = libredox::call::mkns(&names)?;
    let old = libredox::call::setns(namespace)?;
    let _ = libredox::call::close(old);
    if unsafe { libc::setgid(gid) } != 0 || unsafe { libc::setuid(uid) } != 0 {
        return Err(last_error());
    }
    let ids = unsafe {
        (
            libc::getuid(),
            libc::geteuid(),
            libc::getgid(),
            libc::getegid(),
        )
    };
    if ids != (uid, uid, gid, gid) {
        return Err(err(EPERM));
    }
    Ok(socket)
}

pub fn serve(socket: Socket, uid: u32) -> Result<()> {
    let mut proxy = Proxy::new(uid);
    let mut state = SchemeState::new();
    loop {
        let Some(request) = socket.next_request(SignalBehavior::Restart)? else {
            return Ok(());
        };
        match request.kind() {
            RequestKind::Call(call) => {
                let response = call.handle_sync(&mut proxy, &mut state);
                socket.write_response(response, SignalBehavior::Restart)?;
            }
            RequestKind::OnClose { id } => {
                if let Some(inode) = proxy.inode_of(id) {
                    state.on_detach(id, inode, usize::MAX);
                }
                proxy.close(id);
            }
            RequestKind::OnDetach { id, pid } => {
                if let Some(inode) = proxy.inode_of(id) {
                    state.on_detach(id, inode, pid);
                }
            }
            RequestKind::SendFd(request) => {
                socket
                    .write_response(Response::err(EOPNOTSUPP, request), SignalBehavior::Restart)?;
            }
            _ => {}
        }
    }
}

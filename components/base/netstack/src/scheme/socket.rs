use std::cell::RefCell;
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::str;

use libredox::flag::CLOCK_MONOTONIC;
use libredox::protocol::SocketCall;
use redox_scheme::scheme::{register_scheme_inner, Op, SchemeSync};
use redox_scheme::{CallerCtx, OpenResult, Socket};
use scheme_utils::HandleMap;
use smoltcp::socket::AnySocket;
use syscall::data::TimeSpec;
use syscall::flag::{EVENT_READ, EVENT_WRITE};
use syscall::schemev2::NewFdFlags;
use syscall::{
    Error as SyscallError, EventFlags as SyscallEventFlags, Result as SyscallResult, EINVAL,
    EOPNOTSUPP,
};

use super::{Interface, SocketSet};
use crate::router::route_table::RouteTable;
use crate::scheme::smoltcp::iface::SocketHandle;
use crate::scheme::Router;
use crate::Smolnetd;

const EVENT_ERR_BIT: usize = 8;
const SOL_SOCKET: usize = 1;
const IPPROTO_TCP: usize = 6;
const SO_REUSEADDR: usize = 2;
const SO_ERROR: usize = 4;
const SO_SNDBUF: usize = 7;
const SO_RCVBUF: usize = 8;
const SO_KEEPALIVE: usize = 9;
const SO_RCVTIMEO: usize = 20;
const SO_SNDTIMEO: usize = 21;
const TCP_NODELAY: usize = 1;
const TCP_KEEPIDLE: usize = 4;
const SOCKET_CALL_LISTEN: usize = 10;
const DEFAULT_TCP_KEEPIDLE_SECS: u32 = 7200;
const MIN_TCP_KEEPIDLE_SECS: i32 = 1;
const MAX_TCP_KEEPIDLE_SECS: i32 = 86_400;
const TIMEVAL_BYTES: usize = 16;

fn parse_i32_opt(payload: &[u8]) -> SyscallResult<i32> {
    if payload.len() < size_of::<i32>() {
        return Err(SyscallError::new(syscall::EINVAL));
    }
    Ok(i32::from_ne_bytes([
        payload[0], payload[1], payload[2], payload[3],
    ]))
}

fn write_i32_opt(payload: &mut [u8], val: i32) -> SyscallResult<usize> {
    let bytes = val.to_ne_bytes();
    if payload.len() < bytes.len() {
        return Err(SyscallError::new(syscall::EINVAL));
    }
    payload[..bytes.len()].copy_from_slice(&bytes);
    Ok(bytes.len())
}

fn parse_timeval_opt(payload: &[u8]) -> SyscallResult<Option<TimeSpec>> {
    if payload.len() < TIMEVAL_BYTES {
        return Err(SyscallError::new(syscall::EINVAL));
    }
    let tv_sec = i64::from_ne_bytes([
        payload[0], payload[1], payload[2], payload[3], payload[4], payload[5], payload[6],
        payload[7],
    ]);
    let tv_usec = i32::from_ne_bytes([payload[8], payload[9], payload[10], payload[11]]);
    if tv_sec < 0 || !(0..1_000_000).contains(&tv_usec) {
        return Err(SyscallError::new(syscall::EDOM));
    }
    if tv_sec == 0 && tv_usec == 0 {
        Ok(None)
    } else {
        Ok(Some(TimeSpec {
            tv_sec,
            tv_nsec: tv_usec * 1000,
        }))
    }
}

fn write_timeval_opt(payload: &mut [u8], timeout: Option<TimeSpec>) -> SyscallResult<usize> {
    if payload.len() < TIMEVAL_BYTES {
        return Err(SyscallError::new(syscall::EINVAL));
    }
    let (tv_sec, tv_usec) = match timeout {
        Some(ts) => (ts.tv_sec, ts.tv_nsec / 1000),
        None => (0i64, 0i32),
    };
    payload[0..8].copy_from_slice(&tv_sec.to_ne_bytes());
    payload[8..12].copy_from_slice(&tv_usec.to_ne_bytes());
    payload[12..16].fill(0);
    Ok(TIMEVAL_BYTES)
}

pub struct Context {
    pub iface: Interface,
    pub route_table: Rc<RefCell<RouteTable>>,
    pub timer: ::std::time::Instant,
}

impl Context {
    pub fn now_us(&self) -> i64 {
        i64::try_from(self.timer.elapsed().as_micros()).unwrap_or(i64::MAX)
    }
}

#[derive(Clone, Copy)]
pub struct SocketOpts {
    pub rcvbuf: Option<usize>,
    pub sndbuf: Option<usize>,
    pub reuseaddr: bool,
    pub keepalive: bool,
    pub nodelay: bool,
    pub keepidle_secs: u32,
    pub read_timeout: Option<TimeSpec>,
    pub write_timeout: Option<TimeSpec>,
}

impl Default for SocketOpts {
    fn default() -> Self {
        Self {
            rcvbuf: None,
            sndbuf: None,
            reuseaddr: false,
            keepalive: false,
            nodelay: false,
            keepidle_secs: DEFAULT_TCP_KEEPIDLE_SECS,
            read_timeout: None,
            write_timeout: None,
        }
    }
}

pub struct NullFile {
    pub flags: usize,
    pub uid: u32,
    pub gid: u32,
    pub read_enabled: bool,
    pub write_enabled: bool,
    pub opts: SocketOpts,
}

pub struct SocketFile<DataT> {
    pub flags: usize,
    pub data: DataT,

    events: usize,
    socket_handle: SocketHandle,
    read_notified: bool,
    write_notified: bool,
    err_notified: bool,
    read_timeout: Option<TimeSpec>,
    write_timeout: Option<TimeSpec>,
    pub read_enabled: bool,
    pub write_enabled: bool,
    pub reuseaddr: bool,
    pub keepidle_secs: u32,
    pub pending_error: Option<i32>,
    pub reset_pending: bool,
}

impl<DataT> SocketFile<DataT> {
    pub fn clone_with_data(&self, data: DataT) -> SocketFile<DataT> {
        SocketFile {
            flags: self.flags,
            events: self.events,
            read_notified: false,
            write_notified: false,
            err_notified: false,
            read_timeout: self.read_timeout,
            write_timeout: self.write_timeout,
            socket_handle: self.socket_handle,
            read_enabled: self.read_enabled,
            write_enabled: self.write_enabled,
            reuseaddr: self.reuseaddr,
            keepidle_secs: self.keepidle_secs,
            pending_error: None,
            reset_pending: false,
            data,
        }
    }

    pub fn clone_with_handle(&self, socket_handle: SocketHandle, data: DataT) -> SocketFile<DataT> {
        SocketFile {
            flags: self.flags,
            events: self.events,
            read_notified: false,
            write_notified: false,
            err_notified: false,
            read_timeout: self.read_timeout,
            write_timeout: self.write_timeout,
            socket_handle,
            read_enabled: self.read_enabled,
            write_enabled: self.write_enabled,
            reuseaddr: self.reuseaddr,
            keepidle_secs: self.keepidle_secs,
            pending_error: None,
            reset_pending: false,
            data,
        }
    }

    pub fn new_with_data(socket_handle: SocketHandle, data: DataT) -> SocketFile<DataT> {
        SocketFile {
            flags: 0,
            events: 0,
            read_notified: false,
            write_notified: false,
            err_notified: false,
            read_timeout: None,
            write_timeout: None,
            read_enabled: true,
            write_enabled: true,
            reuseaddr: false,
            keepidle_secs: DEFAULT_TCP_KEEPIDLE_SECS,
            pending_error: None,
            reset_pending: false,
            socket_handle,
            data,
        }
    }
}

#[derive(Copy, Clone)]
enum Setting<SettingT: Copy> {
    HopLimit,
    ReadTimeout,
    WriteTimeout,
    #[allow(dead_code)]
    Other(SettingT),
}

pub struct SettingFile<SettingT: Copy> {
    fd: usize,
    socket_handle: SocketHandle,
    setting: Setting<SettingT>,
}

pub enum SchemeFile<SocketT>
where
    SocketT: SchemeSocket,
{
    Setting(SettingFile<SocketT::SettingT>),
    Socket(SocketFile<SocketT::DataT>),
}

impl<SocketT> SchemeFile<SocketT>
where
    SocketT: SchemeSocket,
{
    pub fn socket_handle(&self) -> SocketHandle {
        match *self {
            SchemeFile::Socket(SocketFile { socket_handle, .. })
            | SchemeFile::Setting(SettingFile { socket_handle, .. }) => socket_handle,
        }
    }

    pub fn events(&mut self, socket_set: &mut SocketSet, now_us: i64) -> usize
    where
        SocketT: AnySocket<'static>,
    {
        let mut revents = 0;
        if let SchemeFile::Socket(ref mut sock_file) = *self {
            let socket_handle = sock_file.socket_handle;
            SocketT::sync_state(socket_set, socket_handle, sock_file, now_us);

            let has_err = sock_file.pending_error.is_some() || sock_file.reset_pending;
            if has_err {
                if !sock_file.err_notified {
                    sock_file.err_notified = true;
                    revents |= EVENT_ERR_BIT;
                    if sock_file.events & syscall::EVENT_WRITE.bits() != 0 {
                        sock_file.write_notified = true;
                        revents |= EVENT_WRITE.bits();
                    }
                    if sock_file.events & syscall::EVENT_READ.bits() != 0 {
                        sock_file.read_notified = true;
                        revents |= EVENT_READ.bits();
                    }
                }
                return revents;
            } else {
                sock_file.err_notified = false;
            }

            let can_recv = SocketT::is_readable(socket_set, socket_handle, &sock_file.data);
            let can_send = {
                let socket = socket_set.get::<SocketT>(socket_handle);
                socket.can_send()
            };

            if sock_file.events & syscall::EVENT_READ.bits() == syscall::EVENT_READ.bits()
                && can_recv
            {
                if !sock_file.read_notified {
                    sock_file.read_notified = true;
                    revents |= EVENT_READ.bits();
                }
            } else {
                sock_file.read_notified = false;
            }

            if sock_file.events & syscall::EVENT_WRITE.bits() == syscall::EVENT_WRITE.bits()
                && can_send
            {
                if !sock_file.write_notified {
                    sock_file.write_notified = true;
                    revents |= EVENT_WRITE.bits();
                }
            } else {
                sock_file.write_notified = false;
            }
        }
        revents
    }
}

pub type DupResult<T> = Option<(
    SchemeFile<T>,
    Option<(SocketHandle, <T as SchemeSocket>::DataT)>,
)>;

pub trait SchemeSocket
where
    Self: ::std::marker::Sized,
{
    type SchemeDataT;
    type DataT;
    type SettingT: Copy;

    const BASE_PATH: &'static str;

    fn new_scheme_data() -> Self::SchemeDataT;

    fn can_send(&self) -> bool;
    fn can_recv(&mut self, data: &Self::DataT) -> bool;
    fn may_recv(&self) -> bool;

    fn hop_limit(&self) -> u8;
    fn set_hop_limit(&mut self, hop_limit: u8);

    fn get_setting(
        file: &SocketFile<Self::DataT>,
        setting: Self::SettingT,
        buf: &mut [u8],
    ) -> SyscallResult<usize>;
    fn set_setting(
        file: &mut SocketFile<Self::DataT>,
        setting: Self::SettingT,
        buf: &[u8],
    ) -> SyscallResult<usize>;

    fn default_buf_bytes() -> usize {
        Router::MTU * Smolnetd::SOCKET_BUFFER_SIZE
    }

    fn default_rx_buf_bytes() -> usize {
        Self::default_buf_bytes()
    }

    fn default_tx_buf_bytes() -> usize {
        Self::default_buf_bytes()
    }

    fn clamp_buf_bytes(requested: usize) -> usize {
        requested.clamp(Router::MTU, Router::MTU * Smolnetd::SOCKET_BUFFER_SIZE)
    }

    fn new_socket(
        sockets: &mut SocketSet,
        path: &str,
        uid: u32,
        data: &mut Self::SchemeDataT,
        context: &Context,
        opts: &SocketOpts,
    ) -> SyscallResult<(SocketHandle, Self::DataT)>;

    fn close_file(
        &self,
        file: &SchemeFile<Self>,
        data: &mut Self::SchemeDataT,
    ) -> SyscallResult<()>;

    fn on_last_close(
        _sockets: &mut SocketSet,
        _handle: SocketHandle,
        _file: &SchemeFile<Self>,
        _data: &mut Self::SchemeDataT,
        _now_us: i64,
    ) -> bool {
        true
    }

    fn sweep_lingering(
        _sockets: &mut SocketSet,
        _data: &mut Self::SchemeDataT,
        _now_us: i64,
    ) {
    }

    fn write_buf(&mut self, file: &mut SocketFile<Self::DataT>, buf: &[u8])
        -> SyscallResult<usize>;

    fn read_buf(
        &mut self,
        file: &mut SocketFile<Self::DataT>,
        buf: &mut [u8],
    ) -> SyscallResult<usize>;

    fn fpath(&self, file: &SchemeFile<Self>, buf: &mut [u8]) -> SyscallResult<usize>;

    fn dup(
        sockets: &mut SocketSet,
        file: &mut SchemeFile<Self>,
        path: &str,
        data: &mut Self::SchemeDataT,
    ) -> SyscallResult<DupResult<Self>>;

    fn handle_get_peer_name(
        &self,
        file: &SchemeFile<Self>,
        payload: &mut [u8],
    ) -> SyscallResult<usize>;

    fn handle_shutdown(&mut self, file: &mut SchemeFile<Self>, how: usize) -> SyscallResult<usize>;

    fn handle_recvmsg(
        &mut self,
        file: &mut SchemeFile<Self>,
        payload: &mut [u8],
        flags: usize,
    ) -> SyscallResult<usize>;

    fn supports_tcp_opts() -> bool {
        false
    }

    fn sync_state(
        _sockets: &mut SocketSet,
        _handle: SocketHandle,
        _file: &mut SocketFile<Self::DataT>,
        _now_us: i64,
    ) {
    }

    fn is_readable(sockets: &mut SocketSet, handle: SocketHandle, data: &Self::DataT) -> bool
    where
        Self: AnySocket<'static>,
    {
        let socket = sockets.get_mut::<Self>(handle);
        socket.can_recv(data) || !socket.may_recv()
    }

    fn handle_listen(
        _sockets: &mut SocketSet,
        _file: &mut SocketFile<Self::DataT>,
        _backlog: usize,
        _data: &mut Self::SchemeDataT,
    ) -> SyscallResult<usize> {
        Err(SyscallError::new(syscall::EOPNOTSUPP))
    }

    fn set_sock_opt(
        &mut self,
        _file: &mut SocketFile<Self::DataT>,
        _level: usize,
        _name: usize,
        _buf: &[u8],
    ) -> SyscallResult<usize> {
        Err(SyscallError::new(syscall::ENOPROTOOPT))
    }

    fn get_sock_opt(
        &self,
        _file: &SchemeFile<Self>,
        _level: usize,
        _name: usize,
        _buf: &mut [u8],
    ) -> SyscallResult<usize> {
        Err(SyscallError::new(syscall::ENOPROTOOPT))
    }

    #[expect(
        unused_variables,
        reason = "Only UDP needs to verify the path before duplicating in case of accept on a null socket"
    )]
    fn validate_dup(path: &str) -> SyscallResult<()> {
        Ok(())
    }
}

pub enum Handle<SocketT>
where
    SocketT: SchemeSocket,
{
    SchemeRoot,
    Null(NullFile),
    File(SchemeFile<SocketT>),
}

pub struct SocketScheme<SocketT>
where
    SocketT: SchemeSocket + AnySocket<'static>,
{
    pub handles: HandleMap<Handle<SocketT>>,
    ref_counts: BTreeMap<SocketHandle, usize>,
    context: Context,
    pub socket_set: Rc<RefCell<SocketSet>>,
    pub scheme_file: Socket,
    scheme_data: SocketT::SchemeDataT,
    _phantom_socket: PhantomData<SocketT>,
}

impl<SocketT> SocketScheme<SocketT>
where
    SocketT: SchemeSocket + AnySocket<'static>,
{
    pub fn new(
        name: &str,
        iface: Interface,
        route_table: Rc<RefCell<RouteTable>>,
        socket_set: Rc<RefCell<SocketSet>>,
        scheme_file: Socket,
        timer: ::std::time::Instant,
    ) -> SyscallResult<SocketScheme<SocketT>> {
        let mut scheme = SocketScheme {
            handles: HandleMap::new(),
            ref_counts: BTreeMap::new(),
            socket_set,
            scheme_data: SocketT::new_scheme_data(),
            scheme_file,
            _phantom_socket: PhantomData,
            context: Context {
                iface,
                route_table,
                timer,
            },
        };
        let cap_id = scheme.scheme_root()?;
        register_scheme_inner(&scheme.scheme_file, name, cap_id)?;
        Ok(scheme)
    }

    pub fn now_us(&self) -> i64 {
        self.context.now_us()
    }

    pub fn sweep_lingering(&mut self, now_us: i64) {
        let mut socket_set = self.socket_set.borrow_mut();
        SocketT::sweep_lingering(&mut socket_set, &mut self.scheme_data, now_us);
    }

    pub fn handle_block(&mut self, op: &Op) -> SyscallResult<Option<TimeSpec>> {
        let fd = op.file_id().expect("op is not fd based request");
        let (read_timeout, write_timeout) = {
            let handle = self.handles.get(fd)?;

            if let Handle::File(SchemeFile::Socket(ref scheme_file)) = *handle {
                Ok((scheme_file.read_timeout, scheme_file.write_timeout))
            } else {
                Err(SyscallError::new(syscall::EBADF))
            }
        }?;

        let mut timeout = match op {
            Op::Read(_) => read_timeout,
            Op::Write(_) => write_timeout,
            _ => None,
        };

        if let Some(ref mut timeout) = timeout {
            let cur_time = libredox::call::clock_gettime(CLOCK_MONOTONIC)?;
            *timeout = add_time(
                timeout,
                &TimeSpec {
                    tv_sec: cur_time.tv_sec,
                    tv_nsec: cur_time.tv_nsec as i32,
                },
            )
        }

        Ok(timeout)
    }

    fn get_setting(
        &mut self,
        fd: usize,
        setting: Setting<SocketT::SettingT>,
        buf: &mut [u8],
    ) -> SyscallResult<usize> {
        let file = self.handles.get_mut(fd)?;

        let file = match *file {
            Handle::File(SchemeFile::Socket(ref mut file)) => file,
            _ => return Err(SyscallError::new(syscall::EBADF)),
        };

        match setting {
            Setting::Other(setting) => SocketT::get_setting(file, setting, buf),
            Setting::HopLimit => {
                if let Some(hop_limit) = buf.get_mut(0) {
                    let socket_set = self.socket_set.borrow();
                    let socket = socket_set.get::<SocketT>(file.socket_handle);
                    *hop_limit = socket.hop_limit();
                    Ok(1)
                } else {
                    Err(SyscallError::new(syscall::EIO))
                }
            }
            Setting::ReadTimeout | Setting::WriteTimeout => {
                let timespec = match (setting, file.read_timeout, file.write_timeout) {
                    (Setting::ReadTimeout, Some(read_timeout), _) => read_timeout,
                    (Setting::WriteTimeout, _, Some(write_timeout)) => write_timeout,
                    _ => {
                        return Ok(0);
                    }
                };

                if buf.len() < size_of::<TimeSpec>() {
                    Ok(0)
                } else {
                    let count = timespec.deref().read(buf).map_err(|err| {
                        SyscallError::new(err.raw_os_error().unwrap_or(syscall::EIO))
                    })?;
                    Ok(count)
                }
            }
        }
    }

    fn update_setting(
        &mut self,
        fd: usize,
        setting: Setting<SocketT::SettingT>,
        buf: &[u8],
    ) -> SyscallResult<usize> {
        let file = self.handles.get_mut(fd)?;
        let file = match *file {
            Handle::File(SchemeFile::Socket(ref mut file)) => file,
            _ => return Err(SyscallError::new(syscall::EBADF)),
        };
        match setting {
            Setting::ReadTimeout | Setting::WriteTimeout => {
                let (timeout, count) = {
                    if buf.len() < size_of::<TimeSpec>() {
                        (None, 0)
                    } else {
                        let mut timespec = TimeSpec::default();
                        let count = timespec.deref_mut().write(buf).map_err(|err| {
                            SyscallError::new(err.raw_os_error().unwrap_or(syscall::EIO))
                        })?;
                        if timespec.tv_nsec == 0 && timespec.tv_sec == 0 {
                            (None, 0) // disable timeout
                        } else {
                            (Some(timespec), count)
                        }
                    }
                };
                match setting {
                    Setting::ReadTimeout => {
                        file.read_timeout = timeout;
                    }
                    Setting::WriteTimeout => {
                        file.write_timeout = timeout;
                    }
                    _ => {}
                };
                Ok(count)
            }
            Setting::HopLimit => {
                if let Some(hop_limit) = buf.first() {
                    let mut socket_set = self.socket_set.borrow_mut();
                    let socket = socket_set.get_mut::<SocketT>(file.socket_handle);
                    socket.set_hop_limit(*hop_limit);
                    Ok(1)
                } else {
                    Err(SyscallError::new(syscall::EIO))
                }
            }
            Setting::Other(setting) => SocketT::set_setting(file, setting, buf),
        }
    }

    fn call_inner(
        &mut self,
        fd: usize,
        payload: &mut [u8],
        metadata: &[u64],
        _ctx: &CallerCtx,
    ) -> SyscallResult<usize> {
        let raw_verb = metadata.first().copied().unwrap_or(usize::MAX as u64) as usize;
        if raw_verb == SOCKET_CALL_LISTEN {
            let backlog = metadata.get(1).copied().unwrap_or(0) as usize;
            let handle = self.handles.get_mut(fd)?;
            return match *handle {
                Handle::File(SchemeFile::Socket(ref mut sock_file)) => {
                    let mut socket_set = self.socket_set.borrow_mut();
                    SocketT::handle_listen(
                        &mut socket_set,
                        sock_file,
                        backlog,
                        &mut self.scheme_data,
                    )
                }
                Handle::Null(_) => Err(SyscallError::new(syscall::EINVAL)),
                _ => Err(SyscallError::new(syscall::EBADF)),
            };
        }
        let Some(verb) = SocketCall::try_from_raw(raw_verb) else {
            warn!("Invalid verb in metadata: {:?}", metadata);
            return Err(SyscallError::new(EINVAL));
        };
        match verb {
            SocketCall::SetSockOpt => {
                let name = metadata.get(1).copied().unwrap_or(0) as usize;
                let level = match metadata.get(2).copied().unwrap_or(0) {
                    0 => SOL_SOCKET,
                    raw => usize::try_from(raw).unwrap_or(usize::MAX),
                };
                let handle = self.handles.get_mut(fd)?;
                match *handle {
                    Handle::Null(ref mut null) => match (level, name) {
                        (SOL_SOCKET, SO_RCVBUF) => {
                            let raw = parse_i32_opt(payload)?;
                            if raw <= 0 {
                                return Err(SyscallError::new(syscall::EINVAL));
                            }
                            null.opts.rcvbuf = Some(SocketT::clamp_buf_bytes(raw as usize));
                            Ok(0)
                        }
                        (SOL_SOCKET, SO_SNDBUF) => {
                            let raw = parse_i32_opt(payload)?;
                            if raw <= 0 {
                                return Err(SyscallError::new(syscall::EINVAL));
                            }
                            null.opts.sndbuf = Some(SocketT::clamp_buf_bytes(raw as usize));
                            Ok(0)
                        }
                        (SOL_SOCKET, SO_REUSEADDR) => {
                            null.opts.reuseaddr = parse_i32_opt(payload)? != 0;
                            Ok(0)
                        }
                        (SOL_SOCKET, SO_KEEPALIVE) if SocketT::supports_tcp_opts() => {
                            null.opts.keepalive = parse_i32_opt(payload)? != 0;
                            Ok(0)
                        }
                        (SOL_SOCKET, SO_RCVTIMEO) => {
                            null.opts.read_timeout = parse_timeval_opt(payload)?;
                            Ok(0)
                        }
                        (SOL_SOCKET, SO_SNDTIMEO) => {
                            null.opts.write_timeout = parse_timeval_opt(payload)?;
                            Ok(0)
                        }
                        (IPPROTO_TCP, TCP_NODELAY) if SocketT::supports_tcp_opts() => {
                            null.opts.nodelay = parse_i32_opt(payload)? != 0;
                            Ok(0)
                        }
                        (IPPROTO_TCP, TCP_KEEPIDLE) if SocketT::supports_tcp_opts() => {
                            let raw = parse_i32_opt(payload)?;
                            if !(MIN_TCP_KEEPIDLE_SECS..=MAX_TCP_KEEPIDLE_SECS).contains(&raw) {
                                return Err(SyscallError::new(syscall::EINVAL));
                            }
                            null.opts.keepidle_secs = raw as u32;
                            Ok(0)
                        }
                        _ => Err(SyscallError::new(syscall::ENOPROTOOPT)),
                    },
                    Handle::File(SchemeFile::Socket(ref mut sock_file)) => match (level, name) {
                        (SOL_SOCKET, SO_RCVBUF | SO_SNDBUF) => {
                            let raw = parse_i32_opt(payload)?;
                            if raw <= 0 {
                                return Err(SyscallError::new(syscall::EINVAL));
                            }
                            let _ = SocketT::clamp_buf_bytes(raw as usize);
                            Ok(0)
                        }
                        (SOL_SOCKET, SO_REUSEADDR) => {
                            sock_file.reuseaddr = parse_i32_opt(payload)? != 0;
                            Ok(0)
                        }
                        (SOL_SOCKET, SO_RCVTIMEO) => {
                            sock_file.read_timeout = parse_timeval_opt(payload)?;
                            Ok(0)
                        }
                        (SOL_SOCKET, SO_SNDTIMEO) => {
                            sock_file.write_timeout = parse_timeval_opt(payload)?;
                            Ok(0)
                        }
                        _ => {
                            let mut socket_set = self.socket_set.borrow_mut();
                            let socket = socket_set.get_mut::<SocketT>(sock_file.socket_handle);
                            SocketT::set_sock_opt(socket, sock_file, level, name, payload)
                        }
                    },
                    Handle::File(SchemeFile::Setting(_)) | Handle::SchemeRoot => {
                        Err(SyscallError::new(syscall::EBADF))
                    }
                }
            }
            SocketCall::GetSockOpt => {
                let name = metadata.get(1).copied().unwrap_or(0) as usize;
                let level = match metadata.get(2).copied().unwrap_or(0) {
                    0 => SOL_SOCKET,
                    raw => usize::try_from(raw).unwrap_or(usize::MAX),
                };
                let now_us = self.context.now_us();
                let handle = self.handles.get_mut(fd)?;

                match *handle {
                    Handle::File(ref mut file) => {
                        let mut socket_set = self.socket_set.borrow_mut();
                        if let SchemeFile::Socket(ref mut sock_file) = *file {
                            let socket_handle = sock_file.socket_handle;
                            SocketT::sync_state(&mut socket_set, socket_handle, sock_file, now_us);
                            match (level, name) {
                                (SOL_SOCKET, SO_ERROR) => {
                                    let err = sock_file.pending_error.take().unwrap_or(0);
                                    sock_file.err_notified = false;
                                    return write_i32_opt(payload, err);
                                }
                                (SOL_SOCKET, SO_REUSEADDR) => {
                                    return write_i32_opt(payload, i32::from(sock_file.reuseaddr));
                                }
                                (SOL_SOCKET, SO_RCVTIMEO) => {
                                    return write_timeval_opt(payload, sock_file.read_timeout);
                                }
                                (SOL_SOCKET, SO_SNDTIMEO) => {
                                    return write_timeval_opt(payload, sock_file.write_timeout);
                                }
                                _ => {}
                            }
                        }
                        let socket = socket_set.get_mut::<SocketT>(file.socket_handle());
                        SocketT::get_sock_opt(socket, file, level, name, payload)
                    }
                    Handle::Null(ref null) => match (level, name) {
                        (SOL_SOCKET, SO_RCVBUF) => write_i32_opt(
                            payload,
                            null.opts.rcvbuf.unwrap_or_else(SocketT::default_rx_buf_bytes) as i32,
                        ),
                        (SOL_SOCKET, SO_SNDBUF) => write_i32_opt(
                            payload,
                            null.opts.sndbuf.unwrap_or_else(SocketT::default_tx_buf_bytes) as i32,
                        ),
                        (SOL_SOCKET, SO_REUSEADDR) => {
                            write_i32_opt(payload, i32::from(null.opts.reuseaddr))
                        }
                        (SOL_SOCKET, SO_ERROR) => write_i32_opt(payload, 0),
                        (SOL_SOCKET, SO_KEEPALIVE) if SocketT::supports_tcp_opts() => {
                            write_i32_opt(payload, i32::from(null.opts.keepalive))
                        }
                        (SOL_SOCKET, SO_RCVTIMEO) => {
                            write_timeval_opt(payload, null.opts.read_timeout)
                        }
                        (SOL_SOCKET, SO_SNDTIMEO) => {
                            write_timeval_opt(payload, null.opts.write_timeout)
                        }
                        (IPPROTO_TCP, TCP_NODELAY) if SocketT::supports_tcp_opts() => {
                            write_i32_opt(payload, i32::from(null.opts.nodelay))
                        }
                        (IPPROTO_TCP, TCP_KEEPIDLE) if SocketT::supports_tcp_opts() => {
                            write_i32_opt(payload, null.opts.keepidle_secs as i32)
                        }
                        _ => Err(SyscallError::new(syscall::ENOPROTOOPT)),
                    },
                    Handle::SchemeRoot => Err(SyscallError::new(syscall::EBADF)),
                }
            }
            // SocketCall::SendMsg => self.handle_sendmsg(id, payload, ctx),
            // SocketCall::Unbind => self.handle_unbind(id),
            // SocketCall::GetToken => self.handle_get_token(id, payload),
            SocketCall::GetPeerName => {
                let file = self.handles.get_mut(fd)?;

                let file = match *file {
                    Handle::File(ref mut f) => f,
                    _ => return Err(SyscallError::new(syscall::EBADF)),
                };
                let mut socket_set = self.socket_set.borrow_mut();
                let socket = socket_set.get_mut::<SocketT>(file.socket_handle());

                SocketT::handle_get_peer_name(socket, file, payload)
            }
            SocketCall::RecvMsg => {
                let flags = metadata[1] as usize;
                let handle = self.handles.get_mut(fd)?;

                match *handle {
                    Handle::File(ref mut file) => {
                        let mut socket_set = self.socket_set.borrow_mut();
                        let socket = socket_set.get_mut::<SocketT>(file.socket_handle());

                        SocketT::handle_recvmsg(socket, file, payload, flags)
                    }
                    Handle::Null(_) => Err(SyscallError::new(syscall::EINVAL)),
                    Handle::SchemeRoot => Err(SyscallError::new(syscall::EBADF)),
                }
            }

            SocketCall::Shutdown => {
                let how = metadata[1] as usize;

                match self.handles.get_mut(fd)? {
                    Handle::File(file) => {
                        let mut socket_set = self.socket_set.borrow_mut();
                        let socket = socket_set.get_mut::<SocketT>(file.socket_handle());

                        SocketT::handle_shutdown(socket, file, how)
                    }
                    Handle::Null(null_file) => {
                        match how {
                            0 => null_file.read_enabled = false,
                            1 => null_file.write_enabled = false,
                            2 => {
                                null_file.read_enabled = false;
                                null_file.write_enabled = false;
                            }
                            _ => return Err(SyscallError::new(EINVAL)),
                        }
                        Ok(0)
                    }
                    Handle::SchemeRoot => Err(SyscallError::new(syscall::EBADF)),
                }
            }
            _ => Err(SyscallError::new(EOPNOTSUPP)),
        }
    }

    fn open_inner(
        &mut self,
        path: &str,
        flags: usize,
        uid: u32,
        gid: u32,
        read_enabled: bool,
        write_enabled: bool,
        opts: SocketOpts,
    ) -> SyscallResult<OpenResult> {
        if path.is_empty() {
            let null = NullFile {
                flags,
                uid,
                gid,
                read_enabled,
                write_enabled,
                opts,
            };

            let id = self.handles.insert(Handle::Null(null));

            Ok(OpenResult::ThisScheme {
                number: id,
                flags: NewFdFlags::empty(),
            })
        } else {
            let (socket_handle, data) = SocketT::new_socket(
                &mut self.socket_set.borrow_mut(),
                path,
                uid,
                &mut self.scheme_data,
                &self.context,
                &opts,
            )?;

            let file = SchemeFile::Socket(SocketFile {
                flags,
                events: 0,
                socket_handle,
                read_notified: false,
                write_notified: false,
                err_notified: false,
                write_timeout: opts.write_timeout,
                read_timeout: opts.read_timeout,
                read_enabled,
                write_enabled,
                reuseaddr: opts.reuseaddr,
                keepidle_secs: opts.keepidle_secs,
                pending_error: None,
                reset_pending: false,
                data,
            });

            self.ref_counts.insert(socket_handle, 1);
            let id = self.handles.insert(Handle::File(file));

            Ok(OpenResult::ThisScheme {
                number: id,
                flags: NewFdFlags::empty(),
            })
        }
    }
}

impl<SocketT> SchemeSync for SocketScheme<SocketT>
where
    SocketT: SchemeSocket + AnySocket<'static>,
{
    fn scheme_root(&mut self) -> SyscallResult<usize> {
        Ok(self.handles.insert(Handle::SchemeRoot))
    }

    fn openat(
        &mut self,
        fd: usize,
        path: &str,
        flags: usize,
        _fcntl_flags: u32,
        ctx: &CallerCtx,
    ) -> SyscallResult<OpenResult> {
        match self.handles.get(fd)? {
            Handle::SchemeRoot => {
                self.open_inner(path, flags, ctx.uid, ctx.gid, true, true, SocketOpts::default())
            }
            _ => Err(SyscallError::new(syscall::EACCES)),
        }
    }

    fn call(
        &mut self,
        id: usize,
        payload: &mut [u8],
        metadata: &[u64],
        ctx: &CallerCtx,
    ) -> SyscallResult<usize> {
        self.call_inner(id, payload, metadata, ctx)
    }

    fn on_close(&mut self, fd: usize) {
        let Some(handle) = self.handles.remove(fd) else {
            return;
        };

        // incorrect, and kernel can't send close until all references are gone
        /*self.wait_queue.retain(
            |&WaitHandle {
                 packet: SyscallPacket { a, .. },
                 ..
             }| a != fd,
        );*/

        match handle {
            Handle::SchemeRoot => (),
            Handle::Null(_) => (),
            Handle::File(scheme_file) => {
                let socket_handle = scheme_file.socket_handle();
                let mut socket_set = self.socket_set.borrow_mut();

                let socket = socket_set.get::<SocketT>(socket_handle);
                let _ = socket.close_file(&scheme_file, &mut self.scheme_data);

                let remove = match self.ref_counts.entry(socket_handle) {
                    Entry::Vacant(_) => {
                        warn!("Closing a socket_handle with no ref");
                        true
                    }
                    Entry::Occupied(mut e) => {
                        if *e.get() == 0 {
                            warn!("Closing a socket_handle with no ref");
                            e.remove();
                            true
                        } else {
                            *e.get_mut() -= 1;
                            if *e.get() == 0 {
                                e.remove();
                                true
                            } else {
                                false
                            }
                        }
                    }
                };

                if remove {
                    let now_us = self.context.now_us();
                    if SocketT::on_last_close(
                        &mut socket_set,
                        socket_handle,
                        &scheme_file,
                        &mut self.scheme_data,
                        now_us,
                    ) {
                        socket_set.remove(socket_handle);
                    }
                }
            }
        }
    }

    fn write(
        &mut self,
        fd: usize,
        buf: &[u8],
        _offset: u64,
        _fcntl_flags: u32,
        _ctx: &CallerCtx,
    ) -> SyscallResult<usize> {
        let (fd, setting) = {
            let file = self.handles.get_mut(fd)?;

            match *file {
                Handle::File(SchemeFile::Setting(ref setting_handle)) => {
                    (setting_handle.fd, setting_handle.setting)
                }
                Handle::File(SchemeFile::Socket(ref mut file)) => {
                    let mut socket_set = self.socket_set.borrow_mut();
                    let socket_handle = file.socket_handle;
                    SocketT::sync_state(&mut socket_set, socket_handle, file, self.context.now_us());
                    let socket = socket_set.get_mut::<SocketT>(socket_handle);
                    let ret = SocketT::write_buf(socket, file, buf);
                    match ret {
                        Err(e) if e.errno == syscall::EWOULDBLOCK || e.errno == syscall::EAGAIN => {
                        }
                        _ => file.write_notified = false,
                    }
                    return ret;
                }
                _ => return Err(SyscallError::new(syscall::EBADF)),
            }
        };
        self.update_setting(fd, setting, buf)
    }

    fn read(
        &mut self,
        fd: usize,
        buf: &mut [u8],
        _offset: u64,
        _fcntl_flags: u32,
        _ctx: &CallerCtx,
    ) -> SyscallResult<usize> {
        let (fd, setting) = {
            let file = self.handles.get_mut(fd)?;
            match *file {
                Handle::File(SchemeFile::Setting(ref setting_handle)) => {
                    (setting_handle.fd, setting_handle.setting)
                }
                Handle::File(SchemeFile::Socket(ref mut file)) => {
                    let mut socket_set = self.socket_set.borrow_mut();
                    let socket_handle = file.socket_handle;
                    SocketT::sync_state(&mut socket_set, socket_handle, file, self.context.now_us());
                    let socket = socket_set.get_mut::<SocketT>(socket_handle);

                    let ret = SocketT::read_buf(socket, file, buf);
                    match ret {
                        Err(e) if e.errno == syscall::EWOULDBLOCK || e.errno == syscall::EAGAIN => {
                        }
                        _ => file.read_notified = false,
                    }

                    return ret;
                }
                _ => return Err(SyscallError::new(syscall::EBADF)),
            }
        };
        self.get_setting(fd, setting, buf)
    }

    fn dup(&mut self, fd: usize, buf: &[u8], _ctx: &CallerCtx) -> SyscallResult<OpenResult> {
        let path = str::from_utf8(buf).map_err(|_| SyscallError::new(syscall::EINVAL))?;

        let new_file = {
            let handle = self.handles.get_mut(fd)?;

            let file = match *handle {
                Handle::SchemeRoot => return Err(SyscallError::new(syscall::EBADF)),
                Handle::Null(ref null) => {
                    let (flags, uid, gid, read_enabled, write_enabled, opts) = (
                        null.flags,
                        null.uid,
                        null.gid,
                        null.read_enabled,
                        null.write_enabled,
                        null.opts,
                    );

                    let path =
                        str::from_utf8(buf).map_err(|_| SyscallError::new(syscall::EINVAL))?;

                    SocketT::validate_dup(path)?;

                    return self.open_inner(path, flags, uid, gid, read_enabled, write_enabled, opts);
                }
                Handle::File(ref mut file) => file,
            };

            let socket_handle = file.socket_handle();

            let (new_handle, update_with) = match path {
                "hop_limit" => (
                    SchemeFile::Setting(SettingFile {
                        socket_handle,
                        fd,
                        setting: Setting::HopLimit,
                    }),
                    None,
                ),
                "read_timeout" => (
                    SchemeFile::Setting(SettingFile {
                        socket_handle,
                        fd,
                        setting: Setting::ReadTimeout,
                    }),
                    None,
                ),
                "write_timeout" => (
                    SchemeFile::Setting(SettingFile {
                        socket_handle,
                        fd,
                        setting: Setting::WriteTimeout,
                    }),
                    None,
                ),
                _ => match SocketT::dup(
                    &mut self.socket_set.borrow_mut(),
                    file,
                    path,
                    &mut self.scheme_data,
                )? {
                    Some(some) => some,
                    None => return Err(SyscallError::new(syscall::EWOULDBLOCK)),
                },
            };

            if let Some((socket_handle, data)) = update_with {
                if let SchemeFile::Socket(ref mut file) = *file {
                    // We replace the socket_handle pointed by file so update the ref_counts
                    // accordingly
                    self.ref_counts
                        .entry(file.socket_handle)
                        .and_modify(|e| *e = e.saturating_sub(1))
                        .or_insert(0);

                    *self
                        .ref_counts
                        .entry(socket_handle)
                        .or_insert(0) += 1;

                    file.socket_handle = socket_handle;
                    file.data = data;
                }
            }
            *self
                .ref_counts
                .entry(new_handle.socket_handle())
                .or_insert(0) += 1;
            new_handle
        };

        let id = self.handles.insert(Handle::File(new_file));

        Ok(OpenResult::ThisScheme {
            number: id,
            flags: NewFdFlags::empty(),
        })
    }

    fn fevent(
        &mut self,
        fd: usize,
        events: SyscallEventFlags,
        _ctx: &CallerCtx,
    ) -> SyscallResult<SyscallEventFlags> {
        let file = self.handles.get_mut(fd)?;

        let file = match *file {
            Handle::File(ref mut f) => f,
            _ => return Err(SyscallError::new(syscall::EBADF)),
        };

        match *file {
            SchemeFile::Setting(_) => return Err(SyscallError::new(syscall::EBADF)),
            SchemeFile::Socket(ref mut file) => {
                file.events = events.bits();
                file.read_notified = false;
                file.write_notified = false;
                file.err_notified = false;
            }
        }
        let now_us = self.context.now_us();
        let mut socket_set = self.socket_set.borrow_mut();
        let revents = SyscallEventFlags::from_bits_retain(file.events(&mut socket_set, now_us));
        Ok(revents)
    }

    fn fsync(&mut self, fd: usize, _ctx: &CallerCtx) -> SyscallResult<()> {
        {
            let _file = self.handles.get_mut(fd)?;
        }
        Ok(())
        // TODO Implement fsyncing
        // self.0.network_fsync()
    }

    fn fpath(&mut self, fd: usize, buf: &mut [u8], _ctx: &CallerCtx) -> SyscallResult<usize> {
        let file = self.handles.get_mut(fd)?;

        match *file {
            Handle::File(ref mut f) => {
                let socket_set = self.socket_set.borrow();
                let socket = socket_set.get::<SocketT>(f.socket_handle());

                socket.fpath(f, buf)
            }
            Handle::Null(_) => {
                let path = SocketT::BASE_PATH.as_bytes();
                let count = path.len().min(buf.len());
                buf[..count].copy_from_slice(&path[..count]);

                Ok(count)
            }
            _ => Err(SyscallError::new(syscall::EBADF)),
        }
    }

    fn fcntl(
        &mut self,
        fd: usize,
        cmd: usize,
        arg: usize,
        _ctx: &CallerCtx,
    ) -> SyscallResult<usize> {
        let handle = self.handles.get_mut(fd)?;

        match *handle {
            Handle::File(SchemeFile::Socket(ref mut socket_file)) => match cmd {
                syscall::F_GETFL => Ok(socket_file.flags),
                syscall::F_SETFL => {
                    socket_file.flags = arg & !syscall::O_ACCMODE;
                    Ok(0)
                }
                _ => Err(SyscallError::new(syscall::EINVAL)),
            },
            Handle::Null(ref mut null) => match cmd {
                syscall::F_GETFL => Ok(null.flags),
                syscall::F_SETFL => {
                    null.flags = arg & !syscall::O_ACCMODE;
                    Ok(0)
                }
                _ => Err(SyscallError::new(syscall::EINVAL)),
            },
            _ => Err(SyscallError::new(syscall::EBADF)),
        }
    }
}

fn add_time(a: &TimeSpec, b: &TimeSpec) -> TimeSpec {
    let mut secs = a.tv_sec + b.tv_sec;
    let mut nsecs = a.tv_nsec + b.tv_nsec;

    secs += i64::from(nsecs) / 1_000_000_000;
    nsecs %= 1_000_000_000;

    TimeSpec {
        tv_sec: secs,
        tv_nsec: nsecs,
    }
}

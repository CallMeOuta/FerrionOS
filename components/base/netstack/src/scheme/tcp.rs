use std::str;

use libredox::flag;
use scheme_utils::FpathWriter;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::tcp::{
    Socket as TcpSocket, SocketBuffer as TcpSocketBuffer, State as TcpState,
};
use smoltcp::time::Duration as SmolDuration;
use smoltcp::wire::{IpEndpoint, IpListenEndpoint};
use syscall::{self, Error as SyscallError, Result as SyscallResult};

use super::socket::{Context, DupResult, SchemeFile, SchemeSocket, SocketFile, SocketOpts};
use super::{parse_endpoint, recvmsg, SchemeWrapper, SocketSet};
use crate::port_set::PortSet;

const SOL_SOCKET: usize = 1;
const IPPROTO_TCP: usize = 6;
const SO_SNDBUF: usize = 7;
const SO_RCVBUF: usize = 8;
const SO_KEEPALIVE: usize = 9;
const TCP_NODELAY: usize = 1;
const TCP_KEEPIDLE: usize = 4;
const MIN_TCP_KEEPIDLE_SECS: i32 = 1;
const MAX_TCP_KEEPIDLE_SECS: i32 = 86_400;
const TCP_CONNECT_TIMEOUT_SECS: u64 = 30;
const MAX_LISTEN_BACKLOG: usize = 64;
const TCP_DEFAULT_BUF_BYTES: usize = 1048576;
const TCP_MIN_BUF_BYTES: usize = 16 * 1024;
const TCP_MAX_BUF_BYTES: usize = 4 * 1024 * 1024;
const MAX_LINGERING_SOCKETS: usize = 64;
const LINGER_TIMEOUT_US: i64 = 15_000_000;

struct LingeringTcp {
    handle: SocketHandle,
    port: u16,
    started_us: i64,
}

pub struct TcpSchemeData {
    pub port_set: PortSet,
    lingering: Vec<LingeringTcp>,
}

#[derive(Clone)]
pub struct TcpListenPool {
    pub endpoint: IpListenEndpoint,
    pub rx_buf_bytes: usize,
    pub tx_buf_bytes: usize,
    pub nodelay: bool,
    pub keepalive: bool,
    pub keepidle_secs: u32,
    pub extra_handles: Vec<SocketHandle>,
}

#[derive(Clone)]
pub enum TcpRole {
    Connecting {
        local_port: u16,
        deadline_us: i64,
    },
    Connected {
        local_port: u16,
        saw_established: bool,
        saw_peer_fin: bool,
    },
    Listening(TcpListenPool),
}

fn apply_tcp_opts(socket: &mut TcpSocket<'_>, nodelay: bool, keepalive: bool, keepidle_secs: u32) {
    socket.set_nagle_enabled(!nodelay);
    if keepalive {
        socket.set_keep_alive(Some(SmolDuration::from_secs(u64::from(keepidle_secs))));
    } else {
        socket.set_keep_alive(None);
    }
}

pub type TcpScheme = SchemeWrapper<TcpSocket<'static>>;

impl<'a> SchemeSocket for TcpSocket<'a> {
    type SchemeDataT = TcpSchemeData;
    type DataT = TcpRole;
    type SettingT = ();

    const BASE_PATH: &'static str = "/scheme/tcp/";

    fn new_scheme_data() -> Self::SchemeDataT {
        TcpSchemeData {
            port_set: PortSet::new(49_152u16, 65_535u16).expect("Wrong TCP port numbers"),
            lingering: Vec::new(),
        }
    }

    fn can_send(&self) -> bool {
        self.can_send()
    }

    fn can_recv(&mut self, data: &Self::DataT) -> bool {
        if matches!(data, TcpRole::Listening(_)) {
            self.is_active()
        } else {
            smoltcp::socket::tcp::Socket::can_recv(self)
        }
    }

    fn may_recv(&self) -> bool {
        match self.state() {
            smoltcp::socket::tcp::State::Listen
            | smoltcp::socket::tcp::State::SynSent
            | smoltcp::socket::tcp::State::SynReceived => true,
            _ => smoltcp::socket::tcp::Socket::may_recv(self),
        }
    }

    fn hop_limit(&self) -> u8 {
        self.hop_limit().unwrap_or(64)
    }

    fn set_hop_limit(&mut self, hop_limit: u8) {
        self.set_hop_limit(Some(hop_limit));
    }

    fn get_setting(
        _file: &SocketFile<Self::DataT>,
        _setting: Self::SettingT,
        _buf: &mut [u8],
    ) -> SyscallResult<usize> {
        Ok(0)
    }

    fn set_setting(
        _file: &mut SocketFile<Self::DataT>,
        _setting: Self::SettingT,
        _buf: &[u8],
    ) -> SyscallResult<usize> {
        Ok(0)
    }

    fn default_buf_bytes() -> usize {
        TCP_DEFAULT_BUF_BYTES
    }

    fn clamp_buf_bytes(requested: usize) -> usize {
        requested.clamp(TCP_MIN_BUF_BYTES, TCP_MAX_BUF_BYTES)
    }

    fn new_socket(
        socket_set: &mut SocketSet,
        path: &str,
        uid: u32,
        port_set: &mut Self::SchemeDataT,
        context: &Context,
        opts: &SocketOpts,
    ) -> SyscallResult<(SocketHandle, Self::DataT)> {
        trace!("TCP open {}", path);
        let mut parts = path.split('/');
        let remote_endpoint = parse_endpoint(parts.next().unwrap_or(""));
        let mut local_endpoint = parse_endpoint(parts.next().unwrap_or(""));

        if local_endpoint.port > 0 && local_endpoint.port <= 1024 && uid != 0 {
            return Err(SyscallError::new(syscall::EACCES));
        }

        let rx_buf_bytes = opts
            .rcvbuf
            .map(Self::clamp_buf_bytes)
            .unwrap_or(TCP_DEFAULT_BUF_BYTES);
        let tx_buf_bytes = opts
            .sndbuf
            .map(Self::clamp_buf_bytes)
            .unwrap_or(TCP_DEFAULT_BUF_BYTES);
        assert!(rx_buf_bytes >= TCP_MIN_BUF_BYTES && rx_buf_bytes <= TCP_MAX_BUF_BYTES);
        assert!(tx_buf_bytes >= TCP_MIN_BUF_BYTES && tx_buf_bytes <= TCP_MAX_BUF_BYTES);
        let rx_packets = vec![0; rx_buf_bytes];
        let tx_packets = vec![0; tx_buf_bytes];
        let rx_buffer = TcpSocketBuffer::new(rx_packets);
        let tx_buffer = TcpSocketBuffer::new(tx_packets);
        let socket = TcpSocket::new(rx_buffer, tx_buffer);

        if local_endpoint.port == 0 {
            local_endpoint.port = port_set
                .port_set
                .get_port()
                .ok_or_else(|| SyscallError::new(syscall::EINVAL))?;
        } else if opts.reuseaddr {
            let lingering_count = port_set
                .lingering
                .iter()
                .filter(|entry| entry.port == local_endpoint.port)
                .count();
            if !port_set
                .port_set
                .claim_reusable(local_endpoint.port, lingering_count)
            {
                return Err(SyscallError::new(syscall::EADDRINUSE));
            }
        } else if !port_set.port_set.claim_port(local_endpoint.port) {
            return Err(SyscallError::new(syscall::EADDRINUSE));
        }

        let socket_handle = socket_set.add(socket);

        let tcp_socket = socket_set.get_mut::<TcpSocket>(socket_handle);
        apply_tcp_opts(tcp_socket, opts.nodelay, opts.keepalive, opts.keepidle_secs);

        let role = if remote_endpoint.is_specified() {
            let local_endpoint_addr = match local_endpoint.addr {
                Some(addr) if !addr.is_unspecified() => Some(addr),
                _ => {
                    let route_table = context.route_table.borrow();
                    let addr = route_table
                        .lookup_src_addr(&remote_endpoint.addr.expect("Checked in is_specified"));
                    if addr.is_none() {
                        error!("Opening a TCP connection with a probably invalid source IP as no route have been found for destination: {}", remote_endpoint);
                    }
                    addr
                }
            };
            let local_endpoint = IpListenEndpoint {
                addr: local_endpoint_addr,
                port: local_endpoint.port,
            };

            trace!("Connecting tcp {} {}", local_endpoint, remote_endpoint);
            tcp_socket.set_timeout(Some(SmolDuration::from_secs(TCP_CONNECT_TIMEOUT_SECS)));
            tcp_socket
                .connect(
                    context.iface.borrow_mut().context(),
                    IpEndpoint::new(remote_endpoint.addr.unwrap(), remote_endpoint.port),
                    local_endpoint,
                )
                .expect("Can't connect tcp socket ");
            TcpRole::Connecting {
                local_port: local_endpoint.port,
                deadline_us: context
                    .now_us()
                    .saturating_add((TCP_CONNECT_TIMEOUT_SECS as i64).saturating_mul(1_000_000)),
            }
        } else {
            trace!("Listening tcp {}", local_endpoint);
            tcp_socket
                .listen(local_endpoint)
                .expect("Can't listen on local endpoint");
            TcpRole::Listening(TcpListenPool {
                endpoint: local_endpoint,
                rx_buf_bytes,
                tx_buf_bytes,
                nodelay: opts.nodelay,
                keepalive: opts.keepalive,
                keepidle_secs: opts.keepidle_secs,
                extra_handles: Vec::new(),
            })
        };

        Ok((socket_handle, role))
    }

    fn close_file(
        &self,
        file: &SchemeFile<Self>,
        scheme_data: &mut Self::SchemeDataT,
    ) -> SyscallResult<()> {
        if let SchemeFile::Socket(SocketFile { ref data, .. }) = *file {
            if let Some(endpoint) = self.local_endpoint() {
                scheme_data.port_set.release_port(endpoint.port);
            } else {
                match *data {
                    TcpRole::Listening(ref pool) => {
                        scheme_data.port_set.release_port(pool.endpoint.port);
                    }
                    TcpRole::Connecting { local_port, .. }
                    | TcpRole::Connected { local_port, .. }
                        if local_port != 0 =>
                    {
                        scheme_data.port_set.release_port(local_port);
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn on_last_close(
        sockets: &mut SocketSet,
        handle: SocketHandle,
        file: &SchemeFile<Self>,
        scheme_data: &mut Self::SchemeDataT,
        now_us: i64,
    ) -> bool {
        if let SchemeFile::Socket(SocketFile {
            data: TcpRole::Listening(ref pool),
            ..
        }) = *file
        {
            for &extra in &pool.extra_handles {
                let extra_sock = sockets.get_mut::<TcpSocket>(extra);
                if extra_sock.state() != TcpState::Listen {
                    extra_sock.abort();
                } else {
                    extra_sock.close();
                }
                sockets.remove(extra);
                scheme_data.port_set.release_port(pool.endpoint.port);
            }
        }
        let (state, port) = {
            let tcp_socket = sockets.get_mut::<TcpSocket>(handle);
            tcp_socket.close();
            (
                tcp_socket.state(),
                tcp_socket.local_endpoint().map(|ep| ep.port).unwrap_or(0),
            )
        };
        if state == TcpState::Closed {
            return true;
        }
        if scheme_data.lingering.len() >= MAX_LINGERING_SOCKETS {
            let oldest = scheme_data.lingering.remove(0);
            sockets.get_mut::<TcpSocket>(oldest.handle).abort();
            sockets.remove(oldest.handle);
            if oldest.port != 0 {
                scheme_data.port_set.release_port(oldest.port);
            }
        }
        if port != 0 {
            scheme_data.port_set.acquire_port(port);
        }
        scheme_data.lingering.push(LingeringTcp {
            handle,
            port,
            started_us: now_us,
        });
        assert!(scheme_data.lingering.len() <= MAX_LINGERING_SOCKETS);
        false
    }

    fn sweep_lingering(
        sockets: &mut SocketSet,
        scheme_data: &mut Self::SchemeDataT,
        now_us: i64,
    ) {
        let mut i = 0usize;
        while i < scheme_data.lingering.len() {
            let entry = &scheme_data.lingering[i];
            let state = sockets.get::<TcpSocket>(entry.handle).state();
            let expired = now_us.saturating_sub(entry.started_us) >= LINGER_TIMEOUT_US;
            if state == TcpState::Closed || expired {
                let removed = scheme_data.lingering.swap_remove(i);
                if state != TcpState::Closed {
                    sockets.get_mut::<TcpSocket>(removed.handle).abort();
                }
                sockets.remove(removed.handle);
                if removed.port != 0 {
                    scheme_data.port_set.release_port(removed.port);
                }
            } else {
                i += 1;
            }
        }
    }

    fn write_buf(
        &mut self,
        file: &mut SocketFile<Self::DataT>,
        buf: &[u8],
    ) -> SyscallResult<usize> {
        if !file.write_enabled {
            Err(SyscallError::new(syscall::EPIPE))
        } else if file.reset_pending {
            file.reset_pending = false;
            file.pending_error = None;
            Err(SyscallError::new(syscall::ECONNRESET))
        } else if !self.is_active() {
            if let Some(err) = file.pending_error.take() {
                Err(SyscallError::new(err))
            } else {
                Err(SyscallError::new(syscall::ENOTCONN))
            }
        } else if self.can_send() {
            Ok(self.send_slice(buf).expect("Can't send slice"))
        } else if file.flags & syscall::O_NONBLOCK == syscall::O_NONBLOCK {
            Err(SyscallError::new(syscall::EAGAIN))
        } else {
            Err(SyscallError::new(syscall::EWOULDBLOCK))
        }
    }

    fn read_buf(
        &mut self,
        file: &mut SocketFile<Self::DataT>,
        buf: &mut [u8],
    ) -> SyscallResult<usize> {
        if !file.read_enabled {
            Ok(0)
        } else if self.can_recv(&file.data) {
            let length = self.recv_slice(buf).expect("Can't receive slice");
            Ok(length)
        } else if file.reset_pending {
            file.reset_pending = false;
            file.pending_error = None;
            Err(SyscallError::new(syscall::ECONNRESET))
        } else if !self.is_active() {
            let saw_fin = matches!(
                file.data,
                TcpRole::Connected {
                    saw_peer_fin: true,
                    ..
                }
            );
            if saw_fin {
                Ok(0)
            } else if let Some(err) = file.pending_error.take() {
                Err(SyscallError::new(err))
            } else {
                Err(SyscallError::new(syscall::ENOTCONN))
            }
        } else if !self.may_recv() {
            Ok(0)
        } else if file.flags & syscall::O_NONBLOCK == syscall::O_NONBLOCK {
            Err(SyscallError::new(syscall::EAGAIN))
        } else {
            Err(SyscallError::new(syscall::EWOULDBLOCK))
        }
    }

    fn dup(
        socket_set: &mut SocketSet,
        file: &mut SchemeFile<Self>,
        path: &str,
        port_set: &mut Self::SchemeDataT,
    ) -> SyscallResult<DupResult<Self>> {
        let socket_handle = file.socket_handle();

        let (is_active, local_endpoint) = {
            let socket = socket_set.get::<TcpSocket>(socket_handle);
            (socket.is_active(), socket.local_endpoint())
        };

        let file = match path {
            "listen" => {
                if let SchemeFile::Socket(ref mut tcp_handle) = *file {
                    let TcpRole::Listening(ref mut pool) = tcp_handle.data else {
                        return Err(SyscallError::new(syscall::EINVAL));
                    };

                    let ready_extra_idx = if is_active {
                        None
                    } else {
                        pool.extra_handles
                            .iter()
                            .position(|&h| socket_set.get::<TcpSocket>(h).is_active())
                    };

                    if !is_active && ready_extra_idx.is_none() {
                        if tcp_handle.flags & syscall::O_NONBLOCK == syscall::O_NONBLOCK {
                            return Err(SyscallError::new(syscall::EAGAIN));
                        } else {
                            return Ok(None);
                        }
                    }
                    trace!("TCP creating new listening socket");
                    let rx_packets = vec![0; pool.rx_buf_bytes];
                    let tx_packets = vec![0; pool.tx_buf_bytes];
                    let rx_buffer = TcpSocketBuffer::new(rx_packets);
                    let tx_buffer = TcpSocketBuffer::new(tx_packets);
                    let mut repl = TcpSocket::new(rx_buffer, tx_buffer);
                    apply_tcp_opts(&mut repl, pool.nodelay, pool.keepalive, pool.keepidle_secs);
                    let new_socket_handle = socket_set.add(repl);
                    {
                        let tcp_socket = socket_set.get_mut::<TcpSocket>(new_socket_handle);
                        tcp_socket
                            .listen(pool.endpoint)
                            .expect("Can't listen on local endpoint");
                    }
                    let connected_role = TcpRole::Connected {
                        local_port: pool.endpoint.port,
                        saw_established: true,
                        saw_peer_fin: false,
                    };
                    if let Some(idx) = ready_extra_idx {
                        let accepted_handle = pool.extra_handles[idx];
                        pool.extra_handles[idx] = new_socket_handle;
                        let accepted_port = socket_set
                            .get::<TcpSocket>(accepted_handle)
                            .local_endpoint()
                            .map(|ep| ep.port)
                            .unwrap_or(pool.endpoint.port);
                        port_set.port_set.acquire_port(accepted_port);
                        let new_handle = SchemeFile::Socket(
                            tcp_handle.clone_with_handle(accepted_handle, connected_role),
                        );
                        return Ok(Some((new_handle, None)));
                    }
                    let next_pool = pool.clone();
                    port_set.port_set.acquire_port(
                        local_endpoint
                            .expect("Socket was active so local endpoint must be set")
                            .port,
                    );
                    let new_handle =
                        SchemeFile::Socket(tcp_handle.clone_with_data(connected_role));
                    return Ok(Some((
                        new_handle,
                        Some((new_socket_handle, TcpRole::Listening(next_pool))),
                    )));
                } else {
                    return Err(SyscallError::new(syscall::EBADF));
                }
            }
            _ => {
                trace!("TCP dup unknown {}", path);
                if let SchemeFile::Socket(ref tcp_handle) = *file {
                    SchemeFile::Socket(tcp_handle.clone_with_data(tcp_handle.data.clone()))
                } else {
                    SchemeFile::Socket(SocketFile::new_with_data(
                        socket_handle,
                        TcpRole::Connected {
                            local_port: local_endpoint.map(|ep| ep.port).unwrap_or(0),
                            saw_established: true,
                            saw_peer_fin: false,
                        },
                    ))
                }
            }
        };

        if let SchemeFile::Socket(_) = file {
            if let Some(local_endpoint) = local_endpoint {
                port_set.port_set.acquire_port(local_endpoint.port);
            }
        }

        Ok(Some((file, None)))
    }

    fn fpath(&self, file: &SchemeFile<Self>, buf: &mut [u8]) -> SyscallResult<usize> {
        FpathWriter::with(buf, "tcp", |w| {
            let unspecified = "0.0.0.0:0";
            match self.remote_endpoint() {
                Some(endpoint) => write!(w, "{}", endpoint).unwrap(),
                None => w.push_str(unspecified),
            }
            w.push_str("/");
            match (self.local_endpoint(), file) {
                (Some(endpoint), _) => write!(w, "{}", endpoint).unwrap(),
                (
                    None,
                    SchemeFile::Socket(SocketFile {
                        data: TcpRole::Listening(ref pool),
                        ..
                    }),
                ) => {
                    let endpoint = pool.endpoint;
                    if endpoint.is_specified() {
                        write!(w, "{}", endpoint).unwrap()
                    } else {
                        write!(w, "0.0.0.0:{}", endpoint.port).unwrap()
                    }
                }
                _ => w.push_str(unspecified),
            }

            Ok(())
        })
    }

    fn handle_recvmsg(
        &mut self,
        file: &mut SchemeFile<Self>,
        how: &mut [u8],
        flags: usize,
    ) -> SyscallResult<usize> {
        let socket_file = match file {
            SchemeFile::Socket(ref mut sock_f) => sock_f,
            _ => return Err(SyscallError::new(syscall::EBADF)),
        };
        if !socket_file.read_enabled {
            Ok(0)
        } else if self.can_recv(&socket_file.data) {
            let request = recvmsg::Request::parse(how)?;
            let address = match self.remote_endpoint() {
                Some(endpoint) => format!("{}{}:{}", Self::BASE_PATH, endpoint.addr, endpoint.port),
                None => format!("{}0.0.0.0:0", Self::BASE_PATH),
            };

            let start = recvmsg::write_name(how, address.as_bytes(), request.payload_len)?;
            let payload_length = self
                .recv_slice(&mut how[start..start + request.payload_len])
                .unwrap();
            recvmsg::write_payload_len(how, start, payload_length);

            Ok(start + request.payload_len)
        } else if socket_file.flags & syscall::O_NONBLOCK == syscall::O_NONBLOCK
            || flags & flag::MSG_DONTWAIT as usize != 0
        {
            Err(SyscallError::new(syscall::EAGAIN))
        } else {
            Err(SyscallError::new(syscall::EWOULDBLOCK)) // internally scheduled to re-read
        }
    }

    fn handle_get_peer_name(
        &self,
        file: &SchemeFile<Self>,
        buf: &mut [u8],
    ) -> SyscallResult<usize> {
        self.fpath(file, buf)
    }

    fn handle_shutdown(&mut self, file: &mut SchemeFile<Self>, how: usize) -> SyscallResult<usize> {
        let socket_file = match file {
            SchemeFile::Socket(ref mut file) => file,
            _ => return Err(SyscallError::new(syscall::EBADF)),
        };

        match how {
            0 => socket_file.read_enabled = false, // SHUT_RD
            1 => {
                socket_file.write_enabled = false;
                self.close();
            } // SHUT_WR
            2 => {
                socket_file.read_enabled = false;
                socket_file.write_enabled = false;
                self.close();
            } // SHUT_RDWR
            _ => return Err(SyscallError::new(syscall::EINVAL)),
        }
        Ok(0)
    }

    fn supports_tcp_opts() -> bool {
        true
    }

    fn sync_state(
        sockets: &mut SocketSet,
        handle: SocketHandle,
        file: &mut SocketFile<Self::DataT>,
        now_us: i64,
    ) {
        let tcp_socket = sockets.get_mut::<TcpSocket>(handle);
        let state = tcp_socket.state();
        match file.data {
            TcpRole::Connecting {
                local_port,
                deadline_us,
            } => match state {
                TcpState::Established | TcpState::CloseWait => {
                    tcp_socket.set_timeout(None);
                    file.data = TcpRole::Connected {
                        local_port,
                        saw_established: true,
                        saw_peer_fin: state == TcpState::CloseWait,
                    };
                }
                TcpState::Closed => {
                    let err = if now_us >= deadline_us {
                        syscall::ETIMEDOUT
                    } else {
                        syscall::ECONNREFUSED
                    };
                    file.pending_error = Some(err);
                    file.data = TcpRole::Connected {
                        local_port,
                        saw_established: false,
                        saw_peer_fin: false,
                    };
                }
                TcpState::SynSent | TcpState::SynReceived => {
                    if now_us >= deadline_us {
                        tcp_socket.abort();
                        file.pending_error = Some(syscall::ETIMEDOUT);
                        file.data = TcpRole::Connected {
                            local_port,
                            saw_established: false,
                            saw_peer_fin: false,
                        };
                    }
                }
                _ => {
                    tcp_socket.set_timeout(None);
                    file.data = TcpRole::Connected {
                        local_port,
                        saw_established: true,
                        saw_peer_fin: true,
                    };
                }
            },
            TcpRole::Connected {
                saw_established,
                ref mut saw_peer_fin,
                ..
            } => match state {
                TcpState::CloseWait | TcpState::Closing | TcpState::LastAck | TcpState::TimeWait => {
                    *saw_peer_fin = true;
                }
                TcpState::Closed => {
                    if saw_established && !*saw_peer_fin {
                        *saw_peer_fin = true;
                        file.reset_pending = true;
                        file.pending_error.get_or_insert(syscall::ECONNRESET);
                    }
                }
                _ => {}
            },
            TcpRole::Listening(_) => {}
        }
    }

    fn is_readable(sockets: &mut SocketSet, handle: SocketHandle, data: &Self::DataT) -> bool {
        match data {
            TcpRole::Listening(pool) => {
                if sockets.get::<TcpSocket>(handle).is_active() {
                    return true;
                }
                pool.extra_handles
                    .iter()
                    .any(|&h| sockets.get::<TcpSocket>(h).is_active())
            }
            TcpRole::Connecting { .. } => false,
            TcpRole::Connected { .. } => {
                let socket = sockets.get_mut::<TcpSocket>(handle);
                smoltcp::socket::tcp::Socket::can_recv(socket) || !socket.may_recv()
            }
        }
    }

    fn handle_listen(
        sockets: &mut SocketSet,
        file: &mut SocketFile<Self::DataT>,
        backlog: usize,
        scheme_data: &mut Self::SchemeDataT,
    ) -> SyscallResult<usize> {
        let TcpRole::Listening(ref mut pool) = file.data else {
            return Err(SyscallError::new(syscall::EINVAL));
        };
        let target = backlog.clamp(1, MAX_LISTEN_BACKLOG);
        let desired_extra = target.saturating_sub(1);
        while pool.extra_handles.len() < desired_extra {
            let rx_buffer = TcpSocketBuffer::new(vec![0; pool.rx_buf_bytes]);
            let tx_buffer = TcpSocketBuffer::new(vec![0; pool.tx_buf_bytes]);
            let mut sock = TcpSocket::new(rx_buffer, tx_buffer);
            apply_tcp_opts(&mut sock, pool.nodelay, pool.keepalive, pool.keepidle_secs);
            if sock.listen(pool.endpoint).is_err() {
                return Err(SyscallError::new(syscall::EINVAL));
            }
            let h = sockets.add(sock);
            scheme_data.port_set.acquire_port(pool.endpoint.port);
            pool.extra_handles.push(h);
        }
        while pool.extra_handles.len() > desired_extra {
            let Some(pos) = pool
                .extra_handles
                .iter()
                .rposition(|&h| sockets.get::<TcpSocket>(h).state() == TcpState::Listen)
            else {
                break;
            };
            let h = pool.extra_handles.remove(pos);
            sockets.get_mut::<TcpSocket>(h).close();
            sockets.remove(h);
            scheme_data.port_set.release_port(pool.endpoint.port);
        }
        assert!(1 + pool.extra_handles.len() <= MAX_LISTEN_BACKLOG);
        Ok(0)
    }

    fn set_sock_opt(
        &mut self,
        file: &mut SocketFile<Self::DataT>,
        level: usize,
        name: usize,
        buf: &[u8],
    ) -> SyscallResult<usize> {
        if buf.len() < size_of::<i32>() {
            return Err(SyscallError::new(syscall::EINVAL));
        }
        let val = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
        match (level, name) {
            (SOL_SOCKET, SO_KEEPALIVE) => {
                let on = val != 0;
                if on {
                    self.set_keep_alive(Some(SmolDuration::from_secs(u64::from(
                        file.keepidle_secs,
                    ))));
                } else {
                    self.set_keep_alive(None);
                }
                if let TcpRole::Listening(ref mut pool) = file.data {
                    pool.keepalive = on;
                }
                Ok(0)
            }
            (IPPROTO_TCP, TCP_NODELAY) => {
                let nodelay = val != 0;
                self.set_nagle_enabled(!nodelay);
                if let TcpRole::Listening(ref mut pool) = file.data {
                    pool.nodelay = nodelay;
                }
                Ok(0)
            }
            (IPPROTO_TCP, TCP_KEEPIDLE) => {
                if !(MIN_TCP_KEEPIDLE_SECS..=MAX_TCP_KEEPIDLE_SECS).contains(&val) {
                    return Err(SyscallError::new(syscall::EINVAL));
                }
                file.keepidle_secs = val as u32;
                if self.keep_alive().is_some() {
                    self.set_keep_alive(Some(SmolDuration::from_secs(val as u64)));
                }
                if let TcpRole::Listening(ref mut pool) = file.data {
                    pool.keepidle_secs = val as u32;
                }
                Ok(0)
            }
            _ => Err(SyscallError::new(syscall::ENOPROTOOPT)),
        }
    }

    fn get_sock_opt(
        &self,
        file: &SchemeFile<Self>,
        level: usize,
        name: usize,
        buf: &mut [u8],
    ) -> SyscallResult<usize> {
        let val = match (level, name) {
            (SOL_SOCKET, SO_RCVBUF) => self.recv_capacity() as i32,
            (SOL_SOCKET, SO_SNDBUF) => self.send_capacity() as i32,
            (SOL_SOCKET, SO_KEEPALIVE) => i32::from(self.keep_alive().is_some()),
            (IPPROTO_TCP, TCP_NODELAY) => i32::from(!self.nagle_enabled()),
            (IPPROTO_TCP, TCP_KEEPIDLE) => match *file {
                SchemeFile::Socket(ref sf) => sf.keepidle_secs as i32,
                _ => return Err(SyscallError::new(syscall::EBADF)),
            },
            _ => return Err(SyscallError::new(syscall::ENOPROTOOPT)),
        };
        let bytes = val.to_ne_bytes();
        if buf.len() < bytes.len() {
            return Err(SyscallError::new(syscall::EINVAL));
        }
        buf[0..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }
}

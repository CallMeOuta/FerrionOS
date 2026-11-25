use std::str;

use libredox::flag;
use scheme_utils::FpathWriter;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::tcp::{
    Socket as TcpSocket, SocketBuffer as TcpSocketBuffer, State as TcpState,
};
use smoltcp::wire::{IpEndpoint, IpListenEndpoint};
use syscall::{self, Error as SyscallError, Result as SyscallResult};

use super::socket::{Context, DupResult, SchemeFile, SchemeSocket, SocketFile, SocketOpts};
use super::{parse_endpoint, recvmsg, SchemeWrapper, SocketSet};
use crate::port_set::PortSet;

const SO_SNDBUF: usize = 7;
const SO_RCVBUF: usize = 8;
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

#[derive(Clone, Copy)]
pub struct TcpListenData {
    pub endpoint: IpListenEndpoint,
    pub rx_buf_bytes: usize,
    pub tx_buf_bytes: usize,
}

pub type TcpScheme = SchemeWrapper<TcpSocket<'static>>;

impl<'a> SchemeSocket for TcpSocket<'a> {
    type SchemeDataT = TcpSchemeData;
    type DataT = Option<TcpListenData>;
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
        if data.is_some() {
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
        } else if !port_set.port_set.claim_port(local_endpoint.port) {
            return Err(SyscallError::new(syscall::EADDRINUSE));
        }

        let socket_handle = socket_set.add(socket);

        let tcp_socket = socket_set.get_mut::<TcpSocket>(socket_handle);

        let listen_enpoint = if remote_endpoint.is_specified() {
            let local_endpoint_addr = match local_endpoint.addr {
                Some(addr) if !addr.is_unspecified() => Some(addr),
                _ => {
                    // local ip is 0.0.0.0, resolve it
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
            tcp_socket
                .connect(
                    context.iface.borrow_mut().context(),
                    IpEndpoint::new(remote_endpoint.addr.unwrap(), remote_endpoint.port),
                    local_endpoint,
                )
                .expect("Can't connect tcp socket ");
            None
        } else {
            trace!("Listening tcp {}", local_endpoint);
            tcp_socket
                .listen(local_endpoint)
                .expect("Can't listen on local endpoint");
            Some(TcpListenData {
                endpoint: local_endpoint,
                rx_buf_bytes,
                tx_buf_bytes,
            })
        };

        Ok((socket_handle, listen_enpoint))
    }

    fn close_file(
        &self,
        file: &SchemeFile<Self>,
        scheme_data: &mut Self::SchemeDataT,
    ) -> SyscallResult<()> {
        if let SchemeFile::Socket(SocketFile { data, .. }) = *file {
            if let Some(endpoint) = self.local_endpoint() {
                scheme_data.port_set.release_port(endpoint.port);
            } else if let Some(listen_data) = data {
                scheme_data.port_set.release_port(listen_data.endpoint.port);
            }
        }
        Ok(())
    }

    fn on_last_close(
        sockets: &mut SocketSet,
        handle: SocketHandle,
        _file: &SchemeFile<Self>,
        scheme_data: &mut Self::SchemeDataT,
        now_us: i64,
    ) -> bool {
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
        } else if !self.is_active() {
            Err(SyscallError::new(syscall::ENOTCONN))
        } else if self.can_send() {
            Ok(self.send_slice(buf).expect("Can't send slice"))
        } else if file.flags & syscall::O_NONBLOCK == syscall::O_NONBLOCK {
            Err(SyscallError::new(syscall::EAGAIN))
        } else {
            Err(SyscallError::new(syscall::EWOULDBLOCK)) // internally scheduled to re-write
        }
    }

    fn read_buf(
        &mut self,
        file: &mut SocketFile<Self::DataT>,
        buf: &mut [u8],
    ) -> SyscallResult<usize> {
        if !file.read_enabled {
            Ok(0)
        } else if !self.is_active() {
            Err(SyscallError::new(syscall::ENOTCONN))
        } else if self.can_recv(&file.data) {
            let length = self.recv_slice(buf).expect("Can't receive slice");
            Ok(length)
        } else if !self.may_recv() {
            Ok(0)
        } else if file.flags & syscall::O_NONBLOCK == syscall::O_NONBLOCK {
            Err(SyscallError::new(syscall::EAGAIN))
        } else {
            Err(SyscallError::new(syscall::EWOULDBLOCK)) // internally scheduled to re-read
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
                if let SchemeFile::Socket(ref tcp_handle) = *file {
                    let Some(listen_data) = tcp_handle.data else {
                        return Err(SyscallError::new(syscall::EINVAL));
                    };

                    if !is_active {
                        if tcp_handle.flags & syscall::O_NONBLOCK == syscall::O_NONBLOCK {
                            return Err(SyscallError::new(syscall::EAGAIN));
                        } else {
                            return Ok(None);
                        }
                    }
                    trace!("TCP creating new listening socket");
                    let new_handle = SchemeFile::Socket(tcp_handle.clone_with_data(None));

                    let rx_packets = vec![0; listen_data.rx_buf_bytes];
                    let tx_packets = vec![0; listen_data.tx_buf_bytes];
                    let rx_buffer = TcpSocketBuffer::new(rx_packets);
                    let tx_buffer = TcpSocketBuffer::new(tx_packets);
                    let socket = TcpSocket::new(rx_buffer, tx_buffer);
                    let new_socket_handle = socket_set.add(socket);
                    {
                        let tcp_socket = socket_set.get_mut::<TcpSocket>(new_socket_handle);
                        tcp_socket
                            .listen(listen_data.endpoint)
                            .expect("Can't listen on local endpoint");
                    }
                    port_set.port_set.acquire_port(
                        local_endpoint
                            .expect("Socket was active so local endpoint must be set")
                            .port,
                    );
                    return Ok(Some((
                        new_handle,
                        Some((new_socket_handle, Some(listen_data))),
                    )));
                } else {
                    return Err(SyscallError::new(syscall::EBADF));
                }
            }
            _ => {
                trace!("TCP dup unknown {}", path);
                if let SchemeFile::Socket(ref tcp_handle) = *file {
                    SchemeFile::Socket(tcp_handle.clone_with_data(tcp_handle.data))
                } else {
                    SchemeFile::Socket(SocketFile::new_with_data(socket_handle, None))
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
                        data: Some(listen_data),
                        ..
                    }),
                ) => {
                    let endpoint = listen_data.endpoint;
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

    fn get_sock_opt(
        &self,
        _file: &SchemeFile<Self>,
        name: usize,
        buf: &mut [u8],
    ) -> SyscallResult<usize> {
        match name {
            SO_RCVBUF => {
                let val = self.recv_capacity() as i32;
                let bytes = val.to_ne_bytes();

                if buf.len() < bytes.len() {
                    return Err(SyscallError::new(syscall::EINVAL));
                }

                buf[0..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }
            SO_SNDBUF => {
                let val = self.send_capacity() as i32;
                let bytes = val.to_ne_bytes();

                if buf.len() < bytes.len() {
                    return Err(SyscallError::new(syscall::EINVAL));
                }

                buf[0..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }
            _ => Err(SyscallError::new(syscall::ENOPROTOOPT)),
        }
    }
}

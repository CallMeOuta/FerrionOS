use std::cell::RefCell;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::rc::Rc;
use std::str::FromStr;

use libredox::{flag, Fd};
use redox_scheme::scheme::{IntoTag, Op, SchemeResponse, SchemeState, SchemeSync};
use redox_scheme::{CallerCtx, RequestKind, Response, SignalBehavior, Socket};
#[expect(clippy::single_component_path_imports)]
use smoltcp;
use smoltcp::iface::{Config, Interface as SmoltcpInterface};
use smoltcp::phy::Tracer;
use smoltcp::socket::AnySocket;
use smoltcp::time::{Duration, Instant};
use smoltcp::wire::{
    EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpListenEndpoint, Ipv4Address,
};
use syscall::data::TimeSpec;
use syscall::{self, Error as SyscallError};

use self::icmp::IcmpScheme;
use self::ip::IpScheme;
use self::netcfg::NetCfgScheme;
use self::tcp::TcpScheme;
use self::udp::UdpScheme;
use crate::error::{Error, Result};
use crate::link::ethernet::EthernetLink;
use crate::link::loopback::LoopbackDevice;
use crate::link::{DeviceList, LinkDevice};
use crate::router::route_table::{RouteTable, Rule};
use crate::router::Router;
use crate::scheme::smoltcp::iface::SocketSet as SmoltcpSocketSet;
use crate::scheme::socket::{Handle, SchemeSocket, SocketScheme};

mod icmp;
mod ip;
mod netcfg;
mod recvmsg;
mod socket;
mod tcp;
mod udp;

type SocketSet = SmoltcpSocketSet<'static>;
type Interface = Rc<RefCell<SmoltcpInterface>>;

const MAX_DURATION: Duration = Duration::from_micros(u64::MAX);
const MIN_DURATION: Duration = Duration::from_micros(0);
const MAX_SCHEME_REQUESTS_PER_BATCH: usize = 64;
const MAX_SCHEME_BATCHES_PER_EVENT: usize = 16;
const MAX_PENDING_TIMERS: usize = 32;

fn getcfg(key: &str) -> Result<String> {
    let mut value = String::new();
    let mut file = File::open(format!("/etc/net/{key}"))?;
    file.read_to_string(&mut value)?;
    Ok(value.trim().to_string())
}

pub struct Smolnetd {
    router_device: Tracer<Router>,
    iface: Interface,
    time_file: File,

    socket_set: Rc<RefCell<SocketSet>>,
    timer: ::std::time::Instant,
    pending_scheme_events: bool,
    pending_timers: Vec<u128>,

    ip_scheme: IpScheme,
    udp_scheme: UdpScheme,
    tcp_scheme: TcpScheme,
    icmp_scheme: IcmpScheme,
    netcfg_scheme: NetCfgScheme,
}

impl Smolnetd {
    pub const SOCKET_BUFFER_SIZE: usize = 128; //packets
    pub const MIN_CHECK_TIMEOUT: Duration = Duration::from_millis(10);
    pub const MAX_CHECK_TIMEOUT: Duration = Duration::from_millis(500);

    #[expect(clippy::too_many_arguments, reason = "all needed")]
    pub fn new(
        network_file: Fd,
        hardware_addr: EthernetAddress,
        ip_file: Socket,
        udp_file: Socket,
        tcp_file: Socket,
        icmp_file: Socket,
        time_file: Fd,
        netcfg_file: Socket,
    ) -> Result<Smolnetd> {
        let protocol_addrs = vec![
            //This is a placeholder IP for DHCP
            IpCidr::new(IpAddress::v4(0, 0, 0, 0), 8),
            IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8),
        ];

        let default_gw = Ipv4Address::from_str(getcfg("ip_router").unwrap().trim())
            .expect("Can't parse the 'ip_router' cfg.");

        let devices = Rc::new(RefCell::new(DeviceList::default()));
        let route_table = Rc::new(RefCell::new(RouteTable::default()));
        let mut network_device = Tracer::new(
            Router::new(Rc::clone(&devices), Rc::clone(&route_table)),
            |_timestamp, printer| trace!("{}", printer),
        );

        let mut config = Config::new(HardwareAddress::Ip);
        // The engine's own random numbers start from this seed; its default is 0.
        config.random_seed = crate::random::u64()
            .map_err(|err| Error::from_io_error(err, "failed to read /scheme/rand"))?;
        let timer = ::std::time::Instant::now();
        let mut iface = SmoltcpInterface::new(config, &mut network_device, Instant::ZERO);
        // TCP initial sequence numbers follow RFC 6528 (isn.rs) rather than the engine's
        // random number generator.
        iface.set_tcp_isn_generator(Some(crate::isn::initial_seq_no));
        iface.update_ip_addrs(|ip_addrs| ip_addrs.extend(protocol_addrs));
        iface
            .routes_mut()
            .add_default_ipv4_route(default_gw)
            .expect("Failed to add default gateway");

        let iface = Rc::new(RefCell::new(iface));
        let socket_set = Rc::new(RefCell::new(SocketSet::new(vec![])));

        let loopback = LoopbackDevice::default();
        route_table.borrow_mut().insert_rule(Rule::new(
            "127.0.0.0/8".parse().unwrap(),
            None,
            Rc::clone(loopback.name()),
            "127.0.0.1".parse().unwrap(),
        ));

        let mut eth0 = EthernetLink::new("eth0", unsafe {
            File::from_raw_fd(network_file.into_raw() as RawFd)
        });
        eth0.set_mac_address(hardware_addr);

        devices.borrow_mut().push(loopback);
        devices.borrow_mut().push(eth0);

        Ok(Smolnetd {
            iface: Rc::clone(&iface),
            router_device: network_device,
            socket_set: Rc::clone(&socket_set),
            timer,
            pending_scheme_events: false,
            pending_timers: Vec::with_capacity(MAX_PENDING_TIMERS),
            time_file: unsafe { File::from_raw_fd(time_file.into_raw() as RawFd) },
            ip_scheme: IpScheme::new(
                "ip",
                Rc::clone(&iface),
                Rc::clone(&route_table),
                Rc::clone(&socket_set),
                ip_file,
            )?,
            udp_scheme: UdpScheme::new(
                "udp",
                Rc::clone(&iface),
                Rc::clone(&route_table),
                Rc::clone(&socket_set),
                udp_file,
            )?,
            tcp_scheme: TcpScheme::new(
                "tcp",
                Rc::clone(&iface),
                Rc::clone(&route_table),
                Rc::clone(&socket_set),
                tcp_file,
            )?,
            icmp_scheme: IcmpScheme::new(
                "icmp",
                Rc::clone(&iface),
                Rc::clone(&route_table),
                Rc::clone(&socket_set),
                icmp_file,
            )?,
            netcfg_scheme: NetCfgScheme::new(
                Rc::clone(&iface),
                netcfg_file,
                Rc::clone(&route_table),
                Rc::clone(&devices),
            )?,
        })
    }

    pub fn on_network_scheme_event(&mut self) -> Result<()> {
        self.poll()?;
        Ok(())
    }

    fn run_scheme_batches<F>(&mut self, mut step: F) -> Result<()>
    where
        F: FnMut(&mut Self) -> Result<bool>,
    {
        for _ in 0..MAX_SCHEME_BATCHES_PER_EVENT {
            let more = step(self)?;
            let _ = self.poll()?;
            if !more {
                return Ok(());
            }
        }
        self.pending_scheme_events = true;
        self.arm_timer(Duration::ZERO)
    }

    pub fn on_ip_scheme_event(&mut self) -> Result<()> {
        self.run_scheme_batches(|this| this.ip_scheme.on_scheme_event())
    }

    pub fn on_udp_scheme_event(&mut self) -> Result<()> {
        self.run_scheme_batches(|this| this.udp_scheme.on_scheme_event())
    }

    pub fn on_tcp_scheme_event(&mut self) -> Result<()> {
        self.run_scheme_batches(|this| {
            this.iface.borrow_mut().poll_maintenance(this.now());
            this.tcp_scheme.on_scheme_event()
        })
    }

    pub fn on_icmp_scheme_event(&mut self) -> Result<()> {
        self.run_scheme_batches(|this| this.icmp_scheme.on_scheme_event())
    }

    pub fn on_time_event(&mut self) -> Result<()> {
        let now_ns = self.read_time_ns()?;
        self.pending_timers.retain(|&deadline| deadline > now_ns);
        if self.pending_scheme_events {
            self.pending_scheme_events = false;
            let mut more = false;
            more |= self.ip_scheme.on_scheme_event()?;
            more |= self.udp_scheme.on_scheme_event()?;
            self.iface.borrow_mut().poll_maintenance(self.now());
            more |= self.tcp_scheme.on_scheme_event()?;
            more |= self.icmp_scheme.on_scheme_event()?;
            if more {
                self.pending_scheme_events = true;
            }
        }
        let timeout = self.poll()?;
        let next = if self.pending_scheme_events {
            Duration::ZERO
        } else {
            timeout
        };
        self.arm_timer(next)?;
        //TODO: Fix network scheme to ensure events are not missed
        self.on_network_scheme_event()
    }

    pub fn on_netcfg_scheme_event(&mut self) -> Result<()> {
        self.netcfg_scheme.on_scheme_event()?;
        Ok(())
    }

    fn read_time_ns(&mut self) -> Result<u128> {
        let mut time = TimeSpec::default();
        if self.time_file.read(&mut time)? < size_of::<TimeSpec>() {
            return Err(Error::from_syscall_error(
                syscall::Error::new(syscall::EBADF),
                "Can't read current time",
            ));
        }
        let secs = u128::try_from(time.tv_sec.max(0)).unwrap_or(0);
        let nsecs = u128::try_from(time.tv_nsec.max(0)).unwrap_or(0);
        Ok(secs.saturating_mul(1_000_000_000).saturating_add(nsecs))
    }

    fn arm_timer(&mut self, timeout: Duration) -> Result<()> {
        let now_ns = self.read_time_ns()?;
        let delta_ns = u128::from(timeout.total_micros()).saturating_mul(1_000);
        let target_ns = now_ns.saturating_add(delta_ns);
        if self
            .pending_timers
            .first()
            .is_some_and(|&earliest| earliest <= target_ns)
        {
            return Ok(());
        }
        if self.pending_timers.len() >= MAX_PENDING_TIMERS {
            return Ok(());
        }
        let time = TimeSpec {
            tv_sec: i64::try_from(target_ns / 1_000_000_000).unwrap_or(i64::MAX),
            tv_nsec: (target_ns % 1_000_000_000) as i32,
        };
        self.time_file
            .write_all(&time)
            .map_err(|e| Error::from_io_error(e, "Failed to write to time file"))?;
        let pos = self.pending_timers.partition_point(|&d| d < target_ns);
        self.pending_timers.insert(pos, target_ns);
        assert!(self.pending_timers.len() <= MAX_PENDING_TIMERS);
        Ok(())
    }

    fn now(&self) -> Instant {
        let micros = i64::try_from(self.timer.elapsed().as_micros()).unwrap_or(i64::MAX);
        Instant::from_micros(micros)
    }

    fn poll(&mut self) -> Result<Duration> {
        let timeout = {
            let mut iter_limit = 10usize;
            let mut iface = self.iface.borrow_mut();
            let mut socket_set = self.socket_set.borrow_mut();

            loop {
                let timestamp = self.now();
                if iter_limit == 0 {
                    break MIN_DURATION;
                }
                iter_limit -= 1;

                self.router_device.get_mut().poll(timestamp);

                // TODO: Check what if the bool returned by poll can be useful
                iface.poll(timestamp, &mut self.router_device, &mut socket_set);

                self.router_device.get_mut().dispatch(timestamp);

                if !self.router_device.get_ref().can_recv() {
                    match iface.poll_delay(timestamp, &socket_set) {
                        Some(delay) if delay == Duration::ZERO => {}
                        Some(delay) => break ::std::cmp::min(MAX_DURATION, delay),
                        None => break MAX_DURATION,
                    };
                }
            }
        };

        self.notify_sockets()?;

        Ok(::std::cmp::min(
            ::std::cmp::max(Smolnetd::MIN_CHECK_TIMEOUT, timeout),
            Smolnetd::MAX_CHECK_TIMEOUT,
        ))
    }

    fn notify_sockets(&mut self) -> Result<()> {
        self.ip_scheme.notify_sockets()?;
        self.udp_scheme.notify_sockets()?;
        self.tcp_scheme.notify_sockets()?;
        self.icmp_scheme.notify_sockets()
    }
}

fn post_fevent(socket: &Socket, id: usize, flags: usize) -> syscall::error::Result<()> {
    let fevent_response = Response::post_fevent(id, flags);
    match socket.write_response(fevent_response, SignalBehavior::Restart) {
        Ok(true) => Ok(()), // Write response success
        Ok(false) => Err(syscall::error::Error::new(syscall::EAGAIN)), // Write response failed, retry.
        Err(err) => Err(err),                                          // Error writing response
    }
}

fn parse_endpoint(socket: &str) -> IpListenEndpoint {
    let mut socket_parts = socket.split(':');
    let host = Ipv4Address::from_str(socket_parts.next().unwrap_or(""))
        .ok()
        .filter(|addr| !addr.is_unspecified())
        .map(IpAddress::Ipv4);

    let port = socket_parts
        .next()
        .unwrap_or("")
        .parse::<u16>()
        .unwrap_or(0);
    IpListenEndpoint { addr: host, port }
}

struct WaitHandle {
    until: Option<TimeSpec>,
    cancelling: bool,
    packet: (Op, CallerCtx),
}

type WaitQueue = Vec<WaitHandle>;

pub struct SchemeWrapper<SocketT>
where
    SocketT: SchemeSocket + AnySocket<'static>,
{
    scheme: socket::SocketScheme<SocketT>,
    state: SchemeState,
    wait_queue: WaitQueue,
}
impl<SocketT> SchemeWrapper<SocketT>
where
    SocketT: SchemeSocket + AnySocket<'static>,
{
    pub fn new(
        name: &str,
        iface: Interface,
        route_table: Rc<RefCell<RouteTable>>,
        socket_set: Rc<RefCell<SocketSet>>,
        scheme_file: Socket,
    ) -> Result<Self> {
        Ok(Self {
            scheme: SocketScheme::<SocketT>::new(name, iface, route_table, socket_set, scheme_file)
                .map_err(|e| {
                    Error::from_syscall_error(e, format!("failed to initialize {name} scheme"))
                })?,
            state: SchemeState::new(),
            wait_queue: Vec::new(),
        })
    }
    pub fn on_scheme_event(&mut self) -> Result<bool> {
        for _ in 0..MAX_SCHEME_REQUESTS_PER_BATCH {
            let request = match self
                .scheme
                .scheme_file
                .next_request(SignalBehavior::Restart)
            {
                Ok(Some(req)) => req,
                Ok(None) => return Ok(false),
                Err(error)
                    if error.errno == syscall::EWOULDBLOCK || error.errno == syscall::EAGAIN =>
                {
                    return Ok(false);
                }
                Err(other) => {
                    return Err(Error::from_syscall_error(
                        other,
                        "failed to receive new request",
                    ))
                }
            };

            let req = match request.kind() {
                RequestKind::Call(c) => c,
                RequestKind::SendFd(req) => {
                    let resp = Response::err(syscall::EOPNOTSUPP, req);
                    self.scheme
                        .scheme_file
                        .write_response(resp, SignalBehavior::Restart)
                        .map_err(|e| Error::from_syscall_error(e, "failed to write response"))?;
                    continue;
                }
                RequestKind::RecvFd(req) => {
                    let resp = Response::err(syscall::EOPNOTSUPP, req);
                    self.scheme
                        .scheme_file
                        .write_response(resp, SignalBehavior::Restart)
                        .map_err(|e| Error::from_syscall_error(e, "failed to write response"))?;
                    continue;
                }
                RequestKind::OnClose { id } => {
                    self.scheme.on_close(id);
                    continue;
                }
                RequestKind::Cancellation(req) => {
                    if let Some(idx) = self
                        .wait_queue
                        .iter()
                        .position(|q| q.packet.0.req_id() == req.id)
                    {
                        self.wait_queue[idx].cancelling = true;
                    }
                    continue;
                }
                RequestKind::MsyncMsg | RequestKind::MunmapMsg | RequestKind::MmapMsg => {
                    unreachable!()
                }
                RequestKind::OnDetach { .. } => {
                    continue;
                }
            };
            let caller = req.caller();
            let mut op = match req.op() {
                Ok(op) => op,
                Err(req) => {
                    self.scheme
                        .scheme_file
                        .write_response(
                            Response::err(syscall::EOPNOTSUPP, req),
                            SignalBehavior::Restart,
                        )
                        .map_err(|e| Error::from_syscall_error(e, "failed to write response"))?;
                    continue;
                }
            };
            let resp = match op.handle_sync_dont_consume(&caller, &mut self.scheme, &mut self.state)
            {
                SchemeResponse::Opened(Err(SyscallError {
                    errno: syscall::EWOULDBLOCK,
                }))
                | SchemeResponse::Regular(Err(SyscallError {
                    errno: syscall::EWOULDBLOCK,
                })) if !op.is_explicitly_nonblock() => {
                    match self.scheme.handle_block(&op) {
                        Ok(timeout) => {
                            self.wait_queue.push(WaitHandle {
                                until: timeout,
                                cancelling: false,
                                packet: (op, caller),
                            });
                        }
                        Err(err) => {
                            let _ = self
                                .scheme
                                .scheme_file
                                .write_response(
                                    Response::err(err.errno, op),
                                    SignalBehavior::Restart,
                                )
                                .map_err(|e| {
                                    Error::from_syscall_error(e, "failed to write response")
                                })?;
                            return Err(Error::from_syscall_error(
                                err,
                                "Can't handle blocked socket",
                            ));
                        }
                    }
                    continue;
                }
                SchemeResponse::Regular(r) => Response::new(r, op),
                SchemeResponse::Opened(o) => Response::open_dup_like(o, op),
                SchemeResponse::RegularAndNotifyOnDetach(status) => {
                    Response::new_notify_on_detach(status, op)
                }
            };
            let _ = self
                .scheme
                .scheme_file
                .write_response(resp, SignalBehavior::Restart)
                .map_err(|e| Error::from_syscall_error(e, "failed to write response"))?;
        }
        Ok(true)
    }

    #[expect(clippy::useless_conversion, reason = "cur_time.tv_nsec is i32 on i586")]
    pub fn notify_sockets(&mut self) -> Result<()> {
        let cur_time = libredox::call::clock_gettime(flag::CLOCK_MONOTONIC)
            .map_err(|e| Error::from_syscall_error(e.into(), "Can't get time"))?;

        // Notify non-blocking sockets
        let scheme = &mut self.scheme;
        let state = &mut self.state;

        for (&fd, handle) in scheme.handles.iter_mut() {
            let Handle::File(file) = handle else {
                continue;
            };
            let events = {
                let mut socket_set = scheme.socket_set.borrow_mut();
                file.events(&mut socket_set)
            };
            if events > 0 {
                post_fevent(&scheme.scheme_file, fd, events)
                    .map_err(|e| Error::from_syscall_error(e, "failed to post fevent"))?;
            }
        }
        // Wake up blocking queue
        let queue = &mut self.wait_queue;
        let mut i = 0;
        while i < queue.len() {
            let handle = &mut queue[i];
            let (op, caller) = &mut handle.packet;
            let res = op.handle_sync_dont_consume(caller, scheme, state);

            match res {
                SchemeResponse::Opened(Err(SyscallError {
                    errno: syscall::EWOULDBLOCK,
                }))
                | SchemeResponse::Regular(Err(SyscallError {
                    errno: syscall::EWOULDBLOCK,
                })) if !op.is_explicitly_nonblock() => {
                    if handle.cancelling {
                        let (op, _) = queue.swap_remove(i).packet;
                        scheme
                            .scheme_file
                            .write_response(
                                Response::err(syscall::ECANCELED, op),
                                SignalBehavior::Restart,
                            )
                            .map_err(|e| {
                                Error::from_syscall_error(e, "failed to write response")
                            })?;
                        continue;
                    }
                    match handle.until {
                        Some(until)
                            if (until.tv_sec < cur_time.tv_sec
                                || (until.tv_sec == cur_time.tv_sec
                                    && i64::from(until.tv_nsec) < cur_time.tv_nsec.into())) =>
                        {
                            let (op, _) = queue.swap_remove(i).packet;
                            let _ = scheme
                                .scheme_file
                                .write_response(
                                    Response::err(syscall::ETIMEDOUT, op),
                                    SignalBehavior::Restart,
                                )
                                .map_err(|e| {
                                    Error::from_syscall_error(e, "failed to write response")
                                })?;
                        }
                        _ => {
                            i += 1;
                        }
                    }
                }
                SchemeResponse::Regular(r) => {
                    let (op, _) = queue.swap_remove(i).packet;
                    let _ = scheme
                        .scheme_file
                        .write_response(Response::new(r, op), SignalBehavior::Restart)
                        .map_err(|e| Error::from_syscall_error(e, "failed to write response"))?;
                }
                SchemeResponse::Opened(o) => {
                    let (op, _) = queue.swap_remove(i).packet;
                    let _ = scheme
                        .scheme_file
                        .write_response(Response::open_dup_like(o, op), SignalBehavior::Restart)
                        .map_err(|e| Error::from_syscall_error(e, "failed to write response"))?;
                }
                SchemeResponse::RegularAndNotifyOnDetach(status) => {
                    let (op, _) = queue.swap_remove(i).packet;
                    let _ = scheme
                        .scheme_file
                        .write_response(
                            Response::new_notify_on_detach(status, op),
                            SignalBehavior::Restart,
                        )
                        .map_err(|e| Error::from_syscall_error(e, "failed to write response"))?;
                }
            }
        }

        Ok(())
    }
}

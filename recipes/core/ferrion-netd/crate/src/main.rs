mod config;
mod dhcp;
mod scheme;

use std::cell::RefCell;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::mem::size_of;
use std::net::Ipv4Addr;
use std::os::fd::AsRawFd;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use daemon::Daemon;
use ioslice::IoSlice;
use libredox::errno::{EAGAIN, EINTR, EWOULDBLOCK};
use libredox::flag::{O_CLOEXEC, O_NONBLOCK, O_RDWR};
use libredox::Fd;
use redox_scheme::scheme::{register_scheme_inner, SchemeSync};
use redox_scheme::Socket;
use scheme_utils::ReadinessBased;
use serde_json::json;
use syscall::data::TimeSpec;
use syscall::{Event, EventFlags, CLOCK_MONOTONIC, EVENT_READ};

use crate::config::{load_config, LinkMode, StaticConfig, FALLBACK_DNS, MAX_MTU};
use crate::dhcp::{
    build_decline, build_discover, build_release, build_request_rebinding, build_request_renewing,
    build_request_selecting, compute_timers, parse_reply, DhcpReply, RouteEntry, DHCPACK, DHCPNAK,
    DHCPOFFER, MAX_PACKET_LEN,
};
use crate::scheme::{CtlCommand, NetCtlScheme, NetStatusScheme, StatusSnapshot};

const MAX_REQ_BATCH: usize = 32;
const MAX_UDP_DRAIN: usize = 32;
const MAX_ARP_POLLS: u32 = 24;
const ARP_POLL_MS: u64 = 50;
const LINK_POLL_INTERVAL: Duration = Duration::from_secs(1);
const DECLINE_BACKOFF: Duration = Duration::from_secs(10);
const RELEASE_FLUSH_MS: u64 = 120;
const MIN_RENEW_RETRY_SECS: u64 = 60;
const MAX_BACKOFF_SECS: u64 = 64;
const MAX_REQUEST_TRIES: u32 = 4;
const MAX_MAC_WAIT_ATTEMPTS: usize = 20;

static TERMINATE: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigterm(_: usize) {
    TERMINATE.store(true, Ordering::SeqCst);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinkCarrier {
    Up,
    Down,
    Unknown,
}

impl LinkCarrier {
    fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug)]
struct Lease {
    ip: Ipv4Addr,
    prefix_len: u8,
    server_id: Ipv4Addr,
    routes: Vec<RouteEntry>,
    dns: Vec<Ipv4Addr>,
    mtu: u16,
    lease_secs: u64,
    t1_secs: u64,
    t2_secs: u64,
    acquired_at: Instant,
    t1_at: Instant,
    t2_at: Instant,
    expires_at: Instant,
}

#[derive(Clone, Debug)]
struct PendingOffer {
    ip: Ipv4Addr,
    server_id: Ipv4Addr,
}

#[derive(Clone, Debug)]
enum State {
    Down,
    Static(StaticConfig),
    CarrierDown,
    Init {
        next_attempt: Instant,
    },
    Selecting {
        xid: u32,
        tries: u32,
        next_send: Instant,
    },
    Requesting {
        xid: u32,
        offer: PendingOffer,
        tries: u32,
        next_send: Instant,
    },
    Probing {
        lease: Lease,
        next_poll: Instant,
        polls: u32,
    },
    Bound {
        lease: Lease,
    },
    Renewing {
        lease: Lease,
        xid: u32,
        next_send: Instant,
    },
    Rebinding {
        lease: Lease,
        xid: u32,
        next_send: Instant,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UdpTarget {
    Broadcast,
    Unicast(Ipv4Addr),
}

struct Engine {
    mac: Option<[u8; 6]>,
    mode: LinkMode,
    admin_down: bool,
    carrier: LinkCarrier,
    state: State,
    udp: Option<(Fd, UdpTarget)>,
    event_fd: Option<usize>,
    last_link_poll: Instant,
    snapshot: Rc<RefCell<StatusSnapshot>>,
}

fn rand_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    let mut f = match File::open("/scheme/rand") {
        Ok(f) => f,
        Err(err) => {
            eprintln!("ferrion-netd: open /scheme/rand failed: {err}");
            std::process::exit(1);
        }
    };
    if let Err(err) = f.read_exact(&mut out) {
        eprintln!("ferrion-netd: read /scheme/rand failed: {err}");
        std::process::exit(1);
    }
    out
}

fn rand_xid() -> u32 {
    let v = u32::from_ne_bytes(rand_bytes::<4>());
    if v == 0 {
        1
    } else {
        v
    }
}

fn jitter_ms() -> i64 {
    let raw = u16::from_ne_bytes(rand_bytes::<2>());
    (i64::from(raw) % 2001) - 1000
}

fn backoff_delay(tries: u32) -> Duration {
    let shift = tries.saturating_sub(1).min(4);
    let base_secs = (4u64 << shift).min(MAX_BACKOFF_SECS);
    let ms = (base_secs as i64)
        .saturating_mul(1000)
        .saturating_add(jitter_ms())
        .max(1000);
    Duration::from_millis(ms as u64)
}

fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let mut out = [0u8; 6];
    let mut parts = text.trim().split([':', '-']);
    for slot in &mut out {
        let part = parts.next()?;
        *slot = u8::from_str_radix(part, 16).ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(out)
}

fn read_mac() -> Option<[u8; 6]> {
    for _ in 0..MAX_MAC_WAIT_ATTEMPTS {
        match fs::read_to_string("/scheme/netcfg/ifaces/eth0/mac") {
            Ok(text) => {
                if let Some(mac) = parse_mac(&text) {
                    return Some(mac);
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
            Err(_) => {}
        }
        thread::sleep(Duration::from_millis(50));
    }
    None
}

fn read_link_carrier() -> LinkCarrier {
    match fs::read_to_string("/scheme/netcfg/ifaces/eth0/link") {
        Ok(text) => match text.trim() {
            "up" => LinkCarrier::Up,
            "down" => LinkCarrier::Down,
            _ => LinkCarrier::Unknown,
        },
        Err(_) => LinkCarrier::Unknown,
    }
}

fn write_netcfg(path: &str, data: &str) -> io::Result<()> {
    let mut f = File::create(path)?;
    f.write_all(data.as_bytes())?;
    f.sync_all().or(Ok(()))
}

fn log_netcfg(path: &str, data: &str) -> bool {
    match write_netcfg(path, data) {
        Ok(()) => true,
        Err(err) => {
            eprintln!("ferrion-netd: netcfg write {path} failed: {err}");
            false
        }
    }
}

fn write_dns_files(dns: &[Ipv4Addr]) {
    let primary = dns.first().copied().unwrap_or(FALLBACK_DNS);
    let _ = log_netcfg("/scheme/netcfg/resolv/nameserver", &format!("{primary}\n"));
    let mut content = String::new();
    if dns.is_empty() {
        content.push_str(&format!("{FALLBACK_DNS}\n"));
    } else {
        for ip in dns {
            content.push_str(&format!("{ip}\n"));
        }
    }
    let tmp = "/etc/net/dns.tmp";
    if fs::write(tmp, content.as_bytes()).is_ok() {
        if let Err(err) = fs::rename(tmp, "/etc/net/dns") {
            eprintln!("ferrion-netd: rename /etc/net/dns failed: {err}");
        }
    } else if let Err(err) = fs::write("/etc/net/dns", content.as_bytes()) {
        eprintln!("ferrion-netd: write /etc/net/dns failed: {err}");
    }
}

fn clear_netcfg_addr() {
    let _ = log_netcfg("/scheme/netcfg/ifaces/eth0/addr/clear", "1\n");
    let _ = log_netcfg("/scheme/netcfg/ifaces/eth0/mtu", &format!("{MAX_MTU}\n"));
    write_dns_files(&[]);
}

fn apply_netcfg(
    ip: Ipv4Addr,
    prefix_len: u8,
    routes: &[RouteEntry],
    dns: &[Ipv4Addr],
    mtu: u16,
) -> bool {
    let _ = log_netcfg("/scheme/netcfg/ifaces/eth0/addr/clear", "1\n");
    let _ = log_netcfg("/scheme/netcfg/ifaces/eth0/mtu", &format!("{mtu}\n"));
    if !log_netcfg(
        "/scheme/netcfg/ifaces/eth0/addr/set",
        &format!("{ip}/{prefix_len}\n"),
    ) {
        return false;
    }
    for route in routes {
        let dst = if route.prefix_len == 0 {
            "default".to_owned()
        } else {
            format!("{}/{}", route.network, route.prefix_len)
        };
        let line = match route.via {
            Some(gw) => format!("{dst} via {gw}\n"),
            None => format!("{dst} dev eth0\n"),
        };
        let _ = log_netcfg("/scheme/netcfg/route/add", &line);
    }
    write_dns_files(dns);
    true
}

fn open_udp(target: UdpTarget) -> Option<Fd> {
    let path = match target {
        UdpTarget::Broadcast => "/scheme/udp/255.255.255.255:67/0.0.0.0:68".to_owned(),
        UdpTarget::Unicast(server) => format!("/scheme/udp/{server}:67/0.0.0.0:68"),
    };
    Fd::open(&path, O_RDWR | O_NONBLOCK | O_CLOEXEC, 0).ok()
}

fn lease_from_reply(reply: &DhcpReply, now: Instant) -> Lease {
    let (lease_secs, t1_secs, t2_secs) =
        compute_timers(reply.lease_secs, reply.t1_secs, reply.t2_secs);
    let acquired_at = now;
    Lease {
        ip: reply.yiaddr,
        prefix_len: reply.prefix_len.unwrap_or(24),
        server_id: reply.server_id,
        routes: reply.routes.clone(),
        dns: reply.dns.clone(),
        mtu: reply.mtu.unwrap_or(MAX_MTU),
        lease_secs,
        t1_secs,
        t2_secs,
        acquired_at,
        t1_at: acquired_at + Duration::from_secs(t1_secs),
        t2_at: acquired_at + Duration::from_secs(t2_secs),
        expires_at: acquired_at + Duration::from_secs(lease_secs),
    }
}

impl Engine {
    fn new(mac: Option<[u8; 6]>, mode: LinkMode, snapshot: Rc<RefCell<StatusSnapshot>>) -> Self {
        let now = Instant::now();
        let carrier = if mac.is_some() {
            read_link_carrier()
        } else {
            LinkCarrier::Down
        };
        let mut engine = Self {
            mac,
            mode: mode.clone(),
            admin_down: mac.is_none() || matches!(mode, LinkMode::Off),
            carrier,
            state: State::Down,
            udp: None,
            event_fd: None,
            last_link_poll: now,
            snapshot,
        };
        if mac.is_some() {
            engine.start_for_mode(now);
        }
        engine.refresh_snapshot(now);
        engine
    }

    fn set_event_fd(&mut self, ev_fd: usize) {
        self.event_fd = Some(ev_fd);
    }

    fn close_udp(&mut self) {
        if let Some((fd, _)) = self.udp.take() {
            if let Some(ev_fd) = self.event_fd {
                if let Err(err) = libredox::call::write(
                    ev_fd,
                    &Event {
                        id: fd.raw(),
                        flags: EventFlags::empty(),
                        data: 4,
                    },
                ) {
                    eprintln!("ferrion-netd: event unregister udp failed: {err}");
                }
            }
            drop(fd);
        }
    }

    fn ensure_udp(&mut self, target: UdpTarget) -> bool {
        if let Some((_, cur)) = &self.udp {
            if *cur == target {
                return true;
            }
        }
        self.close_udp();
        if let Some(fd) = open_udp(target) {
            if let Some(ev_fd) = self.event_fd {
                if let Err(err) = libredox::call::write(
                    ev_fd,
                    &Event {
                        id: fd.raw(),
                        flags: EVENT_READ,
                        data: 4,
                    },
                ) {
                    eprintln!("ferrion-netd: event register udp failed: {err}");
                }
            }
            self.udp = Some((fd, target));
            true
        } else {
            false
        }
    }

    fn send_udp(&mut self, target: UdpTarget, payload: &[u8]) {
        if !self.ensure_udp(target) {
            return;
        }
        if let Some((fd, _)) = &self.udp {
            let _ = fd.write(payload);
        }
    }

    fn start_for_mode(&mut self, now: Instant) {
        self.close_udp();
        if self.mac.is_none() || self.admin_down {
            if self.mac.is_some() {
                clear_netcfg_addr();
            }
            self.state = State::Down;
            return;
        }
        if self.carrier == LinkCarrier::Down {
            clear_netcfg_addr();
            self.state = State::CarrierDown;
            return;
        }
        match self.mode.clone() {
            LinkMode::Off => {
                self.admin_down = true;
                clear_netcfg_addr();
                self.state = State::Down;
            }
            LinkMode::Static(cfg) => {
                let routes = match cfg.gateway {
                    Some(gw) => vec![RouteEntry {
                        network: Ipv4Addr::UNSPECIFIED,
                        prefix_len: 0,
                        via: Some(gw),
                    }],
                    None => Vec::new(),
                };
                if apply_netcfg(cfg.address, cfg.prefix_len, &routes, &cfg.dns, cfg.mtu) {
                    self.state = State::Static(cfg);
                } else {
                    clear_netcfg_addr();
                    self.state = State::Down;
                }
            }
            LinkMode::Dhcp => {
                clear_netcfg_addr();
                self.state = State::Init { next_attempt: now };
            }
        }
    }

    fn active_lease(&self) -> Option<&Lease> {
        match &self.state {
            State::Bound { lease }
            | State::Renewing { lease, .. }
            | State::Rebinding { lease, .. } => Some(lease),
            _ => None,
        }
    }

    fn send_release_if_bound(&mut self) {
        let (Some(mac), Some(lease)) = (self.mac, self.active_lease().cloned()) else {
            return;
        };
        let xid = rand_xid();
        let pkt = build_release(xid, mac, lease.ip, lease.server_id);
        self.send_udp(UdpTarget::Unicast(lease.server_id), &pkt);
        thread::sleep(Duration::from_millis(RELEASE_FLUSH_MS));
        self.close_udp();
    }

    fn handle_ctl(&mut self, cmd: CtlCommand) {
        let now = Instant::now();
        if self.mac.is_none() {
            self.refresh_snapshot(now);
            return;
        }
        match cmd {
            CtlCommand::Down => {
                self.send_release_if_bound();
                self.admin_down = true;
                clear_netcfg_addr();
                self.close_udp();
                self.state = State::Down;
            }
            CtlCommand::Release => {
                self.send_release_if_bound();
                clear_netcfg_addr();
                self.close_udp();
                self.state = State::Down;
            }
            CtlCommand::Up => {
                self.admin_down = false;
                if matches!(self.mode, LinkMode::Off) {
                    self.mode = LinkMode::Dhcp;
                }
                self.carrier = read_link_carrier();
                self.start_for_mode(now);
                self.step_timers(now);
            }
            CtlCommand::Reload => {
                self.send_release_if_bound();
                self.mode = load_config();
                self.admin_down = matches!(self.mode, LinkMode::Off);
                self.carrier = read_link_carrier();
                self.start_for_mode(now);
                self.step_timers(now);
            }
        }
        self.refresh_snapshot(Instant::now());
    }

    fn poll_carrier(&mut self, now: Instant) {
        if self.mac.is_none() {
            return;
        }
        if now.duration_since(self.last_link_poll) < LINK_POLL_INTERVAL {
            return;
        }
        self.last_link_poll = now;
        let current = read_link_carrier();
        if current == self.carrier {
            return;
        }
        self.carrier = current;
        if self.admin_down {
            self.refresh_snapshot(now);
            return;
        }
        if current == LinkCarrier::Down {
            self.close_udp();
            clear_netcfg_addr();
            self.state = State::CarrierDown;
        } else if matches!(self.state, State::CarrierDown) {
            self.start_for_mode(now);
        }
        self.refresh_snapshot(now);
    }

    fn enter_selecting(&mut self, now: Instant) {
        let Some(mac) = self.mac else {
            return;
        };
        let xid = rand_xid();
        let pkt = build_discover(xid, mac);
        self.send_udp(UdpTarget::Broadcast, &pkt);
        self.state = State::Selecting {
            xid,
            tries: 1,
            next_send: now + backoff_delay(1),
        };
    }

    fn enter_requesting(&mut self, now: Instant, xid: u32, offer: PendingOffer) {
        let Some(mac) = self.mac else {
            return;
        };
        let pkt = build_request_selecting(xid, mac, offer.ip, offer.server_id);
        self.send_udp(UdpTarget::Broadcast, &pkt);
        self.state = State::Requesting {
            xid,
            offer,
            tries: 1,
            next_send: now + backoff_delay(1),
        };
    }

    fn enter_probing(&mut self, now: Instant, lease: Lease) {
        self.close_udp();
        let _ = log_netcfg(
            "/scheme/netcfg/ifaces/eth0/arp/probe",
            &format!("{}\n", lease.ip),
        );
        self.state = State::Probing {
            lease,
            next_poll: now + Duration::from_millis(ARP_POLL_MS),
            polls: 0,
        };
    }

    fn apply_and_bind(&mut self, now: Instant, lease: Lease) {
        self.close_udp();
        if apply_netcfg(
            lease.ip,
            lease.prefix_len,
            &lease.routes,
            &lease.dns,
            lease.mtu,
        ) {
            self.state = State::Bound { lease };
        } else {
            clear_netcfg_addr();
            self.state = State::Init {
                next_attempt: now + DECLINE_BACKOFF,
            };
        }
    }

    fn renew_retry_delay(now: Instant, deadline: Instant) -> Duration {
        let rem = deadline.saturating_duration_since(now).as_secs();
        Duration::from_secs((rem / 2).max(MIN_RENEW_RETRY_SECS))
    }

    fn enter_renewing(&mut self, now: Instant, lease: Lease) {
        let Some(mac) = self.mac else {
            return;
        };
        let xid = rand_xid();
        let pkt = build_request_renewing(xid, mac, lease.ip);
        self.send_udp(UdpTarget::Unicast(lease.server_id), &pkt);
        let next_send = now + Self::renew_retry_delay(now, lease.t2_at);
        self.state = State::Renewing {
            lease,
            xid,
            next_send,
        };
    }

    fn enter_rebinding(&mut self, now: Instant, lease: Lease) {
        let Some(mac) = self.mac else {
            return;
        };
        let xid = rand_xid();
        let pkt = build_request_rebinding(xid, mac, lease.ip);
        self.send_udp(UdpTarget::Broadcast, &pkt);
        let next_send = now + Self::renew_retry_delay(now, lease.expires_at);
        self.state = State::Rebinding {
            lease,
            xid,
            next_send,
        };
    }

    fn expire_lease(&mut self, now: Instant) {
        self.close_udp();
        clear_netcfg_addr();
        self.state = State::Init { next_attempt: now };
        self.enter_selecting(now);
    }

    fn drain_udp(&mut self, now: Instant) {
        let mut buf = [0u8; MAX_PACKET_LEN];
        for _ in 0..MAX_UDP_DRAIN {
            let n = {
                let Some((fd, _)) = &self.udp else {
                    return;
                };
                match fd.read(&mut buf) {
                    Ok(n) if n > 0 => n,
                    Ok(_) => return,
                    Err(e)
                        if e.errno() == EAGAIN
                            || e.errno() == EWOULDBLOCK
                            || e.errno() == EINTR =>
                    {
                        return;
                    }
                    Err(_) => return,
                }
            };
            self.handle_packet(&buf[..n], now);
        }
    }

    fn handle_packet(&mut self, pkt: &[u8], now: Instant) {
        let Some(mac) = self.mac else {
            return;
        };
        let expected_xid = match &self.state {
            State::Selecting { xid, .. }
            | State::Requesting { xid, .. }
            | State::Renewing { xid, .. }
            | State::Rebinding { xid, .. } => *xid,
            _ => return,
        };
        let Some(reply) = parse_reply(pkt, expected_xid, mac) else {
            return;
        };
        match self.state.clone() {
            State::Selecting { xid, .. } => {
                if reply.msg_type == DHCPOFFER {
                    let offer = PendingOffer {
                        ip: reply.yiaddr,
                        server_id: reply.server_id,
                    };
                    self.enter_requesting(now, xid, offer);
                    self.refresh_snapshot(now);
                }
            }
            State::Requesting { offer, .. } => match reply.msg_type {
                DHCPACK => {
                    if reply.server_id != offer.server_id || reply.yiaddr != offer.ip {
                        return;
                    }
                    let lease = lease_from_reply(&reply, now);
                    self.enter_probing(now, lease);
                    self.refresh_snapshot(now);
                }
                DHCPNAK => {
                    if reply.server_id != offer.server_id {
                        return;
                    }
                    self.close_udp();
                    self.state = State::Init {
                        next_attempt: now + Duration::from_secs(1),
                    };
                    self.refresh_snapshot(now);
                }
                _ => {}
            },
            State::Renewing { lease, .. } => match reply.msg_type {
                DHCPACK => {
                    if reply.server_id != lease.server_id || reply.yiaddr != lease.ip {
                        return;
                    }
                    let updated = lease_from_reply(&reply, now);
                    let same_net = updated.ip == lease.ip
                        && updated.prefix_len == lease.prefix_len
                        && updated.routes == lease.routes
                        && updated.mtu == lease.mtu;
                    if same_net {
                        if updated.dns != lease.dns {
                            write_dns_files(&updated.dns);
                        }
                        self.close_udp();
                        self.state = State::Bound { lease: updated };
                    } else {
                        self.apply_and_bind(now, updated);
                    }
                    self.refresh_snapshot(now);
                }
                DHCPNAK => {
                    if reply.server_id != lease.server_id {
                        return;
                    }
                    self.expire_lease(now);
                    self.refresh_snapshot(now);
                }
                _ => {}
            },
            State::Rebinding { lease, .. } => match reply.msg_type {
                DHCPACK => {
                    if reply.yiaddr != lease.ip {
                        return;
                    }
                    let updated = lease_from_reply(&reply, now);
                    let same_net = updated.ip == lease.ip
                        && updated.prefix_len == lease.prefix_len
                        && updated.routes == lease.routes
                        && updated.mtu == lease.mtu;
                    if same_net {
                        if updated.dns != lease.dns {
                            write_dns_files(&updated.dns);
                        }
                        self.close_udp();
                        self.state = State::Bound { lease: updated };
                    } else {
                        self.apply_and_bind(now, updated);
                    }
                    self.refresh_snapshot(now);
                }
                DHCPNAK => {
                    self.expire_lease(now);
                    self.refresh_snapshot(now);
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn step_timers(&mut self, now: Instant) {
        self.poll_carrier(now);
        let Some(mac) = self.mac else {
            return;
        };
        match self.state.clone() {
            State::Down | State::Static(_) | State::CarrierDown => {}
            State::Init { next_attempt } => {
                if now >= next_attempt {
                    self.enter_selecting(now);
                    self.refresh_snapshot(now);
                }
            }
            State::Selecting {
                xid,
                tries,
                next_send,
            } => {
                if now >= next_send {
                    let next_tries = tries.saturating_add(1);
                    let pkt = build_discover(xid, mac);
                    self.send_udp(UdpTarget::Broadcast, &pkt);
                    self.state = State::Selecting {
                        xid,
                        tries: next_tries,
                        next_send: now + backoff_delay(next_tries),
                    };
                }
            }
            State::Requesting {
                xid,
                offer,
                tries,
                next_send,
            } => {
                if now >= next_send {
                    if tries >= MAX_REQUEST_TRIES {
                        self.enter_selecting(now);
                    } else {
                        let next_tries = tries.saturating_add(1);
                        let pkt = build_request_selecting(xid, mac, offer.ip, offer.server_id);
                        self.send_udp(UdpTarget::Broadcast, &pkt);
                        self.state = State::Requesting {
                            xid,
                            offer,
                            tries: next_tries,
                            next_send: now + backoff_delay(next_tries),
                        };
                    }
                    self.refresh_snapshot(now);
                }
            }
            State::Probing {
                lease,
                next_poll,
                polls,
            } => {
                if now >= next_poll {
                    let status = fs::read_to_string("/scheme/netcfg/ifaces/eth0/arp/probe")
                        .unwrap_or_else(|_| "free\n".into());
                    let trimmed = status.trim();
                    if trimmed.starts_with("conflict") {
                        let _ = log_netcfg("/scheme/netcfg/ifaces/eth0/arp/probe", "clear\n");
                        let xid = rand_xid();
                        let pkt = build_decline(xid, mac, lease.ip, lease.server_id);
                        self.send_udp(UdpTarget::Broadcast, &pkt);
                        thread::sleep(Duration::from_millis(50));
                        self.close_udp();
                        self.state = State::Init {
                            next_attempt: now + DECLINE_BACKOFF,
                        };
                        self.refresh_snapshot(now);
                    } else if trimmed == "free" || trimmed == "idle" || polls >= MAX_ARP_POLLS {
                        let _ = log_netcfg("/scheme/netcfg/ifaces/eth0/arp/probe", "clear\n");
                        self.apply_and_bind(now, lease);
                        self.refresh_snapshot(now);
                    } else {
                        self.state = State::Probing {
                            lease,
                            next_poll: now + Duration::from_millis(ARP_POLL_MS),
                            polls: polls.saturating_add(1),
                        };
                    }
                }
            }
            State::Bound { lease } => {
                if now >= lease.expires_at {
                    self.expire_lease(now);
                    self.refresh_snapshot(now);
                } else if now >= lease.t1_at {
                    self.enter_renewing(now, lease);
                    self.refresh_snapshot(now);
                }
            }
            State::Renewing {
                lease,
                xid,
                next_send,
            } => {
                if now >= lease.expires_at {
                    self.expire_lease(now);
                    self.refresh_snapshot(now);
                } else if now >= lease.t2_at {
                    self.enter_rebinding(now, lease);
                    self.refresh_snapshot(now);
                } else if now >= next_send {
                    let pkt = build_request_renewing(xid, mac, lease.ip);
                    self.send_udp(UdpTarget::Unicast(lease.server_id), &pkt);
                    self.state = State::Renewing {
                        next_send: now + Self::renew_retry_delay(now, lease.t2_at),
                        lease,
                        xid,
                    };
                }
            }
            State::Rebinding {
                lease,
                xid,
                next_send,
            } => {
                if now >= lease.expires_at {
                    self.expire_lease(now);
                    self.refresh_snapshot(now);
                } else if now >= next_send {
                    let pkt = build_request_rebinding(xid, mac, lease.ip);
                    self.send_udp(UdpTarget::Broadcast, &pkt);
                    self.state = State::Rebinding {
                        next_send: now + Self::renew_retry_delay(now, lease.expires_at),
                        lease,
                        xid,
                    };
                }
            }
        }
    }

    fn next_wakeup(&self, now: Instant) -> Duration {
        let mut delay = self
            .last_link_poll
            .checked_add(LINK_POLL_INTERVAL)
            .map(|t| t.saturating_duration_since(now))
            .unwrap_or(LINK_POLL_INTERVAL);
        let state_delay = match &self.state {
            State::Down | State::Static(_) | State::CarrierDown => LINK_POLL_INTERVAL,
            State::Init { next_attempt } => next_attempt.saturating_duration_since(now),
            State::Selecting { next_send, .. } | State::Requesting { next_send, .. } => {
                next_send.saturating_duration_since(now)
            }
            State::Probing { next_poll, .. } => next_poll.saturating_duration_since(now),
            State::Bound { lease } => lease.t1_at.saturating_duration_since(now),
            State::Renewing {
                lease, next_send, ..
            } => lease.t2_at.min(*next_send).saturating_duration_since(now),
            State::Rebinding {
                lease, next_send, ..
            } => lease
                .expires_at
                .min(*next_send)
                .saturating_duration_since(now),
        };
        delay = delay.min(state_delay);
        delay.clamp(Duration::from_millis(25), LINK_POLL_INTERVAL)
    }

    fn refresh_snapshot(&self, now: Instant) {
        if self.mac.is_none() {
            let mut snap = self.snapshot.borrow_mut();
            snap.status_json = "{\"interfaces\":[]}\n".to_owned();
            snap.ifaddrs_text.clear();
            return;
        }
        let mode_str = match &self.mode {
            LinkMode::Dhcp => "dhcp",
            LinkMode::Static(_) => "static",
            LinkMode::Off => "off",
        };
        let state_str = match &self.state {
            State::Down => "down",
            State::Static(_) => "static",
            State::CarrierDown => "carrier-down",
            State::Init { .. } => "init",
            State::Selecting { .. } => "selecting",
            State::Requesting { .. } => "requesting",
            State::Probing { .. } => "probing",
            State::Bound { .. } => "bound",
            State::Renewing { .. } => "renewing",
            State::Rebinding { .. } => "rebinding",
        };

        let (ipv4, mtu, routes_json, dns_json, lease_json) = match &self.state {
            State::Static(cfg) => {
                let routes = match cfg.gateway {
                    Some(gw) => vec![json!({"dst": "0.0.0.0/0", "via": gw.to_string()})],
                    None => Vec::new(),
                };
                let dns: Vec<String> = cfg.dns.iter().map(ToString::to_string).collect();
                (
                    Some(format!("{}/{}", cfg.address, cfg.prefix_len)),
                    cfg.mtu,
                    routes,
                    dns,
                    serde_json::Value::Null,
                )
            }
            State::Bound { lease }
            | State::Renewing { lease, .. }
            | State::Rebinding { lease, .. } => {
                let routes: Vec<serde_json::Value> = lease
                    .routes
                    .iter()
                    .map(|r| {
                        let dst = format!("{}/{}", r.network, r.prefix_len);
                        match r.via {
                            Some(gw) => json!({"dst": dst, "via": gw.to_string()}),
                            None => json!({"dst": dst, "via": serde_json::Value::Null}),
                        }
                    })
                    .collect();
                let dns: Vec<String> = lease.dns.iter().map(ToString::to_string).collect();
                let remaining_secs = lease.expires_at.saturating_duration_since(now).as_secs();
                (
                    Some(format!("{}/{}", lease.ip, lease.prefix_len)),
                    lease.mtu,
                    routes,
                    dns,
                    json!({
                        "server": lease.server_id.to_string(),
                        "lease_secs": lease.lease_secs,
                        "t1_secs": lease.t1_secs,
                        "t2_secs": lease.t2_secs,
                        "remaining_secs": remaining_secs,
                    }),
                )
            }
            _ => (
                None,
                MAX_MTU,
                Vec::new(),
                Vec::new(),
                serde_json::Value::Null,
            ),
        };

        let status_val = json!({
            "interfaces": [
                {
                    "name": "eth0",
                    "mode": mode_str,
                    "state": state_str,
                    "link": self.carrier.as_str(),
                    "mtu": mtu,
                    "ipv4": ipv4,
                    "routes": routes_json,
                    "dns": dns_json,
                    "lease": lease_json,
                }
            ]
        });
        let ifaddrs_line = match &ipv4 {
            Some(cidr) => format!("eth0 {cidr} {}\n", self.carrier.as_str()),
            None => format!("eth0 - {}\n", self.carrier.as_str()),
        };
        let mut snap = self.snapshot.borrow_mut();
        snap.status_json = format!("{status_val}\n");
        snap.ifaddrs_text = ifaddrs_line;
    }
}

fn read_monotonic_ns(time_fd: &mut File) -> Option<u128> {
    let mut ts = TimeSpec::default();
    if time_fd.read(&mut ts).unwrap_or(0) < size_of::<TimeSpec>() {
        return None;
    }
    Some(
        (ts.tv_sec.max(0) as u128)
            .saturating_mul(1_000_000_000)
            .saturating_add(ts.tv_nsec.max(0) as u128),
    )
}

fn arm_timer(time_fd: &mut File, delay: Duration, armed: &mut Option<u128>) {
    let Some(cur_ns) = read_monotonic_ns(time_fd) else {
        return;
    };
    if let Some(existing) = *armed {
        if existing <= cur_ns {
            *armed = None;
        }
    }
    let target_ns = cur_ns.saturating_add(delay.as_nanos());
    if let Some(existing) = *armed {
        if existing <= target_ns {
            return;
        }
    }
    let next = TimeSpec {
        tv_sec: i64::try_from(target_ns / 1_000_000_000).unwrap_or(i64::MAX),
        tv_nsec: (target_ns % 1_000_000_000) as i32,
    };
    if time_fd.write_all(&next).is_ok() {
        *armed = Some(target_ns);
    }
}

fn run(daemon: Daemon) -> Result<()> {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = on_sigterm as *const () as usize;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
    }
    let _ = libredox::call::sigaction(libredox::flag::SIGTERM, Some(&action), None);

    let mac = read_mac();
    let mode = load_config();
    let has_nic = mac.is_some();
    let snapshot = Rc::new(RefCell::new(StatusSnapshot::default()));
    let engine = Rc::new(RefCell::new(Engine::new(mac, mode, Rc::clone(&snapshot))));

    let status_sock = Socket::nonblock().context("create netstatus socket")?;
    let mut status_scheme = NetStatusScheme::new(Rc::clone(&snapshot));
    let status_root = status_scheme
        .scheme_root()
        .map_err(|e| anyhow::anyhow!("netstatus root: {e}"))?;
    register_scheme_inner(&status_sock, "netstatus", status_root)
        .map_err(|e| anyhow::anyhow!("register netstatus: {e}"))?;

    let ctl_sock = Socket::nonblock().context("create netctl socket")?;
    let engine_ctl = Rc::clone(&engine);
    let mut ctl_scheme = NetCtlScheme::new(move |cmd| {
        engine_ctl.borrow_mut().handle_ctl(cmd);
        Ok(())
    });
    let ctl_root = ctl_scheme
        .scheme_root()
        .map_err(|e| anyhow::anyhow!("netctl root: {e}"))?;
    register_scheme_inner(&ctl_sock, "netctl", ctl_root)
        .map_err(|e| anyhow::anyhow!("register netctl: {e}"))?;

    daemon.ready();

    let ns = if has_nic {
        libredox::call::mkns(&[
            IoSlice::new(b"netcfg"),
            IoSlice::new(b"udp"),
            IoSlice::new(b"rand"),
            IoSlice::new(b"time"),
            IoSlice::new(b"event"),
            IoSlice::new(b"file"),
        ])
    } else {
        libredox::call::mkns(&[
            IoSlice::new(b"rand"),
            IoSlice::new(b"time"),
            IoSlice::new(b"event"),
            IoSlice::new(b"file"),
        ])
    }
    .map_err(|e| anyhow::anyhow!("mkns: {e}"))?;
    let prev_ns = libredox::call::setns(ns).map_err(|e| anyhow::anyhow!("setns: {e}"))?;
    let _ = libredox::call::close(prev_ns);

    let mut time_file = File::options()
        .read(true)
        .write(true)
        .open(format!("/scheme/time/{CLOCK_MONOTONIC}"))
        .context("open monotonic time")?;
    let mut event_queue = File::options()
        .read(true)
        .write(true)
        .open("/scheme/event")
        .context("open /scheme/event")?;

    let mut status_handler = ReadinessBased::new(Box::new(status_sock), MAX_REQ_BATCH);
    let mut ctl_handler = ReadinessBased::new(Box::new(ctl_sock), MAX_REQ_BATCH);

    event_queue.write_all(&Event {
        id: status_handler.socket().inner().raw(),
        flags: EVENT_READ,
        data: 1,
    })?;
    event_queue.write_all(&Event {
        id: ctl_handler.socket().inner().raw(),
        flags: EVENT_READ,
        data: 2,
    })?;
    event_queue.write_all(&Event {
        id: time_file.as_raw_fd() as usize,
        flags: EVENT_READ,
        data: 3,
    })?;

    engine
        .borrow_mut()
        .set_event_fd(event_queue.as_raw_fd() as usize);
    engine.borrow_mut().step_timers(Instant::now());
    let mut raw_ev = [0u8; size_of::<Event>()];
    let mut armed_deadline_ns: Option<u128> = None;
    loop {
        if TERMINATE.load(Ordering::SeqCst) {
            engine.borrow_mut().handle_ctl(CtlCommand::Release);
            return Ok(());
        }

        let now = Instant::now();
        {
            let mut eng = engine.borrow_mut();
            eng.drain_udp(now);
            eng.step_timers(now);
            eng.refresh_snapshot(now);
        }

        let _ = status_handler.read_and_process_requests(&mut status_scheme);
        let _ = status_handler.poll_all_requests(&mut status_scheme);
        let _ = status_handler.write_responses();
        let _ = ctl_handler.read_and_process_requests(&mut ctl_scheme);
        let _ = ctl_handler.poll_all_requests(&mut ctl_scheme);
        let _ = ctl_handler.write_responses();

        let delay = engine.borrow().next_wakeup(Instant::now());
        arm_timer(&mut time_file, delay, &mut armed_deadline_ns);

        if let Ok(n) = event_queue.read(&mut raw_ev) {
            if n >= size_of::<Event>() {
                let ev = unsafe { core::ptr::read_unaligned(raw_ev.as_ptr().cast::<Event>()) };
                if ev.data == 3 {
                    armed_deadline_ns = None;
                }
            }
        }
    }
}

fn main() {
    Daemon::new(|daemon| {
        if let Err(err) = run(daemon) {
            eprintln!("ferrion-netd: {err:#}");
            std::process::exit(1);
        }
        std::process::exit(0);
    });
}

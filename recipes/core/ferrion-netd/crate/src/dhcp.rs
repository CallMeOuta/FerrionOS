use std::net::Ipv4Addr;

use crate::config::{is_usable_unicast, MAX_DNS_SERVERS, MAX_MTU, MIN_MTU};

pub const BOOTREQUEST: u8 = 1;
pub const BOOTREPLY: u8 = 2;
pub const HTYPE_ETHERNET: u8 = 1;
pub const HLEN_ETHERNET: u8 = 6;
pub const FLAG_BROADCAST: u16 = 0x8000;
pub const MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];

pub const DHCPDISCOVER: u8 = 1;
pub const DHCPOFFER: u8 = 2;
pub const DHCPREQUEST: u8 = 3;
pub const DHCPDECLINE: u8 = 4;
pub const DHCPACK: u8 = 5;
pub const DHCPNAK: u8 = 6;
pub const DHCPRELEASE: u8 = 7;

pub const OPT_PAD: u8 = 0;
pub const OPT_SUBNET_MASK: u8 = 1;
pub const OPT_ROUTER: u8 = 3;
pub const OPT_DNS: u8 = 6;
pub const OPT_INTERFACE_MTU: u8 = 26;
pub const OPT_REQUESTED_IP: u8 = 50;
pub const OPT_LEASE_TIME: u8 = 51;
pub const OPT_MESSAGE_TYPE: u8 = 53;
pub const OPT_SERVER_ID: u8 = 54;
pub const OPT_PARAM_REQ_LIST: u8 = 55;
pub const OPT_RENEWAL_TIME: u8 = 58;
pub const OPT_REBINDING_TIME: u8 = 59;
pub const OPT_CLASSLESS_STATIC_ROUTES: u8 = 121;
pub const OPT_END: u8 = 255;

pub const PRL_MINIMAL: [u8; 5] = [
    OPT_SUBNET_MASK,
    OPT_ROUTER,
    OPT_DNS,
    OPT_INTERFACE_MTU,
    OPT_CLASSLESS_STATIC_ROUTES,
];

pub const MIN_PACKET_LEN: usize = 240;
pub const MIN_BOOTP_WIRE_LEN: usize = 300;
pub const MAX_PACKET_LEN: usize = 1500;
pub const MAX_OPTIONS_ITERS: usize = 128;
pub const MAX_ROUTES: usize = 32;
pub const MAX_LEASE_SECS: u32 = 30 * 86_400;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteEntry {
    pub network: Ipv4Addr,
    pub prefix_len: u8,
    pub via: Option<Ipv4Addr>,
}

#[derive(Clone, Debug)]
pub struct DhcpReply {
    pub msg_type: u8,
    pub yiaddr: Ipv4Addr,
    pub server_id: Ipv4Addr,
    pub prefix_len: Option<u8>,
    pub routes: Vec<RouteEntry>,
    pub dns: Vec<Ipv4Addr>,
    pub mtu: Option<u16>,
    pub lease_secs: Option<u32>,
    pub t1_secs: Option<u32>,
    pub t2_secs: Option<u32>,
}

fn push_opt(pkt: &mut Vec<u8>, code: u8, data: &[u8]) {
    assert!(data.len() <= u8::MAX as usize);
    pkt.push(code);
    pkt.push(data.len() as u8);
    pkt.extend_from_slice(data);
}

fn base_packet(xid: u32, mac: [u8; 6], ciaddr: Ipv4Addr, broadcast: bool) -> Vec<u8> {
    let mut pkt = vec![0u8; MIN_PACKET_LEN];
    pkt[0] = BOOTREQUEST;
    pkt[1] = HTYPE_ETHERNET;
    pkt[2] = HLEN_ETHERNET;
    pkt[4..8].copy_from_slice(&xid.to_be_bytes());
    if broadcast {
        pkt[10..12].copy_from_slice(&FLAG_BROADCAST.to_be_bytes());
    }
    pkt[12..16].copy_from_slice(&ciaddr.octets());
    pkt[28..34].copy_from_slice(&mac);
    pkt[236..240].copy_from_slice(&MAGIC_COOKIE);
    pkt
}

fn finish_packet(mut pkt: Vec<u8>) -> Vec<u8> {
    pkt.push(OPT_END);
    if pkt.len() < MIN_BOOTP_WIRE_LEN {
        pkt.resize(MIN_BOOTP_WIRE_LEN, 0);
    }
    assert!(pkt.len() <= MAX_PACKET_LEN);
    pkt
}

pub fn build_discover(xid: u32, mac: [u8; 6]) -> Vec<u8> {
    let mut pkt = base_packet(xid, mac, Ipv4Addr::UNSPECIFIED, true);
    push_opt(&mut pkt, OPT_MESSAGE_TYPE, &[DHCPDISCOVER]);
    push_opt(&mut pkt, OPT_PARAM_REQ_LIST, &PRL_MINIMAL);
    finish_packet(pkt)
}

pub fn build_request_selecting(
    xid: u32,
    mac: [u8; 6],
    offered_ip: Ipv4Addr,
    server_id: Ipv4Addr,
) -> Vec<u8> {
    let mut pkt = base_packet(xid, mac, Ipv4Addr::UNSPECIFIED, true);
    push_opt(&mut pkt, OPT_MESSAGE_TYPE, &[DHCPREQUEST]);
    push_opt(&mut pkt, OPT_REQUESTED_IP, &offered_ip.octets());
    push_opt(&mut pkt, OPT_SERVER_ID, &server_id.octets());
    push_opt(&mut pkt, OPT_PARAM_REQ_LIST, &PRL_MINIMAL);
    finish_packet(pkt)
}

pub fn build_request_renewing(xid: u32, mac: [u8; 6], client_ip: Ipv4Addr) -> Vec<u8> {
    let mut pkt = base_packet(xid, mac, client_ip, false);
    push_opt(&mut pkt, OPT_MESSAGE_TYPE, &[DHCPREQUEST]);
    push_opt(&mut pkt, OPT_PARAM_REQ_LIST, &PRL_MINIMAL);
    finish_packet(pkt)
}

pub fn build_request_rebinding(xid: u32, mac: [u8; 6], client_ip: Ipv4Addr) -> Vec<u8> {
    let mut pkt = base_packet(xid, mac, client_ip, false);
    push_opt(&mut pkt, OPT_MESSAGE_TYPE, &[DHCPREQUEST]);
    push_opt(&mut pkt, OPT_PARAM_REQ_LIST, &PRL_MINIMAL);
    finish_packet(pkt)
}

pub fn build_decline(
    xid: u32,
    mac: [u8; 6],
    declined_ip: Ipv4Addr,
    server_id: Ipv4Addr,
) -> Vec<u8> {
    let mut pkt = base_packet(xid, mac, Ipv4Addr::UNSPECIFIED, false);
    push_opt(&mut pkt, OPT_MESSAGE_TYPE, &[DHCPDECLINE]);
    push_opt(&mut pkt, OPT_REQUESTED_IP, &declined_ip.octets());
    push_opt(&mut pkt, OPT_SERVER_ID, &server_id.octets());
    finish_packet(pkt)
}

pub fn build_release(xid: u32, mac: [u8; 6], client_ip: Ipv4Addr, server_id: Ipv4Addr) -> Vec<u8> {
    let mut pkt = base_packet(xid, mac, client_ip, false);
    push_opt(&mut pkt, OPT_MESSAGE_TYPE, &[DHCPRELEASE]);
    push_opt(&mut pkt, OPT_SERVER_ID, &server_id.octets());
    finish_packet(pkt)
}

fn subnet_mask_to_prefix(mask: [u8; 4]) -> Option<u8> {
    let bits = u32::from_be_bytes(mask);
    let ones = bits.leading_ones();
    if ones == 0 || ones > 32 {
        return None;
    }
    let expected = if ones == 32 {
        u32::MAX
    } else {
        !0u32 << (32 - ones)
    };
    if bits != expected {
        return None;
    }
    Some(ones as u8)
}

fn parse_option_121(data: &[u8]) -> Option<Vec<RouteEntry>> {
    let mut routes = Vec::new();
    let mut pos = 0usize;
    for _ in 0..MAX_ROUTES {
        if pos >= data.len() {
            break;
        }
        let prefix_len = data[pos];
        pos += 1;
        if prefix_len > 32 {
            return None;
        }
        let sig_octets = usize::from(prefix_len).div_ceil(8);
        if pos + sig_octets + 4 > data.len() {
            return None;
        }
        let mut net = [0u8; 4];
        net[..sig_octets].copy_from_slice(&data[pos..pos + sig_octets]);
        pos += sig_octets;
        let mask = if prefix_len == 0 {
            0u32
        } else {
            !0u32 << (32 - u32::from(prefix_len))
        };
        let net_masked = Ipv4Addr::from(u32::from_be_bytes(net) & mask);
        let gw = Ipv4Addr::new(data[pos], data[pos + 1], data[pos + 2], data[pos + 3]);
        pos += 4;
        let via = if gw.is_unspecified() {
            None
        } else if is_usable_unicast(gw) {
            Some(gw)
        } else {
            return None;
        };
        routes.push(RouteEntry {
            network: net_masked,
            prefix_len,
            via,
        });
    }
    if pos != data.len() && routes.len() < MAX_ROUTES {
        return None;
    }
    Some(routes)
}

pub fn parse_reply(buf: &[u8], expected_xid: u32, expected_mac: [u8; 6]) -> Option<DhcpReply> {
    if buf.len() < MIN_PACKET_LEN || buf.len() > MAX_PACKET_LEN {
        return None;
    }
    if buf[0] != BOOTREPLY || buf[1] != HTYPE_ETHERNET || buf[2] != HLEN_ETHERNET {
        return None;
    }
    let xid = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if xid != expected_xid {
        return None;
    }
    if buf[28..34] != expected_mac {
        return None;
    }
    if buf[236..240] != MAGIC_COOKIE {
        return None;
    }

    let yiaddr = Ipv4Addr::new(buf[16], buf[17], buf[18], buf[19]);
    let mut msg_type: Option<u8> = None;
    let mut server_id: Option<Ipv4Addr> = None;
    let mut prefix_len: Option<u8> = None;
    let mut opt3_router: Option<Ipv4Addr> = None;
    let mut opt121_routes: Option<Vec<RouteEntry>> = None;
    let mut dns: Vec<Ipv4Addr> = Vec::new();
    let mut mtu: Option<u16> = None;
    let mut lease_secs: Option<u32> = None;
    let mut t1_secs: Option<u32> = None;
    let mut t2_secs: Option<u32> = None;

    let mut pos = MIN_PACKET_LEN;
    let mut saw_end = false;
    for _ in 0..MAX_OPTIONS_ITERS {
        if pos >= buf.len() {
            break;
        }
        let code = buf[pos];
        pos += 1;
        if code == OPT_PAD {
            continue;
        }
        if code == OPT_END {
            saw_end = true;
            break;
        }
        if pos >= buf.len() {
            return None;
        }
        let len = usize::from(buf[pos]);
        pos += 1;
        if pos + len > buf.len() {
            return None;
        }
        let val = &buf[pos..pos + len];
        pos += len;

        match code {
            OPT_MESSAGE_TYPE => {
                if len != 1 {
                    return None;
                }
                msg_type = Some(val[0]);
            }
            OPT_SERVER_ID => {
                if len != 4 {
                    return None;
                }
                let ip = Ipv4Addr::new(val[0], val[1], val[2], val[3]);
                if !is_usable_unicast(ip) {
                    return None;
                }
                server_id = Some(ip);
            }
            OPT_SUBNET_MASK => {
                if len != 4 {
                    return None;
                }
                prefix_len = Some(subnet_mask_to_prefix([val[0], val[1], val[2], val[3]])?);
            }
            OPT_ROUTER => {
                if len == 0 || len % 4 != 0 {
                    return None;
                }
                let ip = Ipv4Addr::new(val[0], val[1], val[2], val[3]);
                if !is_usable_unicast(ip) {
                    return None;
                }
                opt3_router = Some(ip);
            }
            OPT_DNS => {
                if len == 0 || len % 4 != 0 {
                    return None;
                }
                for chunk in val.chunks_exact(4) {
                    let ip = Ipv4Addr::new(chunk[0], chunk[1], chunk[2], chunk[3]);
                    if !is_usable_unicast(ip) {
                        return None;
                    }
                    if dns.len() < MAX_DNS_SERVERS {
                        dns.push(ip);
                    }
                }
            }
            OPT_INTERFACE_MTU => {
                if len != 2 {
                    return None;
                }
                let raw = u16::from_be_bytes([val[0], val[1]]);
                if raw >= MIN_MTU {
                    mtu = Some(raw.min(MAX_MTU));
                }
            }
            OPT_LEASE_TIME => {
                if len != 4 {
                    return None;
                }
                lease_secs = Some(u32::from_be_bytes([val[0], val[1], val[2], val[3]]));
            }
            OPT_RENEWAL_TIME => {
                if len != 4 {
                    return None;
                }
                t1_secs = Some(u32::from_be_bytes([val[0], val[1], val[2], val[3]]));
            }
            OPT_REBINDING_TIME => {
                if len != 4 {
                    return None;
                }
                t2_secs = Some(u32::from_be_bytes([val[0], val[1], val[2], val[3]]));
            }
            OPT_CLASSLESS_STATIC_ROUTES => {
                if len == 0 {
                    return None;
                }
                opt121_routes = Some(parse_option_121(val)?);
            }
            _ => {}
        }
    }

    if !saw_end {
        return None;
    }

    let msg_type = msg_type?;
    let server_id = server_id?;
    if matches!(msg_type, DHCPOFFER | DHCPACK) {
        if !is_usable_unicast(yiaddr) || lease_secs == Some(0) {
            return None;
        }
    }

    let routes = if let Some(r121) = opt121_routes {
        r121
    } else if let Some(gw) = opt3_router {
        vec![RouteEntry {
            network: Ipv4Addr::UNSPECIFIED,
            prefix_len: 0,
            via: Some(gw),
        }]
    } else {
        Vec::new()
    };

    Some(DhcpReply {
        msg_type,
        yiaddr,
        server_id,
        prefix_len,
        routes,
        dns,
        mtu,
        lease_secs,
        t1_secs,
        t2_secs,
    })
}

pub fn compute_timers(
    lease_raw: Option<u32>,
    t1_raw: Option<u32>,
    t2_raw: Option<u32>,
) -> (u64, u64, u64) {
    let lease = lease_raw.unwrap_or(3600).clamp(1, MAX_LEASE_SECS);
    let max_inner = lease.saturating_sub(1).max(1);
    let t1 = t1_raw
        .filter(|&v| v > 0 && v < lease)
        .unwrap_or((lease / 2).max(1))
        .min(max_inner);
    let t2 = t2_raw
        .filter(|&v| v > t1 && v < lease)
        .unwrap_or(((u64::from(lease) * 7 / 8) as u32).max(t1.saturating_add(1)))
        .min(max_inner);
    (u64::from(lease), u64::from(t1), u64::from(t2))
}

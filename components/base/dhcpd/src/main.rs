use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};
use std::{env, process};

use dhcp::Dhcp;

mod dhcp;

macro_rules! try_fmt {
    ($e:expr, $m:expr) => {
        match $e {
            Ok(ok) => ok,
            Err(err) => return Err(format!("{}: {}", $m, err)),
        }
    };
}

fn get_cfg_value(path: &str) -> Result<String, String> {
    let path = format!("/scheme/netcfg/{path}");
    let mut file = File::open(&path).map_err(|_| format!("Can't open {path}"))?;
    let mut result = String::new();
    file.read_to_string(&mut result)
        .map_err(|_| format!("Can't read {path}"))?;
    Ok(result)
}

fn get_iface_cfg_value(iface: &str, cfg: &str) -> Result<String, String> {
    let path = format!("ifaces/{iface}/{cfg}");
    get_cfg_value(&path)
}

fn set_cfg_value(path: &str, value: &str) -> Result<(), String> {
    let path = format!("/scheme/netcfg/{path}");
    let mut file = OpenOptions::new()
        .read(false)
        .write(true)
        .create(false)
        .open(&path)
        .map_err(|_| format!("Can't open {path}"))?;
    file.write(value.as_bytes())
        .map(|_| ())
        .map_err(|_| format!("Can't write {value} to {path}"))?;
    file.sync_data()
        .map_err(|_| format!("Can't commit {value} to {path}"))
}

fn set_iface_cfg_value(iface: &str, cfg: &str, value: &str) -> Result<(), String> {
    let path = format!("ifaces/{iface}/{cfg}");
    set_cfg_value(&path, value)
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Default)]
struct MacAddr {
    bytes: [u8; 6],
}

impl MacAddr {
    fn from_str(string: &str) -> Self {
        MacAddr::try_parse_with_delimeter(string, ':')
            .or_else(|| MacAddr::try_parse_with_delimeter(string, '-'))
            .unwrap_or_default()
    }

    fn try_parse_with_delimeter(string: &str, delimeter: char) -> Option<MacAddr> {
        let mut addr = MacAddr::default();
        let mut segments = 0;

        for part in string.split(delimeter) {
            if segments >= addr.bytes.len() {
                return None;
            }
            addr.bytes[segments] = match u8::from_str_radix(part, 16) {
                Ok(b) => b,
                _ => return None,
            };
            segments += 1;
        }

        if segments == addr.bytes.len() {
            Some(addr)
        } else {
            None
        }
    }

    fn to_string(&self) -> String {
        format!(
            "{:>02X}-{:>02X}-{:>02X}-{:>02X}-{:>02X}-{:>02X}",
            self.bytes[0],
            self.bytes[1],
            self.bytes[2],
            self.bytes[3],
            self.bytes[4],
            self.bytes[5]
        )
    }
}

fn first_dns_server(data: &[u8]) -> Option<[u8; 4]> {
    // Option 6 contains one or more IPv4 addresses in preference order.
    if data.is_empty() || data.len() % 4 != 0 {
        return None;
    }
    Some([data[0], data[1], data[2], data[3]])
}

const BOOTREQUEST: u8 = 1;
const BOOTREPLY: u8 = 2;
const DHCPDISCOVER: u8 = 1;
const DHCPOFFER: u8 = 2;
const DHCPREQUEST: u8 = 3;
const DHCPACK: u8 = 5;
const DHCPNAK: u8 = 6;
const MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];
/// The fixed fields and the magic cookie; the options follow.
const OPTIONS_START: usize = 240;
/// Ask the server to broadcast its replies: the stack has no address to receive a unicast one.
const FLAG_BROADCAST: u16 = 0x8000;

/// A server's reply that answers this client's request.
#[derive(Clone, Debug, Default, PartialEq)]
struct Reply {
    message_type: u8,
    yiaddr: [u8; 4],
    subnet: Option<[u8; 4]>,
    router: Option<[u8; 4]>,
    dns: Option<[u8; 4]>,
    server_id: Option<[u8; 4]>,
}

/// Parses `data` as a reply to the request with transaction ID `xid` from the client
/// with hardware address `mac`. Anything else is `None`: another client's reply, a
/// request, or a message whose fields or options are malformed.
fn parse_reply(data: &[u8], xid: [u8; 4], mac: [u8; 6]) -> Option<Reply> {
    if data.len() < OPTIONS_START
        || data[0] != BOOTREPLY
        || data[1] != 1
        || data[2] != 6
        || data[4..8] != xid
        || data[28..34] != mac
        || data[236..240] != MAGIC_COOKIE
    {
        return None;
    }

    let mut reply = Reply {
        yiaddr: data[16..20].try_into().ok()?,
        ..Reply::default()
    };
    let mut message_type = None;
    let mut options = &data[OPTIONS_START..];
    loop {
        match options {
            [] | [255, ..] => break,
            [0, rest @ ..] => options = rest,
            [code, len, rest @ ..] => {
                let value = rest.get(..usize::from(*len))?;
                options = &rest[value.len()..];
                // A known option of the wrong length makes the whole message malformed.
                match code {
                    1 => reply.subnet = reply.subnet.or(Some(value.try_into().ok()?)),
                    // Options 3 and 6 list addresses in preference order.
                    3 => reply.router = reply.router.or(Some(first_dns_server(value)?)),
                    6 => reply.dns = reply.dns.or(Some(first_dns_server(value)?)),
                    53 => match value {
                        [kind] => message_type = message_type.or(Some(*kind)),
                        _ => return None,
                    },
                    54 => reply.server_id = reply.server_id.or(Some(value.try_into().ok()?)),
                    _ => {}
                }
            }
            [_] => return None,
        }
    }
    reply.message_type = message_type?;
    Some(reply)
}

/// The offer to take: an offer of an address.
fn accept_offer(reply: Reply) -> Option<Reply> {
    (reply.message_type == DHCPOFFER && reply.yiaddr != [0; 4]).then_some(reply)
}

/// The answer to the request for `offer`: an acknowledgment of the offered address
/// or a refusal, from the server that made the offer.
fn accept_answer(reply: Reply, offer: &Reply) -> Option<Reply> {
    if offer.server_id.is_some() && reply.server_id != offer.server_id {
        return None;
    }
    match reply.message_type {
        DHCPACK if reply.yiaddr == offer.yiaddr => Some(reply),
        DHCPNAK => Some(reply),
        _ => None,
    }
}

fn random_xid() -> Result<[u8; 4], String> {
    let mut xid = [0; 4];
    try_fmt!(
        File::open("/scheme/rand").and_then(|mut rand| rand.read_exact(&mut xid)),
        "failed to read /scheme/rand"
    );
    Ok(xid)
}

fn message(xid: [u8; 4], mac: MacAddr, options: &[u8]) -> Dhcp {
    let mut chaddr = [0; 16];
    chaddr[..6].copy_from_slice(&mac.bytes);
    let mut message = Dhcp {
        op: BOOTREQUEST,
        htype: 1,
        hlen: 6,
        hops: 0,
        tid: u32::from_ne_bytes(xid),
        secs: 0,
        flags: FLAG_BROADCAST.to_be(),
        ciaddr: [0, 0, 0, 0],
        yiaddr: [0, 0, 0, 0],
        siaddr: [0, 0, 0, 0],
        giaddr: [0, 0, 0, 0],
        chaddr,
        sname: [0; 64],
        file: [0; 128],
        magic: u32::from_ne_bytes(MAGIC_COOKIE),
        options: [0; 308],
    };
    message.options[..options.len()].copy_from_slice(options);
    message
}

fn send(socket: &UdpSocket, message: &Dhcp) -> Result<(), String> {
    let data = unsafe {
        std::slice::from_raw_parts((message as *const Dhcp) as *const u8, size_of::<Dhcp>())
    };
    try_fmt!(socket.send(data), "failed to send");
    Ok(())
}

/// Waits until `deadline` for a datagram that `accept` takes, and ignores the others.
fn receive(
    socket: &UdpSocket,
    deadline: Instant,
    verbose: bool,
    accept: impl Fn(&[u8]) -> Option<Reply>,
) -> Result<Reply, String> {
    let mut data = vec![0; 65536];
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or("timed out")?;
        try_fmt!(
            socket.set_read_timeout(Some(remaining)),
            "failed to set read timeout"
        );
        let len = try_fmt!(socket.recv(&mut data), "failed to receive");
        match accept(&data[..len]) {
            Some(reply) => return Ok(reply),
            None if verbose => println!("DHCP: Ignored a datagram of {len} bytes"),
            None => {}
        }
    }
}

fn dhcp(iface: &str, verbose: bool) -> Result<(), String> {
    let current_mac = MacAddr::from_str(get_iface_cfg_value(iface, "mac")?.trim());

    let current_ip = get_iface_cfg_value(iface, "addr/list")?
        .lines()
        .next()
        .map(|l| l.to_owned())
        .unwrap_or("0.0.0.0".to_string());

    if verbose {
        println!(
            "DHCP: MAC: {} Current IP: {}",
            current_mac.to_string(),
            current_ip.trim()
        );
    }

    let xid = random_xid()?;

    let socket = try_fmt!(UdpSocket::bind(("0.0.0.0", 68)), "failed to bind udp");
    try_fmt!(
        socket.connect(SocketAddr::from(([255, 255, 255, 255], 67))),
        "failed to connect udp"
    );
    try_fmt!(
        socket.set_write_timeout(Some(Duration::new(30, 0))),
        "failed to set write timeout"
    );
    let deadline = Instant::now() + Duration::new(30, 0);

    send(
        &socket,
        &message(xid, current_mac, &[53, 1, DHCPDISCOVER, 255]),
    )
    .map_err(|err| format!("discover: {err}"))?;
    if verbose {
        println!("DHCP: Sent Discover");
    }

    let offer = receive(&socket, deadline, verbose, |data| {
        parse_reply(data, xid, current_mac.bytes).and_then(accept_offer)
    })
    .map_err(|err| format!("offer: {err}"))?;
    if verbose {
        println!("DHCP: Offer: {offer:?}");
    }

    // If the offer named no server, use "0.0.0.0"
    let server_id = offer.server_id.unwrap_or([0, 0, 0, 0]);
    let [a, b, c, d] = offer.yiaddr;
    let [s0, s1, s2, s3] = server_id;
    send(
        &socket,
        &message(
            xid,
            current_mac,
            &[
                // DHCP Message Type (Request)
                53,
                1,
                DHCPREQUEST,
                // Requested IP Address
                50,
                4,
                a,
                b,
                c,
                d,
                // Server Identifier - use Option 54 from the Offer
                54,
                4,
                s0,
                s1,
                s2,
                s3,
                // End
                255,
            ],
        ),
    )
    .map_err(|err| format!("request: {err}"))?;
    if verbose {
        println!("DHCP: Sent Request");
    }

    let ack = receive(&socket, deadline, verbose, |data| {
        parse_reply(data, xid, current_mac.bytes).and_then(|reply| accept_answer(reply, &offer))
    })
    .map_err(|err| format!("ack: {err}"))?;
    if ack.message_type == DHCPNAK {
        return Err("the server refused the offered address".to_string());
    }
    if verbose {
        println!("DHCP: Ack: {ack:?}");
    }

    // The address is configured only once the server has acknowledged it. The
    // acknowledgment's parameters come first, the offer's fill what it leaves out.
    let mask_len = if let Some(subnet) = ack.subnet.or(offer.subnet) {
        (!u32::from_be_bytes(subnet)).leading_zeros()
    } else {
        println!("DHCP: No subnet mask");
        0
    };

    let new_ips = format!("{a}.{b}.{c}.{d}/{mask_len}\n");
    try_fmt!(
        set_iface_cfg_value(iface, "addr/set", &new_ips),
        "failed to set ip"
    );

    if verbose {
        let new_ip = try_fmt!(get_iface_cfg_value(iface, "addr/list"), "failed to get ip");
        println!("DHCP: New IP: {}", new_ip.trim());
    }

    if let Some(router) = ack.router.or(offer.router) {
        let default_route = format!(
            "default via {}.{}.{}.{}",
            router[0], router[1], router[2], router[3]
        );

        try_fmt!(
            set_cfg_value("route/add", &default_route),
            "failed to set default route"
        );

        if verbose {
            let new_router = try_fmt!(get_cfg_value("route/list"), "failed to get ip router");
            println!("DHCP: New Router: {}", new_router.trim());
        }
    } else {
        println!("DHCP: No router");
    }

    let dns = ack.dns.or(offer.dns).unwrap_or([9, 9, 9, 9]);
    let nameserver = format!("{}.{}.{}.{}", dns[0], dns[1], dns[2], dns[3]);

    try_fmt!(
        set_cfg_value("resolv/nameserver", &nameserver),
        "failed to set name server"
    );
    // User namespaces cannot open netcfg. Publish the same server through
    // the resolver's existing, world-readable configuration file.
    try_fmt!(
        std::fs::write("/etc/net/dns", &nameserver),
        "failed to publish name server"
    );

    if verbose {
        println!("DHCP: New DNS: {nameserver}");
    }

    Ok(())
}

fn main() {
    let mut verbose = false;
    let iface = "eth0";

    //TODO: parse iface from the args
    for arg in env::args().skip(1) {
        match arg.as_ref() {
            "-v" => verbose = true,
            _ => (),
        }
    }

    if let Err(err) = dhcp(iface, verbose) {
        eprintln!("dhcpd: {err}");
        process::exit(1);
    }
}

#[cfg(test)]
mod test {
    use super::MacAddr;

    #[test]
    fn from_str_test() {
        let mac = MacAddr {
            bytes: [0x01, 0x23, 0x45, 0x67, 0x89, 0xab],
        };
        let empty_mac = MacAddr::default();

        assert_eq!(mac, MacAddr::from_str("01:23:45:67:89:ab"));
        assert_eq!(mac, MacAddr::from_str("1:23:45:67:89:ab"));
        assert_eq!(mac, MacAddr::from_str("01:23:45:67:89:AB"));
        assert_eq!(mac, MacAddr::from_str("01-23-45-67-89-ab"));
        assert_eq!(empty_mac, MacAddr::from_str(""));
        assert_eq!(empty_mac, MacAddr::from_str("01:23:45:67:89"));
        assert_eq!(empty_mac, MacAddr::from_str("01:23:45:67:89:ab:cd"));
        assert_eq!(empty_mac, MacAddr::from_str("x1:23:45:67:89:ab"));
        assert_eq!(empty_mac, MacAddr::from_str("01:23-45-67-89-ab"));
        assert_eq!(empty_mac, MacAddr::from_str("01-23-45-67-89-ag"));
        assert_eq!(empty_mac, MacAddr::from_str("01.23.45.67.89.ab"));
        assert_eq!(empty_mac, MacAddr::from_str("01234-23-45-67-89-ab"));
        assert_eq!(empty_mac, MacAddr::from_str("01--23-45-67-89-ab"));
        assert_eq!(empty_mac, MacAddr::from_str("12"));
        assert_eq!(empty_mac, MacAddr::from_str("0:0:0:0:0:0"));

        assert_eq!(mac, MacAddr::from_str(&mac.to_string()));
        assert_eq!(empty_mac, MacAddr::from_str(&empty_mac.to_string()));
    }
}

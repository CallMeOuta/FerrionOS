use alloc::{string::ToString, vec::Vec};

use crate::{
    error::Errno,
    header::{sys_select, sys_socket},
    out::Out,
    platform::{Pal, PalSocket, Sys},
};

use crate::header::{
    bits_arpainet::htons,
    errno::{EADDRINUSE, EAGAIN, EINVAL, EIO, ETIMEDOUT, EWOULDBLOCK},
    netinet_in::{IPPROTO_UDP, in_addr, sockaddr_in},
    sys_socket::constants::{AF_INET, SOCK_DGRAM},
    time::{self, timespec},
};
use core::ffi::c_int;

use super::{
    dns::{Dns, DnsQuery},
    sys::get_dns_server,
};

pub type LookupHost = Vec<in_addr>;

pub fn lookup_host(host: &str) -> Result<LookupHost, Errno> {
    if let Some(host_direct_addr) = parse_ipv4_string(host) {
        // already an ip address
        return Ok(vec![in_addr {
            s_addr: host_direct_addr,
        }]);
    }
    if host.is_empty() {
        return Ok(vec![]);
    }

    let dns_string = get_dns_server()?;

    let dns_addr = parse_ipv4_string(&dns_string).ok_or(Errno(EINVAL))?;

    let response = query(dns_addr, host, 0x0001)?;
    let addrs: Vec<_> = response
        .answers
        .into_iter()
        .filter_map(|answer| {
            if answer.a_type == 0x0001 && answer.a_class == 0x0001 && answer.data.len() == 4 {
                let addr = in_addr {
                    s_addr: u32::from_ne_bytes([
                        answer.data[0],
                        answer.data[1],
                        answer.data[2],
                        answer.data[3],
                    ]),
                };
                Some(addr)
            } else {
                None
            }
        })
        .collect();

    Ok(addrs)
}

/// How long a lookup waits for its answer.
const QUERY_TIMEOUT_SECS: i64 = 7; // TODO: hardcoded, make it configurable?
/// The source ports a query is sent from, chosen at random: the dynamic range of RFC 6335.
const SOURCE_PORTS: core::ops::RangeInclusive<u16> = 49152..=65535;

/// Closes the query's socket however the query ends.
struct QuerySocket(c_int);

impl Drop for QuerySocket {
    fn drop(&mut self) {
        let _ = Sys::close(self.0);
    }
}

fn random_bytes<const N: usize>() -> Result<[u8; N], Errno> {
    let mut bytes = [0; N];
    if Sys::getrandom(&mut bytes, 0)? != N {
        return Err(Errno(EIO));
    }
    Ok(bytes)
}

fn monotonic_now() -> Result<timespec, Errno> {
    let mut now = timespec::default();
    Sys::clock_gettime(time::constants::CLOCK_MONOTONIC, Out::from_mut(&mut now))?;
    Ok(now)
}

/// Binds `sock` to a random source port, trying again while the port is taken.
fn bind_random_port(sock: c_int) -> Result<(), Errno> {
    let span = u32::from(SOURCE_PORTS.end() - SOURCE_PORTS.start()) + 1;
    let mut tries = 0;
    loop {
        let offset = u32::from(u16::from_ne_bytes(random_bytes()?)) % span;
        let port = *SOURCE_PORTS.start() + offset as u16;
        let mut local = [0_u8; size_of::<sockaddr_in>()];
        *plain::from_mut_bytes(&mut local).unwrap() = sockaddr_in {
            sin_family: u16::try_from(AF_INET).expect("AF_INET is positive and below u16::MAX"),
            sin_port: htons(port),
            sin_addr: in_addr { s_addr: 0 },
            ..Default::default()
        };
        match Sys::bind(sock, &local) {
            Err(Errno(EADDRINUSE)) if tries < 16 => tries += 1,
            result => return result,
        }
    }
}

/// Whether `reply` answers the query with ID `id` for `name`'s records of type
/// `q_type`: a response with the same ID and the same single question, the name
/// compared without regard to case (RFC 5452, section 9.1).
fn answers_query(reply: &Dns, id: u16, name: &str, q_type: u16) -> bool {
    let name = name.strip_suffix('.').unwrap_or(name);
    reply.transaction_id == id
        && reply.flags & 0x8000 != 0
        && reply.flags & 0x7800 == 0
        && match reply.queries.as_slice() {
            [question] => {
                question.name.eq_ignore_ascii_case(name)
                    && question.q_type == q_type
                    && question.q_class == 0x0001
            }
            _ => false,
        }
}

/// Sends a query for `name`'s records of type `q_type` to the server at `dns_addr`
/// from a random source port with a random ID, and returns the first reply that
/// answers it. The socket is connected to the server's address and port 53, so the
/// stack hands it only datagrams from there; any other reply is ignored until the
/// timeout.
fn query(dns_addr: u32, name: &str, q_type: u16) -> Result<Dns, Errno> {
    let id = u16::from_ne_bytes(random_bytes()?);
    let packet_data = Dns {
        transaction_id: id,
        flags: 0x0100,
        queries: vec![DnsQuery {
            name: name.to_string(),
            q_type,
            q_class: 0x0001,
        }],
        answers: vec![],
    }
    .compile();

    let mut dest = [0_u8; size_of::<sockaddr_in>()];
    *plain::from_mut_bytes(&mut dest).unwrap() = sockaddr_in {
        sin_family: u16::try_from(AF_INET).expect("AF_INET is positive and below u16::MAX"),
        sin_port: htons(53),
        sin_addr: in_addr { s_addr: dns_addr },
        ..Default::default()
    };

    let sock = QuerySocket(Sys::socket(AF_INET, SOCK_DGRAM, i32::from(IPPROTO_UDP))?);
    bind_random_port(sock.0)?;
    let _ = Sys::connect(sock.0, &dest)?;
    let deadline = monotonic_now()?.tv_sec + QUERY_TIMEOUT_SECS;
    Sys::sendto(sock.0, &packet_data, 0, None)?;

    let mut buf = vec![0u8; 65536];
    loop {
        let now = monotonic_now()?;
        if now.tv_sec >= deadline {
            return Err(Errno(ETIMEDOUT));
        }
        let timeout = sys_select::timeval {
            tv_sec: (deadline - now.tv_sec) as _,
            tv_usec: 0,
        };
        let _ = Sys::setsockopt(
            sock.0,
            sys_socket::constants::SOL_SOCKET,
            sys_socket::constants::SO_RCVTIMEO,
            unsafe {
                core::slice::from_raw_parts(
                    (&raw const timeout).cast::<u8>(),
                    core::mem::size_of::<sys_select::timeval>(),
                )
            },
        );

        let count = match Sys::recvfrom(sock.0, Out::from_mut(&mut buf), 0, None) {
            Ok((count, _)) => count,
            Err(Errno(EAGAIN | EWOULDBLOCK | ETIMEDOUT)) => return Err(Errno(ETIMEDOUT)),
            Err(err) => return Err(err),
        };
        if let Ok(reply) = Dns::parse(&buf[..count])
            && answers_query(&reply, id, name, q_type)
        {
            return Ok(reply);
        }
    }
}

pub fn lookup_addr(addr: &in_addr) -> Result<Vec<Vec<u8>>, Errno> {
    if addr.s_addr == 0 {
        return Ok(vec![]);
    }
    let dns_string = get_dns_server()?;

    if let Some(dns_addr) = parse_ipv4_string(&dns_string) {
        let addr: [u8; 4] = addr.s_addr.to_ne_bytes();
        // Address intentionally backwards for reverse lookup
        let name = format!(
            "{}.{}.{}.{}.in-addr.arpa",
            addr[3], addr[2], addr[1], addr[0]
        );

        let response = query(dns_addr, &name, 0x000C)?;
        let names = response
            .answers
            .into_iter()
            .filter_map(|answer| {
                if answer.a_type == 0x000C && answer.a_class == 0x0001 {
                    // answer.data is encoded kinda weird.
                    // Basically length-prefixed strings for each
                    // subsection of the domain.
                    // We need to parse this to insert periods where
                    // they belong (ie at the end of each string)
                    Some(parse_revdns_answer(&answer.data))
                } else {
                    None
                }
            })
            .collect();
        Ok(names)
    } else {
        Err(Errno(EINVAL))
    }
}

fn parse_revdns_answer(data: &[u8]) -> Vec<u8> {
    if data.is_empty() || data[0] == 0 {
        return vec![0];
    }

    let mut cursor = 0;
    let mut index = 0;
    // First byte is a length; discard
    let mut output = data[1..].to_vec();
    let length = data.len() - 1;
    while index < length {
        let offset = data[index] as usize;
        // CVE-2024-21342
        if offset > length {
            return vec![0];
        }
        index = cursor + offset + 1;
        // First byte was skipped so index is one less
        output[index - 1] = b'.';
        cursor = index;
    }
    // Response is NUL terminated so we must preserve that for C
    match output.last_mut() {
        Some(nul) => *nul = b'\0',
        // XXX: Likely unreachable
        None => {
            debug_assert!(output.is_empty());
            output = vec![0];
        }
    }
    output
}

pub fn parse_ipv4_string(ip_string: &str) -> Option<u32> {
    let mut octets = ip_string.trim().split('.');
    let mut addr = [0u8; 4];
    for octet in &mut addr {
        let text = octets.next()?;
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        *octet = text.parse().ok()?;
    }
    if octets.next().is_some() {
        return None;
    }
    Some(u32::from_ne_bytes(addr))
}

#[cfg(test)]
mod tests {
    use alloc::str;
    use core::ffi::CStr;

    use super::parse_revdns_answer;

    // Actual response from a query
    const DNS_GOOGLE: &[u8] = &[3, 100, 110, 115, 6, 103, 111, 111, 103, 108, 101, 0];
    const EXPECTED_DNS_GOOGLE: &str = "dns.google\0";
    const EXPECTED_DNS_GOOGLE_RT: &str = "dns.google";

    // Fake response that has numbers within the name (e.g. like CDNs)
    const FAKE_WITH_NUMS: &[u8] = &[
        14, 102, 97, 107, 101, 45, 49, 48, 48, 45, 50, 45, 51, 45, 52, 8, 102, 111, 111, 98, 97,
        114, 50, 52, 4, 102, 97, 107, 101, 0,
    ];
    const EXPECTED_FAKE_RESPONSE: &str = "fake-100-2-3-4.foobar24.fake\0";
    const EXPECTED_FAKE_RESPONSE_RT: &str = "fake-100-2-3-4.foobar24.fake";

    const EMPTY_RESPONSE: &[u8] = &[0];
    const EXPECTED_EMPTY_RESPONSE: &str = "\0";
    const EXPECTED_EMPTY_RESPONSE_RT: &str = "";

    #[test]
    fn dns_response_dns_google() {
        let response = parse_revdns_answer(DNS_GOOGLE);
        assert_eq!(
            0,
            *response.last().unwrap(),
            "Response should end with a NUL byte"
        );

        let response_str = str::from_utf8(&response)
            .expect("Response is valid UTF-8; parsing shouldn't change that");
        assert_eq!(EXPECTED_DNS_GOOGLE, response_str);

        let response_cstr = CStr::from_bytes_with_nul(&response)
            .expect("Parsed response should have only one NUL byte");
        let response_cstr_str = response_cstr
            .to_str()
            .expect("Valid UTF-8 bytes to CStr to Rust str should be valid");
        assert_eq!(EXPECTED_DNS_GOOGLE_RT, response_cstr_str);
    }

    #[test]
    fn dns_response_fake_with_nums() {
        let response = parse_revdns_answer(FAKE_WITH_NUMS);
        assert_eq!(
            0,
            *response.last().unwrap(),
            "Response should end with a NUL byte"
        );

        let response_str = str::from_utf8(&response)
            .expect("Response is valid UTF-8; parsing shouldn't change that");
        assert_eq!(EXPECTED_FAKE_RESPONSE, response_str);

        let response_cstr = CStr::from_bytes_with_nul(&response)
            .expect("Parsed response should have only one NUL byte");
        let response_cstr_str = response_cstr
            .to_str()
            .expect("Valid UTF-8 bytes to CStr to Rust str should be valid");
        assert_eq!(EXPECTED_FAKE_RESPONSE_RT, response_cstr_str);
    }

    #[test]
    fn dns_response_empty() {
        let response = parse_revdns_answer(EMPTY_RESPONSE);
        assert_eq!(
            0,
            *response.last().unwrap(),
            "Response should end with a NUL byte"
        );

        let response_str = str::from_utf8(&response)
            .expect("Response is valid UTF-8; parsing shouldn't change that");
        assert_eq!(EXPECTED_EMPTY_RESPONSE, response_str);

        let response_cstr = CStr::from_bytes_with_nul(&response)
            .expect("Parsed response should have only one NUL byte");
        let response_cstr_str = response_cstr
            .to_str()
            .expect("Valid UTF-8 bytes to CStr to Rust str should be valid");
        assert_eq!(EXPECTED_EMPTY_RESPONSE_RT, response_cstr_str);
    }
}

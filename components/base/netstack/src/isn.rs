//! TCP initial sequence numbers as RFC 6528 describes them: a clock that ticks every 4
//! microseconds plus a keyed hash (SipHash-2-4) of the connection's addresses and ports.
//! Numbers of one connection tell nothing about those of another, and a connection reopened
//! with the same addresses and ports starts further along than before.

use std::io;
use std::sync::OnceLock;

use smoltcp::time::Instant;
use smoltcp::wire::{IpAddress, IpEndpoint};

static KEY: OnceLock<[u64; 2]> = OnceLock::new();

/// Reads the hash key from the random scheme; call before the interface opens a connection.
pub fn init() -> io::Result<()> {
    let key = [crate::random::u64()?, crate::random::u64()?];
    KEY.set(key)
        .map_err(|_| io::Error::other("the sequence number key is already set"))
}

/// The generator the interface calls for each connection it opens or accepts.
pub fn initial_seq_no(local: IpEndpoint, remote: IpEndpoint, now: Instant) -> u32 {
    let key = KEY
        .get()
        .expect("isn::init runs before the interface is built");
    seq_no(*key, local, remote, now)
}

fn seq_no(key: [u64; 2], local: IpEndpoint, remote: IpEndpoint, now: Instant) -> u32 {
    let mut tuple = Vec::with_capacity(36);
    for endpoint in [local, remote] {
        match endpoint.addr {
            IpAddress::Ipv4(addr) => tuple.extend_from_slice(&addr.octets()),
        }
        tuple.extend_from_slice(&endpoint.port.to_be_bytes());
    }
    let ticks = (now.total_micros() / 4) as u32;
    ticks.wrapping_add(siphash24(key, &tuple) as u32)
}

fn siphash24(key: [u64; 2], data: &[u8]) -> u64 {
    let mut v = [
        key[0] ^ 0x736f_6d65_7073_6575,
        key[1] ^ 0x646f_7261_6e64_6f6d,
        key[0] ^ 0x6c79_6765_6e65_7261,
        key[1] ^ 0x7465_6462_7974_6573,
    ];
    let round = |v: &mut [u64; 4]| {
        v[0] = v[0].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(13) ^ v[0];
        v[0] = v[0].rotate_left(32);
        v[2] = v[2].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(16) ^ v[2];
        v[0] = v[0].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(21) ^ v[0];
        v[2] = v[2].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(17) ^ v[2];
        v[2] = v[2].rotate_left(32);
    };
    let mut compress = |m: u64| {
        v[3] ^= m;
        round(&mut v);
        round(&mut v);
        v[0] ^= m;
    };
    let words = data.chunks_exact(8);
    let mut last = (data.len() as u64) << 56;
    for (i, byte) in words.remainder().iter().enumerate() {
        last |= u64::from(*byte) << (8 * i);
    }
    for word in words {
        compress(u64::from_le_bytes(word.try_into().unwrap()));
    }
    compress(last);
    v[2] ^= 0xff;
    for _ in 0..4 {
        round(&mut v);
    }
    v[0] ^ v[1] ^ v[2] ^ v[3]
}

//! `ifaddrs.h` implementation.
//!
//! Non-POSIX, see <https://www.man7.org/linux/man-pages/man3/getifaddrs.3.html>.

use core::{mem, ptr, str};

use crate::{
    header::{
        errno, fcntl,
        net_if::{IF_NAMESIZE, IFF_BROADCAST, IFF_LOOPBACK, IFF_RUNNING, IFF_UP},
        netinet_in::{in_addr, sockaddr_in},
        stdlib,
        sys_socket::{constants::AF_INET, sockaddr},
    },
    platform::{
        self, Pal, Sys,
        types::{c_char, c_int, c_uint, c_void},
    },
};

const MAX_IFADDRS_ENTRIES: usize = 16;
const MAX_NETCFG_BYTES: usize = 1024;
const MAX_NETCFG_READS: usize = 32;

struct RawFdGuard(c_int);

impl Drop for RawFdGuard {
    fn drop(&mut self) {
        let _ = Sys::close(self.0);
    }
}

#[repr(C)]
struct IfaddrsEntry {
    ifa: ifaddrs,
    addr: sockaddr_in,
    netmask: sockaddr_in,
    broadaddr: sockaddr_in,
    name: [c_char; IF_NAMESIZE],
}

fn ipv4_sockaddr(octets: [u8; 4]) -> sockaddr_in {
    sockaddr_in {
        sin_family: AF_INET as _,
        sin_port: 0,
        sin_addr: in_addr {
            s_addr: u32::from_ne_bytes(octets),
        },
        sin_zero: [0; 8],
    }
}

fn prefix_mask(prefix: u8) -> [u8; 4] {
    let mask = if prefix == 0 {
        0u32
    } else {
        !0u32 << (32 - u32::from(prefix))
    };
    mask.to_be_bytes()
}

fn broadcast_addr(addr: [u8; 4], mask: [u8; 4]) -> [u8; 4] {
    [
        addr[0] | !mask[0],
        addr[1] | !mask[1],
        addr[2] | !mask[2],
        addr[3] | !mask[3],
    ]
}

fn parse_ipv4_cidr(line: &str) -> Option<([u8; 4], u8)> {
    let (ip_str, prefix_str) = line.split_once('/')?;
    let mut parts = ip_str.split('.');
    let mut octets = [0u8; 4];
    for slot in &mut octets {
        let piece = parts.next()?;
        if piece.is_empty() || !piece.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = piece.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    if prefix_str.is_empty() || !prefix_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let prefix: u8 = prefix_str.parse().ok()?;
    if prefix > 32 {
        return None;
    }
    Some((octets, prefix))
}

fn alloc_entry(
    name: &str,
    flags: c_uint,
    addr: [u8; 4],
    mask: [u8; 4],
    broad: Option<[u8; 4]>,
) -> Option<*mut ifaddrs> {
    let name_bytes = name.as_bytes();
    if name_bytes.len() >= IF_NAMESIZE {
        return None;
    }
    let raw = unsafe { stdlib::malloc(mem::size_of::<IfaddrsEntry>()) }.cast::<IfaddrsEntry>();
    if raw.is_null() {
        return None;
    }
    unsafe {
        ptr::write_bytes(raw, 0, 1);
        let entry = &mut *raw;
        for (idx, &byte) in name_bytes.iter().enumerate() {
            entry.name[idx] = byte as c_char;
        }
        entry.addr = ipv4_sockaddr(addr);
        entry.netmask = ipv4_sockaddr(mask);
        let ifu_broad = match broad {
            Some(b) => {
                entry.broadaddr = ipv4_sockaddr(b);
                (&raw mut entry.broadaddr).cast::<sockaddr>()
            }
            None => ptr::null_mut(),
        };
        entry.ifa = ifaddrs {
            ifa_next: ptr::null_mut(),
            ifa_name: entry.name.as_mut_ptr(),
            ifa_flags: flags,
            ifa_addr: (&raw mut entry.addr).cast::<sockaddr>(),
            ifa_netmask: (&raw mut entry.netmask).cast::<sockaddr>(),
            ifa_ifu: ifaddrs_ifa_ifu {
                ifu_broadaddr: ifu_broad,
            },
            ifa_data: ptr::null_mut(),
        };
        Some(&raw mut entry.ifa)
    }
}

fn has_network_scheme() -> bool {
    if let Ok(fd) = Sys::open(c"/scheme/udp".into(), fcntl::O_RDONLY | fcntl::O_CLOEXEC, 0) {
        let _guard = RawFdGuard(fd);
        return true;
    }
    if let Ok(fd) = Sys::open(c"/scheme/tcp".into(), fcntl::O_RDONLY | fcntl::O_CLOEXEC, 0) {
        let _guard = RawFdGuard(fd);
        return true;
    }
    false
}

fn read_eth0_addr_list(buf: &mut [u8; MAX_NETCFG_BYTES + 1]) -> Result<Option<usize>, c_int> {
    let Ok(fd) = Sys::open(
        c"/scheme/netcfg/ifaces/eth0/addr/list".into(),
        fcntl::O_RDONLY | fcntl::O_CLOEXEC,
        0,
    ) else {
        return Ok(None);
    };
    let _guard = RawFdGuard(fd);
    let mut total = 0usize;
    for _ in 0..MAX_NETCFG_READS {
        let n = match Sys::read(fd, &mut buf[total..]) {
            Ok(0) => return Ok(Some(total)),
            Ok(n) => n,
            Err(_) => return Err(errno::EIO),
        };
        total = total.saturating_add(n);
        if total > MAX_NETCFG_BYTES {
            return Err(errno::EIO);
        }
    }
    Err(errno::EIO)
}

/// Either the broadcast address associated with `ifa_addr` (if applicable
/// for the address family) or the destination address of the
/// point-to-point interface.
#[repr(C)]
union ifaddrs_ifa_ifu {
    /// Broadcast address of interface.
    ifu_broadaddr: *mut sockaddr,
    /// Point-to-point destination address.
    ifu_dstaddr: *mut sockaddr,
}

/// An entry in a linked list describing the network interfaces of the local
/// system.
#[repr(C)]
pub struct ifaddrs {
    /// Next item in list.
    ifa_next: *mut ifaddrs,
    /// Name of interface.
    ifa_name: *mut c_char,
    /// Flags from `SIOCGIFFLAGS`.
    ifa_flags: c_uint,
    /// Address of interface.
    ifa_addr: *mut sockaddr,
    /// Netmask of interface.
    ifa_netmask: *mut sockaddr,
    /// Depends on the bit `IFF_BROADCAST` or `IFF_POINTOPOINT` being set in
    /// `ifa_flags`. The bits are mutually exclusive.
    ifa_ifu: ifaddrs_ifa_ifu,
    /// Address-specific data.
    ifa_data: *mut c_void,
}

/// Frees the dynamically allocated memory used by `ifa`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn freeifaddrs(mut ifa: *mut ifaddrs) {
    while !ifa.is_null() {
        let next = unsafe { (*ifa).ifa_next };
        unsafe { stdlib::free(ifa.cast()) };
        ifa = next;
    }
}

/// Creates a linked list of structures describing the network interfaces of
/// the local system, and stores the address of the first item of the list
/// in `ifap`.
///
/// The data returned by `getifaddrs()` is dynamically allocated and should
/// be freed using `freeifaddrs()` when no longer needed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getifaddrs(ifap: *mut *mut ifaddrs) -> c_int {
    if ifap.is_null() {
        platform::ERRNO.set(errno::EINVAL);
        return -1;
    }
    unsafe {
        *ifap = ptr::null_mut();
    }

    let mut head: *mut ifaddrs = ptr::null_mut();
    let mut tail: *mut ifaddrs = ptr::null_mut();
    let mut count = 0usize;

    let mut push_node = |node: *mut ifaddrs| -> Result<(), c_int> {
        if count >= MAX_IFADDRS_ENTRIES {
            unsafe {
                freeifaddrs(node);
            }
            return Err(errno::ENOMEM);
        }
        if head.is_null() {
            head = node;
            tail = node;
        } else {
            unsafe {
                (*tail).ifa_next = node;
            }
            tail = node;
        }
        count += 1;
        Ok(())
    };

    let lo_flags = if has_network_scheme() {
        (IFF_UP | IFF_LOOPBACK | IFF_RUNNING) as c_uint
    } else {
        IFF_LOOPBACK as c_uint
    };
    let Some(lo_node) = alloc_entry("lo", lo_flags, [127, 0, 0, 1], [255, 0, 0, 0], None) else {
        platform::ERRNO.set(errno::ENOMEM);
        return -1;
    };
    if let Err(err) = push_node(lo_node) {
        unsafe {
            freeifaddrs(head);
        }
        platform::ERRNO.set(err);
        return -1;
    }

    let mut buf = [0u8; MAX_NETCFG_BYTES + 1];
    match read_eth0_addr_list(&mut buf) {
        Err(err) => {
            unsafe {
                freeifaddrs(head);
            }
            platform::ERRNO.set(err);
            return -1;
        }
        Ok(None) => {}
        Ok(Some(len)) => {
            let Ok(text) = str::from_utf8(&buf[..len]) else {
                unsafe {
                    freeifaddrs(head);
                }
                platform::ERRNO.set(errno::EIO);
                return -1;
            };
            let trimmed = text.trim();
            if !trimmed.is_empty() && trimmed != "Not configured" && trimmed != "Device not found" {
                for raw_line in trimmed.lines() {
                    let line = raw_line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let Some((addr, prefix)) = parse_ipv4_cidr(line) else {
                        unsafe {
                            freeifaddrs(head);
                        }
                        platform::ERRNO.set(errno::EIO);
                        return -1;
                    };
                    let mask = prefix_mask(prefix);
                    let broad = broadcast_addr(addr, mask);
                    let eth_flags = (IFF_UP | IFF_BROADCAST | IFF_RUNNING) as c_uint;
                    let Some(eth_node) = alloc_entry("eth0", eth_flags, addr, mask, Some(broad))
                    else {
                        unsafe {
                            freeifaddrs(head);
                        }
                        platform::ERRNO.set(errno::ENOMEM);
                        return -1;
                    };
                    if let Err(err) = push_node(eth_node) {
                        unsafe {
                            freeifaddrs(head);
                        }
                        platform::ERRNO.set(err);
                        return -1;
                    }
                }
            }
        }
    }

    unsafe {
        *ifap = head;
    }
    0
}

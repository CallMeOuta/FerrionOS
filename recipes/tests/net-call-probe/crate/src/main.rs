use std::ffi::CString;
use std::fs;
use std::io::{self, Read, Write};
use std::mem;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream, UdpSocket};
use std::os::fd::AsRawFd;
use std::process::ExitCode;
use std::time::Duration;

extern "C" {
    fn redox_sys_call_v0(
        fd: usize,
        payload: *mut u8,
        payload_len: usize,
        flags: usize,
        metadata: *const u64,
        metadata_len: usize,
    ) -> usize;
    fn redox_fpath_v1(fd: usize, dst_base: *mut u8, dst_len: usize) -> usize;
}

const CALL_READ_WRITE: usize = (1 << 9) | (1 << 10);
const SOCKET_CALL_RECVMSG: u64 = 5;
const WORD: usize = mem::size_of::<usize>();
const DATA: u8 = 7;
const TIMEOUT: Duration = Duration::from_secs(3);
const UDP_PORT: u16 = 47_001;
const TCP_PORT: u16 = 47_011;
const LIVE_PORT: u16 = 47_021;

fn errno_result(raw: usize) -> Result<usize, i32> {
    if raw > usize::MAX - 4096 {
        Err(raw.wrapping_neg() as i32)
    } else {
        Ok(raw)
    }
}

fn recvmsg_call(fd: i32, buf: &mut [u8]) -> Result<usize, i32> {
    let metadata = [SOCKET_CALL_RECVMSG, 0];
    errno_result(unsafe {
        redox_sys_call_v0(
            fd as usize,
            buf.as_mut_ptr(),
            buf.len(),
            CALL_READ_WRITE,
            metadata.as_ptr(),
            metadata.len(),
        )
    })
}

fn request(sizes: [usize; 3], len: usize) -> Vec<u8> {
    let mut buf = vec![0; len.max(3 * WORD)];
    for (index, size) in sizes.iter().enumerate() {
        buf[index * WORD..(index + 1) * WORD].copy_from_slice(&size.to_le_bytes());
    }
    buf.truncate(len);
    buf
}

fn describe(result: Result<usize, i32>) -> String {
    match result {
        Ok(count) => format!("returned {count}"),
        Err(errno) => format!("error {errno} ({})", io::Error::from_raw_os_error(errno)),
    }
}

struct Report {
    failed: bool,
}

impl Report {
    fn line(&mut self, check: &str, pass: bool, detail: impl AsRef<str>) {
        let verdict = if pass { "pass" } else { "FAIL" };
        println!("net-call-probe: {verdict} {check}: {}", detail.as_ref());
        self.failed |= !pass;
    }

    fn einval(&mut self, check: &str, result: Result<usize, i32>) {
        self.line(check, result == Err(libc::EINVAL), describe(result));
    }

    fn setup<T>(&mut self, check: &str, result: io::Result<T>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(err) => {
                self.line(check, false, format!("setup failed: {err}"));
                None
            }
        }
    }
}

fn localhost(port: u16) -> SocketAddrV4 {
    SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)
}

fn wait_readable(fd: i32) -> io::Result<()> {
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    match unsafe { libc::poll(&mut poll, 1, TIMEOUT.as_millis() as i32) } {
        1 => Ok(()),
        0 => Err(io::Error::new(io::ErrorKind::TimedOut, "nothing arrived")),
        _ => Err(io::Error::last_os_error()),
    }
}

fn udp_with_datagram(sender: &UdpSocket) -> io::Result<UdpSocket> {
    let receiver = UdpSocket::bind(localhost(UDP_PORT))?;
    sender.send_to(&[DATA; 100], localhost(UDP_PORT))?;
    wait_readable(receiver.as_raw_fd())?;
    Ok(receiver)
}

fn udp_checks(report: &mut Report) {
    let Some(sender) = report.setup("udp", UdpSocket::bind(localhost(UDP_PORT + 1))) else {
        return;
    };
    let Some(receiver) = report.setup("udp", udp_with_datagram(&sender)) else {
        return;
    };
    let fd = receiver.as_raw_fd();
    report.einval("udp recvmsg into 8 bytes", recvmsg_call(fd, &mut [0; 8]));
    report.einval(
        "udp recvmsg with sizes past usize::MAX",
        recvmsg_call(fd, &mut request([usize::MAX - 8, 16, 0], 3 * WORD)),
    );
    report.einval(
        "udp recvmsg with 1 byte for the sender's name",
        recvmsg_call(fd, &mut request([1, 100, 0], 3 * WORD + 101)),
    );

    let mut byte = [0; 1];
    let result = receiver.recv_from(&mut byte);
    report.line(
        "udp recvfrom into 1 byte",
        matches!(result, Ok((1, _))) && byte == [DATA],
        format!("{result:?}, byte {}", byte[0]),
    );
    byte = [0];
    let result = sender
        .send_to(&[DATA; 100], localhost(UDP_PORT))
        .and_then(|_| wait_readable(fd))
        .and_then(|()| receiver.recv(&mut byte));
    report.line(
        "udp read into 1 byte",
        matches!(result, Ok(1)) && byte == [DATA],
        format!("{result:?}"),
    );

    let result = sender.send_to(&vec![DATA; 200_000], localhost(UDP_PORT));
    report.line(
        "udp datagram of 200,000 bytes",
        result.is_err(),
        format!("{result:?}"),
    );
    let result = sender.send_to(
        &[DATA; 10],
        SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, UDP_PORT),
    );
    report.line(
        "udp datagram to 0.0.0.0",
        result.is_err(),
        format!("{result:?}"),
    );
}

fn tcp_checks(report: &mut Report) {
    let setup = || -> io::Result<(TcpListener, TcpStream, TcpStream)> {
        let listener = TcpListener::bind(localhost(TCP_PORT))?;
        let mut client = TcpStream::connect(localhost(TCP_PORT))?;
        let (server, _) = listener.accept()?;
        client.write_all(&[DATA; 100])?;
        wait_readable(server.as_raw_fd())?;
        Ok((listener, client, server))
    };
    let Some((_listener, _client, mut server)) = report.setup("tcp", setup()) else {
        return;
    };
    let fd = server.as_raw_fd();
    report.einval("tcp recvmsg into 8 bytes", recvmsg_call(fd, &mut [0; 8]));
    report.einval(
        "tcp recvmsg with no room for the peer's name",
        recvmsg_call(fd, &mut request([0, 0, 0], 3 * WORD)),
    );
    report.einval(
        "tcp recvmsg with sizes past usize::MAX",
        recvmsg_call(fd, &mut request([0, usize::MAX - 8, 16], 3 * WORD)),
    );
    let mut data = [0; 100];
    let result = server
        .set_read_timeout(Some(TIMEOUT))
        .and_then(|()| server.read_exact(&mut data));
    report.line(
        "tcp data intact after the refused calls",
        result.is_ok() && data == [DATA; 100],
        format!("{result:?}"),
    );
}

fn fpath_check(report: &mut Report) {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        report.line(
            "fpath",
            false,
            format!("setup failed: {}", io::Error::last_os_error()),
        );
        return;
    }
    let mut buf = [0u8; 4];
    let result = errno_result(unsafe { redox_fpath_v1(fd as usize, buf.as_mut_ptr(), buf.len()) });
    unsafe { libc::close(fd) };
    report.line(
        "fpath of a new tcp socket into 4 bytes",
        result == Ok(4),
        format!("{}, {:?}", describe(result), String::from_utf8_lossy(&buf)),
    );
}

fn stack_answers(report: &mut Report) {
    let udp = || -> io::Result<usize> {
        let a = UdpSocket::bind(localhost(LIVE_PORT))?;
        let b = UdpSocket::bind(localhost(LIVE_PORT + 1))?;
        b.send_to(b"still here", localhost(LIVE_PORT))?;
        a.set_read_timeout(Some(TIMEOUT))?;
        let mut buf = [0; 32];
        Ok(a.recv_from(&mut buf)?.0)
    };
    let result = udp();
    report.line(
        "stack answers udp",
        matches!(result, Ok(10)),
        format!("{result:?}"),
    );
    let tcp = || -> io::Result<usize> {
        let listener = TcpListener::bind(localhost(LIVE_PORT + 2))?;
        let mut client = TcpStream::connect_timeout(&localhost(LIVE_PORT + 2).into(), TIMEOUT)?;
        let (mut server, _) = listener.accept()?;
        client.write_all(b"still here")?;
        server.set_read_timeout(Some(TIMEOUT))?;
        let mut buf = [0; 10];
        server.read_exact(&mut buf)?;
        Ok(buf.len())
    };
    let result = tcp();
    report.line(
        "stack answers tcp",
        matches!(result, Ok(10)),
        format!("{result:?}"),
    );
}

fn mac_checks(report: &mut Report) {
    let names: Vec<String> = match fs::read_dir("/scheme") {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| name.starts_with("network"))
            .collect(),
        Err(err) => {
            report.line("mac", false, format!("cannot list /scheme: {err}"));
            return;
        }
    };
    if names.is_empty() {
        report.line("mac", false, "no network scheme in /scheme");
    }
    for name in names {
        let path = CString::new(format!("/scheme/{name}/mac")).unwrap();
        let pread = |offset: i64| -> Result<(usize, [u8; 8]), i32> {
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY) };
            if fd < 0 {
                return Err(io::Error::last_os_error().raw_os_error().unwrap_or(0));
            }
            let mut buf = [0u8; 8];
            let count = unsafe { libc::pread(fd, buf.as_mut_ptr().cast(), 6, offset) };
            let result = if count < 0 {
                Err(io::Error::last_os_error().raw_os_error().unwrap_or(0))
            } else {
                Ok((count as usize, buf))
            };
            unsafe { libc::close(fd) };
            result
        };
        let first = pread(0);
        for (offset, expected) in [
            (6, Ok(0)),
            (7, Err(libc::EINVAL)),
            (4096, Err(libc::EINVAL)),
        ] {
            let result = pread(offset).map(|(count, _)| count);
            report.line(
                &format!("{name} mac read at {offset}"),
                result == expected,
                describe(result),
            );
        }
        let again = pread(0);
        let whole = |result: &Result<(usize, [u8; 8]), i32>| match result {
            Ok((6, buf)) => Some(
                buf[..6]
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(":"),
            ),
            _ => None,
        };
        report.line(
            &format!("{name} mac read whole before and after"),
            whole(&first).is_some() && whole(&first) == whole(&again),
            format!("{:?} then {:?}", whole(&first), whole(&again)),
        );
    }
}

fn main() -> ExitCode {
    let mut report = Report { failed: false };
    match std::env::args().nth(1).as_deref() {
        None => {
            udp_checks(&mut report);
            tcp_checks(&mut report);
            fpath_check(&mut report);
            stack_answers(&mut report);
        }
        Some("mac") => mac_checks(&mut report),
        Some(other) => {
            eprintln!("net-call-probe: unknown argument {other:?}; use none, or `mac` as root");
            return ExitCode::from(2);
        }
    }
    if report.failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

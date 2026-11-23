use std::ffi::{CStr, CString};
use std::fs;
use std::io;
use std::mem;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::process::ExitCode;
use std::ptr;
use std::thread;
use std::time::{Duration, Instant};

const MAX_ARGS: usize = 64;
const MAX_COUNT: usize = 100_000;
const P999_MIN_SAMPLES: usize = 10_000;
const MAX_BYTES: usize = 65_507;
const MAX_HTTP_BYTES: usize = 1 << 20;
const MAX_HTTP_READS: usize = 4096;
const MAX_ECHO_STEPS: usize = 4096;
const MAX_CONNS: usize = 1024;
const MAX_INTERVAL_MS: u64 = 10_000;
const MAX_TIMEOUT_MS: u64 = 60_000;
const MAX_ADDRS: usize = 64;
const DEFAULT_TIMEOUT_MS: u64 = 5_000;

struct OwnedFd(i32);

impl Drop for OwnedFd {
    fn drop(&mut self) {
        if self.0 >= 0 {
            unsafe {
                libc::close(self.0);
            }
        }
    }
}

fn json_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn last_errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn errno_text(code: i32) -> String {
    io::Error::from_raw_os_error(code).to_string()
}

fn errno_name(code: i32) -> &'static str {
    match code {
        0 => "OK",
        libc::EACCES => "EACCES",
        libc::EADDRINUSE => "EADDRINUSE",
        libc::EADDRNOTAVAIL => "EADDRNOTAVAIL",
        libc::EAFNOSUPPORT => "EAFNOSUPPORT",
        libc::EAGAIN => "EAGAIN",
        libc::EBADF => "EBADF",
        libc::ECONNREFUSED => "ECONNREFUSED",
        libc::ECONNRESET => "ECONNRESET",
        libc::EHOSTUNREACH => "EHOSTUNREACH",
        libc::EINPROGRESS => "EINPROGRESS",
        libc::EINTR => "EINTR",
        libc::EINVAL => "EINVAL",
        libc::EMSGSIZE => "EMSGSIZE",
        libc::ENETDOWN => "ENETDOWN",
        libc::ENETUNREACH => "ENETUNREACH",
        libc::ENOBUFS => "ENOBUFS",
        libc::ENODEV => "ENODEV",
        libc::ENOENT => "ENOENT",
        libc::ENOPROTOOPT => "ENOPROTOOPT",
        libc::ENOSYS => "ENOSYS",
        libc::ENOTCONN => "ENOTCONN",
        libc::EOPNOTSUPP => "EOPNOTSUPP",
        libc::EOVERFLOW => "EOVERFLOW",
        libc::EPERM => "EPERM",
        libc::EPIPE => "EPIPE",
        libc::ETIMEDOUT => "ETIMEDOUT",
        _ => "OTHER",
    }
}

fn print_call_error(op: &str, call: &str, err: i32) {
    println!(
        "{{\"ok\":false,\"op\":{},\"call\":{},\"errno\":{err},\"error\":{},\"text\":{}}}",
        json_escape(op),
        json_escape(call),
        json_escape(errno_name(err)),
        json_escape(&errno_text(err)),
    );
}

fn to_sockaddr(addr: SocketAddrV4) -> libc::sockaddr_in {
    let mut sa: libc::sockaddr_in = unsafe { mem::zeroed() };
    sa.sin_family = libc::AF_INET as libc::sa_family_t;
    sa.sin_port = addr.port().to_be();
    sa.sin_addr = libc::in_addr {
        s_addr: u32::from_ne_bytes(addr.ip().octets()),
    };
    sa
}

fn from_sockaddr(sa: &libc::sockaddr_in) -> SocketAddrV4 {
    let octets = sa.sin_addr.s_addr.to_ne_bytes();
    SocketAddrV4::new(Ipv4Addr::from(octets), u16::from_be(sa.sin_port))
}

fn parse_addr(text: &str) -> Result<SocketAddrV4, String> {
    text.parse::<SocketAddrV4>()
        .map_err(|_| format!("invalid IPv4 host:port {text:?}"))
}

fn parse_usize_bounded(text: &str, name: &str, min: usize, max: usize) -> Result<usize, String> {
    let val = text
        .parse::<usize>()
        .map_err(|_| format!("invalid integer for {name}: {text:?}"))?;
    if val < min || val > max {
        return Err(format!("{name} must be in {min}..={max}, got {val}"));
    }
    Ok(val)
}

fn parse_u64_bounded(text: &str, name: &str, min: u64, max: u64) -> Result<u64, String> {
    let val = text
        .parse::<u64>()
        .map_err(|_| format!("invalid integer for {name}: {text:?}"))?;
    if val < min || val > max {
        return Err(format!("{name} must be in {min}..={max}, got {val}"));
    }
    Ok(val)
}

fn poll_fd(fd: i32, events: i16, timeout_ms: u64) -> Result<i16, i32> {
    let mut pfd = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    let timeout = i32::try_from(timeout_ms.min(MAX_TIMEOUT_MS)).unwrap_or(i32::MAX);
    let rc = unsafe { libc::poll(&mut pfd, 1, timeout) };
    if rc < 0 {
        Err(last_errno())
    } else if rc == 0 {
        Err(libc::ETIMEDOUT)
    } else {
        Ok(pfd.revents)
    }
}

fn set_nonblock(fd: i32, enable: bool) -> Result<(), i32> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
    if flags < 0 {
        return Err(last_errno());
    }
    let next = if enable {
        flags | libc::O_NONBLOCK
    } else {
        flags & !libc::O_NONBLOCK
    };
    if unsafe { libc::fcntl(fd, libc::F_SETFL, next) } < 0 {
        return Err(last_errno());
    }
    Ok(())
}

fn set_sockopt_int(fd: i32, level: i32, name: i32, value: i32) -> i32 {
    let rc = unsafe {
        libc::setsockopt(
            fd,
            level,
            name,
            (&value as *const i32).cast(),
            mem::size_of::<i32>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        last_errno()
    } else {
        0
    }
}

fn get_sockopt_int(fd: i32, level: i32, name: i32) -> Result<i32, i32> {
    let mut value: i32 = 0;
    let mut len = mem::size_of::<i32>() as libc::socklen_t;
    let rc =
        unsafe { libc::getsockopt(fd, level, name, (&mut value as *mut i32).cast(), &mut len) };
    if rc < 0 {
        Err(last_errno())
    } else {
        Ok(value)
    }
}

fn set_sockopt_timeval(fd: i32, name: i32, ms: u64) -> i32 {
    let tv = libc::timeval {
        tv_sec: (ms / 1000) as libc::time_t,
        tv_usec: ((ms % 1000) * 1000) as libc::suseconds_t,
    };
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            name,
            (&tv as *const libc::timeval).cast(),
            mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        last_errno()
    } else {
        0
    }
}

fn get_sockopt_timeval(fd: i32, name: i32) -> Result<u64, i32> {
    let mut tv: libc::timeval = unsafe { mem::zeroed() };
    let mut len = mem::size_of::<libc::timeval>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            name,
            (&mut tv as *mut libc::timeval).cast(),
            &mut len,
        )
    };
    if rc < 0 {
        Err(last_errno())
    } else {
        let secs = u64::try_from(tv.tv_sec.max(0)).unwrap_or(0);
        let usecs = u64::try_from(tv.tv_usec.max(0)).unwrap_or(0);
        Ok(secs.saturating_mul(1000).saturating_add(usecs / 1000))
    }
}

fn write_all_fd(fd: i32, buf: &[u8], timeout_ms: u64) -> Result<(), i32> {
    let mut sent = 0usize;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    for _ in 0..MAX_ECHO_STEPS {
        if sent >= buf.len() {
            return Ok(());
        }
        let left = deadline
            .checked_duration_since(Instant::now())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if left == 0 {
            return Err(libc::ETIMEDOUT);
        }
        poll_fd(fd, libc::POLLOUT, left)?;
        let n = unsafe { libc::send(fd, buf[sent..].as_ptr().cast(), buf.len() - sent, 0) };
        if n < 0 {
            let err = last_errno();
            if err == libc::EINTR || err == libc::EAGAIN {
                continue;
            }
            return Err(err);
        }
        if n == 0 {
            return Err(libc::EPIPE);
        }
        sent += n as usize;
    }
    Err(libc::EOVERFLOW)
}

fn read_exact_fd(fd: i32, buf: &mut [u8], timeout_ms: u64) -> Result<(), i32> {
    let mut got = 0usize;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    for _ in 0..MAX_ECHO_STEPS {
        if got >= buf.len() {
            return Ok(());
        }
        let left = deadline
            .checked_duration_since(Instant::now())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if left == 0 {
            return Err(libc::ETIMEDOUT);
        }
        poll_fd(fd, libc::POLLIN, left)?;
        let n = unsafe { libc::recv(fd, buf[got..].as_mut_ptr().cast(), buf.len() - got, 0) };
        if n < 0 {
            let err = last_errno();
            if err == libc::EINTR || err == libc::EAGAIN {
                continue;
            }
            return Err(err);
        }
        if n == 0 {
            return Err(libc::ECONNRESET);
        }
        got += n as usize;
    }
    Err(libc::EOVERFLOW)
}

fn percentile(sorted: &[f64], num: usize, den: usize) -> f64 {
    assert!(!sorted.is_empty());
    assert!(den > 0 && num <= den);
    let idx = (sorted.len().saturating_sub(1) * num) / den;
    sorted[idx]
}

fn cmd_connect(args: &[String]) -> ExitCode {
    if args.is_empty() {
        eprintln!(
            "usage: net-probe connect <ip:port> [--nonblock] [--timeout-ms N] [--send-http PATH]"
        );
        return ExitCode::from(2);
    }
    let addr = match parse_addr(&args[0]) {
        Ok(a) => a,
        Err(e) => {
            println!(
                "{{\"ok\":false,\"op\":\"connect\",\"error\":{}}}",
                json_escape(&e)
            );
            return ExitCode::from(2);
        }
    };
    let mut nonblock = false;
    let mut timeout_ms = DEFAULT_TIMEOUT_MS;
    let mut http_path: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--nonblock" => {
                nonblock = true;
                i += 1;
            }
            "--timeout-ms" if i + 1 < args.len() => {
                match parse_u64_bounded(&args[i + 1], "--timeout-ms", 1, MAX_TIMEOUT_MS) {
                    Ok(v) => timeout_ms = v,
                    Err(e) => {
                        println!(
                            "{{\"ok\":false,\"op\":\"connect\",\"error\":{}}}",
                            json_escape(&e)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            "--send-http" if i + 1 < args.len() => {
                http_path = Some(args[i + 1].clone());
                i += 2;
            }
            other => {
                println!(
                    "{{\"ok\":false,\"op\":\"connect\",\"error\":{}}}",
                    json_escape(&format!("unknown option {other:?}"))
                );
                return ExitCode::from(2);
            }
        }
    }

    let raw_fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    if raw_fd < 0 {
        print_call_error("connect", "socket", last_errno());
        return ExitCode::FAILURE;
    }
    let fd = OwnedFd(raw_fd);
    if nonblock {
        if let Err(err) = set_nonblock(fd.0, true) {
            print_call_error("connect", "fcntl", err);
            return ExitCode::FAILURE;
        }
    }

    let sa = to_sockaddr(addr);
    let t0 = Instant::now();
    let rc = unsafe {
        libc::connect(
            fd.0,
            (&sa as *const libc::sockaddr_in).cast(),
            mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    let mut connect_errno = if rc < 0 { last_errno() } else { 0 };
    let mut poll_revents: i16 = 0;
    let mut so_error: i32 = 0;
    let mut so_error_get_errno: i32 = 0;

    if nonblock {
        if connect_errno == libc::EINPROGRESS || connect_errno == libc::EAGAIN {
            match poll_fd(
                fd.0,
                libc::POLLOUT | libc::POLLERR | libc::POLLHUP,
                timeout_ms,
            ) {
                Ok(rev) => poll_revents = rev,
                Err(err) => connect_errno = err,
            }
        }
        match get_sockopt_int(fd.0, libc::SOL_SOCKET, libc::SO_ERROR) {
            Ok(v) => so_error = v,
            Err(e) => so_error_get_errno = e,
        }
        let connect_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let ok = (connect_errno == 0 || connect_errno == libc::EINPROGRESS)
            && so_error_get_errno == 0
            && so_error == 0;
        println!(
            "{{\"ok\":{ok},\"op\":\"connect\",\"call\":\"connect\",\"nonblock\":true,\"target\":{},\"connect_errno\":{connect_errno},\"connect_errno_name\":{},\"poll_revents\":{poll_revents},\"so_error\":{so_error},\"so_error_name\":{},\"so_error_text\":{},\"so_error_get_errno\":{so_error_get_errno},\"connect_ms\":{connect_ms:.3}}}",
            json_escape(&addr.to_string()),
            json_escape(errno_name(connect_errno)),
            json_escape(errno_name(so_error)),
            json_escape(&errno_text(so_error)),
        );
        return if ok {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }

    let connect_ms = t0.elapsed().as_secs_f64() * 1000.0;
    if connect_errno != 0 {
        println!(
            "{{\"ok\":false,\"op\":\"connect\",\"call\":\"connect\",\"nonblock\":false,\"target\":{},\"errno\":{connect_errno},\"error\":{},\"text\":{},\"connect_ms\":{connect_ms:.3}}}",
            json_escape(&addr.to_string()),
            json_escape(errno_name(connect_errno)),
            json_escape(&errno_text(connect_errno)),
        );
        return ExitCode::FAILURE;
    }

    let mut bytes_read = 0usize;
    let mut status_line = String::new();
    if let Some(path) = http_path {
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            addr.ip()
        );
        if let Err(err) = write_all_fd(fd.0, req.as_bytes(), timeout_ms) {
            print_call_error("connect", "send", err);
            return ExitCode::FAILURE;
        }
        let mut buf = [0u8; 4096];
        let mut head = Vec::new();
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        for _ in 0..MAX_HTTP_READS {
            let left = deadline
                .checked_duration_since(Instant::now())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            if left == 0 {
                break;
            }
            if poll_fd(fd.0, libc::POLLIN, left).is_err() {
                break;
            }
            let n = unsafe { libc::recv(fd.0, buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n <= 0 {
                break;
            }
            let count = n as usize;
            bytes_read = bytes_read.saturating_add(count);
            if head.len() < 256 {
                let take = (256 - head.len()).min(count);
                head.extend_from_slice(&buf[..take]);
            }
            if bytes_read >= MAX_HTTP_BYTES {
                break;
            }
        }
        status_line = String::from_utf8_lossy(&head)
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_owned();
    }
    let total_ms = t0.elapsed().as_secs_f64() * 1000.0;
    println!(
        "{{\"ok\":true,\"op\":\"connect\",\"call\":\"connect\",\"nonblock\":false,\"target\":{},\"errno\":0,\"connect_ms\":{connect_ms:.3},\"total_ms\":{total_ms:.3},\"bytes\":{bytes_read},\"status_line\":{}}}",
        json_escape(&addr.to_string()),
        json_escape(&status_line),
    );
    ExitCode::SUCCESS
}

fn cmd_bind(args: &[String]) -> ExitCode {
    if args.len() < 2 {
        eprintln!("usage: net-probe bind <tcp|udp> <ip:port> [--echo] [--once] [--max-conns N] [--ready-file PATH] [--timeout-ms N]");
        return ExitCode::from(2);
    }
    let proto = args[0].as_str();
    let sock_type = match proto {
        "tcp" => libc::SOCK_STREAM,
        "udp" => libc::SOCK_DGRAM,
        other => {
            println!(
                "{{\"ok\":false,\"op\":\"bind\",\"error\":{}}}",
                json_escape(&format!("unknown proto {other:?}"))
            );
            return ExitCode::from(2);
        }
    };
    let addr = match parse_addr(&args[1]) {
        Ok(a) => a,
        Err(e) => {
            println!(
                "{{\"ok\":false,\"op\":\"bind\",\"error\":{}}}",
                json_escape(&e)
            );
            return ExitCode::from(2);
        }
    };
    let mut echo = false;
    let mut max_conns: usize = 1;
    let mut ready_file: Option<String> = None;
    let mut timeout_ms = DEFAULT_TIMEOUT_MS;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--echo" => {
                echo = true;
                i += 1;
            }
            "--once" => {
                max_conns = 1;
                i += 1;
            }
            "--max-conns" if i + 1 < args.len() => {
                match parse_usize_bounded(&args[i + 1], "--max-conns", 1, MAX_CONNS) {
                    Ok(v) => max_conns = v,
                    Err(e) => {
                        println!(
                            "{{\"ok\":false,\"op\":\"bind\",\"error\":{}}}",
                            json_escape(&e)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            "--ready-file" if i + 1 < args.len() => {
                ready_file = Some(args[i + 1].clone());
                i += 2;
            }
            "--timeout-ms" if i + 1 < args.len() => {
                match parse_u64_bounded(&args[i + 1], "--timeout-ms", 1, MAX_TIMEOUT_MS) {
                    Ok(v) => timeout_ms = v,
                    Err(e) => {
                        println!(
                            "{{\"ok\":false,\"op\":\"bind\",\"error\":{}}}",
                            json_escape(&e)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            other => {
                println!(
                    "{{\"ok\":false,\"op\":\"bind\",\"error\":{}}}",
                    json_escape(&format!("unknown option {other:?}"))
                );
                return ExitCode::from(2);
            }
        }
    }

    let raw_fd = unsafe { libc::socket(libc::AF_INET, sock_type, 0) };
    if raw_fd < 0 {
        print_call_error("bind", "socket", last_errno());
        return ExitCode::FAILURE;
    }
    let fd = OwnedFd(raw_fd);
    let _ = set_sockopt_int(fd.0, libc::SOL_SOCKET, libc::SO_REUSEADDR, 1);
    let sa = to_sockaddr(addr);
    if unsafe {
        libc::bind(
            fd.0,
            (&sa as *const libc::sockaddr_in).cast(),
            mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    } < 0
    {
        print_call_error("bind", "bind", last_errno());
        return ExitCode::FAILURE;
    }

    let mut local_sa: libc::sockaddr_in = unsafe { mem::zeroed() };
    let mut local_len = mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    let bound = if unsafe {
        libc::getsockname(
            fd.0,
            (&mut local_sa as *mut libc::sockaddr_in).cast(),
            &mut local_len,
        )
    } == 0
    {
        from_sockaddr(&local_sa)
    } else {
        addr
    };

    if proto == "tcp" {
        if unsafe { libc::listen(fd.0, 16) } < 0 {
            print_call_error("bind", "listen", last_errno());
            return ExitCode::FAILURE;
        }
    }
    if let Some(ref path) = ready_file {
        if let Err(err) = fs::write(path, format!("{bound}\n")) {
            println!(
                "{{\"ok\":false,\"op\":\"bind\",\"call\":\"ready-file\",\"text\":{}}}",
                json_escape(&err.to_string())
            );
            return ExitCode::FAILURE;
        }
    }

    let mut handled = 0usize;
    if echo {
        let mut buf = vec![0u8; MAX_BYTES];
        if proto == "tcp" {
            for _ in 0..max_conns {
                if poll_fd(fd.0, libc::POLLIN, timeout_ms).is_err() {
                    break;
                }
                let client_raw = unsafe { libc::accept(fd.0, ptr::null_mut(), ptr::null_mut()) };
                if client_raw < 0 {
                    break;
                }
                let client = OwnedFd(client_raw);
                handled += 1;
                for _ in 0..MAX_ECHO_STEPS {
                    if poll_fd(client.0, libc::POLLIN, timeout_ms).is_err() {
                        break;
                    }
                    let n = unsafe { libc::recv(client.0, buf.as_mut_ptr().cast(), buf.len(), 0) };
                    if n <= 0 {
                        break;
                    }
                    if write_all_fd(client.0, &buf[..n as usize], timeout_ms).is_err() {
                        break;
                    }
                }
            }
        } else {
            for _ in 0..max_conns {
                if poll_fd(fd.0, libc::POLLIN, timeout_ms).is_err() {
                    break;
                }
                let mut peer_sa: libc::sockaddr_in = unsafe { mem::zeroed() };
                let mut peer_len = mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
                let n = unsafe {
                    libc::recvfrom(
                        fd.0,
                        buf.as_mut_ptr().cast(),
                        buf.len(),
                        0,
                        (&mut peer_sa as *mut libc::sockaddr_in).cast(),
                        &mut peer_len,
                    )
                };
                if n < 0 {
                    break;
                }
                let _ = unsafe {
                    libc::sendto(
                        fd.0,
                        buf.as_ptr().cast(),
                        n as usize,
                        0,
                        (&peer_sa as *const libc::sockaddr_in).cast(),
                        peer_len,
                    )
                };
                handled += 1;
            }
        }
    }

    println!(
        "{{\"ok\":true,\"op\":\"bind\",\"proto\":{},\"local\":{},\"handled\":{handled}}}",
        json_escape(proto),
        json_escape(&bound.to_string()),
    );
    ExitCode::SUCCESS
}

fn cmd_echo_bench(args: &[String]) -> ExitCode {
    if args.len() < 2 {
        eprintln!("usage: net-probe echo-bench <tcp|udp> <ip:port> [--count N] [--bytes B] [--nodelay] [--interval-ms M] [--timeout-ms T]");
        return ExitCode::from(2);
    }
    let proto = args[0].as_str();
    if proto != "tcp" && proto != "udp" {
        println!("{{\"ok\":false,\"op\":\"echo-bench\",\"error\":\"proto must be tcp or udp\"}}");
        return ExitCode::from(2);
    }
    let addr = match parse_addr(&args[1]) {
        Ok(a) => a,
        Err(e) => {
            println!(
                "{{\"ok\":false,\"op\":\"echo-bench\",\"error\":{}}}",
                json_escape(&e)
            );
            return ExitCode::from(2);
        }
    };
    let mut count: usize = 100;
    let mut bytes: usize = 1;
    let mut nodelay = false;
    let mut interval_ms: u64 = 0;
    let mut timeout_ms: u64 = DEFAULT_TIMEOUT_MS;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--count" if i + 1 < args.len() => {
                match parse_usize_bounded(&args[i + 1], "--count", 1, MAX_COUNT) {
                    Ok(v) => count = v,
                    Err(e) => {
                        println!(
                            "{{\"ok\":false,\"op\":\"echo-bench\",\"error\":{}}}",
                            json_escape(&e)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            "--bytes" if i + 1 < args.len() => {
                match parse_usize_bounded(&args[i + 1], "--bytes", 1, MAX_BYTES) {
                    Ok(v) => bytes = v,
                    Err(e) => {
                        println!(
                            "{{\"ok\":false,\"op\":\"echo-bench\",\"error\":{}}}",
                            json_escape(&e)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            "--nodelay" => {
                nodelay = true;
                i += 1;
            }
            "--interval-ms" if i + 1 < args.len() => {
                match parse_u64_bounded(&args[i + 1], "--interval-ms", 0, MAX_INTERVAL_MS) {
                    Ok(v) => interval_ms = v,
                    Err(e) => {
                        println!(
                            "{{\"ok\":false,\"op\":\"echo-bench\",\"error\":{}}}",
                            json_escape(&e)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            "--timeout-ms" if i + 1 < args.len() => {
                match parse_u64_bounded(&args[i + 1], "--timeout-ms", 1, MAX_TIMEOUT_MS) {
                    Ok(v) => timeout_ms = v,
                    Err(e) => {
                        println!(
                            "{{\"ok\":false,\"op\":\"echo-bench\",\"error\":{}}}",
                            json_escape(&e)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            other => {
                println!(
                    "{{\"ok\":false,\"op\":\"echo-bench\",\"error\":{}}}",
                    json_escape(&format!("unknown option {other:?}"))
                );
                return ExitCode::from(2);
            }
        }
    }

    let sock_type = if proto == "tcp" {
        libc::SOCK_STREAM
    } else {
        libc::SOCK_DGRAM
    };
    let raw_fd = unsafe { libc::socket(libc::AF_INET, sock_type, 0) };
    if raw_fd < 0 {
        print_call_error("echo-bench", "socket", last_errno());
        return ExitCode::FAILURE;
    }
    let fd = OwnedFd(raw_fd);
    let mut nodelay_errno = 0;
    if proto == "tcp" && nodelay {
        nodelay_errno = set_sockopt_int(fd.0, libc::IPPROTO_TCP, libc::TCP_NODELAY, 1);
    }
    let sa = to_sockaddr(addr);
    if unsafe {
        libc::connect(
            fd.0,
            (&sa as *const libc::sockaddr_in).cast(),
            mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    } < 0
    {
        print_call_error("echo-bench", "connect", last_errno());
        return ExitCode::FAILURE;
    }

    let mut payload = vec![0x5Au8; bytes];
    let mut reply = vec![0u8; bytes];
    let mut samples = Vec::with_capacity(count);
    let mut failed = 0usize;
    let mut last_err = 0i32;

    for idx in 0..count {
        payload[0] = (idx & 0xFF) as u8;
        let t0 = Instant::now();
        let step_ok = if proto == "tcp" {
            match write_all_fd(fd.0, &payload, timeout_ms)
                .and_then(|()| read_exact_fd(fd.0, &mut reply, timeout_ms))
            {
                Ok(()) => reply == payload,
                Err(e) => {
                    last_err = e;
                    false
                }
            }
        } else {
            let sent = unsafe { libc::send(fd.0, payload.as_ptr().cast(), payload.len(), 0) };
            if sent != payload.len() as isize {
                last_err = last_errno();
                false
            } else {
                match poll_fd(fd.0, libc::POLLIN, timeout_ms) {
                    Ok(_) => {
                        let got =
                            unsafe { libc::recv(fd.0, reply.as_mut_ptr().cast(), reply.len(), 0) };
                        if got == reply.len() as isize && reply == payload {
                            true
                        } else {
                            if got < 0 {
                                last_err = last_errno();
                            }
                            false
                        }
                    }
                    Err(e) => {
                        last_err = e;
                        false
                    }
                }
            }
        };
        if step_ok {
            samples.push(t0.elapsed().as_secs_f64() * 1000.0);
        } else {
            failed += 1;
            if proto == "tcp" {
                break;
            }
        }
        if interval_ms > 0 && idx + 1 < count {
            thread::sleep(Duration::from_millis(interval_ms));
        }
    }

    if samples.is_empty() {
        println!(
            "{{\"ok\":false,\"op\":\"echo-bench\",\"call\":\"exchange\",\"proto\":{},\"target\":{},\"count\":{count},\"bytes\":{bytes},\"completed\":0,\"failed\":{failed},\"errno\":{last_err},\"error\":{},\"text\":{}}}",
            json_escape(proto),
            json_escape(&addr.to_string()),
            json_escape(errno_name(last_err)),
            json_escape(&errno_text(last_err)),
        );
        return ExitCode::FAILURE;
    }
    samples.sort_by(|a, b| a.total_cmp(b));
    let min_ms = samples[0];
    let p50_ms = percentile(&samples, 50, 100);
    let p99_ms = percentile(&samples, 99, 100);
    let p999_json = if samples.len() >= P999_MIN_SAMPLES {
        format!("{:.3}", percentile(&samples, 999, 1000))
    } else {
        "null".to_owned()
    };
    let max_ms = *samples.last().unwrap();
    let ok = failed == 0 && samples.len() == count;
    println!(
        "{{\"ok\":{ok},\"op\":\"echo-bench\",\"proto\":{},\"target\":{},\"count\":{count},\"bytes\":{bytes},\"nodelay\":{nodelay},\"nodelay_errno\":{nodelay_errno},\"completed\":{},\"failed\":{failed},\"min_ms\":{min_ms:.3},\"p50_ms\":{p50_ms:.3},\"p99_ms\":{p99_ms:.3},\"p999_ms\":{p999_json},\"max_ms\":{max_ms:.3}}}",
        json_escape(proto),
        json_escape(&addr.to_string()),
        samples.len(),
    );
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn cmd_http_bench(args: &[String]) -> ExitCode {
    if args.len() < 2 {
        eprintln!("usage: net-probe http-bench <ip:port> <path> [--count N] [--interval-ms M] [--timeout-ms T]");
        return ExitCode::from(2);
    }
    let addr = match parse_addr(&args[0]) {
        Ok(a) => a,
        Err(e) => {
            println!(
                "{{\"ok\":false,\"op\":\"http-bench\",\"error\":{}}}",
                json_escape(&e)
            );
            return ExitCode::from(2);
        }
    };
    let path = &args[1];
    let mut count: usize = 20;
    let mut interval_ms: u64 = 1000;
    let mut timeout_ms: u64 = DEFAULT_TIMEOUT_MS;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--count" if i + 1 < args.len() => {
                match parse_usize_bounded(&args[i + 1], "--count", 1, MAX_COUNT) {
                    Ok(v) => count = v,
                    Err(e) => {
                        println!(
                            "{{\"ok\":false,\"op\":\"http-bench\",\"error\":{}}}",
                            json_escape(&e)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            "--interval-ms" if i + 1 < args.len() => {
                match parse_u64_bounded(&args[i + 1], "--interval-ms", 0, MAX_INTERVAL_MS) {
                    Ok(v) => interval_ms = v,
                    Err(e) => {
                        println!(
                            "{{\"ok\":false,\"op\":\"http-bench\",\"error\":{}}}",
                            json_escape(&e)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            "--timeout-ms" if i + 1 < args.len() => {
                match parse_u64_bounded(&args[i + 1], "--timeout-ms", 1, MAX_TIMEOUT_MS) {
                    Ok(v) => timeout_ms = v,
                    Err(e) => {
                        println!(
                            "{{\"ok\":false,\"op\":\"http-bench\",\"error\":{}}}",
                            json_escape(&e)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            other => {
                println!(
                    "{{\"ok\":false,\"op\":\"http-bench\",\"error\":{}}}",
                    json_escape(&format!("unknown option {other:?}"))
                );
                return ExitCode::from(2);
            }
        }
    }

    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        addr.ip()
    );
    let sa = to_sockaddr(addr);
    let mut connect_samples = Vec::with_capacity(count);
    let mut total_samples = Vec::with_capacity(count);
    let mut raw_totals = Vec::with_capacity(count);
    let mut failed = 0usize;
    let mut buf = [0u8; 4096];

    for idx in 0..count {
        let t0 = Instant::now();
        let raw_fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        if raw_fd < 0 {
            failed += 1;
            continue;
        }
        let fd = OwnedFd(raw_fd);
        let rc = unsafe {
            libc::connect(
                fd.0,
                (&sa as *const libc::sockaddr_in).cast(),
                mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            failed += 1;
            continue;
        }
        let c_ms = t0.elapsed().as_secs_f64() * 1000.0;
        if write_all_fd(fd.0, req.as_bytes(), timeout_ms).is_err() {
            failed += 1;
            continue;
        }
        let mut got_any = false;
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        for _ in 0..MAX_HTTP_READS {
            let left = deadline
                .checked_duration_since(Instant::now())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            if left == 0 {
                break;
            }
            if poll_fd(fd.0, libc::POLLIN, left).is_err() {
                break;
            }
            let n = unsafe { libc::recv(fd.0, buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n <= 0 {
                break;
            }
            got_any = true;
        }
        let tot_ms = t0.elapsed().as_secs_f64() * 1000.0;
        if got_any {
            connect_samples.push(c_ms);
            total_samples.push(tot_ms);
            raw_totals.push(tot_ms);
        } else {
            failed += 1;
        }
        if interval_ms > 0 && idx + 1 < count {
            thread::sleep(Duration::from_millis(interval_ms));
        }
    }

    if total_samples.is_empty() {
        println!(
            "{{\"ok\":false,\"op\":\"http-bench\",\"target\":{},\"count\":{count},\"completed\":0,\"failed\":{failed}}}",
            json_escape(&addr.to_string()),
        );
        return ExitCode::FAILURE;
    }
    connect_samples.sort_by(|a, b| a.total_cmp(b));
    total_samples.sort_by(|a, b| a.total_cmp(b));
    let totals_json = raw_totals
        .iter()
        .map(|v| format!("{v:.3}"))
        .collect::<Vec<_>>()
        .join(",");
    let ok = failed == 0 && total_samples.len() == count;
    println!(
        "{{\"ok\":{ok},\"op\":\"http-bench\",\"target\":{},\"count\":{count},\"completed\":{},\"failed\":{failed},\"connect_p50_ms\":{:.3},\"connect_max_ms\":{:.3},\"total_p50_ms\":{:.3},\"total_p99_ms\":{:.3},\"total_max_ms\":{:.3},\"totals_ms\":[{totals_json}]}}",
        json_escape(&addr.to_string()),
        total_samples.len(),
        percentile(&connect_samples, 50, 100),
        connect_samples.last().unwrap(),
        percentile(&total_samples, 50, 100),
        percentile(&total_samples, 99, 100),
        total_samples.last().unwrap(),
    );
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn cmd_lookup(args: &[String]) -> ExitCode {
    if args.len() != 1 {
        eprintln!("usage: net-probe lookup <name>");
        return ExitCode::from(2);
    }
    let Ok(c_name) = CString::new(args[0].as_str()) else {
        println!("{{\"ok\":false,\"op\":\"lookup\",\"error\":\"interior NUL in name\"}}");
        return ExitCode::from(2);
    };
    let mut hints: libc::addrinfo = unsafe { mem::zeroed() };
    hints.ai_family = libc::AF_INET;
    hints.ai_socktype = libc::SOCK_STREAM;
    let mut res: *mut libc::addrinfo = ptr::null_mut();
    let t0 = Instant::now();
    let rc = unsafe { libc::getaddrinfo(c_name.as_ptr(), ptr::null(), &hints, &mut res) };
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let err = last_errno();
    if rc != 0 {
        let gai_msg = unsafe {
            let ptr = libc::gai_strerror(rc);
            if ptr.is_null() {
                String::new()
            } else {
                CStr::from_ptr(ptr).to_string_lossy().into_owned()
            }
        };
        println!(
            "{{\"ok\":false,\"op\":\"lookup\",\"call\":\"getaddrinfo\",\"name\":{},\"gai_rc\":{rc},\"gai_error\":{},\"errno\":{err},\"error\":{},\"text\":{},\"elapsed_ms\":{elapsed_ms:.3},\"addrs\":[]}}",
            json_escape(&args[0]),
            json_escape(&gai_msg),
            json_escape(errno_name(err)),
            json_escape(&errno_text(err)),
        );
        return ExitCode::FAILURE;
    }

    let mut addrs = Vec::new();
    let mut cur = res;
    for _ in 0..MAX_ADDRS {
        if cur.is_null() {
            break;
        }
        let info = unsafe { &*cur };
        if info.ai_family == libc::AF_INET && !info.ai_addr.is_null() {
            let sa = unsafe { &*(info.ai_addr.cast::<libc::sockaddr_in>()) };
            let ip = Ipv4Addr::from(sa.sin_addr.s_addr.to_ne_bytes()).to_string();
            if !addrs.contains(&ip) {
                addrs.push(ip);
            }
        }
        cur = info.ai_next;
    }
    unsafe {
        libc::freeaddrinfo(res);
    }
    let addrs_json = addrs
        .iter()
        .map(|a| json_escape(a))
        .collect::<Vec<_>>()
        .join(",");
    println!(
        "{{\"ok\":true,\"op\":\"lookup\",\"name\":{},\"gai_rc\":0,\"errno\":0,\"elapsed_ms\":{elapsed_ms:.3},\"addrs\":[{addrs_json}]}}",
        json_escape(&args[0]),
    );
    ExitCode::SUCCESS
}

fn cmd_sockopts(args: &[String]) -> ExitCode {
    if args.is_empty() {
        eprintln!("usage: net-probe sockopts <tcp|udp> [--rcvbuf N] [--sndbuf N] [--nodelay 0|1] [--keepalive 0|1] [--rcvtimeo-ms N] [--sndtimeo-ms N]");
        return ExitCode::from(2);
    }
    let proto = args[0].as_str();
    let sock_type = match proto {
        "tcp" => libc::SOCK_STREAM,
        "udp" => libc::SOCK_DGRAM,
        other => {
            println!(
                "{{\"ok\":false,\"op\":\"sockopts\",\"error\":{}}}",
                json_escape(&format!("unknown proto {other:?}"))
            );
            return ExitCode::from(2);
        }
    };
    let mut rcvbuf: Option<i32> = None;
    let mut sndbuf: Option<i32> = None;
    let mut nodelay: Option<i32> = None;
    let mut keepalive: Option<i32> = None;
    let mut rcvtimeo_ms: Option<u64> = None;
    let mut sndtimeo_ms: Option<u64> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--rcvbuf" if i + 1 < args.len() => {
                match parse_usize_bounded(&args[i + 1], "--rcvbuf", 1024, 16 << 20) {
                    Ok(v) => rcvbuf = Some(v as i32),
                    Err(e) => {
                        println!(
                            "{{\"ok\":false,\"op\":\"sockopts\",\"error\":{}}}",
                            json_escape(&e)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            "--sndbuf" if i + 1 < args.len() => {
                match parse_usize_bounded(&args[i + 1], "--sndbuf", 1024, 16 << 20) {
                    Ok(v) => sndbuf = Some(v as i32),
                    Err(e) => {
                        println!(
                            "{{\"ok\":false,\"op\":\"sockopts\",\"error\":{}}}",
                            json_escape(&e)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            "--nodelay" if i + 1 < args.len() => {
                match parse_usize_bounded(&args[i + 1], "--nodelay", 0, 1) {
                    Ok(v) => nodelay = Some(v as i32),
                    Err(e) => {
                        println!(
                            "{{\"ok\":false,\"op\":\"sockopts\",\"error\":{}}}",
                            json_escape(&e)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            "--keepalive" if i + 1 < args.len() => {
                match parse_usize_bounded(&args[i + 1], "--keepalive", 0, 1) {
                    Ok(v) => keepalive = Some(v as i32),
                    Err(e) => {
                        println!(
                            "{{\"ok\":false,\"op\":\"sockopts\",\"error\":{}}}",
                            json_escape(&e)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            "--rcvtimeo-ms" if i + 1 < args.len() => {
                match parse_u64_bounded(&args[i + 1], "--rcvtimeo-ms", 1, MAX_TIMEOUT_MS) {
                    Ok(v) => rcvtimeo_ms = Some(v),
                    Err(e) => {
                        println!(
                            "{{\"ok\":false,\"op\":\"sockopts\",\"error\":{}}}",
                            json_escape(&e)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            "--sndtimeo-ms" if i + 1 < args.len() => {
                match parse_u64_bounded(&args[i + 1], "--sndtimeo-ms", 1, MAX_TIMEOUT_MS) {
                    Ok(v) => sndtimeo_ms = Some(v),
                    Err(e) => {
                        println!(
                            "{{\"ok\":false,\"op\":\"sockopts\",\"error\":{}}}",
                            json_escape(&e)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            other => {
                println!(
                    "{{\"ok\":false,\"op\":\"sockopts\",\"error\":{}}}",
                    json_escape(&format!("unknown option {other:?}"))
                );
                return ExitCode::from(2);
            }
        }
    }

    let raw_fd = unsafe { libc::socket(libc::AF_INET, sock_type, 0) };
    if raw_fd < 0 {
        print_call_error("sockopts", "socket", last_errno());
        return ExitCode::FAILURE;
    }
    let fd = OwnedFd(raw_fd);
    let set_rcvbuf_errno =
        rcvbuf.map(|v| set_sockopt_int(fd.0, libc::SOL_SOCKET, libc::SO_RCVBUF, v));
    let set_sndbuf_errno =
        sndbuf.map(|v| set_sockopt_int(fd.0, libc::SOL_SOCKET, libc::SO_SNDBUF, v));
    let set_keepalive_errno =
        keepalive.map(|v| set_sockopt_int(fd.0, libc::SOL_SOCKET, libc::SO_KEEPALIVE, v));
    let set_nodelay_errno =
        nodelay.map(|v| set_sockopt_int(fd.0, libc::IPPROTO_TCP, libc::TCP_NODELAY, v));
    let set_rcvtimeo_errno = rcvtimeo_ms.map(|v| set_sockopt_timeval(fd.0, libc::SO_RCVTIMEO, v));
    let set_sndtimeo_errno = sndtimeo_ms.map(|v| set_sockopt_timeval(fd.0, libc::SO_SNDTIMEO, v));

    let fmt_opt = |res: Result<i32, i32>| -> String {
        match res {
            Ok(v) => format!("{{\"value\":{v},\"errno\":0}}"),
            Err(e) => format!(
                "{{\"value\":null,\"errno\":{e},\"error\":{}}}",
                json_escape(errno_name(e))
            ),
        }
    };
    let fmt_opt_u64 = |res: Result<u64, i32>| -> String {
        match res {
            Ok(v) => format!("{{\"value\":{v},\"errno\":0}}"),
            Err(e) => format!(
                "{{\"value\":null,\"errno\":{e},\"error\":{}}}",
                json_escape(errno_name(e))
            ),
        }
    };
    let fmt_set = |v: Option<i32>| -> String {
        match v {
            Some(e) => format!("{e}"),
            None => "null".to_owned(),
        }
    };
    let get_rcvbuf = fmt_opt(get_sockopt_int(fd.0, libc::SOL_SOCKET, libc::SO_RCVBUF));
    let get_sndbuf = fmt_opt(get_sockopt_int(fd.0, libc::SOL_SOCKET, libc::SO_SNDBUF));
    let get_so_error = fmt_opt(get_sockopt_int(fd.0, libc::SOL_SOCKET, libc::SO_ERROR));
    let get_keepalive = fmt_opt(get_sockopt_int(fd.0, libc::SOL_SOCKET, libc::SO_KEEPALIVE));
    let get_nodelay = fmt_opt(get_sockopt_int(fd.0, libc::IPPROTO_TCP, libc::TCP_NODELAY));
    let get_rcvtimeo = fmt_opt_u64(get_sockopt_timeval(fd.0, libc::SO_RCVTIMEO));
    let get_sndtimeo = fmt_opt_u64(get_sockopt_timeval(fd.0, libc::SO_SNDTIMEO));
    println!(
        "{{\"ok\":true,\"op\":\"sockopts\",\"proto\":{},\"set\":{{\"rcvbuf_errno\":{},\"sndbuf_errno\":{},\"keepalive_errno\":{},\"nodelay_errno\":{},\"rcvtimeo_errno\":{},\"sndtimeo_errno\":{}}},\"get\":{{\"rcvbuf\":{get_rcvbuf},\"sndbuf\":{get_sndbuf},\"so_error\":{get_so_error},\"keepalive\":{get_keepalive},\"nodelay\":{get_nodelay},\"rcvtimeo_ms\":{get_rcvtimeo},\"sndtimeo_ms\":{get_sndtimeo}}}}}",
        json_escape(proto),
        fmt_set(set_rcvbuf_errno),
        fmt_set(set_sndbuf_errno),
        fmt_set(set_keepalive_errno),
        fmt_set(set_nodelay_errno),
        fmt_set(set_rcvtimeo_errno),
        fmt_set(set_sndtimeo_errno),
    );
    ExitCode::SUCCESS
}

fn cmd_udp_send(args: &[String]) -> ExitCode {
    if args.is_empty() {
        eprintln!("usage: net-probe udp-send <ip:port> [--bytes B] [--count N] [--expect-echo] [--timeout-ms T]");
        return ExitCode::from(2);
    }
    let addr = match parse_addr(&args[0]) {
        Ok(a) => a,
        Err(e) => {
            println!(
                "{{\"ok\":false,\"op\":\"udp-send\",\"error\":{}}}",
                json_escape(&e)
            );
            return ExitCode::from(2);
        }
    };
    let mut bytes: usize = 1472;
    let mut count: usize = 3;
    let mut expect_echo = false;
    let mut timeout_ms = DEFAULT_TIMEOUT_MS;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--bytes" if i + 1 < args.len() => {
                match parse_usize_bounded(&args[i + 1], "--bytes", 1, MAX_BYTES) {
                    Ok(v) => bytes = v,
                    Err(e) => {
                        println!(
                            "{{\"ok\":false,\"op\":\"udp-send\",\"error\":{}}}",
                            json_escape(&e)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            "--count" if i + 1 < args.len() => {
                match parse_usize_bounded(&args[i + 1], "--count", 1, MAX_COUNT) {
                    Ok(v) => count = v,
                    Err(e) => {
                        println!(
                            "{{\"ok\":false,\"op\":\"udp-send\",\"error\":{}}}",
                            json_escape(&e)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            "--expect-echo" => {
                expect_echo = true;
                i += 1;
            }
            "--timeout-ms" if i + 1 < args.len() => {
                match parse_u64_bounded(&args[i + 1], "--timeout-ms", 1, MAX_TIMEOUT_MS) {
                    Ok(v) => timeout_ms = v,
                    Err(e) => {
                        println!(
                            "{{\"ok\":false,\"op\":\"udp-send\",\"error\":{}}}",
                            json_escape(&e)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            other => {
                println!(
                    "{{\"ok\":false,\"op\":\"udp-send\",\"error\":{}}}",
                    json_escape(&format!("unknown option {other:?}"))
                );
                return ExitCode::from(2);
            }
        }
    }

    let raw_fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if raw_fd < 0 {
        print_call_error("udp-send", "socket", last_errno());
        return ExitCode::FAILURE;
    }
    let fd = OwnedFd(raw_fd);
    let sa = to_sockaddr(addr);
    if expect_echo {
        if unsafe {
            libc::connect(
                fd.0,
                (&sa as *const libc::sockaddr_in).cast(),
                mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            )
        } < 0
        {
            print_call_error("udp-send", "connect", last_errno());
            return ExitCode::FAILURE;
        }
    }
    let mut payload = vec![0xA5u8; bytes];
    let mut reply = vec![0u8; bytes];
    let mut sent = 0usize;
    let mut echoed = 0usize;
    let mut last_err = 0i32;

    for idx in 0..count {
        payload[0] = (idx & 0xFF) as u8;
        let rc = if expect_echo {
            unsafe { libc::send(fd.0, payload.as_ptr().cast(), payload.len(), 0) }
        } else {
            unsafe {
                libc::sendto(
                    fd.0,
                    payload.as_ptr().cast(),
                    payload.len(),
                    0,
                    (&sa as *const libc::sockaddr_in).cast(),
                    mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                )
            }
        };
        if rc != payload.len() as isize {
            last_err = last_errno();
            continue;
        }
        sent += 1;
        if expect_echo {
            match poll_fd(fd.0, libc::POLLIN, timeout_ms) {
                Ok(_) => {
                    let got =
                        unsafe { libc::recv(fd.0, reply.as_mut_ptr().cast(), reply.len(), 0) };
                    if got == reply.len() as isize && reply == payload {
                        echoed += 1;
                    } else if got < 0 {
                        last_err = last_errno();
                    }
                }
                Err(e) => last_err = e,
            }
        }
    }
    let ok = sent == count && (!expect_echo || echoed == count);
    println!(
        "{{\"ok\":{ok},\"op\":\"udp-send\",\"call\":\"sendto\",\"target\":{},\"bytes\":{bytes},\"count\":{count},\"sent\":{sent},\"echoed\":{echoed},\"errno\":{last_err},\"error\":{},\"text\":{}}}",
        json_escape(&addr.to_string()),
        json_escape(errno_name(last_err)),
        json_escape(&errno_text(last_err)),
    );
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).take(MAX_ARGS).collect();
    let Some((cmd, rest)) = args.split_first() else {
        eprintln!(
            "usage: net-probe <connect|bind|echo-bench|http-bench|lookup|sockopts|udp-send> ..."
        );
        return ExitCode::from(2);
    };
    match cmd.as_str() {
        "connect" => cmd_connect(rest),
        "bind" => cmd_bind(rest),
        "echo-bench" => cmd_echo_bench(rest),
        "http-bench" => cmd_http_bench(rest),
        "lookup" => cmd_lookup(rest),
        "sockopts" => cmd_sockopts(rest),
        "udp-send" => cmd_udp_send(rest),
        other => {
            eprintln!("net-probe: unknown subcommand {other:?}");
            ExitCode::from(2)
        }
    }
}

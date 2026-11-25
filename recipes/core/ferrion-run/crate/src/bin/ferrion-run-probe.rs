#[cfg(target_os = "redox")]
fn main() {
    std::process::exit(probe::main(std::env::args().skip(1).collect()));
}

#[cfg(not(target_os = "redox"))]
fn main() {
    eprintln!("ferrion-run-probe: runs on FerrionOS only");
    std::process::exit(2);
}

#[cfg(target_os = "redox")]
mod probe {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;

    use libredox::call;
    use syscall::data::StdFsCallMeta;
    use syscall::flag::{F_DUPFD, F_SETFD, StdFsCallKind, UPPER_FDTBL_TAG};

    const FDS_VAR: &str = "FERRION_RUN_PROBE_FDS";
    const EXTRA_FD_VAR: &str = "FERRION_RUN_PROBE_EXTRA_FD";
    const MAX_PIPE_BYTES: usize = 128 << 20;
    const PIPE_CHUNK: usize = 65536;
    const MAX_PIPE_ITERS: usize = (MAX_PIPE_BYTES / 512) + 4096;

    fn request(path: &str) -> String {
        format!("GET {path} HTTP/1.0\r\nHost: ferrion-run-probe\r\n\r\n")
    }

    fn first_line(reply: &[u8]) -> String {
        String::from_utf8_lossy(reply)
            .lines()
            .next()
            .unwrap_or("")
            .to_owned()
    }

    pub fn main(args: Vec<String>) -> i32 {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        match args[..] {
            ["hold-socket", address, ref rest @ ..] => hold_socket(address, rest),
            ["use-fd", path] => use_fd(path),
            ["connect", address, path] => connect(address, path),
            ["escape", scope, secret, marker, victim, home] => ferrion_run::escape::escape(
                scope.as_ref(),
                secret.as_ref(),
                marker,
                victim.as_ref(),
                home.as_ref(),
            ),
            ["race-swap", scope, outside, seconds] => ferrion_run::escape::race_swap(
                scope.as_ref(),
                outside.as_ref(),
                seconds.parse().unwrap_or(10),
            ),
            ["race-read", scope, name, marker, seconds] => ferrion_run::escape::race_read(
                scope.as_ref(),
                name,
                marker,
                seconds.parse().unwrap_or(10),
            ),
            ["schemes"] => ferrion_run::escape::schemes(),
            ["disk"] => ferrion_run::escape::raw_disk(),
            ["tree-hash", dir] => ferrion_run::escape::tree_hash(dir.as_ref()),
            ["clone-escape", dir, outside] => {
                ferrion_run::escape::clone_escape(dir.as_ref(), outside.as_ref())
            }
            ["home"] => ferrion_run::escape::home_report(),
            ["foreign-filetable"] => ferrion_run::escape::foreign_filetable(),
            ["hold-filetable", program, ref rest @ ..] => hold_filetable(program, rest),
            ["fill", path, kib] => fill_file(path, kib.parse().unwrap_or(0)),
            ["fill-disk", path, reserve] => fill_disk(path, reserve),
            ["rename-onto-dir", from, to] => rename_onto_dir(from, to),
            ["lock-hold", path, secs] => lock_hold(path, secs.parse().unwrap_or(0)),
            ["lock-try", path] => lock_try(path),
            ["lock-fchmod", path] => lock_fchmod(path),
            ["retarget-link", path, target] => retarget_link(path, target),
            ["create-many", dir, count] => create_many(dir, count.parse().unwrap_or(0)),
            ["pass-unix", ref rest @ ..] => pass_unix(rest),
            ["unix-child", fd, pipe_bytes] => unix_child(fd, pipe_bytes),
            ["pass-bad", kind, ref rest @ ..] => pass_bad(kind, rest),
            _ => {
                eprintln!("ferrion-run-probe: unknown arguments {args:?}");
                2
            }
        }
    }

    fn fill_random_chunk(state: &mut u64, buf: &mut [u8]) {
        for word in buf.chunks_exact_mut(8) {
            let mut x = *state;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *state = x;
            word.copy_from_slice(&x.to_le_bytes());
        }
    }

    fn fill_file(path: &str, kib: usize) -> i32 {
        let mut file = match std::fs::File::create(path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("ferrion-run-probe: create {path}: {e}");
                return 1;
            }
        };
        let mut chunk = [0u8; 65536];
        let mut rng: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut remaining = kib.saturating_mul(1024);
        while remaining > 0 {
            fill_random_chunk(&mut rng, &mut chunk);
            let n = remaining.min(chunk.len());
            if let Err(e) = file.write_all(&chunk[..n]) {
                eprintln!("ferrion-run-probe: write {path}: {e}");
                return 1;
            }
            remaining -= n;
        }
        let _ = file.sync_all();
        0
    }

    fn fill_disk(ballast: &str, reserve: &str) -> i32 {
        if fill_file(reserve, 512) != 0 {
            return 1;
        }
        let mut file = match std::fs::File::create(ballast) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("ferrion-run-probe: create {ballast}: {e}");
                let _ = std::fs::remove_file(reserve);
                return 1;
            }
        };
        let mut chunk = [0u8; 65536];
        let mut rng: u64 = 0xD1B5_4A32_D192_ED03;
        let mut total_kib = 0usize;
        const MAX_CHUNKS: usize = 32768;
        for _ in 0..MAX_CHUNKS {
            fill_random_chunk(&mut rng, &mut chunk);
            match file.write_all(&chunk) {
                Ok(()) => total_kib += 64,
                Err(_) => break,
            }
        }
        let _ = file.sync_all();
        drop(file);
        let _ = std::fs::remove_file(reserve);
        println!("FILLED {total_kib}");
        0
    }

    fn rename_onto_dir(from: &str, to: &str) -> i32 {
        match std::fs::rename(from, to) {
            Ok(()) => {
                println!("RENAME OK");
                0
            }
            Err(e) => {
                println!("RENAME ERR {}", e.raw_os_error().unwrap_or(-1));
                0
            }
        }
    }

    fn ofd_lock_ex(fd: usize) -> syscall::Result<usize> {
        const LOCK_OFD: u64 = 1 << 63;
        const LOCK_EX: u64 = 1 << 63;
        let mut payload = 0u64.to_ne_bytes();
        let meta = StdFsCallMeta::new(StdFsCallKind::Lock, LOCK_OFD, LOCK_EX);
        syscall::std_fs_call(fd, &mut payload, &meta)
    }

    fn lock_hold(path: &str, secs: u64) -> i32 {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("ferrion-run-probe: open {path}: {e}");
                return 2;
            }
        };
        if let Err(e) = ofd_lock_ex(file.as_raw_fd() as usize) {
            println!("LOCK_BUSY {}", e.errno);
            return 1;
        }
        println!("LOCK_HELD");
        std::thread::sleep(std::time::Duration::from_secs(secs));
        drop(file);
        0
    }

    fn lock_try(path: &str) -> i32 {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("ferrion-run-probe: open {path}: {e}");
                return 2;
            }
        };
        match ofd_lock_ex(file.as_raw_fd() as usize) {
            Ok(_) => {
                println!("LOCK_ACQUIRED");
                0
            }
            Err(e) => {
                println!("LOCK_BUSY {}", e.errno);
                1
            }
        }
    }

    fn lock_fchmod(path: &str) -> i32 {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("ferrion-run-probe: open {path}: {e}");
                return 2;
            }
        };
        if let Err(e) = ofd_lock_ex(file.as_raw_fd() as usize) {
            println!("LOCK_BUSY {}", e.errno);
            return 1;
        }
        if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
            eprintln!(
                "ferrion-run-probe: fchmod {path}: {}",
                std::io::Error::last_os_error()
            );
            return 1;
        }
        println!("LOCK_FCHMOD OK");
        0
    }

    fn retarget_link(path: &str, ext_link: &str) -> i32 {
        use syscall::flag::{O_NOFOLLOW, O_STAT, O_SYMLINK, O_WRONLY};
        let rw_flags = (O_SYMLINK | O_NOFOLLOW | O_STAT | O_WRONLY) as i32;
        let fd = match call::open(path, rw_flags, 0) {
            Ok(fd) => fd,
            Err(e) => {
                eprintln!("ferrion-run-probe: open {path}: {e}");
                return 1;
            }
        };
        let _ = call::close(fd);
        let ext_flags = (O_SYMLINK | O_WRONLY) as i32;
        let efd = match call::open(ext_link, ext_flags, 0) {
            Ok(fd) => fd,
            Err(e) => {
                eprintln!("ferrion-run-probe: open external {ext_link}: {e}");
                return 1;
            }
        };
        let mut buf = [0u8; 256];
        let path_res = call::fpath(efd, &mut buf);
        let _ = call::close(efd);
        match path_res {
            Ok(n) => {
                let text = String::from_utf8_lossy(&buf[..n]);
                println!("RETARGET_OK EXT={text}");
                0
            }
            Err(e) => {
                eprintln!("ferrion-run-probe: fpath {ext_link}: {e}");
                1
            }
        }
    }

    fn create_many(dir: &str, count: usize) -> i32 {
        let start = std::time::Instant::now();
        for i in 0..count {
            let path = format!("{dir}/f{i}");
            if let Err(e) = std::fs::File::create(&path) {
                eprintln!("ferrion-run-probe: create {path}: {e}");
                return 1;
            }
        }
        let elapsed_ms = start.elapsed().as_millis();
        println!("CREATED {count} IN {elapsed_ms}ms");
        0
    }

    fn hold_socket(address: &str, rest: &[&str]) -> i32 {
        let (stdin, command) = match rest {
            ["--stdin", "--", command @ ..] => (true, command),
            ["--", command @ ..] => (false, command),
            _ => (false, &[][..]),
        };
        let Some((program, program_args)) = command.split_first() else {
            eprintln!("ferrion-run-probe: hold-socket needs -- PROGRAM");
            return 2;
        };
        let fd = match TcpStream::connect(address) {
            Ok(stream) => stream.into_raw_fd() as usize,
            Err(e) => {
                eprintln!("ferrion-run-probe: cannot connect to {address}: {e}");
                return 2;
            }
        };
        let upper =
            call::fcntl(fd, F_SETFD, 0).and_then(|_| call::fcntl(fd, F_DUPFD, UPPER_FDTBL_TAG));
        let upper = match upper {
            Ok(upper) => upper,
            Err(e) => {
                eprintln!("ferrion-run-probe: cannot keep the socket across exec: {e}");
                return 2;
            }
        };
        if stdin && let Err(e) = call::dup2(fd, 0) {
            eprintln!("ferrion-run-probe: cannot make the socket standard input: {e}");
            return 2;
        }
        println!("ferrion-run-probe: holding descriptors {fd} and {upper:#x}");
        let error = std::process::Command::new(program)
            .args(program_args)
            .env(FDS_VAR, format!("{fd},{upper}"))
            .exec();
        eprintln!("ferrion-run-probe: cannot run {program}: {error}");
        2
    }

    fn hold_filetable(program: &str, rest: &[&str]) -> i32 {
        let thread_fd = ferrion_run::launch::current_thread_fd();
        let held = match call::dup(thread_fd, b"filetable-binary") {
            Ok(fd) => fd,
            Err(e) => {
                eprintln!("ferrion-run-probe: cannot open this process's file table: {e}");
                return 2;
            }
        };
        println!("ferrion-run-probe: holding this process's file table at {held}");
        let status = std::process::Command::new(program).args(rest).status();
        let _ = call::close(held);
        match status {
            Ok(status) => status.code().unwrap_or(128),
            Err(e) => {
                eprintln!("ferrion-run-probe: cannot run {program}: {e}");
                2
            }
        }
    }

    fn use_fd(path: &str) -> i32 {
        let fds: Vec<usize> = std::env::var(FDS_VAR)
            .unwrap_or_default()
            .split(',')
            .filter_map(|s| s.parse().ok())
            .collect();
        if fds.is_empty() {
            eprintln!("ferrion-run-probe: {FDS_VAR} names no descriptor");
            return 2;
        }
        let mut usable = false;
        for fd in fds {
            let reply = call::write(fd, request(path).as_bytes()).and_then(|_| {
                let mut buf = [0u8; 512];
                call::read(fd, &mut buf).map(|n| buf[..n].to_vec())
            });
            match reply {
                Ok(reply) if !reply.is_empty() => {
                    usable = true;
                    println!(
                        "ferrion-run-probe: descriptor {fd:#x} usable: {}",
                        first_line(&reply)
                    );
                }
                Ok(_) => println!("ferrion-run-probe: descriptor {fd:#x} unusable: no answer"),
                Err(e) => println!("ferrion-run-probe: descriptor {fd:#x} unusable: {e}"),
            }
        }
        if usable { 0 } else { 1 }
    }

    fn connect(address: &str, path: &str) -> i32 {
        let reply = TcpStream::connect(address).and_then(|mut stream| {
            stream.write_all(request(path).as_bytes())?;
            let mut reply = Vec::new();
            stream.read_to_end(&mut reply)?;
            Ok(reply)
        });
        match reply {
            Ok(reply) => {
                println!("ferrion-run-probe: connected: {}", first_line(&reply));
                0
            }
            Err(e) => {
                println!("ferrion-run-probe: cannot connect: {e}");
                1
            }
        }
    }

    fn send_fd(sock: libc::c_int, fd_to_send: libc::c_int) -> Result<(), String> {
        let mut byte = [b'P'; 1];
        let mut iov = libc::iovec {
            iov_base: byte.as_mut_ptr().cast(),
            iov_len: byte.len(),
        };
        let space = unsafe { libc::CMSG_SPACE(size_of::<libc::c_int>() as u32) } as usize;
        let mut cbuf = vec![0u8; space];
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cbuf.as_mut_ptr().cast();
        msg.msg_controllen = unsafe { libc::CMSG_LEN(size_of::<libc::c_int>() as u32) } as _;
        let cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        if cmsg.is_null() {
            return Err("CMSG_FIRSTHDR returned null".into());
        }
        unsafe {
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = msg.msg_controllen;
            std::ptr::copy_nonoverlapping(
                &fd_to_send as *const libc::c_int as *const u8,
                libc::CMSG_DATA(cmsg),
                size_of::<libc::c_int>(),
            );
        }
        let n = unsafe { libc::sendmsg(sock, &msg, 0) };
        if n != 1 {
            return Err(format!("sendmsg: {}", std::io::Error::last_os_error()));
        }
        Ok(())
    }

    fn recv_fd(sock: libc::c_int) -> Result<libc::c_int, String> {
        let mut byte = [0u8; 1];
        let mut iov = libc::iovec {
            iov_base: byte.as_mut_ptr().cast(),
            iov_len: byte.len(),
        };
        let space = unsafe { libc::CMSG_SPACE(size_of::<libc::c_int>() as u32) } as usize;
        let mut cbuf = vec![0u8; space];
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cbuf.as_mut_ptr().cast();
        msg.msg_controllen = space as _;
        let n = unsafe { libc::recvmsg(sock, &mut msg, 0) };
        if n <= 0 {
            return Err(format!("recvmsg: {}", std::io::Error::last_os_error()));
        }
        let cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        if cmsg.is_null() {
            return Err("recvmsg returned no control message".into());
        }
        let (level, ty) = unsafe { ((*cmsg).cmsg_level, (*cmsg).cmsg_type) };
        if level != libc::SOL_SOCKET || ty != libc::SCM_RIGHTS {
            return Err(format!("unexpected cmsg {level}/{ty}"));
        }
        let mut fd: libc::c_int = -1;
        unsafe {
            std::ptr::copy_nonoverlapping(
                libc::CMSG_DATA(cmsg),
                &mut fd as *mut libc::c_int as *mut u8,
                size_of::<libc::c_int>(),
            );
        }
        if fd < 0 {
            return Err("received negative descriptor".into());
        }
        Ok(fd)
    }

    fn mix_cksum(cksum: &mut u64, buf: &[u8]) {
        for &b in buf {
            *cksum = cksum.wrapping_mul(0x100000001b3).wrapping_add(u64::from(b));
        }
    }

    fn pass_unix(rest: &[&str]) -> i32 {
        let mut extra_file: Option<&str> = None;
        let mut pipe_bytes: usize = 0;
        let mut idx = 0;
        let mut command: &[&str] = &[];
        while idx < rest.len() {
            match rest[idx] {
                "--extra-file" if idx + 1 < rest.len() => {
                    extra_file = Some(rest[idx + 1]);
                    idx += 2;
                }
                "--pipe-bytes" if idx + 1 < rest.len() => {
                    pipe_bytes = match rest[idx + 1].parse() {
                        Ok(n) if n <= MAX_PIPE_BYTES => n,
                        _ => {
                            eprintln!("ferrion-run-probe: invalid --pipe-bytes");
                            return 2;
                        }
                    };
                    idx += 2;
                }
                "--" => {
                    command = &rest[idx + 1..];
                    break;
                }
                other => {
                    eprintln!("ferrion-run-probe: unexpected pass-unix arg {other}");
                    return 2;
                }
            }
        }
        let Some((program, program_args)) = command.split_first() else {
            eprintln!("ferrion-run-probe: pass-unix needs -- PROGRAM");
            return 2;
        };
        let (mut parent, child_sock) = match UnixStream::pair() {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("ferrion-run-probe: UnixStream::pair: {e}");
                return 2;
            }
        };
        let child_fd = child_sock.into_raw_fd() as usize;
        if let Err(e) = call::fcntl(child_fd, F_SETFD, 0) {
            eprintln!("ferrion-run-probe: clear cloexec on {child_fd}: {e}");
            let _ = call::close(child_fd);
            return 2;
        }
        let extra_fd = match extra_file {
            Some(path) => match std::fs::File::open(path) {
                Ok(f) => {
                    let fd = f.into_raw_fd() as usize;
                    if let Err(e) = call::fcntl(fd, F_SETFD, 0) {
                        eprintln!("ferrion-run-probe: clear cloexec on extra {fd}: {e}");
                        let _ = call::close(fd);
                        let _ = call::close(child_fd);
                        return 2;
                    }
                    Some(fd)
                }
                Err(e) => {
                    eprintln!("ferrion-run-probe: open extra {path}: {e}");
                    let _ = call::close(child_fd);
                    return 2;
                }
            },
            None => None,
        };
        let fd_str = child_fd.to_string();
        let resolved: Vec<String> = program_args
            .iter()
            .map(|a| a.replace("{fd}", &fd_str))
            .collect();
        let mut cmd = std::process::Command::new(program);
        cmd.args(&resolved);
        if let Some(efd) = extra_fd {
            cmd.env(EXTRA_FD_VAR, efd.to_string());
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("ferrion-run-probe: spawn {program}: {e}");
                let _ = call::close(child_fd);
                if let Some(efd) = extra_fd {
                    let _ = call::close(efd);
                }
                return 2;
            }
        };
        let _ = call::close(child_fd);
        if let Some(efd) = extra_fd {
            let _ = call::close(efd);
        }

        let res = if pipe_bytes == 0 {
            (|| -> Result<String, String> {
                parent
                    .write_all(b"ping")
                    .map_err(|e| format!("write ping: {e}"))?;
                let mut reply = [0u8; 4];
                parent
                    .read_exact(&mut reply)
                    .map_err(|e| format!("read pong: {e}"))?;
                if &reply != b"pong" {
                    return Err(format!("expected pong, got {reply:?}"));
                }
                Ok("PASS_UNIX_OK".into())
            })()
        } else {
            (|| -> Result<String, String> {
                let start = std::time::Instant::now();
                let rfd = recv_fd(parent.as_raw_fd())?;
                let mut reader = unsafe { std::fs::File::from_raw_fd(rfd) };
                let mut buf = vec![0u8; PIPE_CHUNK];
                let mut total = 0usize;
                let mut cksum: u64 = 0xcbf29ce484222325;
                let mut saw_eof = false;
                for _ in 0..MAX_PIPE_ITERS {
                    let n = reader
                        .read(&mut buf)
                        .map_err(|e| format!("read pipe: {e}"))?;
                    if n == 0 {
                        saw_eof = true;
                        break;
                    }
                    total = total.saturating_add(n);
                    if total > pipe_bytes {
                        return Err(format!("pipe sent more than {pipe_bytes} bytes"));
                    }
                    mix_cksum(&mut cksum, &buf[..n]);
                }
                let elapsed_ms = start.elapsed().as_millis();
                if !saw_eof || total != pipe_bytes {
                    return Err(format!(
                        "pipe short read: got {total}/{pipe_bytes} eof={saw_eof}"
                    ));
                }
                let mut child_cksum_bytes = [0u8; 8];
                parent
                    .read_exact(&mut child_cksum_bytes)
                    .map_err(|e| format!("read child cksum: {e}"))?;
                let child_cksum = u64::from_le_bytes(child_cksum_bytes);
                if cksum != child_cksum {
                    return Err(format!(
                        "pipe cksum mismatch: parent={cksum:016x} child={child_cksum:016x}"
                    ));
                }
                Ok(format!(
                    "PASS_PIPE_OK bytes={total} ms={elapsed_ms} cksum={cksum:016x}"
                ))
            })()
        };

        let status = match child.wait() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("ferrion-run-probe: wait: {e}");
                return 1;
            }
        };
        match res {
            Ok(line) if status.success() => {
                println!("{line}");
                0
            }
            Ok(line) => {
                eprintln!("ferrion-run-probe: child exited {status} after {line}");
                status.code().unwrap_or(1)
            }
            Err(e) => {
                eprintln!("ferrion-run-probe: {e} (child {status})");
                1
            }
        }
    }

    fn unix_child(fd_str: &str, pipe_bytes_str: &str) -> i32 {
        let fd: libc::c_int = match fd_str.parse() {
            Ok(n) if n > 2 => n,
            _ => {
                eprintln!("ferrion-run-probe: invalid child fd {fd_str}");
                return 2;
            }
        };
        let pipe_bytes: usize = match pipe_bytes_str.parse() {
            Ok(n) if n <= MAX_PIPE_BYTES => n,
            _ => {
                eprintln!("ferrion-run-probe: invalid pipe_bytes {pipe_bytes_str}");
                return 2;
            }
        };
        if let Ok(extra_str) = std::env::var(EXTRA_FD_VAR)
            && let Ok(efd) = extra_str.parse::<libc::c_int>()
        {
            if unsafe { libc::fcntl(efd, libc::F_GETFD) } >= 0 {
                eprintln!("ferrion-run-probe: EXTRA_FD_LEAKED {efd}");
                return 1;
            }
        }
        let mut sock = unsafe { UnixStream::from_raw_fd(fd) };
        if pipe_bytes == 0 {
            let mut req = [0u8; 4];
            if let Err(e) = sock.read_exact(&mut req) {
                eprintln!("ferrion-run-probe: child read ping: {e}");
                return 1;
            }
            if &req != b"ping" {
                eprintln!("ferrion-run-probe: child unexpected {req:?}");
                return 1;
            }
            if let Err(e) = sock.write_all(b"pong") {
                eprintln!("ferrion-run-probe: child write pong: {e}");
                return 1;
            }
            return 0;
        }
        let mut pfds = [-1 as libc::c_int; 2];
        if unsafe { libc::pipe(pfds.as_mut_ptr()) } != 0 {
            eprintln!(
                "ferrion-run-probe: pipe: {}",
                std::io::Error::last_os_error()
            );
            return 1;
        }
        if let Err(e) = send_fd(sock.as_raw_fd(), pfds[0]) {
            eprintln!("ferrion-run-probe: child send_fd: {e}");
            unsafe {
                libc::close(pfds[0]);
                libc::close(pfds[1]);
            }
            return 1;
        }
        unsafe { libc::close(pfds[0]) };
        let mut writer = unsafe { std::fs::File::from_raw_fd(pfds[1]) };
        let mut chunk = vec![0u8; PIPE_CHUNK];
        let mut rng: u64 = 0x517cc1b727220a95;
        let mut remaining = pipe_bytes;
        let mut cksum: u64 = 0xcbf29ce484222325;
        for _ in 0..MAX_PIPE_ITERS {
            if remaining == 0 {
                break;
            }
            fill_random_chunk(&mut rng, &mut chunk);
            let n = remaining.min(chunk.len());
            mix_cksum(&mut cksum, &chunk[..n]);
            if let Err(e) = writer.write_all(&chunk[..n]) {
                eprintln!("ferrion-run-probe: child pipe write: {e}");
                return 1;
            }
            remaining -= n;
        }
        if remaining != 0 {
            eprintln!("ferrion-run-probe: child pipe incomplete {remaining}");
            return 1;
        }
        drop(writer);
        if let Err(e) = sock.write_all(&cksum.to_le_bytes()) {
            eprintln!("ferrion-run-probe: child write cksum: {e}");
            return 1;
        }
        0
    }

    fn pass_bad(kind: &str, rest: &[&str]) -> i32 {
        let (fd, command) = match (kind, rest) {
            ("file", ["--", command @ ..]) => match std::fs::File::open("/etc/passwd") {
                Ok(f) => (f.into_raw_fd() as usize, command),
                Err(e) => {
                    eprintln!("ferrion-run-probe: open /etc/passwd: {e}");
                    return 2;
                }
            },
            ("pipe", ["--", command @ ..]) => {
                let mut pfds = [-1 as libc::c_int; 2];
                if unsafe { libc::pipe(pfds.as_mut_ptr()) } != 0 {
                    eprintln!(
                        "ferrion-run-probe: pipe: {}",
                        std::io::Error::last_os_error()
                    );
                    return 2;
                }
                unsafe { libc::close(pfds[1]) };
                (pfds[0] as usize, command)
            }
            ("tcp", [address, "--", command @ ..]) => match TcpStream::connect(address) {
                Ok(s) => (s.into_raw_fd() as usize, command),
                Err(e) => {
                    eprintln!("ferrion-run-probe: connect {address}: {e}");
                    return 2;
                }
            },
            _ => {
                eprintln!("ferrion-run-probe: bad pass-bad args {kind} {rest:?}");
                return 2;
            }
        };
        let Some((program, program_args)) = command.split_first() else {
            eprintln!("ferrion-run-probe: pass-bad needs -- PROGRAM");
            return 2;
        };
        if let Err(e) = call::fcntl(fd, F_SETFD, 0) {
            eprintln!("ferrion-run-probe: clear cloexec on {fd}: {e}");
            return 2;
        }
        let fd_str = fd.to_string();
        let resolved: Vec<String> = program_args
            .iter()
            .map(|a| a.replace("{fd}", &fd_str))
            .collect();
        let error = std::process::Command::new(program).args(&resolved).exec();
        eprintln!("ferrion-run-probe: exec {program}: {error}");
        2
    }
}

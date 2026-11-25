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
            ["unix-connect", path] => unix_connect(path),
            ["unix-listen", path, secs] => unix_listen(path, secs.parse().unwrap_or(10)),
            ["wayland-check"] => wayland_check(false, 0),
            ["wayland-check", "--map"] => wayland_check(true, 0),
            ["wayland-check", "--map", secs] => wayland_check(true, secs.parse().unwrap_or(5)),
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

    const MAX_LISTEN_SECS: u64 = 120;
    const MAX_WL_EVENTS: usize = 512;
    const MAX_WL_MSG_BYTES: usize = 4096;
    const WL_WIN_W: u32 = 256;
    const WL_WIN_H: u32 = 256;
    const WL_SHM_FORMAT_XRGB8888: u32 = 1;
    const WL_IDLE_NOTIFIER: &str = "ext_idle_notifier_v1";
    const FORBIDDEN_RESTRICTED_GLOBALS: &[&str] = &[
        "zwlr_screencopy_manager_v1",
        "ext_image_copy_capture_manager_v1",
        "ext_output_image_capture_source_manager_v1",
        "zwlr_virtual_pointer_manager_v1",
        "zwp_virtual_keyboard_manager_v1",
        "zwlr_data_control_manager_v1",
        "ext_data_control_manager_v1",
        "zwlr_layer_shell_v1",
        "ext_session_lock_manager_v1",
        "wp_security_context_manager_v1",
    ];

    fn unix_connect(path: &str) -> i32 {
        match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => {
                println!("UNIX_CONNECT OK");
                0
            }
            Err(e) => {
                println!("UNIX_CONNECT ERR {} {e}", e.raw_os_error().unwrap_or(-1));
                1
            }
        }
    }

    fn unix_listen(path: &str, secs: u64) -> i32 {
        let _ = std::fs::remove_file(path);
        let listener = match std::os::unix::net::UnixListener::bind(path) {
            Ok(l) => l,
            Err(e) => {
                println!("UNIX_LISTEN_ERR {} {e}", e.raw_os_error().unwrap_or(-1));
                return 1;
            }
        };
        let _ = listener.set_nonblocking(true);
        println!("UNIX_LISTENING {path}");
        let _ = std::io::stdout().flush();
        let hold = secs.min(MAX_LISTEN_SECS);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(hold);
        let max_polls = hold.saturating_mul(25).saturating_add(1);
        for _ in 0..max_polls {
            for _ in 0..8 {
                match listener.accept() {
                    Ok((stream, _)) => drop(stream),
                    Err(_) => break,
                }
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        drop(listener);
        let _ = std::fs::remove_file(path);
        0
    }

    fn wl_push_u32(buf: &mut Vec<u8>, v: u32) {
        buf.extend_from_slice(&v.to_ne_bytes());
    }

    fn wl_push_str(buf: &mut Vec<u8>, s: &str) {
        let len = (s.len() + 1) as u32;
        wl_push_u32(buf, len);
        buf.extend_from_slice(s.as_bytes());
        buf.push(0);
        while !buf.len().is_multiple_of(4) {
            buf.push(0);
        }
    }

    fn wl_msg(obj: u32, opcode: u16, payload: &[u8]) -> Vec<u8> {
        let size = 8 + payload.len();
        assert!(size <= MAX_WL_MSG_BYTES && size.is_multiple_of(4));
        let mut out = Vec::with_capacity(size);
        wl_push_u32(&mut out, obj);
        wl_push_u32(&mut out, ((size as u32) << 16) | (opcode as u32));
        out.extend_from_slice(payload);
        out
    }

    fn wl_send_with_fd(sock_fd: i32, data: &[u8], fd: i32) -> std::io::Result<()> {
        let iov = libc::iovec {
            iov_base: data.as_ptr() as *mut libc::c_void,
            iov_len: data.len(),
        };
        let cmsg_space = unsafe { libc::CMSG_SPACE(size_of::<i32>() as u32) as usize };
        let mut ctrl = vec![0u8; cmsg_space];
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &iov as *const libc::iovec as *mut libc::iovec;
        msg.msg_iovlen = 1;
        msg.msg_control = ctrl.as_mut_ptr().cast();
        msg.msg_controllen = ctrl.len() as _;
        let cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        if cmsg.is_null() {
            return Err(std::io::Error::other("CMSG_FIRSTHDR returned null"));
        }
        unsafe {
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(size_of::<i32>() as u32) as _;
            std::ptr::copy_nonoverlapping(
                &fd as *const i32 as *const u8,
                libc::CMSG_DATA(cmsg),
                size_of::<i32>(),
            );
        }
        let sent = unsafe { libc::sendmsg(sock_fd, &msg, 0) };
        if sent < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn wl_read_event(
        stream: &mut std::os::unix::net::UnixStream,
    ) -> std::io::Result<(u32, u16, Vec<u8>)> {
        let mut hdr = [0u8; 8];
        stream.read_exact(&mut hdr)?;
        let obj = u32::from_ne_bytes(hdr[0..4].try_into().unwrap());
        let word = u32::from_ne_bytes(hdr[4..8].try_into().unwrap());
        let opcode = (word & 0xFFFF) as u16;
        let size = (word >> 16) as usize;
        if !(8..=MAX_WL_MSG_BYTES).contains(&size) {
            return Err(std::io::Error::other(format!("bad wl event size {size}")));
        }
        let mut body = vec![0u8; size - 8];
        stream.read_exact(&mut body)?;
        Ok((obj, opcode, body))
    }

    fn wl_parse_u32(body: &[u8], off: &mut usize) -> Option<u32> {
        let end = off.checked_add(4)?;
        let bytes = body.get(*off..end)?;
        *off = end;
        Some(u32::from_ne_bytes(bytes.try_into().unwrap()))
    }

    fn wl_parse_str(body: &[u8], off: &mut usize) -> Option<String> {
        let len = wl_parse_u32(body, off)? as usize;
        if len == 0 || *off + len > body.len() {
            return None;
        }
        let raw = &body[*off..*off + len - 1];
        let padded = (len + 3) & !3;
        *off = off.checked_add(padded)?;
        Some(String::from_utf8_lossy(raw).into_owned())
    }

    fn wayland_check(map_window: bool, hold_secs: u64) -> i32 {
        let Ok(rt) = std::env::var("XDG_RUNTIME_DIR") else {
            println!("WL_CONNECT_ERR 2 XDG_RUNTIME_DIR unset");
            return 1;
        };
        let Ok(disp) = std::env::var("WAYLAND_DISPLAY") else {
            println!("WL_CONNECT_ERR 2 WAYLAND_DISPLAY unset");
            return 1;
        };
        let sock_path = format!("{rt}/{disp}");
        let mut stream = match std::os::unix::net::UnixStream::connect(&sock_path) {
            Ok(s) => s,
            Err(e) => {
                println!("WL_CONNECT_ERR {} {e}", e.raw_os_error().unwrap_or(-1));
                return 1;
            }
        };
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
        let mut init = Vec::new();
        init.extend_from_slice(&wl_msg(1, 1, &2u32.to_ne_bytes()));
        init.extend_from_slice(&wl_msg(1, 0, &3u32.to_ne_bytes()));
        if let Err(e) = stream.write_all(&init) {
            println!("WL_WRITE_ERR {e}");
            return 1;
        }
        let mut compositor: Option<(u32, u32)> = None;
        let mut shm: Option<(u32, u32)> = None;
        let mut xdg_base: Option<(u32, u32)> = None;
        let mut saw_idle_notifier = false;
        let mut forbidden_found: Vec<String> = Vec::new();
        let mut synced = false;
        for _ in 0..MAX_WL_EVENTS {
            let (obj, opcode, body) = match wl_read_event(&mut stream) {
                Ok(ev) => ev,
                Err(e) => {
                    println!("WL_READ_ERR {e}");
                    return 1;
                }
            };
            if obj == 1 && opcode == 0 {
                let mut off = 8;
                let msg = wl_parse_str(&body, &mut off).unwrap_or_default();
                println!("WL_ERROR {msg}");
                return 1;
            }
            if obj == 2 && opcode == 0 {
                let mut off = 0;
                if let (Some(name), Some(iface), Some(ver)) = (
                    wl_parse_u32(&body, &mut off),
                    wl_parse_str(&body, &mut off),
                    wl_parse_u32(&body, &mut off),
                ) {
                    println!("WL_GLOBAL {iface} {ver}");
                    if iface == "wl_compositor" {
                        compositor = Some((name, ver));
                    } else if iface == "wl_shm" {
                        shm = Some((name, ver));
                    } else if iface == "xdg_wm_base" {
                        xdg_base = Some((name, ver));
                    }
                    if iface == WL_IDLE_NOTIFIER {
                        saw_idle_notifier = true;
                    }
                    if FORBIDDEN_RESTRICTED_GLOBALS.contains(&iface.as_str()) {
                        forbidden_found.push(iface);
                    }
                }
            } else if obj == 3 && opcode == 0 {
                synced = true;
                break;
            }
        }
        if !synced {
            println!("WL_SYNC_TIMEOUT");
            return 1;
        }
        if saw_idle_notifier {
            println!("WL_IDLE_NOTIFIER expected-until-O3");
        } else {
            println!("WL_IDLE_NOTIFIER absent");
        }
        println!("WL_FORBIDDEN {}", forbidden_found.len());
        for g in &forbidden_found {
            println!("WL_FORBIDDEN_GLOBAL {g}");
        }
        let _ = std::io::stdout().flush();
        if !forbidden_found.is_empty() {
            return 1;
        }
        if !map_window {
            return 0;
        }
        let (Some((comp_name, comp_ver)), Some((shm_name, _)), Some((xdg_name, xdg_ver))) =
            (compositor, shm, xdg_base)
        else {
            println!("WL_MAP_ERR missing wl_compositor, wl_shm, or xdg_wm_base");
            return 1;
        };
        let mut req = Vec::new();
        let bind_req = |name: u32, iface: &str, ver: u32, new_id: u32| -> Vec<u8> {
            let mut p = Vec::new();
            wl_push_u32(&mut p, name);
            wl_push_str(&mut p, iface);
            wl_push_u32(&mut p, ver);
            wl_push_u32(&mut p, new_id);
            wl_msg(2, 0, &p)
        };
        req.extend_from_slice(&bind_req(comp_name, "wl_compositor", comp_ver.min(4), 4));
        req.extend_from_slice(&bind_req(shm_name, "wl_shm", 1, 5));
        req.extend_from_slice(&bind_req(xdg_name, "xdg_wm_base", xdg_ver.min(2), 6));
        req.extend_from_slice(&wl_msg(4, 0, &7u32.to_ne_bytes()));
        let mut get_xdg = Vec::new();
        wl_push_u32(&mut get_xdg, 8);
        wl_push_u32(&mut get_xdg, 7);
        req.extend_from_slice(&wl_msg(6, 2, &get_xdg));
        req.extend_from_slice(&wl_msg(8, 1, &9u32.to_ne_bytes()));
        let mut title = Vec::new();
        wl_push_str(&mut title, "ferrion-display-probe");
        req.extend_from_slice(&wl_msg(9, 2, &title));
        req.extend_from_slice(&wl_msg(7, 6, &[]));
        if let Err(e) = stream.write_all(&req) {
            println!("WL_MAP_ERR write initial commit: {e}");
            return 1;
        }
        let mut configure_serial: Option<u32> = None;
        for _ in 0..MAX_WL_EVENTS {
            let (obj, opcode, body) = match wl_read_event(&mut stream) {
                Ok(ev) => ev,
                Err(e) => {
                    println!("WL_MAP_ERR configure wait: {e}");
                    return 1;
                }
            };
            if obj == 1 && opcode == 0 {
                let mut off = 8;
                let msg = wl_parse_str(&body, &mut off).unwrap_or_default();
                println!("WL_ERROR {msg}");
                return 1;
            }
            if obj == 6 && opcode == 0 {
                let mut off = 0;
                if let Some(serial) = wl_parse_u32(&body, &mut off) {
                    let _ = stream.write_all(&wl_msg(6, 3, &serial.to_ne_bytes()));
                }
            } else if obj == 8 && opcode == 0 {
                let mut off = 0;
                configure_serial = wl_parse_u32(&body, &mut off);
                break;
            }
        }
        let Some(serial) = configure_serial else {
            println!("WL_MAP_ERR no xdg_surface.configure");
            return 1;
        };
        let shm_size = (WL_WIN_W * WL_WIN_H * 4) as usize;
        let shm_path = format!("/tmp/ferrion-wl-probe-{}.shm", std::process::id());
        let mut shm_file = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&shm_path)
        {
            Ok(f) => f,
            Err(e) => {
                println!("WL_MAP_ERR shm create {shm_path}: {e}");
                return 1;
            }
        };
        let pixel = 0x00FF_4020u32.to_ne_bytes();
        let mut row = Vec::with_capacity((WL_WIN_W * 4) as usize);
        for _ in 0..WL_WIN_W {
            row.extend_from_slice(&pixel);
        }
        for _ in 0..WL_WIN_H {
            if let Err(e) = shm_file.write_all(&row) {
                println!("WL_MAP_ERR shm write: {e}");
                let _ = std::fs::remove_file(&shm_path);
                return 1;
            }
        }
        let _ = shm_file.flush();
        let mut pool_payload = Vec::new();
        wl_push_u32(&mut pool_payload, 10);
        wl_push_u32(&mut pool_payload, shm_size as u32);
        let pool_msg = wl_msg(5, 0, &pool_payload);
        if let Err(e) = wl_send_with_fd(stream.as_raw_fd(), &pool_msg, shm_file.as_raw_fd()) {
            println!("WL_MAP_ERR create_pool sendmsg: {e}");
            let _ = std::fs::remove_file(&shm_path);
            return 1;
        }
        let mut commit_req = Vec::new();
        commit_req.extend_from_slice(&wl_msg(8, 4, &serial.to_ne_bytes()));
        let mut buf_payload = Vec::new();
        wl_push_u32(&mut buf_payload, 11);
        wl_push_u32(&mut buf_payload, 0);
        wl_push_u32(&mut buf_payload, WL_WIN_W);
        wl_push_u32(&mut buf_payload, WL_WIN_H);
        wl_push_u32(&mut buf_payload, WL_WIN_W * 4);
        wl_push_u32(&mut buf_payload, WL_SHM_FORMAT_XRGB8888);
        commit_req.extend_from_slice(&wl_msg(10, 0, &buf_payload));
        let mut attach_payload = Vec::new();
        wl_push_u32(&mut attach_payload, 11);
        wl_push_u32(&mut attach_payload, 0);
        wl_push_u32(&mut attach_payload, 0);
        commit_req.extend_from_slice(&wl_msg(7, 1, &attach_payload));
        let mut damage_payload = Vec::new();
        wl_push_u32(&mut damage_payload, 0);
        wl_push_u32(&mut damage_payload, 0);
        wl_push_u32(&mut damage_payload, WL_WIN_W);
        wl_push_u32(&mut damage_payload, WL_WIN_H);
        commit_req.extend_from_slice(&wl_msg(7, 2, &damage_payload));
        commit_req.extend_from_slice(&wl_msg(7, 6, &[]));
        commit_req.extend_from_slice(&wl_msg(1, 0, &12u32.to_ne_bytes()));
        if let Err(e) = stream.write_all(&commit_req) {
            println!("WL_MAP_ERR commit write: {e}");
            let _ = std::fs::remove_file(&shm_path);
            return 1;
        }
        let mut map_synced = false;
        for _ in 0..MAX_WL_EVENTS {
            let (obj, opcode, body) = match wl_read_event(&mut stream) {
                Ok(ev) => ev,
                Err(e) => {
                    println!("WL_MAP_ERR post-commit wait: {e}");
                    let _ = std::fs::remove_file(&shm_path);
                    return 1;
                }
            };
            if obj == 1 && opcode == 0 {
                let mut off = 8;
                let msg = wl_parse_str(&body, &mut off).unwrap_or_default();
                println!("WL_ERROR {msg}");
                let _ = std::fs::remove_file(&shm_path);
                return 1;
            }
            if obj == 6 && opcode == 0 {
                let mut off = 0;
                if let Some(p) = wl_parse_u32(&body, &mut off) {
                    let _ = stream.write_all(&wl_msg(6, 3, &p.to_ne_bytes()));
                }
            } else if obj == 8 && opcode == 0 {
                let mut off = 0;
                if let Some(s) = wl_parse_u32(&body, &mut off) {
                    let _ = stream.write_all(&wl_msg(8, 4, &s.to_ne_bytes()));
                }
            } else if obj == 12 && opcode == 0 {
                map_synced = true;
                break;
            }
        }
        if !map_synced {
            println!("WL_MAP_ERR sync timeout");
            let _ = std::fs::remove_file(&shm_path);
            return 1;
        }
        println!("WL_MAPPED {WL_WIN_W}x{WL_WIN_H}");
        let _ = std::io::stdout().flush();
        let hold = hold_secs.min(MAX_LISTEN_SECS);
        if hold > 0 {
            let _ = stream.set_nonblocking(true);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(hold);
            let max_polls = hold.saturating_mul(25).saturating_add(1);
            for _ in 0..max_polls {
                if std::time::Instant::now() >= deadline {
                    break;
                }
                if let Ok((obj, opcode, body)) = wl_read_event(&mut stream)
                    && obj == 6
                    && opcode == 0
                {
                    let mut off = 0;
                    if let Some(p) = wl_parse_u32(&body, &mut off) {
                        let _ = stream.write_all(&wl_msg(6, 3, &p.to_ne_bytes()));
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        let _ = std::fs::remove_file(&shm_path);
        0
    }
}

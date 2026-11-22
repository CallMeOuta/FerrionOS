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
    use std::os::fd::IntoRawFd;
    use std::os::unix::process::CommandExt;

    use libredox::call;
    use syscall::flag::{F_DUPFD, F_SETFD, UPPER_FDTBL_TAG};

    const FDS_VAR: &str = "FERRION_RUN_PROBE_FDS";

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
}


use std::process::exit;

use redox_scheme::scheme::{SchemeState, SchemeSync};
use redox_scheme::{CallerCtx, OpenResult, RequestKind, SignalBehavior, Socket};
use syscall::error::{Error, Result, EACCES, EIO};
use syscall::schemev2::NewFdFlags;

enum ServeEnd {
    Closed,
    Failed(Error),
}

enum Outcome {
    Exited(i32),
    Signaled(i32),
    ServerFailed,
}

const PAGE: usize = 4096;
const FILL: u8 = 0xA5;
const ROOT_ID: usize = 1;

struct ShortScheme;

impl SchemeSync for ShortScheme {
    fn scheme_root(&mut self) -> Result<usize> {
        Ok(ROOT_ID)
    }
    fn openat(
        &mut self,
        dirfd: usize,
        _path: &str,
        _flags: usize,
        _fcntl_flags: u32,
        _ctx: &CallerCtx,
    ) -> Result<OpenResult> {
        if dirfd != ROOT_ID {
            return Err(Error::new(EACCES));
        }
        Ok(OpenResult::ThisScheme {
            number: 2,
            flags: NewFdFlags::empty(),
        })
    }
    fn read(
        &mut self,
        _id: usize,
        buf: &mut [u8],
        offset: u64,
        _flags: u32,
        _ctx: &CallerCtx,
    ) -> Result<usize> {
        let count = core::cmp::min(offset as usize, buf.len());
        buf[..count].fill(FILL);
        Ok(count)
    }
}

fn serve(socket: Socket) -> ServeEnd {
    let mut scheme = ShortScheme;
    let mut state = SchemeState::new();
    loop {
        let request = match socket.next_request(SignalBehavior::Restart) {
            Ok(Some(request)) => request,
            Ok(None) => return ServeEnd::Closed,
            Err(err) => return ServeEnd::Failed(err),
        };
        match request.kind() {
            RequestKind::Call(call) => {
                let response = call.handle_sync(&mut scheme, &mut state);
                if let Err(err) = socket.write_response(response, SignalBehavior::Restart) {
                    return ServeEnd::Failed(err);
                }
            }
            RequestKind::OnClose { id } if id == ROOT_ID => {
                return ServeEnd::Closed;
            }
            _ => {}
        }
    }
}

fn pattern(index: usize) -> u8 {
    match (index.wrapping_mul(131).wrapping_add(17) % 251) as u8 {
        0 => 0x5A,
        FILL => 0x3C,
        byte => byte,
    }
}

fn main() {
    let socket = match Socket::create() {
        Ok(socket) => socket,
        Err(err) => {
            println!("scheme-short-read-probe: FAIL setup cannot create a scheme: {err}");
            exit(2);
        }
    };
    let cap = match socket.create_this_scheme_fd(0, ROOT_ID, 0, 0) {
        Ok(cap) => cap,
        Err(err) => {
            println!("scheme-short-read-probe: FAIL setup cannot make the scheme's root: {err}");
            exit(2);
        }
    };
    let child = unsafe { libc::fork() };
    if child < 0 {
        println!("scheme-short-read-probe: FAIL setup fork");
        exit(2);
    }
    if child == 0 {
        drop(socket);
        reader(cap);
    }
    unsafe { libc::close(cap as i32) };
    let server = std::thread::spawn(move || serve(socket));
    let mut status = 0;
    let waited = unsafe { libc::waitpid(child, &mut status, 0) };
    let reader_outcome = if waited != child {
        Outcome::Signaled(-1)
    } else if libc::WIFEXITED(status) {
        Outcome::Exited(libc::WEXITSTATUS(status))
    } else if libc::WIFSIGNALED(status) {
        Outcome::Signaled(libc::WTERMSIG(status))
    } else {
        Outcome::Signaled(-1)
    };
    let serve_end = match server.join() {
        Ok(end) => end,
        Err(_) => ServeEnd::Failed(Error::new(EIO)),
    };
    let outcome = match (reader_outcome, &serve_end) {
        (Outcome::Exited(0), ServeEnd::Failed(_)) => Outcome::ServerFailed,
        (r, _) => r,
    };
    match outcome {
        Outcome::Exited(0) => exit(0),
        Outcome::Exited(code) => exit(code),
        Outcome::Signaled(sig) => {
            println!("scheme-short-read-probe: FAIL the reader did not exit normally (signal {sig}, status {status:#x})");
            exit(2);
        }
        Outcome::ServerFailed => {
            if let ServeEnd::Failed(err) = serve_end {
                println!("scheme-short-read-probe: FAIL server exited with error: {err}");
            }
            exit(2);
        }
    }
}

fn reader(cap: usize) -> ! {
    let fd = match libredox::call::openat(cap, "short", libc::O_RDONLY, 0) {
        Ok(fd) => fd as i32,
        Err(err) => {
            println!("scheme-short-read-probe: FAIL setup cannot open the scheme: {err}");
            exit(2);
        }
    };

    let cases: &[(usize, usize, usize)] = &[
        (100, 64, 8),
        (100, 64, 0),
        (PAGE - 8, 16, 8),
        (4000, 200, 50),
        (4000, 200, 150),
        (4000, 2 * PAGE + 300, 60),
        (4000, 2 * PAGE + 300, 5000),
        (4000, 2 * PAGE + 300, 2 * PAGE + 150),
        (0, PAGE + 100, 20),
        (0, PAGE + 100, PAGE + 40),
        (4000, 200, 200),
    ];

    let mut failures = 0;
    for &(offset, len, count) in cases {
        let total = offset + len + 2 * PAGE;
        let layout = std::alloc::Layout::from_size_align(total, PAGE).unwrap();
        let base = unsafe { std::alloc::alloc(layout) };
        let whole = unsafe { std::slice::from_raw_parts_mut(base, total) };
        for (index, byte) in whole.iter_mut().enumerate() {
            *byte = pattern(index);
        }
        let buf = &mut whole[offset..offset + len];
        let got = unsafe {
            libc::pread(fd, buf.as_mut_ptr().cast(), len, count as libc::off_t)
        };
        let name = format!("offset={offset} len={len} reply={count}");
        if got != count as isize {
            println!("scheme-short-read-probe: FAIL {name} read returned {got}");
            failures += 1;
        } else {
            let (mut zero, mut fill, mut foreign, mut first) = (0, 0, 0, None);
            for (index, &byte) in whole.iter().enumerate() {
                let inside_reply = index >= offset && index < offset + count;
                let ok = if inside_reply { byte == FILL } else { byte == pattern(index) };
                if ok {
                    continue;
                }
                first.get_or_insert(index);
                if inside_reply || byte == pattern(index) {
                    foreign += 1;
                } else if byte == 0 {
                    zero += 1;
                } else if byte == FILL {
                    fill += 1;
                } else {
                    foreign += 1;
                }
            }
            if let Some(first) = first {
                println!(
                    "scheme-short-read-probe: FAIL {name} changed bytes: {zero} zero, {fill} daemon fill, {foreign} foreign; first at buffer index {}",
                    first as isize - offset as isize
                );
                failures += 1;
            } else {
                println!("scheme-short-read-probe: pass {name}");
            }
        }
        unsafe { std::alloc::dealloc(base, layout) };
    }

    unsafe { libc::close(fd) };
    if failures == 0 {
        println!("scheme-short-read-probe: PASS");
        exit(0);
    }
    println!("scheme-short-read-probe: FAIL {failures} of {} cases", cases.len());
    exit(1);
}

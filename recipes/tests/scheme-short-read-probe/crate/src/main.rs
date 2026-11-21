
use std::process::exit;

use redox_scheme::scheme::{SchemeState, SchemeSync};
use redox_scheme::{CallerCtx, OpenResult, RequestKind, SignalBehavior, Socket};
use syscall::error::{Error, Result, EACCES};
use syscall::schemev2::NewFdFlags;

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

fn serve(socket: Socket) -> ! {
    let mut scheme = ShortScheme;
    let mut state = SchemeState::new();
    loop {
        let request = match socket.next_request(SignalBehavior::Restart) {
            Ok(Some(request)) => request,
            _ => exit(0),
        };
        if let RequestKind::Call(call) = request.kind() {
            let response = call.handle_sync(&mut scheme, &mut state);
            if socket.write_response(response, SignalBehavior::Restart).is_err() {
                exit(0);
            }
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
    std::thread::spawn(move || serve(socket));
    let mut status = 0;
    if unsafe { libc::waitpid(child, &mut status, 0) } != child || !libc::WIFEXITED(status) {
        println!("scheme-short-read-probe: FAIL the reader did not exit normally ({status:#x})");
        exit(2);
    }
    exit(libc::WEXITSTATUS(status));
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

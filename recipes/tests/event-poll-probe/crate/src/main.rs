
use std::process::exit;

const PIPES: usize = 200;

fn subscribers() -> usize {
    let stat = std::fs::read_to_string("/scheme/sys/stat").unwrap_or_else(|err| {
        println!("event-poll-probe: FAIL cannot read /scheme/sys/stat: {err}");
        exit(2);
    });
    stat.lines()
        .find_map(|line| line.trim().strip_prefix("event_subcribers:"))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or_else(|| {
            println!("event-poll-probe: FAIL no event_subcribers line in /scheme/sys/stat");
            exit(2);
        })
}

fn main() {
    let mut pipes = Vec::with_capacity(PIPES);
    for _ in 0..PIPES {
        let mut ends = [0; 2];
        if unsafe { libc::pipe(ends.as_mut_ptr()) } != 0 {
            println!("event-poll-probe: FAIL pipe");
            exit(2);
        }
        pipes.push(ends);
    }
    let mut warm = libc::pollfd { fd: pipes[0][0], events: libc::POLLIN, revents: 0 };
    unsafe { libc::poll(&mut warm, 1, 0) };

    let before = subscribers();
    for ends in &pipes {
        let mut pfd = libc::pollfd { fd: ends[0], events: libc::POLLIN, revents: 0 };
        let ready = unsafe { libc::poll(&mut pfd, 1, 0) };
        if ready != 0 {
            println!("event-poll-probe: FAIL poll on an empty pipe returned {ready}");
            exit(1);
        }
    }
    let after = subscribers();
    for ends in &pipes {
        unsafe {
            libc::close(ends[0]);
            libc::close(ends[1]);
        }
    }
    let grown = after.saturating_sub(before);
    if grown >= PIPES / 2 {
        println!("event-poll-probe: FAIL subscribers {before} before, {after} after {PIPES} empty polls");
        exit(1);
    }
    println!("event-poll-probe: PASS subscribers {before} before, {after} after {PIPES} empty polls");
}

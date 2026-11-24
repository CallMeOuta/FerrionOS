use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use wayland_backend::client::{ReadEventsGuard, WaylandError};
use wayland_client::Connection;

use crate::platform::layer_shell_machine::watch_loop;
use crate::runtime::Waker;

const MAX_SIGNAL_RETRIES: usize = 64;
const WATCH_WAIT_BOUND: Duration = Duration::from_millis(100);

#[cfg(target_os = "linux")]
#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn create_eventfd() -> RawFd {
    unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) }
}

#[cfg(not(target_os = "linux"))]
#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn create_pipe() -> Option<[RawFd; 2]> {
    let mut fds = [0 as RawFd; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
        return None;
    }
    assert!(fds[0] >= 0 && fds[1] >= 0);
    Some(fds)
}

#[cfg(not(target_os = "linux"))]
#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn set_fd_cloexec(fd: RawFd) {
    assert!(fd >= 0);
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
    }
}

#[cfg(not(target_os = "linux"))]
#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn set_fd_nonblock(fd: RawFd) {
    assert!(fd >= 0);
    unsafe {
        libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
    }
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn own_fd(raw: RawFd) -> OwnedFd {
    assert!(raw >= 0);
    unsafe { OwnedFd::from_raw_fd(raw) }
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn write_signal_bytes(fd: RawFd, buf: &[u8]) -> isize {
    assert!(fd >= 0);
    assert!(!buf.is_empty());
    unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) }
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn poll_two_fds(pfds: &mut [libc::pollfd; 2]) -> i32 {
    assert_eq!(pfds.len(), 2);
    assert!(pfds[0].fd >= 0 && pfds[1].fd >= 0);
    unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, -1) }
}

struct StopSignal {
    #[cfg(target_os = "linux")]
    fd: OwnedFd,
    #[cfg(not(target_os = "linux"))]
    read: OwnedFd,
    #[cfg(not(target_os = "linux"))]
    write: OwnedFd,
}

impl StopSignal {
    fn new() -> Option<Self> {
        #[cfg(target_os = "linux")]
        {
            let fd = create_eventfd();
            if fd < 0 {
                return None;
            }
            Some(Self { fd: own_fd(fd) })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let fds = create_pipe()?;
            for fd in fds {
                set_fd_cloexec(fd);
                set_fd_nonblock(fd);
            }
            Some(Self {
                read: own_fd(fds[0]),
                write: own_fd(fds[1]),
            })
        }
    }

    fn poll_fd(&self) -> RawFd {
        #[cfg(target_os = "linux")]
        {
            self.fd.as_raw_fd()
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.read.as_raw_fd()
        }
    }

    fn signal(&self) {
        #[cfg(target_os = "linux")]
        let (fd, buf) = (self.fd.as_raw_fd(), 1u64.to_ne_bytes());
        #[cfg(not(target_os = "linux"))]
        let (fd, buf) = (self.write.as_raw_fd(), [1u8]);
        for _ in 0..MAX_SIGNAL_RETRIES {
            let ret = write_signal_bytes(fd, &buf);
            if ret >= 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            {
                break;
            }
        }
    }
}

struct WatchTurns {
    count: u64,
    stopped: bool,
}

pub(crate) struct ShmWatch {
    stop: Arc<StopSignal>,
    turns: Arc<(Mutex<WatchTurns>, Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct ShmWatchDriver {
    conn: Connection,
    stop: Arc<StopSignal>,
    turns: Arc<(Mutex<WatchTurns>, Condvar)>,
    wake: Waker,
}

fn map_watch_dispatch(result: Result<usize, WaylandError>) -> watch_loop::Dispatch {
    match result {
        Ok(n) => watch_loop::Dispatch::Events(n),
        Err(WaylandError::Io(err)) if err.kind() == std::io::ErrorKind::WouldBlock => {
            watch_loop::Dispatch::WouldBlock
        }
        Err(_) => watch_loop::Dispatch::Error,
    }
}

impl watch_loop::Driver for ShmWatchDriver {
    type Guard = ReadEventsGuard;

    fn stopped(&self) -> bool {
        self.turns
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stopped
    }

    fn prepare_read(&mut self) -> Option<ReadEventsGuard> {
        self.conn.prepare_read()
    }

    fn dispatch_inner_queue(&mut self) -> watch_loop::Dispatch {
        map_watch_dispatch(self.conn.backend().dispatch_inner_queue())
    }

    fn poll(&mut self, guard: &ReadEventsGuard) -> watch_loop::Poll {
        let mut pfds = [
            libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.stop.poll_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let ret = poll_two_fds(&mut pfds);
        if ret < 0 {
            return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                watch_loop::Poll::Interrupted
            } else {
                watch_loop::Poll::Error
            };
        }
        if ret == 0 {
            return watch_loop::Poll::Interrupted;
        }
        if pfds[1].revents != 0 {
            return watch_loop::Poll::Stop;
        }
        if pfds[0].revents & libc::POLLNVAL != 0 {
            return watch_loop::Poll::Error;
        }
        if pfds[0].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0 {
            return watch_loop::Poll::Readable;
        }
        watch_loop::Poll::Error
    }

    fn read(&mut self, guard: ReadEventsGuard) -> watch_loop::Dispatch {
        map_watch_dispatch(guard.read())
    }

    fn wake_and_wait(&mut self) {
        let (lock, cond) = &*self.turns;
        let before = {
            let state = lock.lock().unwrap_or_else(|e| e.into_inner());
            if state.stopped {
                return;
            }
            state.count
        };
        (self.wake)();
        let state = lock.lock().unwrap_or_else(|e| e.into_inner());
        drop(cond.wait_timeout_while(state, WATCH_WAIT_BOUND, |s| s.count == before && !s.stopped));
    }
}

impl ShmWatch {
    pub(crate) fn spawn(conn: Connection, wake: Waker) -> Option<Self> {
        let stop = Arc::new(StopSignal::new()?);
        let turns = Arc::new((
            Mutex::new(WatchTurns {
                count: 0,
                stopped: false,
            }),
            Condvar::new(),
        ));
        let mut driver = ShmWatchDriver {
            conn,
            stop: stop.clone(),
            turns: turns.clone(),
            wake,
        };
        let thread = std::thread::Builder::new()
            .name("rune-shm-watch".into())
            .spawn(move || watch_loop::run(&mut driver))
            .ok()?;
        Some(Self {
            stop,
            turns,
            thread: Some(thread),
        })
    }

    pub(crate) fn turned(&self) {
        let (lock, cond) = &*self.turns;
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        state.count = state.count.wrapping_add(1);
        cond.notify_all();
    }
}

impl Drop for ShmWatch {
    fn drop(&mut self) {
        {
            let (lock, cond) = &*self.turns;
            lock.lock().unwrap_or_else(|e| e.into_inner()).stopped = true;
            cond.notify_all();
        }
        self.stop.signal();
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {}
    }
}

//! FerrionOS audio service. Device access is acquired before entering its namespace.
use daemon::SchemeDaemon;
use ioslice::IoSlice;
use libredox::{flag, Fd};
use redox_scheme::Socket;
use scheme_utils::ReadinessBased;
use std::sync::{Arc, Mutex};

mod scheme;

extern "C" fn wake(_: usize) {}

fn run(daemon: SchemeDaemon) -> anyhow::Result<()> {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = wake as *const () as usize;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
    }
    libredox::call::sigaction(flag::SIGUSR1, Some(&action), None)?;
    let pid = libredox::call::getpid()?;
    let hardware = Fd::open("/scheme/audiohw", flag::O_WRONLY | flag::O_CLOEXEC, 0)?;
    let socket = Socket::create()?;
    let state = Arc::new(Mutex::new(scheme::State::new()));
    let mut scheme = scheme::AudioScheme(state.clone());
    daemon.ready_sync_scheme(&socket, &mut scheme)?;
    let namespace = libredox::call::mkns(&[IoSlice::new(b"memory"), IoSlice::new(b"rand")])?;
    let previous = libredox::call::setns(namespace)?;
    libredox::call::close(previous)?;
    std::thread::spawn(move || {
        let result = (|| -> libredox::error::Result<()> {
            loop {
                let samples = state.lock().unwrap().mix();
                libredox::call::kill(pid, flag::SIGUSR1 as u32)?;
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        samples.as_ptr().cast::<u8>(),
                        std::mem::size_of_val(&samples),
                    )
                };
                let mut offset = 0;
                while offset < bytes.len() {
                    let written = hardware.write(&bytes[offset..])?;
                    if written == 0 {
                        return Err(libredox::error::Error::new(libredox::errno::EIO));
                    }
                    offset += written;
                }
            }
        })();
        if let Err(error) = result {
            eprintln!("ferrion-audiod: device: {error}");
            std::process::exit(1);
        }
    });
    let mut requests = ReadinessBased::new(Box::new(socket), 16);
    loop {
        requests.read_and_process_requests(&mut scheme)?;
        requests.poll_all_requests(&mut scheme)?;
        requests.write_responses()?;
    }
}

fn main() {
    SchemeDaemon::new(|daemon| {
        if let Err(error) = run(daemon) {
            eprintln!("ferrion-audiod: {error}");
        }
        std::process::exit(1)
    });
}

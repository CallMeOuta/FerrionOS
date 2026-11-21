//! Random numbers for the stack, read from the system's random scheme.
//!
//! The scheme is opened once, before the stack enters the null namespace, and
//! read again for every value: the engine's seed at start and each local port.

use std::cell::RefCell;
use std::fs::File;
use std::io::{self, Read};

thread_local! {
    static SOURCE: RefCell<Option<File>> = const { RefCell::new(None) };
}

/// Opens the random scheme. Call before entering the null namespace.
pub fn init() -> io::Result<()> {
    let file = File::open("/scheme/rand")?;
    SOURCE.with(|source| *source.borrow_mut() = Some(file));
    Ok(())
}

pub fn fill(buf: &mut [u8]) -> io::Result<()> {
    SOURCE.with(|source| match source.borrow_mut().as_mut() {
        Some(file) => file.read_exact(buf),
        None => Err(io::Error::other("the random scheme was not opened")),
    })
}

pub fn u32() -> io::Result<u32> {
    let mut bytes = [0; 4];
    fill(&mut bytes)?;
    Ok(u32::from_ne_bytes(bytes))
}

pub fn u64() -> io::Result<u64> {
    let mut bytes = [0; 8];
    fill(&mut bytes)?;
    Ok(u64::from_ne_bytes(bytes))
}

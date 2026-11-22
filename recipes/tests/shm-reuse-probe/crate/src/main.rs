// SPDX-License-Identifier: MIT
use std::{
    fs::File,
    io::{self, Write},
    os::fd::AsRawFd,
};

struct Mapping {
    address: *mut u8,
    len: usize,
}
impl Mapping {
    fn new(file: &File, len: usize, writable: bool) -> io::Result<Self> {
        let protection = libc::PROT_READ | if writable { libc::PROT_WRITE } else { 0 };
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                protection,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            address: address.cast(),
            len,
        })
    }
    fn write_pattern(&mut self, seed: usize) {
        for index in 0..self.len {
            unsafe {
                self.address.add(index).write_volatile(pattern(seed, index));
            }
        }
    }
    fn check(&self, seed: usize) -> io::Result<()> {
        for index in 0..self.len {
            let seen = unsafe { self.address.add(index).read_volatile() };
            let expected = pattern(seed, index);
            if seen != expected {
                return Err(io::Error::other(format!(
                    "seed={seed} byte={index} saw={seen:02x} expected={expected:02x}"
                )));
            }
        }
        Ok(())
    }
}
impl Drop for Mapping {
    fn drop(&mut self) {
        assert_eq!(unsafe { libc::munmap(self.address.cast(), self.len) }, 0);
    }
}
fn pattern(seed: usize, index: usize) -> u8 {
    ((seed * 71 + index / 4096 * 17 + index / 4 * 3 + index % 4) % 251) as u8
}
fn exercise(unlink_first: bool, mapped_write: bool, hold_previous: bool) -> io::Result<()> {
    let mut previous: Option<(Mapping, File, usize)> = None;
    for iteration in 0..96 {
        let len = [409, 620, 209][iteration % 3] * 730 * 4;
        let (mut file, path) = tempfile::NamedTempFile::new()?.into_parts();
        let path = if unlink_first {
            path.close()?;
            None
        } else {
            Some(path)
        };
        file.set_len(len as u64)?;
        if mapped_write {
            let mut writer = Mapping::new(&file, len, true)?;
            writer.write_pattern(iteration);
        } else {
            let bytes: Vec<_> = (0..len).map(|i| pattern(iteration, i)).collect();
            file.write_all(&bytes)?;
        }
        let other = file.try_clone()?;
        let reader = Mapping::new(&other, len, false)?;
        reader.check(iteration)?;
        if let Some((mapping, _, seed)) = &previous {
            mapping.check(*seed)?;
        }
        previous = if hold_previous {
            Some((reader, other, iteration))
        } else {
            None
        };
        drop(file);
        if let Some(path) = path {
            path.close()?;
        }
    }
    Ok(())
}
fn main() -> std::process::ExitCode {
    println!(
        "shm-reuse-probe: directory={}",
        std::env::temp_dir().display()
    );
    let mut failures = 0;
    for unlink_first in [false, true] {
        for mapped_write in [false, true] {
            for hold_previous in [false, true] {
                let case = format!("unlink_first={unlink_first} mapped_write={mapped_write} retain_mapping={hold_previous}");
                match exercise(unlink_first, mapped_write, hold_previous) {
                    Ok(()) => println!("shm-reuse-probe: PASS {case} (96 cycles)"),
                    Err(error) => {
                        println!("shm-reuse-probe: FAIL {case}: {error}");
                        failures += 1;
                    }
                }
            }
        }
    }
    if failures == 0 {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

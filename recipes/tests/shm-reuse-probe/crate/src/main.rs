// SPDX-License-Identifier: MIT
use std::{
    fs::File,
    io::{self, Write},
    os::fd::AsRawFd,
    os::unix::fs::FileExt,
};

const CYCLES: usize = 96;
const MAX_READS: usize = 4096;

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
    for iteration in 0..CYCLES {
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
fn read_through_descriptor(file: &File, len: usize, seed: usize, when: &str) -> io::Result<()> {
    let mut bytes = vec![0xa5u8; len + 4096];
    let mut total = 0;
    for _ in 0..MAX_READS {
        let count = file.read_at(&mut bytes[total..], total as u64)?;
        if count == 0 {
            break;
        }
        total += count;
        if total == bytes.len() {
            break;
        }
    }
    if total != len {
        return Err(io::Error::other(format!(
            "{when}: seed={seed} read returned {total} of {len} bytes"
        )));
    }
    if let Some(index) = (0..len).find(|&index| bytes[index] != pattern(seed, index)) {
        return Err(io::Error::other(format!(
            "{when}: seed={seed} byte={index} read={:02x} expected={:02x}",
            bytes[index],
            pattern(seed, index)
        )));
    }
    let size = file.metadata()?.len();
    if size != len as u64 {
        return Err(io::Error::other(format!(
            "{when}: seed={seed} size={size} expected={len}"
        )));
    }
    Ok(())
}
fn read_back(unlink_first: bool, unmap_first: bool) -> io::Result<()> {
    for iteration in 0..CYCLES {
        let len = [409, 620, 209][iteration % 3] * 730 * 4;
        let (file, path) = tempfile::NamedTempFile::new()?.into_parts();
        let path = if unlink_first {
            path.close()?;
            None
        } else {
            Some(path)
        };
        file.set_len(len as u64)?;
        let mut writer = Mapping::new(&file, len, true)?;
        writer.write_pattern(iteration);
        if unmap_first {
            drop(writer);
            read_through_descriptor(&file, len, iteration, "after munmap")?;
        } else {
            read_through_descriptor(&file, len, iteration, "while mapped")?;
            drop(writer);
            read_through_descriptor(&file, len, iteration, "after munmap")?;
        }
        drop(file);
        if let Some(path) = path {
            path.close()?;
        }
    }
    Ok(())
}
fn read_back_cases() -> usize {
    let mut failures = 0;
    for unlink_first in [false, true] {
        for unmap_first in [false, true] {
            let case = format!("read-back unlink_first={unlink_first} unmap_first={unmap_first}");
            match read_back(unlink_first, unmap_first) {
                Ok(()) => println!("shm-reuse-probe: PASS {case} ({CYCLES} cycles)"),
                Err(error) => {
                    println!("shm-reuse-probe: FAIL {case}: {error}");
                    failures += 1;
                }
            }
        }
    }
    failures
}
fn main() -> std::process::ExitCode {
    println!(
        "shm-reuse-probe: directory={}",
        std::env::temp_dir().display()
    );
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("read-back") => {
            return if read_back_cases() == 0 {
                std::process::ExitCode::SUCCESS
            } else {
                std::process::ExitCode::FAILURE
            };
        }
        Some(_) => {
            eprintln!("usage: shm-reuse-probe [read-back]");
            return std::process::ExitCode::from(2);
        }
    }
    let mut failures = 0;
    for unlink_first in [false, true] {
        for mapped_write in [false, true] {
            for hold_previous in [false, true] {
                let case = format!("unlink_first={unlink_first} mapped_write={mapped_write} retain_mapping={hold_previous}");
                match exercise(unlink_first, mapped_write, hold_previous) {
                    Ok(()) => println!("shm-reuse-probe: PASS {case} ({CYCLES} cycles)"),
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

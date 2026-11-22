// SPDX-License-Identifier: MIT
// Retain shared storage for the window; write only released buffer slots.
use std::{
    fs::File,
    io,
    os::fd::{AsFd, AsRawFd, BorrowedFd},
};

pub struct Storage {
    file: File,
    address: *mut u8,
    len: usize,
}
impl Storage {
    pub fn new(len: usize) -> io::Result<Self> {
        let file = tempfile::tempfile()?;
        file.set_len(len as u64)?;
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            file,
            address: address.cast(),
            len,
        })
    }
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
    pub fn write(&mut self, offset: usize, bytes: &[u8]) {
        assert!(offset <= self.len && bytes.len() <= self.len - offset);
        // The release event guarantees that the compositor no longer reads this slot.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.address.add(offset), bytes.len());
        }
    }
}
impl Drop for Storage {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.address.cast(), self.len);
        }
    }
}

use core::ffi::VaList;

use crate::{
    header::{
        stdio::{FILE, fgets, vsnprintf},
        stdlib::abort,
        string::{memcpy, memmove, memset, stpcpy, stpncpy, strcpy, strlen, strncpy, strnlen},
        unistd::{getcwd, pread, read, readlink, readlinkat},
    },
    platform::types::{c_char, c_int, c_void, off_t, size_t, ssize_t},
};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __chk_fail() -> ! {
    log::error!("Buffer overflow detected");
    unsafe { abort() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __memcpy_chk(
    dest: *mut c_void,
    src: *const c_void,
    len: size_t,
    destlen: size_t,
) -> *mut c_void {
    if len > destlen {
        unsafe { __chk_fail() };
    }
    unsafe { memcpy(dest, src, len) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __memmove_chk(
    dest: *mut c_void,
    src: *const c_void,
    len: size_t,
    destlen: size_t,
) -> *mut c_void {
    if len > destlen {
        unsafe { __chk_fail() };
    }
    unsafe { memmove(dest, src, len) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __memset_chk(
    dest: *mut c_void,
    c: c_int,
    len: size_t,
    destlen: size_t,
) -> *mut c_void {
    if len > destlen {
        unsafe { __chk_fail() };
    }
    unsafe { memset(dest, c, len) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __strcpy_chk(
    dest: *mut c_char,
    src: *const c_char,
    destlen: size_t,
) -> *mut c_char {
    let slen = unsafe { strlen(src) };
    if slen >= destlen {
        unsafe { __chk_fail() };
    }
    unsafe { strcpy(dest, src) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __stpcpy_chk(
    dest: *mut c_char,
    src: *const c_char,
    destlen: size_t,
) -> *mut c_char {
    let slen = unsafe { strlen(src) };
    if slen >= destlen {
        unsafe { __chk_fail() };
    }
    unsafe { stpcpy(dest, src) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __strncpy_chk(
    dest: *mut c_char,
    src: *const c_char,
    len: size_t,
    destlen: size_t,
) -> *mut c_char {
    if len > destlen {
        unsafe { __chk_fail() };
    }
    unsafe { strncpy(dest, src, len) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __stpncpy_chk(
    dest: *mut c_char,
    src: *const c_char,
    len: size_t,
    destlen: size_t,
) -> *mut c_char {
    if len > destlen {
        unsafe { __chk_fail() };
    }
    unsafe { stpncpy(dest, src, len) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __strcat_chk(
    dest: *mut c_char,
    src: *const c_char,
    destlen: size_t,
) -> *mut c_char {
    let dlen = unsafe { strnlen(dest, destlen) };
    if dlen == destlen {
        unsafe { __chk_fail() };
    }
    let slen = unsafe { strlen(src) };
    if slen >= destlen - dlen {
        unsafe { __chk_fail() };
    }
    unsafe {
        memcpy(
            dest.add(dlen).cast::<c_void>(),
            src.cast::<c_void>(),
            slen + 1,
        );
    }
    dest
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __strncat_chk(
    dest: *mut c_char,
    src: *const c_char,
    n: size_t,
    destlen: size_t,
) -> *mut c_char {
    let dlen = unsafe { strnlen(dest, destlen) };
    if dlen == destlen {
        unsafe { __chk_fail() };
    }
    let slen = unsafe { strnlen(src, n) };
    if slen >= destlen - dlen {
        unsafe { __chk_fail() };
    }
    unsafe {
        memcpy(dest.add(dlen).cast::<c_void>(), src.cast::<c_void>(), slen);
        *dest.add(dlen + slen) = 0;
    }
    dest
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __vsnprintf_chk(
    s: *mut c_char,
    maxlen: size_t,
    _flag: c_int,
    slen: size_t,
    format: *const c_char,
    ap: VaList,
) -> c_int {
    if maxlen > slen {
        unsafe { __chk_fail() };
    }
    unsafe { vsnprintf(s, maxlen, format, ap) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __snprintf_chk(
    s: *mut c_char,
    maxlen: size_t,
    flag: c_int,
    slen: size_t,
    format: *const c_char,
    __valist: ...
) -> c_int {
    unsafe { __vsnprintf_chk(s, maxlen, flag, slen, format, __valist) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __vsprintf_chk(
    s: *mut c_char,
    _flag: c_int,
    slen: size_t,
    format: *const c_char,
    ap: VaList,
) -> c_int {
    if slen == 0 {
        unsafe { __chk_fail() };
    }
    let written = unsafe { vsnprintf(s, slen, format, ap) };
    if written >= 0 && (written as size_t) >= slen {
        unsafe { __chk_fail() };
    }
    written
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __sprintf_chk(
    s: *mut c_char,
    flag: c_int,
    slen: size_t,
    format: *const c_char,
    __valist: ...
) -> c_int {
    unsafe { __vsprintf_chk(s, flag, slen, format, __valist) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __fgets_chk(
    s: *mut c_char,
    buflen: size_t,
    n: c_int,
    stream: *mut FILE,
) -> *mut c_char {
    if n < 0 || (n as size_t) > buflen {
        unsafe { __chk_fail() };
    }
    unsafe { fgets(s, n, stream) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __read_chk(
    fd: c_int,
    buf: *mut c_void,
    nbytes: size_t,
    buflen: size_t,
) -> ssize_t {
    if nbytes > buflen {
        unsafe { __chk_fail() };
    }
    unsafe { read(fd, buf, nbytes) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __pread_chk(
    fd: c_int,
    buf: *mut c_void,
    nbytes: size_t,
    offset: off_t,
    buflen: size_t,
) -> ssize_t {
    if nbytes > buflen {
        unsafe { __chk_fail() };
    }
    unsafe { pread(fd, buf, nbytes, offset) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __readlink_chk(
    path: *const c_char,
    buf: *mut c_char,
    len: size_t,
    buflen: size_t,
) -> ssize_t {
    if len > buflen {
        unsafe { __chk_fail() };
    }
    unsafe { readlink(path, buf, len) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __readlinkat_chk(
    dirfd: c_int,
    path: *const c_char,
    buf: *mut c_char,
    len: size_t,
    buflen: size_t,
) -> ssize_t {
    if len > buflen {
        unsafe { __chk_fail() };
    }
    unsafe { readlinkat(dirfd, path, buf, len) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __getcwd_chk(
    buf: *mut c_char,
    len: size_t,
    buflen: size_t,
) -> *mut c_char {
    if len > buflen {
        unsafe { __chk_fail() };
    }
    unsafe { getcwd(buf, len) }
}

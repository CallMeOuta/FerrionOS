#ifndef _RELIBC_BITS_FORTIFY_UNISTD_H
#define _RELIBC_BITS_FORTIFY_UNISTD_H

#if defined(__USE_FORTIFY_LEVEL) && __USE_FORTIFY_LEVEL > 0

#ifdef __cplusplus
extern "C" {
#endif

ssize_t __read_chk(int fd, void *buf, size_t nbytes, size_t buflen);
ssize_t __pread_chk(int fd, void *buf, size_t nbytes, off_t offset, size_t buflen);
ssize_t __readlink_chk(const char *path, char *buf, size_t len, size_t buflen);
ssize_t __readlinkat_chk(int dirfd, const char *path, char *buf, size_t len, size_t buflen);
char *__getcwd_chk(char *buf, size_t len, size_t buflen);

ssize_t __ferrion_read_alias(int fildes, void *buf, size_t nbyte) __asm__("read");
ssize_t __ferrion_pread_alias(int fildes, void *buf, size_t nbyte, off_t offset) __asm__("pread");
ssize_t __ferrion_readlink_alias(const char *path, char *buf, size_t bufsize) __asm__("readlink");
ssize_t __ferrion_readlinkat_alias(int dirfd, const char *pathname, char *buf, size_t len) __asm__("readlinkat");
char *__ferrion_getcwd_alias(char *buf, size_t size) __asm__("getcwd");

__ferrion_fortify_inline ssize_t read(int fildes, void *buf, size_t nbyte) {
    size_t __sz = __ferrion_bos0(buf);
    if (__sz != (size_t)-1) {
        return __read_chk(fildes, buf, nbyte, __sz);
    }
    return __ferrion_read_alias(fildes, buf, nbyte);
}

__ferrion_fortify_inline ssize_t pread(int fildes, void *buf, size_t nbyte, off_t offset) {
    size_t __sz = __ferrion_bos0(buf);
    if (__sz != (size_t)-1) {
        return __pread_chk(fildes, buf, nbyte, offset, __sz);
    }
    return __ferrion_pread_alias(fildes, buf, nbyte, offset);
}

__ferrion_fortify_inline ssize_t readlink(const char *path, char *buf, size_t bufsize) {
    size_t __sz = __ferrion_bos(buf);
    if (__sz != (size_t)-1) {
        return __readlink_chk(path, buf, bufsize, __sz);
    }
    return __ferrion_readlink_alias(path, buf, bufsize);
}

__ferrion_fortify_inline ssize_t readlinkat(int dirfd, const char *pathname, char *buf, size_t len) {
    size_t __sz = __ferrion_bos(buf);
    if (__sz != (size_t)-1) {
        return __readlinkat_chk(dirfd, pathname, buf, len, __sz);
    }
    return __ferrion_readlinkat_alias(dirfd, pathname, buf, len);
}

__ferrion_fortify_inline char *getcwd(char *buf, size_t size) {
    size_t __sz = __ferrion_bos(buf);
    if (__sz != (size_t)-1) {
        return __getcwd_chk(buf, size, __sz);
    }
    return __ferrion_getcwd_alias(buf, size);
}

#ifdef __cplusplus
}
#endif

#endif

#endif

crates/rune/src/platform/wayland_shm.rs | mmap_shm | `fd` is asserted non-negative and open via `ftruncate` to `bytes` in `1..=MAX_SHM_BYTES`, and `MAP_FAILED`/null returns are checked before wrapping in `NonNull`.
crates/rune/src/platform/wayland_shm.rs | munmap_shm | `ptr` and `bytes` come exclusively from `ShmSlot`'s matching `mmap_shm` mapping, asserted in `1..=MAX_SHM_BYTES`, and unmapped once in `ShmSlot::drop` after destroying the Wayland buffer and pool.
crates/rune/src/platform/wayland_shm.rs | memfd_create_shm | passes a valid NUL-terminated static C string literal and `MFD_CLOEXEC` to `libc::memfd_create` on Linux and checks the returned fd before use.
crates/rune/src/platform/wayland_shm.rs | shm_open_exclusive | `name` is asserted NUL-terminated before calling `libc::shm_open` with `O_RDWR|O_CREAT|O_EXCL` and mode `0600`.
crates/rune/src/platform/wayland_shm.rs | shm_unlink_name | `name` is asserted NUL-terminated and unlinked immediately after `shm_open_exclusive` succeeded.
crates/rune/src/platform/wayland_shm.rs | set_fd_cloexec | `fd` is asserted non-negative and freshly opened before applying `F_SETFD`/`FD_CLOEXEC`.
crates/rune/src/platform/wayland_shm.rs | own_fd | `raw` is asserted non-negative and freshly created by `memfd_create_shm` or `shm_open_exclusive`, transferring sole ownership into `OwnedFd`.
crates/rune/src/platform/wayland_shm.rs | ftruncate_fd | `fd` is asserted non-negative and owned by this slot while `size` is asserted positive and within `MAX_SHM_BYTES`.
crates/rune/src/platform/wayland_shm.rs | copy_slot_bytes | `src` and `dst` are asserted distinct non-overlapping slot mappings (`dst` not busy) and `offset + len` is asserted within both `src_len` and `dst_len`.
crates/rune/src/platform/wayland_shm.rs | ShmSlot::bytes_mut | asserts `!self.busy`, `0 < self.len <= MAX_SHM_BYTES`, and `self.stride * self.height == self.len` before exposing `&mut [u8]` over the exclusive slot mapping.
crates/rune/src/platform/wayland_shm.rs | foreign_display_backend | `display` is asserted non-null and comes from winit's `wl_display`, which outlives every window and is never closed by a foreign `Backend`.
crates/rune/src/platform/wayland_shm.rs | foreign_surface_id | `surface` is asserted non-null and comes from winit's live `wl_surface` handle for the active window.
crates/rune/src/platform/wayland_shm/watch.rs | create_eventfd | creates a fresh close-on-exec, non-blocking `eventfd` on Linux and checks `< 0` before wrapping.
crates/rune/src/platform/wayland_shm/watch.rs | create_pipe | passes a valid 2-element `RawFd` array to `libc::pipe` on non-Linux Unix and asserts both returned fds are non-negative on success.
crates/rune/src/platform/wayland_shm/watch.rs | set_fd_cloexec | `fd` is asserted non-negative and freshly created by `create_pipe` before setting `FD_CLOEXEC`.
crates/rune/src/platform/wayland_shm/watch.rs | set_fd_nonblock | `fd` is asserted non-negative and freshly created by `create_pipe` before setting `O_NONBLOCK`.
crates/rune/src/platform/wayland_shm/watch.rs | own_fd | `raw` is asserted non-negative and freshly created by `create_eventfd` or `create_pipe`, transferring sole ownership into `OwnedFd`.
crates/rune/src/platform/wayland_shm/watch.rs | write_signal_bytes | `fd` is asserted non-negative and owned by `StopSignal`, and `buf` is asserted non-empty and valid for `buf.len()` bytes.
crates/rune/src/platform/wayland_shm/watch.rs | poll_two_fds | asserts `pfds.len() == 2` and both descriptors (the live Wayland connection fd borrowed by `ReadEventsGuard` and `StopSignal`'s open fd) are non-negative before calling `libc::poll`.

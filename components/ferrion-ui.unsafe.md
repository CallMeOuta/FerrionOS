crates/rune/src/platform/wayland_shm/slots.rs | mmap_shm | `fd` is asserted non-negative and open via `ftruncate` to `bytes` in `1..=MAX_SHM_BYTES`, and `MAP_FAILED`/null returns are checked before wrapping in `NonNull`.
crates/rune/src/platform/wayland_shm/slots.rs | munmap_shm | `ptr` and `bytes` come exclusively from `ShmSlot`'s matching `mmap_shm` mapping, asserted in `1..=MAX_SHM_BYTES`, and unmapped once in `ShmSlot::drop` after destroying the Wayland buffer and pool.
crates/rune/src/platform/wayland_shm/slots.rs | memfd_create_shm | passes a valid NUL-terminated static C string literal and `MFD_CLOEXEC` to `libc::memfd_create` on Linux and checks the returned fd before use.
crates/rune/src/platform/wayland_shm/slots.rs | shm_open_exclusive | `name` is asserted NUL-terminated before calling `libc::shm_open` with `O_RDWR|O_CREAT|O_EXCL` and mode `0600`.
crates/rune/src/platform/wayland_shm/slots.rs | shm_unlink_name | `name` is asserted NUL-terminated and unlinked immediately after `shm_open_exclusive` succeeded.
crates/rune/src/platform/wayland_shm/slots.rs | set_fd_cloexec | `fd` is asserted non-negative and freshly opened before applying `F_SETFD`/`FD_CLOEXEC`.
crates/rune/src/platform/wayland_shm/slots.rs | own_fd | `raw` is asserted non-negative and freshly created by `memfd_create_shm` or `shm_open_exclusive`, transferring sole ownership into `OwnedFd`.
crates/rune/src/platform/wayland_shm/slots.rs | ftruncate_fd | `fd` is asserted non-negative and owned by this slot while `size` is asserted positive and within `MAX_SHM_BYTES`.
crates/rune/src/platform/wayland_shm/slots.rs | copy_slot_bytes | `src` and `dst` are asserted distinct non-overlapping slot mappings (`dst` not busy) and `offset + len` is asserted within both `src_len` and `dst_len`.
crates/rune/src/platform/wayland_shm/slots.rs | ShmSlot::bytes_mut | asserts `!self.busy`, `0 < self.len <= MAX_SHM_BYTES`, and `self.stride * self.height == self.len` before exposing `&mut [u8]` over the exclusive slot mapping.
crates/rune/src/platform/wayland_shm.rs | foreign_display_backend | `display` is asserted non-null and comes from winit's `wl_display`, which outlives every window and is never closed by a foreign `Backend`.
crates/rune/src/platform/wayland_shm.rs | foreign_surface_id | `surface` is asserted non-null and comes from winit's live `wl_surface` handle for the active window.
crates/rune/src/platform/wayland_shm/watch.rs | create_eventfd | creates a fresh close-on-exec, non-blocking `eventfd` on Linux and checks `< 0` before wrapping.
crates/rune/src/platform/wayland_shm/watch.rs | create_pipe | passes a valid 2-element `RawFd` array to `libc::pipe` on non-Linux Unix and asserts both returned fds are non-negative on success.
crates/rune/src/platform/wayland_shm/watch.rs | set_fd_cloexec | `fd` is asserted non-negative and freshly created by `create_pipe` before setting `FD_CLOEXEC`.
crates/rune/src/platform/wayland_shm/watch.rs | set_fd_nonblock | `fd` is asserted non-negative and freshly created by `create_pipe` before setting `O_NONBLOCK`.
crates/rune/src/platform/wayland_shm/watch.rs | own_fd | `raw` is asserted non-negative and freshly created by `create_eventfd` or `create_pipe`, transferring sole ownership into `OwnedFd`.
crates/rune/src/platform/wayland_shm/watch.rs | write_signal_bytes | `fd` is asserted non-negative and owned by `StopSignal`, and `buf` is asserted non-empty and valid for `buf.len()` bytes.
crates/rune/src/platform/wayland_shm/watch.rs | poll_two_fds | asserts `pfds.len() == 2` and both descriptors (the live Wayland connection fd borrowed by `ReadEventsGuard` and `StopSignal`'s open fd) are non-negative before calling `libc::poll`.
crates/rune/src/clipboard/wayland/io.rs | create_pipe | passes a valid 2-element `RawFd` array to `libc::pipe` and asserts both returned descriptors are non-negative on success.
crates/rune/src/clipboard/wayland/io.rs | set_fd_cloexec | `fd` is asserted non-negative and freshly opened by `create_pipe` before applying `F_SETFD`/`FD_CLOEXEC`.
crates/rune/src/clipboard/wayland/io.rs | get_fd_flags | `fd` is asserted non-negative and open before querying file status flags via `F_GETFL`.
crates/rune/src/clipboard/wayland/io.rs | set_fd_flags | `fd` is asserted non-negative and open before applying `O_NONBLOCK` flags via `F_SETFL`.
crates/rune/src/clipboard/wayland/io.rs | own_fd | `raw` is asserted non-negative and freshly created by `create_pipe`, transferring sole ownership into `OwnedFd`.
crates/rune/src/clipboard/wayland/io.rs | write_fd_nonblocking | asserts `fd >= 0` and `!buf.is_empty()` before calling `libc::write` on the non-blocking clipboard pipe with valid `buf.as_ptr()` and `buf.len()`.
crates/rune/src/clipboard/wayland/io.rs | read_fd_nonblocking | asserts `fd >= 0` and `!buf.is_empty()` before calling `libc::read` into the valid mutable buffer `buf` from the non-blocking clipboard pipe.
crates/rune/src/clipboard/wayland/io.rs | poll_fds | asserts `!pfds.is_empty()`, `pfds.len() <= MAX_POLL_FDS`, and every `pfd.fd >= 0` (the live Wayland connection fd, wake pipe, and active transfer pipes) before calling `libc::poll`.
crates/rune/src/clipboard/wayland/io.rs | foreign_display_backend | asserts `display.as_ptr().addr() != 0` on winit's live `wl_display` handle, which outlives the clipboard IO thread and is never closed by a foreign `Backend`.
crates/rune/src/popup_menu/wayland/protocol.rs | foreign_display_backend | asserts `display.as_ptr().addr() != 0` on winit's live `wl_display` handle, which outlives every popup connection and is never closed by a foreign `Backend`.
crates/rune/src/popup_menu/wayland/protocol.rs | foreign_xdg_surface_id | asserts `ptr.as_ptr().addr() != 0` on winit's live parent `xdg_surface` handle; `Host` cancels and asserts `!self.popup.is_open()` before hiding, closing or dropping the parent window.
crates/rune/src/popup_menu/wayland/protocol.rs | foreign_layer_surface_id | asserts `ptr.as_ptr().addr() != 0` on the live `zwlr_layer_surface_v1` handle owned by the active layer overlay.
crates/rune/src/popup_menu/wayland/protocol.rs | create_wgpu_surface | asserts both handles are `RawDisplayHandle::Wayland` and `RawWindowHandle::Wayland` pointing to winit's live `wl_display` and this level's live `wl_surface`, and `LevelPresenter` is dropped before `PopupSurfaceHandles::destroy`.

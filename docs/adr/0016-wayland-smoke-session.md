# The Wayland smoke session and its probe client

Status: accepted

## Context

DD1 of `plans/desktop-design.md` puts a CPU-rendered Wayland compositor (`ferrion-wm-smoke`, on `ferrion-wm-backend`) on its own inputd virtual terminal, with `wayland-probe` as its client. Orbital and the compositors share virtual terminals, and the client talks the Wayland protocol through the pure-Rust backend.

## Decision

### Sessions

- `config/wm-smoke.toml` is a test image: its session replaces Orbital and starts the compositor as root, with the input and display access it inherits from init. No normal desktop's defaults change.
- `config/wayland.toml` starts its compositor in place of orblogin after `30_console.target`, as `desktop-minimal.toml` does, so that Orbital gets VT 3.

### The compositor

`framebuffer` wraps memory the backend owns in a pixman image. The image is dropped before the backend reconfigures or drops the framebuffer, and no CPU write or present happens while a renderer frame borrows it.

### The probe client

- `wayland-probe` uses the Rust Wayland backend, not the C libwayland package, with both protocol crates on the same fork revision.
- It runs as separate processes: `wayland-probe` and `wayland-probe --slow`. The slow client's delay belongs to that process alone.
- Start-up takes two round trips: the second receives the capabilities and formats of the globals just bound.
- `wl_shm` ARGB8888 is little-endian with opaque premultiplied alpha. The anonymous backing file stays alive until every buffer's destroy request, and its contents never change while a compositor can read them. The pool is destroyed right away: `wl_buffer` keeps the pool's storage.
- The first `xdg_surface` commit has no buffer; the configure event permits mapping. A frame callback is not a buffer release, so the client waits for both.
- Each commit's `CLOCK_MONOTONIC` time is printed; the VM test joins it to the compositor's present time by client and frame. The client never claims that a frame was displayed.
- The keymap's descriptor is dropped with the event: reporting physical keys needs no mapping, modifiers, repeats or XKB keycode offset.
- Destruction requests precede the final flush; on a transport failure, disconnecting also releases the server's objects and shared mappings.

## Consequences

Writing to the framebuffer while a renderer frame borrows it is undefined behavior in the compositor. Mapping a buffer on the first commit, or treating a frame callback as a release, breaks against a strict compositor.

## Where

- `config/wm-smoke.toml`, `config/wayland.toml`: the session's init script
- `recipes/core/ferrion-wm-smoke/crate/src/main.rs`: `framebuffer`
- `recipes/tests/wayland-probe/crate/src/main.rs`: `run`, `write_solid_pool`, `pressed_probe_key`, `State`
- `recipes/tests/wayland-probe/crate/Cargo.toml`, `recipes/tests/wayland-probe/recipe.toml`

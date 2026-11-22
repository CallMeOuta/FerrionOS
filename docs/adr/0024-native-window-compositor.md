# The native window compositor and its layout check

Status: accepted

## Context

The full compositor needs the same inputd virtual terminal, DRM presentation and independent cursor updates as the Wayland smoke session. niri's Linux integrations cannot run on FerrionOS, and its Smithay revision is newer than the backend crate's optional Smithay adapter. The display scheme has no page-flip completion event.

## Decision

`ferrion-wm` keeps niri's layout and protocol implementation. Its native adapter lives in `src/backend/ferrion.rs`, with backend selection in a separate module. The recipe applies the platform and IPC patches before the four free-workspace patches. GPL modification notices remain in changed files; explanatory comments are recorded here instead.

The target build selects only the `ferrion` feature. `tty`, `winit` and `use-system-wayland` keep the Linux backends and system Wayland library out of that build while preserving the host's default features. The configuration crate's libinput conversions are also conditional. The native backend dependency disables its optional Smithay feature, so the executable uses one Smithay revision: niri's pin, with the target signal-address and DMA-BUF changes and optional libei dependency carried as a separate MIT patch. The adapter translates the backend's plain input types to that revision's types.

The renderer creates a surfaceless EGL context, draws into an offscreen texture and copies its pixels into the native framebuffer before presentation. The generated configuration selects a fixed output and CPU rendering. A calloop ping wakes the compositor when the native reader queues input; a timer and the backend's `FrameScheduler` defer drawing according to the recent frame cost. The compositor's cursor elements supply the cursor-plane image and hotspot. The native reader can move that plane during a full-frame presentation. An image that exceeds the plane's capacity falls back to compositor rendering.

Frame callbacks and presentation feedback follow the synchronous DRM handoff. Their timestamp means that the driver accepted the update; it does not claim physical scanout or vertical-blank synchronization. A later backend with completion events must move that boundary to the completion event.

`config/wm-columns.toml` follows the smoke session: it starts a root compositor on its own virtual terminal, inheriting input and display access from init, and retains development accounts. Its serial root shell can control the compositor through ordinary IPC and launch Wayland clients. It adds no account, group, scheme entry or hardening exception. This test session provides no capability gate; desktop authorization remains planned work.

The three independent `desktop-layout-probe` processes draw fixture windows. The terminal process delays its draws so the pointer check also runs beside a slow client. A shared mapping supplies three buffer slots, each written only after its previous use is released. The harness floats and places each window through the compositor's existing IPC, then compares the three window regions using the same fixture loader as `desktop-regions`. It measures both layouts with corners disabled and enabled. Shadows are disabled; the single rounded title-corner sample excluded by the harness is named in its result.

The frame-time condition uses the first presented client frame containing each injected key's visible counter change. Client commit-to-handoff, cursor input-to-ioctl completion and output cadence are separate series. Each case saves individual events as well as median, 99th percentile and maximum. Functional checks require the windows and timing samples; the latency condition is recorded separately and does not turn a correctly drawn layout into a failed functional check.

## Consequences

The MIT native backend remains reusable by another compositor. Linux default-feature tests still cover niri's original paths. The CPU path copies complete output frames and supports one fixed-mode output; its measurements determine whether a later session may become the default. The normal desktop continues to start Orbital.

Source and test code carry their respective GPL and MIT notices. The package installs the compositor's GPL license and both MIT dependency licenses. The private issue tracker holds the shared-file coherence failure, the intermittent startup stall and remaining platform limitations, with VM evidence in the lane report.

## Where

- `recipes/core/ferrion-wm-backend/crate/src/platform.rs`
- `recipes/wip/wayland/ferrion-wm/recipe.toml` and its patch list
- `config/wm-columns.toml`, `scripts/ferrion-wm-config.py`
- `recipes/tests/desktop-layout-probe/`, `recipes/tests/shm-reuse-probe/`
- `scripts/desktop_columns.py`, `scripts/desktop_fixtures.py`, `scripts/vm-test.py`

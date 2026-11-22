# ferrion-wm

This recipe builds the FerrionOS compositor from the pinned niri source. The compositor patches are GPL-3.0-or-later. Smithay and `ferrion-wm-backend` retain their MIT licenses. Fork hosting is a separate maintainer decision.

The target uses `--no-default-features --features ferrion`. The default Linux build retains the tty, winit, system Wayland, D-Bus, systemd and screencast integrations. The target disables those integrations, including niri-config's libinput conversions and Smithay's unused libei dependency. Its native adapter uses the backend without its optional Smithay feature, so the executable contains one Smithay version.

The adapter uses surfaceless EGL and Mesa's CPU renderer, copies the finished framebuffer into the DRM scheme, and presents on an inputd virtual terminal. Its cursor image follows the compositor's cursor path; inputd's reader thread moves the cursor plane independently. A cursor or drag icon larger than the plane falls back to composition. The backend's measured frame cost controls the next redraw deadline. Frame callbacks and presentation feedback follow the display handoff; that timestamp is not a scanout timestamp. The initial port has one fixed-mode output and no dma-buf import.

`scripts/ferrion-wm-config.py` generates the two test configs from the checked-in tiling fixture. `--check` verifies that they are current. `config/wm-columns.toml` starts a test-only root session on the pattern of `wm-smoke`; its serial root shell can connect to both the Wayland and IPC sockets. It adds no account, group, or login scheme. This session has no application capability gate; that belongs to a later slice.

Cook with `make r.ferrion-wm,desktop-layout-probe`, build with `make all CONFIG_NAME=wm-columns`, and run `scripts/vm-test.py desktop-columns --config wm-columns --repeat 10`. Follow the repository's container and logging instructions. The check loads the fixture regions used by `desktop-regions`, opens three independent xdg-shell clients, and floats and positions them with ordinary compositor IPC. It records both layouts with corner radii zero and fourteen. The rounded title-corner probe is explicitly skipped; shadows are off. The input-to-presented median and 99th-percentile conditions are recorded separately from the functional verdict.

The timing artifact retains client commit and presentation timestamps, keyboard reads paired with the first presented client update, and cursor read/ioctl timings. One client delays its drawing by 100 ms. QMP injects input through the VM's devices. `result.json` embeds the measurements beside the harness verdict.

The probe writes through persistent shared mappings and reuses a slot only after `wl_buffer.release`. Ordinary file writes followed by a mapping read currently disagree on the OS; the independent `shm-reuse` check preserves that failure. This client workaround does not repair the OS bug.

When combining this recipe with the free-mode work, keep the F1/F3 patches first and append the F4–F7 series. Preserve both sets of patches when integrating separately created recipe files.

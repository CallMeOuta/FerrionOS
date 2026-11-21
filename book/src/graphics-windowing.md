# Windowing and Graphics

## Drivers

The `drivers/graphics` directory of the `base` source (recipe `recipes/core/base`) holds three display drivers: `vesad`, `ihdgd` for Intel GPUs, and `virtio-gpud` for the VirtIO GPU of a virtual machine. Beside them it holds the daemons that draw the boot log and the text console (`fbbootlogd` and `fbcond`), three shared libraries (`console-draw`, `driver-graphics` and `graphics-ipc`) and the benchmark `drawbench`.

### VESA (vesad)

vesad is not really a driver, it writes to a framebuffer given by firmware (via UEFI or BIOS software interrupts).

On a machine whose GPU neither `ihdgd` nor `virtio-gpud` handles, the display is this framebuffer from the firmware.

### Intel GPUs

`ihdgd` is a mode-setting driver for Intel GPUs. Upstream describes it as covering Tiger Lake GPUs and later, and the device IDs it starts for are in `drivers/graphics/ihdgd/config.toml` of the `base` source. FerrionOS has not tested it on hardware.

## GPUs

On Linux/BSDs, the kernel communication with GPU is done by the Linux DRM drivers (Direct Rendering Manager) and user-space applications use the `libdrm` library to access them, which Mesa3D drivers use to work (Mesa3D implements OpenGL/Vulkan drivers, DRM exposes the hardware interfaces).

FerrionOS also uses the Linux DRM API, but fully in user space: the driver code is written in Rust and adapted to the microkernel architecture.

The Linux DRM API is available to user space through the `libdrm` C library (recipe `recipes/libs/libdrm`) and its Rust equivalent, and the windowing system (Orbital) uses it, which reduces the effort of porting programs.

For software rendering, the `mesa` recipe (`recipes/libs/mesa`) fetches a fork of Mesa3D that upstream keeps. The fork adds a backend for the `redox` platform, and the recipe builds only its software renderers, LLVMpipe and Lavapipe.

## Software Rendering

There is no 2D/3D hardware acceleration yet, so OpenGL and Vulkan applications are rendered on the CPU with [LLVMpipe/Lavapipe](https://docs.mesa3d.org/drivers/llvmpipe.html).

## Orbital

Orbital is the display server and window manager that FerrionOS currently uses. The `orbutils` package adds a login screen, an app launcher, a wallpaper program and an image viewer, and `orbterm` is a basic terminal.

Upstream preferred it over Wayland and X11 to avoid their requirements, dependencies, and limitations (EGL on Wayland and GLX on X11): Orbital was simpler to build on a young system API, and it renders natively in software.

FerrionOS has no X11: no X server, no Xwayland and no X11 libraries. Graphical programs use Orbital or Wayland. Wayland is experimental: the `wayland` filesystem configuration (`config/wayland.toml`) starts a Wayland compositor (`cosmic-comp` or `smallvil`) inside an Orbital window. A program that can only draw through X11 does not run on FerrionOS.

### Comparison with X11/Wayland

This display server is simpler than X11 and Wayland, which makes porting quicker and easier. It is not as advanced as X11 and Wayland yet; upstream considers it enough to port most Linux/BSD programs.

Compared to Wayland, Orbital has one protocol server implementation (like X11 for a long time), while Wayland provides protocols for compositors and clients.

Also few X11 or Wayland programs render natively in software, so on FerrionOS a Wayland program that does not would draw partly or fully through LLVMpipe, which reduces performance.

### Features

- App Launcher (bottom bar): `launcher`, from the `orbutils` package that `config/desktop-minimal.toml` installs
- Terminal Emulator: `orbterm` in the `desktop-minimal` config; the `desktop` config currently installs the COSMIC terminal in its place
- File Manager: the `desktop` config currently installs the COSMIC file manager
- Text Editor: the `desktop` config currently installs the COSMIC text editor

Which desktop apps FerrionOS ships is not settled and may change.

If you hold the **Super** key (generally the key with a Windows logo) it will show all keyboard shortcuts in a pop-up.

### Libraries

Upstream lists the following libraries as usable by programs on Orbital. FerrionOS has not tested them, and the recipes for the Slint and egui examples are still marked as work in progress (`recipes/wip/demos`).

- winit
- softbuffer
- Slint (through winit and softbuffer)
- Iced (through winit and softbuffer)
- egui (winit or SDL2 can be used)
- SDL1.2
- SDL2
- Mesa3D's OSMesa

### Security

Orbital is designed so that a GUI program cannot read the input events or the content (framebuffer) of the windows of other GUI programs, as in Wayland. FerrionOS has not tested this and does not claim it as a protection: see [Security](./security.md).

### Clients

Apps (or 'clients') create a window and draw to it by using the [orbclient](https://crates.io/crates/orbclient) crate.

#### Client Examples

If you wish to see examples of client apps that use orbclient to "talk" to Orbital and create windows and draw to them, you can find some in the `examples` folder of the orbclient source. The recipe `recipes/demos/orbclient` fetches that source (`make f.orbclient`) and builds its `simple` example.

### Porting

If you want to port a program to Orbital, see below:

- If the program is written in Rust and uses the `winit` crate, as most do, it can use winit's Orbital backend. Programs that access X11 or Wayland directly need to be ported to `winit`; that change belongs in the program's own project.

- If the program is written in C or C++ and accesses X11 or Wayland directly, it must be ported to the Orbital library, `liborbital` (recipe `recipes/libs/liborbital`).

- Build a port with X11 disabled (meson `-Dx11=disabled`, cmake `-DWITH_X11=OFF`, configure `--without-x`, cargo features without `x11`). FerrionOS has no X11 libraries, and `scripts/no-x11-check.py` fails when a recipe depends on one.

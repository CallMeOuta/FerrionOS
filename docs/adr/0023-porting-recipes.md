# Choices the port recipes make about building and packaging

Status: accepted

## Context

A port recipe adapts another project's build to cross-compilation for FerrionOS ([`agents/skills/recipes.md`](../../agents/skills/recipes.md)). Many projects cannot build out of tree, mix up build-machine and target flags, read the cookbook's variables for their own purposes, or need features FerrionOS lacks. Some packages may not be redistributed.

## Decision

### Build systems that need help

- NetSurf and its libraries (`netsurf`, `netsurf-buildsystem`, `libnsgif`) copy or sync the source into the build directory, because they cannot build out of tree. NetSurf mixes up the flags for the build machine and the target, so the target's `CPPFLAGS` are folded into `CC`.
- `opentyrian`'s build system is only a Makefile, so it sets `COOKBOOK_CONFIGURE=true`; `love` cannot `make install`, so its recipe copies the `love` binary.
- `goaccess` compiles `bin2c` with the build machine's compiler, because it runs during the build. `groff` puts the build directory first on `PATH` for `test-groff`.
- `libicu` unsets `TARGET`, which it uses for something else; `tor-browser` and `firefox-esr` unset the target toolchain variables before `mach`, so the stage-1 build for the build machine is not poisoned. `aws-lc-rs` needs `HOST` to differ from `TARGET`, or GCC bug 95189 breaks its `cc_builder`. Servo's pkg-config variables point at one sysroot directory, because the `pkg-config` crate recognizes only one path.
- `mariadb` builds its build-machine tools first, as its cross-compiling guide describes; `boost` writes its `user-config.jam` in the GCC toolset syntax its documentation gives, and leaves out Boost.Stacktrace, which needs `dlsym(RTLD_NEXT)`.
- `neovim` compiles `libnlua0.so` for the build machine itself (the only official cross path in future is Zig, neovim issue 19579) and rewrites the absolute `liblpeg` path with `patchelf` (issue 23395).
- `libiconv` caches the little-endian autotools check, without which riscv64gc does not build; `libmodplug1` defines `_GNU_SOURCE` for `strings.h`; `termcap` compiles its static-only objects with `-fPIC`; `cosmic-reader` defines `__LITTLE_ENDIAN__` for mupdf.
- `bzip2` always installs the static library, which its README prefers as faster, then rebuilds the objects to link the shared library that distributions ship (linking fails without the rebuild), without touching the binaries built before.
- `zlib` is zlib-rs and `libjpeg` is libjpeg-turbo, both compatible with the libraries they stand for; `zlib` stages both the static and the shared library, and relies on `relocation-model=pic`, the default on Linux and FerrionOS, which `libz-sys` needs when zlib is linked statically.

### Rust ports

- `powerline` and `ratzilla` drop `--locked`: the lock files pin a very old `redox_syscall` and `libc` that fail. `martypc` uses Cargo's shallow git fetching for its many git dependencies.
- `mozjs` and `tikv-jemallocator` stage nothing: library crates are used as dependencies, not installed.
- `wgpu-hello` renders each frame into a texture and copies it into the window with softbuffer, because Mesa's Vulkan has no surface for Orbital windows; a wgpu surface is only tried and its result printed. On FerrionOS winit and softbuffer use Orbital, which needs no feature; on a Linux build machine they use Wayland, so the crate also builds and runs there for comparison. `Gpu::render` returns the triangle turned by `angle` radians as RGBA pixels, row by row. `wgpu-hello-window --frames N` exits after N frames.
- `unzip-rs` builds unzrip, a Rust reimplementation preferred over the abandoned C unzip.
- `zed` builds wasmtime, wasmtime-wasi and zbus from patched copies in `ferrion-vendor/`: each crate archive from crates.io, checked against its sha256 in Zed's `Cargo.lock`, with the patch of the same name. The cookbook does not hash those patches, so after changing one, delete `source/` to fetch again.

### Features left out or chosen

- `profiled` is linked statically, as a core daemon must be.
- `shared-mime-info` generates its database on the build machine instead of building `update-mime-database`, which would bring C code into FerrionOS.
- `retroarch` disables its bundled zlib, which conflicts with the zlib package; `sdl2-mixer` disables MIDI, which would need fluidsynth; `libopus` disables run-time CPU detection on aarch64, where FerrionOS offers none of the APIs libopus uses; `uutils-procps` leaves out `ps`, which conflicts with coreutils; `bind9` includes nghttp2 for DNS over HTTPS; `hangover` enables the 32-bit Windows guest architectures through WoW64, with no 32-bit Unix libraries.
- `libepoxy` enables EGL without X11 in its dynamic build and neither EGL nor Wayland in its static one.
- The wip `libvulkan` builds the Khronos loader (`libvulkan.so.1`), which finds Mesa's lavapipe through `/usr/share/vulkan/icd.d`, without window-system support: Orbital has no Vulkan surface.
- The wip `sqlite3` has no malloc soft limit (it is used in memory a lot), disables mmap (remapping crashed Firefox at start-up), uses the serialized threading mode because of concurrency problems seen with RedoxFS, and keeps temporary files in memory because `/tmp` is slow.
- WebKitGTK 3 disables the features that need D-Bus, the ones with hardware or library problems, and spellcheck (no libenchant); its launcher refuses to start MiniBrowser without compiled GSettings schemas, which break its CSS layout. Servo's launcher forces Mesa's softpipe driver, because a bug in egui or LLVM breaks its fonts.
- `gimp` installs icons that were not converted to PNG (see its patches) and replaces the `/usr/bin/gimp` symlink to `gimp-3.2` with a wrapper.
- `swanstation` uses the DuckStation fork from when it was GPLv3 and still had its Qt and ImGui front ends, which the libretro fork no longer has.

### Data and redistribution

- `sm64ex` and `spacecadetpinball` are not distributed (sm64pc/sm64ex issue 89, k4zmu2a/SpaceCadetPinball issue 1); sm64ex needs the user's own ROM.
- `opentyrian` downloads the freeware Tyrian data at build time. `quake1-shareware` extracts the demo's data from the archive inside the DOS installer, and lowercases its directory and files, which systems other than DOS and Windows expect.
- `fish-shell` exports `PREFIX=/usr` and `SYSCONFDIR=/etc`, without which fish uses `~/.local/share/fish/install` as its data directory, cannot find its scripts and refuses its configuration, and turns off `embed-data` so its scripts are installed as files.

## Consequences

These choices are recipe-local; changing one is checked by cooking that recipe (rung 5 of [`docs/testing.md`](../testing.md)). Distributing `sm64ex` or `spacecadetpinball` packages breaks their projects' terms.

## Where

- `[build] script` and `[source]` of the recipes named above, under `recipes/` and `recipes/wip/`
- `recipes/wip/demos/wgpu-hello/crate/src/window.rs`, `triangle.rs` (`Gpu::render`), `Cargo.toml`
- `recipes/wip/web/servo/servo.sh`, `recipes/wip/web/webkitgtk3/webkit.sh`

# Components of FerrionOS

FerrionOS is made up of several discrete components. A filesystem config in `config/` decides which of them an image contains: each name in its `[packages]` table is a package, built by the recipe of the same name.

## Core

- bootloader - Kernel bootstrap
- kernel - System manager
- bootstrap - User-space bootstrap
- init
- initfs
- drivers - Device drivers
- redoxfs - Filesystem
- audiod - Audio daemon
- netstack - TCP/UDP stack
- ps2d - PS/2 driver
- relibc - C library
- randd
- zerod
- ion - Terminal shell
- orbital - Display server and window manager

`config/base.toml` installs the packages `bootloader`, `kernel`, `relibc` and `base`. The `base` package holds `bootstrap`, `init`, the initfs, the drivers (`ps2d` is one of them), `audiod`, `netstack`, `randd` and `zerod`, and it puts the RedoxFS daemon into the initfs. `config/minimal.toml` adds `ion`, and `config/desktop-minimal.toml` adds `orbital`. The `redoxfs` package, with the RedoxFS tools, comes from `config/server.toml`, which the `desktop` config includes.

## Orbital

`config/desktop-minimal.toml` installs Orbital together with the `orbutils` package, which holds these programs:

- orblogin - Login manager
- launcher - App panel
- background - Wallpaper program
- viewer - Image viewer

The `desktop` config currently installs these desktop apps:

- COSMIC Files - File manager
- COSMIC Editor - Text editor
- COSMIC Terminal - Terminal emulator
- NetSurf - Web browser
- The graphical installer

Which desktop apps FerrionOS ships is not settled and may change.

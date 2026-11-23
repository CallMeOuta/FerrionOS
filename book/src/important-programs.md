# Important Programs

This page lists important programs and libraries that have recipes in the repository, with the status that upstream recorded for each of them as of August 2026.

FerrionOS has not repeated these tests, with one exception: the shells of the x86_64 desktop image, which `scripts/vm-test.py` checks ([Testing](./testing-practices.md)). Treat every other status as inherited and unverified. A status may be out of date; a report of a program that works or fails on a FerrionOS image is welcome ([Participating](./contributing.md)).

The system is designed to be source-compatible with POSIX and Linux applications, so that a program needs only compilation or small patches. All programs are compiled by the cookbook, the cross-compilation system of the repository, whose recipes hold the patches and forks that the programs need.

Upstream tested on the x86_64 (Intel/AMD) CPU architecture. Other CPU architectures may not have been tested.

## Compilers

| Name | Cross-compiles from Linux? | Compiles programs on the system? | Compiled programs run on the system? | Works on the system? |
|---|---|---|---|---|
| GCC 13 | Yes | Yes | Working Well | Yes |
| Rust 1.90 | Yes | Always Crashing | Working | Yes |
| Go 1.25 | Yes | Always Crashing | Always Crashing | No |

## Interpreters

| Name | Can be compiled? | Status recorded upstream |
|---|---|---|
| RustPython | Yes | Working Well |
| CPython 3.12 | Yes | Working Well |
| Lua 5.4 | Yes | Working |
| LuaJit 2.1 | Yes | Often Crashing |
| PHP 8.4 | Yes | Mostly Well |

## GUI Libraries

Most GUI applications run through Orbital, the display server and window manager. It supports the following GUI libraries:

- Mesa3D (OpenGL and EGL, via liborbital)
- SDL1 and SDL2 (via Mesa3D)
- winit (via orbclient)

## Applications

The x86_64 desktop image currently installs the COSMIC terminal, file manager and text editor; Firefox is built separately as a GTK3 Wayland browser. Which desktop apps FerrionOS ships is not settled and may change.

| Name | GUI Backend | Status recorded upstream |
|---|--|---|
| COSMIC Editor | winit | Working Well |
| COSMIC Files | winit | Working Well |
| COSMIC Reader | winit | Working Well |
| COSMIC Store | winit | Not Tested |
| COSMIC Terminal | winit | Working Well |
| FFplay | SDL2 | Working Well |
| Servo | winit | Often Crashes |

## Servers

| Name | Status recorded upstream |
|---|---|
| OpenSSH Daemon | Working Well |
| Nginx | Working Well |
| Simple HTTP Server | Working Well |
| Python Static Server | Not Tested |
| PHP-FPM | Not Tested |

## CLI Tools

The programs listed below may not include the [core utilities](./side-projects.md).

### Terminal Shells

The last column is FerrionOS's own check on the x86_64 desktop image: `scripts/vm-test.py shells` logs in on the serial console and runs one command in each shell. [Shells](./shell.md) describes the shells of an image.

| Name | Status recorded upstream | Checked by FerrionOS |
|---|---|---|
| GNU Bash | Working Well | Runs a command (bash 5.3) |
| Ion | Working Well | Runs a command |
| Nushell | Working | Runs a command as the login shell of `user` (nu 0.110) |
| Fish | Working | Runs a command (fish 4.0) |
| Zsh | Hangs | Runs a command with `zsh -c` (zsh 5.9) and prints `getrusage()` warnings; interactive use is not checked |

### Text Editors

| Name | Status recorded upstream |
|---|---|
| Kibi | Working Well |
| GNU Nano | Working Well |
| Vim | Working Well |
| Sodium | Working Well |
| Neovim | Crashes |

### System Monitors

| Name | Status recorded upstream |
|---|---|
| Bottom | Working Well |
| Htop | Working Well |

### Development Tools

| Name | Status recorded upstream |
|---|---|
| Git | Working Well |
| GNU Autotools | Not Tested |
| GNU Binutils | Working Well |
| GNU Grep | Working Well |
| GNU Make | Working Well |
| GNU Sed | Working Well |

### Media Tools

| Name | Status recorded upstream |
|---|---|
| FFMPEG | Working Well |

### Archive Tools

| Name | Status recorded upstream |
|---|---|
| Bzip2 | Working Well |
| Xz | Working Well |
| Xxhash | Working Well |
| Zstd | Working Well |

### Storage Tools

| Name | Status recorded upstream |
|---|---|
| Ncdu | Not Tested |

### Network Tools

| Name | Status recorded upstream |
|---|---|
| OpenSSH Client | Working |
| Curl | Working Well |
| GoAccess | Working Well |
| Rsync | Working Well |
| SQLite | Not Tested |
| Wget | Working Well |

## Other Programs

Every ported program is a recipe under [`recipes/`](https://github.com/CallMeOuta/FerrionOS/tree/master/recipes) in the repository. A recipe under `recipes/wip/` is unfinished, and its first line says what is missing.

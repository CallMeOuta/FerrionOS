# Why the filesystem configs override, build from source and order what they do

Status: accepted

## Context

A filesystem config under `config/` lists an image's packages, files and accounts, includes other configs, and chooses per package whether it is built here (`"source"`) or taken prebuilt from upstream's package server (`"binary"`) ([`agents/skills/filesystem-configs.md`](../../agents/skills/filesystem-configs.md)). Upstream's prebuilt packages are built for upstream's image, and several of them link X11, which FerrionOS has none of (AGENTS.md "Display").

## Decision

### Virtual terminals and the order of start-up

- inputd numbers virtual terminals in the order they open: 1 for the boot log, then 2, 3 and so on. Orbital switches to the number in its `VT` variable, so it starts after `30_console.target`, whose `fbcond@2` has taken VT 2 by then, and Orbital gets VT 3. Every config that replaces Orbital's start (`desktop-contain.toml`, `wayland.toml`, `redoxer-gui.toml`) starts after the same target for the same reason.
- `desktop-minimal.toml` replaces `30_console_switch` so the desktop does not switch to VT 2, fbcond's text console. `desktop.toml` includes `desktop-minimal.toml` last, so that its replacement wins over the copy `server.toml` brings in again.

### Built from source

- Packages whose prebuilt builds link X11 are built from source with X11 disabled: the display packages in `x86_64/full.toml`, `sys-build.toml`, `wayland.toml` and `x86_64/demo.toml` (gtk3 and webkitgtk3 included), and `libxkbcommon` in `desktop.toml`, which the COSMIC apps need.
- orbdata and orbutils are built from source in `desktop-minimal.toml`: orbdata's recipe lays FerrionOS's login background, wallpaper, launcher button and window-server background over upstream's, and orbutils carries a patch that draws the login box in FerrionOS's colors. Upstream's builds have neither.
- extrautils is built from source in `minimal.toml` for the tar fixes in `ferrion-tar-options.patch`; redoxfs and the installer in `disk-encryption-test.toml` for FerrionOS's patches that add `redoxfs-key` and the installer's recovery key.

### Files written after the packages

Launcher entries that name an OS are replaced by files the config writes with `postinstall`, so they win over the packages' own: the image viewer's entry in `desktop-minimal.toml` (with a description that names no OS), and the netsurf and installer-gui entries in `desktop.toml` (naming FerrionOS).

### Accounts and test fixtures

- `dev-accounts.toml` holds the login accounts with known credentials, for building and booting in QEMU, as AGENTS.md's Security section describes. A config that leaves it out has no login accounts; an image that leaves the build machine carries its own.
- `disk-encryption-test.toml` is a development fixture and is never distributed. It is built with `CONFIG_NAME=disk-encryption-test VERIFIED_BOOT=1`, and `make live` with the same variables, for `scripts/vm-test.py disk-install`. Its public password is a fixture, not a credential; `vm-test.py` and `verified-boot-sign.sh` read it from the config. Its graphical login lets `secure-boot-test.sh` judge the signed case.
- `login-namespace.toml` (item S12) adds `netless`, an account with a password whose entry in `/etc/login_schemes.toml` leaves out the network schemes (`ip`, `icmp`, `tcp`, `udp`); `login` must enter that namespace for an account with a password just as for one without (`ferrion-login-namespace.patch` in userutils). It replaces base's `login_schemes.toml` whole: `root` and `user` keep their lists, `netless` gets `user`'s without the network. Its credential is for this check only, and the image never leaves the build machine. `scripts/vm-test.py login-namespace` checks it.
- `lockdown.toml` (item S13): init grants the hardware right only to services whose unit sets `hardware = true`, the drivers and driver managers. Every other process, root's login shell included, sees no `irq`, `serio`, `kernel.acpi`, `pci` or `acpi` scheme, and its memory scheme maps no physical memory and grants no port I/O. `lockdown-probe`, run as root, prints one line per route and exits 0 when every route is closed; `scripts/vm-test.py lockdown` runs it.
- In `redoxer.toml`, redoxerd is a service and not a script, because it ends QEMU through its exit port, which needs the hardware right; it starts the command under test without the right. In `redoxer-gui.toml`, Orbital holds the right only to pass it to redoxerd.

### Test images

- `auto-test.toml` runs the smaller suites first, so a regression shows sooner. `os-test.toml` installs both the sources and the cross-compiled binaries.
- `x86_64/dev-parity.toml` is the server image plus the tools a programmer working with AI agents reaches for, so that `scripts/vm-test.py dev-parity --config dev-parity --make QEMU_MEM=4096` can try each task (git over HTTPS, Rust, Python, Node.js and npm, OpenSSH, Helix, and the npm packages of two coding agents) and record which work; it carries the dev accounts. Its large new packages use the `binary` rule, because Rust, GCC and Node.js take hours to cook, and the rule carries over to their dependencies the server image does not already build. GCC and binutils are there because rustc calls `cc` to link. Its filesystem size leaves room above the packages' 1.5 GiB for a Cargo build, a Python virtual environment and npm installs.
- `x86_64/firefox-window.toml` tests GTK apart from Firefox's start-up, with the already cooked Firefox package; `x86_64/firefox-wayland.toml` tests Firefox's Wayland-only GTK toolkit in a nested compositor. Each runs only what its check needs (no other Wayland demo programs, no Linux fixture); [`docs/testing.md`](../testing.md) has the commands.

## Consequences

- An Orbital start moved before `30_console.target` puts Orbital on another VT and the login on the wrong screen.
- Switching an X11-disabled package to `binary` brings X11 libraries into an image, which `scripts/no-x11-check.py` does not see (it checks recipes and package names, not prebuilt contents).
- An account, a scheme list or a hardware grant added to a config widens access, and is named in the report as AGENTS.md's Security rules ask.

## Where

- `config/desktop-minimal.toml`: `/usr/lib/init.d/20_orbital`, `/usr/lib/init.d/30_console_switch`, `/usr/share/ui/apps/60_viewer`, `orbdata`, `orbutils`
- `config/desktop.toml`, `config/desktop-contain.toml`, `config/wayland.toml`, `config/redoxer.toml`, `config/redoxer-gui.toml`
- `config/minimal.toml`: `extrautils`; `config/disk-encryption-test.toml`; `config/dev-accounts.toml`; `config/login-namespace.toml`; `config/lockdown.toml`
- `config/sys-build.toml`, `config/x86_64/full.toml`, `config/x86_64/demo.toml`, `config/auto-test.toml`, `config/os-test.toml`
- `config/x86_64/dev-parity.toml`: `[general] filesystem_size`; `config/x86_64/firefox-window.toml`, `config/x86_64/firefox-wayland.toml`

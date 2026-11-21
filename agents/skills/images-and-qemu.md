# Images and QEMU

Read this before building an image or booting one in QEMU.

## Choose and build

`ARCH` and `CONFIG_NAME` select the image, and every command about that image carries the same pair, on the command line or in `.config`. The defaults are the host's architecture and `desktop`. For a check that only needs a shell, `CONFIG_NAME=minimal` or `CONFIG_NAME=server` cooks far fewer packages and has no GUI.

| Goal | Command |
|---|---|
| Everything the config names, the first time | `make all` |
| Cook again, then a new image | `make rebuild` |
| A new image from the packages already cooked | `make image` |
| One recipe into the existing image | `make rp.<recipe>` |

Each of these can start a cook, and `make qemu` starts the whole build when the image is missing. Run them the way [`docs/testing.md`](../../docs/testing.md) says: `CI=1 COOKBOOK_LOGS=true` in the environment, output to a log file. [`docs/build-system.md`](../../docs/build-system.md) explains the stages, and why `make all` can find nothing to do after an edit.

`REPO_BINARY=1` fills the image with upstream's prebuilt packages instead of cooking. That proves a config or the image assembly quickly, and says nothing about a changed recipe.

Every `harddrive.img` is a verified-boot image by default (`VERIFIED_BOOT=1` in `mk/config.mk`): `scripts/verified-boot-sign.sh` signs the kernel and initfs with a development key kept in `build/verified-boot/` and writes that key into the image's UEFI bootloader, which then starts no other kernel or initfs and offers no environment editor. `make p.<recipe>` and `make push` sign the image again after they unmount it; after changing files in an image you mounted yourself, run `make verified-boot-sign` once it is unmounted, or the bootloader refuses a changed kernel or initfs. `VERIFIED_BOOT=0` builds an image that verifies nothing, for work that needs the environment editor. Live images are not covered.

The image is `build/<arch>/<config>/harddrive.img`. `make mount` mounts it at `build/<arch>/<config>/filesystem/` through FUSE, and `make unmount` releases it. A mount, a push (`make p.<recipe>`) and a running QEMU exclude each other: the image takes one writer at a time.

## Boot

`make qemu` boots the image. Its options are make variables defined in `mk/qemu.mk`. The ones a check needs:

- `gpu=no`: no window, and the serial console is the terminal. Without it QEMU opens a display window.
- `qemu_serial_logfile=<file>`: also writes the serial console to a file.
- `kvm=no`: emulates the CPU instead of using KVM, for a host without `/dev/kvm`.
- `net=no` removes the network card. `net=redir` forwards host port 8022 to the guest's 22 and 8080 to its 80.
- `live=yes` boots `redox-live.iso`, which `make live` builds, instead of the hard-drive image.
- Further QEMU flags go in the `QEMUFLAGS` environment variable. On the make command line, `QEMUFLAGS=` replaces every flag the makefile adds.

The accounts come from `config/dev-accounts.toml`, which every config meant for QEMU work includes: `user` with an empty password, and `root` with the password `password`.

On the serial console Ctrl-C goes to the guest, and `Ctrl-a h` lists the keys of QEMU's console multiplexer. QEMU exits when the guest powers off, so `shutdown` in the guest ends the run. From outside, end the `qemu-system-*` process.

`mk/ch.mk` (Cloud Hypervisor, x86_64 only) and `mk/virtiofs.mk` (sharing a host directory with the guest) describe themselves in their header comments.

## Check from outside, as a person would

`scripts/vm-test.py` boots an image through `make qemu`, watches the screen through QEMU's monitor, reads the serial console and types on it. Each run boots with `-snapshot`, so the image stays unchanged, and never starts a build. `scripts/vm-test.py` without arguments lists its tests and options.

- `scripts/vm-test.py boot --repeat 20`: every boot shows only black, the splash and the graphical login, and reaches the login within the timeout. A race that shows up once in a dozen boots needs `--repeat`.
- `scripts/vm-test.py shells`: logs in as `user` on the serial console and runs each shell the x86_64 desktop offers.
- `scripts/vm-test.py logout`: logs in as `user` on the serial console and exits; it passes when the login prompt returns and the serial log has no `failed to send SIGCHLD` line from the process manager.
- `scripts/vm-test.py ls-loop --repeat 3`: logs in as `root` on the serial console and runs `ls /scheme` 1000 times from Ion, which signals each command it starts; it fails when `ls` crashes. Before relibc's `ferrion-sigentry-rcx.patch` it failed in 3 of 3 boots of the desktop image.
- `scripts/vm-test.py signal-handle` (x86_64 desktop): logs in as `user` on the serial console and runs `signal-handle-probe` (`recipes/tests/signal-handle-probe`), which starts a session, has a child become root through `sudo` in it, and sends that child SIGTERM with `kill()` and through a process handle. It passes when the probe prints `REFUSED`: both were refused with EPERM.
- `scripts/vm-test.py screenshots`: captures the boot splash, the login screen, the desktop after logging in, and a window of each program in its `SCREENSHOT_APPS` list as PNG files, for documentation. It logs in on the login screen with Enter (`user` has an empty password) and starts each program from the serial console.
- `scripts/vm-test.py tpm`: boots with a software TPM (swtpm, which the host needs with swtpm_ioctl and tpm2-tools) on QEMU's `tpm-crb` device, logs in as `root` on the serial console, runs `tpm-pcrs` and `attest-statement`, then kills QEMU and reads PCRs 4, 7, 8 and 9 from swtpm on the host. It passes when PCR 4 is not zero (OVMF can lose swtpm while QEMU's monitor sits idle, so the test sends monitor commands until the login), PCRs 7, 8 and 9 agree inside and on the host, the statement carries no PCR value, and `ls /scheme` lists `attest` and no `tpm`. It leaves `pcrs.txt` and `statement.txt` beside the serial log.
- `scripts/vm-test.py rand-seed` (x86_64 with KVM): boots twice in each of three cases and logs in as `root` on the serial console: firmware only (the CPU's RDRAND and RDSEED switched off with `-global host-x86_64-cpu.rdrand=off -global host-x86_64-cpu.rdseed=off`, since `make qemu` appends its own `-cpu`, plus `-device virtio-rng-pci`, without which OVMF has no random-number protocol), timing jitter only (no CPU source, no virtio device), and the host CPU. It passes when the two boots of each case read different 64 bytes from `/scheme/rand`, and `/scheme/rand/status` and randd's boot line say `seeded: yes` with `firmware` among the sources in the first case, `seeded: no` in the second, and a CPU source in the third. It leaves `random.txt` and `random-status.txt` beside each serial log.
- `scripts/vm-test.py driver-namespaces` (x86_64 desktop): presses Enter on the graphical login, logs in as `root` on the serial console and reads `/scheme/sys/iostat`, where the namespace manager answers each namespace handle's fpath with `namespace:hardware=<0|1>;schemes=<names>;`. It passes when every namespace handle held by the drivers in its `DRIVER_NAMESPACES` list names only `memory` and `pipe` (audiod: `memory` and `rand`) and the drivers that always run on that image are running; drivers not running at the snapshot are listed as not exercised. It leaves `iostat.txt` beside the serial log.
- Results print one line per run, `PASS` or `FAIL` with the screens seen and their times; the exit code is 1 when any run fails. Screenshots and the serial log of each run land in `build/vm-test/<arch>/<config>/<test>-<run>/`.
- A new test is a function in the script that drives the `VM` object (`screenshot`, `wait_serial`, `type`, `command` for any QEMU monitor command such as `sendkey`) and returns pass or fail with a note.
- `scripts/secure-boot-test.sh <image>` checks UEFI Secure Boot on an x86_64 image: with test keys enrolled in a copy of OVMF's variables, the bootloader signed with the db key reaches the login, and the unsigned one and one signed with an unenrolled key are refused by the firmware. On a verified-boot image it adds three tamper cases (a changed kernel, a changed initfs, a manifest signed by another key) that the bootloader must refuse. It borrows the screen classifier (`Frame`) from `scripts/vm-test.py`, changes only copies of the image, and leaves `summary.txt` and per case `serial.log` and `screen.png` in `build/secure-boot-test/`. Without arguments it prints its options and the host packages it needs.
- `scripts/measured-boot-test.sh <image>` checks measured boot on an x86_64 image with OVMF and a software TPM (swtpm, tpm2-tools): it reads the PCRs from the host after each boot and the bootloader's TPM event log from the guest's memory, and passes when PCRs 7, 8 and 9 replay from the log, PCR 9 equals the value computed from the image's kernel and initfs (and manifest), a changed initfs changes PCR 9, a boot through the mode menu changes PCR 8, and the image boots without a TPM. A boot in which the firmware loses the TPM is retried once and reported `BLOCKED` on a second stall. It leaves `summary.txt` and per case `pcrs.txt`, `eventlog.bin`, `eventlog.yaml`, `serial.log` and `screen.png` in `build/measured-boot-test/`. Without arguments it prints its options and the host packages it needs.
- `scripts/secure-boot-keys.sh` handles FerrionOS's own Secure Boot keys: `create` makes PK, KEK and db (development keys in `build/secure-boot-keys/`, or certificates for keys on a hardware token), `sign <image>` signs the image's bootloader with db in place (run it last, and never on an image you then hand to `secure-boot-test.sh`, which wants the image as built), and `export <dir>` writes the certificates, signature lists, `.auth` updates and the firmware steps a user follows.

## Check inside the OS without typing

1. Write a throwaway config `config/my_<name>.toml` that includes the config you need and adds an init script under `/usr/lib/init.d/` which runs your command, saves its result to a file and then calls `shutdown`. `config/acid.toml` is the model: its init script starts an ion script that the same config writes to `/usr/lib/`.
2. `make qemu CONFIG_NAME=my_<name> gpu=no qemu_serial_logfile=<log>` builds and boots it, and QEMU exits when the script reaches `shutdown`. Run it under `timeout`: a hang in the guest never returns.
3. `make mount CONFIG_NAME=my_<name>`, read the result under `build/<arch>/my_<name>/filesystem/`, then `make unmount CONFIG_NAME=my_<name>`. `make ci-os-test` in `mk/ci.mk` does the same for the os-test suite.
4. After the unmount, delete the config and `build/<arch>/my_<name>/`.

Done when you have read the result file, or the serial log shows where the boot stopped. A failure here may be one of the platform's known limits: [`docs/platform-state.md`](../../docs/platform-state.md).

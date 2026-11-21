# How QEMU, Cloud Hypervisor and their host helpers are set up

Status: accepted

## Context

`make qemu` and `make ch` boot an image in QEMU or Cloud Hypervisor; `share=` adds virtio-fs shares served by host `virtiofsd` daemons, and `make ch` can network through passt or a host TAP device. These run on hosts with SELinux, firewalld, Debian's `PATH`, macOS and WSL2, each with its own limits. The options themselves are documented in `mk/qemu.mk`, `mk/ch.mk` and `mk/virtiofs.mk`.

## Decision

### QEMU defaults

- On aarch64, `uefi` defaults to `yes` with a live image: U-Boot sets up no framebuffer and FerrionOS cannot set one up itself.
- When `ARCH` differs from the host's architecture, `kvm` defaults to `no`, unless the caller or an earlier branch set it.
- `QEMU_ON_WINDOWS=1` is for WSL2: make runs the Windows `qemu.exe`, with the disk at a Windows path and the serial console on standard input and output.
- `QEMU_BASE_DIR` is the directory above the QEMU binary on `PATH`, where the firmware files that came with that QEMU are looked up.
- With `redoxer=yes`, the guest reports its result through the `isa-debug-exit` device: exit code 51 is success and 53 failure.
- Venus and vhost-user devices need the guest's RAM shared with the host renderer or `virtiofsd`, and the NUMA memory device must be exactly as large as `-m`; one memory backend serves every share.
- `net=redir` forwards host port 8022 to the guest's ssh on 22, 8080 to 8083 to web servers, and 64126 to FerrionOS's own gdbserver.
- `disk=ata` on aarch64 and x86_64 attaches the disks to an AHCI controller, which FerrionOS drives with `ahcid`.

### Cloud Hypervisor

- The hypervisor, its remote tool and its firmware are pinned releases, downloaded through `ch_download`, which checks each file's sha256 and goes through `$@.partial`, so a file with the final name is always a checked download. Every version downloads into its own directory. A pin is updated as [0002](0002-pinned-versions.md) says.
- `CH`, `CH_REMOTE` or `CH_FIRMWARE` set to anything other than the pinned download must name an existing file or a command in `PATH`; `ch-check` refuses anything else before launch.
- A leftover API socket makes Cloud Hypervisor fail with "Address in use", so `make ch` removes it first; `ch-shutdown` uses `shutdown-vmm`, because `shutdown` alone leaves the process running.
- vhost-user backends (virtiofsd, passt) need shared guest RAM.
- The TAP device is created with its offloads off, because FerrionOS's virtio-net driver acknowledges only `VIRTIO_NET_F_MAC`.
- The passt socket lives in `$XDG_RUNTIME_DIR` or `/tmp`, not in `build/`: SELinux limits passt to those two places.

### Host helpers

- `scripts/virtiofs-run.sh` and `scripts/passt-run.sh` start their daemon, run the command given after `--`, and stop the daemon when it ends; they do not `exec` the command, so their cleanup runs. Their startup timeouts are in seconds. `virtiofs-run.sh` treats a share as busy while `virtiofsd` holds the lock on `<socket>.pid`. `passt-run.sh` shortens passt's error output, which repeats its whole usage text after a bad option.
- `scripts/ch-tap-setup.sh` prepares the host for `make ch net=tap`: the TAP device `redox0`, IPv4 forwarding, firewalld NAT and dnsmasq for DHCP and DNS. It must run again after a reboot; run as root without sudo, it takes the device's owner from `TAP_USER`. It adds the sbin directories to `PATH`, which Debian leaves out for ordinary users. It recognizes a TAP device by the kernel's `tun_flags` (`IFF_TAP` is `0x0002`, `IFF_UP` is `0x1`, an owner of `-1` means none). It reads firewalld's runtime target from the `target:` line, because `--get-target` reports only the permanent one, and adds the interface to the zone itself, because firewalld does not always attach an interface created after it started. It checks dnsmasq where it actually binds (DHCP on `0.0.0.0%redox0:67`, DNS on `192.168.249.1:53`), restarts it when the device is recreated, because `--bind-interfaces` keeps the old interface index, and starts it with `--conf-file=/dev/null` so the host's `/etc/dnsmasq.conf` adds no other interfaces. Its timeouts are in seconds.
- `scripts/network-boot.sh` serves the fixed private subnet `NETWORK` (`10.82.120.0/24`), and the host must use the static address `${NETWORK}.1` with mask `255.255.255.0`; any free private subnet works.

## Consequences

- Moving the passt socket into `build/` breaks `make ch` on SELinux hosts.
- Turning the TAP offloads on, or growing the memory backend apart from `-m`, breaks networking or the shares until FerrionOS's drivers support them.

## Where

- `mk/qemu.mk`: `QEMU_BASE_DIR`, `uefi`, `kvm`, `QEMU_ON_WINDOWS`, `redoxer`, the `share`/Venus memory backend, `net=redir`, `disk=ata`
- `mk/ch.mk`: `CH_VERSION`, `CH_FIRMWARE_VERSION`, `ch_download`, `ch-check`, `ch`, `ch-shutdown`, `CH_TAP`, `CH_PASST_SOCK`
- `mk/virtiofs.mk`: `VIRTIOFSD`, `VIRTIOFS_RUN`
- `scripts/virtiofs-run.sh`, `scripts/passt-run.sh`, `scripts/ch-tap-setup.sh`, `scripts/network-boot.sh`

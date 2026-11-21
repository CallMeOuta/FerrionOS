# Cloud Hypervisor

> 📝 **Note:** FerrionOS inherits what this page describes from upstream and has not verified it yet.

[Cloud Hypervisor](https://www.cloudhypervisor.org/) is a virtual machine monitor written in Rust and built for cloud workloads. Unlike QEMU it doesn't emulate a legacy PC. There is no 8259 interrupt controller, no PIT or HPET timer, no PS/2 keyboard and no display. The guest gets CPUs, memory, an I/O APIC, a serial port and VirtIO devices. The build system can boot your image on this platform with the `make ch` command, a path that is inherited from upstream.

- [Requirements](#requirements)
- [Cloud Hypervisor And Firmware](#cloud-hypervisor-and-firmware)
- [Running FerrionOS](#running-ferrionos)
  - [Stopping The Virtual Machine](#stopping-the-virtual-machine)
- [Network](#network)
  - [User-mode Network (passt)](#user-mode-network-passt)
  - [Tap Device](#tap-device)
- [Sharing Directories](#sharing-directories)
- [Options](#options)
- [Running Cloud Hypervisor Without The Build System](#running-cloud-hypervisor-without-the-build-system)
- [Troubleshooting](#troubleshooting)

## Requirements

- An x86-64 Linux host with KVM. Your user must be able to open `/dev/kvm`.
- An image for the x86-64 architecture, the same as your host. Other CPU architectures aren't supported by `make ch` yet.
- `curl` and `sha256sum` on your host. You don't need to install Cloud Hypervisor or its firmware, the build system downloads them (see [Cloud Hypervisor And Firmware](#cloud-hypervisor-and-firmware)).
- For the default network, [passt](https://passt.top/) from 2025 or later (it needs the `--vhost-user` mode). The package is named `passt` on Fedora, Debian, Ubuntu and Arch Linux. You don't need it if you use `net=tap` or `net=no`.
- To share directories, `virtiofsd` (see [VirtIO-FS Shared Directories](./virtio-fs.md#requirements)).

## Cloud Hypervisor And Firmware

The first time you run `make ch`, the build system downloads the files it needs:

- `cloud-hypervisor-static` and `ch-remote-static`, from the [Cloud Hypervisor releases](https://github.com/cloud-hypervisor/cloud-hypervisor/releases).
- The `CLOUDHV.fd` UEFI firmware, from the [Cloud Hypervisor EDK2 releases](https://github.com/cloud-hypervisor/edk2/releases).

The versions are pinned in `mk/ch.mk` (`CH_VERSION` and `CH_FIRMWARE_VERSION`), and each file is checked against its pinned SHA-256 checksum. A download that doesn't match is deleted and `make` stops, so only checked files are ever used.

The files are stored in `build/cloud-hypervisor/x86_64`, with one directory for each version. They are shared by all configurations, and `make clean` doesn't remove them.

To only download the files, without building or starting anything (for example before you go offline), run:

```sh
make ch-fetch
```

To use other files, for example the Cloud Hypervisor package of your distribution, set `CH`, `CH_REMOTE` or `CH_FIRMWARE` to a path or to a program in your `PATH`. Only the files that you don't replace are downloaded:

```sh
make ch CH=cloud-hypervisor
```

To update a pinned version, change the version and the checksums of its files in `mk/ch.mk`. GitHub shows the SHA-256 checksum next to each file on the release page.

## Running FerrionOS

Cloud Hypervisor has no display, so FerrionOS is used through the serial console, and a configuration without a desktop, like `server`, is the best fit (see [Configuration Settings](./configuration-settings.md)). From the FerrionOS directory, run:

```sh
make ch CONFIG_NAME=server
```

If the image doesn't exist yet it's built first, like with `make qemu`, and the first run also downloads Cloud Hypervisor and its firmware. The serial console of FerrionOS appears in your terminal, and you can log in as usual.

The terminal is in raw mode while the virtual machine runs, thus `Ctrl+C` is sent to FerrionOS and doesn't stop the virtual machine.

The log of Cloud Hypervisor is written to `build/x86_64/$CONFIG_NAME/ch.log`.

### Stopping The Virtual Machine

FerrionOS can't power off Cloud Hypervisor by itself yet, so the `shutdown` command inside of FerrionOS doesn't stop the virtual machine. To stop it, run the following command in another terminal, from the FerrionOS directory:

```sh
make ch-shutdown
```

Use the same `CONFIG_NAME` that you used with `make ch`, because the control socket is in the build directory of the configuration.

## Network

### User-mode Network (passt)

By default, `make ch` gives FerrionOS a network through [passt](https://passt.top/), a user-mode network stack. The build system starts `passt` before Cloud Hypervisor and stops it when the virtual machine exits.

- It doesn't need `root`, a tap device or changes to your firewall.
- It answers the DHCP and DNS requests of FerrionOS, and carries its traffic through ordinary sockets of your host.
- FerrionOS gets the address of your host, and nothing from outside can reach FerrionOS unless you forward a port.

To pass options to `passt`, use the `CH_PASST_FLAGS` option. For example, to forward the port 8022 of your host to the port 22 of FerrionOS:

```sh
make ch CH_PASST_FLAGS="-t 8022:22"
```

To capture the network traffic in a file that you can open in Wireshark:

```sh
make ch CH_PASST_FLAGS="--pcap $XDG_RUNTIME_DIR/ferrion.pcap"
```

The socket and the log of `passt` are in `$XDG_RUNTIME_DIR` (or `/tmp`), named `redox-passt-x86_64-$CONFIG_NAME.sock` and `redox-passt-x86_64-$CONFIG_NAME.log`. They aren't in the build directory because SELinux systems only let `passt` use these directories.

### Tap Device

If you need a real network link, which your host can reach on any port and watch with `tcpdump`, use a tap device:

```sh
make ch net=tap
```

The `redox0` tap device must exist before you run the command. The `scripts/ch-tap-setup.sh` script creates it and configures your host as the DHCP server, DNS server and NAT gateway of FerrionOS:

```sh
./scripts/ch-tap-setup.sh
```

- It runs the privileged steps with `sudo`. Run it as the user that starts Cloud Hypervisor, so this user owns the tap device.
- It requires `firewalld` and `dnsmasq`. On systems without `firewalld`, use the default `passt` network.
- FerrionOS gets an IP address between `192.168.249.10` and `192.168.249.50`, and the host is `192.168.249.1`.
- The tap device and `dnsmasq` don't survive a reboot, run the script again after rebooting. It's safe to run it more than once.

To start FerrionOS without a network device, use `make ch net=no`.

## Sharing Directories

The `share` option of `make qemu` works the same way with `make ch`:

```sh
make ch share=$HOME/shared
```

The directory is available in FerrionOS at `/scheme/virtio-fs.host`. Read the [VirtIO-FS Shared Directories](./virtio-fs.md) page for sharing several directories, read-only shares and the other options.

## Options

The options are defined in `mk/ch.mk`. You can pass them on the `make` command line or add them to your `.config` file (see [Configuration Settings](./configuration-settings.md)).

| **Option**       | **Default**                  | **Description** |
|:-----------------|:-----------------------------|:----------------|
| `net`            | (none)                       | `tap` to use the `CH_TAP` tap device, `no` for no network device. Without it, `passt` is used |
| `share`          | (none)                       | Directories to share with VirtIO-FS, see [VirtIO-FS Shared Directories](./virtio-fs.md) |
| `CH`             | `build/cloud-hypervisor/x86_64/$CH_VERSION/cloud-hypervisor-static` | The Cloud Hypervisor executable, a path or a program in `PATH`. The default is downloaded (see [Cloud Hypervisor And Firmware](#cloud-hypervisor-and-firmware)) |
| `CH_REMOTE`      | `build/cloud-hypervisor/x86_64/$CH_VERSION/ch-remote-static` | The `ch-remote` program, used by `make ch-shutdown`. The default is downloaded |
| `CH_FIRMWARE`    | `build/cloud-hypervisor/x86_64/edk2-$CH_FIRMWARE_VERSION/CLOUDHV.fd` | The UEFI firmware file. The default is downloaded |
| `CH_SMP`         | `4`                          | The number of CPU cores |
| `CH_MEM`         | `2048`                       | The amount of memory, in MiB |
| `CH_API_SOCK`    | `build/$ARCH/$CONFIG_NAME/ch.sock` | The control socket, used by `make ch-shutdown` |
| `CH_LOG`         | `build/$ARCH/$CONFIG_NAME/ch.log`  | The log file of Cloud Hypervisor |
| `CH_TAP`         | `redox0`                     | The tap device used with `net=tap` |
| `CH_MAC`         | `52:54:00:12:34:56`          | The MAC address of the network device of FerrionOS |
| `CH_PASST_SOCK`  | `$XDG_RUNTIME_DIR/redox-passt-$ARCH-$CONFIG_NAME.sock` | The socket of `passt`, in `/tmp` when `XDG_RUNTIME_DIR` is not set |
| `CH_PASST_FLAGS` | (none)                       | Extra options for `passt` |
| `CHFLAGS`        | `-v --log-file $(CH_LOG)`    | Extra options for Cloud Hypervisor, the build system adds the CPU, memory, firmware, disk, network and serial options after them |

For example, to start FerrionOS with 8 CPU cores and 4 GiB of memory:

```sh
make ch CH_SMP=8 CH_MEM=4096
```

## Running Cloud Hypervisor Without The Build System

To run an image without the build system, use a command like the following one.

You can use the files that `make ch-fetch` downloads (see [Cloud Hypervisor And Firmware](#cloud-hypervisor-and-firmware)): `cloud-hypervisor-static` is the `cloud-hypervisor` program below.

```sh
cloud-hypervisor \
  --firmware CLOUDHV.fd \
  --cpus boot=4 \
  --memory size=2048M \
  --serial tty --console off \
  --disk path=harddrive.img,image_type=raw
```

FerrionOS drives the network device with only basic features for now, so turn off the offloads when you use a tap device: `--net tap=redox0,mac=52:54:00:12:34:56,offload_tso=off,offload_ufo=off,offload_csum=off`.

## Troubleshooting

- `CH=... not found. Leave CH unset to use the pinned download` (or the same for `CH_REMOTE` or `CH_FIRMWARE`): the file or program that you set doesn't exist. Fix the path, or remove the option to use the downloaded one.
- `ch: ... does not match the pinned sha256 ...`: the download is broken, or the file on the release page changed. Run the command again. If it keeps failing, the checksum in `mk/ch.mk` doesn't match the release, please report it (see [Participating](./contributing.md)).
- The download fails with a `curl` error: the first `make ch` needs network access. Run `make ch-fetch` once while you are online.
- `Cloud Hypervisor: only ARCH=x86_64 is supported`: `make ch` only supports x86-64 images.
- `Cloud Hypervisor: ARCH=... cannot run on this ... host`: Cloud Hypervisor runs the image with KVM, so the image must have the same CPU architecture as your host.
- `ch: a VM is already running`: another virtual machine uses the same control socket. Stop it with `make ch-shutdown`, using the same `CONFIG_NAME`.
- `no tap device redox0`: run `scripts/ch-tap-setup.sh`, or leave out `net=tap` to use `passt`.
- `passt not found` or `passt ... has no --vhost-user mode`: install `passt`, or a newer version, or set `PASST=/path/to/passt`.
- `passt exited and logged nothing`: if SELinux is enforcing, the socket of `passt` must be in `$XDG_RUNTIME_DIR` or `/tmp`, check the `CH_PASST_SOCK` option.
- Cloud Hypervisor fails to open `/dev/kvm`: add your user to the group that owns `/dev/kvm` (usually `kvm`), then log out and in again.
- Your terminal behaves strangely after the virtual machine exits: run the `reset` command to leave raw mode.
- The `shutdown` command in FerrionOS doesn't stop the virtual machine: this is expected for now, use `make ch-shutdown`.

# Getting Started

FerrionOS is at an early, experimental stage. It publishes no images yet: to try it, build an image from the repository and start it in a virtual machine.

On a Linux host, with `git` installed:

```sh
git clone https://github.com/CallMeOuta/FerrionOS.git
cd FerrionOS
./podman_bootstrap.sh -d
CI=1 COOKBOOK_LOGS=true make all
make qemu
```

- `./podman_bootstrap.sh -d` installs the host tools with your distribution's package manager: Podman, FUSE, make and QEMU. It also offers to install Rust through rustup, which the host needs for the tools that assemble the image. Always pass `-d`: without it, the script clones the upstream build system into a new directory and deletes itself.
- `make all` builds the desktop image for the host's architecture. A first build makes the build container, the cross toolchain and every package of the image, takes a long time, and needs 50 GB of storage or more.
- `make qemu` starts the image in QEMU. Log in as `user`, with an empty password.

The accounts of such an image are development accounts: `user` with an empty password, and `root` with the password `password`. Use the image on a test machine only ([Security](./security.md)).

The next pages go into the details:

- [Building FerrionOS](./podman-build.md) explains each step of the build, and how to get a first image faster with prebuilt packages.
- [Running FerrionOS in a Virtual Machine](./running-vm.md) describes the options of `make qemu` and the use of the emulator.
- [Running FerrionOS on Real Hardware](./real-hardware.md) and [Installing FerrionOS](./installing.md) describe paths that FerrionOS has not verified yet.
- [Trying Out FerrionOS](./trying-out-redox.md) and [Tasks](./tasks.md) show what to do in the running system.

The [Build System Reference](./build-system-reference.md) gives a complete view of the build system.

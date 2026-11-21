# Security

FerrionOS is not hardened yet. This page separates three things: what holds on an image today, what is planned, and the security design that the system inherits.

To report a vulnerability, follow the repository's [security policy](https://github.com/CallMeOuta/FerrionOS/blob/master/SECURITY.md). Never report a security weakness in a public issue.

## The State Today

Use a FerrionOS image on a test machine only.

| Area | Today | Planned |
|---|---|---|
| Login accounts | An image built from the repository's configurations carries development accounts: `user` with an empty password, in the group `sudo`, and `root` with the password `password` (`config/dev-accounts.toml`). An empty password logs in without a prompt and satisfies `sudo`. | An image that leaves the build machine carries its own accounts. |
| Boot chain | Verified boot covers the kernel and the initial filesystem only. In an image built with the default settings (`VERIFIED_BOOT=1`), the UEFI bootloader starts only a kernel and an initial filesystem that match a manifest signed with that build's development key, which is made on the build machine ([Boot Process](./boot-process.md#verified-boot)). Whoever can write to the disk can still replace the bootloader and its key, which only UEFI Secure Boot prevents, and Secure Boot covers the bootloader binary only when the bootloader has been signed and its key enrolled. `scripts/secure-boot-test.sh` shows both in QEMU with test keys. | Coverage of the rest of the filesystem, of live images and of the BIOS bootloader (i586 images), and protection against a rollback to an older signed version. |
| Exploit mitigations | Kernel and user-space hardening is incomplete: W^X enforcement in the kernel, stack canaries, and address-space layout randomization in all programs. Mitigations for CPU side-channel attacks, such as Meltdown, are not implemented. | Hardening of the kernel and of user space. |
| Privilege | `sudo`, `su` and `passwd` ask a daemon for privilege, and the repository's configurations set no setuid or setgid bit on a file. A process that runs as `root` is not confined. The system's daemons and drivers run in user space, each in its own address space, as `root`; FerrionOS has not tested how well this contains a faulty driver. | Privilege separation between `root` and the kernel. |
| Sandboxing | `login` has code that places a session in a namespace, which lists the schemes the session may open (below). FerrionOS has not tested namespaces as a boundary. The `contain` tool is a partial implementation, the default configurations do not install it, and it is not a security boundary. | Sandboxing of programs by namespace. |
| Packages | `pkg` installs from upstream's package server. FerrionOS publishes no packages. | Signed FerrionOS releases and updates. |

The security policy keeps the authoritative list of known limitations.

## Development Accounts

The accounts come from `config/dev-accounts.toml`. `config/minimal.toml` includes that file, and so does every configuration built on it, such as `server` and `desktop`. They exist so that an image can be built and tested without typing: the graphical login has `user` filled in and accepts an empty password.

An image for any other purpose needs its own accounts. A personal configuration (`config/my_<name>.toml`, which git ignores) that includes one of the repository's configurations can replace the account tables: a `[users.<name>]` table in the including file replaces the included one as a whole.

## The Inherited Design

### Namespaces

- A namespace is a list of schemes. A process can open by name only what its namespace lists.
- Each process has a namespace, and a child process inherits it.
- `login` has code that builds a namespace for the user who logs in, from `/etc/login_schemes.toml`, which `config/base.toml` writes: `root` is listed with every scheme, and `user` with a list that includes the network, the display, the filesystem and `sudo`. FerrionOS has not tested which sessions end up confined by it.
- A process in the `null` namespace can open nothing by name and can use only the file descriptors it already holds. Many daemons and drivers enter it after they have opened what they need.

### Capabilities

A [capability](https://en.wikipedia.org/wiki/File_descriptor#File_descriptors_as_capabilities) is a file descriptor that carries the right to perform specific actions. The design is moving from checks of user and group IDs toward [capability-based security](https://en.wikipedia.org/wiki/Capability-based_security), in which holding the descriptor is the permission. Many system services and drivers still decide by the caller's user and group ID.

### A Sandbox by Namespace

A program can be confined by giving it a namespace with few schemes, or with none, so that it cannot open new file descriptors. FerrionOS has not tested this as a boundary; treat it as a design direction.

### Unsafe Rust

Some parts of the system's code use unsafe Rust, which the compiler checks less than safe Rust. The components replace unsafe code with safe code where they can, and where they cannot, the review concentrates on the boundary between the two.

### Ported Programs

Programs and libraries written in C and C++ keep the memory-safety problems of their languages when they are ported. That relibc, the kernel and the drivers are written in Rust does not protect them.

# Goals

FerrionOS aims to be a complete, general-purpose operating system that puts correctness, security, safety, freedom, stability, and pragmatism first.

- **A system for everyday use.** The goal is a system that a person can use every day in place of Linux or a BSD, and that runs most Linux and BSD programs with small changes or none.
- **A complete, safe and stable Rust ecosystem.** This is a design choice that is expected to improve correctness and security (see [Why Rust?](./why-rust.md)).
- **A better security design.** The goal is to improve on other Unix-like systems through safe defaults and fewer insecure configurations. This is a goal and not the present state: FerrionOS is not hardened yet, and [Security](./security.md) lists what exists and what is planned.
- **An everyday experience that needs no explanation.** The models are macOS and Windows: a quiet boot straight to a graphical login, one consistent desktop, and ease of use. The quiet boot exists. The rest is a goal, and so is running Windows games through Proton, which is planned and does not work today (see [Influences](./influences.md)).

## What Exists Toward These Goals

### A Complete System

FerrionOS has its own kernel, drivers and filesystem, written in Rust. It builds them from upstream's sources ([Components and Their Sources](./side-projects.md)). The build system has configurations for x86_64, aarch64 and riscv64gc. FerrionOS has verified the x86_64 desktop image in QEMU; [Hardware Support](./hardware-support.md) has the details.

Programs written in C, C++ and Rust are built against `relibc`, a [C standard library](https://en.wikipedia.org/wiki/C_POSIX_library) written in Rust that implements a large part of POSIX. Many programs and libraries build without changes. Others need patches where they rely on functionality outside POSIX.

Programs with a graphical interface draw through Orbital, the display server, with the client libraries `orbclient` and `liborbital`.

[Important Programs](./important-programs.md) records which ported programs are known to run.

### The Rust Ecosystem

Rust supports the target `x86_64-unknown-redox` at Tier 2 and the other `*-unknown-redox` targets at Tier 3 ([platform support](https://doc.rust-lang.org/nightly/rustc/platform-support/redox.html)). Well-known crates such as [`winit`](https://github.com/rust-windowing/winit/), [`nix`](https://github.com/nix-rust/nix) and [`rustix`](https://github.com/bytecodealliance/rustix) support the `redox` target OS. These crates are backed by [Rust's bindings to the C standard library](https://github.com/rust-lang/libc) or by the target's implementation of the [Rust standard library](https://doc.rust-lang.org/std/).

Rust programs for this target link against `relibc`, so compiling one requires `relibc` at link time. [Redoxer](./ci.md) is upstream's tool for compiling and testing a Rust program for the target without the complete build system.

### The Security Design

The kernel is a microkernel influenced by [other microkernels and notable operating systems](./influences.md). Many system services run outside the kernel as [daemons and drivers in user space][userspace]. Drivers and system services are ordinary programs with elevated permissions. They run in a namespace that gives them access to the hardware interrupts that the kernel manages.

Programs, drivers, system services and the kernel talk to each other through [schemes][Scheme]. Schemes appear under the `/scheme` directory, and a program opens or creates one with the ordinary file API. Software that needs more can use [`libredox`](https://crates.io/crates/libredox) and the other crates described in [Libraries and APIs](./libraries-apis.md#crates).

A process sees only the schemes of its namespace. Among programs in the same namespace, the kernel and the drivers decide by the caller's user ID or group ID. The design is moving from these IDs toward [capability-based security](https://en.wikipedia.org/wiki/Capability-based_security), in which holding a file descriptor is the permission.

Program loading (`exec`) is implemented in user space, in relibc, so the kernel does not parse executables or read a program's arguments and environment.

This describes the design. [Security](./security.md) describes what an image enforces today.

[userspace]: ./user-space.md "System Services in User Space"
[Scheme]: ./schemes.md "Schemes"

## What FerrionOS Does Not Aim At

FerrionOS is not a clone of Linux or of a BSD, and full POSIX compliance is not a goal. Well-tested designs are kept where they work.

A large number of unmodified programs and libraries are therefore compatible with FerrionOS, and a program that does not fit the system's design decisions needs porting work. The trade-off is between correctness and compatibility, and both cannot always be had.

### What Makes a Program Hard to Port

A program is hard or impossible to port when one of the following holds:

1. Its source is not available, since programs are built for FerrionOS from source.
2. It is no longer maintained.
3. It uses only interfaces of one operating system, such as Linux kernel interfaces, instead of portable ones.
4. It is written in a language, or needs a compiler, that does not support the `redox` target yet.

Chromium is a well-known example: it is tuned to operating-system-specific calls and accepts little support for systems other than its main ones, and FreeBSD maintains [hundreds of patches](https://github.com/freebsd/freebsd-ports/tree/main/www/chromium/files) to keep it working. Less entangled programs, such as WebKit-based browsers, are easier to port.

### Who Decides

FerrionOS is an independent project led by its maintainer, who decides what the project builds and which changes it accepts. [Participating](./contributing.md) explains how to report problems and suggest improvements.

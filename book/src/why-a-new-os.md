# Why a New OS?

FerrionOS aims to be a robust, reliable and safe general-purpose operating system. This page describes the design choices behind it. FerrionOS inherits them from the upstream project it forked, and it has not yet tested all of the properties they are meant to give: [Security](./security.md) describes the present state.

## Written in Rust

Wherever possible, the system's code is written in [Rust](https://www.rust-lang.org/). Rust enforces a set of rules and checks on the use, sharing and deallocation of memory references. In safe Rust code this rules out most buffer overruns, use after free, and other [memory errors](https://en.wikipedia.org/wiki/Memory_safety#Types_of_memory_errors), and a large share of the security vulnerabilities in operating systems originates from such errors. The Rust compiler rejects this kind of error before it enters the code base.

### Benefits

The following items summarize the benefits of Rust:

- Memory safety

  The compiler checks memory accesses in safe code, which prevents bugs such as buffer overflows and dangling pointers.

- Type safety

  Invalid use of a type is rejected at compile time (a null value cannot cause undefined behavior, for example), and new types can be created to make invalid states impossible to express, which prevents some logic bugs and reduces the effort of formal verification.

- Thread safety

  Safe Rust code is free of data races.

[Why Rust?](./why-rust.md) discusses this choice further.

## Microkernel Architecture

The [microkernel architecture](https://en.wikipedia.org/wiki/Microkernel) moves as many system components as possible out of the operating system kernel. Drivers, subsystems and other operating system functionality run as independent processes in user space (daemons). The kernel's main responsibility is the coordination of these processes and the management of the system's resources.

Most kernels, other than some real-time operating systems, use an event-handler design. Hardware interrupts and application system calls each trigger an event that invokes the appropriate handler. The kernel runs in supervisor mode, with access to all of the system's resources. In [monolithic kernels](https://en.wikipedia.org/wiki/Monolithic_kernel), the operating system's entire response to an event must be completed in supervisor mode. A bug in the kernel, in a driver or in hardware can bring the system into a state in which it cannot respond to *any* event. Because of the large amount of code in the kernel, the potential for vulnerabilities in supervisor mode is much greater than in a microkernel design.

The hardware and the drivers of today are also far more complex than they were when monolithic kernels were adopted. Since the arrival of the Internet and of the [TCP/IP](https://en.wikipedia.org/wiki/Internet_protocol_suite) network stack in [BSD Unix](https://en.wikipedia.org/wiki/Berkeley_Software_Distribution), this complexity has grown fast, and it has brought many bugs that are hard to find because all components of a monolithic kernel share one address space.

In FerrionOS, drivers and many system services run in user mode, like ordinary programs, each in its own address space. The design aims to give each of them only the resources it needs, and to keep a failing driver or a misbehaving device from taking down the rest of the system. FerrionOS has not tested how far this holds: today a system service that exits is not restarted automatically, and daemons run with root privileges.

### What the Architecture Makes Possible

The following items summarize what a microkernel makes possible. They are properties of the architecture, and FerrionOS does not use all of them yet.

- Stability and security

  The kernel is small, and most system components are isolated in user space, which reduces the chance of a [kernel panic](https://en.wikipedia.org/wiki/Kernel_panic) and the severity of security bugs.

- Bug isolation

  Most system components run in user space. Some kinds of bugs in a component or a driver therefore cannot spread to other components or drivers.

- Stable long execution

  When an operating system runs for a long time (days, months or years), bugs accumulate effects that are hard to trace, and that can end in security issues, data corruption or a crash. Where components are isolated, some kinds of bugs cannot spread, and a component can be restarted without restarting the whole system.

- Fewer restarts

  A mature microkernel changes little, apart from bug fixes, so the system rarely needs a restart to update the kernel. Components in user space can be restarted or updated while the system runs.

- Easier development and testing

  Most system components are separate daemons in user space and the kernel is small, so they can be developed and tested independently before integration.

- Easy expansion

  A new driver or system component is added as a daemon in user space that provides a scheme.

- Modularity

  Most system components can be enabled, disabled or updated without a system restart, in a similar way to the modules of some monolithic kernels and to [livepatching](https://en.wikipedia.org/wiki/Kpatch).

- Lower maintenance cost

  Subsystems, system components and drivers can use different versions of the scheme protocol, and keep working without source changes after a change of the system's API or ABI.

You can read more about these properties on the [Microkernels](./microkernels.md) page.

## A Filesystem of Its Own

FerrionOS uses [RedoxFS](./redoxfs.md), a copy-on-write filesystem with checksums and optional encryption. Its design is inspired by [ZFS](https://en.wikipedia.org/wiki/OpenZFS) and adapted to a microkernel: the filesystem runs as a daemon in user space.

## Unix-like Tools and API

FerrionOS provides a Unix-like command interface, with many everyday tools written in Rust under familiar names and with familiar options. Its C library, relibc, provides a subset of the [POSIX](https://en.wikipedia.org/wiki/POSIX) API, so many programs written for Linux and other POSIX systems run after being recompiled. Essential programs are preferably written in Rust, and the user's own programs may be written in any language that supports the target. This gives systems and programs developed for a Unix-like platform a path to FerrionOS.

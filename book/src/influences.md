# Influences

FerrionOS's kernel and system design draw on seL4, MINIX, Plan 9, Linux and BSD. Its everyday experience draws on macOS and Windows. This page says what comes from each of them, and separates what exists from what is a goal.

## Kernel and System Design

FerrionOS inherits this design from the upstream project it forked.

### [seL4](https://sel4.systems/)

A small microkernel that is formally verified and known for its performance.

The design follows the same principles: it keeps kernel space as small as possible, by moving components to user space and reducing the number of system calls, and it keeps the cost of a context switch low.

### [MINIX](https://minix3.org/)

The most influential Unix-like system with a microkernel. It has advanced features such as system modularity, resistance to [kernel panics](https://en.wikipedia.org/wiki/Kernel_panic), driver reincarnation, protection against bad drivers, and secure interfaces for [process communication](https://en.wikipedia.org/wiki/Inter-process_communication).

The architecture of FerrionOS is similar, with its components written in Rust.

- [How MINIX influenced the design](./microkernels.md)

### [Plan 9](http://9p.io/plan9/index.html)

This operating system from Bell Labs takes the concept of "Everything is a File" to the highest level and does all system communication through the filesystem.

- [Drew DeVault explains Plan 9](https://drewdevault.com/2022/11/12/In-praise-of-Plan-9.html)
- [The influence of Plan 9 on the design](./schemes-resources.md)

### [Linux](https://www.kernel.org/)

The most advanced monolithic kernel and the biggest open-source project in the world. It brought many improvements and optimizations to Unix-like systems, and most of the programs that FerrionOS ports are written for it.

The design adopts performance techniques of Linux where they fit a microkernel, and relibc implements Linux-specific functions that ported programs expect.

### [BSD](https://en.wikipedia.org/wiki/Berkeley_Software_Distribution)

This [family](https://en.wikipedia.org/wiki/Research_Unix) of Unix systems, and its open-source descendants, added many improvements to Unix.

- [FreeBSD](https://www.freebsd.org/): [Capsicum](https://man.freebsd.org/cgi/man.cgi?capsicum(4)), a capability-based system, and [jails](https://en.wikipedia.org/wiki/Freebsd_jail), a sandbox technology, influenced the design of namespaces. Part of the system tooling was ported from FreeBSD or written again in Rust after its model.
- [OpenBSD](https://www.openbsd.org/): its [system call](https://man.openbsd.org/pledge.2), [filesystem](https://man.openbsd.org/unveil.2), [display server](https://www.xenocara.org/) and [audio server](https://man.openbsd.org/sndiod.8) sandboxes, and [other innovations](https://www.openbsd.org/innovations.html), influenced the security design.

## Everyday Experience

For what a person sees and does every day, FerrionOS looks to macOS and Windows.

### macOS

- **A quiet boot straight to a graphical login.** This exists. On UEFI, a FerrionOS desktop image shows a black screen, then a splash with a progress bar, then the graphical login, and prints no text on the way ([Boot Process](./boot-process.md#quiet-boot)).
- **One consistent desktop.** This is a goal. The desktop image currently installs the COSMIC terminal, file manager and text editor on the Orbital display server. Which desktop apps FerrionOS ships is not settled and may change.
- **Ease of use.** This is a goal: a person should be able to use the system without knowing how it is built.

### Windows

- **Games.** Running Windows games through [Proton](https://github.com/ValveSoftware/Proton) is a goal. It is planned and does not work today.

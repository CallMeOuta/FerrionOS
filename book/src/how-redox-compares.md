# How FerrionOS Compares to Other Operating Systems

FerrionOS shares a lot with other operating systems. Its kernel and system design draw on seL4, MINIX, Plan 9, Linux and BSD, and its everyday experience draws on macOS and Windows ([Influences](./influences.md)).

## System Calls

The userspace API of FerrionOS is Unix-like. For example, it has the POSIX functions `open`, `pipe`, `pipe2`, `lseek`, `read`, `write`, `brk`, `execv`, and so on. It implements userspace equivalents of most of the system calls that Unix-like systems with monolithic kernels have. The kernel's own system call interface is unstable and may differ from them, although it is closely related to the POSIX APIs built on top of it.

FerrionOS does not necessarily implement these functions as system calls. Much of the machinery for them (typically the [man(2)](https://en.wikipedia.org/wiki/Man_page#Manual_sections) functions) is provided in user space by an interface library, `relibc`.

For example, relibc implements the POSIX function `open` on top of the kernel's system calls and the schemes described below.

## "Everything is a File"

In a model largely inspired by Plan 9, [resources](./resources.md) in FerrionOS can be socket-like or file-like, which gives the system a more unified API. Resources are named by paths, similar to what you would find in Linux or another Unix system. A resource that is managed by a particular resource manager is addressed by a scheme-rooted path. The [Schemes and Resources](./schemes-resources.md) chapter explains this.

## The Kernel

The kernel of FerrionOS is a microkernel. Its architecture is largely inspired by MINIX and seL4.

In contrast to Linux or a BSD, the kernel has about 43,000 lines of Rust, comments and blank lines included. Most system services are provided in user space, either in an interface library or as daemons.

A much smaller amount of code in the kernel makes it easier to find and fix bugs and security issues. Andrew Tanenbaum, the author of MINIX, estimated that properly written C code has one bug for every 1,000 lines. By that rule of thumb, a monolithic kernel with nearly 25,000,000 lines of C could have nearly 25,000 bugs, and a kernel of 43,000 lines about 43.

In a microkernel system the code that a monolithic kernel contains is not removed. It moves to daemons in user space, where a bug has less power.

The idea is that system components and drivers that would be inside a monolithic kernel exist in user space and follow the Principle of Least Authority (POLA). Under this principle, every component:

- Is isolated in memory, as a separate user process (daemon)
  - The failure of one component does not crash other components
  - Foreign and untrusted code does not expose the entire system
  - Bugs and malware cannot spread to other components
- Has restricted communication with other components
- Does not have administrator privileges

This is what the design aims at, and FerrionOS does not meet all of it yet: system daemons run with root privileges today. [Security](./security.md) describes the present state.

## The Everyday Experience

For the everyday experience, FerrionOS looks to macOS and Windows, not to other Unix-like systems.

| | macOS and Windows | FerrionOS today |
|---|---|---|
| Boot | Quiet, straight to a graphical login | The same on UEFI: a black screen, a splash with a progress bar, then the graphical login ([Boot Process](./boot-process.md#quiet-boot)) |
| Desktop | One consistent desktop that the system ships | A goal. The desktop image currently installs the COSMIC terminal, file manager and text editor on the Orbital display server; which desktop apps FerrionOS ships is not settled and may change |
| Ease of use | A person can use the system without knowing how it is built | A goal |
| Games | Windows runs them directly | Running Windows games through Proton is a goal. It is planned and does not work today |

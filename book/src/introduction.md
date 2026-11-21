# Introduction

<img class="ferrion-logo" alt="FerrionOS" width="511" height="180">

This is the FerrionOS Book. It describes how FerrionOS is designed, how to build and run it, and how to develop for it.

FerrionOS began as a fork of Redox OS and is developed independently; it is not affiliated with or endorsed by the Redox OS project.

The book began as a copy of the upstream project's book, which is released under the MIT license, and its pages are being checked against FerrionOS one at a time. A page that describes something FerrionOS has not verified says so. Where the book and the [repository](https://github.com/CallMeOuta/FerrionOS) disagree, the repository is right.

To build FerrionOS and start it in a virtual machine, see [Getting Started](./getting-started.md).

To report a problem or to take part in another way, see [Participating](./contributing.md).

## Conventions

### Notices

The following notices are used throughout the book to convey noteworthy information:

| Notice     | Meaning |
|:---------- |:------- |
| 🛈 **Info**    | Provides neutral information to deliver key facts. |
| 📝 **Note**    | Provides information to enhance understanding. |
| 💡 **Tip**     | Offers suggestions to optimize an experience. |
| ⚠️ **Warning** | Highlights potential risks or mistakes. |

### Names

The book calls the project that FerrionOS forked "upstream". Some components and identifiers keep the names that software matches, such as the filesystem RedoxFS, the C library relibc, and the target triple `x86_64-unknown-redox`.

## What is FerrionOS?

FerrionOS is a general-purpose operating system written in [Rust](https://www.rust-lang.org). It is a Unix-like system built on a microkernel, and it aims to be reliable, secure, usable, correct, and free.

Its C library, relibc, implements a large part of [POSIX](https://en.wikipedia.org/wiki/POSIX), so many programs written for Linux and BSD systems can be compiled for FerrionOS from their source.

The kernel and the system design draw on [seL4](https://sel4.systems/), [MINIX](https://minix3.org/), [Plan 9](http://9p.io/plan9/index.html), [Linux](https://www.kernel.org/) and [BSD](https://en.wikipedia.org/wiki/Berkeley_Software_Distribution). The everyday experience draws on macOS and Windows: a quiet boot straight to a graphical login, one consistent desktop, and ease of use. [Influences](./influences.md) says what comes from each of them, and which parts exist and which are goals.

This book is written so that no prior knowledge of Rust or of operating-system development is required.

### The State of FerrionOS

FerrionOS is at an early stage.

- It publishes no images, packages or releases yet. An image is built from the repository.
- FerrionOS has verified one configuration: the x86_64 desktop image boots in QEMU to a graphical login. [Hardware Support](./hardware-support.md) has the details.
- It is not hardened yet. [Security](./security.md) lists what exists and what is planned.
- The system's components (kernel, drivers, C library, filesystem and others) are built from upstream's sources, with FerrionOS's changes applied as patches. [Components and Their Sources](./side-projects.md) lists them.

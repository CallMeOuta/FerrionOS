# Developer FAQ

This FAQ contains technical questions and answers for developers and testers. To suggest a new question or answer, read the [Participating](./contributing.md) page.

- [General Questions](#general-questions)
    - [Why does FerrionOS have unsafe Rust code?](#why-does-ferrionos-have-unsafe-rust-code)
    - [Why does FerrionOS have Assembly code?](#why-does-ferrionos-have-assembly-code)
    - [Why does FerrionOS do cross-compilation?](#why-does-ferrionos-do-cross-compilation)
    - [Why does FerrionOS port software from source instead of offering binary compatibility?](#why-does-ferrionos-port-software-from-source-instead-of-offering-binary-compatibility)
    - [When relibc ABI breaks?](#when-relibc-abi-breaks)
    - [What are the CPU requirements of FerrionOS?](#what-are-the-cpu-requirements-of-ferrionos)
    - [Why does FerrionOS have its own build system?](#why-does-ferrionos-have-its-own-build-system)
    - [How can I port a program?](#how-can-i-port-a-program)
    - [How can I write a driver?](#how-can-i-write-a-driver)
    - [How can I debug?](#how-can-i-debug)
    - [What are the software and hardware requirements for development?](#what-are-the-software-and-hardware-requirements-for-development)
    - [Does FerrionOS support OpenGL and Vulkan?](#does-ferrionos-support-opengl-and-vulkan)
- [Build System Questions](#build-system-questions)
    - [What is the correct way to update the build system?](#what-is-the-correct-way-to-update-the-build-system)
    - [How can I verify if my build system is up-to-date?](#how-can-i-verify-if-my-build-system-is-up-to-date)
    - [What is a recipe?](#what-is-a-recipe)
    - [When I should rebuild the build system or recipes from scratch?](#when-i-should-rebuild-the-build-system-or-recipes-from-scratch)
    - [How can I test my changes on real hardware?](#how-can-i-test-my-changes-on-real-hardware)
    - [How can I insert files into the FerrionOS image?](#how-can-i-insert-files-into-the-ferrionos-image)
    - [How can I change my FerrionOS variant?](#how-can-i-change-my-ferrionos-variant)
    - [How can I increase the filesystem size of my QEMU image?](#how-can-i-increase-the-filesystem-size-of-my-qemu-image)
    - [How can I change the CPU architecture of my build system?](#how-can-i-change-the-cpu-architecture-of-my-build-system)
    - [How can I cross-compile to ARM64 from a x86-64 computer?](#how-can-i-cross-compile-to-arm64-from-a-x86-64-computer)
    - [How can I use a recipe in my FerrionOS image?](#how-can-i-use-a-recipe-in-my-ferrionos-image)
    - [How to update RedoxFS?](#how-to-update-redoxfs)
    - [I made changes to my recipe. What is the quickest way to test it in QEMU?](#i-made-changes-to-my-recipe-what-is-the-quickest-way-to-test-it-in-qemu)
    - [I made changes to multiple recipes. What is the quickest way to test it in QEMU?](#i-made-changes-to-multiple-recipes-what-is-the-quickest-way-to-test-it-in-qemu)
    - [How can I disable the recipe compilation?](#how-can-i-disable-the-recipe-compilation)
    - [How can I disable recipe compilation except for a specific recipe?](#how-can-i-disable-recipe-compilation-except-for-a-specific-recipe)
    - [How to disable the automatic recipe source update?](#how-to-disable-the-automatic-recipe-source-update)
    - [How can I install the packages needed by recipes (Native Build) or Podman without a new download of the build system?](#how-can-i-install-the-packages-needed-by-recipes-native-build-or-podman-without-a-new-download-of-the-build-system)
    - [How can I build the toolchain from source?](#how-can-i-build-the-toolchain-from-source)
- [Porting Questions](#porting-questions)
    - [How to determine if some program is portable to FerrionOS?](#how-to-determine-if-some-program-is-portable-to-ferrionos)
    - [How to determine the dependencies of some program?](#how-to-determine-the-dependencies-of-some-program)
    - [How can I configure the build system of the recipe?](#how-can-i-configure-the-build-system-of-the-recipe)
    - [How can I search for functions in relibc?](#how-can-i-search-for-functions-in-relibc)
    - [What are the requirements for a recipe?](#what-are-the-requirements-for-a-recipe)
    - [What are the possible problems when porting applications and libraries?](#what-are-the-possible-problems-when-porting-applications-and-libraries)
    - [Why are C/C++ applications and libraries hard and time consuming to port?](#why-are-cc-applications-and-libraries-hard-and-time-consuming-to-port)
- [Scheme Questions](#scheme-questions)
    - [What is a scheme?](#what-is-a-scheme)
    - [When does a regular program need to use a scheme?](#when-does-a-regular-program-need-to-use-a-scheme)
    - [When would I write a program to implement a scheme?](#when-would-i-write-a-program-to-implement-a-scheme)
    - [How do I use a scheme for sandboxing a program?](#how-do-i-use-a-scheme-for-sandboxing-a-program)
    - [How can I see all userspace schemes?](#how-can-i-see-all-userspace-schemes)
    - [How can I see all kernel schemes?](#how-can-i-see-all-kernel-schemes)
    - [What is the difference between kernel and userspace schemes?](#what-is-the-difference-between-kernel-and-userspace-schemes)
    - [How does a userspace daemon provide file-like services?](#how-does-a-userspace-daemon-provide-file-like-services)
    - [How are the system calls used by userspace daemons?](#how-are-the-system-calls-used-by-userspace-daemons)
- [Troubleshooting Questions](#troubleshooting-questions)
    - [Scripts](#scripts)
        - [I can't download the build system bootstrap scripts, how can I fix this?](#i-cant-download-the-build-system-bootstrap-scripts-how-can-i-fix-this)
        - [I tried to run the "podman_bootstrap.sh" and "native_bootstrap.sh" scripts but got an error, how to fix this?](#i-tried-to-run-the-podman_bootstrapsh-and-native_bootstrapsh-scripts-but-got-an-error-how-to-fix-this)
    - [Build System](#build-system)
        - [I ran "make all" but it showed a "rustup can't be found" message, how can I fix this?](#i-ran-make-all-but-it-showed-a-rustup-cant-be-found-message-how-can-i-fix-this)
        - [I tried all troubleshooting methods but my build system is still broken, how can I fix that?](#i-tried-all-troubleshooting-methods-but-my-build-system-is-still-broken-how-can-i-fix-that)
    - [Recipes](#recipes)
        - [I had an error with a recipe, how can I fix that?](#i-had-an-error-with-a-recipe-how-can-i-fix-that)
        - [I tried all methods of the "Troubleshooting the Build" page and my recipe still doesn't build, what can I do?](#i-tried-all-methods-of-the-troubleshooting-the-build-page-and-my-recipe-still-doesnt-build-what-can-i-do)
        - [When I run "make r.recipe" I get a syntax error, how can I fix that?](#when-i-run-make-rrecipe-i-get-a-syntax-error-how-can-i-fix-that)
        - [When I run "cargo update" on some recipe source it calls rustup to install other Rust toolchain version, how can I fix that?](#when-i-run-cargo-update-on-some-recipe-source-it-calls-rustup-to-install-other-rust-toolchain-version-how-can-i-fix-that)
        - [I added the dependency of my program in the "recipe.toml" file but the program build system doesn't detect it, then I installed the program dependency on my Linux distribution and it detected it, why?](#i-added-the-dependency-of-my-program-in-the-recipetoml-file-but-the-program-build-system-doesnt-detect-it-then-i-installed-the-program-dependency-on-my-linux-distribution-and-it-detected-it-why)
        - [I made changes to system daemons, drivers and RedoxFS but they aren't applied in the FerrionOS image, how can I fix that?](#i-made-changes-to-system-daemons-drivers-and-redoxfs-but-they-arent-applied-in-the-ferrionos-image-how-can-i-fix-that)
    - [QEMU](#qemu)
        - [How can I kill the QEMU process if FerrionOS freezes or gets a kernel panic?](#how-can-i-kill-the-qemu-process-if-ferrionos-freezes-or-gets-a-kernel-panic)
    - [Real Hardware](#real-hardware)
        - [I got a kernel panic, what can I do?](#i-got-a-kernel-panic-what-can-i-do)
        - [Some driver is not working with my hardware, what can I do?](#some-driver-is-not-working-with-my-hardware-what-can-i-do)

## General Questions

### Why does FerrionOS have unsafe Rust code?

In some cases `unsafe` declarations are needed to allow some low-level tasks, for example at certain parts in the kernel and drivers; these unsafe parts are generally wrapped with a safe interface.

These are the cases where unsafe Rust is mandatory:

- Implementing a foreign function interface (FFI) (for example the relibc API)
- Working with system calls directly (you should use `libredox`, `relibc` or Rust `libstd` library instead of `redox_syscall`)
- Working with memory mapping and stack allocation
- Working with hardware devices

The design aims to minimize the amount of `unsafe` declared Rust code.

Unsafe Rust still has most of the compiler verification and allows some safe Rust syntax usage, thus it is still more safe than C and C++.

Read the following pages to learn more about Unsafe Rust:

- https://doc.rust-lang.org/book/ch20-01-unsafe-rust.html
- https://doc.rust-lang.org/nomicon/meet-safe-and-unsafe.html

### Why does FerrionOS have Assembly code?

[Assembly](https://en.wikipedia.org/wiki/Assembly_language) is the core of low-level because it's a CPU-specific programming language and deals with things that aren't possible or feasible to do in high-level languages like Rust.

Sometimes required or preferred for accessing hardware, or for carefully optimized hot spots.

Reasons to use Assembly instead of a higher level programming language:

- Deal with low-level things (those that can't be handled by Rust)
- Writing constant time algorithms for cryptography
- Optimizations

Places where Assembly is used:

- `kernel` - Interrupt and system call entry routines, context switching, special CPU instructions and registers
- `drivers` - Port IO need special instructions (x86_64)
- `relibc` - Some parts of the C runtime

### Why does FerrionOS do cross-compilation?

[Cross-compilation](https://en.wikipedia.org/wiki/Cross_compiler) is when you build a program or library from one CPU architecture to another CPU architecture or one operating system to another operating system, but it requires more configuration than native compilation.

Read some of the reasons below:

- When developing a new operating system you can't build applications inside of it because the system interfaces are premature. Thus you need to build the applications from your host system to the new OS and transfer the binaries to the filesystem of the new OS.
- Cross-compilation reduces the porting requirements because you don't need to support the compiler of the program's programming language, the program's build system and build tools. You just need to port the programming language standard library (if used), program libraries or the program source code (dependency-free).
- Some developers prefer to develop from other operating systems like Linux, MacOS, FreeBSD or Windows, the same applies for Linux where some developers write code on MacOS and test their kernel builds in a virtual machine (mostly QEMU) or real hardware.

(Interpreted applications and scripts don't need cross-compilation but the programming language's interpreter or possible compiled dependencies needs to be ported and cross-compiled to FerrionOS)

### Why does FerrionOS port software from source instead of offering binary compatibility?

Ports using POSIX/Linux source compatibility require much less effort, are easier and have less maintenance cost than supporting BSD (BSD libc and kernel ABIs) or Linux (glibc/musl and kernel ABIs) binary compatibility (which would increase API complexity and feature sets and make them mandatory).

- glibc = GNU C Standard Library, with some POSIX APIs
- BSD libc = BSD C Standard Library, with some POSIX APIs

This decision aims to:

- Avoid behavior from the monolithic-kernel architecture that would reduce the microkernel architecture reliability, security and performance
- Keep the system more simple and improve faster, greatly reducing all kinds of possible bugs
- Easily improve system API by not relying on the behavior of BSD and Linux ABIs, but reimplementing some C Standard Library and kernel APIs that can have a good/acceptable microkernel-based implementation
- Improve the reliability and security of programs when possible
- Greatly reduce porting effort by not needing to support a big feature set in a library or application ABI
- Greatly reduce maintenance cost
- Avoid BSD and Linux ABI complexity
- Avoid porting parts of BSD or Linux kernel APIs that are very complex/hard and may reduce the microkernel architecture reliability, security and performance

### When relibc ABI breaks?

When dynamically linked (most cases) it breaks when:

- A function is renamed
- A function is removed
- Any function with `extern C` have their arguments changed
- The size or alignment of any struct is changed

New functions don't break ABI but are backwards-incompatible (programs compiled to previous relibc API can't use them).

When statically linked, the kernel ABI can't break.

### What are the CPU requirements of FerrionOS?

Read [this](./hardware-support.md#cpu-requirements) section.

### Why does FerrionOS have its own build system?

The FerrionOS build system (mostly Cookbook) is a fork of upstream's, which was created to be fully adapted to easy/fast development of the system, cross-compilation and porting. Other build systems do not offer this, or not easily.

Redoxer is upstream's minimal variant of the build system, focused on quick system/application/CI testing without complex C/C++ dependency, QEMU and development configuration; read the [Redoxer](./ci.md) page.

### How can I port a program?

Read the [Application Porting](./porting-applications.md) page.

### How can I write a driver?

Drivers live in the `drivers/` directory of the `base` source (recipe `recipes/core/base`); `make f.base` fetches it into `recipes/core/base/source/`. The [Drivers](./drivers.md) page describes how drivers fit into the system, and the [Coding and Building](./coding-and-building.md) page shows how to change a component in your own checkout.

### How can I debug?

Read the [Debug Methods](./troubleshooting.md#debug-methods) section.

### Does FerrionOS support OpenGL and Vulkan?

Read the [Software Rendering](./graphics-windowing.md#software-rendering) section.

### What are the software and hardware requirements for development?

FerrionOS inherits these requirements from upstream and has not measured them.

- If you are using the Podman Build you need any Linux or Unix-like distribution supporting Podman 4.0 or newer and FUSE 3.x (if you have problems with FUSE in the host system there's [this](./advanced-podman-build.md#installing-without-fuse) workaround to run FUSE inside the Podman container instead of host system)
- If you are using the Native Build, upstream recommends a recent Ubuntu, PopOS or Fedora version

Upstream gives the following hardware requirements as enough for fast compilation of the system and most applications, but some heavy applications may require more.

- An Intel or AMD CPU newer than 10 years with 4 cores/threads or more
- 4GB DDR4 RAM or more (8GB or 16GB for heavy applications)
- 50GB of storage space or more (a high-performance HDD, SATA SSD, and/or NVMe SSD is recommended)
- An Internet connection good enough to not cause timeouts

## Build System Questions

### What is the correct way to update the build system?

Read the [Update The Build System](./build-system-reference.md#update-the-build-system) section.

### How can I verify if my build system is up-to-date?

After the `make pull` command, run the `git rev-parse HEAD` command and verify if it prints the commit hash that the `git ls-remote origin master` command prints, or the latest commit hash of the [commit list](https://github.com/CallMeOuta/FerrionOS/commits/master).

### What is a recipe?

A system component or tooling, application or library port, or meta-package for FerrionOS.

### When I should rebuild the build system or recipes from scratch?

Sometimes the execution of the `make pull rebuild` command is not enough to update the build system and recipes because of breaking changes, learn what to do on the following changes:

- New relibc functions and fixes: to allow a recipe to use the new relibc functions you need to rebuild it with the `make cr.recipe-name` command, sometimes relibc fixes require a complete system rebuild by running the `make repo_clean rebuild` command
- Dependency changes on recipes: if the shared objects had symbol changes or the recipe is statically linked, run the `make cr.recipe-name` command
- Configuration changes on recipes: run the `make cr.recipe-name` command
- Source code changes on recipes: if the shared objects had symbol changes or the recipe is statically linked, run the `make ucr.recipe-name` command
- Changes on the location of the build system artifacts: run the `make clean pull all` command to not cause breakage with the previous artifacts locations, if the previous location of the build artifacts had contents you can try to fix manually or download the build system again to avoid confusion or fix difficult breakage

### How can I test my changes on real hardware?

Read the [Testing on Real Hardware](./coding-and-building.md#testing-on-real-hardware) section.

### How can I insert files into the FerrionOS image?

If you use a [recipe](./coding-and-building.md#insert-files-in-the-image-using-a-recipe) your changes will persist after the `make image` command, but you can also [mount](./coding-and-building.md#insert-files-in-the-image) the filesystem of the image to insert them directly.

### How can I change my FerrionOS variant?

Insert the `CONFIG_NAME?=your-config-name` environment variable to your `.config` file, read the [config](./configuration-settings.md#config) section for more details.

### How can I increase the filesystem size of my QEMU image?

Change the `filesystem_size` data type of your filesystem configuration at: `config/$ARCH/your-config.toml` and run the `make image` command, read the [Filesystem Size](./configuration-settings.md#filesystem-size) section for more details.

### How can I change the CPU architecture of my build system?

Insert the `ARCH?=your-cpu-arch` environment variable on your `.config` file and run the `make all` command, read the [config](./configuration-settings.md#config) section for more details.

If you want to do it temporarily run the `make all ARCH=$ARCH` command.

If you want to clean the binaries of the previous CPU architecture run the following command:

```sh
make repo_clean ARCH=previous-cpu-arch
```

### How can I cross-compile to ARM64 from a x86-64 computer?

Insert the `ARCH?=aarch64` environment variable on your `.config` file and run the `make all` command.

If you want to do it temporarily run the `make all ARCH=aarch64` command.

### How can I use a recipe in my FerrionOS image?

If you want to quickly install the recipe package until the next image creation, run the following command:

```sh
make rp.recipe-name
```

Or (if you want to use a remote package, which is upstream's prebuilt package, to install it more quickly)

```sh
make rp.recipe-name REPO_BINARY=1
```

If you want to permanently install the recipe on your image, read the following steps.

- Go to your filesystem configuration and add the recipe:

```sh
nano config/$ARCH/your-config.toml
```

```toml
[packages]
...
recipe-name = {}
...
```

Or (for a remote package)

```toml
[packages]
...
recipe-name = "binary"
...
```

- Build the recipe and install in an existing image

```sh
make rp.recipe-name
```

Or (for a remote package)

```sh
make rp.recipe-name REPO_BINARY=1
```

### How to update RedoxFS?

The `base` recipe doesn't automatically add your RedoxFS changes to `initfs`, thus it need a manual update.

Read [this](./coding-and-building.md#how-to-update-redoxfs) section to learn how to do it.

### I made changes to my recipe. What is the quickest way to test it in QEMU?

If you did incremental changes (which don't change the binary symbols), run the following command:

- Rebuild the recipe, install to an existing image and launch QEMU

```sh
make rp.recipe-name qemu
```

If you did breaking changes (which changed the binary symbols) run the following command:

- Rebuild the recipe, install to an existing image and launch QEMU

```sh
make crp.recipe-name qemu
```

### I made changes to multiple recipes. What is the quickest way to test it in QEMU?

- Rebuild the modified recipes, install to an existing image and launch QEMU:

```sh
make rp.recipe1,recipe2 qemu
```

If you don't want to specify all modified recipes run the following command:

- Rebuild the modified recipes, install to an existing image and launch QEMU:

```sh
make rebuild-push qemu
```

### How can I disable the recipe compilation?

If you just want to download the pre-built package of one recipe, run the following command:

```sh
make bcr.recipe-name
```

If you want to download the pre-built packages of all recipes, insert the `REPO_BINARY?=1` environment variable to your `.config` file, it will download pre-compiled packages from upstream's package server if available (`REMOTE_PKG_SOURCE` in `src/lib.rs` sets the address). A pre-built package is upstream's build of upstream's source. The `base`, `bootloader`, `kernel` and `userutils` recipes are still built from source: `config/base.toml` sets them to `"source"`, because FerrionOS carries patches for them.

### How can I disable recipe compilation except for a specific recipe?

After inserting the `REPO_BINARY?=1` environment variable to your `.config` file, run the following command:

```sh
make scr.recipe-name
```

Or go to your filesystem configuration and change the recipe type to `source` :

```sh
nano config/$ARCH/your-config.toml
```

```toml
[packages]
...
recipe-name = "source"
...
```

- Install the recipe package in the FerrionOS image

```sh
make rp.recipe-name
```

Or (if the above doesn't work)

```sh
make rebuild
```

### How to disable the automatic recipe source update?

The build system automatically update recipe sources if new upstream commits exist, which can break your local changes.

To learn how to disable it for one or multiple recipes read [this](./configuration-settings.md#local-recipe-changes) section.

To learn how to disable it for all recipes read [this](./configuration-settings.md#cookbook-offline-mode) section.

### How can I install the packages needed by recipes (Native Build) or Podman without a new download of the build system?

- Run the following command from your build system:

```sh
./native_bootstrap.sh -d
```

(If you are using Podman this process is automatic)

Or (for Podman dependencies)

- Run the following command from your build system:

```sh
./podman_bootstrap.sh -d
```

### How can I build the toolchain from source?

- Disable the `PREFIX_BINARY` environment variable inside of your `.config` file:

```sh
nano .config
```

```
PREFIX_BINARY?=0
```

- Clean the previous toolchain and recipe binaries and build new ones:

```sh
make prefix_clean repo_clean prefix rebuild
```

## Porting Questions

### How to determine if some program is portable to FerrionOS?

- The source code of the program must be available
- The program should use cross-platform libraries (if not, more porting effort is required)
- The program's build system should support cross-compilation (if not, more porting effort is required)
- The program shouldn't directly use the Linux kernel API in its code (if not, more porting effort is required)

Some APIs of the Linux kernel can be ported while others cannot, because they require a complete Linux kernel.

### How to determine the dependencies of some program?

Read the [Dependencies](./porting-applications.md#dependencies-1) section.

### How can I configure the build system of the recipe?

Read the [Templates](./porting-applications.md#templates) section.

### How can I search for functions in relibc?

Read the [Search for functions on relibc](./porting-applications.md#search-for-functions-on-relibc) section.

### What are the requirements for a recipe?

Read the [Packaging Policy](./porting-applications.md#packaging-policy) section.

### What are the possible problems when porting applications and libraries?

- Missing build tools
- Cross-compilation configuration problems
- Lack of patches for the `redox` target
- Missing C, POSIX or Linux library functions in relibc
- Runtime crashes or errors

### Why are C/C++ applications and libraries hard and time consuming to port?

- C/C++ don't have an official, advanced and automatic dependency manager and build system which force applications and libraries to select competing build systems with different configurations (GNU Make, GNU Autotools, CMake, Meson and others), projects like [Conan](https://conan.io/) and [vcpkg](https://vcpkg.io/) tried to solve this problem but weren't adopted by most applications/libraries and lack many libraries
- applications and libraries need to manually manage the library versions, to workaround this some applications use bundled libraries which can difficult patching when needed
- Some build systems lack good cross-compilation support which means they require more tweaks and sometimes hacks
- As libraries are manually managed applications with many dependencies, they can take hours to port depending on available library documentation/configuration and developer experience
- Some applications and libraries have bad or lacking documentation about build instructions and configuration

## Scheme Questions

### What is a scheme?

Read the [Schemes and Resources](./schemes-resources.md) page.

### When does a regular program need to use a scheme?

Most schemes are used internally by system components or relibc, you don't need to access them directly. One exception is the pseudoterminal for your command window, which is accessed using the value of `$TTY`, which might have a value of e.g. `pty:18`. Some low-level graphics programming might require you to access your display, which might have a value of e.g. `display:3`

### When would I write a program to implement a scheme?

If you are implementing a kernel service, userspace service or a device driver.

### How do I use a scheme for sandboxing a program?

The `contain` program (recipe `recipes/core/contain`) provides a partial implementation of sandboxing using schemes and namespaces. It is not a security boundary; read the [Security](./security.md) page.

### How can I see all userspace schemes?

Read the [Userspace Schemes](./schemes.md#userspace-schemes) section.

### How can I see all kernel schemes?

Read the [Kernel Schemes](./schemes.md#kernel-schemes) section.

### What is the difference between kernel and userspace schemes?

Read the [Kernel vs Userspace Schemes](./schemes.md#kernel-vs-userspace-schemes) section.

### How does a userspace daemon provide file-like services?

When a regular program calls `open`, `read`, `write`, etc. on a file-like resource, the kernel translates that to a message of type `syscall::schemev2::Sqe` (a submission queue entry), describing the file operation, and makes it available for reading on the appropriate daemon's scheme file descriptor. The daemon answers by writing a message of type `syscall::schemev2::Cqe` (a completion queue entry) to the same file descriptor, and the kernel converts it to the result of the system call. See the [Providing a Scheme](./scheme-operation.md#providing-a-scheme) section and the [Communication](./communication.md) page for more information.

### How are the system calls used by userspace daemons?

All userspace daemons use the system calls through `relibc` (recipe `recipes/core/relibc`) like any normal program.

## Troubleshooting Questions

### Scripts

#### I can't download the build system bootstrap scripts, how can I fix this?

The bootstrap scripts are not downloaded separately: `podman_bootstrap.sh` and `native_bootstrap.sh` are in the FerrionOS directory that the `git clone https://github.com/CallMeOuta/FerrionOS.git` command creates. Run them from that directory with the `-d` option.

#### I tried to run the "podman_bootstrap.sh" and "native_bootstrap.sh" scripts but got an error, how to fix this?

- Verify if you ran the script from the FerrionOS directory with the `-d` option (`./podman_bootstrap.sh -d` or `./native_bootstrap.sh -d`); without `-d` the script clones upstream's build system into a new `redox` directory and deletes itself.
- Verify if you have the GNU Bash shell installed on your system.
- Verify if Podman is supported on your operating system.
- Verify if your operating system is [supported](./building-redox.md) on the `native_bootstrap.sh` script

### Build System

#### I ran "make all" but it showed a "rustup can't be found" message, how can I fix this?

Run the following command:

```sh
source ~/.cargo/env
```

(If you installed rustup before the first `podman_bootstrap.sh` or `native_bootstrap.sh` execution, this error doesn't happen)

#### I tried all troubleshooting methods but my build system is still broken, how can I fix that?

If the `make clean pull container_clean all` command does not work, download a new build system copy as the [New Build System Copy](./troubleshooting.md#new-build-system-copy) section describes. If the new copy fails in the same way, read the [Participating](./contributing.md) page to learn how to report the problem.

### Recipes

#### I had an error with a recipe, how can I fix that?

Read the [Solving Compilation Problems](./troubleshooting.md#solving-compilation-problems) section.

#### I tried all methods of the "Troubleshooting the Build" page and my recipe still doesn't build, what can I do?

- Missing dependencies
- Environment leakage: when some part of the recipe build system does native Linux compilation instead of cross-compilation to FerrionOS
- Misconfigured cross-compilation
- The recipe needs to be ported to FerrionOS

#### When I run "make r.recipe" I get a syntax error, how can I fix that?

Verify if your `recipe.toml` file has some typo, missing data type or value.

#### When I run "cargo update" on some recipe source it calls rustup to install other Rust toolchain version, how can I fix that?

It happens because Cargo is not using the Rust compiler of the cross toolchain, which is upstream's fork of the Rust compiler (recipe `recipes/dev/rust`); to fix that run `make env` from the FerrionOS directory.

It opens a shell with the environment of the build system: the cross toolchain comes first in PATH and the variables that the makefiles export, such as `RUSTUP_TOOLCHAIN`, are set (the other `make` commands already run their commands in this environment). Leave the shell with `exit`.

#### I added the dependency of my program in the "recipe.toml" file but the program build system doesn't detect it, then I installed the program dependency on my Linux distribution and it detected it, why?

Read the [Environment Leakage](./troubleshooting.md#environment-leakage) section.

#### I made changes to system daemons, drivers and RedoxFS but they aren't applied in the FerrionOS image, how can I fix that?

You forgot to update initfs which is manual, read [this](./coding-and-building.md#how-to-update-redoxfs) section to learn how to do this.

### QEMU

#### How can I kill the QEMU process if FerrionOS freezes or gets a kernel panic?

Read the [Kill A Frozen FerrionOS VM](./troubleshooting.md#kill-a-frozen-ferrionos-vm) section.

### Real Hardware

#### I got a kernel panic, what can I do?

Read the [Kernel Panic](./troubleshooting.md#kernel-panic) section.

#### Some driver is not working with my hardware, what can I do?

Read the [Debug Methods](./troubleshooting.md#debug-methods) section, and read the [Participating](./contributing.md) page to learn how to report the problem.

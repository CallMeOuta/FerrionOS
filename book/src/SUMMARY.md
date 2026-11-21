# The FerrionOS Book

[Introduction](./introduction.md)

# Introduction

- [Introducing FerrionOS](./introducing-redox.md)
  - [Goals](./our-goals.md)
  - [Philosophy](./philosophy.md)
  - [Why a New OS?](./why-a-new-os.md)
  - [Why Rust?](./why-rust.md)
  - [Use Cases](./redox-use-cases.md)
  - [How FerrionOS Compares to Other Operating Systems](./how-redox-compares.md)
  - [Influences](./influences.md)
  - [Hardware Support](./hardware-support.md)
  - [Important Programs](./important-programs.md)
  - [Components and Their Sources](./side-projects.md)

# Architecture and Design

- [System Design](./system-design.md)
  - [Microkernels](./microkernels.md)
  - [Boot Process](./boot-process.md)
  - [The Kernel](./kernel.md)
  - [System Services in User Space](./user-space.md)
  - [Communication](./communication.md)
  - [Memory Management](./memory.md)
  - [Scheduling](./scheduling.md)
  - [Drivers](./drivers.md)
  - [RedoxFS](./redoxfs.md)
  - [Windowing and Graphics](./graphics-windowing.md)
  - [Security](./security.md)
  - [Features](./features.md)
  - [Package Management](./package-management.md)

- [Schemes and Resources](./schemes-resources.md)
  - [Scheme-rooted Paths](./scheme-rooted-paths.md)
  - [Resources](./resources.md)
  - [Schemes](./schemes.md)
  - ["Everything is a File"](./everything-is-a-file.md)
  - [Stitching It All Together](./stitching-it-all-together.md)
  - [Scheme Operation](./scheme-operation.md)
  - [Event Scheme](./event-scheme.md)
  - [An Example](./example.md)

- [Programs and Libraries](./programs-libraries.md)
  - [Components of FerrionOS](./components.md)
  - [GUI](./gui.md)
  - [Shells](./shell.md)
  - [System Tools](./system-tools.md)

# Getting Started

- [Getting Started](./getting-started.md)
  - [Running FerrionOS in a Virtual Machine](./running-vm.md)
  - [Running FerrionOS on Real Hardware](./real-hardware.md)
  - [Installing FerrionOS](./installing.md)
  - [Trying Out FerrionOS](./trying-out-redox.md)
  - [Tasks](./tasks.md)
  - [Package Manager](./pkg.md)

# Participating

- [Participating](./contributing.md)

# Developing with and for FerrionOS

- [The Build Process](./build-process.md)
  - [Building FerrionOS](./podman-build.md)
  - [Native Build](./building-redox.md)
  - [Self-hosted Development](./self-hosted.md)
  - [From Nothing To Hello World](./nothing-to-hello-world.md)
  - [Configuration Settings](./configuration-settings.md)
  - [Build System Reference](./build-system-reference.md)
  - [Advanced Podman Build](./advanced-podman-build.md)
  - [Advanced Native Build](./advanced-build.md)
  - [VirtIO-FS Shared Directories](./virtio-fs.md)
  - [Cloud Hypervisor](./cloud-hypervisor.md)
  - [Working with ARM64](./aarch64.md)
  - [Troubleshooting the Build](./troubleshooting.md)
  - [Build Phases](./build-phases.md)

- [Developing for FerrionOS](./developing-for-redox.md)
  - [Developer FAQ](./developer-faq.md)
  - [References](./references.md)
  - [Libraries and APIs](./libraries-apis.md)
  - [Coding and Building](./coding-and-building.md)
  - [Including Programs in FerrionOS](./including-programs.md)
  - [Application Porting](./porting-applications.md)
  - [Porting Case Study](./porting-case-study.md)
  - [Testing](./testing-practices.md)
  - [Redoxer](./ci.md)
  - [Performance](./performance.md)
  - [Tracing](./syscall-debug.md)
  - [Quick Workflow](./quick-workflow.md)

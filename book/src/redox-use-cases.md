# Use Cases

FerrionOS is a general-purpose operating system. This page describes the uses it is aimed at and how far each of them is today. Everything FerrionOS has verified so far was verified in a virtual machine ([Hardware Support](./hardware-support.md)).

## Desktop

The desktop is the use FerrionOS works on first. The x86_64 desktop image boots quietly to a graphical login in QEMU and starts a graphical session on the Orbital display server. It currently installs the COSMIC terminal, file manager and text editor; which desktop apps FerrionOS ships is not settled and may change.

Graphics are drawn by the CPU: there is no hardware-accelerated 2D or 3D yet. Programs can use GUI libraries written in Rust, such as [winit](https://github.com/rust-windowing/winit), [Iced](https://iced.rs/) and [Slint](https://slint-ui.com/).

The goal for the desktop is an everyday experience modeled on macOS and Windows: one consistent desktop and ease of use. Running Windows games through Proton is a goal as well. It is planned and does not work today.

A further objective for the desktop is to develop FerrionOS on FerrionOS itself. [Self-hosted Development](./self-hosted.md) describes how far the inherited tools go; FerrionOS is developed on Linux today.

## Server

The design of FerrionOS suits a server platform: a small kernel, isolated system services, and software written in a memory-safe language. Work remains to be done on important server software, such as databases and web servers, on compatibility with server hardware, and on the hardening described in [Security](./security.md). The `server` configuration builds an image without a graphical interface.

## Infrastructure

The modular architecture suits network infrastructure, such as routers, telecommunication components and edge servers, as more functions are added to these devices. FerrionOS has not been built or tested for such devices.

## Embedded and IoT

For embedded systems with complex user interfaces and broad feature sets, a full-featured operating system with a small kernel is a possible fit. Appliances that are connected to the Internet and carry sensors, microphones and cameras need protection against attacks on the privacy of the people who use them. FerrionOS does not have touchscreen support, video capture, or support for sensors and buttons.

## Mission-Critical Applications

There are no plans for a version of FerrionOS for mission-critical applications, such as satellites or air safety systems. As the tools for correctness proofs of Rust software improve, a version that is proven correct within practical limits may become possible.

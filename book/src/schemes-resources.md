# Schemes and Resources

> 📝 **Note:** Parts of this chapter describe an older scheme interface, in which a daemon creates a scheme by opening `:name` and exchanges `Packet` messages with the kernel, among them the Root Scheme and Providing a Scheme sections of [Scheme Operation](./scheme-operation.md), the code on the [Event Scheme](./event-scheme.md) page, and [An Example](./example.md). The kernel and the daemons that FerrionOS builds use the newer interface that the [Communication](./communication.md) page describes.

An essential design choice of FerrionOS is to refer to resources using scheme-rooted paths. This gives FerrionOS the ability to:

- Treat resources (files, devices, etc.) in a consistent manner
- Provide resource-specific behaviors with a common interface
- Allow management of names and namespaces to provide sandboxing and other security features
- Enable device drivers and other system resource management to communicate with each other using the same mechanisms available to user programs

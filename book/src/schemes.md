# Schemes

The **scheme**, which takes its name from [URI schemes](https://en.wikipedia.org/wiki/List_of_URI_schemes), identifies the type of resource,
and identifies the manager daemon responsible for that resource.

Within FerrionOS, a scheme may be thought of in a few ways. It is all of these things:

- The **type** of a resource, such as "file", "NVMe drive", "TCP connection", etc. (Note that these are not valid scheme names, they are just given by way of example.)
- The starting point for locating the resource, i.e. it is the root of the path to the resource, which the system can then use in establishing a connection to the resource.
- A **uniquely named service** that is provided by some driver or daemon program, with the full path identifying a specific resource accessed via that service.

## Scheme Daemons

A scheme is typically provided by a **daemon**.
A daemon is a program that runs as a process in userspace;
it is typically started by the system during boot.
When the process starts, it registers with kernel using the name of the scheme that it manages.

## Kernel vs. Userspace Schemes

A [userspace scheme](#userspace-schemes) is implemented by a scheme daemon,
described above.
A [kernel scheme](#kernel-schemes) is implemented within the kernel,
and manages critical resources not easily managed with a userspace daemon.
When possible, schemes should be implemented in userspace.

## Accessing Resources

In order to provide "virtual file" behavior, schemes generally implement file-like operations.
However, it is up to the scheme provider to determine what each file-like operation means.
For example, `seek` to an SSD driver scheme might simply add to a file offset, but to a floppy disk controller scheme,
it might cause the physical movement of disk read-write heads.

Typical scheme operations include:

- `open` - Create a **handle** (file descriptor) to a resource provided by the scheme. e.g. `File::create("/scheme/tcp/127.0.0.1/3000")` in a regular program would be converted by the kernel into `open("127.0.0.1/3000")` and sent to the "tcp" scheme provider. The "tcp" scheme provider would parse the name, establish a connection to Internet address "127.0.0.1", port "3000", and return a handle that represents that connection.
- `read` - get some data from the thing represented by the handle, normally consuming that data so the next `read` will return new data.
- `write` - send some data to the thing represented by the handle to be saved, sent or written.
- `seek` - change the logical location where the next `read` or `write` will occur. This may or may not cause some action by the scheme provider.

Schemes may choose to provide other standard operations, such as `mkdir`, but the meaning of the operation is up to the scheme. `mkdir` might create a directory entry, or it might create some type of substructure or container relevant to that particular scheme.

Some schemes implement `fmap`, which creates a memory-mapped area that is shared between the scheme resource and the scheme user. It allows direct memory operations on the resource, rather than reading and writing to a file descriptor. The most common use case for `fmap` is for a device driver to access the physical addresses of a memory-mapped device, using the `memory:` kernel scheme. It is also used for frame buffers in the graphics subsystem.

## Userspace Schemes

FerrionOS creates user-space schemes during initialization, starting various daemon-style programs, each of which can provide one or more schemes.

In the table below, a path in parentheses is the daemon's code inside the `base` source (recipe `recipes/core/base`), which lies in `recipes/core/base/source` after `make f.base`.

| **Scheme** | **Daemon** | **Description** |
|------------|------------|-----------------|
| disk.* | ided, ahcid, nvmed | Storage drivers |
| disk.live | lived | RAM-disk driver that loads the bootable USB data into RAM |
| disk.usb-{id}+{port}-scsi | usbscsid | USB SCSI driver |
| logging | ramfs (`ramfs/`) | Error logging scheme, using an in-memory temporary filesystem |
| initfs | bootstrap (`bootstrap/src/initfs.rs`) | Startup filesystem |
| namespace | bootstrap (`bootstrap/src/initnsmgr.rs`) | Namespace manager |
| proc | bootstrap (`bootstrap/src/procmgr.rs`) | Process manager |
| file | `redoxfs` (recipe `recipes/core/redoxfs`) | Main filesystem |
| virtio-fs.{tag} | virtio-fsd (`drivers/storage/virtio-fsd/`) | [VirtIO-FS](./virtio-fs.md) driver, one scheme for each directory shared by the host |
| network | e1000d, rtl8168d | Link-level network send/receive |
| ip | netstack (`netstack/src/scheme/ip.rs`) | Raw IP packet send/receive |
| tcp | netstack (`netstack/src/scheme/tcp.rs`) | TCP sockets |
| udp | netstack (`netstack/src/scheme/udp.rs`) | UDP sockets |
| icmp | netstack (`netstack/src/scheme/icmp.rs`) | ICMP protocol |
| netcfg | netstack (`netstack/src/scheme/netcfg/`) | Network configuration |
| display.vesa | vesad | VESA driver |
| display.virtio-gpu | virtio-gpud | VirtIO GPU driver |
| orbital | `orbital` (recipe `recipes/gui/orbital`) | Windowing system (window manager and virtual driver) |
| pty | ptyd (`ptyd/`) | Pseudoterminals, used by terminal emulators |
| audiorw | sb16d, ac97d, ihdad | Sound drivers |
| audio | audiod (`audiod/`) | Audio manager and virtual device |
| usb.* | usb*d | USB drivers |
| acpi | acpid | ACPI driver |
| input | inputd | Virtual device |
| sudo | sudo | Privilege manager |
| chan | ipcd (`ipcd/`) | Inter-process communication |
| shm | ipcd (`ipcd/`) | Shared memory manager |
| log | logd (`logd/`) | Logging |
| rand | randd (`randd/`) | Pseudo-random number generator |
| zero | zerod (`zerod/`) | Discard all writes, and always fill read buffers with zeroes |
| null | zerod (`zerod/`), started with the argument `null` | Discard all writes, and read no bytes |

## Kernel Schemes

The kernel provides a small number of schemes in order to support userspace.

The paths in the table below are inside the kernel source (recipe `recipes/core/kernel`), which lies in `recipes/core/kernel/source` after `make f.kernel`.

| **Name** | **Source** | **Description**  |
|----------|------------|------------------|
| user | `src/scheme/user.rs` | Dispatch for user-space schemes |
| debug | `src/scheme/debug.rs` | Debug messages that can't use the `log:` scheme |
| event | `src/scheme/event.rs` | epoll-like file descriptor read/write "ready" events |
| irq | `src/scheme/irq.rs` | Interrupt manager (converts interrupts to messages) |
| pipe | `src/scheme/pipe.rs` | Kernel manager for pipes |
| kernel.proc | `src/scheme/proc.rs` | Process context manager |
| sys | `src/scheme/sys/mod.rs` | System hardware resources information |
| kernel.acpi | `src/scheme/acpi.rs` | Read the CPU configuration (number of cores, etc) |
| memory | `src/scheme/memory.rs` | Physical memory mapping manager |
| time | `src/scheme/time.rs` | Real-time clock timer |
| serio | `src/scheme/serio.rs` | Serial I/O (PS/2) driver (must stay in the kernel due to PS/2 protocol issues) |

## Scheme List

This section has the FerrionOS schemes from the tables above in a list format.

### Userspace

- disk.*
- disk.live
- disk.usb-{id}+{port}-scsi
- logging
- initfs
- namespace
- proc
- file
- virtio-fs.{tag}
- network
- ip
- tcp
- udp
- icmp
- netcfg
- display.vesa
- display.virtio-gpu
- orbital
- pty
- audiorw
- audio
- usb.*
- sudo
- acpi
- input
- chan
- shm
- log
- rand
- zero
- null

### Kernel

- user
- debug
- event
- irq
- pipe
- kernel.proc
- sys
- kernel.acpi
- memory
- time
- serio

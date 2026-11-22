# The kernel drives the IOMMU, and a device's DMA reaches only what its handle grants

Status: accepted

## Context

Item S18 of the security roadmap puts DMA remapping in the kernel. The kernel frees a driver's frames and must cut the device off first; it already allocates DMA buffers and answers their addresses; and it runs before any device gets bus mastering. The IOMMU plan (`plans/iommu.md`, on the maintainer's machine) cuts the work into slices, and took its seven open decisions at their defaults awaiting the maintainer: the kernel holds the mechanism, VT-d comes first, and the work goes ahead now as patch files. Slices 1 and 2 are the first code. Slice 1 turns VT-d translation on with a one-to-one map. Slice 2 adds a DMA handle that confines one device to the memory allocated through it. This record keeps the choices in that code that a reader would otherwise undo, the access they widen, and what slice 3 builds on.

## Decision

### Slice 1: translation on, one-to-one (kernel `ferrion-iommu.patch`)

- The module lives in `src/iommu/` behind the cargo feature `iommu`, which is on by default, so a bisect can switch it off. It runs on the boot CPU after the ACPI tables are read and the other devices are set up, and before the first process. Only x86_64 has a backend; elsewhere the status file says `backend: none`.
- The DMAR table is checked before any field is used: its checksum, at most 64 KiB, at most 256 structures, and every structure's and device scope's length. A malformed table leaves translation off with one error line, never a panic.
- Only the first remapping unit is used, and the log says so when the table lists more. All 256 root entries point at one shared context table, and all its entries at one domain (id 1). One context table for every bus keeps the slice to a handful of pages; a bus gets its own copy when one of its devices gets a handle.
- The one-to-one domain maps every area the frame allocator manages, since every DMA buffer comes from there, at its own address with the largest page the unit offers (1 GiB, 2 MiB, 4 KiB). It never maps the interrupt range 0xFEE00000 to 0xFEFFFFFF. It also maps the firmware's reserved-memory (RMRR) regions in 4 KiB pages. The plan said to log them only, but the VT-d specification asks for an identity mapping of those regions for their devices, and in a domain that already reaches all of RAM the mapping takes nothing away. With more than 32 reserved regions translation stays off.
- Invalidation goes through the invalidation queue only. Register-based invalidation is ignored by units after major version 5, so a unit without the queue, or one that needs write-buffer flushing, is refused with a log line (slice 7 meets them on real hardware).
- Every command, invalidation and wait polls at most 2^26 times. A timeout marks the unit broken: every later operation on it fails, and memory a device might still reach is never freed.
- Table writes are flushed from the CPU cache with `clflush` when the unit does not snoop (extended capability C = 0, as QEMU's model reports).
- A unit the firmware left translating is turned off and programmed again with the kernel's tables.
- `IOMMU=off` in the boot environment leaves translation off, and so does a DMAR table whose flag bit 3 asks the OS not to remap.
- Fault records are read when `/scheme/sys/iommu` is read, logged up to 64 times with the device, the address and the direction, and counted. There is no fault interrupt yet (slice 7).

### Slice 2: the DMA handle (kernel `ferrion-iommu-dma-handle.patch`)

- `openat(memory root, "dma/SSSS-BB-DD.F")` opens the handle of one PCI requester ID: segment, bus, device and function, each exactly 4, 2, 2 and 1 hexadecimal digits. Only the unrestricted root and uid 0 may (EACCES). A second open of the same ID fails with EBUSY while the first handle lives, and at most 64 handles exist (ENFILE). The name lives inside the existing `memory` scheme, so the syscall crate and the scheme list `base` can name stay as they are.
- Opening moves the device out of the one-to-one domain into an empty domain of its own (id 2 plus the handle's slot). Its bus gets a private copy of the shared context table. The device's entry is made not present, invalidated, rewritten and invalidated again, with domain id 0 for the context invalidation under caching mode as the specification asks for a not-present-to-present change. Every other device stays one-to-one.
- `mmap(handle, length)` allocates zeroed, physically contiguous memory exactly as `zeroed?phys_contiguous` does. Inside the address space's lock it takes a second reference on every frame, then maps the frames read-write at their own physical addresses in the device's domain and invalidates under caching mode. Device addresses equal physical addresses for now, and drivers treat them as opaque.
- The mmap offset selects the memory type: 0 write-back, 0x1000 uncacheable, 0x2000 write-combining, 0x3000 device memory, anything else EINVAL. One handle per device thus serves buffers of different types. `MAP_SHARED` is refused, as for `phys_contiguous`. A handle holds at most 256 mappings (ENOSPC) of at most 4 MiB each (EINVAL).
- `call(handle, virtual address)` with metadata `[1]` answers the device address of a page mapped through this handle, and ENOENT for any other page. It replaces the `translation` handle for drivers. `call(handle, [device address, length])` with metadata `[2]`, 16 bytes, removes exactly one mapping from the domain, invalidates and waits, then drops the handle's references.
- `close`, which a dying driver gets for free, puts the device back into the one-to-one domain, invalidates and waits, frees the domain's tables, and only then drops every remaining reference. If an invalidation fails, the frames stay pinned for ever and the status file counts them as `pinned-forever`. Slice 4 will make the entry not present instead of one-to-one.
- The extra reference is what keeps a frame from reuse while the device can reach it. The driver may unmap first or the handle may close first; whichever drops the last reference frees the frames, as the address space's own unmap does. A process's physically contiguous grants are not copied when it forks, so a fork never meets the pinned frames.
- Without a unit (other architectures, `IOMMU=off`, no DMAR table) the handle still opens, allocates and pins, and its status line says `domain none`.
- A refused request is counted against the handle of its requester ID, and its log line says whether it fell outside the device's DMA handle or outside the one-to-one map. `/scheme/sys/iommu` lists one line per handle: `SSSS:BB:DD.F domain N pages P faults F`.
- The kernel's state is one spin lock, held across the bounded waits on the unit. Under QEMU those return at the first poll. Handles are opened and mapped when drivers start, not per request.

### The probe and the checks

- `iommu-probe` is a pcid driver for QEMU's `iommu-testdev` (1b36:0005). `pcid-spawner` starts it from an entry in `/usr/lib/pcid.d/` that only `config/x86_64/iommu-test.toml` installs, so it holds the hardware right drivers get and needs no root shell, which lockdown keeps from raw hardware. It maps BAR0 with one `mmap` on its pcid channel and finds its own address through `/scheme/pci/access`, so it needs nothing from `base`.
- The probe opens its own handle, allocates buffer A through it and buffer B the old way, and has the device write to each. It reads `/scheme/sys/iommu` while the handle is open and prints it, because the device's line goes away with the handle.
- `scripts/vm-test.py iommu` and `dma-outside-grant` judge from QEMU's trace, read on the host, next to what the guest prints. They find each device's address from QEMU's `info pci`, because the order differs between images: the test device is 00:02.0 on the `iommu-test` image.
- `make qemu iommu=test` is `iommu=yes` with `-device iommu-testdev -device edu` after the IOMMU device, which the test device needs before it on the command line.

### Access this widens

- The `iommu-test` image starts `iommu-probe` at boot as root, with the hardware right `pcid-spawner` passes to drivers. It is a test image; no other config installs the entry.
- A process with the unrestricted `memory` root and uid 0 may open the DMA handle of any requester ID, including one another driver is using. That moves the device out of the one-to-one map, so its DMA fails until the handle closes. Such a process could already reach all of RAM through any device. The power stays this wide until slice 3 has `pcid-spawner` open every handle and S13 takes the unrestricted root from drivers.
- `/scheme/sys/iommu` is readable by every process whose namespace holds `sys`: it shows the unit's registers, the fault counts and which devices hold handles. Fault addresses go to the kernel log only.

### What slice 3 needs

- `pcid-spawner` opens `dma/SSSS-BB-DD.F` for each function just before it spawns the driver and passes the descriptor beside `PCID_CLIENT_CHANNEL`. The kernel accepts the open from it today: it runs with the hardware right and uid 0.
- `common`'s `Dma` and `Sgl` map the handle with `MAP_PRIVATE`, choosing the memory type by the offset (0x1000 where they ask for `@uc` today). They ask for device addresses with metadata `[1]`, release with `[2]` and the exact address and length of each mapping, and unmap their own pages.
- The bounds are 4 MiB per mapping, which `Sgl`'s 4 MiB chunks meet, and 256 mappings per handle. `virtio-netd`'s per-packet buffers must come from the pool `virtio-core` has before then, and a driver that needs more than 256 live buffers needs the bound raised here first.
- The probe stops opening its own handle and takes the spawner's, because a second open of its requester ID fails with EBUSY.

## Consequences

- With `make qemu iommu=yes` every device's DMA goes through the unit and is still allowed everywhere it was before. Only a function with a DMA handle is confined, and only in QEMU, until slice 4 removes the one-to-one domain.
- The time to the graphical login did not move: over five boots each, the median was between 2.8 and 3.0 s with and without `iommu=yes`, before and after both slices.
- Removing the reserved-region mapping would cut those devices off from memory the firmware gave them on real machines (USB keyboard emulation, graphics memory taken from RAM). QEMU has no such regions, so no check here would notice.
- Relaxing the order of close (dropping references before the invalidation completes) would let a frame be reused while a device can still write it, which is the case the handle exists to prevent.

## Where

- `recipes/core/kernel/ferrion-iommu.patch` (new `src/iommu/mod.rs`, `dmar.rs`, `vtd.rs`; hooks in `Cargo.toml`, `src/main.rs`, `src/arch/x86_shared/start.rs`, `src/scheme/sys/mod.rs`)
- `recipes/core/kernel/ferrion-iommu-dma-handle.patch` (new `src/iommu/handle.rs`; `src/iommu/mod.rs`, `vtd.rs`, `src/scheme/memory.rs`)
- `recipes/tests/iommu-probe/`, `config/x86_64/iommu-test.toml`, `mk/qemu.mk` (`iommu=test`)
- `scripts/vm-test.py`, `test_iommu` and `test_dma_outside_grant`

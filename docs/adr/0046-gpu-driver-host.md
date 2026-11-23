# GPUs on real hardware are driven by Linux's drivers in a driver host process

Status: accepted

## Context

FerrionOS draws on the CPU on real hardware: its only accelerated path is virtio-gpu with Venus in a virtual machine. A current GPU's driver is about a million lines that hold the vendor's knowledge of its own chips and firmware; Linux's AMD driver with the DRM code under it is 945 C files and 1,359,402 lines at Linux 6.18, without its register headers. Two systems that are not Linux run that code today: FreeBSD through its LinuxKPI layer and OpenBSD through a smaller layer of its own. Both recompile Linux's files inside their kernels and change them to fit (443 and 551 conditionals for the port in the AMD driver stack).

FerrionOS's drivers are processes. The maintainer decided on 2026-11-22 that GPUs on real hardware use Linux's drivers through a driver host process, AMD first for the reference laptop, NVIDIA through `nouveau` with NVK and later Nova. An inventory taken that day (its counts are reproduced by `scripts/linux-driver-l0/run.sh`) measured the cost: at Linux 6.18 the AMD stack calls 1,303 kernel functions and function-like macros from outside itself, FreeBSD's layer has 1,080 of them by name, 45 more are C-library names, and 178 are missing, most of which both ports cut from the driver. `nouveau` shares 955 of its 1,058 and adds 103.

## Decision

- **A driver host is one process per GPU function**, started by `pcid-spawner` like any PCI driver. It links unchanged Linux driver source against a shim that provides the kernel interfaces the source calls, and offers the device as a FerrionOS scheme under a display name and a render name, with the request formats the virtio-gpu driver already serves (ADRs 0028 to 0032). Programs, Mesa and the compositor do not know which kind of driver is behind a scheme.
- **The host is GPL-2.0-only and lives in its own repository and package:** the `driver-host/` folder of `ferrion-hardware`, the repository that holds every FerrionOS driver and the per-machine hardware profiles. On the maintainer's decision of 2026-11-22 that whole repository is GPL-2.0-only, the license of Linux's drivers, so that no FerrionOS driver can be made proprietary: the shim, the front end and the back ends are GPL-2.0-only too, and code taken from Linux (whose drivers here are mostly MIT and whose DRM core and dma-buf are partly GPL-only) and from the BSD layers keeps its notice. FerrionOS's components in its own repository stay MIT and talk to the host only through schemes.
- **It tracks one Linux long-term branch for every driver it contains: 6.18.y**, pinned by release and hash and moved to the newest point release once a month. 6.12 would serve the AMD chip, but `nouveau` drives the NVIDIA bench card only from 6.18, and one DRM core serves both.
- **Linux's files are compiled as they are.** A changed line is recorded with its reason. FreeBSD's LinuxKPI and OpenBSD's layer are the references the shim is written from, group by group; neither is compiled, because each is written on its own kernel's primitives.
- **The Linux code of a host runs under one lock**, as on a single CPU without preemption: a thread gives the lock up only where Linux code may sleep. Spin locks, interrupt masking and RCU then mean what the driver assumes, at the price of one CPU per GPU's driver.
- **A scheme handle is a Linux `struct file`.** The front end calls the DRM core's file operations; user copies read and write the request's message and nothing else; a file descriptor Linux code creates reaches the client as a token that relibc turns into a descriptor. dma-buf and sync-file descriptors are therefore served by Linux's own code behind FerrionOS's descriptors.
- **Every byte the device can reach comes from the host's DMA handle** (ADR 0040), and a buffer a client has mapped is not moved: a scheme lends pages when a mapping is made and cannot take them back.
- **What does not exist on FerrionOS is cut, by name:** user pointers, suspend and run-time power-down, sysfs and debugfs, sound over HDMI and DisplayPort, the driver's own console, and sub-devices behind the GPU's interrupt. An interface the shim does not provide ends the host with its name; nothing missing answers success.
- **Firmware is not part of the host.** AMD's files are redistributable in binary form and ship as an optional package; NVIDIA's are installed by the user.
- **Entry conditions.** A host runs on real hardware only behind an IOMMU that confines its device in strict mode and with a grant for that one device (S13, S18). In a virtual machine, Linux's virtio-gpu driver runs in the host at any time, as the test that compares it with the native driver and with a Linux guest.

## Consequences

- FerrionOS gains hardware acceleration on the reference laptop without writing a GPU driver, and follows Linux's fixes by moving a pin.
- It carries a GPL-2.0 component, about fifty thousand lines of shim by the size of FreeBSD's (a figure to be replaced by the first measured one), and a monthly update whose gate is the differential check in the VM and the checks on the laptop.
- One host uses one CPU for its driver's code. If the measured flip and submission times under load miss their targets, this is the first choice to revisit.
- Buffers that programs have mapped cannot be evicted or moved, so memory pressure ends in an out-of-memory answer where Linux would migrate.
- The laptop loses, against Linux on the same chip: suspend, the battery power profile, sound over the display outputs, hardware video decode until its user-space side is ported, and every tuning file.
- Until the IOMMU and the grant have passed their probes on the machine, a host is as trusted as the kernel and is described as "a separate process; a crash stays in its process".
- A second GPU host on one machine needs a service that owns shared buffers and fences outside both; none is built until such a machine exists.

## Where

- The host: `driver-host/` in github.com/CallMeOuta/ferrion-hardware, built here by a recipe pinned by commit. The counting scripts: `scripts/linux-driver-l0/` (`fetch.sh` downloads the pinned sources and checks their hashes, `run.sh` runs `count.py` over them).
- relibc's `redox-ioctl` (the `amdgpu` and `nouveau` modules, the table chosen by driver name) and the layout checks under `scripts/`.
- `scripts/vm-test.py`: `lxhost-link`, `lxhost-load`, and the GPU checks run against the host.

# The GPU reference guest runs pinned Linux packages only inside QEMU

Status: accepted

## Context

`scripts/gpu-reference-guest.py` builds and boots an offline Linux guest that renders a fixed scene through virgl, a CPU renderer and Zink on Venus, as the host-side reference for FerrionOS's GPU work ([`docs/testing.md`](../testing.md) has how to run it). Its inputs are downloaded Fedora packages, which are untrusted data on the host.

## Decision

### Isolation

- Downloads are data on the host: every downloaded program runs inside QEMU, never on the host. No RPM scriptlet runs.
- Each pinned package was authenticated once through its RPM RSA header signature, checked against the release keys whose fingerprints Fedora publishes, and the signed header's SHA-256 of the compressed payload; the script carries the resulting hashes and checks them on every run. The archive paths name exact builds, not a changing mirror. The alternate variant adds rawhide's Mesa and the glibc runtime it requires to the release set.
- The initramfs drops the packages' privilege bits: the one-shot root init needs none.

### Building the initramfs

- RPM's cpio can carry a hard-link group's data on its last member, and a tar conversion turns that member into a link without data, so the group is read from the original archive.
- RPMs can still name `/lib64` where Fedora aliases it into `/usr`; the kernel's cpio unpacker does not follow those parent links, so every file is written at its real path, and directory records come before their children, the aliases included.
- A static PID 1 stays in place in case the dynamic loader cannot start Python, so the loader's diagnostic reaches the serial log instead of being lost in a panic.

### The guest program

- Every graphics probe must report the renderer that was asked for: the scene's and both of the version probe's.
- Native contexts are a recorded experiment, outside the Venus baseline.
- Before powering off, the guest keeps the earlier kernel log in its record and drains the serial port, so kernel shutdown messages do not interleave with a long record.
- `scene.c` gives the triangle's corners in eighth-pixel units, where multiples of 2 are quarter pixels.

## Consequences

Running a downloaded program on the host, or trusting a package by name instead of by hash, turns the reference into code execution on the build machine.

## Where

- `scripts/gpu-reference-guest.py`: the package pins and release-key fingerprints, the initramfs writer, the init wrapper
- `scripts/gpu-reference/guest.py`, `scripts/gpu-reference/scene.c`

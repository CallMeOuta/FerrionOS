# RedoxFS

The default copy-on-write filesystem of FerrionOS (`components/redoxfs`), inspired by [ZFS](https://docs.freebsd.org/en/books/handbook/zfs/) and adapted to a microkernel architecture. Derived from the upstream project at the revision recorded in [`components/upstream.toml`](../upstream.toml), under the license in [`LICENSE`](LICENSE).

## Features

- Compatible with FerrionOS and Linux (FUSE)
- Copy-on-write
- Data and metadata checksums
- Transparent encryption supported by the FerrionOS bootloader so the kernel can boot from an encrypted partition
- Standard Unix file attributes
- File and directory size limit up to 193 TiB
- File and directory quantity limit up to 4 billion per 193 TiB (`2^32 - 1 = 4294967295`)
- MIT licensed

## Building and checking in FerrionOS

Read [`agents/skills/component-sources.md`](../../agents/skills/component-sources.md) before modifying this tree.

Cook RedoxFS from the repository root:

```sh
CI=1 COOKBOOK_LOGS=true make r.redoxfs
```

Rebuild the host filesystem tools (`build/fstools/bin/`) with `CI=1 COOKBOOK_LOGS=true make fstools`. After changing on-disk layout, encryption, or mount behavior, cook its dependents (`CI=1 COOKBOOK_LOGS=true make r.redoxfs,bootloader,base,initfs`) and rebuild the image.

Verify changes with deterministic simulation and the VM checks in [`scripts/vm-test.py`](../../scripts/vm-test.py) (see [`docs/testing.md`](../../docs/testing.md)):

- `sim/redoxfs/run.sh 1000 10000` (and `sim/redoxfs/run.sh maps 1000 10000` for shared mappings; `--flush-on-unmount` for host image-build mode)
- `boot`, `boot-gc-generation`, `mkfs-reuse`, `disk-keys`, `one-password`, `key-slot-roll`, `file-locks`, `directory-fsync`, `shm-reuse`, and `shm-read-back`

For contribution policy, see [`CONTRIBUTING.md`](../../CONTRIBUTING.md) and [`docs/code-policy.md`](../../docs/code-policy.md).

## Tooling

- `redoxfs` - Mount a RedoxFS image or partition
- `redoxfs-ar` - Write files from a directory into a RedoxFS disk image
- `redoxfs-clone` - Clone a RedoxFS disk image
- `redoxfs-key` - Manage RedoxFS encryption key slots
- `redoxfs-mkfs` - Create an empty RedoxFS disk image
- `redoxfs-resize` - Resize a RedoxFS disk image

## Rootless block device access on Linux

Accessing block devices on Linux normally requires root permissions. To run RedoxFS tools against a block device without root, grant read-write ACL permission to the intended user:

```sh
sudo setfacl -m u:<user>:rw /path/to/disk
```

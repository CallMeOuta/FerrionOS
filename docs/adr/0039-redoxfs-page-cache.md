# RedoxFS serves every shared file mapping from one page cache per node

Status: accepted

## Context

A shared mapping of a file on RedoxFS is made of pages RedoxFS owns: for `mmap`, the kernel asks the file system for an address in its own memory (`MmapPrep`) and lends the pages there to the client, which then reads and writes them directly. RedoxFS stores files on disk, written through transactions. The bytes a mapping shows, the bytes `read` returns and the size `fstat` reports have to agree, as they do on Linux, and they have to agree whether the change came through `write`, `ftruncate`, `O_TRUNC` or another shared mapping. Wayland clients, SQLite in WAL mode and anything else that maps a file depends on that.

## Decision

- RedoxFS keeps one page cache per node, `PageCache` in `src/page_cache.rs` (`ferrion-map-coherence.patch`). It holds the pages of every mapped range of an open node, as one block of anonymous memory per node that `mremap` grows to the highest mapped end. A range read into the cache is read from the disk once, when it is first mapped, with zeros after the end of the file.
- The cache's pages are updated in place, never replaced: the kernel has lent them to clients, so a new copy would not reach a live mapping. `write` copies into every cached page it overlaps. A size change zeroes the cached bytes between the old and the new end.
- A cache update is applied only after its transaction has committed. `PageCache::write` and `PageCache::truncate` return a `CacheUpdate` from inside `FileSystem::tx`, and the scheme passes it to `PageCache::apply` once `tx` returns. A failed commit therefore leaves the cache matching the disk.
- Ranges a writable shared mapping covers are marked dirty. For those ranges the cache holds the newest bytes, so `read` takes them from the cache. Every other range is read from the disk.
- `munmap` and `fsync` write the dirty ranges back up to the file's size and no further, so unmapping never changes the size.
- A node's pages are unmapped and dropped when its last handle and its last mapping are gone. The cache holds exactly the nodes that are open or mapped, and a node is never kept for a later `mmap`.
- Bounds per node: mappings end at most `MAX_MAPPED_BYTES` (64 GiB) into the file and number at most `MAX_MAPPED_RANGES` (4096) ranges, refused with `ENOMEM`; at most `MAX_NODE_HANDLES` (2^20) handles, refused with `EMFILE`. A map or unmap of no bytes, or at an offset that is not a page boundary, answers `EINVAL`.
- The cache reaches memory through the `PageMemory` trait: `mmap` and `mremap` on FerrionOS, and a heap buffer in the simulation that refuses to move.

## Consequences

- A file mapped again after its last close is read from the disk again: a cache that outlived its users would have to be invalidated. Keeping pages across closes, for speed, would be a new decision with its own check.
- `write` on a mapped file costs one extra copy per mapped byte it overlaps, and `read` one extra copy per byte of a dirty range.
- When the commit that `munmap` or `fsync` starts fails, the dirty ranges stay marked in the cache while the node is open or mapped, and a later `fsync` writes them. Once the node is dropped they are lost, as data written through a mapping is lost on Linux when writeback fails.
- `PageCache` builds for every target, so `sim/redoxfs/run.sh maps` drives the same code as the mount on FerrionOS. Only `MappedPages` and the scheme glue are FerrionOS-only.

## Where

- `recipes/core/redoxfs/ferrion-map-coherence.patch`: `src/page_cache.rs`, `src/mount/redox/resource.rs` (`MappedPages`, `FileResource`), `src/mount/redox/scheme.rs` (`open`, `write`, `ftruncate`, `on_close`)
- `sim/redoxfs/src/maps.rs` (`run.sh maps`), with write errors during commits from `SimDisk::arm_write_error` in `sim/redoxfs/src/disk.rs`
- `recipes/tests/shm-reuse-probe`, `scripts/vm-test.py` `test_shm_reuse` and `test_shm_read_back`

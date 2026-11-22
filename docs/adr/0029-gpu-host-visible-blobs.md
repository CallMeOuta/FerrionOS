# Host-visible GPU blobs, the shared-memory window and render-handle mmap offsets

Status: accepted

## Context

Slice GP4 of the GPU work lets Mesa's Venus driver share memory with the host: its instance ring, its replies and every host-visible Vulkan allocation are host blobs (`VIRTGPU_BLOB_MEM_HOST3D`) that the guest maps. virtio-gpu exposes them through a shared-memory region, the host-visible window, which QEMU places in a 64-bit BAR. GP3 left that window unmapped ([0028](0028-gpu-render-handles.md)). The calls are in [`docs/gpu-needs-ledger.md`](../gpu-needs-ledger.md) and the mapping rules in [`docs/gpu-conventions.md`](../gpu-conventions.md).

## Decision

### virtio-core and the window

- virtio-core reads the device's shared-memory capabilities (`VIRTIO_PCI_CAP_SHARED_MEMORY_CFG`, with its 64-bit offset and length) and gives virtio-gpud the region with id 1, the host-visible window, as a `SharedMemoryRegion` (BAR, offset, physical address, length). `pcid` does not map that BAR, and virtio-gpud never maps the whole window: QEMU's default window is 4 GiB, and only the ranges of mapped blobs are mapped, each with the cache mode the device reports for it (`map_info`: 1 cached, 2 uncached, 3 write-combined).
- Window ranges come from a page-aligned first-fit allocator that coalesces freed ranges. A range is freed only after the device answers `RESOURCE_UNMAP_BLOB` with success, so a range the host may still use is never handed out again.

### The render handle's requests

- `RESOURCE_CREATE_BLOB` accepts only `HOST3D` blobs, `MAPPABLE` and `SHAREABLE` flags, the `DEFER_MAPPING` hint, a nonzero size it rounds up to whole pages (Venus's 131,268-byte ring becomes 33 pages, as Linux rounds), at most 1 TiB, a command of at most 64 KiB, and at most 4096 blobs per handle until GP12's quotas. A nonempty command goes to the device in a `SUBMIT_3D` before the `RESOURCE_CREATE_BLOB` that names its result, as Linux sends it.
- `GUEST` and `HOST3D_GUEST` blobs get `EINVAL`: Venus asks for neither.
- `VIRTGPU_PARAM_HOST_VISIBLE` answers 1 only when the device offers blobs and the window exists, which is what lets Venus start.
- `MAP` answers an offset of the blob's handle-local slot shifted left by 40, so the slot has 23 bits and bit 63 stays clear (a negative `off_t` would be refused by C code), and the 40 bits below are the offset inside the blob. `mmap` refuses an offset with bit 63, an unaligned offset, a zero length, an unknown slot, a blob not created mappable and a range past the blob's end.
- The first `mmap` of a blob sends `RESOURCE_MAP_BLOB`, maps the window range with the device's cache mode and counts mapped pages; the last `munmap` sends `RESOURCE_UNMAP_BLOB`. `GEM_CLOSE` removes the handle's number at once but destroys the blob only when its last mapping goes, as Linux keeps a mapped buffer alive.
- Closing the render handle destroys its context first, then every blob it still holds.

### The integration

- A blob's GEM handle is known to submissions ([0030](0030-gpu-submission-and-fences.md)) from its creation to its `GEM_CLOSE`: `EXECBUFFER` accepts it in `bo_handles`, `WAIT` waits for the last submission that named it, and after `GEM_CLOSE` both answer `ENOENT` (base `ferrion-gpu-blob-submit.patch`).
- Blob and context commands wait for the device on virtio-gpud's control queue, the one submissions use. Before such a command, any submission still waiting for room on the ring is placed first, so the host sees commands in the order clients made them (base `ferrion-gpu-queue-order.patch`). virtio-core's interrupt thread harvests completions, so a command that waits never depends on the driver's own event loop and cannot deadlock with submissions in flight.

## Consequences

- QEMU 11.1.3 on the build host maps blobs through virglrenderer's `MAP_FIXED` path, which emits no `virtio_gpu_cmd_res_map_blob` trace event; `gpu-blob` then accepts the driver's own first-map log line together with a check that two host blobs stay distinct across a reused window range.
- A window smaller than 4 GiB aborts QEMU today because `pcid` sizes 64-bit BARs without turning off memory decoding (`.scratch/gpu/issues/02-pcid-bar-sizing.md`).
- Blobs count against no quota but the per-handle cap until GP12 (`.scratch/gpu/issues/03-blob-quotas.md`).
- Scanout from a blob (`SET_SCANOUT_BLOB`) is not served: it needs PRIME to bring a render-handle blob to the display handle, the rest of GP6 and GP7.

## Where

- `recipes/core/base/ferrion-gpu-blob.patch` (virtio-core's shared-memory capability, `driver-graphics/src/blob.rs`, `virtio-gpud/src/blob.rs`), `ferrion-gpu-blob-submit.patch`, `ferrion-gpu-queue-order.patch`
- `recipes/core/relibc/ferrion-gpu-blob.patch` (map, resource info and blob create in the DRM translation)
- `recipes/tests/gpu-probe-blob/`, `scripts/vm-test.py` `test_gpu_blob`

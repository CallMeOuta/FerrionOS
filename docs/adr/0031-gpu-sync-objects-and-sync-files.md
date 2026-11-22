# DRM sync objects, timelines and sync files on render handles

Status: accepted

## Context

Slice GP7 gives the render handle Linux's explicit synchronization: DRM sync objects, binary and timeline, and sync files. Venus uses timeline sync objects once `DRM_CAP_SYNCOBJ_TIMELINE` answers 1, naming them as out sync objects of its submissions; without them it falls back to sync objects it simulates over sync files. Mesa's common Vulkan runtime and a future compositor's explicit sync use sync files. The calls are in [`docs/gpu-needs-ledger.md`](../gpu-needs-ledger.md); submissions are [0030](0030-gpu-submission-and-fences.md)'s.

## Decision

### The objects

- A fence is a set of leaves (`FenceLeaf`), each signaled once, with a status and a `CLOCK_MONOTONIC` timestamp; signaling a leaf wakes the driver's event loop, which then answers the waits and polls it unblocks. A merged sync file holds the leaves of both, at most 64, and is signaled when all are.
- Sync objects live in a table per render handle, at most 4096 per handle and 1024 pending timeline points per object, so another handle's numbers mean nothing (`ENOENT`). A sync object crosses processes as a descriptor (`HANDLE_TO_FD` without flags, `FD_TO_HANDLE`), the only way Linux lets one cross.
- `DRM_CAP_SYNCOBJ` and `DRM_CAP_SYNCOBJ_TIMELINE` answer 1 on a render handle, `DRM_CAP_PRIME` import and export, `DRM_CAP_TIMESTAMP_MONOTONIC` 1, and a render handle now answers `GET_CAP` instead of refusing it with `EACCES`.

### Requests

- Waits never block the driver: a wait that cannot be answered stays pending (at most 64 per handle) and is answered when a leaf signals or its deadline passes, with `ETIME`. relibc turns Linux's absolute `CLOCK_MONOTONIC` deadlines into the time remaining before the request leaves the process, so no clock value crosses processes, and gives the caller's fields back as they were.
- Sync files answer `poll` (readable once signaled, and from then on), `SYNC_IOC_MERGE` (a two-descriptor call that leaves the merged fence under a token, then a `dup` that makes it a descriptor), `SYNC_IOC_FILE_INFO` (Linux's count-first protocol; a too-small array gets `EINVAL`) and `SYNC_IOC_SET_DEADLINE` (accepted, nothing to do).
- `DMA_BUF_IOCTL_SYNC` is accepted and does nothing, because every mapping is coherent; the type-`b` sync-file export and import answer `ENOTTY` for good, so Venus stops asking, and relibc answers `ENOTTY` for any ioctl type it does not know.
- `DRM_IOCTL_SYNCOBJ_EVENTFD` checks its arguments and answers `EOPNOTSUPP`: the eventfd is another scheme's descriptor, which a call to this scheme cannot carry. A consumer exports a sync file and polls it (`.scratch/gpu/issues/01-syncobj-eventfd.md`).

### Exporting a sync file before anything is submitted

Linux answers `EINVAL` when a binary sync object holds no fence or a timeline point has none yet. FerrionOS instead gives a sync file that signals when that sync object or point is later signaled, from the CPU or by a submission. Nothing in Mesa relies on this: Venus exports a sync file only after a submission named the sync object (`vn_create_sync_file`), and Zink goes through Venus. Mesa's common runtime relies on the opposite: `vk_drm_syncobj`'s `spin_wait_for_sync_file` takes a successful export as proof that a fence was submitted, for drivers whose kernel lacks `DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE`; FerrionOS answers that flag, and no FerrionOS driver uses `vk_drm_syncobj` today. The looser answer is kept because it is the only way to make an unsignaled sync file without the GPU, which the checks of merge, poll timeouts and `FENCE_FD_IN` need; Linux's tests use `sw_sync` for the same purpose. It must be revisited before a driver that uses `vk_drm_syncobj` (a native context, GP13) runs on FerrionOS, and the maintainer decides whether to match Linux then (`.scratch/gpu/issues/04-syncobj-export-before-submit.md`).

## Consequences

- Submission fences are sync files and become sync-object points, so Venus takes its timeline path and Zink on Venus gets real fences.
- The driver keeps per-handle state for every sync object and pending wait; the caps above bound it until GP12's quotas per grant.

## Where

- `recipes/core/base/ferrion-gpu-syncobj.patch` (`driver-graphics/src/syncobj.rs`), `ferrion-gpu-fences.patch`
- `recipes/core/relibc/ferrion-gpu-syncobj.patch` (`redox-ioctl/src/sync.rs`, `src/header/sys_ioctl/redox/sync.rs`), `ferrion-gpu-fences.patch`
- `recipes/tests/gpu-probe-syncobj/`, `scripts/vm-test.py` `test_gpu_syncobj`

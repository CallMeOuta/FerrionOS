# GPU command submission, its fences, and waiting for them

Status: accepted

## Context

Slice GP4 makes the render handle submit work: Venus sends its command streams with `DRM_IOCTL_VIRTGPU_EXECBUFFER` on a ring of its context, and learns that the host finished through a fence, as a sync file or as a point on a sync object. GP3's driver waited for the device with no timeout ([0028](0028-gpu-render-handles.md)); submissions can take far longer than a context command, so they must not block the driver. The sync objects and sync files are [0031](0031-gpu-sync-objects-and-sync-files.md)'s.

## Decision

### The driver

- virtio-gpud sends `SUBMIT_3D` with `VIRTIO_GPU_FLAG_FENCE` and, when the client named a ring, `VIRTIO_GPU_FLAG_INFO_RING_IDX`, the command after the 32-byte header in one buffer. It does not wait: the request's future is kept and polled when the queue wakes the driver's event loop, and the fence is a gp7 `FenceLeaf` that the driver signals when the device answers, which QEMU does only once the host has retired the fence. A device error signals it with `-ENOMEM` or `-EIO`.
- At most 256 submissions are in flight on a device (`EBUSY` beyond), 64 fences waiting to become descriptors on a handle (`ENOMEM`), 4 MiB of command, 1024 buffers and 1024 sync objects in each direction per submission (`EINVAL`).
- A submission that finds the control queue's ring full waits in the driver behind any earlier one, and every command the driver sends and waits for (contexts, blobs) first places those that wait, so the host sees the control queue in request order. Venus relies on that order: a blob's creation names the result of commands it submitted before. virtio-core's interrupt thread harvests completions, so waiting on the ring never depends on the driver's event loop.
- `EXECBUFFER` checks everything before the device sees anything: the context, flags, ring, stride (16, read only when a sync-object count is nonzero, as Linux reads it), every sync object and its flags, every buffer handle (`ENOENT` for one the handle does not hold) and the fence quota. The submission's fence then becomes the fence of every buffer named, which `WAIT` waits for, and of every out sync object at its point.
- In sync objects and a `FENCE_FD_IN` sync file are waited for before the submission reaches the device. Linux blocks the caller there; FerrionOS keeps the request pending in the driver, which goes on serving other requests, and runs it again from the start once every fence it waits for has signaled. A binary sync object or a timeline point that holds no fence gets `EINVAL`, as in Linux, and `VIRTGPU_EXECBUF_SYNCOBJ_RESET` resets an in sync object after the submission.
- `FENCE_FD_OUT` leaves the fence in the handle under a number that relibc turns into a descriptor with a `dup` on the render handle; that descriptor is a sync file, so poll, merge and file info work on it as on any other.
- `WAIT` with `NOWAIT` answers `EBUSY` while the buffer's last fence is pending, and otherwise blocks the request until it signals.

### relibc

- relibc checks counts, sizes and null pointers before the call (`EINVAL`, `EFAULT`), sends the sync-object arrays and buffer list before the command, aligned, and does the `dup` for `FENCE_FD_OUT`, setting close-on-exec. For `FENCE_FD_IN` it names the sync file as the call's second descriptor, so the driver receives the sync file itself and no descriptor number crosses processes; a descriptor of another scheme gets `EINVAL`.

### What the integration removed

- gp4-submit's own fence descriptor type, with its 16-byte state read, and `FENCE_DUP_UNSIGNALED`, a test-only command that made a fence no submission would signal. The checks that used it now use a sync file exported from an unsignaled sync object, which [0031](0031-gpu-sync-objects-and-sync-files.md) keeps possible.

## Consequences

- A client can keep at most 256 submissions in flight per device, shared by every handle, until GP12's quotas per grant.
- A request waiting for an in fence that never signals stays pending until the client cancels it or closes the handle; it holds no device resource.
- The device's fence numbers are not shown to clients: Linux's sync files do not show them either. `gpu-submit` checks that every submission has its `virtio_gpu_fence_resp` in QEMU's trace instead.

## Where

- `recipes/core/base/ferrion-gpu-submit.patch` (`driver-graphics/src/submit.rs`, `virtio-gpud/src/submit.rs`), `ferrion-gpu-fences.patch`, `ferrion-gpu-blob-submit.patch`, `ferrion-gpu-queue-order.patch`
- `recipes/core/relibc/ferrion-gpu-submit.patch`, `ferrion-gpu-fences.patch`
- `recipes/tests/gpu-probe-submit/`, `scripts/vm-test.py` `test_gpu_submit`

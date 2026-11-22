# DRM page flips, flip completion events and vertical-blank events on display handles

Status: accepted

## Context

Slice GP6 puts a rendered buffer on the screen the way Linux compositors do it: `DRM_IOCTL_MODE_PAGE_FLIP` with an event that says when the new framebuffer is shown, and `DRM_IOCTL_WAIT_VBLANK` to pace frames. Until then a display handle answered neither, and a virtual terminal's compositor could only set a plane and copy damage. virtio-gpu has no hardware vertical blank.

## Decision

- A display handle serves `MODE_PAGE_FLIP` (`0xB0`, 24 bytes) with `DRM_MODE_PAGE_FLIP_EVENT` and `DRM_MODE_PAGE_FLIP_ASYNC`, `WAIT_VBLANK` (`0x3A`, 24 bytes) with absolute and relative sequences, `_DRM_VBLANK_EVENT`, `_DRM_VBLANK_NEXTONMISS` and the high-CRTC bits, and `GET_CAP(DRM_CAP_CRTC_IN_VBLANK_EVENT)` answers 1. Unknown flags, a nonzero reserved field and `_DRM_VBLANK_SIGNAL` get `EINVAL`, an unknown CRTC or framebuffer `ENOENT`, a second flip while one is in flight on the CRTC `EBUSY`, a full flip queue (16) `EWOULDBLOCK` and a full event queue (1024) `ENOMEM`.
- An adapter that cannot flip refuses with `EINVAL` (vesad and ihdgd today). virtio-gpud completes a flip on its asynchronous pending-frame queue: transfer, set scanout, flush, and the flip's event when the flush is answered.
- Events are Linux's 32-byte `drm_event_vblank` records, read whole from the display handle; `poll` is readable while one is queued; a nonblocking read of an empty queue answers `EAGAIN` and a blocking one waits without blocking the driver. inputd now passes `O_NONBLOCK` through when it opens the display.
- Vertical blanks are synthesized: the timestamp is `CLOCK_MONOTONIC` when the flush completes, and the sequence counts at the mode's refresh rate (60 Hz when the mode gives none), strictly increasing per flip. `WAIT_VBLANK` answers at once with the current sequence and time, and queues an event when asked, rather than sleeping inside the single-threaded display driver.
- A render handle still refuses both requests with `EACCES`, as a Linux render node does.
- The scanout source is an enum in `virtio-gpud/src/flip.rs` with one variant, the 2D resource of a dumb buffer; scanout from a blob (`SET_SCANOUT_BLOB`) adds a variant there without changing the requests, the events or relibc.

## Consequences

- On QEMU with the 3D device (`gpu=venus`) a flip takes about ten times as long as on `gpu=virtio` (median 37 ms against 4 ms on the lane's host), the cost of the 3D device's flush path; it is measured again on the integrated image.
- Orbital keeps its damage-copy presentation and still ignores flip and vblank events.
- Showing a buffer rendered on a render handle needs that buffer on the display handle, which waits for PRIME (the rest of GP7) and then the blob scanout variant above.

## Where

- `recipes/core/base/ferrion-drm-page-flip.patch` (`driver-graphics/src/flip.rs`, `virtio-gpud/src/flip.rs`, inputd)
- `recipes/core/relibc/ferrion-drm-page-flip.patch` (the `WAIT_VBLANK` payload and translation)
- `recipes/tests/gpu-probe-flip/`, `scripts/vm-test.py` `test_gpu_flip`, and the PPM header fix in `Frame.from_ppm`

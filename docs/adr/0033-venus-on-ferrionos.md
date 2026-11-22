# Mesa's Venus Vulkan driver on FerrionOS

Status: accepted

## Context

Venus is Mesa's Vulkan driver for virtio-gpu: it serializes Vulkan calls into a ring in a host blob and the host's virglrenderer replays them on the host's GPU. Slice GP4 builds it for FerrionOS on the render handle of [0028](0028-gpu-render-handles.md), the blobs of [0029](0029-gpu-host-visible-blobs.md), the submissions of [0030](0030-gpu-submission-and-fences.md) and the sync objects of [0031](0031-gpu-sync-objects-and-sync-files.md). Mesa 26.3.1 is the recipe's release since GP2.

## Decision

- Mesa's recipe builds `-Dvulkan-drivers=swrast,virtio` and installs `libvulkan_virtio.so` with its ICD file `virtio_icd.<arch>.json` beside lavapipe's. One cook builds Venus and Zink ([0034](0034-zink-selection.md)).
- `venus-protocol` is a recipe of its own, pinned at tag v1.1.3's commit, and a build-only dependency of Mesa: Mesa 26.3.1 asks for exactly that version, and the cookbook's meson runs with `-Dwrap_mode=nofallback`, so Mesa's own wrap cannot download it. It ships nothing into an image.
- Device discovery (`ferrion-venus.patch`, `vn_renderer_virtgpu.c` under `__redox__`) opens the render scheme directly instead of enumerating `/dev/dri` through udev and sysfs, which FerrionOS has neither of: `FERRION_GPU_RENDER_FD` names a descriptor the program was given (duplicated close-on-exec; an invalid one fails without trying anything else), `FERRION_GPU_RENDER_PATH` a path to open, and otherwise `/scheme/gpu.virtio-gpu` and then every `gpu.*` name in `/scheme`. Every candidate must still pass Venus's own checks: `virtio_gpu` with major version 0 and a nonzero `VIRTGPU_PARAM_3D_FEATURES`. The device is reported on the platform bus, with no PCI bus information, because the render handle refuses `GET_UNIQUE` and FerrionOS has no PCI enumeration in libdrm yet.
- relibc does not export `gettid()`, so `vn_gettid` on FerrionOS is a process-local counter in thread-local storage, which is all Venus uses it for: telling its own threads apart (`.scratch/gpu/issues/05-relibc-gettid.md`).
- Venus finds timeline sync objects (`DRM_CAP_SYNCOBJ_TIMELINE` answers 1) and so submits with out sync objects instead of simulating them over sync files.
- `gpu-probe vk-exact` picks the physical device whose driver is `VK_DRIVER_ID_MESA_VENUS`, refusing a CPU device, draws GP0's reference triangle at 256 by 256 and compares every pixel with the reference (17,540 triangle pixels, hash `47972063c7a47bfb`). `scripts/vm-test.py vk-exact` runs it as root on `gpu=venus` with only Venus's ICD, `VN_DEBUG=init,result` and `MESA_LOG_LEVEL=debug`, and QEMU's `virtio_gpu_*` trace.

## Consequences

- Only root reaches the render name today, so only root runs Venus; a user's grant is GP12's.
- A program that passes its render descriptor by `FERRION_GPU_RENDER_FD` decides which GPU Venus uses, whatever the namespace holds.
- `VK_EXT_pci_bus_info` and `VK_EXT_physical_device_drm` report nothing useful until enumeration reports the bus and device numbers (GP4's enumeration row in [`docs/gpu-conventions.md`](../gpu-conventions.md)).

## Where

- `recipes/libs/mesa/recipe.toml`, `recipes/libs/mesa/ferrion-venus.patch`
- `recipes/libs/venus-protocol/recipe.toml`
- `recipes/tests/gpu-probe/vk_exact.c`, `scripts/vm-test.py` `test_vk_exact`

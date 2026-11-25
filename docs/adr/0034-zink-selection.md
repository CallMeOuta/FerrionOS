# Zink, the EGL platforms that choose it, and its frames in Orbital windows

Status: accepted

## Context

Slice GP5 gives FerrionOS OpenGL on the host's GPU through Zink, Mesa's OpenGL driver over Vulkan, running on Venus ([0033](0033-venus-on-ferrionos.md)). Until then OpenGL was llvmpipe on the CPU. A program must get Zink when a GPU is there and llvmpipe when it is not, without configuration, and an Orbital window must still show what Zink drew.

## Decision

- Mesa's recipe builds `-Dgallium-drivers=llvmpipe,softpipe,zink` and `-Dvulkan-layers=device-select` and lists `libvulkan` among the package's dependencies, since Zink loads the Vulkan loader at run time, which the build does not reveal. `-Dplatforms` stays `redox`; Mesa adds the surfaceless EGL platform by itself whenever EGL is built. The implicit `VK_LAYER_MESA_device_select` layer (`libVkLayer_MESA_device_select.so` and its JSON under `/usr/share/vulkan/implicit_layer.d`) orders non-CPU Vulkan devices ahead of lavapipe (`VK_PHYSICAL_DEVICE_TYPE_CPU`) so Zink's surfaceless `pdev_count = 1` probe and all Vulkan clients pick Venus automatically without `VK_DRIVER_FILES`, while still leaving lavapipe for `gpu=virtio` (where Zink rejects the CPU device and EGL falls back to llvmpipe).
- The choice (`ferrion-zink.patch`, `platform_redox.c`): `MESA_LOADER_DRIVER_OVERRIDE`, when set and nonempty, picks Zink exactly when it says `zink`; otherwise `LIBGL_ALWAYS_SOFTWARE=1` picks llvmpipe; otherwise Zink is tried when a `gpu.*` name is in `/scheme`, which reads the program's namespace without opening anything. When Zink fails to create its screen, as it does when no Vulkan device answers, EGL falls back to llvmpipe, so a program never ends up without OpenGL.
- The surfaceless platform allows Zink without a DRM device under the same conditions (`platform_surfaceless.c`).
- In the DRM pipe loader (`ferrion-gbm.patch`, `pipe_loader_drm.c`), every gallium probe of a `virtio_gpu` DRM fd (GBM and the DRM EGL platform) on FerrionOS is renamed to `zink` before the `get_nctx_caps` native-context check, so virgl and native contexts are never tried there; GP13 (native contexts) will revisit this rule. GBM on a virtio-gpu device without 3D fails with EINVAL; no software fallback.
- `zink_render_rdev` (`ferrion-gbm.patch`, `zink_screen.c`) skips Linux render-node `rdev` lookup on FerrionOS and reports no device numbers, so Zink takes the `VK_LAYER_MESA_device_select` layer's first physical device (Venus ahead of lavapipe) and enables `caps->dmabuf` when the Vulkan dma-buf extensions are present.
- Zink's frames reach an Orbital window by reading the back buffer and handing it to the swrast loader's `putImage2` (`kopper_present_swrast`), only for drawables that are not Vulkan WSI windows, so no other platform's path changes.
- Zink over lavapipe needs `LIBGL_ALWAYS_SOFTWARE=1` as well as the override, because Zink refuses a CPU Vulkan device otherwise; `scripts/vm-test.py gl-exact` uses that pair, and `gl-exact --venus` uses the override with Venus's ICD alone and requires the renderer string to name Zink and Venus and QEMU's trace to show submissions.
- `gpu-probe gl-exact` draws the reference triangle with OpenGL ES 2 on a surfaceless context and compares every pixel with the reference.

## Consequences

- Every EGL program on a machine with the render name now tries Zink first and pays its start-up before llvmpipe takes over when Zink fails; the start-up cost is measured with and without the attempt.
- OpenGL programs on Orbital keep working through a CPU copy of each frame; zero-copy presentation waits for PRIME and blob scanout ([0032](0032-gpu-page-flips-and-vblank.md)).

## Where

- `recipes/libs/mesa/recipe.toml`, `recipes/libs/mesa/ferrion-zink.patch`, `recipes/libs/mesa/ferrion-gbm.patch`
- `recipes/tests/gpu-probe/gl_exact.c`, `scripts/vm-test.py` `test_gl_exact`
- `recipes/tests/gpu-probe-gbm/main.c`, `scripts/vm-test.py` `test_gpu_gbm`
- `recipes/demos/sdl2-gears` (`--benchmark`, for the frame-time comparison)

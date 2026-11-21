# Mesa CPU port

The recipe selects Mesa 26.3.1, the stable release chosen by the maintainer, and pins the release archive by BLAKE3. The FerrionOS patches carry the effective CPU graphics port from the fork at `9850ea1ad7d687240757ebf9a73760ceb4ba28a9`, relative to upstream Mesa `d6695f16` (24.0.8). Source URLs are recorded in each patch. Imported code retains its license notices.

## Original commits

The fork contains 25 non-merge commits. Their surviving changes are grouped by function so the next Mesa update can review the platform separately from portability work.

| Original commit | Disposition in this release |
| --- | --- |
| `f72591d9d` | `ferrion-util.patch` and `ferrion-meson.patch`; the deleted OSMesa target and later-reverted hunks are omitted |
| `bec3364ad` | Reverted within the fork; no surviving change |
| `1c7f7a6b6` | The physical-memory fix is superseded by upstream's sysconf feature checks |
| `0c4416109` | Gallium Xlib winsys removed upstream |
| `d9501e7cd` | Reverted within the fork; Gallium Xlib GLX removed upstream |
| `6d506cfc4` | `ferrion-drm-headers.patch`; GLX changes are obsolete or reverted |
| `cfa7ce1f0` | Removes earlier debug prints; no surviving change |
| `7709b3010` | GLX-only build adjustment; GLX is disabled |
| `0a616d41e` | EGL build support folded into the platform patches; DRI loader guards are superseded upstream |
| `4a841b158` | `ferrion-meson.patch`, `ferrion-egl-platform.patch`, `ferrion-orbital.patch`; ownership metadata is omitted |
| `a55e0f9d7` | EGL extension definitions in `ferrion-egl-platform.patch` |
| `ccd14526f` | EGL client extension strings in `ferrion-egl-platform.patch` |
| `37df2da94` | Window synchronization in `ferrion-orbital.patch` |
| `0e517e1cc` | Reverts temporary guards; no surviving change |
| `8330107ce` | EGL build wiring folded into `ferrion-egl-platform.patch` |
| `b32fe95a2` | `ferrion-drm-headers.patch` |
| `1f6312a2f` | Descriptor-based native windows in `ferrion-orbital.patch` |
| `989fe4c2f` | `ferrion-jit-diagnostic.patch` |
| `7cbe95c60` | GNU declarations in `ferrion-meson.patch` |
| `2def5d84b` | alloca header in `ferrion-util.patch` |
| `d4cdaf101` | Orbital surface and damage support in the EGL patches |
| `86bf69c48` | Explicit EGL enablement in `ferrion-meson.patch`; static builds also disable EGL explicitly |
| `bc992462a` | Gallium Xlib GLX target removed upstream |
| `cff600b17` | Damage, stride and resize fixes in `ferrion-orbital.patch`; display detection in `ferrion-egl-platform.patch` |
| `fda1194af` | Upstream already guards the removed LLVM AVX-512 features |

## Current interfaces

The util patch uses `DETECT_OS_POSIX`, the current name of the POSIX capability macro. The physical-memory path uses the release's `HAVE_SYSCONF` and `HAVE_SC_PHYS_PAGES` checks.

The EGL platform uses direct Gallium DRI calls and `struct dri_loader_funcs`. Its swap entry point handles both full frames and damage rectangles. It filters native configurations by channel shifts and sizes before calling the current four-argument `dri2_add_config`. Display teardown includes the platform case. EGL borrows a native window descriptor; the adapter duplicates it before giving ownership to liborbital.

Mesa 26 links Gallium into EGL instead of opening and closing a swrast driver object at display initialization and termination. There is no driver `dlclose` to suppress here. The Mesa 24 workaround was removed after relibc's DSO callback fix passed `dso-atexit` and normal EGL cleanup.

Only CPU drivers are selected by the recipe. OSMesa and the old Gallium `swrast` alias were removed upstream; the same CPU selection is spelled `llvmpipe,softpipe`. Vulkan still calls its CPU selection `swrast`. LLVM JIT remains enabled and the shader cache remains disabled. No X11 or GLX is built.

The patch split leaves later GPU support separate: the existing `system_has_kms_drm` platform entry is retained, while Venus thread IDs and render-scheme discovery remain future work. Venus and Zink are not enabled by this recipe.

The separate GP1 `ferrion-libsync-wait.patch` follows the six port patches. It corrects the nanosecond subtraction after an interrupted fence wait and applies unchanged to this release.

## Build and package changes

This release requires Meson 1.5.0, Python 3.10 and glslangValidator 12.2 or newer. The existing build container supplies Meson 1.7.0 and Python 3.13.5. The pinned `host:glslang` recipe supplies glslangValidator 16.0.0 with its optimizer disabled; it adds no SPIRV-Tools input and ships no tool in the image.

The package removes the OSMesa header, library and pkg-config file, libglapi and the old `swrast_dri.so` and `kms_swrast_dri.so` modules. It adds `libgallium-26.3.1.so`, `gbm/dri_gbm.so`, the GBM backend ABI header and the lavapipe drirc defaults. libglvnd continues to supply public OpenGL, EGL and GLES dispatch; Mesa supplies vendor EGL and Vulkan libraries. A clean cook produces these changes without editing the stage or image.

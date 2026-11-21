# Linux's virtio-gpu header, for the layout check

`linux/virtgpu_drm.h`, `linux/drm.h` and `linux/drm_mode.h` are Linux's `include/uapi/drm/` files at the tag `v7.3`, commit `c03c1a14507dfaf8b856c2608f9ac216021fc222`, copied unchanged. Each keeps its own MIT license notice. [`../virtgpu-layout-check.py`](../virtgpu-layout-check.py) compiles them, checks their sha256 first, and compares them with the `virtgpu` module of relibc's crate `redox-ioctl`.

To move to a newer Linux, copy the three files from its tree, update the tag, the commit and the digests in the script, and run it.

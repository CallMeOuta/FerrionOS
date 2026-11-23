# Architecture decision records

Each record explains a decision a reader would otherwise reverse by mistake: its context, the decision, its consequences, and where in the code it lives. Read the records of an area before changing it. A record is changed when its decision changes, and a new decision gets the next number. [0001](0001-no-code-comments.md) says why these records replaced the code's comments.

| Record | Decision |
|---|---|
| [0001](0001-no-code-comments.md) | Code carries no comments; decisions live in ADRs and debts in issues |
| [0002](0002-pinned-versions.md) | A pin is a hash; its version is read from upstream, and a held-back pin says why here |
| [0003](0003-cookbook-sources-and-revisions.md) | The cookbook reads git state itself and defaults its mirrors |
| [0004](0004-cookbook-build-terminal.md) | Build scripts run on a pseudo-terminal the cookbook owns |
| [0005](0005-recipe-schema.md) | What the recipe keys mean beyond their names |
| [0006](0006-cookbook-staging-and-publishing.md) | A cooked package is staged, then renamed into place |
| [0007](0007-repo-command-behavior.md) | How the repo command selects recipes, logs and reports |
| [0008](0008-make-target-guards.md) | Make targets keep the container and the cookbook from rebuilding needlessly |
| [0009](0009-emulators-and-host-networking.md) | How QEMU, Cloud Hypervisor and their host helpers are set up |
| [0010](0010-boot-signing.md) | How images are signed for verified boot and Secure Boot, and how that is tested |
| [0011](0011-host-setup.md) | How the bootstrap scripts, the containers and the Nix shell set up a host |
| [0012](0012-repository-checks-and-conventions.md) | What the repository's own checks enforce, and how files are kept |
| [0013](0013-tpm-attestation.md) | The TPM is driven by tpmd and reached only through attestd |
| [0014](0014-ferrion-run.md) | How ferrion-run starts a program in a narrower namespace |
| [0015](0015-fdobjd.md) | fdobjd serves eventfd and timerfd objects, and its probe checks the public calls |
| [0016](0016-wayland-smoke-session.md) | The Wayland smoke session and its probe client |
| [0017](0017-linux-layer-probes.md) | How the Linux layer's probes and fixtures are built and packaged |
| [0018](0018-in-os-probes.md) | What the in-OS probes rely on, and how the test suites are packaged |
| [0019](0019-filesystem-configs.md) | Why the filesystem configs override, build from source and order what they do |
| [0020](0020-identity-and-desktop-fixtures.md) | FerrionOS's identity files and desktop mockups are generated from a few sources |
| [0021](0021-gpu-reference-guest.md) | The GPU reference guest runs pinned Linux packages only inside QEMU |
| [0022](0022-toolchain-recipes.md) | How the toolchain and language recipes cross-compile |
| [0023](0023-porting-recipes.md) | Choices the port recipes make about building and packaging |
| [0024](0024-tigerstyle.md) | Code FerrionOS owns follows TigerStyle: stated bounds, asserted invariants, no unbounded growth |
| [0025](0025-tests-without-unit-tests.md) | Changes are proven by VM checks, differential tests and simulation, not unit tests |
| [0026](0026-measured-targets.md) | Work is stated as a measured target |
| [0027](0027-monorepo.md) | The kernel, relibc, base and RedoxFS move into this repository |
| [0028](0028-gpu-render-handles.md) | A GPU's render handle, its Venus context, and the check that proves them |
| [0029](0029-gpu-host-visible-blobs.md) | Host-visible GPU blobs, the shared-memory window and render-handle mmap offsets |
| [0030](0030-gpu-submission-and-fences.md) | GPU command submission, its fences, and waiting for them |
| [0031](0031-gpu-sync-objects-and-sync-files.md) | DRM sync objects, timelines and sync files on render handles |
| [0032](0032-gpu-page-flips-and-vblank.md) | DRM page flips and synthesized vertical-blank events on display handles |
| [0033](0033-venus-on-ferrionos.md) | Mesa's Venus Vulkan driver, its pinned protocol and its render-scheme discovery |
| [0034](0034-zink-selection.md) | Zink, the EGL platforms that choose it, and its frames in Orbital windows |
| [0035](0035-signed-update-index.md) | Signed update index, verify-before-parse client and release tool |
| [0036](0036-native-window-compositor.md) | The native window compositor, its dependency gates and layout timing check |
| [0037](0037-stack-guard-and-exec-entropy.md) | Per-process random stack guard, exec entropy helper and fortify checks in relibc |
| [0038](0038-relro-and-bind-now.md) | RELRO and immediate binding in relibc and the image audit |
| [0039](0039-redoxfs-page-cache.md) | RedoxFS serves every shared file mapping from one page cache per node |
| [0040](0040-kernel-iommu-and-dma-handles.md) | The kernel drives the IOMMU, and a device's DMA reaches only what its handle grants |
| [0041](0041-anonymous-page-discard.md) | In-place private anonymous page discard (syscall 219), madvise and POSIX abort |
| [0042](0042-sparse-page-table-walks.md) | Sparse page-table walks, resident memory accounting and deferred page-table freeing |
| [0043](0043-pthread-mutex-fastpath.md) | Three-state pthread_mutex WAITING_BIT, constant owner for normal/default mutexes, and private FUTEX_WAKE fast path |
| [0044](0044-network-lab-and-baseline.md) | Unprivileged network lab, probe crate and NW0 repeatability and priority rules |
| [0046](0046-gpu-driver-host.md) | GPUs on real hardware are driven by Linux's drivers in a driver host process |
| [0049](0049-fork-file-table-and-grants.md) | Fork, exec and posix_spawn lock order, child file-table reconciliation, fail-closed close-on-exec, grant snapshots and bounded child cleanup |

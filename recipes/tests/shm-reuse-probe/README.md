# Shared file mapping reuse

`shm-reuse-probe` creates fresh files of three sizes, writes distinct patterns through ordinary file writes or a writable shared mapping, and reads every byte through a second shared mapping. It repeats each case 96 times, both with the name retained and with immediate unlinking, and with or without a prior file and mapping kept alive. It releases mappings and file handles between cycles.

Run `scripts/vm-test.py shm-reuse --config shm-reuse` on the corresponding image, or install this package in another test image. The probe uses the caller's temporary directory and creates no service or account.

The initial FerrionOS run passes the four mapped-write cases and fails the four file-write cases; the Linux host passes all eight. This reproduces wrong or stale bytes before any Wayland or OpenGL operation. The compositor's layout probe uses mapped writes and retains three reusable buffer slots while this OS failure remains open. The probe does not assign the cause to the kernel, filesystem, or C library.

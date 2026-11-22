# Shared file mapping reuse

`shm-reuse-probe` creates fresh files of three sizes, writes distinct patterns through ordinary file writes or a writable shared mapping, and reads every byte through a second shared mapping. It repeats each case 96 times, both with the name retained and with immediate unlinking, and with or without a prior file and mapping kept alive. It releases mappings and file handles between cycles.

`shm-reuse-probe read-back` writes the pattern through a writable shared mapping and reads it back through the descriptor with `pread`, while the mapping is live and after `munmap`, and checks that the file keeps its size. It repeats each case 96 times, named and unlinked.

Run `scripts/vm-test.py shm-reuse shm-read-back --config shm-reuse` on the corresponding image, or install this package in another test image. The probe uses the caller's temporary directory and creates no service or account.

Before RedoxFS's `ferrion-map-coherence.patch`, FerrionOS passed the four mapped-write cases and failed the four file-write cases and the four read-back cases. With it all twelve pass, as they do on Linux.

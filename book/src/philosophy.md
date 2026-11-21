# Philosophy

FerrionOS's build system is released under the MIT license, and so are the system components it builds from upstream's sources, such as the kernel, the drivers, the C library, the filesystem and the display server. Ported programs keep their own licenses.

The MIT license has the following properties:

- It gives you, the user of the software, complete and unrestrained access to the software, such that you may *inspect*, *modify*, and *redistribute* your changes
  - *Inspection* Anyone may inspect the software for security vulnerabilities
  - *Modification* Anyone may modify the software to fix security vulnerabilities
  - *Redistribution* Anyone may redistribute the software to patch the security vulnerabilities
- It is compatible with GPL licenses: projects licensed under the GPL can be distributed with FerrionOS
- It allows for the incorporation of GPL-incompatible free software, such as OpenZFS, which is CDDL licensed

The license does not restrict the software that may run on FerrionOS. Thanks to the microkernel architecture, even traditionally tightly coupled components such as drivers can be distributed separately, so their authors are free to choose a license for them.

The license allows FerrionOS to be used everywhere with minimum restrictions, and FerrionOS aims to remain free software.

Code that FerrionOS takes from another project keeps its license notice and names its source.

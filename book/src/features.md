# Features

This page compares FerrionOS with other operating systems for common and important features.

In the FerrionOS column:

- **Yes** means that FerrionOS has verified the feature in QEMU on x86_64 ([Hardware Support](./hardware-support.md)).
- **In the source** means that the components contain the feature and FerrionOS has not verified it.
- **No** means that the components do not contain it.

| **Feature**                                                            | **FerrionOS**                  | **Linux (GNU/Linux)**  | **FreeBSD** | **Plan 9** |
|------------------------------------------------------------------------|--------------------------------|------------------------|-------------|------------|
| [SMP](https://en.wikipedia.org/wiki/Symmetric_multiprocessing)         | In the source                  | Yes                    | Yes         | Yes        |
| [NUMA](https://en.wikipedia.org/wiki/Non-uniform_memory_access)        | In the source                  | Yes                    | Yes         | No         |
| Full Disk Encryption                                                   | In the source (RedoxFS)        | Yes                    | Yes         | No         |
| Exploit Mitigations                                                    | No (planned)                   | Yes                    | Yes         | No         |
| OpenGL/Vulkan                                                          | Drawn by the CPU only          | Yes                    | Yes         | No         |
| [UEFI](https://en.wikipedia.org/wiki/UEFI) Boot Loader                 | Yes                            | Yes                    | Yes         | No         |
| [IDE](https://en.wikipedia.org/wiki/Parallel_ATA)                      | In the source                  | Yes                    | Yes         | Yes        |
| [SATA](https://en.wikipedia.org/wiki/SATA)                             | In the source                  | Yes                    | Yes         | Yes        |
| [NVMe](https://en.wikipedia.org/wiki/NVM_Express)                      | Yes                            | Yes                    | Yes         | No         |
| [PCI](https://en.wikipedia.org/wiki/Peripheral_Component_Interconnect) | Yes                            | Yes                    | Yes         | No         |
| [PCIe](https://en.wikipedia.org/wiki/PCI_Express)                      | Yes                            | Yes                    | Yes         | No         |
| [USB](https://en.wikipedia.org/wiki/USB)                               | In the source (incomplete)     | Yes                    | Yes         | Yes        |
| [Ethernet](https://en.wikipedia.org/wiki/Ethernet)                     | In the source                  | Yes                    | Yes         | Yes        |
| [Wi-Fi](https://en.wikipedia.org/wiki/Wi-Fi)                           | No                             | Yes                    | Yes         | No         |
| [Bluetooth](https://en.wikipedia.org/wiki/Bluetooth)                   | No                             | Yes                    | Yes         | No         |

Notes on the FerrionOS column:

- SMP: the kernel's `multi_core` feature is on by default, and `make qemu` starts the x86_64 guest with four CPUs. FerrionOS has not measured how the system uses them.
- NUMA: `numa` is a default feature of the kernel.
- Full disk encryption: RedoxFS can encrypt a filesystem (`redoxfs-mkfs --encrypt`, and the `encrypt_disk` setting of the installer). FerrionOS has not tested it and does not claim it as a protection ([RedoxFS](./redoxfs.md)).
- Exploit mitigations: see [Security](./security.md).
- OpenGL and Vulkan: Mesa is built with software renderers only; there is no hardware acceleration.
- NVMe, PCI and PCIe: the verified QEMU machine has a PCIe bus and boots from an NVMe disk.
- Ethernet: on a FerrionOS image in QEMU the `virtio-netd` driver starts. FerrionOS has not yet checked a network connection on its own image.
- USB: the source has one host-controller driver, `xhcid`, and drivers for hubs, input devices and mass storage.

## External References

- [Rust OS Comparison](https://github.com/flosse/rust-os-comparison) - A table comparing some operating systems written in Rust.

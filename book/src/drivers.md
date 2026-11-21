# Drivers

On FerrionOS the device drivers are user-space daemons. Each driver is a process with its own address space, and many of the drivers enter the null namespace once they have opened the resources they need (see [Namespaces](./scheme-operation.md#namespaces)).

This design aims at keeping a faulty driver from damaging the rest of the system, whereas on a monolithic kernel a driver runs in the same memory address space as the filesystem (thus at the same privilege level) and could wipe your data. FerrionOS has not tested how well the design contains a faulty driver, and drivers run as root today. See [Security](./security.md).

You can find the driver documentation in the `README.md` of the drivers directory and in the code of the drivers.

- Drivers: the `drivers/` directory of the `base` source (recipe `recipes/core/base`), which lies in `recipes/core/base/source/drivers/` after `make f.base`

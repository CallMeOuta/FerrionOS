# Programs and Libraries

- FerrionOS is a general-purpose operating system: it is designed to run any type of program.

Some programs are interpreted by a runtime for the program's language, such as a script running in the GNU Bash shell or a Python program. Others are compiled into CPU instructions that run on a particular operating system (FerrionOS) and specific hardware (e.g. x86 compatible CPU in 64-bit mode).

- In FerrionOS, the binaries use the standard [ELF](https://en.wikipedia.org/wiki/Executable_and_Linkable_Format) ("Executable and Linkable Format") format.

Programs could directly invoke the kernel's system calls, but most call library functions that are higher-level and more easy to use. The program is linked with the libraries it needs.

- Most C/C++ programs call functions in a [C Standard Library](https://en.wikipedia.org/wiki/C_standard_library) (libc) such as `fopen()`
- FerrionOS includes a Rust implementation of the C Standard Library called relibc (recipe `recipes/core/relibc`). Ported C programs such as Git and Python link against it. relibc has partial [POSIX compatibility](https://en.wikipedia.org/wiki/C_POSIX_library).
- relibc has Linux functions for libraries and programs
- Rust programs implicitly or explicitly call functions in the [Rust Standard Library](https://doc.rust-lang.org/std/).
- The Rust standard library implements its system-dependent parts (such as file access and setting environment variables) for the `redox` target, mostly through its Unix code; [Libraries and APIs](./libraries-apis.md#rust-std-crate) gives the paths. Most of `std` is implemented for that target, so many Rust programs can be compiled for FerrionOS.

The cookbook, the build tool of the FerrionOS repository (`src/`), compiles C, C++ and Rust programs into binaries for FerrionOS from recipes (software ports) in `recipes/`.

Programs are ported case by case. The changes a program needs are kept as `.patch` files beside its `recipe.toml` and listed in its `[source] patches`. FerrionOS carries its changes to a component the same way (for example `recipes/core/kernel/ferrion-quiet-boot.patch`), and some recipes fetch forks that upstream keeps.

- [OS Internals Documentation](https://wiki.osdev.org/How_kernel,_compiler,_and_C_library_work_together)
- [ELF Documentation](https://wiki.osdev.org/ELF)

# Package Management

FerrionOS's package management is similar to that of the major Linux distributions.

Most packages are dynamically linked. The cookbook's build templates call `DYNAMIC_INIT`, which makes a program link against shared libraries, and a recipe with a custom script links statically unless the script calls `DYNAMIC_INIT` itself. The daemons and drivers of the `base` package and the RedoxFS daemon, for example, are statically linked, and so is every program for the `riscv64gc-unknown-redox` target, where `DYNAMIC_INIT` does nothing.

Static linking provides a few advantages over dynamic linking:

- **Separate Library Code**

  A statically linked program carries its own copy of the library code that it uses. Upstream's reasoning is that an attacker must then inject code into each program's address space separately, instead of into one shared library, which raises the cost of an attack. FerrionOS has not tested this and does not claim it as a protection (see [Security](./security.md)).

- **Better Performance**

  When a program is built with static linking, its library references are resolved before execution. Thus, there's no need for processing on the dynamic linker.

  This means a statically linked program will open faster than its dynamically linked equivalent, provided both are loaded entirely from disk.

- **Simpler Dependency Management**

  When a dynamically linked program depends on multiple versions of the same library, naming conflicts can arise from the identical object or symbol names within those versions. This issue can necessitate isolating the library files, often by giving them unique names or placing them in separate `/lib` directories, to ensure the proper version is used in each case.

  With *static linking*, however, there's no need for run-time dependency management, as library dependencies are included within the executable binaries.

  Rust programs aren't affected by this problem because of Cargo.

> 📝 **Note:** A Rust program links its Rust crates statically. `DYNAMIC_INIT` only makes it link the C library and other shared libraries dynamically.

## Format

### What is "pkgar" ?

Short for "package archive", `pkgar` is a file format, library, and command-line executable for creating and extracting signed collections of files, primarily for use in package management on FerrionOS.

The technical details are still in development upstream, so this page reviews the goals of `pkgar` and some examples that demonstrate its design principles.

`pkgar` has the following goals:

- Atomic - Updates are done atomically if possible
- Economical - Transfer of data must only occur when hashes change, allowing for
  network and data usage to be minimized
- Fast - Encryption and hashing algorithms are chosen for performance, and
  packages can potentially be extracted in parallel
- Minimal - Unlike other formats such as `tar`, the metadata included in a
  `pkgar` file is only what is required to extract the package
- Relocatable - Packages can be installed to any directory, by any user,
  provided the user can verify the package signature and has access to that
  directory.
- Secure - Packages are signed, and verification of all contents must occur before installation of a package completes.

To demonstrate how the format's design achieves these goals, let's look at some
examples.

### Example 1: Newly installed package

In this example, a package is installed that has never been installed on the
system, from a remote repository. We assume that the repository's public key is
already installed on disk, and that the URL to the package's `pkgar` is known.

A FerrionOS image names no package source: `/etc/pkg.d`, where `pkg` reads its sources, is empty, because FerrionOS publishes no packages yet. Upstream's packages would replace FerrionOS's patched kernel and `base`; read from the code, verified boot would then refuse the next start, which has not been tried yet.

First, a small, fixed-size header portion of the `pkgar` is downloaded. This is currently 136 bytes in size. It contains a NaCL signature, a NaCL public key, a BLAKE3 hash of the entry metadata, a 32-bit count of entry metadata structs and 32 bits of flags.

Before this header can be used, it is verified. The public key must match the
one installed on disk. The signature of the struct must verify against the
public key. If this is true, the hash and entry count are considered valid.

The entry metadata can now be downloaded to a temporary file. During the
download, the BLAKE3 hash is calculated. If this hash matches the hash in the
header, the metadata is considered valid and is moved atomically to the correct
location for future use. Both the header and metadata are stored in this file.

Each entry metadata struct contains a BLAKE3 hash of the entry data, a 64-bit
offset of the file data in the data portion of the `pkgar`, a 64-bit size of the
file data, a 32-bit mode identifying Unix permissions, and up to a 256-byte
relative path for the file.

For each entry, before downloading the file data, the path can be validated for
install permissions. The file data is downloaded to a temporary file, with no
read, write, or execute permissions. While the download is happening, the BLAKE3
hash is calculated. If this hash matches, the file data is considered valid.

After downloading all entries, the temporary files have their permissions set
as indicated by the mode in the metadata. They are then moved atomically to the
correct location. At this point, the package is successfully installed.

### Example 2: Updated package

In this example, a package is updated, and only one file changes. This is to
demonstrate the capabilities of `pkgar` to minimize disk writes and network
traffic.

First, the header is downloaded. The header is verified as before. Since a file
has changed, the metadata hash will have changed. The metadata will be
downloaded and verified. Both header and metadata will be atomically updated on
disk.

The entry metadata will be compared to the previous entry metadata. The hash for
one specific file will have changed. Only the contents for that file will be
downloaded to a temporary file, and verified. Once that is complete, it will be
atomically updated on disk. The package update is successfully completed, and
only the header, entry metadata, and the files that have changed were
downloaded and written.

### Example 3: Package verification

In this example, a package is verified against the metadata saved on disk. It is
possible to reconstruct a package from an installed system, for example, in
order to install that package from a live disk.

First, the header is verified as before. The entry metadata is then verified.
If there is a mismatch, an error is thrown and the package could be reinstalled.

The entry metadata will be compared to the files on disk. The mode of each file
will be compared to the metadata mode. Then the hash of the file data will be
compared to the hash in the metadata. If there is a mismatch, again, an error
is thrown and the package could be reinstalled.

It would be possible to perform this process while copying the package to a new
target. This allows the installation of a package from a live disk to a new
install without having to store the entire package contents.

### Conclusion

As the examples show, the design of `pkgar` aims at a package manager that never installs invalid data on disk in accessible files, and that installs fast and efficiently. These are goals of the design: FerrionOS has not tested them and does not claim them as protections (see [Security](./security.md)).

The source of `pkgar` is fairly small and worth reading. The recipe `recipes/core/pkgar` builds it, and `make f.pkgar` fetches its source.

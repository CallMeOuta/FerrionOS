# Redoxer

Redoxer is upstream's tool for building a program for the `*-unknown-redox` targets and running it in a virtual machine, without the build system. It serves [continuous integration](https://en.wikipedia.org/wiki/Continuous_integration) for programs that support the target, and quick tests during development.

> 📝 **Note:** Redoxer downloads upstream's toolchain and upstream's image. A test with it shows that a program builds for the target and runs on upstream's system. It does not test a FerrionOS image: for that, add the program to an image and boot it ([Including Programs in FerrionOS](./including-programs.md), [Testing](./testing-practices.md)). FerrionOS has not verified the commands on this page.

The recipe `recipes/dev/redoxer` names the tool's repository, and the README there describes its command line. This page assumes that you have read it.

## Installing Redoxer

Redoxer can be installed with Cargo or used from a container image. To install the `redoxer` crate from `crates.io`:

```sh
cargo install redoxer
```

Then download the toolchain and the image, and test the installation by running `true`:

```sh
redoxer exec true
```

To run Redoxer from upstream's container image instead:

```sh
docker run -ti --rm docker.io/redoxos/redoxer redoxer exec true
```

The container image holds the toolchain and the image for the virtual machine, so nothing else is downloaded when it runs. It is available for `x86_64` Linux only.

The commands below call the tool as `redoxer`. With the container, put the `docker run` command in front of it, or define an alias in `.bashrc`:

```sh
alias redoxer='docker run -ti --rm -v ".:/mnt" -w /mnt -e REDOXER_SYSROOT --device /dev/kvm docker.io/redoxos/redoxer redoxer'
```

## Build and Test a Rust Project

Redoxer wraps Cargo. To check and build a Rust project for the target:

```sh
redoxer check
```

```sh
redoxer build
```

To run the tests of a Rust project in the virtual machine:

```sh
redoxer test
```

Running `redoxer` without arguments lists the Cargo commands it wraps.

## Build and Test a C or C++ Project

Redoxer has the shorthands `ar`, `cc` and `cxx` for the archiver and the GCC compilers. To build a single `main.c` file for the target:

```sh
redoxer cc main.c -o main
```

To run that executable in the virtual machine:

```sh
redoxer exec ./main
```

To build a larger C or C++ project with GNU Make, CMake, Meson and so on, set the compiler variables to Redoxer:

```sh
export AR="redoxer ar"
export CC="redoxer cc"
export CXX="redoxer cxx"
```

## Building With Native Libraries

A project may need native libraries such as OpenSSL or zlib. To make them available to the build, run:

```sh
redoxer pkg openssl3 zlib
```

This downloads the compiled libraries from upstream's package server and extracts them to `target/$TARGET/sysroot`, or to the directory named by `REDOXER_SYSROOT`. Later Redoxer calls link against them through `RUSTFLAGS`, `LDFLAGS` and similar variables.

`redoxer env` shows the environment variables it sets, and `redoxer env bash` opens a shell with them.

## Testing With Additional Files

The `--folder` option copies the contents of a directory into `/root` of the virtual machine:

- Add the contents of the current directory

```sh
redoxer exec --folder . true
```

Packages installed with `redoxer pkg` can be added in another place than the root directory:

```sh
REDOXER_SYSROOT=sysroot redoxer pkg curl
```

```sh
# the trailing "/" in ./sysroot/usr/ matters (rsync behavior)
redoxer exec --folder ./sysroot/usr/:/usr curl --version
```

The `--artifact` option copies files from the virtual machine back to a directory after a successful run. It can be combined with `--folder`:

- Add the current directory and copy it back

```sh
redoxer exec --folder . --artifact . touch file
```

## CI Templates

- GitHub Actions:

```yml
jobs:
  redoxer:
    runs-on: ubuntu-latest
    container: redoxos/redoxer
    env:
      REDOXER_QEMU_ARGS: "-smp 1"
    steps:
      - run: ln -sf /root/.redoxer ~/.redoxer
      - uses: actions/checkout@v6
      - run: redoxer test
```

- GitLab CI:

```yml
redoxer_test:
  image: redoxos/redoxer
  variables:
    REDOXER_QEMU_ARGS: "-smp 1"
  script:
    - redoxer test
```

Two details of these templates:

- `REDOXER_QEMU_ARGS: "-smp 1"` runs the virtual machine with one CPU, to avoid tests that fail from time to time under contention between CPUs.
- `ln -sf /root/.redoxer ~/.redoxer` is needed on GitHub, which does not run the job as `/root`, so that Redoxer finds the toolchain and the image of the container.

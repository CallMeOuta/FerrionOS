# Performance

- [Benchmarks](#benchmarks)
- [Profiling](#profiling)
  - [Kernel](#kernel)
- [Compare Performance Difference Between Commits](#compare-performance-difference-between-commits)

## Benchmarks

This section contain commands for system benchmark.

- CPU performance

```sh
dd bs=1M count=1024 if=/scheme/zero | sha256sum
```

- RAM performance

```sh
dd bs=1M count=1024 if=/scheme/zero of=/scheme/null
```

- Filesystem read performance

(Add the `neverball` recipe on your filesystem image, by using the `make rp.neverball` or `sudo pkg install neverball` commands, or permanently adding using `neverball = {}` on your filesystem configuration)

```sh
dd bs=1M count=256 if=/usr/games/neverball/neverball of=/scheme/null conv=fdatasync
```

- Filesystem write performance

(Add the `neverball` recipe on your filesystem image, you can also install it with the `sudo pkg install neverball` command)

```sh
dd bs=1M count=256 if=/usr/games/neverball/neverball of=fs_write_speed_bench conv=fdatasync
```

- Userspace IPC performance

```sh
dd bs=4k count=100000 < /scheme/zero > /scheme/null
```

## Targets

- `make flamegraph` : Mounts the image, reads the kernel profile `/home/root/kprof.txt` from it, resolves it with the symbols of the `profiling-kernel` build and writes the flamegraph to `build/$ARCH/$CONFIG_NAME/flamegraph/kflamegraph.svg`
- `make rotate-flamegraph` : Moves the `build/$ARCH/$CONFIG_NAME/flamegraph` directory to `flamegraph.old`, replacing an older `flamegraph.old`
- `make differential-flamegraph` : Compares `flamegraph/collapsed.txt` with `flamegraph.old/collapsed.txt` and writes the differential flamegraph to `build/$ARCH/$CONFIG_NAME/flamegraph/differential.svg`
- `make acid-output` : Mounts the image and copies `/home/root/acid.txt` from it to `build/$ARCH/$CONFIG_NAME/acid-output/output.txt`
- `make rotate-acid-output` : Moves the `build/$ARCH/$CONFIG_NAME/acid-output` directory to `acid-output.old`, replacing an older `acid-output.old`
- `make compare-acid-output` : Runs the `bench_compare.py` script of the `acid` source (recipe `recipes/tests/acid`) on `acid-output.old/output.txt` and `acid-output/output.txt`

## Profiling

This section explain how to configure and do system performance profiling.

### Kernel

You can create a flamegraph showing the kernel's most frequent operations, using time-based sampling.

This is an example flamegraph. If you open the image in a new tab, there is useful mouse hover behavior.

![Kernel Flamegraph](./assets/kernel_flamegraph.svg "Kernel Flamegraph")

The steps below are for profiling on `x86_64`, running in `QEMU`. It is possible to run the tests on real hardware, although retrieving the data require additional effort.

If you use PS/2 input devices in real hardware you need to disable the `serio_command` code in the `src/profiling.rs` file.

#### Setup

1. Open a terminal window in the FerrionOS directory.

2. Install the tools: 

```sh
cargo install redox-kprofiling inferno
```

3. Replace the kernel with the profiling kernel in a personal filesystem configuration (`config/my_profiler.toml`, for example; step 6 shows a complete one) that includes the configuration you want to profile. `config/base.toml` brings the kernel with the line `kernel = "source"`. In the `[packages]` section of your configuration, `kernel = "ignore"` drops that package and `profiling-kernel = "source"` installs the same kernel source built with the `profiling` feature in its place:

```toml
[packages]
kernel = "ignore"
profiling-kernel = "source"
```

4. Temporarily install the `profiled` recipe with the `make rp.profiled` command or permanently install by adding it in the `[packages]` section of your filesystem configuration and run `make rp.profiled` :

```toml
[packages]
profiled = {}
```

5. Boot QEMU and use the `sudo kprof_record <app-command> [APP-ARGS...]` command to profile the kernel when running the specified application (can't be used with multiple applications in the same command).

6. (Optional) The following filesystem configuration is used for automated profiling: create the filesystem configuration (`config/my_profiler.toml`, for example) or adapt your existing configuration with the following content:

```toml
include = [ "minimal.toml" ]

# General settings
[general]
# Filesystem size in MiB
filesystem_size = 256

# Package settings
[packages]
bash = {}
kernel = "ignore" # Replaced by the profiling kernel
profiling-kernel = "source" # Kernel built with the profiling feature
profiled = {} # Profiling daemon
# Add any other recipes you need for testing here

[[files]]
path = "/usr/bin/perf_tests.sh"
data = """
#!/usr/bin/env bash
dd bs=4k count=100000 < /scheme/zero > /scheme/null
"""

# Script to perform performance tests - add your tests here
# If you will be testing manually, you don't need this section
[[files]]
path = "/usr/lib/init.d/99_tests"
data = """
echo Waiting for startup to complete...
sleep 5
echo
echo Running tests...
kprof_record /usr/bin/perf_tests.sh
echo Shutting down...
shutdown
"""
```

7. In the FerrionOS directory, create the file `.config`, or extend the one you have, with the following content (`CONFIG_NAME` selects your filesystem configuration for every `make` command, the other two settings are optional):

```make
# This needs to match the name of your filesystem configuration file
CONFIG_NAME=my_profiler
# QEMU core count, add if you want to increase the default core count (4)
QEMU_SMP=5
# Don't use the QEMU GUI
gpu=no
```

8. In the FerrionOS terminal window, run the `make r.profiling-kernel,profiled image` command.

#### Profiling

**QEMU can't be running during this step, verify if it's closed beforehand to prevent data corruption**

9. The `make flamegraph` command will create a SVG of profiling data collected inside of FerrionOS, the SVG location is: `build/$ARCH/$CONFIG_NAME/flamegraph/kflamegraph.svg`

Use the `KPROF_OPTIONS=option-value` environment variable to determine what formatting options you want for your flamegraph:

- 'i' for relaxed checking of function length
- 'o' for reporting function plus offset rather than just function
- 'x' for both grouping by function and with offset

Replace the "option-value" with your preferred formatting options.

Example:

```sh
make flamegraph KPROF_OPTIONS=xo
```

10. View your flamegraph SVG in a web browser (replace `x86_64` and `my_profiler` with your `ARCH` and `CONFIG_NAME`).

```sh
firefox build/x86_64/my_profiler/flamegraph/kflamegraph.svg
```

#### Real Hardware

TODO: test

Boot the system, and when you're done profiling, kill `profiled` and extract the `/root/profiling.txt` file (Details TBD)

## Compare Performance Difference Between Commits

To measure improvements or detect regressions.

### Kernel

This section explain how to measure performance difference between kernel commit A and B.

The kernel recipe pins a revision and applies patches, and under the default rule the cookbook restores that state. Before you check out another commit in `recipes/core/kernel/source`, set the rule of the recipe to `local` (`make f.kernel`, then `make lc.kernel`): the cookbook then builds what is in that directory. The `profiling-kernel` recipe uses the same directory and restores it as well under its default rule, so set both rules when your image uses it (`make lc.kernel,profiling-kernel`). `make cc.kernel` (or `make cc.kernel,profiling-kernel`) puts the default rule back.

1. Checkout commit A, rebuild recipe (`make cr.kernel`), and login as `root` user

2. Run one of the following commands and shutdown once finished:

```sh
kprof_record ACID_BENCH_OUTPUT=acid.txt
```

```sh
acid ACID_BENCH_OUTPUT=acid.txt
```

3. Run `make acid-output` and `make rotate-acid-output`

4. Checkout commit B, rebuild recipe (`make cr.kernel`), login as `root` user, and repeat step 2

5. Run `make acid-output` and `make compare-acid-output`

The comparison data is stored at: `build/$ARCH/$CONFIG_NAME/acid-output` (newer) and `build/$ARCH/$CONFIG_NAME/acid-output.old` (older)

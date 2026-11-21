# Tracing

(They should be used together with the methods described in [Debug Methods](./troubleshooting.md#debug-methods) section)

- [relibc](#relibc)
- [Kernel](#kernel)

## relibc

You can use `relibc` tracing to know what file descriptor resources and system function parameters programs are using, its debugging power is similar to `strace` tool.

To enable: disable the `no_trace` relibc feature flag (by removing it from the `default` feature list in `Cargo.toml` of the relibc source (recipe `recipes/core/relibc`) and run the `make rp.relibc` (if the application is dynamically linked) or `make crp.relibc,app-name` (if the application is statically linked) command to use it) and use the `RELIBC_LOG_LEVEL=trace app-name` environment variable and value to possibly show where a application error and panic (like if you get the `EINVAL` error where it shouldn't be) or hang happens. It's mostly useful for hangs but can also be for crashes, depending on type.

Fetch the relibc source with `make f.relibc` and set the rule of the recipe to `local` with `make lc.relibc` before the edit: the cookbook then builds what is in `recipes/core/relibc/source` and does not fetch it again. `make cc.relibc` puts the default rule back.

## Kernel

If you want to monitor what system calls are being made by a program, to investigate behavior, bugs or performance, there is a mechanism set up to do this.

You will learn how to configure the kernel to print a trace of system calls.

### Modifying the Kernel

You will be modifying the kernel in your own checkout, but you will not be making extensive changes.

- The kernel source is in the directory `recipes/core/kernel/source`
- If the directory is missing, go to your FerrionOS directory and run `make f.kernel`

The kernel recipe pins a revision (`rev`) and applies two patches, and under the default rule the cookbook puts the source back into that state. Once you have fetched the kernel source into its "source" directory, set the rule of the recipe to `local`, so the cookbook builds what is in that directory and neither fetches nor patches it:

```sh
make lc.kernel
```

When you are done tracing, `make cc.kernel` puts the default rule back. The next cook of the kernel then restores the pinned revision with its patches, which discards your edits.

### feature "syscall_debug"

In order to configure printing out of system calls, you will need to enable the feature "syscall_debug" for the kernel.

- In the file `recipes/core/kernel/source/Cargo.toml`, scroll down to the `default` features list (around line 79), where the feature "syscall_debug" is commented out, and remove the `#` in front of it:

```toml
default = [
  "acpi",
  #"debugger",
  "multi_core",
  "numa",
  "serial_debug",
  "self_modifying",
  "x86_kvm_pv",
  #"busy_panic",
  #"drop_panic",
  "syscall_debug", # <---- Like this
]
```

### Modify the "debug.rs" file

The file `src/syscall/debug.rs` of the kernel source contains the code to print out the system calls that match a particular set of conditions.

In the function `debug_start` (around line 210), the boolean `do_debug` determines if the system calls should be printed.

At the revision that the kernel recipe pins, it looks like this:

```rust
pub fn debug_start([a, b, c, d, e, f, g]: [usize; 7], token: &mut CleanLockToken) {
    if cfg!(not(feature = "syscall_debug")) {
        return;
    }

    #[expect(clippy::needless_bool)]
    let do_debug = if false && {
        let ctx = crate::context::current();
        let guard = ctx.read(token.token());
        guard.name.contains("init") || guard.name.contains("bootstrap")
    } {
        if a == SYS_CLOCK_GETTIME || a == SYS_YIELD || a == SYS_FUTEX {
            false
        } else if (a == SYS_WRITE || a == SYS_FSYNC) && (b == 1 || b == 2) {
            false
        } else {
            true
        }
    } else {
        false
    };
```

Obviously, the condition `false && whatever` will evaluate to false, so remove the first bit (it's to prevent accidentally turning tracing on)

The program name is read from the context, and compared with the string you specify. The name from the context normally has the full path, so we just use the `contains({name})` test.

But if your program is called "ls" for example, you will get a system call trace for any program that contains the letters "ls", so you could try something like `ends_with("/ls")`.

You can modify the boolean expression however you want, assuming you are not publishing the code. To get messages for more than one program, join the tests with `||`, as the excerpt does for "init" and "bootstrap".

If you want to hold onto the lock for the context a little longer, you will have to rework the expression a bit.

Next, there are some system calls that we skip because they are very frequent and not usually interesting. But if you want to get that level of detail, feel free to modify which system calls are filtered, `gettime`, `yield` and `futex` are typically ignored.

Also, writes to file descriptors 1 and 2 (stdout and stderr) are typically not reported so it doesn't interfere with output and debug code from your program as much.

A message will be printed at the start of the system call, and a message will be printed when the system call completes.

(A flag is set so the kernel knows to print the result.)

### Building The Changes

To include these changes in your FerrionOS image, run the following command:

```
make rp.kernel
```

Or (if the above doesn't work)

```
make r.kernel image
```

### Where Do The Messages Go

Kernel messages always go to the serial port, if you are running `make qemu` the messages will appear in that terminal.

- Consider to run the following command to capture the output:

```
make qemu |& tee my_log.txt
```

If you are doing the testing on real hardware you should probably use the `server` variant and run commands from the console.

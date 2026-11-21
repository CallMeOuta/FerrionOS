# Porting Case Study

As an example of porting a Rust program that is not trivial, this page retells how [gitoxide](https://github.com/Byron/gitoxide) was ported upstream. The repository has the result as the recipe `recipes/dev/gitoxide`, so do not repeat the steps for gitoxide itself: read them for the method, and apply it to the program you port in your own checkout.

The goal of a port is to capture everything in the recipe and its patch files. A fork of the program's repository is the last resort.

## Build on Linux

Before the port starts, build the program for Linux and make sure it works. This is not part of the port. It shows that later problems do not come from the program itself. Follow the program's own build instructions:

```sh
git clone https://github.com/Byron/gitoxide.git
```

```sh
cd gitoxide
```

```sh
cargo run --bin ein
```

## Create a Recipe

Work in a FerrionOS checkout that has built an image ([Building FerrionOS](./podman-build.md)). A new port lives in `recipes/wip/<category>/<name>/`, and its `recipe.toml` opens with a `#TODO` line that says what is missing ([Application Porting](./porting-applications.md)).

The first draft of the recipe names the source and the template:

```toml
#TODO not compiled or tested
[source]
git = "https://github.com/Byron/gitoxide"
[build]
template = "cargo"
```

## First Attempt

Cook the recipe, and keep the output:

```sh
CI=1 COOKBOOK_LOGS=true make r.gitoxide 2>&1 | tee gitoxide.log
```

The first round of errors, among other messages:

```
error[E0425]: cannot find value `POLLRDNORM` in crate `libc`
error[E0425]: cannot find value `POLLWRBAND` in crate `libc`
```

## Find Where the Problem Is

The suspicion is that the `libc` crate does not define these items for the `redox` target. `libc` is a crate of the Rust project that has a part for each operating system.

Clone `libc` beside the recipe and look for the constants:

```sh
git clone https://github.com/rust-lang/libc.git
```

```sh
grep -nrw "POLLRDNORM" --include "*.rs" libc
```

They are not defined in the part for this target, `src/unix/redox/mod.rs`. Now look in relibc, whose source is in `recipes/core/relibc/source` after `make f.relibc`:

```sh
grep -nrw "POLLRDNORM" --include "*.rs" recipes/core/relibc/source
```

Both constants are defined in relibc, and they have an implementation. They are only missing from the `libc` crate.

## Work With a Changed Crate

Add the constants to the local copy of `libc`, in `libc/src/unix/redox/mod.rs`. Copy the declarations from relibc and adapt them to the style of `libc`:

```rust
pub const POLLRDNORM: ::c_short = 0x040;
pub const POLLRDBAND: ::c_short = 0x080;
pub const POLLWRNORM: ::c_short = 0x100;
pub const POLLWRBAND: ::c_short = 0x200;
```

To test the change, the program has to use the local `libc`. Fetch the program's source, and set the recipe's rule to `local`, so that the cookbook does not replace your edits ([Changing a Component](./coding-and-building.md#changing-a-component)):

```sh
make f.gitoxide
```

```sh
make lc.gitoxide
```

In the `Cargo.toml` of the program's source, after the `[dependencies]` section, add:

```toml
[patch.crates-io]
libc = { path = "../libc" }
```

Raise the version number in the `Cargo.toml` of the local `libc`, so that it takes priority, and update the lock file of the program:

```sh
cargo update --manifest-path recipes/wip/dev/gitoxide/source/Cargo.toml
```

Cook again:

```sh
make r.gitoxide
```

The `libc` errors are gone. A change like this one belongs in the `libc` crate in the end. It is proposed to that project by the person who wrote it, under that project's contribution rules. Until a release of `libc` contains it, the recipe carries the change as a patch.

## Creating a Custom Recipe

gitoxide uses [OpenSSL](https://docs.rs/openssl/latest/openssl/), which has build instructions of its own. The repository has recipes for OpenSSL (`openssl1`, `openssl3`) that are adapted to the target, so the port uses one of them.

That needs a custom build script. A good first step is a script that only prints the environment, to see which variables a script can use:

```toml
[build]
template = "custom"
script = """
printenv
"""
```

`make r.gitoxide` now prints variables such as `TARGET` and `COOKBOOK_ROOT`, and builds nothing.

The cookbook provides shell functions for custom scripts, among them `cookbook_cargo` and `cookbook_configure`.

- A custom script for a Rust program sets up the environment and then calls `cookbook_cargo`, which runs `cargo` for the target.
- A custom script for a program with a `configure` script sets up the environment and then calls `cookbook_configure`.
- A script can also contain the whole build process and use neither function.

The functions are defined in `src/cook/script.rs` of the repository, and `build()` in `src/cook/cook_build.rs` sets the `COOKBOOK_*` variables.

A dependency on the OpenSSL recipe makes the cookbook cook it first and unpack it into the sysroot of this recipe. The script then sets the variables that the [OpenSSL bindings](https://docs.rs/openssl/latest/openssl/) crate documents:

```toml
[build]
dependencies = [
    "openssl3",
]
template = "custom"
script = """
export OPENSSL_DIR="${COOKBOOK_SYSROOT}"
export OPENSSL_STATIC="true"
cookbook_cargo
"""
```

## Linker Errors

The next stop is the linker, which links the program and its libraries into the executable. It reports undefined functions and missing static variables:

```
undefined reference to `tzset'
undefined reference to `cfmakeraw'
```

`tzset` is a time zone function, and `cfmakeraw` belongs to `termios`. Both are normally part of the C library. The `libc` crate declares them, and relibc, the C library of the target, did not implement them at the time.

## Add Missing Functions to relibc

relibc is a component like any other: its source is in `recipes/core/relibc/source`. Set its rule to `local` before the first edit:

```sh
make lc.relibc
```

After the functions `tzset` and `cfmakeraw` are implemented in relibc (the work is omitted here), note that a function is exported only when it is declared like this:

```
#[no_mangle]
extern "C" fn tzset() ...
```

Cook relibc and the program again, and push them into the image:

```sh
make crp.relibc,gitoxide
```

## Testing in QEMU

Add the program to a filesystem configuration of your own, for example `config/x86_64/my_desktop.toml`. Git ignores files whose names start with `my_`.

```toml
include = ["desktop.toml"]

[packages]
gitoxide = {}
```

Tell `make` about the configuration, build its image, and start it:

```sh
echo "CONFIG_NAME?=my_desktop" >> .config
```

```sh
CI=1 COOKBOOK_LOGS=true make all
```

```sh
make qemu
```

Log in as `user`, with an empty password, open a terminal, and try the program:

```sh
gix clone https://github.com/Byron/gitoxide
```

There are still errors, and the port makes progress from here in the same way.

## Keeping the Result

- A change to the program's source becomes a patch file beside the recipe, listed in `[source] patches`. For a recipe with a tar source, `scripts/patch-tar.sh` writes `redox.patch` from your edits.
- A change to relibc or to another component becomes a patch file beside that component's recipe.
- When the recipe carries every change, clear the `local` rules with `make cc.<recipe>`, and prove the port from a clean fetch with `make ucr.<recipe>`.
- Update the `#TODO` line of the recipe to its real state.

FerrionOS's maintainers make all changes to its repository. A port that you built in your own checkout or fork can be offered as reference work, linked from an issue: see [Participating](./contributing.md). A change to a third-party crate, such as `libc`, goes to that crate's project under its own rules.

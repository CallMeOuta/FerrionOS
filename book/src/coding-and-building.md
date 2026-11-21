# Coding and Building

(Before reading this page you must read the [Build System](./build-system-reference.md) page)

This page explains common development tasks in a FerrionOS checkout: changing the source of a component, building it, and testing the result.

- [Visual Studio Code Configuration](#visual-studio-code-configuration)
- [VS Code Tips](#vs-code-tips)
- [Changing a Component](#changing-a-component)
  - [The Recipe](#the-recipe)
  - [Protect Your Edits](#protect-your-edits)
  - [Edit Your Code](#edit-your-code)
  - [Verify Your Code on Linux](#verify-your-code-on-linux)
  - [Cook and Push](#cook-and-push)
  - [Test Your Changes](#test-your-changes)
  - [Make the Change Last](#make-the-change-last)
- [Test Outside the Build System](#test-outside-the-build-system)
- [Testing on Real Hardware](#testing-on-real-hardware)
- [Update Crates](#update-crates)
- [Search for Text in Files](#search-for-text-in-files)
- [The Image](#the-image)
  - [Build Your Recipe](#build-your-recipe)
  - [Make a New Image](#make-a-new-image)
  - [The Quickest Way to Test a Change](#the-quickest-way-to-test-a-change)
  - [Insert Text Files Through the Serial Console](#insert-text-files-through-the-serial-console)
  - [Insert Files in the Image Using a Recipe](#insert-files-in-the-image-using-a-recipe)
  - [Insert Files in the Image](#insert-files-in-the-image)
- [Working with an Unpublished Version of a Crate](#working-with-an-unpublished-version-of-a-crate)
- [How to Update RedoxFS](#how-to-update-redoxfs)

## Visual Studio Code Configuration

Before you start VS Code to work on code for FerrionOS, run the following command in your terminal:

```sh
rustup target add x86_64-unknown-redox
```

(If you are not building for x86_64, change `x86_64` in `x86_64-unknown-redox` to the CPU architecture that you are using)

Code behind directives such as `#[cfg(target_os = "redox")]` is disabled by default. To enable live syntax checking and compiler warnings for that code, add the following line to your VS Code configuration file (`.vscode/settings.json`):

```json
"rust-analyzer.cargo.target": "x86_64-unknown-redox"
```

If you browse a codebase with native dependencies, such as the kernel, the analyzer may report errors because it lacks the GCC cross toolchain. [Redoxer](./ci.md), upstream's tool, can install one with `redoxer toolchain`. Add its directory to the end of your `PATH` (for example in `~/.bashrc`), so that it does not replace the Rust toolchain you use:

```sh
export PATH="$PATH:$HOME/.redoxer/x86_64-unknown-redox/toolchain/bin"
```

## VS Code Tips

VS Code is helpful for working with unfamiliar code, and its Rust support is good. With the `rust-analyzer` extension installed, as the [overview](https://code.visualstudio.com/docs/languages/rust) describes, you get inferred types and parameter names as inline hints, definitions and references, refactoring support, and a comparison against the repository.

- **Start in the `source` directory of a recipe.** For example, `code ~/FerrionOS/recipes/games/redox-games/source`. The hints appear after a minute or two.
- **Do not open the whole FerrionOS directory for Rust work.** The analyzer cannot treat the checkout and every fetched source as one project. It is fine for editing recipes, configurations and makefiles.
- **Do not build the system in a VS Code terminal.** Use a separate terminal, so that you can close VS Code without ending the build.

## Changing a Component

The code of the system's components is not in the FerrionOS repository. A recipe fetches it into `recipes/<category>/<name>/source/` ([Components and Their Sources](./side-projects.md)). This section changes one component in your own checkout, from the first edit to a change that survives a fresh fetch.

The example is the recipe `redox-games`, a collection of terminal games. We change `minesweeper` so that it marks a flagged spot with **P** instead of **F**.

To follow it, add `redox-games` to your filesystem configuration and build the image, as [Including Programs in FerrionOS](./including-programs.md) describes. Then start the image with `make qemu`, log in, open a terminal and run `minesweeper`: the letter `f` puts an `F` on the field. Close QEMU, and keep the terminal you ran `make` in. That is your "Build" terminal, in the FerrionOS directory.

### The Recipe

The recipe is the directory `recipes/games/redox-games`. Its `recipe.toml` tells the cookbook where to get the source and how to build it. `make find.redox-games` prints the directory of a recipe.

A cook fetches the source into `recipes/games/redox-games/source` and builds it in `recipes/games/redox-games/target`.

### Protect Your Edits

The cookbook owns the `source` directory. A cook fetches first, and a fetch can check the source out again at the head of its branch and discard local changes. Targets with a `u` among their letters (`make u.<recipe>`, `make ucr.<recipe>`), `make unfetch` and `make distclean` delete `source` altogether.

Before the first edit, fetch the source and set the recipe's rule to `local`:

```sh
make f.redox-games
```

```sh
make lc.redox-games
```

Under the `local` rule the cookbook builds what is in `source` and neither fetches nor patches it. `make cc.redox-games` puts the default rule back. The `local` rule does not protect from the `u` targets.

### Edit Your Code

Open a second terminal, the "Coding" terminal, in the source directory, and edit the file with the editor of your choice:

```sh
cd ~/FerrionOS/recipes/games/redox-games/source
```

```sh
nano src/minesweeper/main.rs
```

Search for the definition of the constant `FLAGGED`, near the top of the file, and change its value to `P`:

```
const FLAGGED: &'static str = "P";
```

### Verify Your Code on Linux

Most programs for FerrionOS are source-compatible with Linux. Build and test your program on Linux first where that is possible.

- In the "Coding" terminal, check for errors with the Linux version of `cargo`:

```sh
cargo check
```

The code of `redox-games` is old, and the compiler prints warnings for it.

- The recipe builds more than one program, so name the one to run:

```sh
cargo run --bin minesweeper
```

### Cook and Push

In the "Build" terminal, with QEMU stopped, cook the recipe and push its package into the existing image:

```sh
make rp.redox-games
```

`make rp.<recipe>` is `make r.<recipe>` followed by `make p.<recipe>`. `make all` and `make qemu` do not check whether a recipe needs to be cooked again, so they do not pick up the change.

To keep the output for reading, cook with the logging convention of the build:

```sh
CI=1 COOKBOOK_LOGS=true make r.redox-games
```

The output ends with `cook redox-games - successful`, and on a failure the log is `build/logs/<target>/redox-games.log`.

### Test Your Changes

- Start the image with `make qemu`
- Log in as `user`, with an empty password
- Open a terminal
- Run `minesweeper`
- Move to a square with the arrow keys or `WSAD` and type `f` to set a flag: the character `P` appears

You have changed a program and built it into the image.

### Make the Change Last

An edit under `source` exists only in your checkout. A fresh fetch does not have it. The recipe has to carry the change, as a patch file beside `recipe.toml`:

```sh
git -C recipes/games/redox-games/source diff > recipes/games/redox-games/my-flag.patch
```

List the file in the recipe:

```toml
[source]
git = "https://gitlab.redox-os.org/redox-os/games.git"
patches = ["my-flag.patch"]
```

The cookbook applies the patches in order with `patch --strip=1` after each fetch. Then clear the `local` rule and prove the recipe from a clean fetch, which deletes `source` first:

```sh
make cc.redox-games
```

```sh
make ucr.redox-games
```

FerrionOS carries its own changes to components in the same way, as `ferrion-*.patch` files.

FerrionOS's maintainers make all changes to its repository. Work that you built in your own checkout or fork can be offered as reference work, linked from an issue: see [Participating](./contributing.md).

## Test Outside the Build System

[Redoxer](./ci.md) is upstream's tool for building a Rust, C or C++ program for the target and running it in a virtual machine, without the build system. It downloads upstream's toolchain and upstream's image, so it tests the program against upstream's system and not against a FerrionOS image.

## Testing on Real Hardware

FerrionOS has not been run on physical hardware yet ([Running FerrionOS on Real Hardware](./real-hardware.md)). The build system makes a bootable image with `make live`, which is used instead of `make image`. An image built from the repository's configurations has development accounts: give an image that leaves your machine its own accounts ([Security](./security.md)).

`make live` writes the file `build/$ARCH/your-config/redox-live.iso`, which you write to a USB drive, a disk, or a CD or DVD. For a USB drive, [Popsicle](https://github.com/pop-os/popsicle) is a convenient tool.

### Full bootable image

- Cook what changed and make a bootable image:

```sh
make rebuild live
```

### Partial bootable image

- Cook your changes to one recipe and make a bootable image, without making a hard-drive image:

```sh
make cr.recipe-name live
```

- Cook several recipes and make a bootable image, which is quicker than `make rebuild`:

```sh
make r.recipe1,recipe2 live
```

### Write the bootable image to a USB drive

If you cannot use Popsicle, use the `dd` tool:

- Go to the directory of your image:

```sh
cd build/$ARCH/your-config
```

- Find the ID of your USB drive. This command shows the IDs of all disks that are connected to your computer:

```sh
ls /dev/disk/by-id
```

Look for the items that begin with `usb` and find the model of your drive. Do not use an ID that ends in `part-x`.

- Write the image, with `usb-your-device-model` replaced by the ID you found:

```sh
sudo dd if=redox-live.iso of=/dev/disk/by-id/usb-your-device-model oflag=sync bs=4M status=progress
```

**Double-check the `of=/dev/disk/by-id/usb-your-device-model` part to avoid data loss**

### Burn a CD or DVD with the bootable image

- Go to the directory of your image:

```sh
cd build/$ARCH/your-config
```

- Check whether your optical drive can write:

```sh
cat /proc/sys/dev/cdrom/info
```

The item "Can write" shows `1` (yes) or `0` (no). The output also names the optical drives of the computer, as `/dev/srX`.

- Burn the disk with [xorriso](https://www.gnu.org/software/xorriso/), where `X` in `/dev/srX` is the number of your optical drive:

```sh
xorriso -as cdrecord -v -sao dev=/dev/srX redox-live.iso
```

## Update Crates

Read [this](./porting-applications.md#update-crates) section to learn how to update crates.

## Search for Text in Files

To find the file that contains a particular command, crate or function call, use the `grep` command.

- Command examples

```sh
grep -rnw "redox-syscall" --include "Cargo.toml"
```

This command shows every `Cargo.toml` file that contains the text "redox-syscall". It helps to find the recipe that contains a command or uses a crate.

```sh
grep -rni "physmap" --include "*.rs"
```

This command finds every `.rs` file that contains the text "physmap". It helps to find where a function is used or defined.

The options mean:

- `-n`: display the line number of the match in each file.
- `-r`: search directories recursively.
- `-w`: match only whole words.
- `-i`: ignore case.

## The Image

This section explains how to cook recipes and how to create and change the image.

### Build Your Recipe

You can cook one recipe, instead of having `make rebuild` check every recipe of the configuration. This shortens the build while you resolve compilation or linking errors.

- In your "Build" terminal, in the FerrionOS directory, run:

```sh
make r.redox-games
```

The makefiles have a rule for the target `r.recipe`, where `recipe` is the name of a recipe. It cooks the recipe and makes its package.

After the recipe has been cooked, `make p.redox-games` pushes the package into the existing image.

To keep a log of a failing cook:

```sh
make cr.recipe-name 2>&1 | tee recipe-name.log
```

### Make a New Image

If pushing into the existing image does not work, make a new image.

- In your "Build" terminal, in the FerrionOS directory, run:

```sh
make image
```

`make image` cooks no recipe if the last complete cook succeeded, and it always creates a new image from the packages that exist, including the recipe you cooked in the previous step.

### The Quickest Way to Test a Change

Run:

```sh
make rp.recipe-name qemu
```

Or, if your change does not allow incremental compilation:

```sh
make crp.recipe-name qemu
```

This cooks the recipe you changed, pushes it into the image and starts QEMU.

### Insert Text Files Through the Serial Console

To move text, such as the output of a command, a log or a script, between your host and a FerrionOS instance in QEMU, use the terminal that started QEMU. `qemu_serial_logfile` writes everything that appears on the serial console to a file:

```sh
make qemu gpu=no qemu_serial_logfile=qemu.log
```

```
ferrion login: user
# run your commands, with their output on the terminal
```

After you shut the guest down, the output is in the file `qemu.log`.

To move a text file onto FerrionOS, paste it into the terminal from the clipboard:

```sh
cat > mylog.log << EOF
# paste the text here, then type EOF on a line of its own
EOF
```

For large files, files that are not plain text, or many files, share a directory of the host with [VirtIO-FS](./virtio-fs.md), or use one of the two methods below.

Files that you create in a running FerrionOS stay in the image until you make a new image.

### Insert Files in the Image Using a Recipe

The recipe `myfiles` packages your own files, so that they are in every new image. Its package installs everything from its `source` directory into `/home/user`.

- Create the `source` directory in the recipe directory of `myfiles`, and copy or move your files into it:

```sh
mkdir recipes/other/myfiles/source
```

- Cook the recipe and push it into the image:

```sh
make rp.myfiles
```

- To have the files in every new image, add the recipe to the `[packages]` section of your filesystem configuration:

```toml
[packages]
myfiles = {}
```

- Start QEMU to check your files:

```sh
make qemu
```

### Insert Files in the Image

You can mount an existing image and copy files into it or out of it. A recipe is the repeatable way; use the mount for a file you need once.

**FerrionOS must not be running in QEMU while the image is mounted.**

- In your "Build" terminal, in the FerrionOS directory, run:

```sh
make mount
```

The image is now mounted as the directory `build/x86_64/your-config/filesystem`. Copy your files, and close every program that uses the directory.

- Unmount the image before you start QEMU:

```sh
make unmount
```

```sh
make qemu
```

A new image, from `make image` or `make rebuild`, does not have the files you copied.

## Working with an Unpublished Version of a Crate

Many of the libraries that the components use are published on [crates.io](https://crates.io/) and are downloaded from there. To build against a changed copy of such a crate before a new version is published, follow these sections of the Cargo documentation:

- [Overriding Dependencies](https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html)
- [Working with an unpublished minor version](https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html#working-with-an-unpublished-minor-version)

## How to Update RedoxFS

RedoxFS is built into the `initfs`, which is part of the `base` package. After a change to the source of the `redoxfs` recipe, cook `base` as well, so that the `initfs` contains your change. To cook both and push them into the existing image, run:

```sh
make rp.redoxfs,base
```

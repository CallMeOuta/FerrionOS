# Including Programs in FerrionOS

(Before reading this page you must read the [Build System](./build-system-reference.md) page)

This page shows how to add programs to a FerrionOS image. It is a simplified version of the [Application Porting](./porting-applications.md) page.

- [Existing Recipe](#existing-recipe)
  - [Set Up the Build Environment](#set-up-the-build-environment)
  - [Set Up Your Configuration](#set-up-your-configuration)
  - [Build the Image](#build-the-image)
  - [Dependencies](#dependencies)
  - [Update Crates](#update-crates)
- [Modifying an Existing Recipe](#modifying-an-existing-recipe)
- [Create Your Own Hello World](#create-your-own-hello-world)
  - [Setting Up the Recipe](#setting-up-the-recipe)
  - [Writing the Program](#writing-the-program)
  - [Adding the Program to the Image](#adding-the-program-to-the-image)
- [Running Your Program](#running-your-program)

The cookbook makes packaging simple. First, this page adds a program that already has a recipe. Then it creates a new program and adds it. The [Coding and Building](./coding-and-building.md) page discusses the development cycle in more detail.

## Existing Recipe

The repository has recipes for many programs. Each one is a directory `recipes/<category>/<name>`, and adding a program to your image means adding its name to your filesystem configuration. This example adds the `redox-games` package, which contains several terminal games.

### Set Up the Build Environment

- Follow the steps in [Building FerrionOS](./podman-build.md) to build the default image. This takes a long time the first time.
- Start the image in QEMU:

```sh
cd ~/FerrionOS
```

```sh
make qemu
```

The default `desktop` configuration does not include the games.

- Log in as `user`, with an empty password.
- Open a terminal from the bar at the bottom of the screen and type `ls /usr/bin`. `minesweeper` **is not** listed.
- Close QEMU.

### Set Up Your Configuration

Read the [Configuration Settings](./configuration-settings.md) page first.

- In the FerrionOS directory, create a filesystem configuration of your own, `config/x86_64/my_config.toml`, with this content. It includes the desktop configuration from the same directory and adds a package. Git ignores files whose names start with `my_`.

```toml
include = ["desktop.toml"]

[packages]
redox-games = {}
```

- Make it the configuration that `make` builds, by adding this line to your [.config](./configuration-settings.md#config) file:

```
CONFIG_NAME?=my_config
```

### Build the Image

A new configuration has its own build directory, `build/x86_64/my_config`, and no image yet. Build it, and start it in QEMU:

```sh
CI=1 COOKBOOK_LOGS=true make all
```

```sh
make qemu
```

The packages that were cooked for the `desktop` image are reused, so only `redox-games` is cooked.

- Log in as `user`, with an empty password.
- Open a terminal and type `ls /usr/bin`. `minesweeper` **is** listed.
- Type `minesweeper`. Play with the arrow keys or `WSAD`, `space` to reveal a spot, and `f` to flag a spot when you suspect a mine. When you type `f`, an `F` appears.

If the cook fails, its log is `build/logs/<target>/redox-games.log`.

### Dependencies

Read the [Dependencies](./porting-applications.md#dependencies-1) section to learn how to handle the dependencies of a recipe.

### Update Crates

Read the [Update crates](./porting-applications.md#update-crates) section to learn how to update the crates of a Rust program.

## Modifying an Existing Recipe

To change the source of an existing recipe, work in its `source` directory after setting the recipe's rule to `local`, as [Changing a Component](./coding-and-building.md#changing-a-component) describes. Without that rule, the cookbook can replace the directory at the next fetch.

## Create Your Own Hello World

To include a program of your own, create a recipe for it. This example adds the "Hello World" program that Cargo generates for a new Rust project. The process is largely the same for other Rust programs.

### Setting Up the Recipe

The cookbook builds only programs that have a recipe under `recipes`. Create the directory `recipes/other/hello-world`, and in it the file `recipe.toml` with these lines:

```toml
[build]
template = "cargo"
```

The `[build]` section defines how the cookbook builds the project. There are several templates, and `"cargo"` is the one for Rust projects.

A `[source]` section would tell the cookbook where to download the program from. This example develops the program in the recipe's `source` directory, so the recipe has no `[source]` section, and the cookbook uses the directory as it is.

### Writing the Program

Cargo writes the program. In `recipes/other/hello-world`, run:

```sh
mkdir source
```

```sh
cd source
```

```sh
cargo init --name hello-world
```

```sh
cargo generate-lockfile
```

This creates `Cargo.toml`, the `src` directory with the "Hello World" program, and the `Cargo.lock` file that the cookbook's build expects.

### Adding the Program to the Image

A program runs in FerrionOS only when its package is in the image. Add `hello-world` to the `[packages]` section of your filesystem configuration, `config/x86_64/my_config.toml` from the first example:

```toml
[packages]
# Add the item below
hello-world = {}
```

When the image is made, the build system installs the packages of the configuration into its filesystem.

To cook the program and push it into the existing image, run:

```sh
cd ~/FerrionOS
```

```sh
make rp.hello-world
```

## Running Your Program

Run `make qemu`, log in, open a terminal, and run `hello-world`. It prints:

```
Hello, world!
```

The `hello-world` program is in `/usr/bin` on FerrionOS.

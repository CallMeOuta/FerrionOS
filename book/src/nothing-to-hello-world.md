# From Nothing To Hello World

This page is the shortest path from an empty directory to your own program running on FerrionOS. It uses prebuilt packages for most of the system, so only your program and the four components that FerrionOS patches are compiled.

The example is a "Hello World" program written in Rust. The steps assume an x86_64 Linux host.

 1. Clone the repository and enter it.

    ```sh
    git clone https://github.com/CallMeOuta/FerrionOS.git
    ```

    ```sh
    cd FerrionOS
    ```

 2. Install the host tools. Always pass `-d`: without it, the script clones the upstream build system and deletes itself.

    ```sh
    ./podman_bootstrap.sh -d
    ```

 3. Enable the Rust toolchain in the current shell.

    ```sh
    source ~/.cargo/env
    ```

 4. Create the `.config` file. `REPO_BINARY` downloads prebuilt packages, and `CONFIG_NAME` selects the filesystem configuration that step 8 creates.

    ```sh
    printf 'REPO_BINARY?=1\nCONFIG_NAME?=my_config\n' > .config
    ```

 5. Create the recipe directory of the program, with a `source` directory in it.

    ```sh
    mkdir -p recipes/other/hello-world/source
    ```

 6. Create a Cargo project in the `source` directory, and its lock file, which the cookbook's build expects. Cargo writes the "Hello World" program.

    ```sh
    cargo init --name hello-world recipes/other/hello-world/source
    ```

    ```sh
    cargo generate-lockfile --manifest-path recipes/other/hello-world/source/Cargo.toml
    ```

 7. Create the recipe, `recipes/other/hello-world/recipe.toml`, with this content:

    ```toml
    [build]
    template = "cargo"
    ```

 8. Create the filesystem configuration, `config/x86_64/my_config.toml`, with this content. It takes everything from the desktop configuration and adds the program, built from source. Git ignores files whose names start with `my_`.

    ```toml
    include = ["desktop.toml"]

    [packages]
    hello-world = "source"
    ```

 9. Build the image.

    ```sh
    CI=1 COOKBOOK_LOGS=true make all
    ```

10. Start the image in QEMU without a window.

    ```sh
    make qemu gpu=no
    ```

11. At the `ferrion login:` prompt, type `user` and press Enter.

12. Run the program.

    ```sh
    hello-world
    ```

13. Shut down the virtual machine.

    ```sh
    sudo shutdown
    ```

After a change to the program, `make rp.hello-world` cooks it again and pushes it into the existing image. Stop QEMU first.

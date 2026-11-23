# Trying Out FerrionOS

There are several programs, demos and other tools to try on FerrionOS. Most of these are not included in the default `desktop` image: they are in the `demo` config (`config/x86_64/demo.toml` has the full list), which FerrionOS has not built or verified yet. What this page says about the `demo` config is inherited from upstream.

FerrionOS publishes no images, so build the `demo` image yourself with `CI=1 COOKBOOK_LOGS=true make all CONFIG_NAME=demo` ([Building FerrionOS](./podman-build.md)). The config does not list the `ffmpeg6` and `rustpython` packages, which the FFMPEG and RustPython sections below need; [Including Programs in FerrionOS](./including-programs.md) describes how to add a package to an image.

The system has no Wi-Fi driver, so if you need Internet access for some of the things you want to do, use an Ethernet cable or [run FerrionOS in a virtual machine](./running-vm.md). Most of the suggestions below do not require Internet access.

Many of the available commands are in the folder `/usr/bin`, which is included in your command path. Open a Terminal window and type `ls /usr/bin` (or `ls /scheme/file/usr/bin`) to see some of the available commands.

## Programs

### FFMPEG

The most advanced multimedia library of the world.

- Run the following command to play an audio file:

```sh
ffplay music-name.mp3
```

(Change the audio format according to your file)

- Run the following command to play a video file:

```sh
ffplay video-name.mp4
```

(Change the video format according to your file)

### COSMIC Files

A file manager written in Rust, similar to GNOME Nautilus or Files. The `desktop` config currently installs it, so the default image has it as well. Which desktop apps FerrionOS ships is not settled and may change.

### COSMIC Editor

A text editor written in Rust, similar to KDE KWrite. The `desktop` config currently installs it as well.

### Git

Git is a tool used for source code management.

- Run the following command to download a Git repository:

```sh
git clone repository-link
```

(Replace the "repository-link" part with your repository URL)

### RustPython

RustPython is a Python 3.11+ interpreter written in Rust.

- Run the following command to run your Python script:

```sh
rustpython script-name.py
```

(Upstream states that the [PyPI](https://pypi.org/) dependency manager is supported)

### Periodic Table

The Periodic Table `/usr/bin/periodictable` is a demonstration of the **OrbTk** user interface toolkit.

### Kibi

Kibi is the default terminal text editor inspired on GNU Nano but with more features, run the `kibi` command to use it.

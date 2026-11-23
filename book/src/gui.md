# GUI

The graphical session of FerrionOS is provided by programs that run in user space.

- **Orbital** is the display server and window manager. It sets up the `orbital` scheme, manages the display, and handles requests for window creation, redraws and event polling.
- **`orblogin`** is the graphical login screen.
- **`launcher`** is the bar that starts applications.

A filesystem configuration starts them from an init file: `config/desktop-minimal.toml` writes `/usr/lib/init.d/20_orbital`, which starts `orbital` with `orblogin` and `launcher` on the third virtual terminal.

## Programs

The `desktop` configuration currently installs these programs with a graphical interface:

- COSMIC Terminal (`cosmic-term`), a terminal emulator
- COSMIC Files (`cosmic-files`), a file manager
- COSMIC Editor (`cosmic-edit`), a text editor
- The graphical installer (`installer-gui`)
- Orbital's own utilities (`orbutils`)

Which desktop apps FerrionOS ships is not settled and may change. One consistent desktop is a goal ([Influences](./influences.md)).

[Windowing and Graphics](./graphics-windowing.md) describes the display drivers, the rendering and the libraries that programs can use.

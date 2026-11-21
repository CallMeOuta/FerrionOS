# FerrionOS's identity files and desktop mockups are generated from a few sources

Status: accepted

## Context

FerrionOS's mark, name, colors and the pictures derived from them (login background, wallpaper, launcher button, boot splash colors, the book's logo and favicon, the project page's card) must agree with each other and with the desktop mockups of `plans/desktop-design.md`, which the VM tests compare screenshots against. Orbital's login program and wallpaper program scale images, and the orbdata package installs the files Orbital uses.

## Decision

### One generator, three sources

`branding/generate.py` builds every derived identity file from `mark.svg` (the mark on a grid of 4 by 7 units), `wordmark.svg` (the name, outlined) and `color.tokens.json` (ground and ink). Its outputs are the files `main` writes under `branding/`; the text ones say in their first lines that they are generated. `orbdata/`, `book/` and `github/` hold the images for the package, the book and the project page, each at the path or under the name its user expects. It runs after a source changes, with Python's standard library and three programs on `PATH`: `rsvg-convert` for PNG files, `magick` for the JPEG and `optipng` for the two PNG files uploaded to the project page.

### Geometry

- Lengths are in units of 16 reference pixels. The lockup puts the mark above the name, 2 units from the foot of the mark to the cap line of the name, with the name a quarter unit in from the left edge to its first stem; the mark and the name sit on a grid fine enough for both to use whole numbers.
- The login background and the wallpaper are the size of the reference screen, 1280 by 800: orblogin and the wallpaper program scale an image to cover the screen and leave one of the screen's own size as it is. orblogin draws its box, `LOGIN_BOX_HEIGHT` pixels high, on the center, so the lockup sits above it, 4 units from the name's baseline to the top of the box.
- The empty part of the boot splash's progress bar is ink at `TRACK_SHARE` over ground.
- The book's logo fills the slot its first page sets with width and height, for light and dark pages, with the name starting at the left edge where the page's text starts. The favicon is the mark in ink on a ground tile, which shows on any tab bar. The project page's card centers the lockup on the ground: its box across, and from the top of the mark to the baseline of the name down.
- `branding/orbital-background.toml` holds the lines for the end of the `orbital.toml` the orbdata package installs: the ground, which Orbital paints behind every window.

### The orbdata overlay

The orbdata recipe lays FerrionOS's login background, wallpaper and launcher button from `branding/orbdata/` over upstream's files of the same paths, and sets Orbital's background to the ground (`branding/orbital-background.toml`), in its build script, because these are images and generated text that a patch file cannot carry or would copy. After regenerating them, cook again with `make cr.orbdata`: the cookbook watches only `recipe.toml` and `source/`.

### The desktop mockups

- `scripts/desktop-fixtures/mockups.py` builds the mockups of `plans/desktop-design.md` at the reference screen of 1280 by 800 pixels, QEMU's default mode and `scripts/vm-test.py`'s, from the identity kit and the desktop image: ground and ink from `branding/color.tokens.json`, the mark, Fira Sans and Fira Mono from orbdata's fetched source, and symbolic icons from cosmic-icons' fetched source (`make f.orbdata f.cosmic-icons`).
- Every other neutral is ground or ink at a fixed share over the other (`SHARES`, in percent: ink over ground in the dark theme, ground over ink in the light one). The shares, the one accent (amber by default, with the set the prototype offers) and the three window-control colors, which are the red, yellow and green of the terminal's sixteen colors, are the maintainer's prototype's.
- Each screen is written twice. The rich picture, beside the script, has soft shadows (each layer as the prototype sets it: vertical offset, blur radius, opacity) and translucent panels, which only a renderer that can blur draws. The flat picture under `flat/` is what a CPU renderer draws: the same boxes with opaque fills and no shadow, with its regions as `<rect>` elements under `<g id="regions">` (the rectangle a part of the shell occupies, and probe pixels with the color expected there). The plan's slices compare VM screenshots against the flat files. Text is outlined, so a mockup looks the same in every viewer.
- Icons are measured without the transparent square that pads each to its box; a few icons drawn one pixel larger than 16 are centered. The small mark is 8 by 14 pixels, as `branding/mark-16.svg` draws it. The lock screen puts the mark where `branding/mark.rs` puts it on the boot screen, 314 pixels above the center, and the cap line of the time where the login screen puts the cap line of the name. Cursor lines are counted from 1.
- `--check` renders `flat/` and reads every probe back (it needs `rsvg-convert` and `magick`); `--tokens` prints the plan's token tables. The script needs fontTools.

## Consequences

- Editing a generated file by hand is undone at the next run of the generator; change the source and run it.
- Changing a size in the generator without the mockups, or the other way round, makes the desktop's VM comparisons fail.

## Where

- `branding/generate.py`: `UNIT`, `MARK_TO_NAME`, `NAME_INSET`, `SCREEN`, `LOGIN_BOX_HEIGHT`, `NAME_TO_BOX`, `BOOK_LOGO`, `FAVICON`, `TRACK_SHARE`, `main`
- `branding/mark.rs`, `branding/orbital-background.toml` (generated)
- `recipes/gui/orbdata/recipe.toml`: `[build] script`
- `scripts/desktop-fixtures/mockups.py`: `W`, `H`, `SHARES`, `main`

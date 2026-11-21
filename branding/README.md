# FerrionOS identity kit

The mark, the wordmark, the two colors and every file made from them. Decided by the maintainer on 2026-11-20 after one round of three candidates; the record of that round is at the end.

Installed in the x86_64 desktop image on 2026-11-20: the login screen, the launcher's start button, the wallpaper and Orbital's background come from `orbdata/` and `orbital-background.toml` through `recipes/gui/orbdata`, the boot splash draws `mark.rs` through `recipes/core/base`'s patch, the bootloader fills the screen with the ground, `recipes/gui/orbutils`'s patch gives the login box the neutral colors, and `config/dev-accounts.toml` puts `mark.txt` above the console's login prompt. Screenshots of the booted image matched the expected frames' probe pixels; images Orbital draws come out one step darker than the tokens (`#0e0d0b` for the ground, `#eceae6` for the ink), while flat fills are exact.

## The kit

### Mark

`mark.svg`: a capital F on a grid of 4 by 7 units, cut through its stem above the middle arm. Every stroke and the cut are 1 unit; the top arm is 4 units and the middle arm 3.

- The one distinctive element is the cut. Its job is to stop the letter reading as a stock F: it turns one letter into two hooks that stay apart at 16 pixels, where the cut is 2 pixels.
- It is drawn at a whole number of pixels per unit and never resampled. The table under "Slots" lists every size in use.
- In a square slot the mark is centered by its box, and half a pixel is rounded down.

### Wordmark

`wordmark.svg`: `FerrionOS`, one word, in Fira Sans SemiBold, the sans face that the orbdata package ships, outlined with the font's own kerning and no added tracking. Where a surface sets type itself, the name is plain text in that face.

### Lockup

`lockup.svg`: the mark above the name, on a box of 10 by 11 units.

- The mark is centered at the top. The cap line of the name is 2 units under the foot of the mark, its capitals are 1.5 units high, and its first stem starts a quarter unit from the left edge of the box.
- The maintainer decided it for the login screen, where nothing else names the system. It is also what the project page and the book show: the head of the README, the social preview, and the first page of the book.
- The mark goes alone where the slot is an icon's or the name is already beside it: the launcher's start button, symbolic icons, a favicon, an avatar, the text console, and the boot splash.
- The name goes alone, as text, in running prose.

### Color

`color.tokens.json`, in the Design Tokens format (2025.10), holds two colors, and no hue.

|Token|Value|Job|
|---|---|---|
|`ground`|`#0f0e0c`, OKLCH 0.164 0.004 84.6|the one background from power-on to login, and behind the mark everywhere|
|`ink`|`#edebe7`, OKLCH 0.94 0.006 84.6|the mark, the wordmark and text on the ground; 16.2:1 on it|

On a light page the two swap: the art is in the ground color. What is drawn next to the mark on the ground takes ink at a fixed share over ground and no other color:

|Share|Value|Job|
|---|---|---|
|6%|`#1c1b19`|a panel|
|16%|`#33312f`|a button, and the empty part of a progress bar|
|24%|`#444341`|a button under the pointer|
|42%|`#6c6b68`|the edge of a field, 3.2:1 on a panel|
|70%|`#aaa9a5`|a label, 7.3:1 on a panel|

### Slots

The one thing that varies is the ground the mark sits on. The shape never varies and nothing is random.

|Ground|Meaning|
|---|---|
|solid|the ground color fills the screen or the image|
|tile|a ground-color square the size of the slot|
|none|transparent: ink for dark pages, the ground color for light ones, `currentColor` in the symbolic files|
|cells|`#` characters in a text console|

Every slot in use, and the file for it:

|Slot|Size|Pixels per unit|Ground|File|
|---|---|---|---|---|
|Symbolic icon|16x16|2, the mark is 8x14 at (4, 1)|none|`mark-16.svg`|
|Symbolic icon|32x32|4, the mark is 16x28 at (8, 2)|none|`mark-32.svg`|
|Launcher's start button|48x48|6, the mark is 24x42 at (12, 3)|none|`orbdata/usr/share/icons/places/start-here.png`|
|Favicon of the book|32x32|4|tile|`book/theme/favicon.png`|
|Avatar|500x500|40, the mark is 160x280 at (170, 110), inside a round crop|tile|`github/avatar.png`|
|Text console|8 columns by 7 rows|2 columns by 1 row|cells|`mark.txt`|
|Boot splash|the screen|16 at 1280x800, scaled as the login background is|solid|`mark.rs`|
|Login background, lockup|1280x800|16, the box of the lockup at (560, 86)|solid|`orbdata/usr/share/ui/login.png`|
|Logo of the book, lockup|511x180|16|none, one file for light pages and one for dark|`book/src/assets/logo-light.png`, `logo-dark.png`|
|Head of the README, lockup|320x352|32|none, one file for light pages and one for dark|`github/readme-light.svg`, `readme-dark.svg`|
|Social preview, lockup|1280x640|40, the ink is 108 pixels or more from every edge|solid|`github/social-preview.png`|

A new slot gets a row here and a few lines in `generate.py`.

### Rules of use

- One mark per view. It is never a bullet, a separator, a watermark or a spinner. The wallpaper carries no mark, because the launcher's start button already shows it on the desktop.
- On the boot path the mark appears once and stays where it is: the splash draws it where the login screen shows it, and its progress bar sits where the login screen sets the name.
- Flat. No gradient, shadow, outline or glow.
- The mark is never mirrored. Mirrored, it is close to another project's mark ("Lookalike check" below).

## Files

Sources, edited by hand:

|File|What|
|---|---|
|`mark.svg`|the mark on its grid|
|`wordmark.svg`|the name, outlined; it carries the font's license notice|
|`color.tokens.json`|ground and ink|

`python3 branding/generate.py` writes everything else from those three. It needs `rsvg-convert` (librsvg), `magick` (ImageMagick) and `optipng` on `PATH`. A generated file is never edited: change a source or the script and run it again.

|File|What|For|
|---|---|---|
|`lockup.svg`|the mark above the name, `currentColor`|any page that can color it|
|`mark-16.svg`, `mark-32.svg`|the mark for symbolic icon slots, `currentColor`|an icon theme, a website|
|`mark.txt`|the mark in `#`, 7 lines of at most 8 columns|`/etc/issue`|
|`mark.rs`|the mark as seven bytes, the colors, and its place on the screen, as Rust constants|fbbootlogd's boot splash, in the `base` recipe's patch|
|`orbital-background.toml`|the ground as Orbital's `background_color`|appended to the `orbital.toml` of the orbdata package|
|`orbdata/`|three images at their paths in the orbdata package|copied over that package by its recipe|
|`book/`|a favicon and two logos at their paths in `book/`|the book|
|`github/`|social preview, avatar and the two README headers|the project page; the PNG files are optimized with `optipng -strip all`|

## What replaces the upstream brand

Every image with upstream's brand in the x86_64 desktop image, found by scanning the staged files of the 72 packages built or fetched for that config (6,891 image files) on 2026-11-20:

|File in the image|Package|Was|Now|
|---|---|---|---|
|`/usr/share/ui/login.png`|orbdata|1024x768, 47,993 bytes: the upstream logo behind the login box|`orbdata/usr/share/ui/login.png`, 1280x800, the lockup on the ground|
|`/usr/share/icons/places/start-here.png`|orbdata|48x48, 1,307 bytes: the upstream logo as the launcher's start button|`orbdata/usr/share/icons/places/start-here.png`, 48x48, the mark|
|`/usr/share/ui/background.jpg`|orbdata|3840x2221, 2,519,476 bytes: upstream's wallpaper, a space photograph without a logo|`orbdata/usr/share/ui/background.jpg`, 1280x800, the ground|
|the mark's place on the boot splash|base (fbbootlogd)|an empty square|the mark, from `mark.rs`|

And in the book:

|File|Was|Now|
|---|---|---|
|`book/theme/favicon.png`|32x32, the upstream logo|`book/theme/favicon.png`|
|the two logos of the first page|511x180 each, the upstream logo for light and for dark themes|`book/src/assets/logo-light.png`, `logo-dark.png`|

What the scan covered and found clean:

- No image file of any staged package has the upstream name in its file name.
- All 31 PNG files of orbdata were looked at; the two above are the only ones with a logo.
- No PNG is embedded in `orbital`, `orblogin`, `launcher`, `background`, `viewer`, the installer's GUI, the three COSMIC programs or NetSurf, so no about dialog draws an upstream image. Their text was not read.
- pop-icon-theme ships 294 files named after other distributions (`distributor-logo*.svg`, `start-here-*.svg`, `*-logo-icon.svg`) and cosmic-icons one `start-here-symbolic.svg`. None is upstream's, and none of the programs above asks for them by name.

Why two files are not the size of the ones they replace: orblogin and the wallpaper program scale an image to cover the screen and leave one of the screen's own size untouched. At 1280x800, the reference screen, both are drawn pixel for pixel; the old login image was drawn at 1.25 times with 80 pixels cut top and bottom.

## Limits

- Other screens: orblogin scales the login background by the larger of width / 1280 and height / 800. The mark stays on whole pixels where 16 times that scale is a whole number (1920x1080, 1600x900, 800x600, 2560x1440) and softens elsewhere (1024x768, 1366x768).
- A screen wider than 2.04:1 cuts into the top of the mark, and at 640x480 the name ends 6 pixels above the login box. Fixing both needs orblogin to draw the lockup from `mark.rs` and the font, not from an image.
- orblogin draws its box at its own whole-number scale. On a screen where that scale is 2 the box grows towards the name; this was not tried.
- orblogin's box keeps upstream's blue buttons until its colors are changed to the shares above.
- Fira Sans was inherited from the orbdata package, not chosen for FerrionOS.

## Record of the round

Three candidates were shown on 2026-11-20, each on the login screen, at 16 and 32 pixels and in the console: the name alone, the split F, and an F whose middle arm floats off the stem. The maintainer's answer: all three look good, the login screen shows the mark with the name under it, the colors are the neutral pair, and there is no round for a hue. From that: the split F is the mark, the name is the wordmark, and the third candidate is retired.

The brief:

- Job: tell a person, at 16 pixels and from across a room, that the system on screen is FerrionOS.
- Audience: people who start a FerrionOS virtual machine on their own desktop. First developers and the agents working for them, later people who expect a system as easy as macOS or Windows and who play games.
- Character: iron as a material. Small, hard, exact, plain. No forge, no rust, no sparks.
- Must be drawn at 16 pixels first, in one flat color, on whole pixels, and stay the same drawing at every larger size.
- Must exist in the text console as plain ASCII.
- Must not resemble the upstream logo, the Rust gear, Fedora's f, Facebook's f or Firefox, or use anything another operating system or another project named for iron could also wear.

Paths dropped on the way:

- Two identical hooks, with both arms 4 units: the first drawing of the mark. At 16 pixels a vision model read it as two shapes as often as a letter.
- The F left whole, in any proportion: it reads as an F and belongs to nobody. Fortnite's F scores 0.73 against it in the screening below.
- The F with its middle arm floating off a whole stem, the third candidate: the quietest of the three at 16 pixels, and mirrored the nearest to Ferrous Systems' mark (0.74).
- The stem with both arms loose: every stencil alphabet has it. Nearest: PagerDuty 0.70, Square Enix 0.64.
- The F over a short bar, after the ridge on the F key: a letter over a bar is the text-color control, and the bar sits where an E's third arm would.
- The F on a key cap: it is the picture of "press F".
- A dot for the middle arm, or beside the letter: Figma's lozenges and the loose square in Ferrous Systems' mark.
- The foot of the stem cut off as a dot: an exclamation mark, which is Pop!_OS's device.
- The top arm cut off: Fila's F.
- A lowercase f, or any f in a disc or a tile: Fedora and Facebook. A block F filling a square: Flipboard.
- The [F-pentomino](https://en.wikipedia.org/wiki/Pentomino), five cells that one naming calls F and another calls R: its message is the hidden part, and as a shape it reads as a lowercase f and as a game piece.
- Fe, with or without an element tile or a charge: any project named for iron could wear it.
- An atom or an ion diagram: too near the upstream logo, and ReactOS wears an atom too.
- Iron objects (anvil, ingot, beam, magnet): metal puns are the category metaphor of the Rust ecosystem.
- Gear, shield, seed, nested squares for a virtual machine, a terminal prompt, a power symbol from the "io" in the name: category metaphors or working controls.
- The name stacked three by three, since it has nine letters: unreadable at 16 pixels.
- A hue: blue is the old login screen, Fedora and Facebook; orange is Firefox and Ubuntu; green, amber and red mean state in the system the mark sits in.

### Lookalike check

What it covered:

- 3,474 marks: all 3,472 icons of [Simple Icons](https://www.npmjs.com/package/simple-icons) 16.38.0 (tarball sha256 `40f6282307c399d6feb87678e92ad4cbe469585539eb13d4bab6a430aabc01f4`), which include the five marks the brief names, plus Ferrous Systems' mark and a drawn four-pane square, since Windows is not in that set. 145 of them have names that start with F, and a hand-picked 63 are operating systems, platforms or Rust projects.
- Method: each mark is rendered in black, cropped to its ink and fitted in a 48-pixel box on a 64-pixel canvas. The score is the mean of the correlation of the two images after a 1.6-pixel blur and the overlap of their ink. Identical marks score 1.00.
- Scale: pairs that are known to be alike score 0.75 to 0.83 (Firefox and Firefox Browser 0.75, Fedora and Facebook 0.81, Formbricks and Formspree 0.83). Random pairs of unrelated marks have a median of 0.42, a 95th percentile of 0.67 and a 99th of 0.78.
- Every sheet of nearest neighbors was also read by eye, as drawn and mirrored.
- The name neighbors found by a web search: [Ferrous Systems](https://ferrous-systems.com/), [Ferrocene](https://ferrocene.dev/), [Ferron](https://ferron.sh/), and an unrelated simulation app for Mac and iPad that is also named [Ferrion](https://ferrionapp.com/), whose icon is an illustrated airfoil.

The mark's scores:

|Against|Score|
|---|---|
|Nearest of 3,474|Paddle 0.63, Picsart 0.59|
|Nearest brand starting with F|Formbricks 0.53|
|Nearest operating system, platform or Rust mark|Gentoo 0.49|
|The upstream logo|0.17|
|Rust|0.40|
|Fedora|0.38|
|Facebook|0.39|
|Firefox, two versions|0.40, 0.26|
|Ferrous Systems, as drawn|0.39|
|Ferrous Systems, with our mark mirrored|0.69|
|Ferron|0.32|
|Ferrocene|0.46|

The mark resembles none of the five marks the brief names.

Ferrous Systems is the neighbor to know about, and the maintainer has not ruled on it:

- Its mark is a backwards F in three pieces. Any capital F is its nearest neighbor once mirrored (a plain one scores 0.71), and ours is an F in pieces too.
- Its wordmark is set in Fira Sans, the face of ours, and both names begin with Ferr.
- What differs: our mark faces the reading direction and theirs the other way, ours is two pieces and theirs three with a loose square, their name is lowercase beside the mark and ours mixed case under it.
- If it has to move further away, the first thing to change is the face of the name.

What it did not cover:

- A reverse-image search: the services reachable at the time needed each image at a public address, and the candidates were not public then.
- Trademark registers and app stores.
- People. A small vision model, asked once what the 16-pixel mark reads as, answered "capital F" with 0.62 of its weight and "two shapes" with 0.29. A test with people who have not seen the mark is still owed.
- The lockup as a whole. Only the mark and the name were screened, each alone.

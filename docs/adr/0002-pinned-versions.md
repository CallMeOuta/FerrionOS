# A pin is a hash; its version is read from upstream, and a held-back pin says why here

Status: accepted

## Context

FerrionOS pins its inputs (AGENTS.md "Security"): a git recipe has `rev`, a tar recipe has `blake3`, a download in a makefile or script is checked against a sha256, and a GitHub Action is used at a commit. Beside many pins sat a comment with the version the hash stands for (`# v7.0.1`, `# release v1.29.3`, `# The branch's head when pinned.`). Such a label is a second record of the version: nothing checks it, and it drifts from the hash the first time someone moves the pin and forgets it.

## Decision

- A pin is the commit hash or the checksum, and nothing else. The version it stands for is read from upstream: the tag or release that points at the commit, or the release page that publishes the checksum. No comment, ADR or document repeats it.
- To move a pin, take the new version's commit or checksum from upstream's tag or release page, and change the hash and any version field together, in one commit that names the version in its body. Where the version is part of a download's name (mdBook in `.github/workflows/book.yml`, Cloud Hypervisor in `mk/ch.mk`), the version variable and its sha256 change together; GitHub shows each asset's sha256 beside it on the release page.
- A pin held back on purpose is listed below with the reason, so that nobody moves it by mistake. The list names the recipe, not the version.

### Pins held back on purpose

- `recipes/dev/rustpython`: newer revisions require `bits/libc-header-start.h` for bindgen.
- `recipes/wip/games/open-world/asciicker`: the newer release line pulls in an unnecessary V8 JavaScript dependency.
- `recipes/wip/gnome/mutter`: the pinned release series is the last with the logind and udev feature flags.
- `recipes/wip/libs/gnome/gtksourceview`: later versions use GTK 4.
- `recipes/libs/librsvg`: newer versions need cargo-c to build.
- `recipes/wip/libs/other/libusb`: later versions make libudev mandatory.
- `recipes/wip/libs/other/xxhash`: the pinned development snapshot is the one with CMake support.
- `recipes/wip/net/other/bind9`: the pinned version is the last before IPv6 became mandatory.
- `recipes/wip/time/sowon`: newer commits removed the SDL2 front end.

## Consequences

- A reviewer checks a moved pin against upstream's tag, not against a comment.
- A reason for holding a pin back lives here and is deleted here when the reason is gone.

## Where

- `src/recipe.rs`: `SourceRecipe` (`rev`, `blake3`), described in [0005](0005-recipe-schema.md)
- `mk/ch.mk`: `CH_VERSION`, `CH_FIRMWARE_VERSION`, `ch_download`
- `.github/workflows/book.yml`: `MDBOOK_VERSION`, `MDBOOK_SHA256`, the `uses:` lines
- The `[source]` of each recipe listed above

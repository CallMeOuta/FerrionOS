# The cookbook reads git state itself and defaults its mirrors

Status: accepted

## Context

The cookbook fetches every recipe's source, decides whether a checkout is current, and fetches prebuilt packages from a remote package source. It runs on Linux, macOS, FreeBSD and FerrionOS, where a full git library is not always available, and its terminal interface shares the screen with the progress output of these fetches.

## Decision

### Git revisions are read from the repository's files

- `get_git_head_rev` returns the commit `HEAD` points at and whether `HEAD` is detached.
- `get_git_tag_rev` accepts a `rev` that is either a full commit hash or a tag name, and resolves a tag to its commit.
- `get_git_ref_entry` resolves a reference the way git stores it: a loose file under `refs/`, and otherwise its line in `packed-refs` (git's own maintenance packs references into that file).
- `GitRemoteTracking` keeps four separate facts: the local branch (from `.git/HEAD`), the branch it merges (`[branch <name>].merge` in `.git/config`), its remote (`[branch <name>].remote`, usually `origin`), and the remote's default branch (`refs/remotes/<remote>/HEAD`), with the remote's URL. The default branch is kept apart from the tracked branch because the two differ when a recipe names a `branch`.

### An absent hash input is an empty string

`get_combined_blake3` gives an input with no hash an empty identity, because pkgutils accepts an empty string where a hash field is omitted.

### Mirrors

- When `cookbook.toml` configures no mirrors, `init_config` maps `ftp.gnu.org/gnu` to a GNU mirror, so GNU tarballs download from a mirror by default. A `cookbook.toml` with its own `mirrors` table replaces that default entirely.
- `REMOTE_PKG_SOURCE` is the default package source for recipes whose build type is `remote`; the same mirror table can redirect it.

### Progress output

- `init_binary_repo` resets the repository manager's callbacks before handing it out, so its progress output does not write over the pty the terminal interface draws.
- `fetch_package_name` appends the package name after the progress counters that `fetch_package_increment` printed.
- `download_increment` always reports in megabytes, so successive lines are comparable.

## Consequences

- A git feature the readers do not cover (another reference storage, a worktree's `.git` file) makes the cookbook misjudge a checkout; extend these functions rather than shelling out per host.
- Changing the default mirror changes where every GNU recipe downloads from on machines without a `cookbook.toml`.

## Where

- `src/cook/fs.rs`: `get_git_head_rev`, `get_git_tag_rev`, `get_git_ref_entry`, `GitRemoteTracking`, `get_combined_blake3`
- `src/config.rs`: `init_config`, the default `mirrors` entry
- `src/lib.rs`: `REMOTE_PKG_SOURCE`
- `src/cook/fetch_repo.rs`: `init_binary_repo`, `fetch_package_name`, `download_increment`

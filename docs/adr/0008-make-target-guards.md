# Make targets keep the container and the cookbook from rebuilding needlessly

Status: accepted

## Context

Most targets run twice: once on the host, which starts the container, and once inside it ([`agents/skills/build-scripts.md`](../../agents/skills/build-scripts.md)). Both the container and the cookbook are rebuilt when their tag files look older than their inputs, and some hosts (Nix, CI) manage the build tools themselves.

## Decision

- `NOT_ON_PODMAN` marks a run where executing the cookbook binary is not safe, because the container that built it is absent; the clean targets in `Makefile` check it before calling the cookbook.
- `SKIP_CHECK_TOOLS=1` skips the host tool checks of `mk/depends.mk`, for hosts whose tools are managed outside the build, such as a Nix shell or CI.
- `bc.%` with `REPO_BINARY=0` set explicitly, and `sc.%` with `REPO_BINARY=1`, change only the named recipes' rule (`change-rule-local`), so their dependencies are not switched as well. An empty `REPO_BINARY` changes the dependencies too.
- `repo-rollback.%` touches the container tag and the cookbook tag after checking out the old commit; otherwise the older files look newer than the tags and Podman and the cookbook rebuild.
- `KPROF_OPTIONS` takes the single-letter options of the kprofiling component (listed in its `src/main.rs`, recipe `redox-kprofiling`); `xo`, for example, shows instruction offsets. `KPROF_FLAMEGRAPH_OPTIONS` is passed to the flame graph tool, for example `--reverse`.
- A line that calls `$(error ...)` inside an indented conditional of `mk/virtiofs.mk` starts at column one, because make reads a line that starts with a tab as a recipe line.

## Consequences

Dropping a `touch` after a rollback costs a full container or cookbook rebuild; indenting the `$(error ...)` line makes make reject the file.

## Where

- `Makefile`: `NOT_ON_PODMAN`, the `clean` and `distclean` targets, `KPROF_OPTIONS`, `KPROF_FLAMEGRAPH_OPTIONS`
- `mk/depends.mk`: `SKIP_CHECK_TOOLS`
- `mk/repo.mk`: `bc.%`, `sc.%`, `repo-rollback.%`
- `mk/virtiofs.mk`: the `share` parsing block

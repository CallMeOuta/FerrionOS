# FerrionOS's core components move into this repository

Status: accepted; migration planned, not yet done

## Context

FerrionOS changes its core components (the kernel, relibc, base and RedoxFS) through stacks of patch files beside their recipes, applied at build time to sources fetched from upstream's repositories. On 2026-11-21 master's relibc carried twelve such patches, base nineteen, and the kernel and RedoxFS seven each, with more waiting in lanes. Every lane that touches a component edits the same patch list, a patch cannot be refactored without regenerating it, simulation tests (ADR 0025) need the source as ordinary files, and a change across two components cannot be one commit. The maintainer decided on 2026-11-21, from Paradigm's "Effective Swarm Riding for Engineers" ("monorepo everything"), to bring these components into this repository.

## Decision

- The kernel, relibc, base and RedoxFS live as source trees in this repository, at the upstream revisions their recipes pin today, with FerrionOS's patch stacks applied as ordinary commits. Their recipes build from those trees instead of fetching.
- Each tree records the upstream repository and revision it started from, keeps upstream's license files and notices, and names its source as AGENTS.md requires.
- Bringing in upstream's later changes is a deliberate merge of a new upstream revision into the tree, recorded with that revision, not a refetch.
- The layout, the order of the move, the cutover of in-flight lanes and the way back are set by the migration plan before any tree moves.

## Consequences

- A change to a component is an edit and a commit; patch lists and their ordering conflicts disappear for these four components.
- The repository grows by these components' sources.
- Until the migration runs, the patch-stack rules stay in force.

## Where

- `plans/monorepo.md` (on the maintainer's machine) until the migration lands; then the trees, their recipes and `docs/upstream.md`

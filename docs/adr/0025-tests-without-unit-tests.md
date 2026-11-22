# Changes are proven by VM checks, differential tests and simulation, not unit tests

Status: accepted

## Context

Coding agents write most of FerrionOS's code and, left alone, also write unit tests that pin what they just wrote: they pass by construction, slow every later change and catch little. The bugs that cost FerrionOS time were found by booting an image (`scripts/vm-test.py`), by comparing against a reference system (the Linux layer's output against real Linux, Firefox's glyphs against the host's rendering), or would have been found by running a component under injected faults (RedoxFS's cleanup at generation 1,024, its directory times, its directory `fsync`). The maintainer decided on 2026-11-21, from Paradigm's "Effective Swarm Riding for Engineers", to remove the unit tests agents wrote and to stop adding them.

## Decision

- An agent writes no unit tests. A change is proven by one of three kinds of check:
  - a VM check: `scripts/vm-test.py` boots an image and checks behavior from outside, red before the change and green after;
  - a differential test: the same input run on FerrionOS and on a reference (real Linux in Podman, the host's own tools, a model of the expected state), with every difference a failure;
  - a deterministic simulation test: the component runs against simulated time, disk and scheduling driven by a seed, with faults injected and invariants checked, and a failing seed is kept as a regression seed.
- Unit tests that agents wrote are removed. Where one guarded a real fix, the fix is covered by one of the three kinds above, or an issue records the gap.
- Tests that came with fetched components or with code brought in from another project stay; FerrionOS does not edit them.
- A probe under `recipes/tests/` is part of a VM check, not a unit test.

## Consequences

- `docs/testing.md` orders the checks by cost; a host-only unit test is no longer a rung.
- Simulation harnesses become first-class code with their own seeds and budgets, starting with RedoxFS.
- Code review asks which of the three kinds proves a change, and what it was red on.

## Where

- AGENTS.md, Rules; [`docs/testing.md`](../testing.md)
- `scripts/vm-test.py`; the fixtures recorded on Linux under `recipes/tests/linux-*-fixtures/`; simulation harnesses as they land

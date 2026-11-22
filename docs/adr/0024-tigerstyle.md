# Code FerrionOS owns follows TigerStyle

Status: accepted

## Context

FerrionOS's own code (services such as `fdobjd`, `ferrion-audiod`, `linux-sessiond`, the Linux runtime `linux-run`, the cookbook, and the patches it carries on fetched components) is written mostly by coding agents working in parallel, and its failures so far were limits nobody stated: a descriptor-closing loop that ran to `RLIM_INFINITY`, an allocator cleanup that wrote on a read-only disk once every 1,024 generations, a wake-up lost across `fork()`. The maintainer adopted TigerStyle (https://tigerstyle.dev/, TigerBeetle's style) on 2026-11-21, from Paradigm's "Effective Swarm Riding for Engineers".

## Decision

- Every loop, queue, buffer, table and retry in code FerrionOS owns has an explicit upper bound, stated as a named constant, and what happens at the bound is a defined error, never growth.
- Invariants are asserted where they are relied on: function arguments and results, the states a state machine may take, the relation between two fields. An assertion that fails stops the program; it is never caught to continue.
- A long-running service allocates what it needs at start or per request from a bounded pool, so one client cannot grow it without limit.
- Control flow stays simple: no recursion in services, no unbounded iteration over input a caller controls, and every blocking wait has a timeout or a cancellation path.
- States that cannot occur are made unrepresentable in the types where the language allows it.
- Fetched components keep their own style; FerrionOS's patches to them follow these rules in the code they add.

## Consequences

- New code carries more assertions and more named limits, and its first failures are assertion stops with a location instead of silent growth or hangs.
- Deterministic simulation tests (ADR 0025) find more, because assertions turn a wrong state into a stop the simulator records with its seed.
- Existing code is brought to these rules when it is changed, not in one sweep.

## Where

- AGENTS.md, Rules
- Code FerrionOS owns: `src/`, `recipes/**/crate/`, the `ferrion-*.patch` files

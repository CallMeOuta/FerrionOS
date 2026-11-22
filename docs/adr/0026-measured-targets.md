# Work is stated as a measured target

Status: accepted

## Context

Tasks phrased as "optimize X" or "fix the bugs" end when the worker decides they are done. Tasks phrased as a number to reach or a reference to match end when the number says so, and keep going until then. The maintainer adopted this on 2026-11-21, from Paradigm's "Effective Swarm Riding for Engineers" ("do not say optimize X, say make this 10x faster"; "do not say fix all bugs, say differentially test this and close all gaps").

## Decision

- A task names its measure and its target: a time, a size, a rate, or a reference to match exactly. Examples: "make Firefox's cold start 10x faster, measured by `vm-test.py firefox-start`"; "differentially test `linux-run` against Linux on the LC fixtures and close every gap"; "run RedoxFS under simulation until 10 million operations across 1,000 seeds find no invariant failure".
- The measure is taken before the change and after it with the same command, and both numbers go into the commit message.
- A task that cannot name a measure names the check whose red-to-green defines done.

## Consequences

- Performance and correctness work produce numbers in the history, and a regression shows as a number that moved.
- `docs/vm-baseline.md` keeps the reference numbers that targets are stated against.

## Where

- AGENTS.md, Rules; [`docs/vm-baseline.md`](../vm-baseline.md)

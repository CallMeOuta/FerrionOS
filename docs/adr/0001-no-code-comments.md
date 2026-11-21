# Code carries no comments; decisions live in ADRs and debts in issues

Status: accepted

## Context

The build system FerrionOS forked carried thousands of comments: status lines in recipes (`#TODO not compiled or tested`), banners and restatements of the code, commented-out code, notes about code that had since changed, and, among them, records of real decisions. A comment is not checked by anything, so it drifts from the code; a `TODO` or a "workaround for" lets a shortcut stand and be copied into the next recipe. The maintainer decided on 2026-11-21 to delete the comments and keep what they knew elsewhere.

## Decision

- Code carries no comments. Each comment found is one of four kinds, and goes where its kind belongs:
  - a tautology (it repeats what the code shows) is deleted;
  - a stale comment (it describes code or facts that no longer hold, or is commented-out code) is deleted;
  - an excuse (it lets a hack, a shortcut, or a broken or unverified state stand) is deleted, and the defect becomes an issue that names the file and what to fix;
  - a decision record (it explains an important, non-obvious choice) moves into an ADR in `docs/adr/`, written to stand on its own.
- Some comments stay, because a rule or a tool needs the comment itself: legal and license notices, and the source notice of code brought in from another project; directives a tool reads (a shebang, `shellcheck`, `noqa`); and the comments AGENTS.md requires: the documentation of make targets in `Makefile` and `mk/*.mk` (a `##` line, or the comment block right above a target), of build variables in `mk/config.mk` and of emulator options in `mk/qemu.mk` and `mk/virtiofs.mk`, and the reason for a `mode` with a setuid, setgid or world-write bit. `scripts/comment-check.config.json` names each kind.
- `scripts/comment-check.py` enforces the rule with a ratchet. `scripts/comment-check.baseline.json` holds, per tracked file, how many comment lines it may have; the check fails when a file has more. The counts only go down: after comments are deleted, `python3 scripts/comment-check.py --update` lowers them, and it refuses to raise any. A file whose comments are all required counts zero.
- Where a script needs to tell its user something, it says it in its usage message, not in a comment.

## Consequences

- A new comment fails the check unless it is one of the required kinds.
- Knowledge that used to sit beside the code is in an ADR or an issue; reading an area starts with its ADRs ([README](README.md)).
- Generated files keep whatever comments their generator writes until the generator changes; the baseline counts them.
- The first sweep left some files alone (other lanes' work in progress, vendored code, generated files); their comments stay in the baseline for a later run.

## Where

- `scripts/comment-check.py`, `scripts/comment_extract.py`, `scripts/comment-check.config.json`, `scripts/comment-check.baseline.json`
- AGENTS.md, Rules; [`docs/testing.md`](../testing.md), rung 1

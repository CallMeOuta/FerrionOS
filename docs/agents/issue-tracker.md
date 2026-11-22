# Issue tracker: local Markdown

Issues and specs for this repository live as Markdown files in `.scratch/` at the repository root. `.scratch/` is git-ignored, like `plans/`: its files stay on the maintainer's machine and are never committed or pushed. Older issues remain in the GitHub repository's Issues; `gh` in this checkout targets `CallMeOuta/FerrionOS`.

## Conventions

- One feature per directory: `.scratch/<feature-slug>/`.
- The spec is `.scratch/<feature-slug>/spec.md`.
- Implementation issues are one file per ticket at `.scratch/<feature-slug>/issues/<NN>-<slug>.md`, numbered from `01`, never a single combined tickets file.
- Triage state is a `Status:` line near the top of each issue file, holding one of the role strings in `triage-labels.md`.
- Comments and conversation history are appended at the bottom of the file under a `## Comments` heading.

## Decision inbox

Run `python3 scripts/inbox.py` for one pending decision at a time. Enter accepts the preselected recommendation; a number selects another option immediately; `s` skips this session; `o` opens the source with `VISUAL`, `EDITOR` or `vi`; `q` quits. No confirmation step is needed. The remaining count includes skipped decisions. Piped input uses one action per line, with an empty line accepting the recommendation; end of input quits without answering.

`--list` prints the oldest-first queue and `--json` returns the same queue as a JSON array for agents. `--root PATH` selects a different repository root, including a temporary fixture. Each JSON item includes its source path and line, kind, creation time, question, context, numbered options, recommendation and any format error. Listing never changes files.

The inbox collects `.scratch/**/issues/*.md` with `Status: ready-for-human` or `Status: needs-info` and a `## Decision` section. Only ask for a decision an agent cannot derive from the existing requirements. Technical diagnosis and already specified fixes stay `ready-for-agent`; do not invent choices to fill the inbox.

Use this format, with a single status in the header, an optional ISO 8601 creation date or timestamp, one question, consecutive single-digit options and exactly one `[recommended]` marker. `Context:` is one paragraph on one line. Put the consequences of each choice in the options or context. An option containing the word `wontfix` closes the issue instead of handing it back to an agent.

```markdown
# Name the blocked work

Status: ready-for-human
Created: 2026-11-21

## Decision

Question: Should new accounts receive disk-unlock access automatically?

1. Require explicit enrollment. [recommended]
2. Enroll every new account.
3. Keep the current behavior. [wontfix]

Context: Creating a login account does not currently grant disk-unlock access. Explicit enrollment keeps the two permissions separate; automatic enrollment makes every account password a disk-unlock credential.
```

Creation time determines queue order; without `Created:`, the file modification time is used. Dates without a timezone are UTC. Path and source line break ties. Add `Created:` when first raising a question so subsequent edits do not change its age.

Under `## Notes` in `build/LANES.md`, a line starting with `question to the maintainer:` (optionally preceded by `- `) also enters the inbox. Put its creation date, context and numbered options on immediately following lines indented by two spaces:

```markdown
- question to the maintainer: Should this lane become the default?
  Created: 2026-11-21
  Context: The optional lane passes its functional check but has no quiet-host timing measurement yet.
  1. Keep it optional until the timing check passes. [recommended]
  2. Make it the default now.
```

Malformed decisions and bare Notes questions remain visible but cannot be answered until their author adds valid options. Open their source or skip them; the inbox never invents a recommendation.

Every answer is written with its UTC date, question, exact selected option and resulting status under `## Comments` in the source file. Issue status becomes `ready-for-agent`, or `wontfix` for that option. A Notes question becomes `answered by the maintainer:` with an indented `Status:` so it leaves the queue; its answer is also kept under `## Comments`. The issue's earlier acceptance text is background: the selected decision governs the agent's next work.

Saving checks that the displayed file has not changed, writes and syncs a temporary file beside it, replaces the source atomically and syncs its directory before reporting success or advancing. Inbox writers serialize through a directory lock; external editors must not write the file during the save. A stale file or write failure stops the session instead of claiming an answer. Opening an editor reloads the queue. Skipping, quitting, invalid keys and end of input never record an answer. Limits and their failure messages live in `scripts/inbox.py`; no partial queue is silently returned at a scan limit.

The acceptance target is command-to-first-decision latency below 0.2 seconds with 100 issues, with every accepted answer persisted before the next decision. Run `python3 scripts/inbox-session.py` for the scripted session proof: it runs the actual CLI against a temporary `.scratch` copy, compares saved files with three selected answers, checks read-only actions and stale/write-failure refusal, and leaves the real queue unchanged. It uses the two pending one-password questions as migration fixtures; run it before those questions are answered in the real queue. On 2026-11-21, the final session's first decision appeared in 0.063245 seconds with 100 issues and all three answers were saved; before this change no inbox command existed.

## When a skill says "publish to the issue tracker"

Create a new file under `.scratch/<feature-slug>/`, creating the directory if needed.

## When a skill says "fetch the relevant ticket"

Read the file at the referenced path. The maintainer normally passes the path or the issue number directly.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a file with one **child** file per ticket.

- **Map**: `.scratch/<effort>/map.md` (the Notes, Decisions-so-far and Fog body).
- **Child ticket**: `.scratch/<effort>/issues/NN-<slug>.md`, numbered from `01`, with the question in the body. A `Type:` line records the ticket type (`research`, `prototype`, `grilling` or `task`); a `Status:` line records `claimed` or `resolved`.
- **Blocking**: a `Blocked by: NN, NN` line near the top. A ticket is unblocked when every file it lists is `resolved`.
- **Frontier**: scan `.scratch/<effort>/issues/` for files that are open, unblocked and unclaimed; the lowest number wins.
- **Claim**: set `Status: claimed` and save before any work.
- **Resolve**: append the answer under an `## Answer` heading, set `Status: resolved`, then append a context pointer (gist and link) to the map's Decisions-so-far in `map.md`.

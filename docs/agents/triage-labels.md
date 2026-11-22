# Triage labels

The skills speak in terms of five canonical triage roles. This file maps each role to the string this repository's issue tracker uses for it, written as the `Status:` line of an issue file under `.scratch/`.

| Role in the skills | String in this tracker | Meaning |
|---|---|---|
| `needs-triage` | `needs-triage` | The maintainer needs to evaluate this issue |
| `needs-info` | `needs-info` | Waiting on the reporter for more information |
| `ready-for-agent` | `ready-for-agent` | Fully specified, ready for an agent working alone |
| `ready-for-human` | `ready-for-human` | Requires a maintainer decision or human implementation |
| `wontfix` | `wontfix` | Will not be acted on |

When a skill names a role (for example "apply the AFK-ready triage label"), write the corresponding string from this table as the issue's `Status:` line. To rename a role, edit the second column.

## Decisions awaiting the maintainer

When work needs a concrete maintainer choice, set `ready-for-human` and add the question, numbered options, exactly one `[recommended]` option and a one-paragraph `Context:` under `## Decision`. Use `needs-info` instead when the missing choice is information the maintainer must supply. The exact format and Notes convention are in [issue-tracker.md](issue-tracker.md#decision-inbox).

`python3 scripts/inbox.py` displays those issues and pending Notes questions one at a time, oldest first. `--list` is the readable queue; `--json` is the agent interface. Issues without a `## Decision` section do not enter this queue, and technical work with settled requirements belongs in `ready-for-agent`, not in a fabricated human decision.

An inbox answer appends a dated record under `## Comments` and changes the status to `ready-for-agent`; choosing an option containing `wontfix` sets `wontfix`. A skip or quit changes neither status nor comments. Agents read the recorded answer before implementing; it supersedes any earlier proposal that disagrees.

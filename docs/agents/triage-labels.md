# Triage labels

The skills speak in terms of five canonical triage roles. This file maps each role to the string this repository's issue tracker uses for it, written as the `Status:` line of an issue file under `.scratch/`.

| Role in the skills | String in this tracker | Meaning |
|---|---|---|
| `needs-triage` | `needs-triage` | The maintainer needs to evaluate this issue |
| `needs-info` | `needs-info` | Waiting on the reporter for more information |
| `ready-for-agent` | `ready-for-agent` | Fully specified, ready for an agent working alone |
| `ready-for-human` | `ready-for-human` | Requires a person to implement |
| `wontfix` | `wontfix` | Will not be acted on |

When a skill names a role (for example "apply the AFK-ready triage label"), write the corresponding string from this table as the issue's `Status:` line. To rename a role, edit the second column.

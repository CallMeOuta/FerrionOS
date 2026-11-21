# Code Policy

How changes to FerrionOS are made. It applies to the maintainers and to every coding agent working for them; [AGENTS.md](../AGENTS.md) holds the rules specific to this repository.

## Testing policy

A change comes with the cheapest check that can fail for it, as [testing.md](testing.md) orders them, and its report names the highest check reached. A bug fix adds a test when no existing test covers the path; a change to what an image does is proven by booting that image, with `scripts/vm-test.py` where a test exists.

## Human language policy

The [human language policy](../CONTRIBUTING.md#human-language-policy) applies to user-facing strings, code, comments, documentation, and commit messages.

## Commits

**Do:**

* Split changes into separate, atomic commits: one commit per feature or fix, with the build and the system working after each.
* Keep commits rebased on `master`. The history has no merge commits.
* Write the subject line as `Category: Brief description of what is being changed`, in the imperative mood ("kernel: Log the FerrionOS name at boot", not "kernel: Logged ..."), with a capital letter after the category and no period at the end.
* Pick the category from where the change is:
  * a recipe's name for changes under its recipe directory: `kernel`, `bootloader`, `base`, `userutils`, `fish-shell`
  * `config` for `config/`, `cookbook` for `src/`, `mk` for `Makefile` and `mk/`, `scripts`, `podman`
  * `docs` for `docs/` and the root documents, `book` for `book/`, `agents` for `AGENTS.md`, `CLAUDE.md` and `agents/`, `CI` for `.github/`, `branding`, and `Meta` for the repository as a whole
  * several categories joined with `+`, for example `kernel+bootloader: ...`
* Leave an empty line after the subject, and wrap the body at 72 characters. A URL may stand alone on a longer line.
* Explain in the body what changed and why, and which check proves it.
* Amend existing commits when adding changes after a review, where relevant.
* Run images through `optipng -strip all` before committing them.

**Do not:**

* Introduce changes that are incompatible with the project license (MIT), or bring in code without its license notice.
* Touch anything outside the stated scope of the change.
* Use weasel words such as "refactor" or "fix" to avoid explaining what is being changed.
* Include commented-out code.
* Add a `Signed-off-by` line.
* Add jokes or other "funny" things to user-facing parts of the system.

**Reusing reference work from an issue:** check its license first. MIT work is reused with its author credited, as `--author` when the commit is mostly theirs or a `Co-authored-by:` line otherwise, using the name the issue gives; work under another license is reused only as far as that license allows, and its notice comes with it. The commit body links the issue.

`scripts/lint-commit-message.py` checks the format rules above. Install it as a commit hook with:

```sh
printf '#!/usr/bin/env bash\nexec python3 scripts/lint-commit-message.py "$1"\n' > .git/hooks/commit-msg
chmod +x .git/hooks/commit-msg
```

## Use of AI and coding agents

FerrionOS is developed with AI assistance and coding agents. The maintainer who introduces a change is responsible for it, whoever or whatever wrote it, and it meets the same standard as carefully written human work.

* Agents start from [AGENTS.md](../AGENTS.md) and prove each change with the checks in [testing.md](testing.md); a change is not done until its check has run.
* Do not use AI-generated text to respond to issues or take part in project discussion without holding it to the same standard as human-written text.
* Do not use AI-generated descriptions or summaries as a substitute for understanding the relevant code, issue, or discussion.

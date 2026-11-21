# Participating in FerrionOS

FerrionOS is developed by its maintainers, who introduce all changes to the codebase. The project does not accept pull requests from non-maintainers.

Your involvement still matters, and these all move the project forward:

* Clear bug reports with reduced test cases, and testing on virtual machines and hardware.
* Suggestions for features, designs and improvements.
* Reference work: something you built in your own fork or project, linked from an issue for the maintainers to study and use as a reference.
* Documentation improvements: a page that is wrong, unclear or missing, with your proposed text or a link to it.
* Security reports, through [SECURITY.md](SECURITY.md).

Read [AGENTS.md](AGENTS.md) and the documents it points to, and [the FerrionOS Book](book/src/SUMMARY.md), to understand how FerrionOS is built and how it works.

## Communication

FerrionOS has no chat server yet. Bugs, suggestions and documentation improvements go to the [issue tracker](https://github.com/CallMeOuta/FerrionOS/issues); security reports go through [SECURITY.md](SECURITY.md).

## Issue policy

Please file issues keeping the following in mind:

* One issue per bug or suggestion. Putting multiple things in the same issue makes both discussion and completion unnecessarily complicated.
* Use the form that fits: "Report a bug" follows the [detailed issue-reporting guidelines](ISSUES.md); "Suggest an improvement" covers features, designs and documentation.
* No support requests. A failed build is a bug when it fails from a fresh clone of `master` on a supported host with the documented commands; report it with the whole log. Questions about setting up a host are not issues.
* Do not comment on issues just to add a joke or irrelevant commentary. Everyone watching an issue is notified of each comment, so keep comments relevant.
* Never report a security weakness in a public issue: follow [SECURITY.md](SECURITY.md).

## Reference work

An issue may link to work of your own, such as a branch, a commit or a patch in your fork, as a reference for the maintainers. The maintainers read it, and may reuse it, adapt it or write their own version; they do not merge it as it is.

* Link to a fixed revision (a commit or a tag, not only a branch name), so the reference does not change after it is read.
* Say what the work does, what you tested, and what is unfinished.
* State its license. Work offered under the MIT license, as FerrionOS is, can be reused directly; work under another license is reused only as far as that license allows. Never link code you do not have the right to share.
* When the maintainers reuse your work, the commit names you as its author or co-author, as you ask in the issue.

## Project governance

FerrionOS is an independent project led by its maintainer, who decides what the project builds and which changes it accepts.

## Human language policy

In FerrionOS, we treat human language as seriously as we do programming language.

The following applies to all user-facing strings, code, comments, documentation, and commit messages:

* The official project language is American English with ISO 8601 dates and metric units.
* Use proper spelling, grammar, and punctuation.
* Write in an authoritative and technical tone.
* Avoid contractions, slang, and idioms.
* Avoid humor, sarcasm, and other forms of non-literal language.
* Use gender-neutral pronouns, except when referring to a specific person.

This also applies to debug logging and other internal strings, as they may be exposed to users in the future.

## On neutrality

Our goal is to build an operating system for everyone, irrespective of their specific opinions and worldviews. To achieve this, we set our differences aside and focus on the shared goal of building the system.

This means:

* We welcome involvement from anyone who is committed to the project's goals and respects the collaborative environment.
* Discussion and work in project spaces remain strictly related to operating system development. The project takes no positions on issues outside this scope.
* The project is not a platform to advertise or promote causes unrelated to operating system development.
* Discussions of societal politics and other divisive topics are discouraged in project spaces.

## On bad-faith contributions and brigading

We reserve the right to reject issues and other interactions that appear to be motivated by bad faith. Anyone found participating in social media brigading of FerrionOS will be permanently banned from the project.

## How the maintainers work

[docs/code-policy.md](docs/code-policy.md) describes how changes are made: commit format, testing, the human language policy in code, and the use of AI and coding agents, which hold their output to the same standard as any other work.

FerrionOS is released under the MIT license in [LICENSE](LICENSE). Code brought in from another project keeps its license notice and names its source.

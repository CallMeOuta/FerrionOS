# Contributing to FerrionOS

Thank you for helping. FerrionOS's code, issues and pull requests live at [github.com/CallMeOuta/FerrionOS](https://github.com/CallMeOuta/FerrionOS).

## Code of conduct

We follow the [Rust Code of Conduct](https://www.rust-lang.org/policies/code-of-conduct).

## Issues

Report a bug, a failing build or a missing feature as an issue at [github.com/CallMeOuta/FerrionOS/issues](https://github.com/CallMeOuta/FerrionOS/issues). Say what you ran, on which host and architecture, and attach the whole log: `your-command 2>&1 | tee file-name.log` keeps one.

## Security weaknesses

Never report a security weakness in a public issue or pull request. Report it privately through GitHub's [private vulnerability reporting](https://github.com/CallMeOuta/FerrionOS/security/advisories/new), with the file and commit it is in. This holds for weaknesses in components the build fetches too.

## Pull requests

- Keep a pull request to one change, small enough to review. Split a big one when the parts still work on their own.
- Write a short title and say in the description what the change fixes or improves, and which checks you ran.
- Personal information stays out of the repository and its history, commit messages included.

## Contribution terms

When making a contribution you agree to the following terms:

- I understand these changes in full and will be able to respond to review comments.
- I have read the [Developer Certificate of Origin](https://developercertificate.org/) and certify my contribution under its conditions.

Your contributions are released under the MIT licence in [LICENSE](LICENSE). Code brought in from another project keeps its licence notice and names its source.

## Contributions made with AI

FerrionOS welcomes contributions written with LLMs and coding agents, under the same bar as any other: the person who submits a contribution answers for it, the contribution terms apply unchanged, and the change is verified the same way. Agents working in this repository start from [AGENTS.md](AGENTS.md).

## Verifying a change

- [AGENTS.md](AGENTS.md) routes each kind of change to its task guide in `agents/skills/` and to the reference in `docs/`, and holds the rules every change follows: security, naming, sources of truth.
- [docs/testing.md](docs/testing.md) lists the checks from cheapest to most expensive and what each proves. Run the cheapest one that can fail for your change, climb as far as the change needs, and name in the pull request the highest check you reached.
- A failure that also happens without your change is pre-existing: report it with its log and keep it out of your change.

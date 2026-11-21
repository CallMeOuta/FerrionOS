# Domain docs

How the engineering skills consume this repository's domain documentation when exploring the codebase. The layout is single-context.

## Before exploring, read these

- **`GLOSSARY.md`** at the repository root.
- **`docs/adr/`**: the ADRs that touch the area you are about to work in.

When either is missing, proceed silently: the `/domain-modeling` skill (reached through `/grill-with-docs` and `/improve-codebase-architecture`) creates them when a term or a decision is actually resolved, so their absence needs no mention and no upfront creation.

## File structure

```
/
├── GLOSSARY.md
├── docs/adr/
│   ├── 0001-<decision>.md
│   └── 0002-<decision>.md
└── src/
```

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactoring proposal, a hypothesis, a test name), use the term as `GLOSSARY.md` defines it, and keep to the glossary's term where it lists synonyms to avoid.

A concept missing from the glossary is a signal: either the language is invented and the project does not use it (reconsider it), or there is a real gap (note it for `/domain-modeling`).

## Flag ADR conflicts

When your output contradicts an existing ADR, say so explicitly instead of overriding it silently:

> _Contradicts ADR-0007 (the decision's title), but worth reopening because…_

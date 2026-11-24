# `redox_syscall`

System call numbers, flags, types, and inline assembly wrappers for the FerrionOS kernel interface. Derived from the upstream project at the revision recorded in [`components/upstream.toml`](../../upstream.toml), under the license in [`LICENSE`](LICENSE).

This library is intended primarily for `components/kernel` and `components/relibc`. Regular applications are strongly discouraged from calling it directly because the raw syscall ABI is unstable and changes frequently.

Built as part of `components/kernel` with `CI=1 COOKBOOK_LOGS=true make r.kernel`. See [`components/kernel/README.md`](../README.md) and [`agents/skills/component-sources.md`](../../../agents/skills/component-sources.md) for build and VM verification commands.

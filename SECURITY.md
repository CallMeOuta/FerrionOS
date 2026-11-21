# Security Policy

## Reporting a vulnerability

Report security issues in FerrionOS privately, through GitHub's private vulnerability reporting: on the repository's Security tab, choose "Report a vulnerability", or go directly to [the reporting form](https://github.com/CallMeOuta/FerrionOS/security/advisories/new). Do not report security issues as public GitHub issues or in other public places: a vulnerability disclosed before it is fixed can put people using FerrionOS at risk.

A useful report names the commit you tested, the architecture and config, and where it ran, and includes the smallest test case that reproduces the issue. If you found the issue with a fuzzer, include the input that triggers it and the build you ran it against. Make sure the issue reproduces on a current build of the `master` branch.

## Supported versions

FerrionOS has no releases yet. Security fixes go into the `master` branch.

## What happens after you report

The maintainers triage each report. A report they accept gets a security advisory. Once the issue is fixed, the advisory is published, unless other projects received the same report, in which case it is published when the longest agreed disclosure timeline ends. Researchers who report security issues privately are credited in the advisory.

The maintainers reserve the right to reject reports that are not security issues, or that are outside the scope of FerrionOS. A report determined not to be a security issue should be filed as a [GitHub issue](https://github.com/CallMeOuta/FerrionOS/issues/new?template=bug_report.yml) instead. If you do not re-report it, the maintainers may do so themselves.

FerrionOS does not offer bug bounties.

## Scope

Examples of issues in scope:

* Memory-safety bugs in the kernel, in drivers, or in system services that an unprivileged process or a device can trigger
* Escapes from a process's namespace, or access to a scheme that the process was not given
* Privilege escalation through `sudo`, `login`, `su`, or another program that runs with more privilege than its caller
* Bypasses of the boot chain's signature checks, once they exist
* Flaws in how packages are fetched, verified, or installed that let a third party change what gets installed
* Network-reachable flaws in the network stack or in services that an image starts by default

Weaknesses in components that FerrionOS builds from other projects are in scope as well: report them here, and the maintainers coordinate with the project that owns the code.

## Known limitations

FerrionOS is not hardened yet, and the following are known and planned work rather than vulnerabilities:

* The development images log in as `user` with an empty password and as `root` with the password `password` (`config/dev-accounts.toml`). They are meant for test machines only.
* Verified boot covers the kernel and initial filesystem only. In an image built with the default settings (`VERIFIED_BOOT=1`), the UEFI bootloader starts only a kernel and initial filesystem that match a manifest signed with that build's own development key, made on the build machine; whoever can write the disk can still replace the bootloader and its key, which only Secure Boot prevents. It does not yet cover the rest of the filesystem, live images, the BIOS bootloader (i586 images), or rollback to an older signed version. Secure Boot covers the bootloader binary only when it has been signed and its key enrolled.
* Kernel and user-space hardening (W^X enforcement in the kernel, stack canaries, address-space layout randomization in all programs) is incomplete.

Report a flaw in one of these areas only when it goes beyond the limitation as stated.

## Responsible disclosure

The maintainers work with security researchers to resolve security issues in a timely manner. A default 30-day disclosure timeline applies to all security issues, and it may be extended if the maintainers and the reporter agree that more time is needed. The maintainers keep the reporter informed of progress and resolution throughout the process.

When a security issue is also reported to other projects, the maintainers work with the longest disclosure timeline, so that all parties have enough time to resolve the issue.

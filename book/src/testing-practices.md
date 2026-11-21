# Testing

- [Choosing a Check](#choosing-a-check)
- [Checking a Boot From Outside](#checking-a-boot-from-outside)
- [The Security Audit](#the-security-audit)
- [Secure Boot](#secure-boot)
- [Measured Boot](#measured-boot)
- [Test Suites Inside the System](#test-suites-inside-the-system)
- [Running a Suite by Hand](#running-a-suite-by-hand)

Test the boot after every change to the system. Even a small change can stop the boot, and a change to one component can show up in another.

Before you change anything, check how the unmodified image behaves on your machine, so that you have something to compare with.

## Choosing a Check

The repository's [`docs/testing.md`](https://github.com/CallMeOuta/FerrionOS/blob/master/docs/testing.md) orders the checks from the cheapest to the most expensive and says what each one proves: whether a file parses, the cookbook's own tests, the dependency graph of a recipe or a configuration, a fetch, a cook, an image and its boot, and tests inside the running system. Climb only as far as your change needs, since a cook or an image build takes a long time.

## Checking a Boot From Outside

`scripts/vm-test.py` boots an image through `make qemu` and checks it the way a person would: it watches the screen, reads the serial console and types on it. Every run boots with QEMU's `-snapshot` option, so the image stays unchanged, and a run never starts a build. Run it from the FerrionOS directory. Without arguments it lists its tests and options.

- `scripts/vm-test.py boot` checks the quiet boot: only the black screen, the splash and the graphical login appear, and the login arrives within the timeout.
- `scripts/vm-test.py shells` logs in as `user` on the serial console and runs a command in each shell that the x86_64 desktop image offers.

Useful options:

| Option | Meaning |
|---|---|
| `--repeat <n>` | Boots that many times for each test. A race that shows once in a dozen boots needs it. |
| `--arch <arch>`, `--config <name>` | The image to test. The defaults are `x86_64` and `desktop`. |
| `--timeout <seconds>` | How long a step may take. The default is 60. |
| `--make VAR=VALUE` | An extra variable for `make qemu`, such as `net=no`. It can be repeated. |

Each run prints one line, `PASS` or `FAIL`, with the screens it saw and their times. The exit code is 1 when a run fails. The screenshots and the serial log of each run are in `build/vm-test/<arch>/<config>/<test>-<run>/`.

## The Security Audit

`scripts/security-audit.py` reads the repository without building anything and reports:

- `accounts`: for each filesystem configuration, the users with an empty or a written password, and the members of the group `sudo`
- `sources`: the recipes outside `recipes/wip` whose git source has no `rev` or whose tar source has no `blake3`
- `downloads`: the build inputs that are fetched without a pinned hash

It prints a summary, or one JSON object per line with `--json`, and `--check <name>` runs a single check. The script exits with 1 when it finds a failure that is missing from `scripts/security-audit.baseline.jsonl`, the record of the known findings. Run it after a change to accounts, to the `[source]` of a recipe, or to a download.

## Secure Boot

`scripts/secure-boot-test.sh <harddrive.img>` checks UEFI Secure Boot for an x86_64 image in QEMU. It makes test keys, enrolls them in a copy of the firmware's variables, and boots copies of the image: the bootloader signed with the enrolled key must reach the login, and the unsigned bootloader and one signed with another key must be refused by the firmware. On a verified-boot image ([Boot Process](./boot-process.md#verified-boot)) it adds three cases that the bootloader must refuse: a changed kernel, a changed `initfs`, and a manifest signed by another key. The image itself is not changed. Without arguments the script prints its options and the host tools it needs.

The keys are test keys. [Security](./security.md) says what Secure Boot and verified boot cover.

## Measured Boot

`scripts/measured-boot-test.sh <harddrive.img>` checks measured boot ([Boot Process](./boot-process.md#measured-boot)) for an x86_64 image in QEMU with a software TPM (packages `swtpm` and `tpm2-tools`). It boots copies of the image and, after each boot, reads the PCRs from the software TPM and the bootloader's event log from the guest's memory. It passes when PCRs 7, 8 and 9 match the log, PCR 9 matches the image's kernel and `initfs` (and its manifest on a verified-boot image), a changed `initfs` changes PCR 9 but not PCR 8, a boot through the menu changes PCR 8 but not PCR 9, and the image also boots without a TPM. With a software TPM, QEMU's firmware sometimes loses the TPM during startup; the script then tries the boot once more and reports `BLOCKED` if it happens again. The image itself is not changed. Without arguments the script prints its options and the host tools it needs.

## Test Suites Inside the System

Some filesystem configurations run a test suite at boot and then shut the system down, so that QEMU exits when the run is over:

| Configuration | What it runs |
|---|---|
| `acid` | `acid`, a test suite for crashes, regressions and race conditions |
| `os-test` | [os-test](https://sortix.org/os-test/), a POSIX test suite |
| `auto-test` | `acid`, the tests of relibc, and os-test |
| `sys-build` | A build of the system on FerrionOS itself |

Build and boot one like any other configuration, without a window:

```sh
CI=1 COOKBOOK_LOGS=true make all CONFIG_NAME=os-test
```

```sh
make qemu CONFIG_NAME=os-test gpu=no
```

`make ci-os-test` runs os-test this way, then mounts the image and copies the results to `build/os-test/<arch>/`.

FerrionOS has not run these suites on its own images yet.

## Running a Suite by Hand

These procedures are inherited from upstream and not verified by FerrionOS. Each `rp.` target cooks a recipe and pushes it into the existing image; stop QEMU first.

### acid

- Push the test programs into the image and start QEMU:

```sh
make rp.acid-bins qemu
```

- In FerrionOS, run a test:

```sh
acid test-name
```

### relibc-tests

- Push the test programs into the image and start QEMU:

```sh
make rp.relibc-tests-bins qemu
```

- In FerrionOS, run all tests, or one of them:

```sh
cd /usr/share/relibc-tests
```

```sh
make run
```

```sh
make run-once TESTBIN=test-name
```

### os-test

- Push the test programs into the image and start QEMU:

```sh
make rp.os-test-bins qemu
```

- In FerrionOS, run all suites, or one of them from its own directory:

```sh
cd /usr/share/os-test
```

```sh
make test
```

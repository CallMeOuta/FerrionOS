# Issue-reporting guidelines

Report problems using the [bug report form](https://github.com/CallMeOuta/FerrionOS/issues/new?template=bug_report.yml). It asks for what the maintainers need to reproduce and investigate the problem, including a reduced test case.

## What to include

* The commit you built (`git rev-parse HEAD`), or the image you downloaded and its date.
* The architecture (`x86_64`, `aarch64` or `riscv64gc`) and the config (`CONFIG_NAME`, for example `desktop` or `minimal`).
* Where it ran: the emulator and its version (`qemu-system-x86_64 --version`), or the hardware model.
* The exact commands you ran, from a fresh clone where possible.
* The whole log. For a build, `make all 2>&1 | tee build.log` keeps one. For a boot, `make qemu qemu_serial_logfile=serial.log` writes the serial console to a file. A screenshot shows what a log cannot, such as a wrong screen.

## How you can write a reduced test case

The single most important thing you can provide is a reduced test case: the smallest setup that still shows the problem.

1. **Build problems:** find the first recipe that fails (the log names it), and cook it alone with `make r.<recipe>`. If it fails the same way, that command and its log are the test case.
2. **Boot problems:** start from `CONFIG_NAME=minimal` instead of `desktop`. If the problem remains, remove packages from a copy of the config (`config/my_<name>.toml`, which git ignores) until removing one more makes the problem disappear. Attach that config.
3. **Problems inside the running system:** write the steps as commands. A config whose init script runs them and saves the result, as [agents/skills/images-and-qemu.md](agents/skills/images-and-qemu.md) describes, reproduces the problem without typing. `scripts/vm-test.py` can drive the screen and the serial console from outside.
4. **Ported programs:** check whether the program behaves the same on Linux. If it does, the problem belongs to the program's own project.

Keep removing parts while the problem still happens. When removing a part makes the problem disappear, put that part back: it is part of the cause, and naming it in the report saves the most time.

# Shells

A shell reads the commands a user types, or the lines of a script, and runs programs according to them. FerrionOS offers several shells, and each user account has one of them as its login shell.

## The Shells of an Image

The x86_64 desktop image offers five shells. `/etc/shells` lists them:

| Shell | Path |
|---|---|
| GNU Bash | `/usr/bin/bash` |
| fish | `/usr/bin/fish` |
| Ion | `/usr/bin/ion` |
| Nushell | `/usr/bin/nu` |
| Zsh | `/usr/bin/zsh` |

- The login shell of `user` is Nushell.
- `root` and the system's scripts use Ion.
- The recipe of Zsh is still work in progress: Zsh prints `getrusage()` warnings.

`config/x86_64/desktop.toml` sets this up. fish, Zsh and Nushell build only for x86_64 today, so the desktop images of the other architectures offer Bash and Ion, and `user` logs in to Ion there. The `minimal` configuration has Ion only.

`scripts/vm-test.py shells` checks the five shells on a built image: it logs in as `user` on the serial console and runs a command in each shell ([Testing](./testing-practices.md)).

## Changing a User's Shell

On a running system, `usermod` changes the login shell of a user. The path must be one that `/etc/shells` lists:

```sh
sudo usermod -s /usr/bin/fish user
```

To change the shell in the image instead, set `shell` in a `[users.<name>]` table of a personal configuration, `config/my_<name>.toml`, that includes the configuration you build. Such a table replaces the included one as a whole, so it repeats the other keys:

```toml
include = ["x86_64/desktop.toml"]

[users.user]
password = ""
shell = "/usr/bin/fish"
```

The same mechanism moves `root` and the system's scripts to another shell. A script then names its interpreter in its first line, for example `#!/usr/bin/nu`.

## Ion

Ion is a shell written in Rust for this system, and it also runs on Linux. It is a library for running commands as well, and the scripts that the repository's configurations install are written for it. Its manual is an mdBook in the `manual` directory of its source (recipe `recipes/core/ion`).

## Background

### What is a shell?

A shell is a layer around the operating system kernel and libraries that allows users to interact with the operating system. A shell can run on any operating system that provides the interfaces it needs. Shells can be graphical (GUI) or command-line (CLI).

### Text shells

A text shell is a program that provides an interactive user interface to an operating system. It reads what the user types and performs operations according to the input. This is similar to the read-eval-print loop (REPL) of many programming languages, such as Python.

### Typical Unix shells

The best-known shell is [GNU Bash](https://www.gnu.org/software/bash/), which is found in the majority of Linux distributions. FreeBSD uses **tcsh** by default.

There are many more shell implementations, and these two stand for two fundamentally different families:

- Bourne shell syntax (bash, sh, zsh)
- C shell syntax (csh, tcsh)

Other shells, such as **fish**, **Nushell** and **xonsh**, leave some features of the older shells behind to make the language safer and more regular.

### Features

Most shells, including the five above, offer a command history, completion, and shortcuts for editing a line.

### A scripting language

A shell is also used to write scripts for common tasks or for system configuration after startup. A shell language is meant as glue that connects other programs, and not as a general programming language.

### Relation to terminals

Early [terminals](https://en.wikipedia.org/wiki/Computer_terminal) were devices used to communicate with large computer systems such as [IBM mainframes](https://en.wikipedia.org/wiki/IBM_mainframe). Unix-like operating systems of today implement virtual terminals and terminal emulators instead.

A terminal reads input from a keyboard and displays the text output of the shell and of the programs that run inside it. It converts key strokes into control codes that the shell uses. The shell gives the user a prompt (for instance the user name and the working directory), line editing, a history, and the ability to run other programs.

# System Tools

### coreutils

Coreutils is a collection of basic command line utilities included with FerrionOS (or with Linux, BSD, etc.). This includes programs like `ls`, `cp`, `cat` and various other tools necessary for basic command line interaction.

Most of them come from [uutils](https://github.com/uutils/coreutils), the Rust implementation of the GNU Coreutils (recipe `recipes/core/uutils`). `clear`, `df`, `free`, `ps`, `reset`, `shutdown`, `time`, `uptime` and `which` come from the `coreutils` recipe (`recipes/core/coreutils`).

Available programs:

- `ls` - Shows the files and folders of the current directory.
- `cp` - Copies and pastes some file or folder.
- `cat` - Shows the output of some text file.
- `chmod` - Changes the permissions of some file or directory.
- `clear` - Cleans the terminal output.
- `dd` - Copies and converts a file.
- `df` - Shows the disk partitions information.
- `du` - Shows disk usage on file systems.
- `env` - Displays and modifies environment variables.
- `free` - Shows the RAM usage.
- `kill` - Kills a process.
- `ln` - Creates a link to a file.
- `mkdir` - Creates a directory.
- `ps` - Shows all running processes.
- `reset` - Restarts the terminal to allow the command-line input.
- `shutdown` - Shuts the system down.
- `sort` - Sorts, merges, or sequence checks text files.
- `stat` - Returns data about an inode.
- `tail` - Copies the last part of a file.
- `tee` - Duplicates the standard output.
- `test` - Evaluates expression.
- `time` - Counts the time that some command takes to finish its operation.
- `touch` - Updates the timestamp of some file or folder.
- `uname` - Shows the system information, like kernel version and architecture type.
- `uptime` - Shows how long your system has been running for.
- `which` - Shows the path where some program is located.

### userutils

Userutils (recipe `recipes/core/userutils`) contains the utilities for dealing with users and groups in FerrionOS.

They are heavily influenced by Unix and are, when needed, tailored to the system's own use cases.

These implementations strive to be as simple as possible drawing particular
inspiration by BSD systems. They are indeed small, by choice.

Available programs:

- `id` - Displays user identity.
- `login` - Allows users to login into the system
- `passwd` - Allows users to modify their passwords, and (as `root` with `--enroll LOGIN`) enrolls an account to unlock an encrypted disk at startup.
- `su` - Allows users to substitute identity.
- `sudo` - Enables users to execute a command as another user.
- `useradd` - Adds a user
- `usermod` - Modifies user information
- `userdel` - Deletes a user, revoking its disk-unlock slot first if the account was enrolled.
- `groupadd` - Adds a user group
- `groupmod` - Modifies group information
- `groupdel` - Removes a user group

`whoami` is a link to `id`. `getty`, which `init` uses to open a terminal line and start `login`, is part of the `base` package.

On an encrypted installation set up with one password, `passwd` changes the account's login password and rewraps its disk-unlock key slot in `/etc/unlock-slots` in one operation. If an administrator reset the account's login password with `passwd LOGIN`, the disk slot keeps the previous password until the owner runs `passwd`, which prompts for `previous disk password: ` (or the recovery key) and rewraps the slot before saving the new password.

Accounts added after installation with `useradd` do not unlock the disk automatically. An administrator enrolls a non-root account with a non-empty password by running `passwd --enroll LOGIN`, which verifies the account's password and an existing disk password or recovery key, adds a new RedoxFS key slot, and records it in `/etc/unlock-slots`. Removing an enrolled account with `userdel LOGIN` asks for a disk password or recovery key, removes the account's key slot (rolling the header ring), and keeps the account if slot removal fails. The bootloader's startup name list (`EFI/BOOT/UNLOCK.TXT` on the EFI partition) is written at installation and is not rewritten when accounts are enrolled or removed later; that file is display-only and does not affect which slot unlocks the disk or which account logs in.

### extrautils

Some additional command line tools are included in extrautils (recipe `recipes/core/extrautils`), such as `less`, `tar` and `dmesg`.

Available programs:

- `calc` - Do math operations.
- `cur` - Move terminal cursor keys using `vi` keybindings.
- `dmesg` - Show the kernel message buffer.
- `gunzip` - Decompress `tar.gz` archives.
- `gzip` - Compress files into `tar.gz` archives.
- `info` - Read Markdown files with help pages.
- `keymap` - Change the keyboard map.
- `less` - Show the text file content one page at a time.
- `man` - Show the program manual.
- `mdless` - Pager with Markdown support.
- `mtxt` - Various text conversions, like lowercase to uppercase.
- `rem` - Countdown tool.
- `resize` - Print the size of the terminal in the form of shell commands to export the `COLUMNS` and `LINES` environment variables.
- `screenfetch` - Show system information.
- `tar` - Manipulate `tar` archives.
- `unzip` - Manipulate `zip` archives.

`grep` comes from the `uutils-grep` package and `watch` from the `uutils-procps` package. `config/minimal.toml` installs both.

### binutils

Binutils (recipe `recipes/core/binutils`) contains utilities for manipulating binary files. The `minimal`, `server` and `desktop` configs do not install it.

Available programs:

- `hex` - Filter and show files in hexadecimal format.
- `hexdump` - Filter and show files in hexadecimal format (better output formatting).
- `strings` - Find printable strings in files.

### contain

`contain` (recipe `recipes/core/contain`) provides containers (namespaces). Upstream calls it a partial implementation of sandboxing. The `minimal`, `server` and `desktop` configs do not install it (`config/server.toml` has it commented out), and it is not a security boundary: see [Security](./security.md).

### acid

`acid` (recipe `recipes/tests/acid`) is the general-purpose test suite for the system, to detect crashes, regressions and race conditions. The `acid` config (`config/acid.toml`) builds an image that runs it at boot ([Testing](./testing-practices.md)).

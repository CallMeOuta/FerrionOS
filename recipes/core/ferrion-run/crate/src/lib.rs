use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[cfg(target_os = "redox")]
pub mod launch;

#[cfg(target_os = "redox")]
pub mod escape;

pub mod sessions;

pub const NETWORK_SCHEMES: [&str; 4] = ["ip", "icmp", "tcp", "udp"];

pub const SCOPED_SCHEMES: [&str; 13] = [
    "event",
    "memory",
    "pipe",
    "time",
    "rand",
    "null",
    "zero",
    "sys",
    "proc",
    "uds_stream",
    "uds_dgram",
    "eventfd",
    "timerfd",
];

pub const SYSTEM_PATHS: [&str; 8] = [
    "/usr", "/bin", "/lib", "/include", "/share", "/ssl", "/etc", "/dev",
];

pub const PASSED_VARIABLES: [&str; 10] = [
    "PATH", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "TZ", "COLUMNS", "LINES", "USER", "LOGNAME",
];

pub const PROFILE_DIR: &str = "/etc/ferrion-run";

pub const STATE_DIR: &str = ".local/state/ferrion-run";

pub const MAX_GRANTS: usize = 32;

pub const EXIT_REFUSED: i32 = 125;

pub const USAGE: &str = "\
usage: ferrion-run [OPTION ...] [--] PROGRAM [ARGUMENT ...]
       ferrion-run sessions | diff SESSION | apply SESSION | discard SESSION

Starts PROGRAM in a scoped session: a namespace of its own in which the file
system holds only what the session was given. Every path keeps its real name.
  --files DIR        DIR, read-write.
  --read-only PATH   PATH, a folder or a file, read-only.
  --clone DIR        DIR through a copy-on-write layer: PROGRAM reads DIR, and its
                     writes and deletes land in the session's layer, never in DIR.
                     The layer outlives the session: `ferrion-run diff SESSION`
                     lists what changed, `apply SESSION` writes it into DIR,
                     `discard SESSION` throws it away, `sessions` lists layers.
  --no-network       PROGRAM gets no ip, icmp, tcp or udp scheme, so it cannot open
                     a socket; a connection fails as on a machine without a network.
  --env NAME         passes the caller's variable NAME on to PROGRAM.
  --pass-fd FD       passes one pre-opened Unix socket descriptor (FD > 2) to
                     PROGRAM; at most one may be given.
  --pty              PROGRAM may open new terminals (the pty scheme). WARNING: the
                     pty scheme is not scoped to the session; give it only to
                     programs you trust.
Options may repeat; ferrion-run holds at most 32 folders and files.

Besides these, every session gets read-only /usr, /bin, /lib, /include, /share,
/ssl, /etc and /dev, an empty home of its own (HOME) and an empty /tmp of its own,
both in ~/.local/state/ferrion-run/SESSION, which the session itself cannot see
otherwise. Any other path answers as missing. PROGRAM cannot rename or remove a
folder that holds another of its folders or ferrion-run's sessions. A symlink it
writes or moves stays, and since programs outside a session follow it,
ferrion-run lists each such symlink that leads outside its folder when PROGRAM
exits; `diff` and `apply` list those in a layer. A symlink that was there before
the session and that PROGRAM re-aims by changing a folder or symlink on its way
is not listed. The namespace holds the `file` scheme
of the session and event, memory, pipe, time, rand, null, zero, sys, proc,
uds_stream, uds_dgram, eventfd and timerfd, plus the network schemes and pty as
above; never sudo, file-scope, debug, log, shm, chan, audio, the display schemes or
a disk. Through sys PROGRAM still reads the list of processes and the paths of the
descriptors every process holds. PROGRAM's environment holds PATH, TERM, LANG,
LC_ALL, LC_CTYPE, TZ, COLUMNS, LINES, USER, LOGNAME, HOME, TMPDIR and the --env
variables, nothing else. It starts in the caller's folder when that folder is in
the session, in its home otherwise.

Every launch closes the descriptors PROGRAM would inherit except standard input,
output, error and any single Unix socket passed with --pass-fd, so no other
socket opened before the launch reaches it. Standard input, output and error
must be a terminal, a pipe or the null device: ferrion-run refuses to start when
one is a socket or a file on disk. ferrion-run waits for PROGRAM; the session's
files close when ferrion-run exits, also for programs PROGRAM left running.

A profile in /etc/ferrion-run/<file name of PROGRAM>.toml applies to every launch
of that program:
  version = 1
  network = false     # the same as --no-network; true leaves the network to the
                      # command line
  allow_sudo = true   # optional, default false. WARNING: through sudo PROGRAM can
                      # get a namespace with the network, every other scheme and
                      # the whole file system, so the session no longer holds.
Apart from allow_sudo, a profile and the command line only take access away.
ferrion-run exits with 125 when it refuses to start PROGRAM, and with PROGRAM's
status otherwise.
";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantKind {
    Files,
    ReadOnly,
    Clone,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    pub no_network: bool,
    pub pty: bool,
    pub pass_fd: Option<usize>,
    pub grants: Vec<(GrantKind, PathBuf)>,
    pub env: Vec<OsString>,
    pub program: OsString,
    pub args: Vec<OsString>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Run(Options),
    Sessions,
    Diff(String),
    Apply(String),
    Discard(String),
}

pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut args = args.into_iter().peekable();
    if let Some(first) = args.peek().and_then(|a| a.to_str()) {
        let first = first.to_owned();
        let session = |args: &mut dyn Iterator<Item = OsString>| -> Result<String, String> {
            let id = args
                .next()
                .and_then(|a| a.into_string().ok())
                .ok_or(format!("{first} needs a session"))?;
            if args.next().is_some() {
                return Err(format!("{first} takes one session"));
            }
            if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                return Err(format!("{id:?} is not a session name"));
            }
            Ok(id)
        };
        match first.as_str() {
            "sessions" => {
                args.next();
                return if args.next().is_some() {
                    Err("sessions takes no argument".into())
                } else {
                    Ok(Command::Sessions)
                };
            }
            "diff" | "apply" | "discard" => {
                args.next();
                let id = session(&mut args)?;
                return Ok(match first.as_str() {
                    "diff" => Command::Diff(id),
                    "apply" => Command::Apply(id),
                    _ => Command::Discard(id),
                });
            }
            _ => {}
        }
    }
    let mut no_network = false;
    let mut pty = false;
    let mut pass_fd = None;
    let mut grants = Vec::new();
    let mut env = Vec::new();
    let program = loop {
        let Some(arg) = args.next() else {
            return Err("no program given".into());
        };
        let kind = match arg.to_str() {
            Some("-h" | "--help") => return Ok(Command::Help),
            Some("--no-network") => {
                no_network = true;
                continue;
            }
            Some("--pty") => {
                pty = true;
                continue;
            }
            Some("--pass-fd") => {
                if pass_fd.is_some() {
                    return Err("--pass-fd may be given at most once".into());
                }
                let raw = args.next().ok_or("--pass-fd needs a descriptor number")?;
                let fd: usize = raw.to_str().and_then(|s| s.parse().ok()).ok_or_else(|| {
                    format!(
                        "--pass-fd {} is not a descriptor number",
                        raw.to_string_lossy()
                    )
                })?;
                if fd <= 2 {
                    return Err(format!("--pass-fd {fd} must be greater than 2"));
                }
                pass_fd = Some(fd);
                continue;
            }
            Some("--env") => {
                let name = args.next().ok_or("--env needs a variable name")?;
                env.push(name);
                continue;
            }
            Some("--files") => GrantKind::Files,
            Some("--read-only") => GrantKind::ReadOnly,
            Some("--clone") => GrantKind::Clone,
            Some("--") => match args.next() {
                Some(program) => break program,
                None => return Err("no program given".into()),
            },
            Some(option) if option.starts_with('-') => {
                return Err(format!("unknown option {option}"));
            }
            _ => break arg,
        };
        let path = args
            .next()
            .ok_or_else(|| format!("{} needs a path", arg.to_string_lossy()))?;
        if grants.len() >= MAX_GRANTS {
            return Err(format!("more than {MAX_GRANTS} folders and files"));
        }
        grants.push((kind, PathBuf::from(path)));
    };
    Ok(Command::Run(Options {
        no_network,
        pty,
        pass_fd,
        grants,
        env,
        program,
        args: args.collect(),
    }))
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub version: u32,
    pub network: bool,
    #[serde(default)]
    pub allow_sudo: bool,
}

pub fn profile_path(program: &OsStr) -> Option<PathBuf> {
    let name = Path::new(program).file_name()?;
    let mut file = name.to_owned();
    file.push(".toml");
    Some(Path::new(PROFILE_DIR).join(file))
}

pub fn parse_profile(text: &str) -> Result<Profile, String> {
    let profile: Profile = toml::from_str(text).map_err(|e| e.message().to_owned())?;
    if profile.version != 1 {
        return Err(format!(
            "version {} is not supported (only 1)",
            profile.version
        ));
    }
    Ok(profile)
}

pub fn child_schemes(
    caller: &[String],
    withhold_network: bool,
    allow_sudo: bool,
    pty: bool,
) -> Vec<String> {
    caller
        .iter()
        .filter(|name| {
            let name = name.as_str();
            SCOPED_SCHEMES.contains(&name)
                || (!withhold_network && NETWORK_SCHEMES.contains(&name))
                || (allow_sudo && name == "sudo")
                || (pty && name == "pty")
        })
        .cloned()
        .collect()
}

pub fn scheme_of_path(path: &str) -> Option<&str> {
    let name = match path.strip_prefix("/scheme/") {
        Some(rest) => rest.split('/').next()?,
        None => path.split_once(':')?.0,
    };
    (!name.is_empty()).then_some(name)
}

const SOCKET_SCHEMES: [&str; 7] = [
    "ip",
    "icmp",
    "tcp",
    "udp",
    "uds_stream",
    "uds_dgram",
    "chan",
];
const UNIX_SOCKET_SCHEMES: [&str; 2] = ["uds_stream", "uds_dgram"];
const STDIO_SCHEMES: [&str; 5] = ["pty", "pipe", "debug", "null", "zero"];

pub fn check_pass_fd(fd: usize, path: &str) -> Result<(), String> {
    if fd <= 2 {
        return Err(format!("--pass-fd {fd} must be greater than 2"));
    }
    let Some(scheme) = scheme_of_path(path) else {
        return Err(format!("descriptor {fd} has an unreadable path {path:?}"));
    };
    let scheme = scheme.strip_prefix("kernel.").unwrap_or(scheme);
    if UNIX_SOCKET_SCHEMES.contains(&scheme) {
        Ok(())
    } else {
        Err(format!(
            "descriptor {fd} comes from scheme {scheme}, which is not a Unix socket"
        ))
    }
}

pub fn check_stdio(fd: usize, path: &str) -> Result<(), String> {
    let Some(scheme) = scheme_of_path(path) else {
        return Err(format!("descriptor {fd} has an unreadable path {path:?}"));
    };
    let scheme = scheme.strip_prefix("kernel.").unwrap_or(scheme);
    if STDIO_SCHEMES.contains(&scheme) {
        Ok(())
    } else if SOCKET_SCHEMES.contains(&scheme) {
        Err(format!("descriptor {fd} is a socket ({scheme})"))
    } else if scheme == "file" {
        Err(format!("descriptor {fd} is a file on disk"))
    } else {
        Err(format!(
            "descriptor {fd} comes from scheme {scheme}, which is not a terminal, pipe or null device"
        ))
    }
}

pub fn parse_snapshot(bytes: &[u8]) -> Result<Vec<usize>, String> {
    if !bytes.len().is_multiple_of(8) {
        return Err(format!(
            "file table snapshot of {} bytes is not whole entries",
            bytes.len()
        ));
    }
    bytes
        .chunks_exact(8)
        .map(|chunk| {
            let value = u64::from_le_bytes(chunk.try_into().unwrap());
            usize::try_from(value).map_err(|_| format!("descriptor {value} does not fit"))
        })
        .collect()
}

pub fn new_handles(before: &[usize], after: &[usize]) -> Vec<usize> {
    after
        .iter()
        .copied()
        .filter(|fd| !before.contains(fd))
        .collect()
}

pub fn to_close(table: &[usize], keep: &[usize]) -> Vec<usize> {
    table
        .iter()
        .copied()
        .filter(|fd| *fd > 2 && !keep.contains(fd))
        .collect()
}

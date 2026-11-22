use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[cfg(target_os = "redox")]
pub mod launch;

pub const NETWORK_SCHEMES: [&str; 4] = ["ip", "icmp", "tcp", "udp"];

pub const PROFILE_DIR: &str = "/etc/ferrion-run";

pub const EXIT_REFUSED: i32 = 125;

pub const USAGE: &str = "\
usage: ferrion-run [--no-network] [--] PROGRAM [ARGUMENT ...]

Starts PROGRAM in a namespace of its own. This release limits network access only:
  --no-network   PROGRAM gets no ip, icmp, tcp or udp scheme, so it cannot open a
                 socket; a connection fails as on a machine without a network.
  --files DIR, --read-only DIR
                 not available yet: a storage scope waits for a file-access proxy,
                 and ferrion-run refuses these options instead of ignoring them.

PROGRAM never gets the sudo scheme, which would hand it a namespace with everything
back, unless its profile says allow_sudo = true.

Every launch closes the descriptors PROGRAM would inherit except standard input,
output and error, so a socket opened before the launch does not reach it. Standard
input, output and error must be a terminal, a pipe or the null device: ferrion-run
refuses to start when one is a socket or a file on disk.

A profile in /etc/ferrion-run/<file name of PROGRAM>.toml applies to every launch
of that program:
  version = 1
  network = false     # the same as --no-network; true leaves the network to the
                      # command line
  allow_sudo = true   # optional, default false. WARNING: through sudo PROGRAM can
                      # get a namespace with the network and every other scheme,
                      # so --no-network no longer holds for it.
Apart from allow_sudo, a profile and the command line only take access away.
ferrion-run exits with 125 when it refuses to start PROGRAM.
";

#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    pub no_network: bool,
    pub program: OsString,
    pub args: Vec<OsString>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Run(Options),
}

pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut args = args.into_iter();
    let mut no_network = false;
    let program = loop {
        let Some(arg) = args.next() else {
            return Err("no program given".into());
        };
        match arg.to_str() {
            Some("-h" | "--help") => return Ok(Command::Help),
            Some("--no-network") => no_network = true,
            Some("--files" | "--read-only") => {
                return Err(format!(
                    "{} is not available yet: a storage scope waits for a file-access proxy",
                    arg.to_string_lossy()
                ));
            }
            Some("--") => match args.next() {
                Some(program) => break program,
                None => return Err("no program given".into()),
            },
            Some(option) if option.starts_with('-') => {
                return Err(format!("unknown option {option}"));
            }
            _ => break arg,
        }
    };
    Ok(Command::Run(Options {
        no_network,
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

pub fn child_schemes(caller: &[String], withhold_network: bool, allow_sudo: bool) -> Vec<String> {
    caller
        .iter()
        .filter(|name| !(withhold_network && NETWORK_SCHEMES.contains(&name.as_str())))
        .filter(|name| allow_sudo || name.as_str() != "sudo")
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
const STDIO_SCHEMES: [&str; 5] = ["pty", "pipe", "debug", "null", "zero"];

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

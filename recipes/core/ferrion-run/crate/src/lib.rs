//! ferrion-run's policy: options, profiles, the rule for standard descriptors and the
//! descriptor bookkeeping. Nothing here calls the OS; `launch` (FerrionOS only) does.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[cfg(target_os = "redox")]
pub mod launch;

/// The schemes that carry the network. A namespace without them cannot open a socket.
pub const NETWORK_SCHEMES: [&str; 4] = ["ip", "icmp", "tcp", "udp"];

/// Where profiles live: `<PROFILE_DIR>/<program file name>.toml`.
pub const PROFILE_DIR: &str = "/etc/ferrion-run";

/// Exit status when ferrion-run refuses or fails before the program starts.
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

/// Parses the arguments after the program name. Options stop at `--` or at the first
/// argument that is not an option; everything after PROGRAM goes to it unchanged.
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
    /// Gives the program the `sudo` scheme back; never the default.
    #[serde(default)]
    pub allow_sudo: bool,
}

/// The profile file for PROGRAM, by its file name; None when PROGRAM names no file.
pub fn profile_path(program: &OsStr) -> Option<PathBuf> {
    let name = Path::new(program).file_name()?;
    let mut file = name.to_owned();
    file.push(".toml");
    Some(Path::new(PROFILE_DIR).join(file))
}

/// Parses a profile; an unknown key or version is an error, so a profile written for a
/// later ferrion-run is refused instead of half applied.
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

/// The schemes the program receives: the caller's own, less the network when it is
/// withheld, and less `sudo` unless a profile grants it.
pub fn child_schemes(caller: &[String], withhold_network: bool, allow_sudo: bool) -> Vec<String> {
    caller
        .iter()
        .filter(|name| !(withhold_network && NETWORK_SCHEMES.contains(&name.as_str())))
        .filter(|name| allow_sudo || name.as_str() != "sudo")
        .cloned()
        .collect()
}

/// The scheme a descriptor's path names: `/scheme/<name>/...` or `<name>:...`.
pub fn scheme_of_path(path: &str) -> Option<&str> {
    let name = match path.strip_prefix("/scheme/") {
        Some(rest) => rest.split('/').next()?,
        None => path.split_once(':')?.0,
    };
    (!name.is_empty()).then_some(name)
}

/// Schemes whose descriptors are sockets or connections to another process.
const SOCKET_SCHEMES: [&str; 7] = [
    "ip",
    "icmp",
    "tcp",
    "udp",
    "uds_stream",
    "uds_dgram",
    "chan",
];
/// Schemes a standard descriptor may come from: terminals, pipes and the null devices.
const STDIO_SCHEMES: [&str; 5] = ["pty", "pipe", "debug", "null", "zero"];

/// Whether a standard descriptor whose path is `path` may pass to the program. A socket
/// and a file on disk are refused, and so is any scheme not known to be a terminal, a
/// pipe or a null device.
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

/// Parses a file-table snapshot: one little-endian u64 descriptor number after another.
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

/// Descriptors in `after` that were not in `before`.
pub fn new_handles(before: &[usize], after: &[usize]) -> Vec<usize> {
    after
        .iter()
        .copied()
        .filter(|fd| !before.contains(fd))
        .collect()
}

/// Descriptors of `table` to close: all but `keep` and the standard three.
pub fn to_close(table: &[usize], keep: &[usize]) -> Vec<usize> {
    table
        .iter()
        .copied()
        .filter(|fd| *fd > 2 && !keep.contains(fd))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn run(list: &[&str]) -> Result<Command, String> {
        parse_args(args(list))
    }

    #[test]
    fn options_stop_at_program() {
        assert_eq!(
            run(&["--no-network", "curl", "--no-network", "-v"]),
            Ok(Command::Run(Options {
                no_network: true,
                program: "curl".into(),
                args: args(&["--no-network", "-v"]),
            }))
        );
        assert_eq!(
            run(&["--", "-dash"]),
            Ok(Command::Run(Options {
                no_network: false,
                program: "-dash".into(),
                args: vec![],
            }))
        );
    }

    #[test]
    fn refuses_what_it_cannot_do() {
        assert!(
            run(&["--files", "/tmp/x", "ls"])
                .unwrap_err()
                .contains("file-access proxy")
        );
        assert!(
            run(&["--read-only", "/tmp/x", "ls"])
                .unwrap_err()
                .contains("file-access proxy")
        );
        assert!(
            run(&["--network", "curl"])
                .unwrap_err()
                .contains("unknown option")
        );
        assert!(run(&["--no-network"]).is_err());
        assert!(run(&["--"]).is_err());
        assert_eq!(run(&["--help"]), Ok(Command::Help));
    }

    #[test]
    fn profile_by_file_name() {
        assert_eq!(
            profile_path(OsStr::new("/usr/bin/curl")),
            Some(PathBuf::from("/etc/ferrion-run/curl.toml"))
        );
        assert_eq!(profile_path(OsStr::new("..")), None);
        assert_eq!(profile_path(OsStr::new("/")), None);
    }

    #[test]
    fn profile_is_strict() {
        assert_eq!(
            parse_profile("version = 1\nnetwork = false\n"),
            Ok(Profile {
                version: 1,
                network: false,
                allow_sudo: false,
            })
        );
        assert_eq!(
            parse_profile("version = 1\nnetwork = true\nallow_sudo = true\n").map(|p| p.allow_sudo),
            Ok(true)
        );
        assert!(parse_profile("version = 2\nnetwork = false\n").is_err());
        assert!(parse_profile("version = 1\n").is_err());
        assert!(parse_profile("version = 1\nnetwork = false\nfiles = \"/tmp\"\n").is_err());
    }

    #[test]
    fn withholds_network_and_sudo() {
        let caller: Vec<String> = ["file", "tcp", "pty", "udp", "ip", "icmp", "sudo"]
            .map(String::from)
            .to_vec();
        assert_eq!(child_schemes(&caller, true, false), ["file", "pty"]);
        assert_eq!(
            child_schemes(&caller, false, false),
            ["file", "tcp", "pty", "udp", "ip", "icmp"]
        );
        assert_eq!(child_schemes(&caller, true, true), ["file", "pty", "sudo"]);
        assert_eq!(child_schemes(&caller, false, true), caller);
    }

    #[test]
    fn standard_descriptors() {
        assert_eq!(scheme_of_path("/scheme/pty/3"), Some("pty"));
        assert_eq!(scheme_of_path("tcp:10.0.2.2:80"), Some("tcp"));
        assert_eq!(scheme_of_path("/home/user"), None);
        assert!(check_stdio(0, "/scheme/pty/3").is_ok());
        assert!(check_stdio(1, "/scheme/kernel.pipe/7").is_ok());
        assert!(check_stdio(2, "/scheme/null").is_ok());
        assert!(
            check_stdio(0, "/scheme/tcp/10.0.2.2:80")
                .unwrap_err()
                .contains("socket")
        );
        assert!(
            check_stdio(0, "/scheme/uds_stream/x")
                .unwrap_err()
                .contains("socket")
        );
        assert!(
            check_stdio(1, "/scheme/file/home/user/out")
                .unwrap_err()
                .contains("file on disk")
        );
        assert!(check_stdio(1, "/scheme/sudo").is_err());
        assert!(check_stdio(1, "").is_err());
    }

    #[test]
    fn descriptor_bookkeeping() {
        let mut bytes = Vec::new();
        for fd in [0u64, 1, 2, 5, 1 << 40] {
            bytes.extend(fd.to_le_bytes());
        }
        assert_eq!(parse_snapshot(&bytes), Ok(vec![0, 1, 2, 5, 1 << 40]));
        assert!(parse_snapshot(&bytes[..7]).is_err());
        assert_eq!(new_handles(&[0, 1, 7], &[0, 1, 9]), [9]);
        assert_eq!(to_close(&[0, 1, 2, 3, 7, 1 << 40], &[7]), [3, 1 << 40]);
    }
}

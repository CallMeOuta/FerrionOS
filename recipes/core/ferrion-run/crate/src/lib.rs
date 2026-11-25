use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use ferrion_manifest::{GrantsFile, Manifest, effective_grant};
use serde::Deserialize;

#[cfg(target_os = "redox")]
pub mod launch;

#[cfg(target_os = "redox")]
pub mod escape;

pub mod manifest;
pub mod sessions;

pub const NETWORK_SCHEMES: [&str; 4] = ["ip", "icmp", "tcp", "udp"];

pub const AUDIO_SCHEMES: [&str; 1] = ["audio"];

pub const MANIFEST_DIR: &str = "/usr/share/ferrion/apps";

pub const GRANTS_DIR: &str = "/var/lib/ferrion/grants";

pub const APP_HOME_ROOT_REL: &str = ".local/share/ferrion/apps";

pub const MAX_PATH_ENTRIES: usize = 64;

pub const FOLDER_CAPABILITIES: [(&str, &str, &str); 6] = [
    ("files-documents", "Documents", "XDG_DOCUMENTS_DIR"),
    ("files-downloads", "Downloads", "XDG_DOWNLOAD_DIR"),
    ("files-desktop", "Desktop", "XDG_DESKTOP_DIR"),
    ("files-pictures", "Pictures", "XDG_PICTURES_DIR"),
    ("files-music", "Music", "XDG_MUSIC_DIR"),
    ("files-videos", "Videos", "XDG_VIDEOS_DIR"),
];

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

pub const SYSTEM_PATHS: [&str; 9] = [
    "/usr",
    "/bin",
    "/lib",
    "/include",
    "/share",
    "/ssl",
    "/etc",
    "/dev",
    "/var/cache/fontconfig",
];

pub const PASSED_VARIABLES: [&str; 10] = [
    "PATH", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "TZ", "COLUMNS", "LINES", "USER", "LOGNAME",
];

pub const PROFILE_DIR: &str = "/etc/ferrion-run";

pub const STATE_DIR: &str = ".local/state/ferrion-run";

pub const MAX_GRANTS: usize = 32;

pub const NESTED_LIVENESS_POLL_MS: u64 = 100;

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
  --display          grants the compositor's Wayland socket in a private
                     XDG_RUNTIME_DIR as wayland-0. WARNING: with --display a
                     session reaches ferrion-wm's main socket as a restricted
                     client: windows, input to its own windows, paste with
                     focus; no capture, input emulation, clipboard monitoring,
                     layer surfaces or lock; it can see the person's idle state.
Options may repeat; ferrion-run holds at most 32 folders and files.

Besides these, every session gets read-only /usr, /bin, /lib, /include, /share,
/ssl, /etc, /dev and /var/cache/fontconfig (when present, nothing else under
/var), an empty home of its own (HOME) and an empty /tmp of its own,
both in ~/.local/state/ferrion-run/SESSION, which the session itself cannot see
otherwise. Any other path answers as missing. PROGRAM cannot rename or remove a
folder that holds another of its folders or ferrion-run's sessions. A symlink it
writes or moves stays, and since programs outside a session follow it,
ferrion-run lists each such symlink that leads outside its folder when PROGRAM
exits; `diff` and `apply` list those in a layer. A symlink that was there before
the session and that PROGRAM re-aims by changing a folder or symlink on its way
is not listed. The namespace holds the `file` scheme
of the session, a session-private `shm` prefix (each session's shared-memory
objects live under its own prefix and it cannot reach any object outside it),
and event, memory, pipe, time, rand, null, zero, sys, proc,
uds_stream, uds_dgram, eventfd and timerfd, plus the network schemes and pty as
above; never sudo, file-scope, debug, log, chan, audio, the display schemes or
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

A manifest in /usr/share/ferrion/apps/<package>.toml whose exec matches the
canonical system path of PROGRAM (/usr or /bin) replaces profiles: grants come
from the intersection of that manifest and /var/lib/ferrion/grants/<uid>.toml
(root-owned; missing file uses class defaults, malformed file is refused).
Under a manifest, --files, --read-only, --clone, --pty, --display and --pass-fd
are refused; HOME is a persistent per-app folder (~/.local/share/ferrion/apps/<id>/home);
granted folders are advertised in XDG_*_DIR and $HOME/.config/user-dirs.dirs;
display grants the compositor's Wayland socket as wayland-0 in a private
XDG_RUNTIME_DIR; network-internet gates ip, icmp, tcp and udp (network-local
and network-loopback-shared fail closed until netstack views land);
audio-playback gates the audio scheme (capture still requires audiod's grant);
sudo is never passed.

A legacy profile in /etc/ferrion-run/<file name of PROGRAM>.toml is deprecated
and read only when no manifest matches:
  version = 1
  network = false     # the same as --no-network; true leaves the network to the
                      # command line
  allow_sudo = true   # optional, default false. WARNING: through sudo PROGRAM can
                      # get a namespace with the network, every other scheme and
                      # the whole file system, so the session no longer holds.
Apart from allow_sudo in legacy profiles, flags only take access away.

Nested sessions: when started inside an existing ferrion-run session (where
/scheme/file-scope/new is absent), ferrion-run asks the session's file proxy
at /.ferrion-scope/new for a strictly narrower sub-scope (up to depth 4 and
16 direct children; past either fails with EMLINK). Every child grant is
validated in the proxy against the parent's scope and refused with exit 125
(wider-than-parent) if it exceeds the parent's grants or schemes; the child
also gets a private shm sub-prefix under the parent's shm handle; --pass-fd
stays allowed for nested ad-hoc launches. v1 limits: child grants inside a
parent --clone grant are refused; when the parent ends, the child's process
group is killed within 100 ms (NESTED_LIVENESS_POLL_MS), while deeper
descendants that left the process group keep running with dead file handles
until procmgr's session tag lands.

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
    pub display: bool,
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
    let mut display = false;
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
            Some("--display") => {
                display = true;
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
        display,
        grants,
        env,
        program,
        args: args.collect(),
    }))
}

pub fn is_valid_wayland_display(name: &str) -> bool {
    let Some(suffix) = name.strip_prefix("wayland-") else {
        return false;
    };
    (1..=3).contains(&suffix.len()) && suffix.bytes().all(|b| b.is_ascii_digit())
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestPolicy {
    pub id: String,
    pub resolved_exec: PathBuf,
    pub allow_network: bool,
    pub allow_audio: bool,
    pub allow_display: bool,
    pub granted_folders: Vec<(&'static str, &'static str, &'static str)>,
    pub allow_home: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchPolicy {
    Manifest(ManifestPolicy),
    Legacy {
        withhold_network: bool,
        allow_sudo: bool,
    },
}

pub fn manifest_policy(
    manifest: &Manifest,
    grants: &GrantsFile,
    options: &Options,
    resolved_exec: PathBuf,
) -> Result<ManifestPolicy, String> {
    if !options.grants.is_empty() || options.pty || options.display || options.pass_fd.is_some() {
        return Err(format!(
            "grants for {} come from the grant record, not command-line flags",
            manifest.id
        ));
    }
    let allow_network =
        effective_grant(manifest, grants, "network-internet") && !options.no_network;
    let allow_audio = effective_grant(manifest, grants, "audio-playback");
    let allow_display = effective_grant(manifest, grants, "display");
    let granted_folders = FOLDER_CAPABILITIES
        .iter()
        .copied()
        .filter(|(cap, _, _)| effective_grant(manifest, grants, cap))
        .collect();
    let allow_home = effective_grant(manifest, grants, "files-home");
    assert!(!effective_grant(manifest, grants, "sudo"));
    Ok(ManifestPolicy {
        id: manifest.id.clone(),
        resolved_exec,
        allow_network,
        allow_audio,
        allow_display,
        granted_folders,
        allow_home,
    })
}

pub fn is_system_exec_path(canonical: &str) -> bool {
    canonical.starts_with("/usr/") || canonical.starts_with("/bin/")
}

pub fn normalize_system_exec(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("/bin/") {
        format!("/usr/bin/{rest}")
    } else {
        path.to_owned()
    }
}

pub fn format_user_dirs(entries: &[(&str, String)]) -> String {
    let mut out = String::new();
    for (var, path) in entries {
        assert!(!path.contains(['\n', '\0']));
        let escaped = path.replace('\\', "\\\\").replace('"', "\\\"");
        out.push_str(var);
        out.push_str("=\"");
        out.push_str(&escaped);
        out.push_str("\"\n");
    }
    out
}

pub fn child_schemes(
    caller: &[String],
    withhold_network: bool,
    allow_sudo: bool,
    pty: bool,
    allow_audio: bool,
) -> Vec<String> {
    caller
        .iter()
        .filter(|name| {
            let name = name.as_str();
            SCOPED_SCHEMES.contains(&name)
                || (!withhold_network && NETWORK_SCHEMES.contains(&name))
                || (allow_sudo && name == "sudo")
                || (pty && name == "pty")
                || (allow_audio && AUDIO_SCHEMES.contains(&name))
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

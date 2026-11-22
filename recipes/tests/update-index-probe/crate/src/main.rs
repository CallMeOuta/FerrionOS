use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use ferrion_update::client::{ClientState, FetchRequest, Fetcher, apply_update};
use ferrion_update::crypto::{Blake3Hash, PublicKey};
use ferrion_update::error::FetchError;
use ferrion_update::format::{Arch, CalendarDate, Channel, ReleaseNumber};

const MAX_HTTP_HEADER_BYTES: usize = 16 * 1024;
const MAX_STATE_BYTES: usize = 256 * 1024;
const HTTP_CHUNK_BYTES: usize = 16 * 1024;
const HTTP_TIMEOUT_SECS: u64 = 30;
const MAX_CLI_ARGS: usize = 64;

struct HttpFetcher {
    addr: String,
}

impl Fetcher for HttpFetcher {
    fn fetch(&mut self, request: &FetchRequest) -> Result<Vec<u8>, FetchError> {
        let mut stream = TcpStream::connect(&self.addr)
            .map_err(|e| FetchError::Transport(format!("connect {}: {e}", self.addr)))?;
        let timeout = Some(Duration::from_secs(HTTP_TIMEOUT_SECS));
        let _ = stream.set_read_timeout(timeout);
        let _ = stream.set_write_timeout(timeout);

        let wire = format!(
            "GET {} HTTP/1.0\r\nHost: {}\r\nUser-Agent: {}\r\nConnection: close\r\n\r\n",
            request.path, self.addr, request.user_agent
        );
        stream
            .write_all(wire.as_bytes())
            .map_err(|e| FetchError::Transport(format!("write request: {e}")))?;

        let mut head_buf = Vec::with_capacity(1024);
        let mut byte = [0u8; 1];
        let header_end;
        loop {
            let n = stream
                .read(&mut byte)
                .map_err(|e| FetchError::Transport(format!("read header: {e}")))?;
            if n == 0 {
                return Err(FetchError::Transport(
                    "connection closed before HTTP headers completed".to_string(),
                ));
            }
            head_buf.push(byte[0]);
            if head_buf.len() >= 4 && &head_buf[head_buf.len() - 4..] == b"\r\n\r\n" {
                header_end = head_buf.len() - 4;
                break;
            }
            if head_buf.len() >= MAX_HTTP_HEADER_BYTES {
                return Err(FetchError::Transport(format!(
                    "HTTP response headers exceed {MAX_HTTP_HEADER_BYTES} bytes"
                )));
            }
        }

        let header_text = std::str::from_utf8(&head_buf[..header_end])
            .map_err(|_| FetchError::Transport("non-UTF-8 HTTP response headers".to_string()))?;
        let mut lines = header_text.split("\r\n");
        let status_line = lines
            .next()
            .ok_or_else(|| FetchError::Transport("empty HTTP response".to_string()))?;
        let status_code = parse_status_code(status_line)?;
        if status_code == 404 {
            return Err(FetchError::NotFound(request.path.clone()));
        }
        if status_code != 200 {
            return Err(FetchError::Transport(format!(
                "HTTP status {status_code} for {}",
                request.path
            )));
        }

        for line in lines {
            if let Some((name, val)) = line.split_once(':')
                && name.trim().eq_ignore_ascii_case("content-length")
                && let Ok(declared) = val.trim().parse::<u64>()
                && declared > request.max_bytes
            {
                return Err(FetchError::SizeLimitExceeded {
                    path: request.path.clone(),
                    limit: request.max_bytes,
                });
            }
        }

        let max_usize =
            usize::try_from(request.max_bytes).map_err(|_| FetchError::SizeLimitExceeded {
                path: request.path.clone(),
                limit: request.max_bytes,
            })?;
        let mut body = Vec::new();
        let mut chunk = [0u8; HTTP_CHUNK_BYTES];
        loop {
            let n = stream
                .read(&mut chunk)
                .map_err(|e| FetchError::Transport(format!("read body: {e}")))?;
            if n == 0 {
                break;
            }
            let remaining = max_usize.saturating_sub(body.len());
            if n > remaining {
                return Err(FetchError::SizeLimitExceeded {
                    path: request.path.clone(),
                    limit: request.max_bytes,
                });
            }
            body.extend_from_slice(&chunk[..n]);
        }
        Ok(body)
    }
}

fn parse_status_code(line: &str) -> Result<u16, FetchError> {
    let mut parts = line.split_whitespace();
    let version = parts
        .next()
        .ok_or_else(|| FetchError::Transport("missing HTTP version".to_string()))?;
    if !version.starts_with("HTTP/") {
        return Err(FetchError::Transport(format!(
            "invalid HTTP status line: {line:?}"
        )));
    }
    let code_str = parts
        .next()
        .ok_or_else(|| FetchError::Transport("missing HTTP status code".to_string()))?;
    code_str
        .parse::<u16>()
        .map_err(|_| FetchError::Transport(format!("invalid HTTP status code: {code_str:?}")))
}

fn load_state(path: &Path) -> Result<ClientState, String> {
    let bytes = fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    if bytes.len() > MAX_STATE_BYTES {
        return Err(format!("state file exceeds {MAX_STATE_BYTES} bytes"));
    }
    let text = String::from_utf8(bytes).map_err(|e| format!("state UTF-8 error: {e}"))?;
    let mut release_key: Option<PublicKey> = None;
    let mut emergency_key: Option<PublicKey> = None;
    let mut arch: Option<Arch> = None;
    let mut channel: Option<Channel> = None;
    let mut installed_release: Option<ReleaseNumber> = None;
    let mut trusted_serial: Option<u64> = None;
    let mut last_waypoint: Option<ReleaseNumber> = None;
    let mut installed_packages = BTreeMap::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (k, v) = trimmed
            .split_once('=')
            .ok_or_else(|| format!("malformed state line: {trimmed:?}"))?;
        let k = k.trim();
        let v = v.trim();
        if let Some(pkg) = k.strip_prefix("pkg.") {
            installed_packages.insert(pkg.to_string(), v.parse::<Blake3Hash>()?);
            continue;
        }
        match k {
            "release_key" => release_key = Some(v.parse::<PublicKey>()?),
            "emergency_key" => emergency_key = Some(v.parse::<PublicKey>()?),
            "arch" => arch = Some(v.parse::<Arch>()?),
            "channel" => channel = Some(v.parse::<Channel>()?),
            "installed_release" => installed_release = Some(v.parse::<ReleaseNumber>()?),
            "trusted_serial" => {
                trusted_serial = Some(
                    v.parse::<u64>()
                        .map_err(|_| format!("invalid trusted_serial {v:?}"))?,
                );
            }
            "last_waypoint" => last_waypoint = Some(v.parse::<ReleaseNumber>()?),
            other => return Err(format!("unknown state key {other:?}")),
        }
    }

    Ok(ClientState {
        release_key: release_key.ok_or("state missing release_key")?,
        emergency_key,
        arch: arch.ok_or("state missing arch")?,
        channel: channel.ok_or("state missing channel")?,
        installed_release: installed_release.ok_or("state missing installed_release")?,
        trusted_serial: trusted_serial.ok_or("state missing trusted_serial")?,
        last_waypoint,
        installed_packages,
    })
}

fn save_state(path: &Path, state: &ClientState) -> Result<(), String> {
    let mut out = String::new();
    out.push_str(&format!("release_key={}\n", state.release_key));
    if let Some(ek) = state.emergency_key {
        out.push_str(&format!("emergency_key={ek}\n"));
    }
    out.push_str(&format!("arch={}\n", state.arch));
    out.push_str(&format!("channel={}\n", state.channel));
    out.push_str(&format!("installed_release={}\n", state.installed_release));
    out.push_str(&format!("trusted_serial={}\n", state.trusted_serial));
    if let Some(wp) = state.last_waypoint {
        out.push_str(&format!("last_waypoint={wp}\n"));
    }
    for (name, head) in &state.installed_packages {
        out.push_str(&format!("pkg.{name}={head}\n"));
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    let tmp = path.with_added_extension("tmp");
    fs::write(&tmp, out.as_bytes()).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path)
        .map_err(|e| format!("renaming {} to {}: {e}", tmp.display(), path.display()))?;
    Ok(())
}

fn next_arg(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn cmd_init(args: &[String]) -> Result<(), String> {
    let mut state_path: Option<PathBuf> = None;
    let mut pubkey: Option<PublicKey> = None;
    let mut arch = Arch::X86_64;
    let mut channel = Channel::Dev;
    let mut installed: Option<ReleaseNumber> = None;
    let mut serial: u64 = 0;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--state" => state_path = Some(PathBuf::from(next_arg(args, &mut i, "--state")?)),
            "--pubkey" => pubkey = Some(next_arg(args, &mut i, "--pubkey")?.parse::<PublicKey>()?),
            "--arch" => arch = next_arg(args, &mut i, "--arch")?.parse::<Arch>()?,
            "--channel" => channel = next_arg(args, &mut i, "--channel")?.parse::<Channel>()?,
            "--installed" => {
                installed = Some(next_arg(args, &mut i, "--installed")?.parse::<ReleaseNumber>()?)
            }
            "--serial" => {
                let raw = next_arg(args, &mut i, "--serial")?;
                serial = raw
                    .parse::<u64>()
                    .map_err(|_| format!("invalid --serial {raw:?}"))?;
            }
            other => return Err(format!("unknown init flag {other}")),
        }
        i += 1;
    }

    let state = ClientState {
        release_key: pubkey.ok_or("init requires --pubkey")?,
        emergency_key: None,
        arch,
        channel,
        installed_release: installed.ok_or("init requires --installed")?,
        trusted_serial: serial,
        last_waypoint: None,
        installed_packages: BTreeMap::new(),
    };
    let path = state_path.ok_or("init requires --state")?;
    save_state(&path, &state)?;
    println!(
        "INITIALIZED release={} serial={}",
        state.installed_release, state.trusted_serial
    );
    Ok(())
}

fn cmd_sync(args: &[String]) -> Result<bool, String> {
    let mut state_path: Option<PathBuf> = None;
    let mut server: Option<String> = None;
    let mut today: Option<CalendarDate> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--state" => state_path = Some(PathBuf::from(next_arg(args, &mut i, "--state")?)),
            "--server" => server = Some(next_arg(args, &mut i, "--server")?),
            "--today" => today = Some(next_arg(args, &mut i, "--today")?.parse::<CalendarDate>()?),
            other => return Err(format!("unknown sync flag {other}")),
        }
        i += 1;
    }

    let path = state_path.ok_or("sync requires --state")?;
    let addr = server.ok_or("sync requires --server")?;
    let today = today.ok_or("sync requires --today")?;

    let mut state = load_state(&path)?;
    let before = state.clone();
    let mut fetcher = HttpFetcher { addr };
    match apply_update(&mut state, today, &mut fetcher, true) {
        Ok(outcome) => {
            save_state(&path, &state)?;
            let (stale_flag, age_days) = match &outcome.stale_notice {
                Some(n) => ("yes", n.age_days),
                None => ("no", 0),
            };
            println!(
                "ACCEPTED release={} serial={} packages={} stale={} age_days={}",
                outcome.target_release,
                outcome.root.serial,
                outcome.verified_packages.len(),
                stale_flag,
                age_days
            );
            Ok(true)
        }
        Err(err) => {
            assert_eq!(state, before);
            println!("REFUSED reason={} detail={}", err.reason_code(), err);
            Ok(false)
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() > MAX_CLI_ARGS {
        eprintln!("update-index-probe: too many arguments");
        return ExitCode::from(2);
    }
    let Some((cmd, rest)) = args.split_first() else {
        eprintln!("usage: update-index-probe <init|sync> ...");
        return ExitCode::from(2);
    };
    match cmd.as_str() {
        "init" => match cmd_init(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("update-index-probe: {e}");
                ExitCode::from(2)
            }
        },
        "sync" => match cmd_sync(rest) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => {
                eprintln!("update-index-probe: {e}");
                ExitCode::from(2)
            }
        },
        other => {
            eprintln!("update-index-probe: unknown subcommand {other:?}");
            ExitCode::from(2)
        }
    }
}

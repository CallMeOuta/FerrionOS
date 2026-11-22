mod audio;

use std::ffi::CStr;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const HELP: &str = "FerrionOS system commands
  ferrion install <package>...     Install packages
  ferrion remove <package>...      Remove packages
  ferrion search <query>           Search packages
  ferrion update [package]...      Update packages (all installed if omitted)
  ferrion shell list               List offered login shells
  ferrion shell set <shell>        Change your login shell for your next login
  ferrion status                   Read system and security status
  ferrion audio status             Read audio format and mixer status
  ferrion audio apps               List audio apps (root)
  ferrion audio volume [0..100] [--app ID | --session ID]
                                  Read or set volume (app/session require root)
  ferrion help                     Show this help

Options (any command): --json, --yes
Changes ask for confirmation; scripts must pass --yes.
Package changes require root. Shell changes use sudo when needed;
run shell set as your own account, not as sudo ferrion.
Audio master volume uses a cubic response. App/session percentages are linear
amplitude: 50 means -6.0206 dB; 0 mutes. Omit the percentage to read volume.
JSON: one object, schema_version 1, command, ok, exit_code, data or error.
Unknown security state is never reported as off. Status is a local observation,
not a remote attestation or a guarantee of containment.
Exit codes: 0 success, 1 operation failed, 2 usage, 3 confirmation declined or
required, 4 permission denied, 5 service or required data unavailable,
6 no package source configured (install, search and update fetch nothing).";

// pkg reads its package sources from the files in this directory (pkg-lib's
// RepoManager::update_remotes).
const PACKAGE_SOURCES: &str = "/etc/pkg.d";
const NO_PACKAGE_SOURCE: i32 = 6;

const SHELLS: [(&str, &str); 5] = [
    ("bash", "/usr/bin/bash"),
    ("fish", "/usr/bin/fish"),
    ("ion", "/usr/bin/ion"),
    ("nushell", "/usr/bin/nu"),
    ("zsh", "/usr/bin/zsh"),
];

#[derive(Debug)]
struct Failure {
    code: i32,
    message: String,
    data: Value,
}

type Result<T, E = Failure> = std::result::Result<T, E>;

fn fail(code: i32, message: impl Into<String>) -> Failure {
    Failure {
        code,
        message: message.into(),
        data: Value::Null,
    }
}

fn io_failure(error: io::Error) -> Failure {
    let code = match error.kind() {
        io::ErrorKind::PermissionDenied => 4,
        io::ErrorKind::NotFound => 5,
        _ => 1,
    };
    fail(code, error.to_string())
}

#[derive(Debug, PartialEq)]
enum Action {
    Help,
    Status,
    Audio(audio::Action),
    ShellList,
    ShellSet(String),
    Package(String, Vec<String>),
}

#[derive(Debug)]
struct Options {
    json: bool,
    yes: bool,
    action: Action,
}

fn parse(args: &[String]) -> Result<Options> {
    let json = args.iter().any(|s| s == "--json");
    let yes = args.iter().any(|s| s == "--yes");
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|s| !matches!(*s, "--json" | "--yes"))
        .collect();
    let action = match words.as_slice() {
        [] | ["help"] | ["--help"] => Action::Help,
        ["status"] => Action::Status,
        ["audio", rest @ ..] => Action::Audio(audio::parse(rest)?),
        ["shell", "list"] => Action::ShellList,
        ["shell", "set", shell] => Action::ShellSet((*shell).into()),
        [op @ ("install" | "remove" | "update" | "search"), rest @ ..] => {
            if (*op != "update" && rest.is_empty()) || (*op == "search" && rest.len() != 1) {
                return Err(fail(
                    2,
                    "Provide packages, or one query for search. See ferrion help.",
                ));
            }
            if rest.iter().any(|s| !valid_package(s)) {
                return Err(fail(
                    2,
                    "Use package names or a search query containing letters, digits, '.', '_' or '-'; start with a letter or digit.",
                ));
            }
            Action::Package((*op).into(), rest.iter().map(|s| (*s).into()).collect())
        }
        _ => return Err(fail(2, "Unknown command or arguments. See ferrion help.")),
    };
    Ok(Options { json, yes, action })
}

fn valid_package(name: &str) -> bool {
    name.as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn confirm(yes: bool, action: &str) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        return Err(fail(
            3,
            "Confirmation required. Review the command, then pass --yes.",
        ));
    }
    eprint!("{action}? [y/N] ");
    io::stderr().flush().map_err(io_failure)?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).map_err(io_failure)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(fail(3, "Canceled. Nothing changed."))
    }
}

fn uid() -> libc::uid_t {
    // getuid has no preconditions and does not trust the caller's environment.
    unsafe { libc::getuid() }
}

fn account() -> Result<(String, String)> {
    // This process is single-threaded. Copy the libc-owned fields before another lookup.
    unsafe {
        let entry = libc::getpwuid(uid());
        if entry.is_null() {
            return Err(fail(5, "Your account is not in the user database."));
        }
        let entry = &*entry;
        if entry.pw_name.is_null() || entry.pw_shell.is_null() {
            return Err(fail(5, "Your account has no login name or shell."));
        }
        Ok((
            CStr::from_ptr(entry.pw_name).to_string_lossy().into_owned(),
            CStr::from_ptr(entry.pw_shell)
                .to_string_lossy()
                .into_owned(),
        ))
    }
}

fn allowed_shells(text: &str) -> Vec<(&'static str, &'static str)> {
    SHELLS
        .into_iter()
        .filter(|(_, path)| {
            text.lines()
                .any(|line| line.split('#').next().unwrap_or("").trim() == *path)
        })
        .collect()
}

fn shell_command(action: &Action, yes: bool) -> Result<Value> {
    let text = fs::read_to_string("/etc/shells").map_err(io_failure)?;
    let offered = allowed_shells(&text);
    let (user, current) = account()?;
    if *action == Action::ShellList {
        return Ok(
            json!({"user": user, "current": current, "shells": offered.iter().map(|(name, path)| {
            json!({"name": name, "path": path, "installed": executable(path)})
        }).collect::<Vec<_>>()}),
        );
    }
    let Action::ShellSet(requested) = action else {
        unreachable!()
    };
    let (_, path) = offered
        .into_iter()
        .find(|(name, path)| {
            requested == name || requested == path || (requested == "nu" && *name == "nushell")
        })
        .ok_or_else(|| fail(2, "Choose an offered shell from ferrion shell list."))?;
    if !executable(path) {
        return Err(fail(
            5,
            "The selected shell is not installed or executable.",
        ));
    }
    if current == path {
        return Ok(json!({"user": user, "shell": path, "changed": false}));
    }
    confirm(yes, &format!("Change {user}'s login shell to {path}"))?;
    let mut command = if uid() == 0 {
        Command::new("/usr/bin/usermod")
    } else {
        if !io::stdin().is_terminal() {
            return Err(fail(
                4,
                "Shell changes need sudo authentication on a terminal.",
            ));
        }
        let mut command = Command::new("/usr/bin/sudo");
        command.arg("/usr/bin/usermod");
        command
    };
    // sudo's password prompt belongs on stderr, never in the JSON stream.
    let status = command
        .args(["-s", path, &user])
        .stdout(Stdio::from(io::stderr()))
        .status()
        .map_err(io_failure)?;
    if !status.success() {
        return Err(fail(
            1,
            "usermod failed. The login shell was not confirmed changed.",
        ));
    }
    if account()?.1 != path {
        return Err(fail(
            1,
            "The saved login shell does not match the requested shell.",
        ));
    }
    Ok(json!({"user": user, "shell": path, "changed": true, "effective": "next login"}))
}

fn executable(path: &str) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The sources pkg would fetch from: each line of a file in `dir` that is not a comment.
/// A missing directory names none.
fn package_sources(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut sources = Vec::new();
    for path in entries.filter_map(|entry| Some(entry.ok()?.path())) {
        if let Ok(text) = fs::read_to_string(&path) {
            sources.extend(
                text.lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                    .map(str::to_owned),
            );
        }
    }
    sources
}

fn package_failed(success: bool, stderr: &str) -> bool {
    // Today's pkg reports its operation errors but returns zero (pkg-cli/src/main.rs).
    // Capture removes terminal colors, so the explicit error prefix is unambiguous.
    !success || stderr.lines().any(|line| line.starts_with("error:"))
}

fn package_command(op: &str, packages: &[String], yes: bool) -> Result<Value> {
    // Only remove works without a source; the others would fetch the source's index.
    if op != "remove" && package_sources(Path::new(PACKAGE_SOURCES)).is_empty() {
        return Err(Failure {
            code: NO_PACKAGE_SOURCE,
            message: format!(
                "This image has no package source yet: FerrionOS does not publish packages, and {PACKAGE_SOURCES} names none. Nothing was fetched or changed."
            ),
            data: json!({"packages": packages, "package_source": "none"}),
        });
    }
    if op != "search" {
        if uid() != 0 {
            return Err(fail(
                4,
                "Package changes require root. Run this command with sudo.",
            ));
        }
        let targets = if packages.is_empty() {
            "all installed packages".into()
        } else {
            packages.join(", ")
        };
        confirm(yes, &format!("{op} {targets}"))?;
    }
    let output = Command::new("/usr/bin/pkg")
        .arg(op)
        .args(packages)
        .arg("--yes")
        .stdin(Stdio::null())
        .output()
        .map_err(io_failure)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut data = json!({"packages": packages, "stdout": stdout, "stderr": stderr, "backend_exit_code": output.status.code()});
    if package_failed(output.status.success(), &stderr) {
        return Err(Failure {
            code: 1,
            message: "pkg failed. See the captured output.".into(),
            data,
        });
    }
    if op == "search" {
        data["matches"] = json!(
            stdout
                .lines()
                .filter_map(|line| {
                    let (index, name) = line.split_once(": ")?;
                    index.parse::<usize>().ok()?;
                    Some(name)
                })
                .collect::<Vec<_>>()
        );
    }
    Ok(data)
}

fn field<'a>(text: &'a str, key: &str, delimiter: char) -> Option<&'a str> {
    text.lines()
        .filter_map(|line| line.split_once(delimiter))
        .find_map(|(name, value)| (name.trim() == key).then(|| value.trim()))
}

fn observation(state: &str, source: &str, detail: &str) -> Value {
    json!({"state": state, "source": source, "detail": detail})
}

fn randomness(text: &str) -> Value {
    let state = match field(text, "seeded", ':') {
        Some("yes") => "on",
        Some("no") => "off",
        _ => "unknown",
    };
    observation(
        state,
        "/scheme/rand/status",
        "Seeded randomness, not merely available output",
    )
}

fn lockdown() -> Value {
    if uid() != 0 {
        return observation(
            "unknown",
            "/scheme/memory/physical",
            "Root is required for this access probe",
        );
    }
    match fs::File::open("/scheme/memory/physical") {
        Ok(_) => observation(
            "off",
            "/scheme/memory/physical",
            "Root can open physical memory",
        ),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => observation(
            "on",
            "/scheme/memory/physical",
            "Physical-memory access denied to this root process; not a full lockdown audit",
        ),
        Err(e) => observation("unknown", "/scheme/memory/physical", &e.to_string()),
    }
}

fn measured_boot() -> Value {
    if uid() != 0 {
        return observation(
            "unknown",
            "attest-statement",
            "Root is required to read the verdict",
        );
    }
    match Command::new("/usr/bin/attest-statement")
        .stdin(Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            let verdict = field(&text, "verdict", ':').unwrap_or("unknown");
            observation(
                verdict,
                "attest-statement",
                "Local verdict; no remote Verifier appraisal",
            )
        }
        Ok(output) => observation(
            "unknown",
            "attest-statement",
            &String::from_utf8_lossy(&output.stderr),
        ),
        Err(e) => observation("unknown", "attest-statement", &e.to_string()),
    }
}

fn system_status() -> Result<Value> {
    let release = fs::read_to_string("/etc/os-release")
        .or_else(|_| fs::read_to_string("/usr/lib/os-release"))
        .map_err(io_failure)?;
    let version = field(&release, "VERSION_ID", '=')
        .ok_or_else(|| fail(5, "os-release has no VERSION_ID."))?
        .trim_matches(['\'', '"']);
    let random = match fs::read_to_string("/scheme/rand/status") {
        Ok(text) => randomness(&text),
        Err(e) => observation("unknown", "/scheme/rand/status", &e.to_string()),
    };
    Ok(json!({"version": version,
        "verified_boot": observation("unknown", "boot environment", "This boot exposes no verification status"),
        "measured_boot": measured_boot(), "lockdown": lockdown(),
        "disk_encryption": observation("unknown", "root filesystem", "This filesystem exposes no encryption status"),
        "seeded_randomness": random}))
}

fn execute(options: &Options) -> Result<Value> {
    match &options.action {
        Action::Help => Ok(json!({"help": HELP})),
        Action::Status => system_status(),
        Action::Audio(action) => audio::execute(action, options.yes),
        Action::ShellList | Action::ShellSet(_) => shell_command(&options.action, options.yes),
        Action::Package(op, packages) => package_command(op, packages, options.yes),
    }
}

fn command_name(action: &Action) -> &str {
    match action {
        Action::Help => "help",
        Action::Status => "status",
        Action::Audio(action) => audio::name(action),
        Action::ShellList => "shell list",
        Action::ShellSet(_) => "shell set",
        Action::Package(op, _) => op,
    }
}

fn response(command: &str, result: Result<Value>) -> (i32, Value) {
    match result {
        Ok(data) => (
            0,
            json!({"schema_version": 1, "command": command, "ok": true, "exit_code": 0, "data": data}),
        ),
        Err(error) => (
            error.code,
            json!({"schema_version": 1, "command": command, "ok": false, "exit_code": error.code, "error": error.message, "data": error.data}),
        ),
    }
}

fn print_text(action: Option<&Action>, value: &Value) {
    if let Some(error) = value["error"].as_str() {
        eprintln!("ferrion: {error}");
    }
    let Some(action) = action else {
        return;
    };
    let data = &value["data"];
    if value["ok"] != true {
        if let Action::Package(..) = action
            && data.get("stdout").is_some()
        {
            print!("{}", data["stdout"].as_str().unwrap_or(""));
            eprint!("{}", data["stderr"].as_str().unwrap_or(""));
        }
        return;
    }
    match action {
        Action::Help => {
            if let Some(help) = data["help"].as_str() {
                println!("{help}");
            }
        }
        Action::Audio(audio_action) => audio::print(audio_action, data),
        Action::Package(..) => {
            if data.get("stdout").is_some() {
                print!("{}", data["stdout"].as_str().unwrap_or(""));
                eprint!("{}", data["stderr"].as_str().unwrap_or(""));
            }
        }
        Action::ShellList => {
            if let Some(shells) = data["shells"].as_array() {
                for shell in shells {
                    println!(
                        "{}  {}{}{}",
                        shell["name"].as_str().unwrap_or(""),
                        shell["path"].as_str().unwrap_or(""),
                        if shell["path"] == data["current"] {
                            " (current)"
                        } else {
                            ""
                        },
                        if shell["installed"] == false {
                            " (not installed)"
                        } else {
                            ""
                        }
                    );
                }
            }
        }
        Action::ShellSet(_) => {
            if let Some(shell) = data["shell"].as_str() {
                println!(
                    "Login shell: {shell}{}",
                    if data["changed"] == true {
                        " (next login)"
                    } else {
                        " (unchanged)"
                    }
                );
            }
        }
        Action::Status => {
            if let Some(version) = data["version"].as_str() {
                println!("FerrionOS {version}");
                for (key, label) in [
                    ("verified_boot", "Verified boot"),
                    ("measured_boot", "Measured boot"),
                    ("lockdown", "Lockdown"),
                    ("disk_encryption", "Disk encryption"),
                    ("seeded_randomness", "Seeded randomness"),
                ] {
                    println!(
                        "{label}: {} — {}",
                        data[key]["state"].as_str().unwrap_or("unknown"),
                        data[key]["detail"].as_str().unwrap_or("")
                    );
                }
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut json = args.iter().any(|s| s == "--json");
    let (action, code, value) = match parse(&args) {
        Ok(options) => {
            json = options.json;
            let (code, value) = response(command_name(&options.action), execute(&options));
            (Some(options.action), code, value)
        }
        Err(error) => {
            let (code, value) = response("invalid", Err(error));
            (None, code, value)
        }
    };
    if json {
        println!("{value}");
    } else {
        print_text(action.as_ref(), &value);
    }
    std::process::exit(code);
}

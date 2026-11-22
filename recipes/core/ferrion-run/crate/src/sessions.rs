use std::fs;
use std::path::{Path, PathBuf};

use ferrion_scope::links::Outside;
use ferrion_scope::overlay::{LAYER, LOWER, Session};

use crate::STATE_DIR;

pub const PID: &str = "pid";
pub const CLONE_PREFIX: &str = "clone-";

pub fn state_root() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(Path::new(&home).join(STATE_DIR))
}

pub fn clone_dirs(session: &Path) -> Result<Vec<PathBuf>, String> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(session)
        .map_err(|e| format!("cannot read {}: {e}", session.display()))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(CLONE_PREFIX)
        })
        .map(|entry| entry.path())
        .filter(|path| path.join(LAYER).is_dir() && path.join(LOWER).is_file())
        .collect();
    dirs.sort();
    Ok(dirs)
}

fn running(session: &Path) -> bool {
    let Ok(text) = fs::read_to_string(session.join(PID)) else {
        return false;
    };
    let Ok(pid) = text.trim().parse::<i32>() else {
        return false;
    };
    let found = unsafe { libc::kill(pid, 0) } == 0;
    found || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

fn session_dir(id: &str) -> Result<PathBuf, String> {
    let dir = state_root()?.join(id);
    if !dir.is_dir() {
        return Err(format!("no session {id} in {}", state_root()?.display()));
    }
    if running(&dir) {
        return Err(format!("session {id} is still running"));
    }
    Ok(dir)
}

pub fn list() -> Result<(), String> {
    let root = state_root()?;
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("cannot read {}: {e}", root.display())),
    };
    let mut ids: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    for id in ids {
        let dir = root.join(&id);
        let state = if running(&dir) { "running" } else { "ended" };
        for clone in clone_dirs(&dir)? {
            let lower = fs::read_to_string(clone.join(LOWER)).unwrap_or_default();
            println!("{id} {state} {}", lower.trim_end());
        }
    }
    Ok(())
}

pub fn diff(id: &str) -> Result<(), String> {
    let dir = session_dir(id)?;
    for clone in clone_dirs(&dir)? {
        let session =
            Session::open(&clone).map_err(|e| format!("cannot read {}: {e}", clone.display()))?;
        println!("{}:", session.lower.display());
        for change in session
            .changes()
            .map_err(|e| format!("cannot compare {}: {e}", session.lower.display()))?
        {
            println!("{}", change.line());
        }
        for (rel, outside) in outside_links(&session)? {
            println!("! {rel} -> {} leads outside the folder", outside.describe());
        }
    }
    Ok(())
}

fn outside_links(session: &Session) -> Result<Vec<(String, Outside)>, String> {
    session.outside_links().map_err(|e| {
        format!(
            "cannot read the symlinks of {}: {e}",
            session.layer.display()
        )
    })
}

pub fn apply(id: &str) -> Result<(), String> {
    let dir = session_dir(id)?;
    for clone in clone_dirs(&dir)? {
        let session =
            Session::open(&clone).map_err(|e| format!("cannot read {}: {e}", clone.display()))?;
        let changes = session
            .changes()
            .map_err(|e| format!("cannot compare {}: {e}", session.lower.display()))?;
        for (rel, outside) in outside_links(&session)? {
            eprintln!(
                "ferrion-run: writing a symlink that leads outside {}: {rel} -> {}",
                session.lower.display(),
                outside.describe()
            );
        }
        session
            .apply()
            .map_err(|e| format!("cannot apply to {}: {e}", session.lower.display()))?;
        println!(
            "{}: {} changes applied",
            session.lower.display(),
            changes.len()
        );
    }
    fs::remove_dir_all(&dir).map_err(|e| format!("cannot remove {}: {e}", dir.display()))
}

pub fn discard(id: &str) -> Result<(), String> {
    let dir = session_dir(id)?;
    fs::remove_dir_all(&dir).map_err(|e| format!("cannot remove {}: {e}", dir.display()))
}

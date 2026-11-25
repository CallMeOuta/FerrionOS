use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use ferrion_manifest::{
    GrantsFile, MAX_GRANTS_FILE_BYTES, MAX_INSTALLED_MANIFESTS, MAX_MANIFEST_BYTES, Manifest,
    Origin, parse_grants_bytes, parse_manifest_bytes,
};

use crate::{MAX_PATH_ENTRIES, is_system_exec_path, normalize_system_exec};

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, std::io::Error> {
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub fn resolve_program_canonical(program: &OsStr) -> Option<PathBuf> {
    let as_path = Path::new(program);
    if program.to_string_lossy().contains('/') {
        return fs::canonicalize(as_path).ok();
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var).take(MAX_PATH_ENTRIES) {
        let candidate = dir.join(program);
        if candidate.is_file()
            && let Ok(canon) = fs::canonicalize(&candidate)
        {
            return Some(canon);
        }
    }
    None
}

pub fn load_manifest_for_exec(
    manifest_dir: &Path,
    resolved_exec: &Path,
) -> Result<Option<Manifest>, String> {
    let Some(resolved_str) = resolved_exec.to_str() else {
        return Ok(None);
    };
    if !is_system_exec_path(resolved_str) {
        return Ok(None);
    }
    let entries = match fs::read_dir(manifest_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", manifest_dir.display())),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", manifest_dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        files.push(path);
        if files.len() > MAX_INSTALLED_MANIFESTS {
            return Err(format!(
                "{}: too-many-manifests (limit {MAX_INSTALLED_MANIFESTS})",
                manifest_dir.display()
            ));
        }
    }
    files.sort();
    let mut seen_execs: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut matched: Option<Manifest> = None;
    for path in files {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            return Err(format!(
                "{}: invalid-id (non-UTF-8 file name)",
                path.display()
            ));
        };
        let bytes = read_bounded(&path, MAX_MANIFEST_BYTES)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let manifest = parse_manifest_bytes(&bytes, Origin::Repository { package: stem })
            .map_err(|err| format!("{}: {} ({err})", path.display(), err.reason_code()))?;
        let exec_key = match fs::canonicalize(&manifest.exec) {
            Ok(canon) => canon
                .to_str()
                .map(str::to_owned)
                .unwrap_or_else(|| normalize_system_exec(&manifest.exec)),
            Err(_) => normalize_system_exec(&manifest.exec),
        };
        if let Some(prev) = seen_execs.insert(exec_key.clone(), path.clone()) {
            return Err(format!(
                "{}: duplicate-exec {exec_key:?} (also in {})",
                path.display(),
                prev.display()
            ));
        }
        if exec_key == resolved_str {
            matched = Some(manifest);
        }
    }
    Ok(matched)
}

pub fn load_grants_file(path: &Path) -> Result<GrantsFile, String> {
    let bytes = match read_bounded(path, MAX_GRANTS_FILE_BYTES) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(GrantsFile::empty()),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    parse_grants_bytes(&bytes)
        .map_err(|err| format!("{}: {} ({err})", path.display(), err.reason_code()))
}

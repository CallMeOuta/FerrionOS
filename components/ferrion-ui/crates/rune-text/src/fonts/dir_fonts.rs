use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use parley::fontique::{Collection, FallbackKey, FamilyId, Script};

static LOGGED_DIR_ERROR: AtomicBool = AtomicBool::new(false);

pub const DEFAULT_FONT_DIRS: &[&str] = &["/usr/share/fonts"];
pub const FONT_DIRS_ENV: &str = "RUNE_FONT_DIRS";

pub const MAX_FONT_DIRS: usize = 8;
pub const MAX_SCAN_DEPTH: usize = 6;
pub const MAX_FONT_FILES: usize = 256;
pub const MAX_FONT_FILE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FontDirError {
    TooManyDirs { count: usize, limit: usize },
    DepthExceeded { path: PathBuf, depth: usize, limit: usize },
    TooManyFiles { count: usize, limit: usize },
    FileTooLarge { path: PathBuf, bytes: u64, limit: u64 },
    Io { path: PathBuf, message: String },
}

impl fmt::Display for FontDirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FontDirError::TooManyDirs { count, limit } => {
                write!(f, "font directory count {count} exceeds limit {limit}")
            }
            FontDirError::DepthExceeded { path, depth, limit } => {
                write!(f, "font directory {} depth {depth} exceeds limit {limit}", path.display())
            }
            FontDirError::TooManyFiles { count, limit } => {
                write!(f, "font file count {count} exceeds limit {limit}")
            }
            FontDirError::FileTooLarge { path, bytes, limit } => {
                write!(f, "font file {} size {bytes} bytes exceeds limit {limit}", path.display())
            }
            FontDirError::Io { path, message } => {
                write!(f, "font directory {}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for FontDirError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirFontSummary {
    pub files: usize,
    pub families: usize,
}

struct ScriptSample {
    tag: [u8; 4],
    samples: [char; 4],
}

const SCRIPT_SAMPLES: &[ScriptSample] = &[
    ScriptSample { tag: *b"Arab", samples: ['\u{0627}', '\u{0628}', '\u{0645}', '\u{0631}'] },
    ScriptSample { tag: *b"Hebr", samples: ['\u{05D0}', '\u{05D1}', '\u{05E9}', '\u{05DC}'] },
    ScriptSample { tag: *b"Deva", samples: ['\u{0928}', '\u{092E}', '\u{0938}', '\u{0924}'] },
    ScriptSample { tag: *b"Thai", samples: ['\u{0E2A}', '\u{0E27}', '\u{0E14}', '\u{0E35}'] },
    ScriptSample { tag: *b"Hani", samples: ['\u{4E2D}', '\u{6587}', '\u{65E5}', '\u{672C}'] },
    ScriptSample { tag: *b"Hira", samples: ['\u{3042}', '\u{3044}', '\u{3046}', '\u{306E}'] },
    ScriptSample { tag: *b"Kana", samples: ['\u{30A2}', '\u{30A4}', '\u{30A6}', '\u{30F3}'] },
    ScriptSample { tag: *b"Hang", samples: ['\u{D55C}', '\u{AE00}', '\u{AC00}', '\u{B098}'] },
];

pub fn configured_font_dirs() -> Vec<PathBuf> {
    if let Some(raw) = rune_core::env::var(FONT_DIRS_ENV) {
        return raw.split(':').filter(|s| !s.is_empty()).map(PathBuf::from).collect();
    }
    if cfg!(target_os = "redox") {
        return DEFAULT_FONT_DIRS.iter().map(PathBuf::from).collect();
    }
    Vec::new()
}

fn is_font_extension(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    ext.eq_ignore_ascii_case("ttf")
        || ext.eq_ignore_ascii_case("otf")
        || ext.eq_ignore_ascii_case("ttc")
        || ext.eq_ignore_ascii_case("otc")
}

pub fn collect_font_files(dirs: &[PathBuf]) -> Result<Vec<PathBuf>, FontDirError> {
    let (files, err) = collect_font_files_bounded(dirs);
    if let Some(err) = err {
        return Err(err);
    }
    Ok(files)
}

fn collect_font_files_bounded(dirs: &[PathBuf]) -> (Vec<PathBuf>, Option<FontDirError>) {
    let mut first_err: Option<FontDirError> = None;
    let capped_dirs = if dirs.len() > MAX_FONT_DIRS {
        first_err.get_or_insert(FontDirError::TooManyDirs {
            count: dirs.len(),
            limit: MAX_FONT_DIRS,
        });
        &dirs[..MAX_FONT_DIRS]
    } else {
        dirs
    };
    let mut files = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = Vec::new();
    for root in capped_dirs {
        match std::fs::symlink_metadata(root) {
            Ok(meta) => {
                if meta.is_dir() {
                    stack.push((root.clone(), 0));
                } else if meta.is_file() {
                    record_font_file(root, meta.len(), &mut files, &mut first_err);
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                first_err.get_or_insert(FontDirError::Io {
                    path: root.clone(),
                    message: err.to_string(),
                });
            }
        }
    }
    while let Some((dir, depth)) = stack.pop() {
        assert!(depth <= MAX_SCAN_DEPTH);
        let read_dir = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(err) => {
                first_err.get_or_insert(FontDirError::Io {
                    path: dir.clone(),
                    message: err.to_string(),
                });
                continue;
            }
        };
        let mut entries: Vec<PathBuf> = Vec::new();
        for entry in read_dir {
            match entry {
                Ok(e) => entries.push(e.path()),
                Err(err) => {
                    first_err.get_or_insert(FontDirError::Io {
                        path: dir.clone(),
                        message: err.to_string(),
                    });
                }
            }
        }
        entries.sort();
        for path in entries {
            let sym_meta = match std::fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                Err(err) => {
                    first_err.get_or_insert(FontDirError::Io {
                        path: path.clone(),
                        message: err.to_string(),
                    });
                    continue;
                }
            };
            if sym_meta.file_type().is_symlink() {
                let target_meta = match std::fs::metadata(&path) {
                    Ok(meta) => meta,
                    Err(_) => continue,
                };
                if target_meta.is_file() {
                    record_font_file(&path, target_meta.len(), &mut files, &mut first_err);
                }
                continue;
            }
            if sym_meta.is_dir() {
                let next_depth = depth + 1;
                if next_depth > MAX_SCAN_DEPTH {
                    first_err.get_or_insert(FontDirError::DepthExceeded {
                        path,
                        depth: next_depth,
                        limit: MAX_SCAN_DEPTH,
                    });
                    continue;
                }
                stack.push((path, next_depth));
            } else if sym_meta.is_file() {
                record_font_file(&path, sym_meta.len(), &mut files, &mut first_err);
            }
        }
    }
    files.sort();
    files.dedup();
    assert!(files.len() <= MAX_FONT_FILES);
    (files, first_err)
}

fn record_font_file(
    path: &Path,
    bytes: u64,
    files: &mut Vec<PathBuf>,
    first_err: &mut Option<FontDirError>,
) {
    if !is_font_extension(path) {
        return;
    }
    if bytes > MAX_FONT_FILE_BYTES {
        first_err.get_or_insert(FontDirError::FileTooLarge {
            path: path.to_path_buf(),
            bytes,
            limit: MAX_FONT_FILE_BYTES,
        });
        return;
    }
    if files.len() >= MAX_FONT_FILES {
        first_err.get_or_insert(FontDirError::TooManyFiles {
            count: files.len() + 1,
            limit: MAX_FONT_FILES,
        });
        return;
    }
    files.push(path.to_path_buf());
}

fn is_cjk_script(tag: [u8; 4]) -> bool {
    matches!(&tag, b"Hani" | b"Hira" | b"Kana" | b"Hang")
}

fn register_font_files(
    collection: &mut Collection,
    files: &[PathBuf],
    primary_families: (&str, &str),
    cjk_families: &mut Vec<FamilyId>,
) -> DirFontSummary {
    if files.is_empty() {
        return DirFontSummary::default();
    }
    let before: HashSet<FamilyId> = collection.family_ids().collect();
    collection.load_fonts_from_paths(files);
    let mut added: Vec<FamilyId> = collection
        .family_ids()
        .filter(|id| !before.contains(id))
        .collect();
    added.sort_unstable();

    let primary = [
        collection.family_id(primary_families.0),
        collection.family_id(primary_families.1),
    ];
    let mut by_source: Vec<(parley::fontique::SourceId, Vec<(FamilyId, parley::fontique::FontInfo)>)> =
        Vec::new();
    for &id in &added {
        if primary.contains(&Some(id)) {
            continue;
        }
        let Some(family) = collection.family(id) else { continue };
        let Some(first) = family.fonts().first().cloned() else { continue };
        let source_id = first.source().id();
        if let Some((_, list)) = by_source.iter_mut().find(|(sid, _)| *sid == source_id) {
            list.push((id, first));
        } else {
            by_source.push((source_id, vec![(id, first)]));
        }
    }

    let mut script_families: [Vec<FamilyId>; 8] = Default::default();
    for (_, group) in by_source {
        let Some((_, first_font)) = group.first() else { continue };
        let Some(blob) = first_font.load(None) else { continue };
        let bytes = blob.as_ref();
        for (family_id, font_info) in group {
            let Some(charmap) = font_info.charmap_index().charmap(bytes) else { continue };
            for (idx, spec) in SCRIPT_SAMPLES.iter().enumerate() {
                if spec.samples.iter().all(|&ch| charmap.map(ch).is_some_and(|gid| gid != 0))
                    && !script_families[idx].contains(&family_id)
                {
                    script_families[idx].push(family_id);
                }
            }
        }
    }

    let mut cjk_added = false;
    for (idx, spec) in SCRIPT_SAMPLES.iter().enumerate() {
        let families = &script_families[idx];
        if families.is_empty() {
            continue;
        }
        if is_cjk_script(spec.tag) {
            for &id in families {
                if !cjk_families.contains(&id) {
                    cjk_families.push(id);
                    cjk_added = true;
                }
            }
        } else {
            if matches!(&spec.tag, b"Arab" | b"Hebr") {
                collection.append_fallbacks(
                    FallbackKey::new(Script::from_bytes(*b"Latn"), None),
                    families.iter().copied(),
                );
            }
            collection.append_fallbacks(
                FallbackKey::new(Script::from_bytes(spec.tag), None),
                families.iter().copied(),
            );
        }
    }
    if cjk_added {
        super::language::register(collection, cjk_families);
    }

    DirFontSummary { files: files.len(), families: added.len() }
}

pub(crate) fn register_configured_font_dirs(
    collection: &mut Collection,
    primary_families: (&str, &str),
    cjk_families: &mut Vec<FamilyId>,
) -> DirFontSummary {
    let dirs = configured_font_dirs();
    if dirs.is_empty() {
        return DirFontSummary::default();
    }
    let (files, err) = collect_font_files_bounded(&dirs);
    if let Some(err) = err
        && !LOGGED_DIR_ERROR.swap(true, Ordering::Relaxed)
    {
        eprintln!("rune-text: {err}");
    }
    register_font_files(collection, &files, primary_families, cjk_families)
}

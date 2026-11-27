#![forbid(unsafe_code)]

use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value};

use super::{FileDialogError, FileDialogKind, FileDialogOptions, FileDialogResult};

mod process;

pub use process::MAX_KILL_WAIT;
#[cfg(feature = "fault-hooks")]
pub(crate) use process::set_fault_for_test;
#[cfg(all(feature = "native-file-dialog", target_os = "redox"))]
pub(super) use process::show;
#[cfg(all(
    feature = "native-file-dialog",
    any(target_os = "linux", target_os = "redox")
))]
pub(super) use process::show_with_path;
pub(crate) use process::shutdown;
#[cfg(feature = "native-file-dialog")]
pub(super) use process::tooling_override_path;

pub const PROTOCOL_VERSION: u32 = 1;
pub const DEFAULT_CHOOSER_PATH: &str = "/usr/libexec/ferrion-file-chooser";
pub const MAX_IPC_BYTES: usize = 65_536;
pub const MAX_PATH_BYTES: usize = 4_096;
pub const MAX_FILTERS: usize = 32;
pub const MAX_FILTER_EXTENSIONS: usize = 64;
pub const MAX_TITLE_BYTES: usize = 256;
pub const MAX_FILENAME_BYTES: usize = 256;
pub const MAX_ERROR_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantMode {
    Read,
    Write,
    Dir,
}

impl GrantMode {
    pub const fn for_kind(kind: FileDialogKind) -> Self {
        match kind {
            FileDialogKind::Open => Self::Read,
            FileDialogKind::Save => Self::Write,
            FileDialogKind::Folder => Self::Dir,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Dir => "dir",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            "dir" => Some(Self::Dir),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChooserRequest {
    pub version: u32,
    pub kind: FileDialogKind,
    pub options: FileDialogOptions,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChooserReply {
    Ok {
        mode: GrantMode,
        path: PathBuf,
        overwrite_confirmed: bool,
    },
    Cancel,
    Error(String),
}

pub fn encode_request(req: &ChooserRequest) -> Result<Vec<u8>, FileDialogError> {
    if req.version != PROTOCOL_VERSION {
        return Err(FileDialogError(format!(
            "unsupported chooser protocol version {}",
            req.version
        )));
    }
    if req.options.title.len() > MAX_TITLE_BYTES || req.options.title.contains('\0') {
        return Err(FileDialogError("file chooser title is invalid".into()));
    }
    if req.options.filters.len() > MAX_FILTERS {
        return Err(FileDialogError(format!(
            "file chooser filters exceed limit of {MAX_FILTERS}"
        )));
    }
    let mut obj = Map::new();
    obj.insert("version".into(), Value::from(req.version));
    obj.insert("kind".into(), Value::from(kind_to_str(req.kind)));
    obj.insert("title".into(), Value::from(req.options.title.as_str()));
    if let Some(dir) = &req.options.directory {
        let dir_str = path_to_valid_str(dir, "directory")?;
        obj.insert("directory".into(), Value::from(dir_str));
    }
    if let Some(name) = &req.options.file_name {
        if name.len() > MAX_FILENAME_BYTES || name.contains('\0') {
            return Err(FileDialogError("file chooser file_name is invalid".into()));
        }
        obj.insert("file_name".into(), Value::from(name.as_str()));
    }
    let mut filters = Vec::with_capacity(req.options.filters.len());
    for (name, exts) in &req.options.filters {
        if exts.len() > MAX_FILTER_EXTENSIONS {
            return Err(FileDialogError(format!(
                "file chooser filter extensions exceed limit of {MAX_FILTER_EXTENSIONS}"
            )));
        }
        let mut f = Map::new();
        f.insert("name".into(), Value::from(name.as_str()));
        let ext_vals: Vec<Value> = exts.iter().map(|e| Value::from(e.as_str())).collect();
        f.insert("extensions".into(), Value::Array(ext_vals));
        filters.push(Value::Object(f));
    }
    obj.insert("filters".into(), Value::Array(filters));
    let mut bytes = serde_json::to_vec(&Value::Object(obj))
        .map_err(|e| FileDialogError(format!("file chooser request encode failed: {e}")))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_IPC_BYTES {
        return Err(FileDialogError(format!(
            "file chooser request exceeds {MAX_IPC_BYTES} bytes"
        )));
    }
    Ok(bytes)
}

pub fn decode_request(raw: &[u8]) -> Result<ChooserRequest, FileDialogError> {
    if raw.is_empty() || raw.len() > MAX_IPC_BYTES {
        return Err(FileDialogError(format!(
            "file chooser request size {} out of bounds (1..={MAX_IPC_BYTES})",
            raw.len()
        )));
    }
    let text = std::str::from_utf8(raw)
        .map_err(|_| FileDialogError("file chooser request is not valid UTF-8".into()))?
        .trim();
    let val: Value = serde_json::from_str(text)
        .map_err(|e| FileDialogError(format!("file chooser request JSON invalid: {e}")))?;
    let obj = val
        .as_object()
        .ok_or_else(|| FileDialogError("file chooser request must be a JSON object".into()))?;
    let version = obj
        .get("version")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| FileDialogError("file chooser request missing version".into()))?;
    if version != PROTOCOL_VERSION {
        return Err(FileDialogError(format!(
            "file chooser protocol version mismatch: got {version}, expected {PROTOCOL_VERSION}"
        )));
    }
    let kind_str = obj
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| FileDialogError("file chooser request missing kind".into()))?;
    let kind = str_to_kind(kind_str)
        .ok_or_else(|| FileDialogError(format!("unknown file chooser kind: {kind_str}")))?;
    let title = obj
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if title.len() > MAX_TITLE_BYTES || title.contains('\0') {
        return Err(FileDialogError("file chooser request title invalid".into()));
    }
    let directory = match obj.get("directory") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            if s.len() > MAX_PATH_BYTES || s.contains('\0') {
                return Err(FileDialogError("file chooser directory invalid".into()));
            }
            Some(PathBuf::from(s))
        }
        Some(_) => {
            return Err(FileDialogError(
                "file chooser directory must be a string".into(),
            ));
        }
    };
    let file_name = match obj.get("file_name") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            if s.len() > MAX_FILENAME_BYTES || s.contains('\0') {
                return Err(FileDialogError("file chooser file_name invalid".into()));
            }
            Some(s.clone())
        }
        Some(_) => {
            return Err(FileDialogError(
                "file chooser file_name must be a string".into(),
            ));
        }
    };
    let mut filters = Vec::new();
    if let Some(arr) = obj.get("filters") {
        let items = arr
            .as_array()
            .ok_or_else(|| FileDialogError("file chooser filters must be an array".into()))?;
        if items.len() > MAX_FILTERS {
            return Err(FileDialogError(format!(
                "file chooser filters exceed limit of {MAX_FILTERS}"
            )));
        }
        for item in items {
            let fobj = item
                .as_object()
                .ok_or_else(|| FileDialogError("file chooser filter must be an object".into()))?;
            let fname = fobj
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let exts_arr = fobj
                .get("extensions")
                .and_then(Value::as_array)
                .ok_or_else(|| FileDialogError("file chooser filter missing extensions".into()))?;
            if exts_arr.len() > MAX_FILTER_EXTENSIONS {
                return Err(FileDialogError(format!(
                    "file chooser filter extensions exceed limit of {MAX_FILTER_EXTENSIONS}"
                )));
            }
            let mut exts = Vec::with_capacity(exts_arr.len());
            for ext in exts_arr {
                let s = ext
                    .as_str()
                    .ok_or_else(|| FileDialogError("filter extension must be a string".into()))?;
                exts.push(s.to_owned());
            }
            filters.push((fname, exts));
        }
    }
    Ok(ChooserRequest {
        version,
        kind,
        options: FileDialogOptions {
            title,
            directory,
            file_name,
            filters,
        },
    })
}

pub fn encode_reply(reply: &ChooserReply) -> Result<Vec<u8>, FileDialogError> {
    let mut obj = Map::new();
    obj.insert("version".into(), Value::from(PROTOCOL_VERSION));
    match reply {
        ChooserReply::Ok {
            mode,
            path,
            overwrite_confirmed,
        } => {
            let path_str = path_to_valid_str(path, "reply path")?;
            obj.insert("status".into(), Value::from("ok"));
            obj.insert("mode".into(), Value::from(mode.as_str()));
            obj.insert("path".into(), Value::from(path_str));
            obj.insert(
                "overwrite_confirmed".into(),
                Value::from(*overwrite_confirmed),
            );
        }
        ChooserReply::Cancel => {
            obj.insert("status".into(), Value::from("cancel"));
        }
        ChooserReply::Error(msg) => {
            let bounded = truncate_str(msg, MAX_ERROR_BYTES);
            obj.insert("status".into(), Value::from("error"));
            obj.insert("message".into(), Value::from(bounded));
        }
    }
    let mut bytes = serde_json::to_vec(&Value::Object(obj))
        .map_err(|e| FileDialogError(format!("file chooser reply encode failed: {e}")))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_IPC_BYTES {
        return Err(FileDialogError(format!(
            "file chooser reply exceeds {MAX_IPC_BYTES} bytes"
        )));
    }
    Ok(bytes)
}

pub fn validate_reply(kind: FileDialogKind, raw: &[u8]) -> FileDialogResult {
    if raw.is_empty() {
        return Err(FileDialogError(
            "file chooser closed without a reply".into(),
        ));
    }
    if raw.len() > MAX_IPC_BYTES {
        return Err(FileDialogError(format!(
            "file chooser reply exceeded {MAX_IPC_BYTES} bytes"
        )));
    }
    let text = std::str::from_utf8(raw)
        .map_err(|_| FileDialogError("file chooser reply is not valid UTF-8".into()))?
        .trim();
    if text.is_empty() {
        return Err(FileDialogError(
            "file chooser returned an empty reply".into(),
        ));
    }
    let val: Value = serde_json::from_str(text)
        .map_err(|e| FileDialogError(format!("file chooser reply JSON invalid: {e}")))?;
    let obj = val
        .as_object()
        .ok_or_else(|| FileDialogError("file chooser reply must be a JSON object".into()))?;
    let version = obj
        .get("version")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| FileDialogError("file chooser reply missing version".into()))?;
    if version != PROTOCOL_VERSION {
        return Err(FileDialogError(format!(
            "file chooser reply version {version} does not match expected {PROTOCOL_VERSION}"
        )));
    }
    if obj.contains_key("paths") {
        return Err(FileDialogError(
            "file chooser reply must contain at most one path".into(),
        ));
    }
    let status = obj
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| FileDialogError("file chooser reply missing status".into()))?;
    match status {
        "cancel" => Ok(None),
        "error" => {
            let msg = obj
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("file chooser reported an error");
            Err(FileDialogError(
                truncate_str(msg, MAX_ERROR_BYTES).to_owned(),
            ))
        }
        "ok" => {
            let expected_mode = GrantMode::for_kind(kind);
            let mode_str = obj
                .get("mode")
                .and_then(Value::as_str)
                .ok_or_else(|| FileDialogError("file chooser reply missing mode".into()))?;
            let mode = GrantMode::parse(mode_str).ok_or_else(|| {
                FileDialogError(format!("file chooser reply has unknown mode: {mode_str}"))
            })?;
            if mode != expected_mode {
                return Err(FileDialogError(format!(
                    "file chooser mode mismatch: expected {}, got {}",
                    expected_mode.as_str(),
                    mode.as_str()
                )));
            }
            let path_str = obj
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| FileDialogError("file chooser reply missing path".into()))?;
            let overwrite_confirmed = obj
                .get("overwrite_confirmed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let path = validate_chosen_path(kind, path_str, overwrite_confirmed)?;
            Ok(Some(path))
        }
        other => Err(FileDialogError(format!(
            "file chooser reply has unknown status: {other}"
        ))),
    }
}

fn validate_chosen_path(
    kind: FileDialogKind,
    path_str: &str,
    overwrite_confirmed: bool,
) -> Result<PathBuf, FileDialogError> {
    if path_str.is_empty() || path_str.len() > MAX_PATH_BYTES {
        return Err(FileDialogError(format!(
            "file chooser path length {} out of bounds (1..={MAX_PATH_BYTES})",
            path_str.len()
        )));
    }
    if path_str.contains('\0') {
        return Err(FileDialogError(
            "file chooser path contains a NUL byte".into(),
        ));
    }
    let path = Path::new(path_str);
    if !path.is_absolute() {
        return Err(FileDialogError("file chooser path must be absolute".into()));
    }
    for comp in path.components() {
        if matches!(comp, Component::ParentDir | Component::CurDir) {
            return Err(FileDialogError(
                "file chooser path must not contain relative segments".into(),
            ));
        }
    }
    match kind {
        FileDialogKind::Open => {
            let meta = std::fs::metadata(path)
                .map_err(|e| FileDialogError(format!("chosen file is not accessible: {e}")))?;
            if meta.is_dir() {
                return Err(FileDialogError(
                    "file chooser returned a directory for an Open request".into(),
                ));
            }
        }
        FileDialogKind::Folder => {
            let meta = std::fs::metadata(path)
                .map_err(|e| FileDialogError(format!("chosen folder is not accessible: {e}")))?;
            if !meta.is_dir() {
                return Err(FileDialogError(
                    "file chooser returned a non-directory for a Folder request".into(),
                ));
            }
        }
        FileDialogKind::Save => {
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| FileDialogError("save target must end with a file name".into()))?;
            if !is_plain_file_name(file_name) {
                return Err(FileDialogError(
                    "save target file name must be a plain name".into(),
                ));
            }
            let parent = path
                .parent()
                .ok_or_else(|| FileDialogError("save target missing parent directory".into()))?;
            let parent_meta = std::fs::metadata(parent).map_err(|e| {
                FileDialogError(format!(
                    "save target parent directory is not accessible: {e}"
                ))
            })?;
            if !parent_meta.is_dir() {
                return Err(FileDialogError(
                    "save target parent is not a directory".into(),
                ));
            }
            if std::fs::symlink_metadata(path).is_ok() {
                if std::fs::metadata(path).is_ok_and(|m| m.is_dir()) {
                    return Err(FileDialogError(
                        "save target cannot overwrite a directory".into(),
                    ));
                }
                if !overwrite_confirmed {
                    return Err(FileDialogError(
                        "save target already exists without overwrite confirmation".into(),
                    ));
                }
            }
        }
    }
    Ok(PathBuf::from(path_str))
}

pub fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_FILENAME_BYTES
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\0')
}

fn kind_to_str(kind: FileDialogKind) -> &'static str {
    match kind {
        FileDialogKind::Open => "open",
        FileDialogKind::Save => "save",
        FileDialogKind::Folder => "folder",
    }
}

fn str_to_kind(raw: &str) -> Option<FileDialogKind> {
    match raw {
        "open" => Some(FileDialogKind::Open),
        "save" => Some(FileDialogKind::Save),
        "folder" => Some(FileDialogKind::Folder),
        _ => None,
    }
}

fn path_to_valid_str<'a>(path: &'a Path, label: &str) -> Result<&'a str, FileDialogError> {
    let s = path
        .to_str()
        .ok_or_else(|| FileDialogError(format!("file chooser {label} is not valid UTF-8")))?;
    if s.len() > MAX_PATH_BYTES || s.contains('\0') {
        return Err(FileDialogError(format!("file chooser {label} is invalid")));
    }
    Ok(s)
}

fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

use std::collections::{BTreeMap, BTreeSet};

use crate::catalog::{CapabilityClass, lookup_capability, validate_catalog_invariants};
use crate::error::ManifestError;
use crate::{
    FORMAT_VERSION, MAX_CAPABILITIES, MAX_EXEC_LEN, MAX_ID_LEN, MAX_MANIFEST_BYTES, MAX_NAME_LEN,
    MAX_REASON_LEN, MAX_RESERVED_IDS, MAX_REVIEW_EXCEPTIONS, MAX_REVIEW_SCENES, MAX_SCENE_ARG_LEN,
    MAX_SCENE_ARGS, MIN_EXEC_LEN, MIN_ID_LEN, MIN_NAME_LEN, MIN_REASON_LEN,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AppKind {
    Native,
    Ported,
    Linux,
    Windows,
}

impl AppKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Ported => "ported",
            Self::Linux => "linux",
            Self::Windows => "windows",
        }
    }

    pub fn parse_str(value: &str) -> Result<Self, ManifestError> {
        match value {
            "native" => Ok(Self::Native),
            "ported" => Ok(Self::Ported),
            "linux" => Ok(Self::Linux),
            "windows" => Ok(Self::Windows),
            other => Err(ManifestError::InvalidKind {
                found: other.to_owned(),
            }),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Origin<'a> {
    Repository { package: &'a str },
    Outside { reserved_ids: &'a BTreeSet<String> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityDecl {
    pub class: CapabilityClass,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewScene {
    pub name: String,
    pub args: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewException {
    pub element: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReviewSection {
    pub scenes: Vec<ReviewScene>,
    pub exceptions: Vec<ReviewException>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub format: u32,
    pub id: String,
    pub name: String,
    pub kind: AppKind,
    pub exec: String,
    pub capabilities: BTreeMap<String, CapabilityDecl>,
    pub review: ReviewSection,
}

const TOP_KEYS: [&str; 7] = [
    "format",
    "id",
    "name",
    "kind",
    "exec",
    "capabilities",
    "review",
];

pub fn validate_id_segment(
    value: &str,
    min_len: usize,
    max_len: usize,
) -> Result<(), &'static str> {
    assert!(min_len >= 1 && min_len <= max_len);
    let len = value.len();
    if len < min_len || len > max_len {
        return Err("length out of bounds");
    }
    if value.starts_with('.') || value.starts_with('-') {
        return Err("must not start with '.' or '-'");
    }
    if value.ends_with('.') || value.ends_with('-') {
        return Err("must not end with '.' or '-'");
    }
    if value.contains("..") {
        return Err("must not contain '..'");
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
    {
        return Err("must use only lowercase ASCII letters, digits, '-' and '.'");
    }
    Ok(())
}

fn validate_plain_text(value: &str, min_len: usize, max_len: usize) -> Result<(), &'static str> {
    assert!(min_len >= 1 && min_len <= max_len);
    let len = value.len();
    if len < min_len || len > max_len {
        return Err("length out of bounds");
    }
    if value.trim() != value {
        return Err("must not have leading or trailing whitespace");
    }
    if value.bytes().any(|b| b < 0x20 || b == 0x7F) {
        return Err("must not contain ASCII control characters");
    }
    Ok(())
}

fn validate_exec_path(value: &str) -> Result<(), &'static str> {
    let len = value.len();
    if !(MIN_EXEC_LEN..=MAX_EXEC_LEN).contains(&len) {
        return Err("length out of bounds");
    }
    if !value.starts_with('/') {
        return Err("must be an absolute path starting with '/'");
    }
    if value.ends_with('/') {
        return Err("must not end with '/'");
    }
    if value.bytes().any(|b| b <= 0x20 || b == 0x7F || b == b':') {
        return Err("must not contain control characters, spaces or ':'");
    }
    if value.starts_with("/scheme/") || value == "/scheme" {
        return Err("must not point at /scheme");
    }
    for segment in value[1..].split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err("must be normalized without empty, '.' or '..' segments");
        }
    }
    Ok(())
}

fn required_string<'a>(
    table: &'a toml::Table,
    field: &'static str,
) -> Result<&'a str, ManifestError> {
    let Some(val) = table.get(field) else {
        return Err(ManifestError::MissingField { field });
    };
    val.as_str().ok_or_else(|| ManifestError::InvalidToml {
        detail: format!("{field} must be a string"),
    })
}

fn parse_capabilities(
    table: &toml::Table,
) -> Result<BTreeMap<String, CapabilityDecl>, ManifestError> {
    let Some(raw) = table.get("capabilities") else {
        return Ok(BTreeMap::new());
    };
    let Some(caps_table) = raw.as_table() else {
        return Err(ManifestError::InvalidToml {
            detail: "capabilities must be a table".to_owned(),
        });
    };
    if caps_table.len() > MAX_CAPABILITIES {
        return Err(ManifestError::TooManyCapabilities {
            count: caps_table.len(),
            limit: MAX_CAPABILITIES,
        });
    }
    let mut parsed = BTreeMap::new();
    for (cap_name, cap_value) in caps_table {
        let Some(spec) = lookup_capability(cap_name) else {
            return Err(ManifestError::UnknownCapability {
                capability: cap_name.clone(),
            });
        };
        if spec.class == CapabilityClass::Never {
            return Err(ManifestError::ForbiddenCapability {
                capability: cap_name.clone(),
            });
        }
        let Some(entry_table) = cap_value.as_table() else {
            return Err(ManifestError::InvalidToml {
                detail: format!("capabilities.{cap_name} must be a table"),
            });
        };
        for key in entry_table.keys() {
            if key != "reason" {
                return Err(ManifestError::UnknownKey {
                    key: format!("capabilities.{cap_name}.{key}"),
                });
            }
        }
        let Some(reason_val) = entry_table.get("reason") else {
            return Err(ManifestError::MissingReason {
                capability: cap_name.clone(),
            });
        };
        let Some(reason) = reason_val.as_str() else {
            return Err(ManifestError::InvalidReason {
                capability: cap_name.clone(),
                detail: "reason must be a string",
            });
        };
        if let Err(detail) = validate_plain_text(reason, MIN_REASON_LEN, MAX_REASON_LEN) {
            return Err(ManifestError::InvalidReason {
                capability: cap_name.clone(),
                detail,
            });
        }
        let prev = parsed.insert(
            cap_name.clone(),
            CapabilityDecl {
                class: spec.class,
                reason: reason.to_owned(),
            },
        );
        assert!(prev.is_none());
    }
    Ok(parsed)
}

fn parse_review(table: &toml::Table) -> Result<ReviewSection, ManifestError> {
    let Some(raw) = table.get("review") else {
        return Ok(ReviewSection::default());
    };
    let Some(review_table) = raw.as_table() else {
        return Err(ManifestError::InvalidReview {
            detail: "review must be a table".to_owned(),
        });
    };
    for key in review_table.keys() {
        if key != "scene" && key != "exception" {
            return Err(ManifestError::UnknownKey {
                key: format!("review.{key}"),
            });
        }
    }
    let mut scenes = Vec::new();
    let mut seen_scene_names = BTreeSet::new();
    if let Some(raw_scenes) = review_table.get("scene") {
        let Some(arr) = raw_scenes.as_array() else {
            return Err(ManifestError::InvalidReview {
                detail: "review.scene must be an array of tables".to_owned(),
            });
        };
        if arr.len() > MAX_REVIEW_SCENES {
            return Err(ManifestError::InvalidReview {
                detail: format!(
                    "too many review scenes ({}, limit {MAX_REVIEW_SCENES})",
                    arr.len()
                ),
            });
        }
        for (idx, item) in arr.iter().enumerate() {
            let Some(scene_table) = item.as_table() else {
                return Err(ManifestError::InvalidReview {
                    detail: format!("review.scene[{idx}] must be a table"),
                });
            };
            for key in scene_table.keys() {
                if key != "name" && key != "args" {
                    return Err(ManifestError::UnknownKey {
                        key: format!("review.scene[{idx}].{key}"),
                    });
                }
            }
            let Some(name_val) = scene_table.get("name") else {
                return Err(ManifestError::InvalidReview {
                    detail: format!("review.scene[{idx}] is missing name"),
                });
            };
            let Some(name) = name_val.as_str() else {
                return Err(ManifestError::InvalidReview {
                    detail: format!("review.scene[{idx}].name must be a string"),
                });
            };
            if let Err(detail) = validate_id_segment(name, MIN_ID_LEN, MAX_ID_LEN) {
                return Err(ManifestError::InvalidReview {
                    detail: format!("review.scene[{idx}].name {name:?}: {detail}"),
                });
            }
            if !seen_scene_names.insert(name.to_owned()) {
                return Err(ManifestError::InvalidReview {
                    detail: format!("duplicate review.scene name {name:?}"),
                });
            }
            let mut args = Vec::new();
            if let Some(raw_args) = scene_table.get("args") {
                let Some(args_arr) = raw_args.as_array() else {
                    return Err(ManifestError::InvalidReview {
                        detail: format!("review.scene[{idx}].args must be an array"),
                    });
                };
                if args_arr.len() > MAX_SCENE_ARGS {
                    return Err(ManifestError::InvalidReview {
                        detail: format!(
                            "review.scene[{idx}].args has {} items, limit {MAX_SCENE_ARGS}",
                            args_arr.len()
                        ),
                    });
                }
                for (arg_idx, arg_val) in args_arr.iter().enumerate() {
                    let Some(arg_str) = arg_val.as_str() else {
                        return Err(ManifestError::InvalidReview {
                            detail: format!("review.scene[{idx}].args[{arg_idx}] must be a string"),
                        });
                    };
                    if let Err(detail) = validate_plain_text(arg_str, 1, MAX_SCENE_ARG_LEN) {
                        return Err(ManifestError::InvalidReview {
                            detail: format!("review.scene[{idx}].args[{arg_idx}]: {detail}"),
                        });
                    }
                    args.push(arg_str.to_owned());
                }
            }
            scenes.push(ReviewScene {
                name: name.to_owned(),
                args,
            });
        }
    }
    let mut exceptions = Vec::new();
    if let Some(raw_exceptions) = review_table.get("exception") {
        let Some(arr) = raw_exceptions.as_array() else {
            return Err(ManifestError::InvalidReview {
                detail: "review.exception must be an array of tables".to_owned(),
            });
        };
        if arr.len() > MAX_REVIEW_EXCEPTIONS {
            return Err(ManifestError::InvalidReview {
                detail: format!(
                    "too many review exceptions ({}, limit {MAX_REVIEW_EXCEPTIONS})",
                    arr.len()
                ),
            });
        }
        for (idx, item) in arr.iter().enumerate() {
            let Some(exc_table) = item.as_table() else {
                return Err(ManifestError::InvalidReview {
                    detail: format!("review.exception[{idx}] must be a table"),
                });
            };
            for key in exc_table.keys() {
                if key != "element" && key != "reason" {
                    return Err(ManifestError::UnknownKey {
                        key: format!("review.exception[{idx}].{key}"),
                    });
                }
            }
            let Some(element_val) = exc_table.get("element") else {
                return Err(ManifestError::InvalidReview {
                    detail: format!("review.exception[{idx}] is missing element"),
                });
            };
            let Some(element) = element_val.as_str() else {
                return Err(ManifestError::InvalidReview {
                    detail: format!("review.exception[{idx}].element must be a string"),
                });
            };
            if let Err(detail) = validate_plain_text(element, 1, MAX_NAME_LEN) {
                return Err(ManifestError::InvalidReview {
                    detail: format!("review.exception[{idx}].element: {detail}"),
                });
            }
            let Some(reason_val) = exc_table.get("reason") else {
                return Err(ManifestError::InvalidReview {
                    detail: format!("review.exception[{idx}] is missing reason"),
                });
            };
            let Some(reason) = reason_val.as_str() else {
                return Err(ManifestError::InvalidReview {
                    detail: format!("review.exception[{idx}].reason must be a string"),
                });
            };
            if let Err(detail) = validate_plain_text(reason, MIN_REASON_LEN, MAX_REASON_LEN) {
                return Err(ManifestError::InvalidReview {
                    detail: format!("review.exception[{idx}].reason: {detail}"),
                });
            }
            exceptions.push(ReviewException {
                element: element.to_owned(),
                reason: reason.to_owned(),
            });
        }
    }
    Ok(ReviewSection { scenes, exceptions })
}

pub fn parse_manifest_bytes(bytes: &[u8], origin: Origin<'_>) -> Result<Manifest, ManifestError> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestError::ManifestTooLarge {
            len: bytes.len(),
            limit: MAX_MANIFEST_BYTES,
        });
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ManifestError::InvalidUtf8)?;
    parse_manifest(text, origin)
}

pub fn parse_manifest(text: &str, origin: Origin<'_>) -> Result<Manifest, ManifestError> {
    validate_catalog_invariants();
    if text.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestError::ManifestTooLarge {
            len: text.len(),
            limit: MAX_MANIFEST_BYTES,
        });
    }
    match origin {
        Origin::Repository { package } => {
            assert!(!package.is_empty());
        }
        Origin::Outside { reserved_ids } => {
            if reserved_ids.len() > MAX_RESERVED_IDS {
                return Err(ManifestError::TooManyReservedIds {
                    count: reserved_ids.len(),
                    limit: MAX_RESERVED_IDS,
                });
            }
        }
    }
    let table: toml::Table = toml::from_str(text).map_err(|e| ManifestError::InvalidToml {
        detail: e.message().to_owned(),
    })?;
    let Some(format_val) = table.get("format") else {
        return Err(ManifestError::MissingField { field: "format" });
    };
    let Some(format_raw) = format_val.as_integer() else {
        return Err(ManifestError::InvalidToml {
            detail: "format must be an integer".to_owned(),
        });
    };
    if format_raw != i64::from(FORMAT_VERSION) {
        return Err(ManifestError::UnsupportedFormat { found: format_raw });
    }
    for key in table.keys() {
        if !TOP_KEYS.contains(&key.as_str()) {
            return Err(ManifestError::UnknownKey { key: key.clone() });
        }
    }
    let id = required_string(&table, "id")?;
    if let Err(detail) = validate_id_segment(id, MIN_ID_LEN, MAX_ID_LEN) {
        return Err(ManifestError::InvalidId {
            id: id.to_owned(),
            detail,
        });
    }
    match origin {
        Origin::Repository { package } => {
            if id != package {
                return Err(ManifestError::PackageIdMismatch {
                    package: package.to_owned(),
                    id: id.to_owned(),
                });
            }
        }
        Origin::Outside { reserved_ids } => {
            if reserved_ids.contains(id) {
                return Err(ManifestError::ReservedId { id: id.to_owned() });
            }
        }
    }
    let name = required_string(&table, "name")?;
    if let Err(detail) = validate_plain_text(name, MIN_NAME_LEN, MAX_NAME_LEN) {
        return Err(ManifestError::InvalidName { detail });
    }
    let kind_str = required_string(&table, "kind")?;
    let kind = AppKind::parse_str(kind_str)?;
    let exec = required_string(&table, "exec")?;
    if let Err(detail) = validate_exec_path(exec) {
        return Err(ManifestError::InvalidExec { detail });
    }
    let capabilities = parse_capabilities(&table)?;
    let review = parse_review(&table)?;
    let manifest = Manifest {
        format: FORMAT_VERSION,
        id: id.to_owned(),
        name: name.to_owned(),
        kind,
        exec: exec.to_owned(),
        capabilities,
        review,
    };
    assert_eq!(manifest.format, FORMAT_VERSION);
    assert!(manifest.capabilities.len() <= MAX_CAPABILITIES);
    assert!(manifest.capabilities.keys().all(|cap| {
        matches!(
            lookup_capability(cap),
            Some(spec) if spec.class != CapabilityClass::Never
        )
    }));
    Ok(manifest)
}

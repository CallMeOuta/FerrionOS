use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    ManifestTooLarge {
        len: usize,
        limit: usize,
    },
    InvalidUtf8,
    InvalidToml {
        detail: String,
    },
    MissingField {
        field: &'static str,
    },
    UnsupportedFormat {
        found: i64,
    },
    UnknownKey {
        key: String,
    },
    InvalidId {
        id: String,
        detail: &'static str,
    },
    PackageIdMismatch {
        package: String,
        id: String,
    },
    ReservedId {
        id: String,
    },
    TooManyReservedIds {
        count: usize,
        limit: usize,
    },
    InvalidName {
        detail: &'static str,
    },
    InvalidKind {
        found: String,
    },
    InvalidExec {
        detail: &'static str,
    },
    TooManyCapabilities {
        count: usize,
        limit: usize,
    },
    UnknownCapability {
        capability: String,
    },
    ForbiddenCapability {
        capability: String,
    },
    MissingReason {
        capability: String,
    },
    InvalidReason {
        capability: String,
        detail: &'static str,
    },
    InvalidReview {
        detail: String,
    },
    GrantsTooLarge {
        len: usize,
        limit: usize,
    },
    TooManyGrantApps {
        count: usize,
        limit: usize,
    },
    InvalidGrant {
        detail: String,
    },
}

impl ManifestError {
    pub const fn reason_code(&self) -> &'static str {
        match self {
            Self::ManifestTooLarge { .. } => "manifest-too-large",
            Self::InvalidUtf8 => "invalid-utf8",
            Self::InvalidToml { .. } => "invalid-toml",
            Self::MissingField { .. } => "missing-field",
            Self::UnsupportedFormat { .. } => "unsupported-format",
            Self::UnknownKey { .. } => "unknown-key",
            Self::InvalidId { .. } => "invalid-id",
            Self::PackageIdMismatch { .. } => "package-id-mismatch",
            Self::ReservedId { .. } => "reserved-id",
            Self::TooManyReservedIds { .. } => "too-many-reserved-ids",
            Self::InvalidName { .. } => "invalid-name",
            Self::InvalidKind { .. } => "invalid-kind",
            Self::InvalidExec { .. } => "invalid-exec",
            Self::TooManyCapabilities { .. } => "too-many-capabilities",
            Self::UnknownCapability { .. } => "unknown-capability",
            Self::ForbiddenCapability { .. } => "forbidden-capability",
            Self::MissingReason { .. } => "missing-reason",
            Self::InvalidReason { .. } => "invalid-reason",
            Self::InvalidReview { .. } => "invalid-review",
            Self::GrantsTooLarge { .. } => "grants-too-large",
            Self::TooManyGrantApps { .. } => "too-many-grant-apps",
            Self::InvalidGrant { .. } => "invalid-grant",
        }
    }
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ManifestTooLarge { len, limit } => {
                write!(f, "manifest is {len} bytes, limit is {limit}")
            }
            Self::InvalidUtf8 => write!(f, "manifest is not valid UTF-8"),
            Self::InvalidToml { detail } => write!(f, "invalid TOML: {detail}"),
            Self::MissingField { field } => write!(f, "missing required field {field:?}"),
            Self::UnsupportedFormat { found } => {
                write!(f, "format {found} is not supported (only 1)")
            }
            Self::UnknownKey { key } => write!(f, "unknown key {key:?}"),
            Self::InvalidId { id, detail } => write!(f, "invalid id {id:?}: {detail}"),
            Self::PackageIdMismatch { package, id } => {
                write!(f, "repository id {id:?} does not match package {package:?}")
            }
            Self::ReservedId { id } => {
                write!(f, "outside bundle id {id:?} names a repository package")
            }
            Self::TooManyReservedIds { count, limit } => {
                write!(f, "reserved id table has {count} entries, limit is {limit}")
            }
            Self::InvalidName { detail } => write!(f, "invalid name: {detail}"),
            Self::InvalidKind { found } => write!(f, "invalid kind {found:?}"),
            Self::InvalidExec { detail } => write!(f, "invalid exec path: {detail}"),
            Self::TooManyCapabilities { count, limit } => {
                write!(
                    f,
                    "manifest declares {count} capabilities, limit is {limit}"
                )
            }
            Self::UnknownCapability { capability } => {
                write!(f, "unknown capability {capability:?}")
            }
            Self::ForbiddenCapability { capability } => {
                write!(f, "capability {capability:?} belongs to the never class")
            }
            Self::MissingReason { capability } => {
                write!(f, "capability {capability:?} is missing a reason")
            }
            Self::InvalidReason { capability, detail } => {
                write!(f, "invalid reason for {capability:?}: {detail}")
            }
            Self::InvalidReview { detail } => write!(f, "invalid review section: {detail}"),
            Self::GrantsTooLarge { len, limit } => {
                write!(f, "grants file is {len} bytes, limit is {limit}")
            }
            Self::TooManyGrantApps { count, limit } => {
                write!(f, "grants file lists {count} apps, limit is {limit}")
            }
            Self::InvalidGrant { detail } => write!(f, "invalid grant entry: {detail}"),
        }
    }
}

impl std::error::Error for ManifestError {}

use std::fmt;

use crate::crypto::Blake3Hash;
use crate::format::{Arch, Channel, ReleaseNumber};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    NotFound(String),
    SizeLimitExceeded { path: String, limit: u64 },
    Transport(String),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(path) => write!(f, "not found: {path}"),
            Self::SizeLimitExceeded { path, limit } => {
                write!(f, "response for {path} exceeds limit of {limit} bytes")
            }
            Self::Transport(msg) => write!(f, "fetch failed: {msg}"),
        }
    }
}

impl std::error::Error for FetchError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    Fetch(FetchError),
    RootTooLarge {
        limit: usize,
    },
    IndexTooLarge {
        limit: usize,
    },
    InvalidSignature {
        target: &'static str,
    },
    UnsupportedFormat {
        target: &'static str,
        found: u64,
        waypoint: Option<ReleaseNumber>,
    },
    UnknownKeyOrSyntax {
        target: &'static str,
        detail: String,
    },
    InvalidField {
        target: &'static str,
        detail: String,
    },
    RollbackSerial {
        found: u64,
        trusted: u64,
    },
    MissingChannel {
        channel: Channel,
    },
    RollbackRelease {
        found: ReleaseNumber,
        installed: ReleaseNumber,
    },
    MissingRelease {
        release: ReleaseNumber,
    },
    ReleaseWithdrawn {
        release: ReleaseNumber,
    },
    MissingArch {
        release: ReleaseNumber,
        arch: Arch,
    },
    IndexHashMismatch {
        expected: Blake3Hash,
        actual: Blake3Hash,
    },
    IndexReleaseMismatch {
        expected: ReleaseNumber,
        actual: ReleaseNumber,
    },
    ObjectTooLarge {
        package: String,
        limit: u64,
    },
    ObjectSizeMismatch {
        package: String,
        expected: u64,
        actual: u64,
    },
    ObjectHashMismatch {
        package: String,
        expected: Blake3Hash,
        actual: Blake3Hash,
    },
    PkgarInvalid {
        package: String,
        detail: String,
    },
    LimitExceeded {
        what: &'static str,
        limit: usize,
    },
    RotationRefused {
        detail: String,
    },
}

impl UpdateError {
    pub fn reason_code(&self) -> &'static str {
        match self {
            Self::Fetch(FetchError::SizeLimitExceeded { .. }) => "size-limit-exceeded",
            Self::Fetch(FetchError::NotFound(_)) => "not-found",
            Self::Fetch(FetchError::Transport(_)) => "transport-error",
            Self::RootTooLarge { .. } => "root-too-large",
            Self::IndexTooLarge { .. } => "index-too-large",
            Self::InvalidSignature { target } if *target == "index.toml" => {
                "invalid-index-signature"
            }
            Self::InvalidSignature { .. } => "invalid-signature",
            Self::UnsupportedFormat { .. } => "unsupported-format",
            Self::UnknownKeyOrSyntax { .. } => "unknown-key-or-syntax",
            Self::InvalidField { .. } => "invalid-field",
            Self::RollbackSerial { .. } => "rollback-serial",
            Self::MissingChannel { .. } => "missing-channel",
            Self::RollbackRelease { .. } => "rollback-release",
            Self::MissingRelease { .. } => "missing-release",
            Self::ReleaseWithdrawn { .. } => "release-withdrawn",
            Self::MissingArch { .. } => "missing-arch",
            Self::IndexHashMismatch { .. } => "index-hash-mismatch",
            Self::IndexReleaseMismatch { .. } => "index-release-mismatch",
            Self::ObjectTooLarge { .. } => "object-too-large",
            Self::ObjectSizeMismatch { .. } => "object-size-mismatch",
            Self::ObjectHashMismatch { .. } => "object-hash-mismatch",
            Self::PkgarInvalid { .. } => "pkgar-invalid",
            Self::LimitExceeded { .. } => "limit-exceeded",
            Self::RotationRefused { .. } => "rotation-refused",
        }
    }
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fetch(err) => write!(f, "{err}"),
            Self::RootTooLarge { limit } => {
                write!(f, "root.toml exceeds maximum size of {limit} bytes")
            }
            Self::IndexTooLarge { limit } => {
                write!(f, "index.toml exceeds maximum size of {limit} bytes")
            }
            Self::InvalidSignature { target } => {
                write!(f, "invalid Ed25519 signature for {target}")
            }
            Self::UnsupportedFormat {
                target,
                found,
                waypoint: Some(wp),
            } => write!(
                f,
                "unsupported {target} format {found}; update to waypoint {wp} first"
            ),
            Self::UnsupportedFormat {
                target,
                found,
                waypoint: None,
            } => write!(
                f,
                "unsupported {target} format {found} with no known waypoint; update from the release medium"
            ),
            Self::UnknownKeyOrSyntax { target, detail } => {
                write!(f, "invalid or unknown key in {target}: {detail}")
            }
            Self::InvalidField { target, detail } => {
                write!(f, "invalid field in {target}: {detail}")
            }
            Self::RollbackSerial { found, trusted } => write!(
                f,
                "refused root.toml serial {found} below trusted serial {trusted}"
            ),
            Self::MissingChannel { channel } => {
                write!(f, "root.toml has no entry for channel {channel}")
            }
            Self::RollbackRelease { found, installed } => write!(
                f,
                "refused release {found} below installed release {installed}"
            ),
            Self::MissingRelease { release } => {
                write!(f, "root.toml has no release metadata for {release}")
            }
            Self::ReleaseWithdrawn { release } => {
                write!(f, "release {release} is marked withdrawn")
            }
            Self::MissingArch { release, arch } => {
                write!(f, "release {release} has no snapshot index for {arch}")
            }
            Self::IndexHashMismatch { expected, actual } => write!(
                f,
                "snapshot index hash mismatch: expected {expected}, got {actual}"
            ),
            Self::IndexReleaseMismatch { expected, actual } => write!(
                f,
                "snapshot index release mismatch: expected {expected}, got {actual}"
            ),
            Self::ObjectTooLarge { package, limit } => write!(
                f,
                "package {package} object exceeds stated size of {limit} bytes"
            ),
            Self::ObjectSizeMismatch {
                package,
                expected,
                actual,
            } => write!(
                f,
                "package {package} size mismatch: expected {expected} bytes, got {actual}"
            ),
            Self::ObjectHashMismatch {
                package,
                expected,
                actual,
            } => write!(
                f,
                "package {package} hash mismatch: expected {expected}, got {actual}"
            ),
            Self::PkgarInvalid { package, detail } => {
                write!(f, "package {package} archive invalid: {detail}")
            }
            Self::LimitExceeded { what, limit } => {
                write!(f, "{what} exceeds upper bound of {limit}")
            }
            Self::RotationRefused { detail } => write!(f, "key rotation refused: {detail}"),
        }
    }
}

impl std::error::Error for UpdateError {}

impl From<FetchError> for UpdateError {
    fn from(err: FetchError) -> Self {
        Self::Fetch(err)
    }
}

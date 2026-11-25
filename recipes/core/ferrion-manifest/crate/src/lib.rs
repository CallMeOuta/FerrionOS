pub mod catalog;
pub mod error;
pub mod grants;
pub mod parse;

pub const FORMAT_VERSION: u32 = 1;
pub const GRANTS_FORMAT_VERSION: u32 = 1;
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_GRANTS_FILE_BYTES: usize = 64 * 1024;
pub const MAX_GRANT_APPS: usize = 256;
pub const MAX_INSTALLED_MANIFESTS: usize = 512;
pub const MIN_ID_LEN: usize = 1;
pub const MAX_ID_LEN: usize = 64;
pub const MIN_NAME_LEN: usize = 1;
pub const MAX_NAME_LEN: usize = 64;
pub const MIN_EXEC_LEN: usize = 2;
pub const MAX_EXEC_LEN: usize = 256;
pub const MAX_CAPABILITIES: usize = 32;
pub const MIN_REASON_LEN: usize = 8;
pub const MAX_REASON_LEN: usize = 160;
pub const MIN_USB_CLASS_LEN: usize = 1;
pub const MAX_USB_CLASS_LEN: usize = 32;
pub const MAX_REVIEW_SCENES: usize = 32;
pub const MAX_SCENE_ARGS: usize = 16;
pub const MAX_SCENE_ARG_LEN: usize = 128;
pub const MAX_REVIEW_EXCEPTIONS: usize = 32;
pub const MAX_RESERVED_IDS: usize = 8192;
pub const MAX_NON_APP_PACKAGES: usize = 512;

pub use catalog::{
    CATALOG, CapabilityClass, CapabilityMatch, CapabilitySpec, USB_DEVICE_PREFIX,
    lookup_capability, validate_catalog_invariants,
};
pub use error::ManifestError;
pub use grants::{GrantsFile, effective_grant, parse_grants, parse_grants_bytes};
pub use parse::{
    AppKind, CapabilityDecl, Manifest, Origin, ReviewException, ReviewScene, ReviewSection,
    parse_manifest, parse_manifest_bytes, validate_id_segment,
};

use std::collections::BTreeMap;

use crate::catalog::{CapabilityClass, lookup_capability, validate_catalog_invariants};
use crate::error::ManifestError;
use crate::parse::{Manifest, validate_id_segment};
use crate::{
    FORMAT_VERSION, GRANTS_FORMAT_VERSION, MAX_CAPABILITIES, MAX_GRANT_APPS, MAX_GRANTS_FILE_BYTES,
    MAX_ID_LEN, MIN_ID_LEN,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GrantsFile {
    pub format: u32,
    pub apps: BTreeMap<String, BTreeMap<String, bool>>,
}

impl GrantsFile {
    pub fn empty() -> Self {
        Self {
            format: GRANTS_FORMAT_VERSION,
            apps: BTreeMap::new(),
        }
    }
}

const GRANTS_TOP_KEYS: [&str; 2] = ["format", "apps"];

pub fn parse_grants_bytes(bytes: &[u8]) -> Result<GrantsFile, ManifestError> {
    if bytes.len() > MAX_GRANTS_FILE_BYTES {
        return Err(ManifestError::GrantsTooLarge {
            len: bytes.len(),
            limit: MAX_GRANTS_FILE_BYTES,
        });
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ManifestError::InvalidUtf8)?;
    parse_grants(text)
}

pub fn parse_grants(text: &str) -> Result<GrantsFile, ManifestError> {
    validate_catalog_invariants();
    if text.len() > MAX_GRANTS_FILE_BYTES {
        return Err(ManifestError::GrantsTooLarge {
            len: text.len(),
            limit: MAX_GRANTS_FILE_BYTES,
        });
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
    if format_raw != i64::from(GRANTS_FORMAT_VERSION) {
        return Err(ManifestError::UnsupportedFormat { found: format_raw });
    }
    for key in table.keys() {
        if !GRANTS_TOP_KEYS.contains(&key.as_str()) {
            return Err(ManifestError::UnknownKey { key: key.clone() });
        }
    }
    let mut apps = BTreeMap::new();
    if let Some(raw_apps) = table.get("apps") {
        let Some(apps_table) = raw_apps.as_table() else {
            return Err(ManifestError::InvalidGrant {
                detail: "apps must be a table".to_owned(),
            });
        };
        if apps_table.len() > MAX_GRANT_APPS {
            return Err(ManifestError::TooManyGrantApps {
                count: apps_table.len(),
                limit: MAX_GRANT_APPS,
            });
        }
        for (app_id, app_val) in apps_table {
            if let Err(detail) = validate_id_segment(app_id, MIN_ID_LEN, MAX_ID_LEN) {
                return Err(ManifestError::InvalidId {
                    id: app_id.clone(),
                    detail,
                });
            }
            let Some(caps_table) = app_val.as_table() else {
                return Err(ManifestError::InvalidGrant {
                    detail: format!("apps.{app_id} must be a table"),
                });
            };
            if caps_table.len() > MAX_CAPABILITIES {
                return Err(ManifestError::TooManyCapabilities {
                    count: caps_table.len(),
                    limit: MAX_CAPABILITIES,
                });
            }
            let mut app_grants = BTreeMap::new();
            for (cap_name, cap_val) in caps_table {
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
                let Some(allowed) = cap_val.as_bool() else {
                    return Err(ManifestError::InvalidGrant {
                        detail: format!("apps.{app_id}.{cap_name} must be a boolean"),
                    });
                };
                let prev = app_grants.insert(cap_name.clone(), allowed);
                assert!(prev.is_none());
            }
            let prev = apps.insert(app_id.clone(), app_grants);
            assert!(prev.is_none());
        }
    }
    let parsed = GrantsFile {
        format: GRANTS_FORMAT_VERSION,
        apps,
    };
    assert_eq!(parsed.format, GRANTS_FORMAT_VERSION);
    assert!(parsed.apps.len() <= MAX_GRANT_APPS);
    Ok(parsed)
}

pub fn effective_grant(manifest: &Manifest, grants: &GrantsFile, capability: &str) -> bool {
    assert_eq!(manifest.format, FORMAT_VERSION);
    assert_eq!(grants.format, GRANTS_FORMAT_VERSION);
    let Some(decl) = manifest.capabilities.get(capability) else {
        return false;
    };
    assert_ne!(decl.class, CapabilityClass::Never);
    if let Some(explicit) = grants
        .apps
        .get(&manifest.id)
        .and_then(|app_grants| app_grants.get(capability))
    {
        return *explicit;
    }
    matches!(decl.class, CapabilityClass::Automatic)
}

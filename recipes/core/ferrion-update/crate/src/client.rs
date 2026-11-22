use std::collections::BTreeMap;

use crate::crypto::{Blake3Hash, PublicKey, Signature, verify_detached};
use crate::error::{FetchError, UpdateError};
use crate::format::{
    Arch, CalendarDate, Channel, IndexFile, KeyRotation, PackageEntry, ReleaseNumber, RootFile,
    parse_index_toml, parse_root_toml, parse_rotation_toml,
};
use crate::pkgar::{VerifiedFile, verify_pkgar};
use crate::{MAX_INDEX_BYTES, MAX_ROOT_BYTES, MAX_ROTATION_BYTES, SIGNATURE_BYTES, USER_AGENT};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchRequest {
    pub path: String,
    pub max_bytes: u64,
    pub user_agent: &'static str,
}

impl FetchRequest {
    pub fn new(path: String, max_bytes: u64) -> Self {
        assert!(path.starts_with('/'));
        assert!(!path.contains('?'));
        assert!(!path.contains(".."));
        assert!(!path.bytes().any(|b| b.is_ascii_control() || b == b' '));
        assert!(max_bytes > 0);
        Self {
            path,
            max_bytes,
            user_agent: USER_AGENT,
        }
    }
}

pub trait Fetcher {
    fn fetch(&mut self, request: &FetchRequest) -> Result<Vec<u8>, FetchError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientState {
    pub release_key: PublicKey,
    pub emergency_key: Option<PublicKey>,
    pub arch: Arch,
    pub channel: Channel,
    pub installed_release: ReleaseNumber,
    pub trusted_serial: u64,
    pub last_waypoint: Option<ReleaseNumber>,
    pub installed_packages: BTreeMap<String, Blake3Hash>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaleNotice {
    pub signed: CalendarDate,
    pub today: CalendarDate,
    pub age_days: i64,
    pub fresh_days: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedPackage {
    pub name: String,
    pub head: Blake3Hash,
    pub object: Blake3Hash,
    pub size: u64,
    pub files: Vec<VerifiedFile>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VerifyOutcome {
    pub root: RootFile,
    pub target_release: ReleaseNumber,
    pub via_waypoint: bool,
    pub index: IndexFile,
    pub verified_packages: Vec<VerifiedPackage>,
    pub stale_notice: Option<StaleNotice>,
    pub next_state: ClientState,
}

pub fn object_path(hash: &Blake3Hash) -> String {
    format!("/objects/{}/{}", hash.prefix_two(), hash.to_hex())
}

pub fn index_path(release: ReleaseNumber, arch: Arch) -> String {
    format!("/snap/{release}/{arch}/index.toml")
}

pub fn verify_root_bytes(
    root_bytes: &[u8],
    sig_bytes: &[u8],
    release_key: &PublicKey,
    waypoint: Option<ReleaseNumber>,
) -> Result<RootFile, UpdateError> {
    if root_bytes.len() > MAX_ROOT_BYTES {
        return Err(UpdateError::RootTooLarge {
            limit: MAX_ROOT_BYTES,
        });
    }
    let Some(sig) = Signature::from_slice(sig_bytes) else {
        return Err(UpdateError::InvalidSignature {
            target: "root.toml",
        });
    };
    if !verify_detached(&sig, root_bytes, release_key) {
        return Err(UpdateError::InvalidSignature {
            target: "root.toml",
        });
    }
    parse_root_toml(root_bytes, waypoint)
}

pub fn verify_index_bytes(
    index_bytes: &[u8],
    sig_bytes: &[u8],
    expected_hash: &Blake3Hash,
    release_key: &PublicKey,
    expected_release: ReleaseNumber,
    waypoint: Option<ReleaseNumber>,
) -> Result<IndexFile, UpdateError> {
    if index_bytes.len() > MAX_INDEX_BYTES {
        return Err(UpdateError::IndexTooLarge {
            limit: MAX_INDEX_BYTES,
        });
    }
    let actual_hash = Blake3Hash::hash_bytes(index_bytes);
    if actual_hash != *expected_hash {
        return Err(UpdateError::IndexHashMismatch {
            expected: *expected_hash,
            actual: actual_hash,
        });
    }
    let Some(sig) = Signature::from_slice(sig_bytes) else {
        return Err(UpdateError::InvalidSignature {
            target: "index.toml",
        });
    };
    if !verify_detached(&sig, index_bytes, release_key) {
        return Err(UpdateError::InvalidSignature {
            target: "index.toml",
        });
    }
    let index = parse_index_toml(index_bytes, waypoint)?;
    if index.release != expected_release {
        return Err(UpdateError::IndexReleaseMismatch {
            expected: expected_release,
            actual: index.release,
        });
    }
    Ok(index)
}

pub fn verify_package_object(
    pkg: &PackageEntry,
    bytes: &[u8],
    pkgar_key: Option<&PublicKey>,
) -> Result<VerifiedPackage, UpdateError> {
    let actual_len = bytes.len() as u64;
    if actual_len > pkg.size {
        return Err(UpdateError::ObjectTooLarge {
            package: pkg.name.clone(),
            limit: pkg.size,
        });
    }
    if actual_len != pkg.size {
        return Err(UpdateError::ObjectSizeMismatch {
            package: pkg.name.clone(),
            expected: pkg.size,
            actual: actual_len,
        });
    }
    let actual_hash = Blake3Hash::hash_bytes(bytes);
    if actual_hash != pkg.object {
        return Err(UpdateError::ObjectHashMismatch {
            package: pkg.name.clone(),
            expected: pkg.object,
            actual: actual_hash,
        });
    }
    let files = verify_pkgar(&pkg.name, bytes, &pkg.head, pkgar_key)?;
    Ok(VerifiedPackage {
        name: pkg.name.clone(),
        head: pkg.head,
        object: pkg.object,
        size: pkg.size,
        files,
    })
}

pub fn rotate_release_key(
    state: &ClientState,
    rotation_bytes: &[u8],
    rotation_sig: &[u8],
    next_root_bytes: &[u8],
    next_root_sig: &[u8],
) -> Result<(KeyRotation, RootFile, ClientState), UpdateError> {
    let Some(emergency_key) = state.emergency_key else {
        return Err(UpdateError::RotationRefused {
            detail: "no emergency key configured".to_string(),
        });
    };
    if rotation_bytes.len() > MAX_ROTATION_BYTES {
        return Err(UpdateError::RotationRefused {
            detail: format!("rotation statement exceeds {MAX_ROTATION_BYTES} bytes"),
        });
    }
    let Some(rot_sig) = Signature::from_slice(rotation_sig) else {
        return Err(UpdateError::InvalidSignature {
            target: "rotation.toml",
        });
    };
    if !verify_detached(&rot_sig, rotation_bytes, &emergency_key) {
        return Err(UpdateError::InvalidSignature {
            target: "rotation.toml",
        });
    }
    let rotation = parse_rotation_toml(rotation_bytes)?;
    if rotation.serial <= state.trusted_serial {
        return Err(UpdateError::RollbackSerial {
            found: rotation.serial,
            trusted: state.trusted_serial,
        });
    }
    let root = verify_root_bytes(
        next_root_bytes,
        next_root_sig,
        &rotation.new_key,
        state.last_waypoint,
    )?;
    if root.serial < rotation.serial {
        return Err(UpdateError::RollbackSerial {
            found: root.serial,
            trusted: rotation.serial,
        });
    }
    let mut next_state = state.clone();
    next_state.release_key = rotation.new_key;
    next_state.trusted_serial = root.serial;
    Ok((rotation, root, next_state))
}

pub fn check_and_verify(
    state: &ClientState,
    today: CalendarDate,
    fetcher: &mut dyn Fetcher,
    verify_all_objects: bool,
) -> Result<VerifyOutcome, UpdateError> {
    let root_req = FetchRequest::new("/root.toml".to_string(), MAX_ROOT_BYTES as u64);
    let root_bytes = match fetcher.fetch(&root_req) {
        Ok(b) => b,
        Err(FetchError::SizeLimitExceeded { .. }) => {
            return Err(UpdateError::RootTooLarge {
                limit: MAX_ROOT_BYTES,
            });
        }
        Err(e) => return Err(UpdateError::Fetch(e)),
    };
    let root_sig_req = FetchRequest::new("/root.toml.sig".to_string(), SIGNATURE_BYTES as u64);
    let root_sig = match fetcher.fetch(&root_sig_req) {
        Ok(b) => b,
        Err(FetchError::SizeLimitExceeded { .. }) => {
            return Err(UpdateError::InvalidSignature {
                target: "root.toml",
            });
        }
        Err(e) => return Err(UpdateError::Fetch(e)),
    };

    let root = verify_root_bytes(
        &root_bytes,
        &root_sig,
        &state.release_key,
        state.last_waypoint,
    )?;

    if root.serial < state.trusted_serial {
        return Err(UpdateError::RollbackSerial {
            found: root.serial,
            trusted: state.trusted_serial,
        });
    }

    let channel_release = root
        .channels
        .get(state.channel)
        .ok_or(UpdateError::MissingChannel {
            channel: state.channel,
        })?;
    if channel_release < state.installed_release {
        return Err(UpdateError::RollbackRelease {
            found: channel_release,
            installed: state.installed_release,
        });
    }

    let channel_entry = root
        .releases
        .get(&channel_release)
        .ok_or(UpdateError::MissingRelease {
            release: channel_release,
        })?;
    if channel_entry.withdrawn {
        return Err(UpdateError::ReleaseWithdrawn {
            release: channel_release,
        });
    }

    let (target_release, via_waypoint, target_entry) = match channel_entry.via {
        Some(via) if state.installed_release < via => {
            let via_entry = root
                .releases
                .get(&via)
                .ok_or(UpdateError::MissingRelease { release: via })?;
            if via_entry.withdrawn {
                return Err(UpdateError::ReleaseWithdrawn { release: via });
            }
            (via, true, via_entry)
        }
        _ => (channel_release, false, channel_entry),
    };

    assert!(target_release >= state.installed_release);

    let age_days = today.days_since(root.signed);
    let stale_notice = if age_days > root.fresh_days as i64 {
        Some(StaleNotice {
            signed: root.signed,
            today,
            age_days,
            fresh_days: root.fresh_days,
        })
    } else {
        None
    };

    let expected_index_hash =
        target_entry
            .index
            .get(&state.arch)
            .ok_or(UpdateError::MissingArch {
                release: target_release,
                arch: state.arch,
            })?;

    let idx_path = index_path(target_release, state.arch);
    let idx_sig_path = format!("{idx_path}.sig");
    let idx_req = FetchRequest::new(idx_path, MAX_INDEX_BYTES as u64);
    let index_bytes = match fetcher.fetch(&idx_req) {
        Ok(b) => b,
        Err(FetchError::SizeLimitExceeded { .. }) => {
            return Err(UpdateError::IndexTooLarge {
                limit: MAX_INDEX_BYTES,
            });
        }
        Err(e) => return Err(UpdateError::Fetch(e)),
    };
    let idx_sig_req = FetchRequest::new(idx_sig_path, SIGNATURE_BYTES as u64);
    let index_sig = match fetcher.fetch(&idx_sig_req) {
        Ok(b) => b,
        Err(FetchError::SizeLimitExceeded { .. }) => {
            return Err(UpdateError::InvalidSignature {
                target: "index.toml",
            });
        }
        Err(e) => return Err(UpdateError::Fetch(e)),
    };

    let index = verify_index_bytes(
        &index_bytes,
        &index_sig,
        expected_index_hash,
        &state.release_key,
        target_release,
        target_entry.via.or(state.last_waypoint),
    )?;

    let mut verified_packages = Vec::new();
    let mut next_packages = state.installed_packages.clone();
    for pkg in &index.package {
        let unchanged = state
            .installed_packages
            .get(&pkg.name)
            .is_some_and(|h| *h == pkg.head);
        if unchanged && !verify_all_objects {
            continue;
        }
        let obj_req = FetchRequest::new(object_path(&pkg.object), pkg.size);
        let obj_bytes = match fetcher.fetch(&obj_req) {
            Ok(b) => b,
            Err(FetchError::SizeLimitExceeded { .. }) => {
                return Err(UpdateError::ObjectTooLarge {
                    package: pkg.name.clone(),
                    limit: pkg.size,
                });
            }
            Err(e) => return Err(UpdateError::Fetch(e)),
        };
        let verified = verify_package_object(pkg, &obj_bytes, None)?;
        next_packages.insert(pkg.name.clone(), verified.head);
        verified_packages.push(verified);
    }

    let mut next_state = state.clone();
    next_state.trusted_serial = root.serial;
    next_state.installed_release = target_release;
    next_state.last_waypoint = target_entry.via.or(state.last_waypoint);
    next_state.installed_packages = next_packages;

    assert!(next_state.trusted_serial >= state.trusted_serial);
    assert!(next_state.installed_release >= state.installed_release);

    Ok(VerifyOutcome {
        root,
        target_release,
        via_waypoint,
        index,
        verified_packages,
        stale_notice,
        next_state,
    })
}

pub fn apply_update(
    state: &mut ClientState,
    today: CalendarDate,
    fetcher: &mut dyn Fetcher,
    verify_all_objects: bool,
) -> Result<VerifyOutcome, UpdateError> {
    let outcome = check_and_verify(state, today, fetcher, verify_all_objects)?;
    *state = outcome.next_state.clone();
    Ok(outcome)
}

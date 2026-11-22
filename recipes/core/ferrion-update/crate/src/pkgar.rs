use crate::MAX_PKGAR_ENTRIES;
use crate::crypto::{Blake3Hash, Keypair, PublicKey, Signature, verify_detached};
use crate::error::UpdateError;

pub const PKGAR_HEADER_SIZE: usize = 136;
pub const PKGAR_ENTRY_SIZE: usize = 308;
pub const PKGAR_ENTRY_PATH_LEN: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedFile {
    pub path: String,
    pub blake3: Blake3Hash,
    pub mode: u32,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PkgarHeadInfo {
    pub head_hash: Blake3Hash,
    pub public_key: PublicKey,
    pub entry_count: u32,
    pub flags: u32,
}

pub fn inspect_pkgar_head(package: &str, bytes: &[u8]) -> Result<PkgarHeadInfo, UpdateError> {
    if bytes.len() < PKGAR_HEADER_SIZE {
        return Err(UpdateError::PkgarInvalid {
            package: package.to_string(),
            detail: format!("archive shorter than {PKGAR_HEADER_SIZE}-byte header"),
        });
    }
    let sig = Signature::from_slice(&bytes[0..64]).expect("slice length is 64");
    let mut pk_bytes = [0u8; 32];
    pk_bytes.copy_from_slice(&bytes[64..96]);
    let public_key = PublicKey(pk_bytes);
    let mut head_bytes = [0u8; 32];
    head_bytes.copy_from_slice(&bytes[96..128]);
    let head_hash = Blake3Hash(head_bytes);
    let entry_count = u32::from_le_bytes([bytes[128], bytes[129], bytes[130], bytes[131]]);
    let flags = u32::from_le_bytes([bytes[132], bytes[133], bytes[134], bytes[135]]);

    if !verify_detached(&sig, &bytes[64..PKGAR_HEADER_SIZE], &public_key) {
        return Err(UpdateError::PkgarInvalid {
            package: package.to_string(),
            detail: "pkgar header Ed25519 signature invalid".to_string(),
        });
    }

    Ok(PkgarHeadInfo {
        head_hash,
        public_key,
        entry_count,
        flags,
    })
}

pub fn verify_pkgar(
    package: &str,
    bytes: &[u8],
    expected_head: &Blake3Hash,
    expected_key: Option<&PublicKey>,
) -> Result<Vec<VerifiedFile>, UpdateError> {
    let info = inspect_pkgar_head(package, bytes)?;
    if let Some(key) = expected_key {
        if info.public_key != *key {
            return Err(UpdateError::PkgarInvalid {
                package: package.to_string(),
                detail: "pkgar header public key mismatch".to_string(),
            });
        }
    }
    if info.head_hash != *expected_head {
        return Err(UpdateError::PkgarInvalid {
            package: package.to_string(),
            detail: format!(
                "pkgar head hash mismatch: expected {expected_head}, got {}",
                info.head_hash
            ),
        });
    }
    let version = (info.flags & 0xff) as u8;
    let packaging = ((info.flags >> 16) & 0xff) as u8;
    if version != 0 {
        return Err(UpdateError::PkgarInvalid {
            package: package.to_string(),
            detail: format!("unsupported pkgar data version {version}"),
        });
    }
    if packaging != 0 {
        return Err(UpdateError::PkgarInvalid {
            package: package.to_string(),
            detail: format!("unsupported pkgar packaging mode {packaging}"),
        });
    }
    let count = info.entry_count as usize;
    if count > MAX_PKGAR_ENTRIES {
        return Err(UpdateError::LimitExceeded {
            what: "pkgar entries",
            limit: MAX_PKGAR_ENTRIES,
        });
    }
    let entries_len =
        count
            .checked_mul(PKGAR_ENTRY_SIZE)
            .ok_or_else(|| UpdateError::PkgarInvalid {
                package: package.to_string(),
                detail: "entries table size overflow".to_string(),
            })?;
    let data_start =
        PKGAR_HEADER_SIZE
            .checked_add(entries_len)
            .ok_or_else(|| UpdateError::PkgarInvalid {
                package: package.to_string(),
                detail: "header plus entries size overflow".to_string(),
            })?;
    if data_start > bytes.len() {
        return Err(UpdateError::PkgarInvalid {
            package: package.to_string(),
            detail: "archive truncated inside entries table".to_string(),
        });
    }

    let entries_slice = &bytes[PKGAR_HEADER_SIZE..data_start];
    let actual_head = Blake3Hash::hash_bytes(entries_slice);
    if actual_head != info.head_hash {
        return Err(UpdateError::PkgarInvalid {
            package: package.to_string(),
            detail: format!(
                "entries table BLAKE3 mismatch: header says {}, computed {actual_head}",
                info.head_hash
            ),
        });
    }

    let data_slice = &bytes[data_start..];
    let mut files = Vec::with_capacity(count);
    for idx in 0..count {
        let base = idx * PKGAR_ENTRY_SIZE;
        let entry = &entries_slice[base..base + PKGAR_ENTRY_SIZE];
        let mut file_hash_bytes = [0u8; 32];
        file_hash_bytes.copy_from_slice(&entry[0..32]);
        let expected_file_hash = Blake3Hash(file_hash_bytes);
        let offset = u64::from_le_bytes(entry[32..40].try_into().expect("8 bytes"));
        let size = u64::from_le_bytes(entry[40..48].try_into().expect("8 bytes"));
        let mode = u32::from_le_bytes(entry[48..52].try_into().expect("4 bytes"));
        let raw_path = &entry[52..52 + PKGAR_ENTRY_PATH_LEN];
        let nul_pos =
            raw_path
                .iter()
                .position(|&b| b == 0)
                .ok_or_else(|| UpdateError::PkgarInvalid {
                    package: package.to_string(),
                    detail: format!("entry {idx} path missing NUL terminator"),
                })?;
        let path_str =
            std::str::from_utf8(&raw_path[..nul_pos]).map_err(|_| UpdateError::PkgarInvalid {
                package: package.to_string(),
                detail: format!("entry {idx} path is not valid UTF-8"),
            })?;
        if !valid_entry_path(path_str) {
            return Err(UpdateError::PkgarInvalid {
                package: package.to_string(),
                detail: format!("entry {idx} has unsafe path {path_str:?}"),
            });
        }

        let offset_usize = usize::try_from(offset).map_err(|_| UpdateError::PkgarInvalid {
            package: package.to_string(),
            detail: format!("entry {idx} offset overflow"),
        })?;
        let size_usize = usize::try_from(size).map_err(|_| UpdateError::PkgarInvalid {
            package: package.to_string(),
            detail: format!("entry {idx} size overflow"),
        })?;
        let end =
            offset_usize
                .checked_add(size_usize)
                .ok_or_else(|| UpdateError::PkgarInvalid {
                    package: package.to_string(),
                    detail: format!("entry {idx} range overflow"),
                })?;
        if end > data_slice.len() {
            return Err(UpdateError::PkgarInvalid {
                package: package.to_string(),
                detail: format!("entry {idx} ({path_str}) data runs past end of archive"),
            });
        }
        let actual_file_hash = Blake3Hash::hash_bytes(&data_slice[offset_usize..end]);
        if actual_file_hash != expected_file_hash {
            return Err(UpdateError::PkgarInvalid {
                package: package.to_string(),
                detail: format!(
                    "entry {idx} ({path_str}) BLAKE3 mismatch: expected {expected_file_hash}, got {actual_file_hash}"
                ),
            });
        }
        files.push(VerifiedFile {
            path: path_str.to_string(),
            blake3: expected_file_hash,
            mode,
            size,
        });
    }
    Ok(files)
}

fn valid_entry_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') {
        return false;
    }
    for comp in path.split('/') {
        if comp.is_empty() || comp == "." || comp == ".." {
            return false;
        }
    }
    true
}

pub fn build_uncompressed_pkgar(
    keypair: &Keypair,
    files: &[(&str, u32, &[u8])],
) -> Result<(Vec<u8>, Blake3Hash), String> {
    if files.len() > MAX_PKGAR_ENTRIES {
        return Err(format!("too many pkgar entries: {}", files.len()));
    }
    let mut entries_buf = Vec::with_capacity(files.len() * PKGAR_ENTRY_SIZE);
    let mut data_buf = Vec::new();
    for &(path, mode, content) in files {
        if !valid_entry_path(path) {
            return Err(format!("invalid entry path {path:?}"));
        }
        let path_bytes = path.as_bytes();
        if path_bytes.len() >= PKGAR_ENTRY_PATH_LEN {
            return Err(format!("entry path too long: {path:?}"));
        }
        let offset = data_buf.len() as u64;
        let size = content.len() as u64;
        let file_hash = Blake3Hash::hash_bytes(content);
        data_buf.extend_from_slice(content);

        let mut entry = [0u8; PKGAR_ENTRY_SIZE];
        entry[0..32].copy_from_slice(&file_hash.0);
        entry[32..40].copy_from_slice(&offset.to_le_bytes());
        entry[40..48].copy_from_slice(&size.to_le_bytes());
        entry[48..52].copy_from_slice(&mode.to_le_bytes());
        entry[52..52 + path_bytes.len()].copy_from_slice(path_bytes);
        entries_buf.extend_from_slice(&entry);
    }

    let head_hash = Blake3Hash::hash_bytes(&entries_buf);
    let count = files.len() as u32;
    let flags: u32 = 0;

    let mut header_body = [0u8; 72];
    header_body[0..32].copy_from_slice(&keypair.public.0);
    header_body[32..64].copy_from_slice(&head_hash.0);
    header_body[64..68].copy_from_slice(&count.to_le_bytes());
    header_body[68..72].copy_from_slice(&flags.to_le_bytes());

    let sig = keypair.sign(&header_body);
    let mut out = Vec::with_capacity(PKGAR_HEADER_SIZE + entries_buf.len() + data_buf.len());
    out.extend_from_slice(&sig.0);
    out.extend_from_slice(&header_body);
    out.extend_from_slice(&entries_buf);
    out.extend_from_slice(&data_buf);
    Ok((out, head_hash))
}

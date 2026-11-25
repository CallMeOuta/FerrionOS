use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::Read;
use std::process::ExitCode;

use ferrion_manifest::{
    CATALOG, MAX_GRANTS_FILE_BYTES, MAX_ID_LEN, MAX_MANIFEST_BYTES, MAX_NON_APP_PACKAGES,
    MAX_RESERVED_IDS, ManifestError, Origin, parse_grants_bytes, parse_manifest_bytes,
    validate_catalog_invariants,
};

const MAX_RESERVED_FILE_BYTES: usize = (MAX_RESERVED_IDS + 2) * (MAX_ID_LEN + 2);

fn read_bounded(path: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut buf = Vec::new();
    let mut handle = file.take((max_bytes as u64).saturating_add(1));
    handle
        .read_to_end(&mut buf)
        .map_err(|e| format!("{path}: {e}"))?;
    Ok(buf)
}

fn load_reserved_ids(path: &str) -> Result<BTreeSet<String>, (i32, String)> {
    let bytes =
        read_bounded(path, MAX_RESERVED_FILE_BYTES).map_err(|msg| (2, format!("ERR io: {msg}")))?;
    if bytes.len() > MAX_RESERVED_FILE_BYTES {
        let err = ManifestError::TooManyReservedIds {
            count: MAX_RESERVED_IDS + 1,
            limit: MAX_RESERVED_IDS,
        };
        return Err((1, format!("ERR {}: {err}", err.reason_code())));
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| {
        let err = ManifestError::InvalidUtf8;
        (1, format!("ERR {}: {err}", err.reason_code()))
    })?;
    let mut ids = BTreeSet::new();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        ids.insert(line.to_owned());
        if ids.len() > MAX_RESERVED_IDS {
            let err = ManifestError::TooManyReservedIds {
                count: ids.len(),
                limit: MAX_RESERVED_IDS,
            };
            return Err((1, format!("ERR {}: {err}", err.reason_code())));
        }
    }
    Ok(ids)
}

fn run() -> Result<(), (i32, String)> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("catalog") if args.len() == 1 => {
            validate_catalog_invariants();
            for spec in CATALOG {
                println!(
                    "{}\t{}\t{}",
                    spec.name,
                    spec.match_kind.as_str(),
                    spec.class.as_str()
                );
            }
            Ok(())
        }
        Some("limits") if args.len() == 1 => {
            println!("MAX_MANIFEST_BYTES={MAX_MANIFEST_BYTES}");
            println!("MAX_RESERVED_IDS={MAX_RESERVED_IDS}");
            println!("MAX_NON_APP_PACKAGES={MAX_NON_APP_PACKAGES}");
            Ok(())
        }
        Some("repo") if args.len() == 3 => {
            let package = &args[1];
            let path = &args[2];
            if package.is_empty() {
                return Err((2, "ERR usage: package name must not be empty".to_owned()));
            }
            let bytes =
                read_bounded(path, MAX_MANIFEST_BYTES).map_err(|msg| (2, format!("ERR io: {msg}")))?;
            match parse_manifest_bytes(&bytes, Origin::Repository { package }) {
                Ok(manifest) => {
                    println!(
                        "OK {} {} {}",
                        manifest.id,
                        manifest.kind.as_str(),
                        manifest.capabilities.len()
                    );
                    Ok(())
                }
                Err(err) => Err((1, format!("ERR {}: {err}", err.reason_code()))),
            }
        }
        Some("outside") if args.len() == 3 => {
            let reserved_path = &args[1];
            let path = &args[2];
            let reserved_ids = load_reserved_ids(reserved_path)?;
            let bytes =
                read_bounded(path, MAX_MANIFEST_BYTES).map_err(|msg| (2, format!("ERR io: {msg}")))?;
            match parse_manifest_bytes(
                &bytes,
                Origin::Outside {
                    reserved_ids: &reserved_ids,
                },
            ) {
                Ok(manifest) => {
                    println!(
                        "OK {} {} {}",
                        manifest.id,
                        manifest.kind.as_str(),
                        manifest.capabilities.len()
                    );
                    Ok(())
                }
                Err(err) => Err((1, format!("ERR {}: {err}", err.reason_code()))),
            }
        }
        Some("grants") if args.len() == 2 => {
            let path = &args[1];
            let bytes = read_bounded(path, MAX_GRANTS_FILE_BYTES)
                .map_err(|msg| (2, format!("ERR io: {msg}")))?;
            match parse_grants_bytes(&bytes) {
                Ok(grants) => {
                    println!("OK grants {}", grants.apps.len());
                    Ok(())
                }
                Err(err) => Err((1, format!("ERR {}: {err}", err.reason_code()))),
            }
        }
        _ => Err((
            2,
            "ERR usage: ferrion-manifest-check catalog | limits | grants <path> | repo <package> <path> | outside <reserved-ids-file> <path>"
                .to_owned(),
        )),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, msg)) => {
            eprintln!("{msg}");
            ExitCode::from(code as u8)
        }
    }
}

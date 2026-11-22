use redoxfs::key_tool::Secret;
use redoxfs::key_tool::{
    EXIT_USAGE, MAX_ROLL_RETRIES, Outcome, read_secret, recovery_key, require_seeded_random,
};
use redoxfs::{
    Disk, DiskFile, FileSystem, HEADER_RING, Header, KeySlot, Salt, has_other_live_slot,
};
use std::{env, io};

type Result<T, E = Box<dyn std::error::Error>> = std::result::Result<T, E>;

enum KeyErr {
    Usage(String),
    Outcome {
        outcome: Outcome,
        slot: Option<usize>,
        message: String,
    },
}

impl KeyErr {
    fn usage(msg: impl Into<String>) -> Self {
        Self::Usage(msg.into())
    }

    fn unchanged(msg: impl Into<String>) -> Self {
        Self::Outcome {
            outcome: Outcome::Unchanged,
            slot: None,
            message: msg.into(),
        }
    }
}

enum Volume {
    Disk {
        path: String,
        block: u64,
        fs: Option<FileSystem<DiskFile>>,
    },
    #[cfg(target_os = "redox")]
    Mounted(libredox::Fd),
}

impl Volume {
    fn open(path: &str, password: &[u8]) -> Result<Self> {
        #[cfg(target_os = "redox")]
        if path == "/" {
            return Ok(Self::Mounted(libredox::Fd::open(
                path,
                libredox::flag::O_RDONLY | libredox::flag::O_DIRECTORY | libredox::flag::O_CLOEXEC,
                0,
            )?));
        }
        let fs = FileSystem::open(DiskFile::open(path)?, Some(password), None, false)?;
        let block = fs.block;
        Ok(Self::Disk {
            path: path.to_string(),
            block,
            fs: Some(fs),
        })
    }

    fn header(&mut self) -> Result<Header> {
        match self {
            Self::Disk { fs, .. } => Ok(fs.as_ref().ok_or("volume is closed")?.header),
            #[cfg(target_os = "redox")]
            Self::Mounted(fd) => Self::read_mounted_header(fd),
        }
    }

    fn reread_header(&mut self) -> Result<Header> {
        match self {
            Self::Disk { path, block, fs } => {
                let mut best: Option<Header> = None;
                if let Some(active) = fs.as_mut() {
                    for i in 0..HEADER_RING {
                        let mut candidate = Header::default();
                        unsafe { active.disk.read_at(*block + i, &mut candidate)? };
                        if !candidate.valid() {
                            continue;
                        }
                        if best.is_none_or(|cur| candidate.generation() > cur.generation()) {
                            best = Some(candidate);
                        }
                    }
                    let header = best.ok_or("no valid header found on re-read")?;
                    active.header = header;
                    Ok(header)
                } else {
                    let mut disk = DiskFile::open(path)?;
                    for i in 0..HEADER_RING {
                        let mut candidate = Header::default();
                        unsafe { disk.read_at(*block + i, &mut candidate)? };
                        if !candidate.valid() {
                            continue;
                        }
                        if best.is_none_or(|cur| candidate.generation() > cur.generation()) {
                            best = Some(candidate);
                        }
                    }
                    best.ok_or_else(|| "no valid header found on re-read".into())
                }
            }
            #[cfg(target_os = "redox")]
            Self::Mounted(fd) => Self::read_mounted_header(fd),
        }
    }

    #[cfg(target_os = "redox")]
    fn read_mounted_header(fd: &libredox::Fd) -> Result<Header> {
        use redoxfs::key_tool::{KEY_CALL, KEY_HEADER};
        let mut header = Header::default();
        let count = fd.call_rw(
            &mut header,
            syscall::CallFlags::empty(),
            &[KEY_CALL, KEY_HEADER, 0],
        )?;
        if count != header.len() || !header.valid() {
            return Err("invalid header reply".into());
        }
        Ok(header)
    }

    fn set(
        &mut self,
        index: usize,
        old: KeySlot,
        slot: KeySlot,
        password: &[u8],
        reopen: bool,
    ) -> Result<()> {
        match self {
            Self::Disk { path, fs, .. } => {
                if reopen {
                    let disk = match fs.take() {
                        Some(old_fs) => old_fs.disk,
                        None => DiskFile::open(&*path)?,
                    };
                    *fs = Some(FileSystem::open(disk, Some(password), None, false)?);
                }
                let active = fs.as_mut().ok_or("volume is closed")?;
                active.set_key_slot(index, slot)?;
                active.disk.file.sync_all()?;
            }
            #[cfg(target_os = "redox")]
            Self::Mounted(fd) => {
                use redoxfs::key_tool::{KEY_CALL, KEY_SET};
                let mut payload = Secret(Vec::with_capacity(96 + password.len()));
                payload.0.extend_from_slice(&old.to_bytes());
                payload.0.extend_from_slice(&slot.to_bytes());
                payload.0.extend_from_slice(password);
                fd.call_rw(
                    &mut payload.0,
                    syscall::CallFlags::empty(),
                    &[KEY_CALL, KEY_SET, index as u64],
                )?;
            }
        }
        #[cfg(not(target_os = "redox"))]
        let _ = (old, password);
        Ok(())
    }
}

fn apply_slot(
    volume: &mut Volume,
    index: usize,
    initial_old: KeySlot,
    target: KeySlot,
    auth_initial: &[u8],
    auth_applied: &[u8],
) -> (Outcome, Option<String>) {
    let mut current_old = initial_old;
    let mut auth = auth_initial;
    for attempt in 0..=MAX_ROLL_RETRIES {
        match volume.set(index, current_old, target, auth, attempt > 0) {
            Ok(()) => return (Outcome::Applied { complete: true }, None),
            Err(err) => {
                let reread = match volume.reread_header() {
                    Ok(header) => header,
                    Err(read_err) => {
                        return (
                            Outcome::Unknown,
                            Some(format!("{err}; header re-read failed: {read_err}")),
                        );
                    }
                };
                let observed = reread.key_slots[index];
                if observed.to_bytes() == target.to_bytes() {
                    if attempt == MAX_ROLL_RETRIES {
                        return (
                            Outcome::Applied { complete: false },
                            Some(format!(
                                "{err}; header ring roll incomplete after {MAX_ROLL_RETRIES} retries"
                            )),
                        );
                    }
                    current_old = observed;
                    auth = auth_applied;
                    continue;
                }
                if observed.to_bytes() == initial_old.to_bytes() {
                    return (Outcome::Unchanged, Some(err.to_string()));
                }
                return (
                    Outcome::Unknown,
                    Some(format!(
                        "{err}; header slot {index} matches neither old nor new"
                    )),
                );
            }
        }
    }
    unreachable!()
}

fn run() -> std::result::Result<(), KeyErr> {
    let mut args = env::args().skip(1);
    let operation = args.next().ok_or_else(|| {
        KeyErr::usage(
            "usage: redoxfs-key list|add|change|remove [--key-file PATH|-] [--new-key-file PATH] [--slot N] [--recovery] VOLUME",
        )
    })?;
    let mut key_file = None;
    let mut new_key_file = None;
    let mut index = None;
    let mut recovery = false;
    let mut path = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--key-file" => {
                key_file = Some(
                    args.next()
                        .ok_or_else(|| KeyErr::usage("missing key file"))?,
                )
            }
            "--new-key-file" => {
                new_key_file = Some(
                    args.next()
                        .ok_or_else(|| KeyErr::usage("missing new key file"))?,
                )
            }
            "--slot" => {
                let raw = args.next().ok_or_else(|| KeyErr::usage("missing slot"))?;
                index = Some(
                    raw.parse::<usize>()
                        .map_err(|err| KeyErr::usage(err.to_string()))?,
                );
            }
            "--recovery" => recovery = true,
            _ if !arg.starts_with('-') && path.is_none() => path = Some(arg),
            _ => return Err(KeyErr::usage("unknown or duplicate argument")),
        }
    }
    let path = path.ok_or_else(|| KeyErr::usage("missing volume (use / for mounted root)"))?;
    if !matches!(operation.as_str(), "list" | "add" | "change" | "remove")
        || (recovery && (operation != "add" || new_key_file.is_some()))
        || (new_key_file.is_some() && !matches!(operation.as_str(), "add" | "change"))
    {
        return Err(KeyErr::usage("invalid operation or options"));
    }
    let both_on_stdin = key_file.as_deref() == Some("-") && new_key_file.as_deref() == Some("-");
    let (password, mut stdin_new) = if both_on_stdin {
        let both = read_secret(Some("-"), "").map_err(|err| KeyErr::unchanged(err.to_string()))?;
        let split = both.0.iter().position(|&b| b == 0).ok_or_else(|| {
            KeyErr::usage(
                "with --key-file - and --new-key-file -, stdin holds OLD, a NUL byte, then NEW",
            )
        })?;
        let old = Secret(both.0[..split].to_vec());
        let new = Secret(both.0[split + 1..].to_vec());
        if old.0.is_empty() || new.0.is_empty() || new.0.contains(&0) {
            return Err(KeyErr::unchanged("passphrase must contain 1 to 4096 bytes"));
        }
        (old, Some(new))
    } else {
        (
            read_secret(key_file.as_deref(), "Current passphrase")
                .map_err(|err| KeyErr::unchanged(err.to_string()))?,
            None,
        )
    };
    let mut volume =
        Volume::open(&path, &password.0).map_err(|err| KeyErr::unchanged(err.to_string()))?;
    let header = volume
        .header()
        .map_err(|err| KeyErr::unchanged(err.to_string()))?;
    let (unlocked, keys) = header
        .unlock(&password.0)
        .ok_or_else(|| KeyErr::unchanged("incorrect passphrase"))?;
    if operation == "list" {
        for (index, slot) in header.key_slots.iter().enumerate() {
            if !slot.is_empty() {
                println!("slot {index}: in use");
            }
        }
        return Ok(());
    }
    let index = match (operation.as_str(), index) {
        ("add", None) => header
            .key_slots
            .iter()
            .position(KeySlot::is_empty)
            .ok_or_else(|| KeyErr::unchanged("no empty slots"))?,
        ("change", None) => unlocked,
        ("remove", None) => return Err(KeyErr::usage("remove requires --slot")),
        (_, Some(index)) if index < 64 => index,
        _ => return Err(KeyErr::usage("slot out of range")),
    };
    let old = header.key_slots[index];
    if (operation == "add" && !old.is_empty()) || (operation == "change" && old.is_empty()) {
        return Err(KeyErr::unchanged("slot is not available for this operation"));
    }
    let new_secret;
    let slot = if operation == "remove" {
        if !has_other_live_slot(&header.key_slots, index) {
            return Err(KeyErr::unchanged("cannot remove the last slot"));
        }
        KeySlot::default()
    } else {
        require_seeded_random().map_err(|err| KeyErr::unchanged(err.to_string()))?;
        new_secret = if recovery {
            recovery_key().map_err(|err| KeyErr::unchanged(err.to_string()))?
        } else {
            let value = match stdin_new.take() {
                Some(value) => value,
                None => read_secret(new_key_file.as_deref(), "New passphrase")
                    .map_err(|err| KeyErr::unchanged(err.to_string()))?,
            };
            if new_key_file.is_none() {
                let confirmation = read_secret(None, "Confirm new passphrase")
                    .map_err(|err| KeyErr::unchanged(err.to_string()))?;
                if value.0 != confirmation.0 {
                    return Err(KeyErr::unchanged("passphrases differ"));
                }
            }
            value
        };
        let salt = Salt::new().map_err(|err| KeyErr::unchanged(err.to_string()))?;
        let slot = KeySlot::new(&new_secret.0, salt, keys)
            .map_err(|err| KeyErr::unchanged(err.to_string()))?;
        let mut check = header;
        check.key_slots = [KeySlot::default(); 64];
        check.key_slots[index] = slot;
        if check.unlock(&new_secret.0).is_none() {
            return Err(KeyErr::unchanged("new slot verification failed"));
        }
        let auth_applied = if operation == "change" {
            new_secret.0.as_slice()
        } else {
            password.0.as_slice()
        };
        let (outcome, err) = apply_slot(
            &mut volume,
            index,
            old,
            slot,
            &password.0,
            auth_applied,
        );
        if outcome != (Outcome::Applied { complete: true }) {
            return Err(KeyErr::Outcome {
                outcome,
                slot: Some(index),
                message: err.unwrap_or_else(|| "key slot update failed".to_string()),
            });
        }
        if recovery {
            use std::io::Write;
            let mut out = io::stdout().lock();
            out.write_all(b"Recovery key (type all digits without spaces): ")
                .map_err(|err| KeyErr::Outcome {
                    outcome: Outcome::Applied { complete: false },
                    slot: Some(index),
                    message: err.to_string(),
                })?;
            for (i, group) in new_secret.0.chunks_exact(6).enumerate() {
                if i != 0 {
                    out.write_all(b" ").map_err(|err| KeyErr::Outcome {
                        outcome: Outcome::Applied { complete: false },
                        slot: Some(index),
                        message: err.to_string(),
                    })?;
                }
                out.write_all(group).map_err(|err| KeyErr::Outcome {
                    outcome: Outcome::Applied { complete: false },
                    slot: Some(index),
                    message: err.to_string(),
                })?;
            }
            out.write_all(b"
").map_err(|err| KeyErr::Outcome {
                outcome: Outcome::Applied { complete: false },
                slot: Some(index),
                message: err.to_string(),
            })?;
            out.flush().map_err(|err| KeyErr::Outcome {
                outcome: Outcome::Applied { complete: false },
                slot: Some(index),
                message: err.to_string(),
            })?;
        }
        println!("slot {index}: updated; header ring rolled");
        println!("slot {index}: outcome={}", outcome.label());
        return Ok(());
    };
    let (outcome, err) = apply_slot(&mut volume, index, old, slot, &password.0, &password.0);
    if outcome != (Outcome::Applied { complete: true }) {
        return Err(KeyErr::Outcome {
            outcome,
            slot: Some(index),
            message: err.unwrap_or_else(|| "key slot removal failed".to_string()),
        });
    }
    println!("slot {index}: removed; header ring rolled");
    println!("slot {index}: outcome={}", outcome.label());
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        match error {
            KeyErr::Usage(msg) => {
                eprintln!("redoxfs-key: {msg}");
                std::process::exit(EXIT_USAGE);
            }
            KeyErr::Outcome {
                outcome,
                slot,
                message,
            } => {
                if let Some(slot) = slot {
                    println!("slot {slot}: outcome={}", outcome.label());
                }
                eprintln!("redoxfs-key: {message}");
                std::process::exit(outcome.exit_code());
            }
        }
    }
}

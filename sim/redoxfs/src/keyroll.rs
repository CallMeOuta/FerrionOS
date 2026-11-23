use redoxfs::key_tool::{
    EXIT_APPLIED_PARTIAL, EXIT_UNCHANGED, EXIT_UNKNOWN, EXIT_USAGE, MAX_ROLL_RETRIES, Outcome,
};
use redoxfs::{
    FileSystem, HEADER_RING, Header, KeySlot, Salt, has_other_live_slot, key_slot_cas_accepts,
};
use syscall::error::{EINVAL, EIO, ENOKEY};

use crate::disk::{self, Durability, SimDisk};
use crate::model::Time;
use crate::rng::{Rng, seed_entropy};

const DISK_BYTES: usize = 16 << 20;
const START: Time = (1_700_000_000, 0);
const FIXED_POINTS: [u64; 4] = [1, 2, 128, 255];

pub struct KeyRollStats {
    pub ops: u64,
    pub injected_errors: u64,
    pub retries: u64,
    pub exhausted: u64,
}

fn read_newest_header(media: &[u8], block: u64) -> Result<Header, String> {
    let mut best: Option<Header> = None;
    for slot in 0..HEADER_RING {
        let start = ((block + slot) as usize) * disk::BLOCK;
        let mut header = Header::default();
        header.copy_from_slice(&media[start..start + disk::BLOCK]);
        if !header.valid() {
            continue;
        }
        if best.is_none_or(|cur| header.generation() > cur.generation()) {
            best = Some(header);
        }
    }
    best.ok_or_else(|| "no valid header found in ring".to_string())
}

fn verify_ring_consistent(fs: &FileSystem<SimDisk>) -> Result<(), String> {
    let expected = fs.header.key_slots;
    let media = fs.disk.media();
    let mut valid_count = 0usize;
    for slot in 0..HEADER_RING {
        let start = ((fs.block + slot) as usize) * disk::BLOCK;
        let mut header = Header::default();
        header.copy_from_slice(&media[start..start + disk::BLOCK]);
        if !header.valid() {
            continue;
        }
        valid_count += 1;
        let found = header.key_slots;
        for (index, (got, want)) in found.iter().zip(expected.iter()).enumerate() {
            if got.to_bytes() != want.to_bytes() {
                return Err(format!(
                    "ring header {slot} (generation {}) retains stale slot {index}",
                    header.generation()
                ));
            }
        }
    }
    if valid_count == 0 {
        return Err("ring contains no valid headers".to_string());
    }
    Ok(())
}

fn ring_has_old_slot(media: &[u8], block: u64, index: usize, old: KeySlot) -> bool {
    let old_bytes = old.to_bytes();
    for slot in 0..HEADER_RING {
        let start = ((block + slot) as usize) * disk::BLOCK;
        let mut header = Header::default();
        header.copy_from_slice(&media[start..start + disk::BLOCK]);
        if header.valid() && header.key_slots[index].to_bytes() == old_bytes {
            return true;
        }
    }
    false
}

fn verify_open(media: &[u8], password: &[u8], should_open: bool, label: &str) -> Result<(), String> {
    let disk = SimDisk::new(media.to_vec(), Durability::Ordered);
    match FileSystem::open(disk, Some(password), None, false) {
        Ok(_) if should_open => Ok(()),
        Ok(_) => Err(format!("{label}: volume opened unexpectedly")),
        Err(err) if !should_open && err.errno == ENOKEY => Ok(()),
        Err(err) if should_open => Err(format!("{label}: volume failed to open: {err}")),
        Err(err) => Err(format!("{label}: expected ENOKEY, got {err}")),
    }
}

fn make_slot(header: &Header, auth: &[u8], new_pass: &[u8]) -> Result<KeySlot, String> {
    let (_, keys) = header
        .unlock(auth)
        .ok_or_else(|| "failed to unlock header to derive new slot".to_string())?;
    let salt = Salt::new().map_err(|err| format!("salt: {err}"))?;
    KeySlot::new(new_pass, salt, keys).map_err(|err| format!("key slot: {err}"))
}

fn run_roll_case(
    fs: &mut FileSystem<SimDisk>,
    index: usize,
    target: KeySlot,
    fault_at: u64,
    exhaust: bool,
    stats: &mut KeyRollStats,
) -> Result<Outcome, String> {
    assert!(fault_at >= 1 && fault_at <= HEADER_RING);
    let initial_old = fs.header.key_slots[index];
    let mut current_old = initial_old;
    let mut dummy_bytes = [0x5Au8; 48];
    if dummy_bytes == initial_old.to_bytes() || dummy_bytes == target.to_bytes() {
        dummy_bytes[0] ^= 0xFF;
    }

    fs.disk.arm_write_error(fault_at);
    let mut outcome = Outcome::Unknown;
    for attempt in 0..=MAX_ROLL_RETRIES {
        if attempt > 0 && exhaust {
            fs.disk.arm_write_error(1);
        }
        let res = fs.set_key_slot(index, target);
        fs.disk.disarm_write_error();
        match res {
            Ok(()) => {
                outcome = Outcome::Applied { complete: true };
                break;
            }
            Err(err) => {
                if err.errno != EIO {
                    return Err(format!("expected EIO on injected fault, got {err}"));
                }
                stats.injected_errors += 1;
                let observed_header = read_newest_header(fs.disk.media(), fs.block)?;
                let observed = observed_header.key_slots[index];
                assert!(key_slot_cas_accepts(
                    observed.to_bytes(),
                    initial_old.to_bytes(),
                    target.to_bytes()
                ));
                assert!(key_slot_cas_accepts(
                    observed.to_bytes(),
                    current_old.to_bytes(),
                    target.to_bytes()
                ));
                assert!(!key_slot_cas_accepts(
                    dummy_bytes,
                    current_old.to_bytes(),
                    target.to_bytes()
                ));
                if target.is_empty() {
                    assert!(has_other_live_slot(&observed_header.key_slots, index));
                }
                if observed.to_bytes() == target.to_bytes() {
                    if attempt == 0 && fault_at > 1 && initial_old.to_bytes() != target.to_bytes() {
                        assert!(ring_has_old_slot(fs.disk.media(), fs.block, index, initial_old));
                    }
                    if attempt == MAX_ROLL_RETRIES {
                        stats.exhausted += 1;
                        outcome = Outcome::Applied { complete: false };
                        break;
                    }
                    current_old = observed;
                    stats.retries += 1;
                    continue;
                }
                if observed.to_bytes() == initial_old.to_bytes() {
                    outcome = Outcome::Unchanged;
                    break;
                }
                return Err(format!(
                    "slot {index} matched neither old nor target after write error"
                ));
            }
        }
    }

    if outcome == Outcome::Unchanged {
        assert_eq!(fault_at, 1);
        fs.set_key_slot(index, target)
            .map_err(|err| format!("recovery roll after unchanged failed: {err}"))?;
    } else if outcome == (Outcome::Applied { complete: false }) {
        assert!(exhaust);
        let observed_header = read_newest_header(fs.disk.media(), fs.block)?;
        assert_eq!(observed_header.key_slots[index].to_bytes(), target.to_bytes());
        assert!(key_slot_cas_accepts(
            observed_header.key_slots[index].to_bytes(),
            initial_old.to_bytes(),
            target.to_bytes()
        ));
        fs.set_key_slot(index, target)
            .map_err(|err| format!("final idempotent roll after exhaustion failed: {err}"))?;
    }

    verify_ring_consistent(fs)?;
    stats.ops += 1;
    Ok(outcome)
}

pub fn run_seed(seed: u64, ops: u64) -> Result<KeyRollStats, String> {
    assert_eq!(Outcome::Applied { complete: true }.exit_code(), 0);
    assert_eq!(EXIT_USAGE, 1);
    assert_eq!(Outcome::Unchanged.exit_code(), EXIT_UNCHANGED);
    assert_eq!(Outcome::Applied { complete: false }.exit_code(), EXIT_APPLIED_PARTIAL);
    assert_eq!(Outcome::Unknown.exit_code(), EXIT_UNKNOWN);

    let mut rng = Rng::new(seed);
    seed_entropy(rng.next_u64());
    let mut stats = KeyRollStats {
        ops: 0,
        injected_errors: 0,
        retries: 0,
        exhausted: 0,
    };

    let p0 = b"ferrion-keyroll-root-pass";
    let disk = SimDisk::new(vec![0; DISK_BYTES], Durability::Ordered);
    let mut fs = FileSystem::create(disk, Some(p0), START.0, START.1)
        .map_err(|err| format!("create encrypted fs: {err}"))?;

    assert!(!has_other_live_slot(&fs.header.key_slots, 0));
    let last_err = fs.set_key_slot(0, KeySlot::default());
    if last_err.err().map(|err| err.errno) != Some(EINVAL) {
        return Err("removing the only live slot did not fail with EINVAL".to_string());
    }
    assert!(has_other_live_slot(&fs.header.key_slots, 1));
    fs.set_key_slot(1, KeySlot::default())
        .map_err(|err| format!("idempotent remove on already-empty slot failed: {err}"))?;
    verify_ring_consistent(&fs)?;

    let mut pass_seq = 1u64;
    let total_cycles = ops.max(1);
    for cycle in 0..total_cycles {
        let fault_add = FIXED_POINTS
            .get(cycle as usize)
            .copied()
            .unwrap_or_else(|| rng.range(1, HEADER_RING));
        let fault_change = FIXED_POINTS
            .get((cycle as usize + 1) % FIXED_POINTS.len())
            .copied()
            .filter(|_| cycle < FIXED_POINTS.len() as u64)
            .unwrap_or_else(|| rng.range(1, HEADER_RING));
        let fault_remove = FIXED_POINTS
            .get((cycle as usize + 2) % FIXED_POINTS.len())
            .copied()
            .filter(|_| cycle < FIXED_POINTS.len() as u64)
            .unwrap_or_else(|| rng.range(1, HEADER_RING));
        let exhaust = cycle % 2 == 1;

        let p_add = format!("slot1-add-{seed}-{pass_seq}");
        pass_seq += 1;
        let add_slot = make_slot(&fs.header, p0, p_add.as_bytes())?;
        let add_out = run_roll_case(&mut fs, 1, add_slot, fault_add, exhaust && fault_add > 1, &mut stats)?;
        if fault_add == 1 {
            assert_eq!(add_out, Outcome::Unchanged);
        } else if exhaust {
            assert_eq!(add_out, Outcome::Applied { complete: false });
        } else {
            assert_eq!(add_out, Outcome::Applied { complete: true });
        }
        verify_open(fs.disk.media(), p_add.as_bytes(), true, "after add")?;

        let p_chg = format!("slot1-chg-{seed}-{pass_seq}");
        pass_seq += 1;
        let chg_slot = make_slot(&fs.header, p_add.as_bytes(), p_chg.as_bytes())?;
        let chg_out = run_roll_case(&mut fs, 1, chg_slot, fault_change, exhaust && fault_change > 1, &mut stats)?;
        if fault_change == 1 {
            assert_eq!(chg_out, Outcome::Unchanged);
        } else if exhaust {
            assert_eq!(chg_out, Outcome::Applied { complete: false });
        } else {
            assert_eq!(chg_out, Outcome::Applied { complete: true });
        }
        verify_open(fs.disk.media(), p_chg.as_bytes(), true, "after change new")?;
        verify_open(fs.disk.media(), p_add.as_bytes(), false, "after change old")?;

        let rem_out = run_roll_case(
            &mut fs,
            1,
            KeySlot::default(),
            fault_remove,
            exhaust && fault_remove > 1,
            &mut stats,
        )?;
        if fault_remove == 1 {
            assert_eq!(rem_out, Outcome::Unchanged);
        } else if exhaust {
            assert_eq!(rem_out, Outcome::Applied { complete: false });
        } else {
            assert_eq!(rem_out, Outcome::Applied { complete: true });
        }
        verify_open(fs.disk.media(), p_chg.as_bytes(), false, "after remove old")?;
        verify_open(fs.disk.media(), p0, true, "after remove root")?;
    }

    Ok(stats)
}

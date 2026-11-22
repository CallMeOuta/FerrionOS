use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::process::ExitCode;

use ferrion_update::client::{
    ClientState, FetchRequest, Fetcher, apply_update, index_path, object_path, rotate_release_key,
};
use ferrion_update::crypto::{Blake3Hash, Keypair};
use ferrion_update::error::{FetchError, UpdateError};
use ferrion_update::format::{
    Arch, CalendarDate, Channel, Channels, IndexFile, KeyRotation, PackageEntry, PackageSet,
    ReleaseEntry, ReleaseNumber, RootFile, render_index_toml, render_root_toml,
};
use ferrion_update::pkgar::build_uncompressed_pkgar;
use ferrion_update::{FORMAT_VERSION, MAX_ROOT_BYTES, USER_AGENT};

const MAX_SEEDS: usize = 100_000;
const MAX_STEPS: usize = 100_000;
const MAX_RELEASES_PER_DAY: u8 = 95;

struct Clock {
    day: i64,
    counter: u8,
}

impl Clock {
    fn base_date() -> CalendarDate {
        CalendarDate::new(2026, 11, 1).expect("valid base date")
    }

    fn today(&self) -> CalendarDate {
        CalendarDate::from_days_since_epoch(Self::base_date().days_since_epoch() + self.day)
            .expect("valid offset date")
    }

    fn next_release(&mut self) -> ReleaseNumber {
        if self.counter >= MAX_RELEASES_PER_DAY {
            self.day += 1;
            self.counter = 0;
        }
        let d = Clock::today(self);
        let raw = (d.year as u64) * 1_000_000
            + (d.month as u64) * 10_000
            + (d.day as u64) * 100
            + u64::from(self.counter);
        self.counter += 1;
        ReleaseNumber::new(raw).expect("valid release number")
    }

    fn advance_days(&mut self, days: i64) {
        assert!(days >= 0);
        self.day += days;
        self.counter = 0;
    }
}

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9e37_79b9_7f4a_7c15 | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        assert!(n > 0);
        (self.next() % n as u64) as usize
    }

    fn seed32(&mut self) -> [u8; 32] {
        let mut out = [0u8; 32];
        for chunk in out.chunks_exact_mut(8) {
            chunk.copy_from_slice(&self.next().to_le_bytes());
        }
        out
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            out.push(self.next() as u8);
        }
        out
    }
}

#[derive(Default)]
struct Counts {
    case1_wrong_key: u64,
    case2_replay_serial: u64,
    case3_channel_downgrade: u64,
    case4_index_hash_mismatch: u64,
    case5_object_byte_flip: u64,
    case6_object_too_large: u64,
    case7_unknown_key_or_format: u64,
    case8_stale_then_fresh: u64,
    extra_root_too_large: u64,
    extra_bad_index_sig: u64,
    extra_index_release_mismatch: u64,
    extra_object_truncated: u64,
    extra_pkgar_corrupt: u64,
    extra_waypoint: u64,
    extra_withdrawn: u64,
    extra_key_rotation: u64,
    valid_upgrades: u64,
    total_requests: u64,
}

#[derive(Clone)]
struct PublishedSnapshot {
    release: ReleaseNumber,
    serial: u64,
    root_bytes: Vec<u8>,
    root_sig: Vec<u8>,
    index_bytes: Vec<u8>,
    index_sig: Vec<u8>,
    index_hash: Blake3Hash,
    objects: BTreeMap<String, Vec<u8>>,
}

struct MockFetcher {
    files: BTreeMap<String, Vec<u8>>,
    requests: u64,
}

impl MockFetcher {
    fn from_snapshot(snap: &PublishedSnapshot, arch: Arch) -> Self {
        let mut files = BTreeMap::new();
        files.insert("/root.toml".to_string(), snap.root_bytes.clone());
        files.insert("/root.toml.sig".to_string(), snap.root_sig.clone());
        let ip = index_path(snap.release, arch);
        files.insert(ip.clone(), snap.index_bytes.clone());
        files.insert(format!("{ip}.sig"), snap.index_sig.clone());
        for (path, bytes) in &snap.objects {
            files.insert(path.clone(), bytes.clone());
        }
        Self { files, requests: 0 }
    }
}

impl Fetcher for MockFetcher {
    fn fetch(&mut self, request: &FetchRequest) -> Result<Vec<u8>, FetchError> {
        assert_eq!(request.user_agent, USER_AGENT);
        assert!(request.path.starts_with('/'));
        assert!(!request.path.contains('?'));
        assert!(!request.path.contains(".."));
        self.requests += 1;
        let Some(data) = self.files.get(&request.path) else {
            return Err(FetchError::NotFound(request.path.clone()));
        };
        if data.len() as u64 > request.max_bytes {
            return Err(FetchError::SizeLimitExceeded {
                path: request.path.clone(),
                limit: request.max_bytes,
            });
        }
        Ok(data.clone())
    }
}

fn publish_snapshot(
    rng: &mut Rng,
    release_key: &Keypair,
    pkgar_key: &Keypair,
    arch: Arch,
    channel: Channel,
    release: ReleaseNumber,
    floor: ReleaseNumber,
    via: Option<ReleaseNumber>,
    serial: u64,
    signed: CalendarDate,
    fresh_days: u32,
    extra_releases: &BTreeMap<ReleaseNumber, ReleaseEntry>,
) -> PublishedSnapshot {
    assert!(release.date() <= signed);
    let pkg_count = 1 + rng.below(3);
    let mut packages = Vec::with_capacity(pkg_count);
    let mut objects = BTreeMap::new();
    let names = ["kernel", "base", "ferrion-cli"];
    for idx in 0..pkg_count {
        let name = names[idx];
        let payload_len = 32 + rng.below(256);
        let payload = rng.bytes(payload_len);
        let file_path = format!("usr/lib/{name}/data.bin");
        let (pkgar_bytes, head_hash) =
            build_uncompressed_pkgar(pkgar_key, &[(&file_path, 0o644, &payload)])
                .expect("build synthetic pkgar");
        let obj_hash = Blake3Hash::hash_bytes(&pkgar_bytes);
        let size = pkgar_bytes.len() as u64;
        objects.insert(object_path(&obj_hash), pkgar_bytes);
        packages.push(PackageEntry {
            name: name.to_string(),
            set: if idx < 2 {
                PackageSet::System
            } else {
                PackageSet::App
            },
            rev: release,
            head: head_hash,
            object: obj_hash,
            size,
            needs: Vec::new(),
            patch: Vec::new(),
        });
    }

    let index = IndexFile {
        format: FORMAT_VERSION,
        release,
        system: release,
        fixes: Vec::new(),
        package: packages,
    };
    let index_bytes = render_index_toml(&index).into_bytes();
    let index_sig = release_key.sign(&index_bytes).0.to_vec();
    let index_hash = Blake3Hash::hash_bytes(&index_bytes);

    let mut releases = extra_releases.clone();
    let mut idx_map = BTreeMap::new();
    idx_map.insert(arch, index_hash);
    releases.insert(
        release,
        ReleaseEntry {
            index: idx_map,
            floor,
            security: false,
            via,
            withdrawn: false,
            rollout: Vec::new(),
        },
    );

    let mut channels = Channels::default();
    channels.set(channel, release);

    let root = RootFile {
        format: FORMAT_VERSION,
        serial,
        signed,
        fresh_days,
        channels,
        releases,
        keys: None,
        mirrors: Vec::new(),
    };
    let root_bytes = render_root_toml(&root).into_bytes();
    let root_sig = release_key.sign(&root_bytes).0.to_vec();

    PublishedSnapshot {
        release,
        serial,
        root_bytes,
        root_sig,
        index_bytes,
        index_sig,
        index_hash,
        objects,
    }
}

struct SimEnv {
    rng: Rng,
    arch: Arch,
    channel: Channel,
    release_key: Keypair,
    attacker_key: Keypair,
    emergency_key: Keypair,
    pkgar_key: Keypair,
    clock: Clock,
    next_serial: u64,
    state: ClientState,
    history: Vec<PublishedSnapshot>,
}

impl SimEnv {
    fn new(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let arch = match rng.below(3) {
            0 => Arch::X86_64,
            1 => Arch::Aarch64,
            _ => Arch::Riscv64gc,
        };
        let channel = match rng.below(3) {
            0 => Channel::Stable,
            1 => Channel::Beta,
            _ => Channel::Dev,
        };
        let release_key = Keypair::from_seed(&rng.seed32());
        let attacker_key = Keypair::from_seed(&rng.seed32());
        let emergency_key = Keypair::from_seed(&rng.seed32());
        let pkgar_key = Keypair::from_seed(&rng.seed32());
        let mut clock = Clock { day: 0, counter: 0 };
        let base_date = clock.today();
        let rel0 = clock.next_release();
        let snap0 = publish_snapshot(
            &mut rng,
            &release_key,
            &pkgar_key,
            arch,
            channel,
            rel0,
            rel0,
            None,
            1,
            base_date,
            45,
            &BTreeMap::new(),
        );
        let mut state = ClientState {
            release_key: release_key.public,
            emergency_key: Some(emergency_key.public),
            arch,
            channel,
            installed_release: rel0,
            trusted_serial: 1,
            last_waypoint: None,
            installed_packages: BTreeMap::new(),
        };
        let mut fetcher = MockFetcher::from_snapshot(&snap0, arch);
        let outcome =
            apply_update(&mut state, base_date, &mut fetcher, true).expect("initial sync");
        assert!(outcome.stale_notice.is_none());
        assert_eq!(state.installed_release, rel0);
        assert_eq!(state.trusted_serial, 1);

        Self {
            rng,
            arch,
            channel,
            release_key,
            attacker_key,
            emergency_key,
            pkgar_key,
            clock: Clock { day: 1, counter: 1 },
            next_serial: 2,
            state,
            history: vec![snap0],
        }
    }

    fn alloc_release(&mut self) -> ReleaseNumber {
        self.clock.next_release()
    }

    fn alloc_serial(&mut self) -> u64 {
        let s = self.next_serial;
        self.next_serial += 1;
        s
    }

    fn advance_days(&mut self, days: i64) {
        self.clock.advance_days(days);
    }

    fn make_valid_next(&mut self, fresh_days: u32) -> PublishedSnapshot {
        let rel = self.alloc_release();
        let ser = self.alloc_serial();
        let signed = self.clock.today();
        publish_snapshot(
            &mut self.rng,
            &self.release_key,
            &self.pkgar_key,
            self.arch,
            self.channel,
            rel,
            self.state.installed_release,
            None,
            ser,
            signed,
            fresh_days,
            &BTreeMap::new(),
        )
    }

    fn run_case(&mut self, case_id: usize, counts: &mut Counts) -> Result<(), String> {
        let before = self.state.clone();
        match case_id {
            0 => {
                let mut snap = self.make_valid_next(45);
                if self.rng.below(2) == 0 {
                    snap.root_bytes = b"\xff\xfe not even valid utf8 or toml {{{{".to_vec();
                }
                snap.root_sig = self.attacker_key.sign(&snap.root_bytes).0.to_vec();
                let mut f = MockFetcher::from_snapshot(&snap, self.arch);
                let err = apply_update(&mut self.state, self.clock.today(), &mut f, true)
                    .expect_err("case 1 wrong key must fail");
                counts.total_requests += f.requests;
                if !matches!(
                    err,
                    UpdateError::InvalidSignature {
                        target: "root.toml"
                    }
                ) {
                    return Err(format!(
                        "case 1 expected InvalidSignature(root.toml), got {err:?}"
                    ));
                }
                if self.state != before {
                    return Err("case 1 mutated client state".to_string());
                }
                counts.case1_wrong_key += 1;
            }
            1 => {
                if !self
                    .history
                    .iter()
                    .any(|s| s.serial < self.state.trusted_serial)
                {
                    let good = self.make_valid_next(45);
                    let mut f = MockFetcher::from_snapshot(&good, self.arch);
                    apply_update(&mut self.state, self.clock.today(), &mut f, true)
                        .map_err(|e| format!("case 2 setup upgrade failed: {e}"))?;
                    counts.total_requests += f.requests;
                    self.history.push(good);
                    counts.valid_upgrades += 1;
                }
                let before_replay = self.state.clone();
                let old = self
                    .history
                    .iter()
                    .find(|s| s.serial < before_replay.trusted_serial)
                    .cloned()
                    .ok_or("case 2 missing older snapshot")?;
                let mut f = MockFetcher::from_snapshot(&old, self.arch);
                let err = apply_update(&mut self.state, self.clock.today(), &mut f, true)
                    .expect_err("case 2 replay must fail");
                counts.total_requests += f.requests;
                if !matches!(err, UpdateError::RollbackSerial { .. }) {
                    return Err(format!("case 2 expected RollbackSerial, got {err:?}"));
                }
                if self.state != before_replay {
                    return Err("case 2 mutated client state".to_string());
                }
                counts.case2_replay_serial += 1;
            }
            2 => {
                let good = self.make_valid_next(45);
                let mut f = MockFetcher::from_snapshot(&good, self.arch);
                apply_update(&mut self.state, self.clock.today(), &mut f, true)
                    .map_err(|e| format!("case 3 setup upgrade failed: {e}"))?;
                counts.total_requests += f.requests;
                self.history.push(good);
                counts.valid_upgrades += 1;

                let before_downgrade = self.state.clone();
                let older_rel = self.history[0].release;
                assert!(older_rel < before_downgrade.installed_release);
                let ser = self.alloc_serial();
                let mut channels = Channels::default();
                channels.set(self.channel, older_rel);
                let mut releases = BTreeMap::new();
                let mut idx_map = BTreeMap::new();
                idx_map.insert(self.arch, self.history[0].index_hash);
                releases.insert(
                    older_rel,
                    ReleaseEntry {
                        index: idx_map,
                        floor: older_rel,
                        security: false,
                        via: None,
                        withdrawn: false,
                        rollout: Vec::new(),
                    },
                );
                let root = RootFile {
                    format: FORMAT_VERSION,
                    serial: ser,
                    signed: self.clock.today(),
                    fresh_days: 45,
                    channels,
                    releases,
                    keys: None,
                    mirrors: Vec::new(),
                };
                let root_bytes = render_root_toml(&root).into_bytes();
                let root_sig = self.release_key.sign(&root_bytes).0.to_vec();
                let mut f = MockFetcher::from_snapshot(&self.history[0], self.arch);
                f.files.insert("/root.toml".to_string(), root_bytes);
                f.files.insert("/root.toml.sig".to_string(), root_sig);
                let err = apply_update(&mut self.state, self.clock.today(), &mut f, true)
                    .expect_err("case 3 downgrade must fail");
                counts.total_requests += f.requests;
                if !matches!(err, UpdateError::RollbackRelease { .. }) {
                    return Err(format!("case 3 expected RollbackRelease, got {err:?}"));
                }
                if self.state != before_downgrade {
                    return Err("case 3 mutated client state".to_string());
                }
                counts.case3_channel_downgrade += 1;
            }
            3 => {
                let mut snap = self.make_valid_next(45);
                if self.rng.below(2) == 0 {
                    snap.index_bytes = b"\xff bad toml {{{{".to_vec();
                } else {
                    snap.index_bytes.push(b'\n');
                }
                snap.index_sig = self.release_key.sign(&snap.index_bytes).0.to_vec();
                let mut f = MockFetcher::from_snapshot(&snap, self.arch);
                let err = apply_update(&mut self.state, self.clock.today(), &mut f, true)
                    .expect_err("case 4 index hash mismatch must fail");
                counts.total_requests += f.requests;
                if !matches!(err, UpdateError::IndexHashMismatch { .. }) {
                    return Err(format!("case 4 expected IndexHashMismatch, got {err:?}"));
                }
                if self.state != before {
                    return Err("case 4 mutated client state".to_string());
                }
                counts.case4_index_hash_mismatch += 1;
            }
            4 => {
                let mut snap = self.make_valid_next(45);
                let key = snap.objects.keys().next().cloned().ok_or("no object")?;
                let bytes = snap.objects.get_mut(&key).expect("object exists");
                let pos = self.rng.below(bytes.len());
                bytes[pos] ^= 0x01;
                let mut f = MockFetcher::from_snapshot(&snap, self.arch);
                let err = apply_update(&mut self.state, self.clock.today(), &mut f, true)
                    .expect_err("case 5 byte flip must fail");
                counts.total_requests += f.requests;
                if !matches!(err, UpdateError::ObjectHashMismatch { .. }) {
                    return Err(format!("case 5 expected ObjectHashMismatch, got {err:?}"));
                }
                if self.state != before {
                    return Err("case 5 mutated client state".to_string());
                }
                counts.case5_object_byte_flip += 1;
            }
            5 => {
                let mut snap = self.make_valid_next(45);
                let key = snap.objects.keys().next().cloned().ok_or("no object")?;
                let bytes = snap.objects.get_mut(&key).expect("object exists");
                let extra = 1 + self.rng.below(64);
                bytes.extend(std::iter::repeat_n(0x41u8, extra));
                let mut f = MockFetcher::from_snapshot(&snap, self.arch);
                let err = apply_update(&mut self.state, self.clock.today(), &mut f, true)
                    .expect_err("case 6 oversized object must fail");
                counts.total_requests += f.requests;
                if !matches!(err, UpdateError::ObjectTooLarge { .. }) {
                    return Err(format!("case 6 expected ObjectTooLarge, got {err:?}"));
                }
                if self.state != before {
                    return Err("case 6 mutated client state".to_string());
                }
                counts.case6_object_too_large += 1;
            }
            6 => {
                let variant = self.rng.below(4);
                let mut snap = self.make_valid_next(45);
                match variant {
                    0 => {
                        let mut text = String::from_utf8(snap.root_bytes).expect("utf8");
                        text = format!("unknown_field = \"bad\"\n{text}");
                        snap.root_bytes = text.into_bytes();
                        snap.root_sig = self.release_key.sign(&snap.root_bytes).0.to_vec();
                        let mut f = MockFetcher::from_snapshot(&snap, self.arch);
                        let err = apply_update(&mut self.state, self.clock.today(), &mut f, true)
                            .expect_err("case 7a unknown root key must fail");
                        counts.total_requests += f.requests;
                        if !matches!(
                            err,
                            UpdateError::UnknownKeyOrSyntax {
                                target: "root.toml",
                                ..
                            }
                        ) {
                            return Err(format!(
                                "case 7a expected UnknownKeyOrSyntax(root), got {err:?}"
                            ));
                        }
                    }
                    1 => {
                        let mut text = String::from_utf8(snap.index_bytes).expect("utf8");
                        text = format!("unexpected_key = 42\n{text}");
                        let new_idx = text.into_bytes();
                        let new_hash = Blake3Hash::hash_bytes(&new_idx);
                        let old_hex = snap.index_hash.to_string();
                        let new_hex = new_hash.to_string();
                        let root_str = String::from_utf8(snap.root_bytes)
                            .expect("utf8")
                            .replace(&old_hex, &new_hex);
                        snap.root_bytes = root_str.into_bytes();
                        snap.root_sig = self.release_key.sign(&snap.root_bytes).0.to_vec();
                        snap.index_bytes = new_idx;
                        snap.index_sig = self.release_key.sign(&snap.index_bytes).0.to_vec();
                        let mut f = MockFetcher::from_snapshot(&snap, self.arch);
                        let err = apply_update(&mut self.state, self.clock.today(), &mut f, true)
                            .expect_err("case 7b unknown index key must fail");
                        counts.total_requests += f.requests;
                        if !matches!(
                            err,
                            UpdateError::UnknownKeyOrSyntax {
                                target: "index.toml",
                                ..
                            }
                        ) {
                            return Err(format!(
                                "case 7b expected UnknownKeyOrSyntax(index), got {err:?}"
                            ));
                        }
                    }
                    2 => {
                        let text = String::from_utf8(snap.root_bytes).expect("utf8").replacen(
                            "format = 1",
                            "format = 2",
                            1,
                        );
                        snap.root_bytes = text.into_bytes();
                        snap.root_sig = self.release_key.sign(&snap.root_bytes).0.to_vec();
                        let mut f = MockFetcher::from_snapshot(&snap, self.arch);
                        let err = apply_update(&mut self.state, self.clock.today(), &mut f, true)
                            .expect_err("case 7c unknown root format must fail");
                        counts.total_requests += f.requests;
                        if !matches!(
                            err,
                            UpdateError::UnsupportedFormat {
                                target: "root.toml",
                                found: 2,
                                ..
                            }
                        ) {
                            return Err(format!(
                                "case 7c expected UnsupportedFormat(root, 2), got {err:?}"
                            ));
                        }
                    }
                    _ => {
                        let text = String::from_utf8(snap.index_bytes).expect("utf8").replacen(
                            "format = 1",
                            "format = 2",
                            1,
                        );
                        let new_idx = text.into_bytes();
                        let new_hash = Blake3Hash::hash_bytes(&new_idx);
                        let root_str = String::from_utf8(snap.root_bytes)
                            .expect("utf8")
                            .replace(&snap.index_hash.to_string(), &new_hash.to_string());
                        snap.root_bytes = root_str.into_bytes();
                        snap.root_sig = self.release_key.sign(&snap.root_bytes).0.to_vec();
                        snap.index_bytes = new_idx;
                        snap.index_sig = self.release_key.sign(&snap.index_bytes).0.to_vec();
                        let mut f = MockFetcher::from_snapshot(&snap, self.arch);
                        let err = apply_update(&mut self.state, self.clock.today(), &mut f, true)
                            .expect_err("case 7d unknown index format must fail");
                        counts.total_requests += f.requests;
                        if !matches!(
                            err,
                            UpdateError::UnsupportedFormat {
                                target: "index.toml",
                                found: 2,
                                ..
                            }
                        ) {
                            return Err(format!(
                                "case 7d expected UnsupportedFormat(index, 2), got {err:?}"
                            ));
                        }
                    }
                }
                if self.state != before {
                    return Err("case 7 mutated client state".to_string());
                }
                counts.case7_unknown_key_or_format += 1;
            }
            7 => {
                let fresh_days = 45u32;
                let stale_snap = self.make_valid_next(fresh_days);
                let overdue = 1 + (self.rng.below(90) as i64);
                self.advance_days(fresh_days as i64 + overdue);
                let mut f1 = MockFetcher::from_snapshot(&stale_snap, self.arch);
                let outcome1 = apply_update(&mut self.state, self.clock.today(), &mut f1, true)
                    .map_err(|e| format!("case 8 stale root must be accepted, got {e}"))?;
                counts.total_requests += f1.requests;
                let Some(notice) = outcome1.stale_notice else {
                    return Err("case 8 expected stale_notice to be Some".to_string());
                };
                if notice.age_days != fresh_days as i64 + overdue {
                    return Err(format!(
                        "case 8 wrong age_days: got {}, expected {}",
                        notice.age_days,
                        fresh_days as i64 + overdue
                    ));
                }
                self.history.push(stale_snap);

                let fresh_snap = self.make_valid_next(fresh_days);
                let mut f2 = MockFetcher::from_snapshot(&fresh_snap, self.arch);
                let outcome2 = apply_update(&mut self.state, self.clock.today(), &mut f2, true)
                    .map_err(|e| format!("case 8 replacement root failed: {e}"))?;
                counts.total_requests += f2.requests;
                if outcome2.stale_notice.is_some() {
                    return Err("case 8 replacement root unexpectedly stale".to_string());
                }
                if self.state.installed_release != fresh_snap.release
                    || self.state.trusted_serial != fresh_snap.serial
                {
                    return Err("case 8 replacement did not advance state".to_string());
                }
                self.history.push(fresh_snap);
                counts.case8_stale_then_fresh += 1;
            }
            8 => {
                let snap = self.make_valid_next(45);
                let mut f = MockFetcher::from_snapshot(&snap, self.arch);
                f.files
                    .insert("/root.toml".to_string(), vec![b'a'; MAX_ROOT_BYTES + 1]);
                let err = apply_update(&mut self.state, self.clock.today(), &mut f, true)
                    .expect_err("oversized root must fail");
                counts.total_requests += f.requests;
                if !matches!(err, UpdateError::RootTooLarge { .. }) {
                    return Err(format!("expected RootTooLarge, got {err:?}"));
                }
                if self.state != before {
                    return Err("oversized root mutated state".to_string());
                }
                counts.extra_root_too_large += 1;
            }
            9 => {
                let mut snap = self.make_valid_next(45);
                snap.index_sig = self.attacker_key.sign(&snap.index_bytes).0.to_vec();
                let mut f = MockFetcher::from_snapshot(&snap, self.arch);
                let err = apply_update(&mut self.state, self.clock.today(), &mut f, true)
                    .expect_err("bad index sig must fail");
                counts.total_requests += f.requests;
                if !matches!(
                    err,
                    UpdateError::InvalidSignature {
                        target: "index.toml"
                    }
                ) {
                    return Err(format!(
                        "expected InvalidSignature(index.toml), got {err:?}"
                    ));
                }
                if self.state != before {
                    return Err("bad index sig mutated state".to_string());
                }
                counts.extra_bad_index_sig += 1;
            }
            10 => {
                let mut snap = self.make_valid_next(45);
                let new_idx = self.history[0].index_bytes.clone();
                let new_hash = Blake3Hash::hash_bytes(&new_idx);
                let root_str = String::from_utf8(snap.root_bytes)
                    .expect("utf8")
                    .replace(&snap.index_hash.to_string(), &new_hash.to_string());
                snap.root_bytes = root_str.into_bytes();
                snap.root_sig = self.release_key.sign(&snap.root_bytes).0.to_vec();
                snap.index_bytes = new_idx;
                snap.index_sig = self.release_key.sign(&snap.index_bytes).0.to_vec();
                let mut f = MockFetcher::from_snapshot(&snap, self.arch);
                let err = apply_update(&mut self.state, self.clock.today(), &mut f, true)
                    .expect_err("index release mismatch must fail");
                counts.total_requests += f.requests;
                if !matches!(err, UpdateError::IndexReleaseMismatch { .. }) {
                    return Err(format!("expected IndexReleaseMismatch, got {err:?}"));
                }
                if self.state != before {
                    return Err("index release mismatch mutated state".to_string());
                }
                counts.extra_index_release_mismatch += 1;
            }
            11 => {
                let mut snap = self.make_valid_next(45);
                let key = snap.objects.keys().next().cloned().ok_or("no object")?;
                let bytes = snap.objects.get_mut(&key).expect("object exists");
                bytes.pop();
                let mut f = MockFetcher::from_snapshot(&snap, self.arch);
                let err = apply_update(&mut self.state, self.clock.today(), &mut f, true)
                    .expect_err("truncated object must fail");
                counts.total_requests += f.requests;
                if !matches!(err, UpdateError::ObjectSizeMismatch { .. }) {
                    return Err(format!("expected ObjectSizeMismatch, got {err:?}"));
                }
                if self.state != before {
                    return Err("truncated object mutated state".to_string());
                }
                counts.extra_object_truncated += 1;
            }
            12 => {
                let rel = self.alloc_release();
                let ser = self.alloc_serial();
                let bogus_bytes = b"not a valid pkgar archive at all".to_vec();
                let obj_hash = Blake3Hash::hash_bytes(&bogus_bytes);
                let fake_head = Blake3Hash::hash_bytes(b"fake head");
                let index = IndexFile {
                    format: FORMAT_VERSION,
                    release: rel,
                    system: rel,
                    fixes: Vec::new(),
                    package: vec![PackageEntry {
                        name: "broken".to_string(),
                        set: PackageSet::System,
                        rev: rel,
                        head: fake_head,
                        object: obj_hash,
                        size: bogus_bytes.len() as u64,
                        needs: Vec::new(),
                        patch: Vec::new(),
                    }],
                };
                let idx_bytes = render_index_toml(&index).into_bytes();
                let idx_sig = self.release_key.sign(&idx_bytes).0.to_vec();
                let idx_hash = Blake3Hash::hash_bytes(&idx_bytes);
                let mut channels = Channels::default();
                channels.set(self.channel, rel);
                let mut releases = BTreeMap::new();
                let mut idx_map = BTreeMap::new();
                idx_map.insert(self.arch, idx_hash);
                releases.insert(
                    rel,
                    ReleaseEntry {
                        index: idx_map,
                        floor: self.state.installed_release,
                        security: false,
                        via: None,
                        withdrawn: false,
                        rollout: Vec::new(),
                    },
                );
                let root = RootFile {
                    format: FORMAT_VERSION,
                    serial: ser,
                    signed: self.clock.today(),
                    fresh_days: 45,
                    channels,
                    releases,
                    keys: None,
                    mirrors: Vec::new(),
                };
                let root_bytes = render_root_toml(&root).into_bytes();
                let root_sig = self.release_key.sign(&root_bytes).0.to_vec();
                let mut objects = BTreeMap::new();
                objects.insert(object_path(&obj_hash), bogus_bytes);
                let snap = PublishedSnapshot {
                    release: rel,
                    serial: ser,
                    root_bytes,
                    root_sig,
                    index_bytes: idx_bytes,
                    index_sig: idx_sig,
                    index_hash: idx_hash,
                    objects,
                };
                let mut f = MockFetcher::from_snapshot(&snap, self.arch);
                let err = apply_update(&mut self.state, self.clock.today(), &mut f, true)
                    .expect_err("corrupt pkgar must fail");
                counts.total_requests += f.requests;
                if !matches!(err, UpdateError::PkgarInvalid { .. }) {
                    return Err(format!("expected PkgarInvalid, got {err:?}"));
                }
                if self.state != before {
                    return Err("corrupt pkgar mutated state".to_string());
                }
                counts.extra_pkgar_corrupt += 1;
            }
            13 => {
                let wp_rel = self.alloc_release();
                let wp_ser = self.alloc_serial();
                let wp_snap = publish_snapshot(
                    &mut self.rng,
                    &self.release_key,
                    &self.pkgar_key,
                    self.arch,
                    self.channel,
                    wp_rel,
                    self.state.installed_release,
                    None,
                    wp_ser,
                    self.clock.today(),
                    45,
                    &BTreeMap::new(),
                );
                let mut extra = BTreeMap::new();
                let mut wp_map = BTreeMap::new();
                wp_map.insert(self.arch, wp_snap.index_hash);
                extra.insert(
                    wp_rel,
                    ReleaseEntry {
                        index: wp_map,
                        floor: self.state.installed_release,
                        security: false,
                        via: None,
                        withdrawn: false,
                        rollout: Vec::new(),
                    },
                );
                let target_rel = self.alloc_release();
                let target_ser = self.alloc_serial();
                let target_snap = publish_snapshot(
                    &mut self.rng,
                    &self.release_key,
                    &self.pkgar_key,
                    self.arch,
                    self.channel,
                    target_rel,
                    wp_rel,
                    Some(wp_rel),
                    target_ser,
                    self.clock.today(),
                    45,
                    &extra,
                );
                let mut f = MockFetcher::from_snapshot(&target_snap, self.arch);
                let wp_idx_path = index_path(wp_rel, self.arch);
                f.files
                    .insert(wp_idx_path.clone(), wp_snap.index_bytes.clone());
                f.files
                    .insert(format!("{wp_idx_path}.sig"), wp_snap.index_sig.clone());
                for (p, b) in &wp_snap.objects {
                    f.files.insert(p.clone(), b.clone());
                }
                let out1 = apply_update(&mut self.state, self.clock.today(), &mut f, true)
                    .map_err(|e| format!("waypoint step 1 failed: {e}"))?;
                if !out1.via_waypoint || out1.target_release != wp_rel {
                    return Err(format!(
                        "expected waypoint {wp_rel}, got {:?}",
                        out1.target_release
                    ));
                }
                let out2 = apply_update(&mut self.state, self.clock.today(), &mut f, true)
                    .map_err(|e| format!("waypoint step 2 failed: {e}"))?;
                counts.total_requests += f.requests;
                if out2.via_waypoint || out2.target_release != target_rel {
                    return Err(format!(
                        "expected final {target_rel}, got {:?}",
                        out2.target_release
                    ));
                }
                self.history.push(wp_snap);
                self.history.push(target_snap);
                counts.extra_waypoint += 1;
            }
            14 => {
                let mut snap = self.make_valid_next(45);
                let text = String::from_utf8(snap.root_bytes)
                    .expect("utf8")
                    .replace("security = false", "security = false\nwithdrawn = true");
                snap.root_bytes = text.into_bytes();
                snap.root_sig = self.release_key.sign(&snap.root_bytes).0.to_vec();
                let mut f = MockFetcher::from_snapshot(&snap, self.arch);
                let err = apply_update(&mut self.state, self.clock.today(), &mut f, true)
                    .expect_err("withdrawn release must fail");
                counts.total_requests += f.requests;
                if !matches!(err, UpdateError::ReleaseWithdrawn { .. }) {
                    return Err(format!("expected ReleaseWithdrawn, got {err:?}"));
                }
                if self.state != before {
                    return Err("withdrawn release mutated state".to_string());
                }
                counts.extra_withdrawn += 1;
            }
            15 => {
                let new_key = Keypair::from_seed(&self.rng.seed32());
                let rot_ser = self.alloc_serial();
                let rot = KeyRotation {
                    format: FORMAT_VERSION,
                    serial: rot_ser,
                    new_key: new_key.public,
                };
                let rot_bytes = format!(
                    "format = {}\nserial = {}\nnew_key = \"{}\"\n",
                    rot.format, rot.serial, rot.new_key
                )
                .into_bytes();
                let forged_sig = self.attacker_key.sign(&rot_bytes).0;
                let rel = self.alloc_release();
                let next_snap = publish_snapshot(
                    &mut self.rng,
                    &new_key,
                    &self.pkgar_key,
                    self.arch,
                    self.channel,
                    rel,
                    self.state.installed_release,
                    None,
                    rot_ser,
                    self.clock.today(),
                    45,
                    &BTreeMap::new(),
                );
                let err = rotate_release_key(
                    &self.state,
                    &rot_bytes,
                    &forged_sig,
                    &next_snap.root_bytes,
                    &next_snap.root_sig,
                )
                .expect_err("forged rotation must fail");
                if !matches!(
                    err,
                    UpdateError::InvalidSignature {
                        target: "rotation.toml"
                    }
                ) {
                    return Err(format!(
                        "expected forged rotation InvalidSignature, got {err:?}"
                    ));
                }
                let good_sig = self.emergency_key.sign(&rot_bytes).0;
                let (_, _, rotated_state) = rotate_release_key(
                    &self.state,
                    &rot_bytes,
                    &good_sig,
                    &next_snap.root_bytes,
                    &next_snap.root_sig,
                )
                .map_err(|e| format!("valid key rotation failed: {e}"))?;
                self.state = rotated_state;
                self.release_key = new_key;
                let revoked_snap = self.history[0].clone();
                let mut f_rev = MockFetcher::from_snapshot(&revoked_snap, self.arch);
                let rev_err = apply_update(&mut self.state, self.clock.today(), &mut f_rev, true)
                    .expect_err("revoked key snapshot must fail");
                counts.total_requests += f_rev.requests;
                if !matches!(
                    rev_err,
                    UpdateError::InvalidSignature {
                        target: "root.toml"
                    }
                ) {
                    return Err(format!(
                        "expected revoked key InvalidSignature, got {rev_err:?}"
                    ));
                }
                let mut f = MockFetcher::from_snapshot(&next_snap, self.arch);
                apply_update(&mut self.state, self.clock.today(), &mut f, true)
                    .map_err(|e| format!("post-rotation sync failed: {e}"))?;
                counts.total_requests += f.requests;
                self.history = vec![next_snap];
                counts.extra_key_rotation += 1;
            }
            _ => {
                let good = self.make_valid_next(45);
                let mut f = MockFetcher::from_snapshot(&good, self.arch);
                let out = apply_update(&mut self.state, self.clock.today(), &mut f, true)
                    .map_err(|e| format!("valid upgrade failed: {e}"))?;
                counts.total_requests += f.requests;
                if out.stale_notice.is_some() {
                    return Err("fresh valid upgrade returned stale_notice".to_string());
                }
                if self.state.installed_release != good.release
                    || self.state.trusted_serial != good.serial
                {
                    return Err("valid upgrade did not advance state".to_string());
                }
                self.history.push(good);
                counts.valid_upgrades += 1;
            }
        }
        Ok(())
    }
}

fn run_seed(seed: u64, steps: usize, counts: &mut Counts) -> Result<(), String> {
    let mut env = SimEnv::new(seed);
    for mandatory in 0..8 {
        env.run_case(mandatory, counts)
            .map_err(|e| format!("seed {seed} mandatory case {}: {e}", mandatory + 1))?;
    }
    for step in 0..steps {
        let case_id = env.rng.below(18);
        env.run_case(case_id, counts)
            .map_err(|e| format!("seed {seed} step {step} case {case_id}: {e}"))?;
    }
    Ok(())
}

fn load_regressions() -> Vec<u64> {
    let Ok(text) = fs::read_to_string("regressions.txt") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(seed) = trimmed.parse::<u64>() {
            out.push(seed);
        }
    }
    out
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() < 2 || args.len() > 3 {
        eprintln!("usage: ferrion-update-sim SEEDS STEPS [FIRST_SEED]");
        return ExitCode::from(2);
    }
    let seeds: usize = match args[0].parse() {
        Ok(n) if n <= MAX_SEEDS => n,
        _ => {
            eprintln!("invalid SEEDS (must be 0..={MAX_SEEDS})");
            return ExitCode::from(2);
        }
    };
    let steps: usize = match args[1].parse() {
        Ok(n) if n <= MAX_STEPS => n,
        _ => {
            eprintln!("invalid STEPS (must be 0..={MAX_STEPS})");
            return ExitCode::from(2);
        }
    };
    let first_seed: u64 = match args.get(2) {
        Some(s) => match s.parse() {
            Ok(n) => n,
            Err(_) => {
                eprintln!("invalid FIRST_SEED");
                return ExitCode::from(2);
            }
        },
        None => 0,
    };

    let mut counts = Counts::default();
    let regressions = load_regressions();
    for seed in &regressions {
        if let Err(err) = run_seed(*seed, steps, &mut counts) {
            eprintln!("FAIL regression {err}");
            return ExitCode::FAILURE;
        }
    }
    for offset in 0..seeds {
        let seed = first_seed.wrapping_add(offset as u64);
        if let Err(err) = run_seed(seed, steps, &mut counts) {
            eprintln!("FAIL {err}");
            return ExitCode::FAILURE;
        }
    }

    println!(
        "PASS ferrion-update-sim: {} regressions + {} seeds x {} steps | \
         c1_wrong_key={} c2_replay={} c3_downgrade={} c4_idx_hash={} \
         c5_obj_flip={} c6_obj_oversize={} c7_unknown={} c8_stale_fresh={} | \
         extra(root_big={} idx_sig={} idx_rel={} obj_trunc={} pkgar={} waypoint={} withdrawn={} rot={}) \
         valid={} requests={}",
        regressions.len(),
        seeds,
        steps,
        counts.case1_wrong_key,
        counts.case2_replay_serial,
        counts.case3_channel_downgrade,
        counts.case4_index_hash_mismatch,
        counts.case5_object_byte_flip,
        counts.case6_object_too_large,
        counts.case7_unknown_key_or_format,
        counts.case8_stale_then_fresh,
        counts.extra_root_too_large,
        counts.extra_bad_index_sig,
        counts.extra_index_release_mismatch,
        counts.extra_object_truncated,
        counts.extra_pkgar_corrupt,
        counts.extra_waypoint,
        counts.extra_withdrawn,
        counts.extra_key_rotation,
        counts.valid_upgrades,
        counts.total_requests,
    );
    ExitCode::SUCCESS
}

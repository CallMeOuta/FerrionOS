use crate::disk::CRASH_KINDS;
use crate::engine::Engine;
use crate::model::{Kind, NAME_MAX, NodeId, Path, Time};
use crate::op::{Op, PATTERN_KINDS, Pattern, Step, Target};
use crate::rng::Rng;

pub const RECORD_BYTES: u64 = 128 * 1024;
pub const INLINE_BYTES: u64 = 3968;
pub const LEVEL1_OFFSET: u64 = 128 * RECORD_BYTES;
pub const LEVEL2_OFFSET: u64 = (128 + 64 * 256) * RECORD_BYTES;
pub const MAX_WRITE: u64 = 600_000;
pub const MAX_CRASH_AFTER: u64 = 40;
pub const MAX_READ_ERROR_AFTER: u64 = 40;
pub const KEY_SLOT_CHOICES: u64 = 4;
pub const REFORMAT_EVERY: u64 = 2500;
pub const MAX_REFORMAT_CRASH_AFTER: u64 = 300;

const SHORT_NAMES: [&str; 12] = ["a", "b", "c", "d", "e", "f", "g", "h", "x", "y", "z", "q"];

fn name(rng: &mut Rng, wide: bool) -> String {
    if rng.chance(1, 200) {
        let len = rng.range(NAME_MAX as u64 - 2, NAME_MAX as u64 + 1) as usize;
        return "n".repeat(len);
    }
    if wide {
        let len = rng.range(80, 240) as usize;
        let mut text = "w".repeat(len);
        text.push_str(&format!("{:04}", rng.below(2000)));
        return text;
    }
    let base = rng.pick(&SHORT_NAMES);
    if rng.chance(1, 2) {
        base.to_string()
    } else {
        format!("{base}{}", rng.below(4))
    }
}

fn advance(rng: &mut Rng, now: Time) -> Time {
    let mut nanos = u64::from(now.1) + rng.below(1_000_000_000);
    let mut seconds = now.0 + rng.below(3);
    if nanos >= 1_000_000_000 {
        nanos -= 1_000_000_000;
        seconds += 1;
    }
    (seconds, nanos as u32)
}

fn random_time(rng: &mut Rng, now: Time) -> Time {
    (
        now.0.saturating_sub(100_000_000) + rng.below(101_000_000),
        rng.below(1_000_000_000) as u32,
    )
}

struct View<'a> {
    engine: &'a Engine,
    all: Vec<(Path, NodeId)>,
    dirs: Vec<Path>,
    files: Vec<Path>,
}

impl<'a> View<'a> {
    fn new(engine: &'a Engine) -> View<'a> {
        let all = engine.paths();
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for (path, id) in all.iter() {
            match engine.model.node(*id).kind {
                Kind::Dir => dirs.push(path.clone()),
                Kind::File => files.push(path.clone()),
            }
        }
        View {
            engine,
            all,
            dirs,
            files,
        }
    }

    fn any_path(&self, rng: &mut Rng) -> Option<Path> {
        let candidates: Vec<&Path> = self.all.iter().map(|(path, _)| path).filter(|path| !path.is_empty()).collect();
        if candidates.is_empty() {
            None
        } else {
            Some((*rng.pick(&candidates)).clone())
        }
    }

    fn file(&self, rng: &mut Rng) -> Option<Path> {
        if self.files.is_empty() {
            None
        } else {
            Some(rng.pick(&self.files).clone())
        }
    }

    fn new_path(&self, rng: &mut Rng) -> Path {
        let mut path = rng.pick(&self.dirs).clone();
        if rng.chance(1, 30) {
            path.push(name(rng, false));
        }
        path.push(name(rng, self.engine.profile.wide_names));
        path
    }

    fn subdir_of(&self, rng: &mut Rng, path: &Path) -> Option<Path> {
        let inside: Vec<&Path> = self
            .dirs
            .iter()
            .filter(|dir| dir.len() >= path.len() && dir[..path.len()] == path[..])
            .collect();
        if inside.is_empty() {
            None
        } else {
            Some((*rng.pick(&inside)).clone())
        }
    }
}

fn write_op(rng: &mut Rng, target: Target, size: u64, budget: u64) -> Op {
    let offset = match rng.below(100) {
        0..50 => size,
        50..72 => rng.below(size + 1),
        72..84 => size + rng.below(300_000),
        84..92 => {
            let record = rng.below(8) * RECORD_BYTES;
            (record + rng.below(8)).saturating_sub(4)
        }
        92..97 => INLINE_BYTES - 8 + rng.below(16),
        97..99 => LEVEL1_OFFSET + rng.below(4 * RECORD_BYTES),
        _ => LEVEL2_OFFSET + rng.below(2 * RECORD_BYTES),
    };
    let mut len = match rng.below(100) {
        0..55 => rng.range(1, 4096),
        55..78 => rng.range(4096, 140_000),
        78..90 => rng.range(140_000, MAX_WRITE),
        90..95 => 0,
        _ => rng.range(1, 64),
    };
    len = len.min(budget);
    Op::Write {
        target,
        offset,
        len: len as u32,
        pattern: Pattern {
            kind: rng.below(u64::from(PATTERN_KINDS)) as u8,
            seed: rng.next_u64(),
        },
    }
}

fn truncate_len(rng: &mut Rng, size: u64) -> u64 {
    match rng.below(100) {
        0..15 => 0,
        15..50 => rng.below(size + 1),
        50..72 => size + rng.below(300_000),
        72..84 => rng.below(8) * RECORD_BYTES + rng.below(3),
        84..96 => INLINE_BYTES - 4 + rng.below(8),
        _ => LEVEL1_OFFSET + rng.below(RECORD_BYTES),
    }
}

pub fn generate(engine: &Engine, rng: &mut Rng, out: &mut Vec<Step>) {
    let profile = &engine.profile;
    let now = engine.now;
    if rng.chance(1, REFORMAT_EVERY) {
        if profile.crash_per_mille != 0 && rng.chance(1, 2) {
            out.push(Step {
                time: now,
                op: Op::Crash {
                    after: rng.below(MAX_REFORMAT_CRASH_AFTER) as u32,
                    kind: *rng.pick(&CRASH_KINDS),
                    salt: rng.next_u64(),
                },
            });
        }
        out.push(Step {
            time: advance(rng, now),
            op: Op::Reformat,
        });
        return;
    }
    if rng.chance(profile.crash_per_mille, 1000) {
        out.push(Step {
            time: now,
            op: Op::Crash {
                after: rng.below(MAX_CRASH_AFTER) as u32,
                kind: *rng.pick(&CRASH_KINDS),
                salt: rng.next_u64(),
            },
        });
    }
    if rng.chance(profile.read_error_per_mille, 1000) {
        out.push(Step {
            time: now,
            op: Op::ReadError {
                nth: rng.range(1, MAX_READ_ERROR_AFTER) as u32,
            },
        });
    }
    if rng.chance(profile.enumerate_per_mille, 1000) {
        out.push(Step {
            time: now,
            op: Op::Enumerate {
                salt: rng.next_u64(),
            },
        });
    }
    let view = View::new(engine);
    let time = advance(rng, now);
    let model = &engine.model;
    let budget_total = profile.disk_bytes / 100 * profile.data_percent;
    let used = model.bytes();
    let budget = budget_total.saturating_sub(used);
    let full = used > budget_total * 3 / 4;
    let crowded = model.nodes.len() >= profile.max_nodes;
    let open = engine.open_handles();
    let op = loop {
        let choice = rng.below(1000);
        let candidate = match choice {
            0..140 if !crowded => Some(Op::Create(view.new_path(rng))),
            140..190 if !crowded => Some(Op::Mkdir(view.new_path(rng))),
            190..410 if !full => view.file(rng).map(|path| {
                let size = model.node(model.lookup(&path).unwrap()).size;
                write_op(rng, Target::Path(path), size, budget)
            }),
            410..480 => view.file(rng).map(|path| {
                let size = model.node(model.lookup(&path).unwrap()).size;
                let len = if full { rng.below(size + 1) } else { truncate_len(rng, size) };
                Op::Truncate {
                    target: Target::Path(path),
                    len,
                }
            }),
            480..560 => view.any_path(rng).map(|from| {
                let is_dir = model.node(model.lookup(&from).unwrap()).kind == Kind::Dir;
                let to = if is_dir && rng.chance(1, 10) {
                    let mut inside = view.subdir_of(rng, &from).unwrap_or_else(|| from.clone());
                    inside.push(name(rng, false));
                    inside
                } else if rng.chance(1, 3) {
                    view.any_path(rng).unwrap_or_else(|| view.new_path(rng))
                } else {
                    view.new_path(rng)
                };
                Op::Rename {
                    from,
                    to,
                    noreplace: rng.chance(1, 5),
                }
            }),
            560..600 if !crowded => view.file(rng).map(|from| Op::Link {
                from,
                to: if rng.chance(1, 5) {
                    view.any_path(rng).unwrap_or_else(|| view.new_path(rng))
                } else {
                    view.new_path(rng)
                },
            }),
            600..690 => {
                let pick = if full || rng.chance(9, 10) {
                    view.file(rng)
                } else {
                    view.any_path(rng)
                };
                pick.map(Op::Unlink)
            }
            690..740 => {
                let empty: Vec<&Path> = view
                    .dirs
                    .iter()
                    .filter(|dir| {
                        !dir.is_empty() && model.node(model.lookup(dir).unwrap()).entries.is_empty()
                    })
                    .collect();
                if !empty.is_empty() && rng.chance(7, 10) {
                    Some(Op::Rmdir((*rng.pick(&empty)).clone()))
                } else {
                    view.any_path(rng).map(Op::Rmdir)
                }
            }
            740..780 => {
                if !open.is_empty() && rng.chance(1, 3) {
                    Some(Op::Fsync(Target::Handle(*rng.pick(&open))))
                } else if rng.chance(1, 4) {
                    Some(Op::Fsync(Target::Path(Vec::new())))
                } else {
                    view.any_path(rng).map(|path| Op::Fsync(Target::Path(path)))
                }
            }
            780..810 => {
                let target = if !open.is_empty() && rng.chance(1, 3) {
                    Some(Target::Handle(*rng.pick(&open)))
                } else {
                    view.any_path(rng).map(Target::Path)
                };
                target.map(|target| Op::SetTimes {
                    target,
                    atime: random_time(rng, time),
                    mtime: random_time(rng, time),
                })
            }
            810..850 => engine.free_handle().and_then(|handle| {
                let path = if rng.chance(1, 8) {
                    rng.pick(&view.dirs).clone()
                } else {
                    view.file(rng)?
                };
                Some(Op::Open { path, handle })
            }),
            850..885 if !open.is_empty() => Some(Op::Close(*rng.pick(&open))),
            885..935 if !open.is_empty() => {
                let handle = *rng.pick(&open);
                let id = engine.handle_node(handle).unwrap();
                let node = model.node(id);
                if node.kind != Kind::File {
                    None
                } else if rng.chance(2, 3) && !full {
                    Some(write_op(rng, Target::Handle(handle), node.size, budget))
                } else {
                    let len = truncate_len(rng, node.size);
                    Some(Op::Truncate {
                        target: Target::Handle(handle),
                        len: if full { len.min(node.size) } else { len },
                    })
                }
            }
            935..940 if profile.encrypted => {
                let slot = rng.below(KEY_SLOT_CHOICES) as u8;
                if rng.chance(1, 3) {
                    Some(Op::ClearKeySlot(slot))
                } else {
                    Some(Op::SetKeySlot {
                        slot,
                        password: format!("pw{}", rng.below(1000)),
                    })
                }
            }
            940..1000 if rng.chance(profile.remount_per_mille * 16, 1000) => Some(Op::Remount),
            _ => None,
        };
        if let Some(op) = candidate {
            break op;
        }
    };
    out.push(Step { time, op });
}

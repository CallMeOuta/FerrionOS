use std::collections::BTreeMap;

use redoxfs::{
    ALLOC_GC_THRESHOLD, ALLOC_LIST_ENTRIES, Access, Barriers, Disk, FileSystem, HEADER_RING,
    Header, KeySlot, Node, Salt, Transaction, TreeData, TreePtr,
};
use syscall::error::{
    EACCES, EEXIST, EINVAL, EIO, EISDIR, ENOENT, ENOSPC, ENOTDIR, ENOTEMPTY, Error, Result,
};

use crate::check::{self, Snapshot};
use crate::disk::{self, CrashKind, Durability, Guard, SimDisk, View};
use crate::model::{Errno, Kind, Model, NodeId, Path, Time};
use crate::op::{MAX_HANDLES, Op, Step, Target};
use crate::rng::{Rng, seed_entropy};

pub const START_TIME: Time = (1_700_000_000, 0);
pub const ENCRYPTED_MOUNT_EVERY: u64 = 64;
pub const MOUNT_EVERY: u64 = 8;
pub const ENOSPC_SLACK_BLOCKS: u64 = 512;
pub const SQUASH_SLACK_ENTRIES: usize = 320;
pub const FIRST_PASSWORD: &str = "pw0";
const RECORD_LEVEL: usize = 5;

#[derive(Clone, Debug)]
pub struct Profile {
    pub durability: Durability,
    pub disk_bytes: u64,
    pub encrypted: bool,
    pub wide_names: bool,
    pub max_nodes: usize,
    pub crash_per_mille: u64,
    pub read_error_per_mille: u64,
    pub enumerate_per_mille: u64,
    pub remount_per_mille: u64,
    pub full_check_every: u64,
    pub data_percent: u64,
    pub barriers: bool,
}

impl Profile {
    pub fn for_seed(seed: u64, barriers: bool) -> Profile {
        let mut rng = Rng::new(seed ^ 0x9f0f_11e5_0000_0001);
        let durability = if rng.chance(1, 2) && barriers {
            Durability::Ordered
        } else {
            Durability::Volatile
        };
        let disk_bytes = *rng.pick(&[4u64, 8, 8, 16, 32]) << 20;
        let encrypted = rng.chance(1, 16);
        let wide_names = rng.chance(1, 8);
        let data_percent = if rng.chance(1, 4) { 92 } else { 50 };
        Profile {
            durability,
            disk_bytes,
            encrypted,
            wide_names,
            max_nodes: if wide_names { 900 } else { 160 },
            crash_per_mille: match (barriers, encrypted) {
                (false, _) => 0,
                (true, true) => 2,
                (true, false) => 8,
            },
            read_error_per_mille: 10,
            enumerate_per_mille: if encrypted || !barriers { 0 } else { 25 },
            remount_per_mille: if barriers { 2 } else { 8 },
            full_check_every: 256,
            data_percent,
            barriers,
        }
    }
}

#[derive(Default, Clone, Debug)]
pub struct Stats {
    pub ops: u64,
    pub failed_ops: u64,
    pub enospc: u64,
    pub crashes: u64,
    pub crash_mounts: u64,
    pub boundaries: u64,
    pub read_errors: u64,
    pub remounts: u64,
    pub checks: u64,
    pub writes: u64,
    pub flushes: u64,
    pub layout_differences: u64,
    pub durable_mounts: u64,
    pub unformatted: u64,
}

impl Stats {
    pub fn add(&mut self, other: &Stats) {
        self.ops += other.ops;
        self.failed_ops += other.failed_ops;
        self.enospc += other.enospc;
        self.crashes += other.crashes;
        self.crash_mounts += other.crash_mounts;
        self.boundaries += other.boundaries;
        self.read_errors += other.read_errors;
        self.remounts += other.remounts;
        self.checks += other.checks;
        self.writes += other.writes;
        self.flushes += other.flushes;
        self.layout_differences += other.layout_differences;
        self.durable_mounts += other.durable_mounts;
        self.unformatted += other.unformatted;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Free,
    Open { node: NodeId, fs: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mounted {
    Fresh,
    CleanRemount,
    AfterCrash,
    Reformatted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DurableCheck {
    PerOp { mount_every: u64 },
    AfterFlush { media_current: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CrashPlan {
    after: u32,
    kind: CrashKind,
    salt: u64,
}

#[derive(Default)]
struct Armed {
    crash: Option<CrashPlan>,
    read_error: Option<u32>,
    enumerate: Option<u64>,
}

enum After {
    Ran {
        fs_ok: bool,
        actual: std::result::Result<Option<u32>, Errno>,
        read_error_fired: bool,
        remaining_crash: Option<CrashPlan>,
    },
    PowerLost {
        lost: disk::Lost,
        plan: CrashPlan,
        before: Model,
        read_error_fired: bool,
    },
}

const HEADER_KEY_SLOTS: usize = 64;
type RawKeySlots = [[u8; 48]; HEADER_KEY_SLOTS];

pub struct Engine {
    pub profile: Profile,
    fs: Option<FileSystem<SimDisk>>,
    pub model: Model,
    handles: [Slot; MAX_HANDLES as usize],
    armed: Armed,
    password: Option<String>,
    verified_slots: Option<(RawKeySlots, Vec<Option<String>>)>,
    lost: Option<disk::Lost>,
    touched: Vec<NodeId>,
    fatal: Option<String>,
    durable_check: DurableCheck,
    pub stats: Stats,
    pub now: Time,
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Some(fs) = self.fs.take() {
            if fs.disk.is_crashed() {
                let _ = fs.disk.into_lost();
            }
        }
    }
}

fn errno(err: Error) -> Errno {
    err.errno
}

fn resolve<D: Disk>(tx: &mut Transaction<D>, path: &[String]) -> Result<TreeData<Node>> {
    let mut node = tx.read_tree(TreePtr::<Node>::root())?;
    for part in path {
        if !node.data().is_dir() {
            return Err(Error::new(ENOTDIR));
        }
        node = tx.find_node(node.ptr(), part)?;
    }
    Ok(node)
}

fn resolve_parent<'a, D: Disk>(
    tx: &mut Transaction<D>,
    path: &'a [String],
) -> Result<(TreeData<Node>, &'a str)> {
    let (name, parent_path) = path.split_last().ok_or(Error::new(EINVAL))?;
    let parent = resolve(tx, parent_path)?;
    if !parent.data().is_dir() {
        return Err(Error::new(ENOTDIR));
    }
    Ok((parent, name.as_str()))
}

fn child<D: Disk>(
    tx: &mut Transaction<D>,
    parent: TreePtr<Node>,
    name: &str,
) -> Result<Option<TreeData<Node>>> {
    match tx.find_node(parent, name) {
        Ok(node) => Ok(Some(node)),
        Err(err) if err.errno == ENOENT => Ok(None),
        Err(err) => Err(err),
    }
}

fn node_type(node: &TreeData<Node>) -> u16 {
    if node.data().is_dir() {
        Node::MODE_DIR
    } else if node.data().is_symlink() {
        Node::MODE_SYMLINK
    } else {
        Node::MODE_FILE
    }
}

fn mount<D: Disk>(disk: D, password: Option<&str>) -> Result<FileSystem<D>> {
    FileSystem::open(disk, password.map(str::as_bytes), None, false)
}

fn free_at_level(fs: &FileSystem<SimDisk>, level: usize) -> bool {
    fs.allocator()
        .levels()
        .iter()
        .skip(level)
        .any(|starts| !starts.is_empty())
}

impl Engine {
    pub fn new(profile: Profile, seed: u64) -> std::result::Result<Engine, String> {
        assert!(
            disk::take_lost().is_none(),
            "stale lost disk left on worker thread"
        );
        seed_entropy(seed);
        let password = profile.encrypted.then(|| FIRST_PASSWORD.to_string());
        let disk = SimDisk::new(vec![0; profile.disk_bytes as usize], profile.durability);
        let fs = FileSystem::create(
            disk,
            password.as_deref().map(str::as_bytes),
            START_TIME.0,
            START_TIME.1,
        )
        .map_err(|err| format!("mkfs failed: {err}"))?;
        let mut model = Model::new(START_TIME, TreePtr::<Node>::root().id());
        model.key_slots[0] = password.clone();
        let durable_check = if profile.barriers {
            let mount_every = if profile.encrypted {
                ENCRYPTED_MOUNT_EVERY
            } else {
                MOUNT_EVERY
            };
            DurableCheck::PerOp { mount_every }
        } else {
            DurableCheck::AfterFlush {
                media_current: false,
            }
        };
        let mut engine = Engine {
            profile,
            fs: None,
            model,
            handles: [Slot::Free; MAX_HANDLES as usize],
            armed: Armed::default(),
            password,
            verified_slots: None,
            touched: Vec::new(),
            fatal: None,
            durable_check,
            lost: None,
            stats: Stats::default(),
            now: START_TIME,
        };
        engine.adopt(fs, Mounted::Fresh);
        engine.check_durable(true, true)?;
        Ok(engine)
    }

    fn adopt(&mut self, mut fs: FileSystem<SimDisk>, why: Mounted) {
        assert!(!fs.disk.is_crashed(), "adopting a crashed disk");
        fs.disk.guard = Some(Guard::new(fs.allocator().clone(), &fs.header));
        fs.access = Access::ReadWrite {
            barriers: if self.profile.barriers {
                Barriers::EveryCommit
            } else {
                Barriers::OnUnmount
            },
        };
        self.handles = [Slot::Free; MAX_HANDLES as usize];
        self.verified_slots = None;
        if let DurableCheck::AfterFlush { media_current } = &mut self.durable_check {
            *media_current = match why {
                Mounted::Fresh | Mounted::Reformatted => true,
                Mounted::CleanRemount | Mounted::AfterCrash => false,
            };
        }
        self.fs = Some(fs);
    }

    fn fs(&mut self) -> &mut FileSystem<SimDisk> {
        self.fs.as_mut().expect("mounted file system")
    }

    pub fn open_handles(&self) -> Vec<u8> {
        (0..MAX_HANDLES)
            .filter(|&slot| matches!(self.handles[slot as usize], Slot::Open { .. }))
            .collect()
    }

    pub fn free_handle(&self) -> Option<u8> {
        (0..MAX_HANDLES).find(|&slot| matches!(self.handles[slot as usize], Slot::Free))
    }

    pub fn handle_node(&self, slot: u8) -> Option<NodeId> {
        match self.handles.get(slot as usize).copied() {
            Some(Slot::Open { node, .. }) => Some(node),
            _ => None,
        }
    }

    fn open_count_for_fs(&self, fs_id: u32) -> u32 {
        self.handles
            .iter()
            .filter(|slot| matches!(slot, Slot::Open { fs, .. } if *fs == fs_id))
            .count() as u32
    }

    fn close_slot(&mut self, slot: u8) -> Result<()> {
        let Slot::Open { node, fs } = self.handles[slot as usize] else {
            panic!("close_slot on free handle slot {slot}");
        };
        self.handles[slot as usize] = Slot::Free;
        let last_for_fs = self.open_count_for_fs(fs) == 0;
        let result = self.fs().tx(|tx| tx.on_close_node(TreePtr::new(fs)));
        match result {
            Ok(()) => {
                self.model.close(node, true);
                if last_for_fs {
                    self.model.clear_release_pending();
                }
                Ok(())
            }
            Err(err) => {
                self.model.close(node, false);
                Err(err)
            }
        }
    }

    fn assert_handle_invariants(&self) {
        let fs = self.fs.as_ref().expect("mounted file system");
        let mut by_node: BTreeMap<NodeId, u32> = BTreeMap::new();
        let mut by_fs: BTreeMap<u32, u32> = BTreeMap::new();
        for slot in self.handles.iter() {
            if let Slot::Open { node, fs: fs_id } = *slot {
                let mnode = self
                    .model
                    .nodes
                    .get(&node)
                    .expect("open slot refers to missing model node");
                assert!(
                    !mnode.is_release_pending(),
                    "open slot refers to release-pending node {node}"
                );
                assert_eq!(
                    mnode.fs_id,
                    Some(fs_id),
                    "open slot fs id {fs_id} disagrees with model node {node}"
                );
                *by_node.entry(node).or_insert(0) += 1;
                *by_fs.entry(fs_id).or_insert(0) += 1;
            }
        }
        for (&id, mnode) in self.model.nodes.iter() {
            let expected = by_node.get(&id).copied().unwrap_or(0);
            assert_eq!(
                mnode.open_count(),
                expected,
                "model node {id} open count {} differs from open slots {expected}",
                mnode.open_count()
            );
        }
        for (&fs_id, &count) in by_fs.iter() {
            let usage = fs.node_usages.get(&fs_id).copied().unwrap_or(0);
            assert_eq!(
                usage,
                u64::from(count),
                "fs node_usages[{fs_id}] is {usage}, open slots {count}"
            );
        }
        for (&fs_id, &usage) in fs.node_usages.iter() {
            let count = by_fs.get(&fs_id).copied().unwrap_or(0);
            assert_eq!(
                usage,
                u64::from(count),
                "fs node_usages[{fs_id}] is {usage}, open slots {count}"
            );
        }
    }

    fn applicable(&self, op: &Op) -> bool {
        let handle_ok = |target: &Target| match target {
            Target::Handle(slot) => self.handle_node(*slot).is_some(),
            Target::Path(_) => true,
        };
        match op {
            Op::Write { target, .. } | Op::Truncate { target, .. } => {
                handle_ok(target)
                    && match target {
                        Target::Handle(slot) => {
                            self.model.node(self.handle_node(*slot).unwrap()).kind == Kind::File
                        }
                        Target::Path(_) => true,
                    }
            }
            Op::Fsync(target) | Op::SetTimes { target, .. } => handle_ok(target),
            Op::Open { handle, .. } => {
                (*handle as usize) < self.handles.len()
                    && matches!(self.handles[*handle as usize], Slot::Free)
            }
            Op::Close(handle) => self.handle_node(*handle).is_some(),
            Op::Link { from, .. } => !matches!(
                self.model.lookup(from).map(|id| self.model.node(id).kind),
                Ok(Kind::Dir)
            ),
            Op::SetKeySlot { slot, .. } | Op::ClearKeySlot(slot) => {
                self.profile.encrypted && (*slot as usize) < self.model.key_slots.len()
            }
            _ => true,
        }
    }

    fn target_node(&self, target: &Target, file: bool) -> std::result::Result<NodeId, Errno> {
        match target {
            Target::Handle(slot) => Ok(self.handle_node(*slot).expect("open handle")),
            Target::Path(path) => {
                if file {
                    self.model.file_target(path)
                } else {
                    self.model.lookup(path)
                }
            }
        }
    }

    fn expected(&self, op: &Op) -> std::result::Result<(), Errno> {
        match op {
            Op::Create(path) | Op::Mkdir(path) => self.model.check_create(path).map(drop),
            Op::Write { target, .. } | Op::Truncate { target, .. } => {
                self.target_node(target, true).map(drop)
            }
            Op::Rename {
                from,
                to,
                noreplace,
            } => self.model.check_rename(from, to, *noreplace).map(drop),
            Op::Link { from, to } => self.model.check_link(from, to).map(drop),
            Op::Unlink(path) => self.model.check_unlink(path, false).map(drop),
            Op::Rmdir(path) => self.model.check_unlink(path, true).map(drop),
            Op::Fsync(target) | Op::SetTimes { target, .. } => {
                self.target_node(target, false).map(drop)
            }
            Op::Open { path, .. } => self.model.lookup(path).map(drop),
            Op::ClearKeySlot(slot) => {
                let others = self
                    .model
                    .key_slots
                    .iter()
                    .enumerate()
                    .any(|(index, entry)| index != *slot as usize && entry.is_some());
                if others { Ok(()) } else { Err(EINVAL) }
            }
            _ => Ok(()),
        }
    }

    fn apply(&mut self, op: &Op, data: &[u8], created: Option<u32>) {
        let now = self.now;
        self.touched.clear();
        let result = match op {
            Op::Create(path) | Op::Mkdir(path) => {
                let kind = if matches!(op, Op::Mkdir(_)) {
                    Kind::Dir
                } else {
                    Kind::File
                };
                self.model.create(path, kind, now).map(|id| {
                    self.model.node_mut(id).fs_id = created;
                    self.touched.push(id);
                })
            }
            Op::Write { target, offset, .. } => self.target_node(target, true).map(|id| {
                if !data.is_empty() {
                    self.model.write(id, *offset, data);
                }
                self.touched.push(id);
            }),
            Op::Truncate { target, len } => self.target_node(target, true).map(|id| {
                self.model.truncate(id, *len);
                self.touched.push(id);
            }),
            Op::Rename {
                from,
                to,
                noreplace,
            } => self.model.rename(from, to, *noreplace, now),
            Op::Link { from, to } => self.model.link(from, to, now),
            Op::Unlink(path) => self.model.unlink(path, false, now),
            Op::Rmdir(path) => self.model.unlink(path, true, now),
            Op::Fsync(_) => Ok(()),
            Op::SetTimes {
                target,
                atime,
                mtime,
            } => self.target_node(target, false).map(|id| {
                self.model.set_times(id, *atime, *mtime);
            }),
            Op::Open { path, handle } => self.model.lookup(path).map(|id| {
                self.model.open(id);
                let fs = self
                    .model
                    .node(id)
                    .fs_id
                    .expect("open node has a file system id");
                self.handles[*handle as usize] = Slot::Open { node: id, fs };
            }),
            Op::Close(_) => Ok(()),
            Op::SetKeySlot { slot, password } => {
                self.model.key_slots[*slot as usize] = Some(password.clone());
                Ok(())
            }
            Op::ClearKeySlot(slot) => {
                self.model.key_slots[*slot as usize] = None;
                Ok(())
            }
            Op::Remount => Ok(()),
            Op::Reformat => {
                self.model = Model::new(now, TreePtr::<Node>::root().id());
                self.model.key_slots[0] =
                    self.profile.encrypted.then(|| FIRST_PASSWORD.to_string());
                self.handles = [Slot::Free; MAX_HANDLES as usize];
                Ok(())
            }
            Op::Crash { .. } | Op::ReadError { .. } | Op::Enumerate { .. } => Ok(()),
        };
        assert!(
            result.is_ok(),
            "model rejected an operation it had accepted"
        );
    }

    fn handle_ptr(&self, target: &Target) -> Option<TreePtr<Node>> {
        match target {
            Target::Handle(slot) => match self.handles[*slot as usize] {
                Slot::Open { fs, .. } => Some(TreePtr::new(fs)),
                Slot::Free => None,
            },
            Target::Path(_) => None,
        }
    }

    fn run_fs(&mut self, op: &Op, data: &[u8]) -> Result<Option<u32>> {
        let now = self.now;
        match op {
            Op::Create(path) | Op::Mkdir(path) => {
                let mode = if matches!(op, Op::Mkdir(_)) {
                    Node::MODE_DIR | 0o755
                } else {
                    Node::MODE_FILE | 0o644
                };
                self.fs().tx(|tx| {
                    let (parent, name) = resolve_parent(tx, path)?;
                    if child(tx, parent.ptr(), name)?.is_some() {
                        return Err(Error::new(EEXIST));
                    }
                    let node = tx.create_node(parent.ptr(), name, mode, now.0, now.1)?;
                    tx.touch_dir(parent.ptr(), now.0, now.1)?;
                    Ok(Some(node.id()))
                })
            }
            Op::Write { target, offset, .. } => {
                let handle = self.handle_ptr(target);
                self.fs().tx(|tx| {
                    let ptr = match (handle, target) {
                        (Some(ptr), _) => ptr,
                        (None, Target::Path(path)) => {
                            let node = resolve(tx, path)?;
                            if node.data().is_dir() {
                                return Err(Error::new(EISDIR));
                            }
                            node.ptr()
                        }
                        (None, Target::Handle(_)) => unreachable!(),
                    };
                    tx.write_node(ptr, *offset, data, now.0, now.1)?;
                    Ok(None)
                })
            }
            Op::Truncate { target, len } => {
                let handle = self.handle_ptr(target);
                self.fs().tx(|tx| {
                    let ptr = match (handle, target) {
                        (Some(ptr), _) => ptr,
                        (None, Target::Path(path)) => {
                            let node = resolve(tx, path)?;
                            if node.data().is_dir() {
                                return Err(Error::new(EISDIR));
                            }
                            node.ptr()
                        }
                        (None, Target::Handle(_)) => unreachable!(),
                    };
                    tx.truncate_node(ptr, *len, now.0, now.1)?;
                    Ok(None)
                })
            }
            Op::Rename {
                from,
                to,
                noreplace,
            } => self.fs().tx(|tx| {
                let orig = resolve(tx, from)?;
                let (orig_parent, orig_name) = resolve_parent(tx, from)?;
                let (new_parent, new_name) = resolve_parent(tx, to)?;
                let existing = child(tx, new_parent.ptr(), new_name)?;
                if *noreplace && existing.is_some() {
                    return Err(Error::new(EEXIST));
                }
                if let Some(target) = existing.as_ref() {
                    if target.data().is_dir() {
                        if !orig.data().is_dir() {
                            return Err(Error::new(EISDIR));
                        }
                        let mut children = Vec::new();
                        tx.child_nodes(target.ptr(), &mut children)?;
                        if !children.is_empty() {
                            return Err(Error::new(ENOTEMPTY));
                        }
                    } else if orig.data().is_dir() {
                        return Err(Error::new(ENOTDIR));
                    }
                }
                tx.rename_node(orig_parent.ptr(), orig_name, new_parent.ptr(), new_name)?;
                tx.touch_dir(orig_parent.ptr(), now.0, now.1)?;
                if new_parent.id() != orig_parent.id() {
                    tx.touch_dir(new_parent.ptr(), now.0, now.1)?;
                }
                Ok(None)
            }),
            Op::Link { from, to } => self.fs().tx(|tx| {
                let orig = resolve(tx, from)?;
                let (new_parent, new_name) = resolve_parent(tx, to)?;
                if let Some(target) = child(tx, new_parent.ptr(), new_name)? {
                    if target.data().is_dir() {
                        if !orig.data().is_dir() {
                            return Err(Error::new(EACCES));
                        }
                    } else if orig.data().is_dir() {
                        return Err(Error::new(ENOTDIR));
                    }
                }
                tx.link_node(new_parent.ptr(), new_name, orig.ptr())?;
                tx.touch_dir(new_parent.ptr(), now.0, now.1)?;
                Ok(None)
            }),
            Op::Unlink(path) | Op::Rmdir(path) => {
                let rmdir = matches!(op, Op::Rmdir(_));
                self.fs().tx(|tx| {
                    let node = resolve(tx, path)?;
                    let (parent, name) = resolve_parent(tx, path)?;
                    if rmdir {
                        if !node.data().is_dir() {
                            return Err(Error::new(ENOTDIR));
                        }
                        tx.remove_node(parent.ptr(), name, Node::MODE_DIR)?;
                    } else {
                        if node.data().is_dir() {
                            return Err(Error::new(EISDIR));
                        }
                        tx.remove_node(parent.ptr(), name, node_type(&node))?;
                    }
                    tx.touch_dir(parent.ptr(), now.0, now.1)?;
                    Ok(None)
                })
            }
            Op::Fsync(target) => {
                let handle = self.handle_ptr(target);
                let is_dir = self.fs().tx(|tx| match (handle, target) {
                    (Some(ptr), _) => Ok(tx.read_tree(ptr)?.data().is_dir()),
                    (None, Target::Path(path)) => Ok(resolve(tx, path)?.data().is_dir()),
                    (None, Target::Handle(_)) => unreachable!(),
                })?;
                if is_dir {
                    self.fs().tx(|tx| tx.sync(false).map(|_| ()))?;
                } else {
                    self.fs().tx(|_| Ok(()))?;
                }
                Ok(None)
            }
            Op::SetTimes {
                target,
                atime,
                mtime,
            } => {
                let handle = self.handle_ptr(target);
                self.fs().tx(|tx| {
                    let mut node = match (handle, target) {
                        (Some(ptr), _) => tx.read_tree(ptr)?,
                        (None, Target::Path(path)) => resolve(tx, path)?,
                        (None, Target::Handle(_)) => unreachable!(),
                    };
                    let mut changed = false;
                    if node.data().mtime() != *mtime {
                        node.data_mut().set_mtime(mtime.0, mtime.1);
                        changed = true;
                    }
                    if node.data().atime() != *atime {
                        node.data_mut().set_atime(atime.0, atime.1);
                        changed = true;
                    }
                    if changed {
                        tx.sync_tree(node)?;
                    }
                    Ok(None)
                })
            }
            Op::Open { path, .. } => {
                let ptr = self.fs().tx(|tx| Ok(resolve(tx, path)?.ptr()))?;
                self.fs().tx(|tx| tx.on_open_node(ptr))?;
                Ok(Some(ptr.id()))
            }
            Op::Close(slot) => {
                self.close_slot(*slot)?;
                Ok(None)
            }
            Op::SetKeySlot { slot, password } => {
                let current = self
                    .password
                    .clone()
                    .expect("encrypted file system password");
                let fs = self.fs();
                let (_, keys) = fs
                    .header
                    .unlock(current.as_bytes())
                    .ok_or(Error::new(EIO))?;
                let salt = Salt::new().map_err(|_| Error::new(EIO))?;
                let entry =
                    KeySlot::new(password.as_bytes(), salt, keys).map_err(|_| Error::new(EIO))?;
                fs.set_key_slot(*slot as usize, entry)?;
                Ok(None)
            }
            Op::ClearKeySlot(slot) => {
                self.fs().set_key_slot(*slot as usize, KeySlot::default())?;
                Ok(None)
            }
            Op::Remount => {
                for slot in self.open_handles() {
                    self.close_slot(slot)?;
                    self.refresh_guard();
                }
                self.fs().cleanup()?;
                self.model.clear_release_pending();
                self.fs().disk.flush()?;
                self.refresh_guard();
                let mut fs = self.fs.take().expect("mounted file system");
                fs.disk.disarm_read_error();
                let password = self.password.clone();
                let fs = match mount(fs.disk, password.as_deref()) {
                    Ok(fs) => fs,
                    Err(err) => {
                        self.fatal = Some(format!(
                            "remount: the file system does not mount after a clean unmount: {err}"
                        ));
                        return Err(err);
                    }
                };
                self.adopt(fs, Mounted::CleanRemount);
                self.fs().cleanup()?;
                self.model.clear_release_pending();
                Ok(None)
            }
            Op::Reformat => {
                self.handles = [Slot::Free; MAX_HANDLES as usize];
                let fs = self.fs.take().expect("mounted file system");
                let mut disk = fs.disk;
                disk.guard = None;
                let password = self.profile.encrypted.then(|| FIRST_PASSWORD.to_string());
                match FileSystem::create(disk, password.as_deref().map(str::as_bytes), now.0, now.1)
                {
                    Ok(fs) => {
                        self.password = password;
                        self.adopt(fs, Mounted::Reformatted);
                        Ok(Some(TreePtr::<Node>::root().id()))
                    }
                    Err(err) => {
                        self.lost = disk::take_lost();
                        if self.lost.is_none() {
                            self.fatal =
                                Some(format!("reformat failed without a power loss: {err}"));
                        }
                        Err(err)
                    }
                }
            }
            Op::Crash { .. } | Op::ReadError { .. } | Op::Enumerate { .. } => Ok(None),
        }
    }

    fn refresh_guard(&mut self) {
        let fs = self.fs.as_mut().expect("mounted file system");
        let generation = fs.header.generation();
        let stale = fs
            .disk
            .guard
            .as_ref()
            .is_none_or(|guard| guard.free_generation != generation || guard.wrote_non_header);
        if stale {
            let free = fs.allocator().clone();
            let header = fs.header;
            fs.disk.guard = Some(Guard::new(free, &header));
        }
    }

    fn take_violations(&mut self) -> std::result::Result<(), String> {
        let fs = self.fs.as_mut().expect("mounted file system");
        if fs.disk.violations.is_empty() {
            return Ok(());
        }
        Err(fs.disk.violations.join("; "))
    }

    fn reconcile_key_slot(
        &mut self,
        op: &Op,
        before_raw: &RawKeySlots,
    ) -> std::result::Result<(), String> {
        let (slot, requested) = match op {
            Op::SetKeySlot { slot, password } => (*slot as usize, Some(password.clone())),
            Op::ClearKeySlot(slot) => (*slot as usize, None),
            _ => return Ok(()),
        };
        let header = self.fs.as_ref().expect("mounted file system").header;
        let after_raw = header_slot_bytes(&header);
        for index in 0..after_raw.len() {
            if index != slot && after_raw[index] != before_raw[index] {
                return Err(format!(
                    "failed {} altered unrelated key slot {index}",
                    op_text(op)
                ));
            }
        }
        if after_raw[slot] == before_raw[slot] {
            return Ok(());
        }
        if key_slot_matches(&header, slot, requested.as_deref()).is_ok() {
            self.model.key_slots[slot] = requested;
            return Ok(());
        }
        Err(format!(
            "failed {} left key slot {slot} in neither its old nor requested state",
            op_text(op)
        ))
    }

    fn sync_passwords(&mut self) {
        if !self.profile.encrypted {
            return;
        }
        let valid = self.password.as_ref().is_some_and(|password| {
            self.model
                .key_slots
                .iter()
                .any(|slot| slot.as_ref() == Some(password))
        });
        if !valid {
            self.password = self.model.key_slots.iter().flatten().next().cloned();
        }
    }

    fn run_step(
        &mut self,
        op: &Op,
        data: &[u8],
        armed: Armed,
    ) -> std::result::Result<After, String> {
        let crash_ctx = armed.crash.map(|plan| (plan, self.model.clone()));
        {
            let fs = self.fs();
            if let Some((plan, _)) = &crash_ctx {
                fs.disk.arm_crash(u64::from(plan.after));
            }
            if let Some(nth) = armed.read_error {
                fs.disk.arm_read_error(u64::from(nth));
            }
            if armed.enumerate.is_some() {
                fs.disk.start_recording();
            }
        }
        let read_errors_before = self.fs().disk.read_errors;
        let actual = self.run_fs(op, data).map_err(errno);
        if let Some(fatal) = self.fatal.take() {
            return Err(fatal);
        }
        let crashed = self.fs.as_ref().is_none_or(|fs| fs.disk.is_crashed());
        if !crashed {
            let fs = self.fs();
            fs.disk.disarm_crash();
            fs.disk.disarm_read_error();
            let read_error_fired = fs.disk.read_errors != read_errors_before;
            return Ok(After::Ran {
                fs_ok: actual.is_ok(),
                actual,
                read_error_fired,
                remaining_crash: crash_ctx.map(|(plan, _)| plan),
            });
        }
        let lost = match self.fs.take() {
            Some(fs) => fs.disk.into_lost(),
            None => match self.lost.take() {
                Some(lost) => lost,
                None => return Err("file system unmounted without a lost disk".to_string()),
            },
        };
        let read_error_fired = lost.read_errors != read_errors_before;
        let Some((plan, before)) = crash_ctx else {
            return Err("disk lost power without an armed crash plan".to_string());
        };
        Ok(After::PowerLost {
            lost,
            plan,
            before,
            read_error_fired,
        })
    }

    pub fn step(&mut self, step: &Step) -> std::result::Result<(), String> {
        let expected_barriers = if self.profile.barriers {
            Barriers::EveryCommit
        } else {
            Barriers::OnUnmount
        };
        assert_eq!(
            self.fs.as_ref().expect("mounted file system").access,
            Access::ReadWrite {
                barriers: expected_barriers,
            }
        );
        match &step.op {
            Op::Crash { after, kind, salt } => {
                self.armed.crash = Some(CrashPlan {
                    after: *after,
                    kind: *kind,
                    salt: *salt,
                });
                return Ok(());
            }
            Op::ReadError { nth } => {
                self.armed.read_error = Some(*nth);
                return Ok(());
            }
            Op::Enumerate { salt } => {
                if !self.profile.encrypted {
                    self.armed.enumerate = Some(*salt);
                }
                return Ok(());
            }
            _ => {}
        }
        self.now = step.time;
        let op = &step.op;
        if !self.applicable(op) {
            self.armed = Armed::default();
            return Ok(());
        }
        self.stats.ops += 1;
        let armed = std::mem::take(&mut self.armed);
        let enum_ctx = armed.enumerate.map(|salt| (salt, self.model.clone()));
        let data = match op {
            Op::Write { len, pattern, .. } => pattern.bytes(*len as usize),
            _ => Vec::new(),
        };
        let expected = self.expected(op);
        let writes_before = self.fs().disk.writes;
        let generation_before = self.fs().header.generation();
        let uuid_before = self.fs().header.uuid();
        let slots_before = matches!(op, Op::SetKeySlot { .. } | Op::ClearKeySlot(_))
            .then(|| header_slot_bytes(&self.fs().header));
        let close_target = match op {
            Op::Close(slot) => self.handle_node(*slot),
            _ => None,
        };
        let (fs_ok, actual, read_error_fired, remaining_crash) =
            match self.run_step(op, &data, armed)? {
                After::PowerLost {
                    lost,
                    plan,
                    before,
                    read_error_fired,
                } => {
                    if read_error_fired {
                        self.stats.read_errors += 1;
                    }
                    let mut after = before.clone();
                    if expected.is_ok() {
                        match op {
                            Op::Close(_) => {
                                after.close(close_target.expect("open handle"), true);
                            }
                            Op::Remount => {
                                after.close_all();
                            }
                            _ => {
                                std::mem::swap(&mut self.model, &mut after);
                                self.apply(op, &data, None);
                                std::mem::swap(&mut self.model, &mut after);
                            }
                        }
                    }
                    return self.crash(lost, plan.kind, plan.salt, vec![before, after], op);
                }
                After::Ran {
                    fs_ok,
                    actual,
                    read_error_fired,
                    remaining_crash,
                } => (fs_ok, actual, read_error_fired, remaining_crash),
            };
        if read_error_fired {
            self.stats.read_errors += 1;
        }
        let outcome = self.judge(op, expected, actual.clone(), read_error_fired)?;
        let wrote = self.fs().disk.writes - writes_before;
        let refused = !outcome && expected.err().is_some() && expected.err() == actual.err();
        let unchanged = matches!(op, Op::Fsync(_)) && fs_ok;
        if wrote != 0 && (refused || unchanged) {
            return Err(format!(
                "{step}: wrote {wrote} blocks although it changed nothing"
            ));
        }
        if outcome {
            let created = match (op, &actual) {
                (Op::Create(_) | Op::Mkdir(_), Ok(created)) => *created,
                _ => None,
            };
            self.apply(op, &data, created);
        } else {
            self.touched.clear();
            if expected.is_ok() {
                if let Some(before_raw) = slots_before {
                    self.reconcile_key_slot(op, &before_raw)
                        .map_err(|err| format!("{step}: {err}"))?;
                }
            }
        }
        self.assert_handle_invariants();
        if !outcome {
            self.stats.failed_ops += 1;
        } else if matches!(op, Op::Remount) {
            self.stats.remounts += 1;
        }
        self.sync_passwords();
        self.take_violations()
            .map_err(|err| format!("{}: {err}", step))?;
        if outcome && matches!(op, Op::SetKeySlot { .. } | Op::ClearKeySlot(_)) {
            self.check_ring_key_slots()
                .map_err(|err| format!("{step}: {err}"))?;
        }
        if outcome && matches!(op, Op::Reformat) && self.fs().header.uuid() == uuid_before {
            return Err(format!("{step}: the new file system kept the old UUID"));
        }
        if let Some((salt, before)) = enum_ctx {
            let recording = self.fs().disk.stop_recording().expect("recording");
            self.enumerate(&recording, salt, &before, op)?;
        }
        self.refresh_guard();
        if let Some(plan) = remaining_crash {
            let lost = self
                .fs
                .take()
                .expect("mounted file system")
                .disk
                .into_lost();
            let model = self.model.clone();
            return self.crash(lost, plan.kind, plan.salt, vec![model], op);
        }
        let generation_after = self.fs().header.generation();
        let threshold = generation_after / ALLOC_GC_THRESHOLD * ALLOC_GC_THRESHOLD;
        let squashed = (outcome && matches!(op, Op::Remount | Op::Reformat))
            || (threshold >= ALLOC_GC_THRESHOLD
                && generation_before <= threshold
                && threshold < generation_after);
        let full = self.stats.ops % self.profile.full_check_every == 0;
        self.check_durable(full, squashed)
    }

    fn judge(
        &mut self,
        op: &Op,
        expected: std::result::Result<(), Errno>,
        actual: std::result::Result<Option<u32>, Errno>,
        read_error_fired: bool,
    ) -> std::result::Result<bool, String> {
        match (&expected, &actual) {
            (Ok(()), Ok(_)) => return Ok(true),
            (Err(want), Err(got)) if want == got => return Ok(false),
            _ => {}
        }
        if read_error_fired && actual == Err(EIO) {
            return Ok(false);
        }
        if actual == Err(ENOSPC) {
            self.stats.enospc += 1;
            let needed = match op {
                Op::Write { len, .. } => u64::from(*len) / 4096 * 2,
                Op::Truncate { len, .. } => len / 4096 / 8,
                _ => 0,
            };
            let fs = self.fs.as_ref().expect("mounted file system");
            let free = fs.allocator().free();
            if free > needed + ENOSPC_SLACK_BLOCKS && free_at_level(fs, RECORD_LEVEL) {
                return Err(format!(
                    "{}: ENOSPC with {free} free blocks and free records",
                    op_text(op)
                ));
            }
            return Ok(false);
        }
        Err(format!(
            "{}: expected {}, got {}",
            op_text(op),
            describe(&expected.map(|_| None)),
            describe(&actual)
        ))
    }

    fn crash(
        &mut self,
        lost: disk::Lost,
        kind: CrashKind,
        salt: u64,
        candidates: Vec<Model>,
        op: &Op,
    ) -> std::result::Result<(), String> {
        self.stats.crashes += 1;
        let mut rng = Rng::new(salt);
        let image = lost.crash_image(kind, &mut rng);
        self.stats.writes += lost.writes;
        self.stats.flushes += lost.flushes;
        drop(lost);
        let context = format!("after power loss ({}) during {}", kind.name(), op_text(op));
        let passwords = self.candidate_passwords(&candidates);
        let mut mounted = None;
        let mut last_error = None;
        for password in passwords.iter() {
            let attempt = SimDisk::new(image.clone(), self.profile.durability);
            match mount(attempt, password.as_deref()) {
                Ok(fs) => {
                    mounted = Some((fs, password.clone()));
                    break;
                }
                Err(err) => last_error = Some(err),
            }
        }
        let Some((mut fs, password)) = mounted else {
            if matches!(op, Op::Reformat) {
                self.stats.unformatted += 1;
                let after = candidates
                    .into_iter()
                    .last()
                    .expect("model after the reformat");
                return self.format_lost_disk(image, after);
            }
            return Err(format!(
                "{context}: the file system does not mount: {}",
                last_error.map_or("no password".to_string(), |err| err.to_string())
            ));
        };
        self.stats.crash_mounts += 1;
        let snapshot =
            check::snapshot(&mut fs).map_err(|err| format!("{context}: walk failed: {err}"))?;
        if !snapshot.problems.is_empty() {
            return Err(format!("{context}: {}", snapshot.problems.join("; ")));
        }
        let mut errors = Vec::new();
        let mut chosen = None;
        for (index, candidate) in candidates.iter().enumerate() {
            if self.profile.encrypted {
                if let Err(err) = key_slots_match(&fs.header, &candidate.key_slots) {
                    errors.push(err);
                    continue;
                }
            }
            match check::compare(&snapshot, candidate, true) {
                Ok(matching) => {
                    let mut all = candidate.nodes.keys().copied();
                    match check::compare_contents(
                        &mut fs, &snapshot, candidate, &matching, &mut all,
                    ) {
                        Ok(_) => {
                            chosen = Some((index, matching));
                            break;
                        }
                        Err(err) => errors.push(err),
                    }
                }
                Err(err) => errors.push(err),
            }
        }
        let Some((index, matching)) = chosen else {
            return Err(format!(
                "{context}: the mounted state matches neither the state before nor after: {}",
                errors.join(" | ")
            ));
        };
        let mut model = candidates.into_iter().nth(index).expect("candidate");
        for (model_id, fs_id) in matching.ids.iter() {
            model.node_mut(*model_id).fs_id = Some(*fs_id);
        }
        model.close_all();
        self.model = model;
        self.password = password;
        self.sync_passwords();
        self.adopt(fs, Mounted::AfterCrash);
        self.fs()
            .cleanup()
            .map_err(|err| format!("{context}: cleanup after mount failed: {err}"))?;
        self.assert_handle_invariants();
        self.take_violations()
            .map_err(|err| format!("{context}: cleanup: {err}"))?;
        self.refresh_guard();
        self.check_durable(true, true)
            .map_err(|err| format!("{context}: after cleanup: {err}"))
    }

    fn format_lost_disk(
        &mut self,
        image: Vec<u8>,
        model: Model,
    ) -> std::result::Result<(), String> {
        let now = self.now;
        let password = self.profile.encrypted.then(|| FIRST_PASSWORD.to_string());
        let disk = SimDisk::new(image, self.profile.durability);
        let fs = FileSystem::create(disk, password.as_deref().map(str::as_bytes), now.0, now.1)
            .map_err(|err| {
                format!("formatting a disk left unformatted by a power loss failed: {err}")
            })?;
        self.model = model;
        self.password = password;
        self.adopt(fs, Mounted::Reformatted);
        self.assert_handle_invariants();
        self.check_durable(true, true)
    }

    fn check_ring_key_slots(&self) -> std::result::Result<(), String> {
        let fs = self.fs.as_ref().expect("mounted file system");
        let current = fs.header.key_slots;
        let media = fs.disk.media();
        for slot in 0..HEADER_RING {
            let start = ((fs.block + slot) as usize) * disk::BLOCK;
            let mut header = Header::default();
            header.copy_from_slice(&media[start..start + disk::BLOCK]);
            if !header.valid() {
                continue;
            }
            let slots = header.key_slots;
            for (index, (found, wanted)) in slots.iter().zip(current.iter()).enumerate() {
                if found.to_bytes() != wanted.to_bytes() {
                    return Err(format!(
                        "ring header {slot} (generation {}) still holds an old key slot {index}",
                        header.generation()
                    ));
                }
            }
        }
        Ok(())
    }

    fn candidate_passwords(&self, candidates: &[Model]) -> Vec<Option<String>> {
        if !self.profile.encrypted {
            return vec![None];
        }
        let mut passwords: Vec<Option<String>> = Vec::new();
        if let Some(current) = self.password.as_ref() {
            passwords.push(Some(current.clone()));
        }
        for candidate in candidates {
            for slot in candidate.key_slots.iter().flatten() {
                if !passwords.iter().any(|known| known.as_ref() == Some(slot)) {
                    passwords.push(Some(slot.clone()));
                }
            }
        }
        passwords
    }

    fn enumerate(
        &mut self,
        recording: &disk::Recording,
        salt: u64,
        before: &Model,
        op: &Op,
    ) -> std::result::Result<(), String> {
        let durability = self.profile.durability;
        let mut rng = Rng::new(salt);
        let fs = self.fs.as_ref().expect("mounted file system");
        let media = fs.disk.media();
        let Some(boundaries) = disk::boundaries(recording, media, durability, &mut rng) else {
            return Ok(());
        };
        let after = &self.model;
        let candidates = [after, before];
        let touched = self.touched.clone();
        for boundary in boundaries {
            self.stats.boundaries += 1;
            let view = View::new(media, Some(&recording.undo), boundary.overlay);
            let mut mounted = match mount(view, None) {
                Ok(mounted) => mounted,
                Err(_) if matches!(op, Op::Reformat) => {
                    self.stats.unformatted += 1;
                    continue;
                }
                Err(err) => {
                    return Err(format!(
                        "{}: the file system does not mount: {err}",
                        boundary.label
                    ));
                }
            };
            mounted.access = Access::ReadOnly;
            let snapshot = check::snapshot(&mut mounted)
                .map_err(|err| format!("{}: walk failed: {err}", boundary.label))?;
            if !snapshot.problems.is_empty() {
                return Err(format!(
                    "{}: {}",
                    boundary.label,
                    snapshot.problems.join("; ")
                ));
            }
            let mut errors = Vec::new();
            let mut matched = false;
            for candidate in candidates.iter() {
                match check::compare(&snapshot, candidate, true) {
                    Ok(matching) => {
                        let mut which = touched.iter().copied();
                        match check::compare_contents(
                            &mut mounted,
                            &snapshot,
                            candidate,
                            &matching,
                            &mut which,
                        ) {
                            Ok(_) => {
                                matched = true;
                                break;
                            }
                            Err(err) => errors.push(err),
                        }
                    }
                    Err(err) => errors.push(err),
                }
            }
            if !matched {
                return Err(format!(
                    "{}: matches neither the state before nor after the operation: {}",
                    boundary.label,
                    errors.join(" | ")
                ));
            }
            if mounted.disk.refused_writes != 0 {
                return Err(format!(
                    "{}: a read-only mount tried {} writes",
                    boundary.label, mounted.disk.refused_writes
                ));
            }
        }
        Ok(())
    }

    pub fn check_durable(&mut self, full: bool, squashed: bool) -> std::result::Result<(), String> {
        self.stats.checks += 1;
        let touched = self.touched.clone();
        let live_only = match &mut self.durable_check {
            DurableCheck::PerOp { mount_every } => {
                let unflushed = self
                    .fs
                    .as_ref()
                    .expect("mounted file system")
                    .disk
                    .unflushed();
                if unflushed != 0 {
                    return Err(format!(
                        "the operation returned with {unflushed} writes still in the volatile cache"
                    ));
                }
                !full && !squashed && self.stats.ops % *mount_every != 0
            }
            DurableCheck::AfterFlush { media_current } => !std::mem::take(media_current),
        };
        if live_only {
            let model = &self.model;
            let encrypted = self.profile.encrypted;
            let verified_slots = &mut self.verified_slots;
            let fs = self.fs.as_mut().expect("mounted file system");
            let writes = fs.disk.writes;
            let snapshot = check::snapshot(fs).map_err(|err| format!("live walk failed: {err}"))?;
            verify(
                fs,
                &snapshot,
                model,
                full,
                &touched,
                encrypted,
                verified_slots,
            )?;
            if fs.disk.writes != writes {
                return Err(format!(
                    "transactions that changed nothing wrote {} blocks",
                    fs.disk.writes - writes
                ));
            }
            return Ok(());
        }
        self.stats.durable_mounts += 1;
        let password = self.password.clone();
        let fs = self.fs.as_ref().expect("mounted file system");
        let live_generation = fs.header.generation();
        let live_slots = self
            .profile
            .encrypted
            .then(|| header_slot_bytes(&fs.header));
        let live_free = disk::free_ranges(fs.allocator());
        let live_layout = fs.allocator().levels().clone();
        let view = View::new(fs.disk.media(), None, BTreeMap::new());
        let mut mounted = mount(view, password.as_deref())
            .map_err(|err| format!("the durable state does not mount: {err}"))?;
        mounted.access = Access::ReadOnly;
        let snapshot =
            check::snapshot(&mut mounted).map_err(|err| format!("durable walk failed: {err}"))?;
        if snapshot.generation != live_generation {
            return Err(format!(
                "the disk holds generation {} after the file system committed generation {live_generation}",
                snapshot.generation
            ));
        }
        if let Some(expected_slots) = live_slots {
            if header_slot_bytes(&mounted.header) != expected_slots {
                return Err(
                    "the durable header's key slots differ from the running header".to_string(),
                );
            }
        }
        let rebuilt_free = disk::free_ranges(mounted.allocator());
        if rebuilt_free != live_free {
            let only_live: Vec<_> = live_free
                .iter()
                .filter(|range| !rebuilt_free.contains(range))
                .take(4)
                .collect();
            let only_rebuilt: Vec<_> = rebuilt_free
                .iter()
                .filter(|range| !live_free.contains(range))
                .take(4)
                .collect();
            return Err(format!(
                "the free set rebuilt from the allocator log differs from the running allocator: running only {only_live:?}, rebuilt only {only_rebuilt:?}"
            ));
        }
        if mounted.allocator().levels() != &live_layout {
            self.stats.layout_differences += 1;
        }
        if squashed {
            let entries: usize = mounted
                .allocator()
                .levels()
                .iter()
                .map(|level| level.len())
                .sum();
            let bound = (entries + SQUASH_SLACK_ENTRIES).div_ceil(ALLOC_LIST_ENTRIES) + 2;
            if snapshot.alloc_blocks > bound {
                return Err(format!(
                    "the allocator log keeps {} blocks after a squash, more than {bound}",
                    snapshot.alloc_blocks
                ));
            }
        }
        verify(
            &mut mounted,
            &snapshot,
            &self.model,
            full,
            &touched,
            self.profile.encrypted,
            &mut self.verified_slots,
        )?;
        if mounted.disk.refused_writes != 0 {
            return Err(format!(
                "a read-only mount tried {} writes",
                mounted.disk.refused_writes
            ));
        }
        Ok(())
    }

    pub fn finish(&mut self) -> std::result::Result<(), String> {
        if matches!(self.durable_check, DurableCheck::AfterFlush { .. }) {
            self.fs()
                .disk
                .flush()
                .map_err(|err| format!("the flush at unmount failed: {err}"))?;
            self.durable_check = DurableCheck::AfterFlush {
                media_current: true,
            };
        }
        self.check_durable(true, false)?;
        if let Some(fs) = self.fs.as_ref() {
            self.stats.writes += fs.disk.writes;
            self.stats.flushes += fs.disk.flushes;
        }
        Ok(())
    }

    pub fn paths(&self) -> Vec<(Path, NodeId)> {
        self.model.paths(self.profile.max_nodes * 2)
    }
}

fn header_slot_bytes(header: &Header) -> RawKeySlots {
    header.key_slots.map(|slot| slot.to_bytes())
}

fn key_slot_matches(
    header: &Header,
    index: usize,
    slot: Option<&str>,
) -> std::result::Result<(), String> {
    let stored = header.key_slots[index];
    match slot {
        None => {
            if !stored.is_empty() {
                return Err(format!("key slot {index} should be empty"));
            }
        }
        Some(password) => {
            if stored.is_empty() {
                return Err(format!("key slot {index} should hold {password}"));
            }
            let mut single = *header;
            single.key_slots = [KeySlot::default(); HEADER_KEY_SLOTS];
            single.key_slots[index] = stored;
            if single.unlock(password.as_bytes()).is_none() {
                return Err(format!("{password} does not open key slot {index}"));
            }
        }
    }
    Ok(())
}

fn key_slots_match(header: &Header, slots: &[Option<String>]) -> std::result::Result<(), String> {
    let stored = header.key_slots;
    for (index, slot) in slots.iter().enumerate() {
        key_slot_matches(header, index, slot.as_deref())?;
    }
    for (index, entry) in stored.iter().enumerate().skip(slots.len()) {
        if !entry.is_empty() {
            return Err(format!("key slot {index} outside the model is not empty"));
        }
    }
    Ok(())
}

fn verify<D: Disk>(
    fs: &mut FileSystem<D>,
    snapshot: &Snapshot,
    model: &Model,
    full: bool,
    touched: &[NodeId],
    encrypted: bool,
    verified_slots: &mut Option<(RawKeySlots, Vec<Option<String>>)>,
) -> std::result::Result<(), String> {
    if !snapshot.problems.is_empty() {
        return Err(snapshot.problems.join("; "));
    }
    if encrypted {
        let raw = header_slot_bytes(&fs.header);
        let unchanged = !full
            && verified_slots
                .as_ref()
                .is_some_and(|(cached_raw, cached_model)| {
                    *cached_raw == raw && cached_model.as_slice() == model.key_slots.as_slice()
                });
        if !unchanged {
            key_slots_match(&fs.header, &model.key_slots)?;
            *verified_slots = Some((raw, model.key_slots.clone()));
        }
    }
    let matching = check::compare(snapshot, model, false)?;
    if full {
        let mut all = model.nodes.keys().copied();
        check::compare_contents(fs, snapshot, model, &matching, &mut all)?;
        check::find_all(fs, snapshot)?;
    } else {
        let mut which = touched.iter().copied();
        check::compare_contents(fs, snapshot, model, &matching, &mut which)?;
    }
    Ok(())
}

fn describe(result: &std::result::Result<Option<u32>, Errno>) -> String {
    match result {
        Ok(_) => "success".to_string(),
        Err(code) => format!("{} ({code})", errno_name(*code)),
    }
}

pub fn errno_name(code: Errno) -> &'static str {
    match code {
        ENOENT => "ENOENT",
        EEXIST => "EEXIST",
        ENOTDIR => "ENOTDIR",
        EISDIR => "EISDIR",
        EINVAL => "EINVAL",
        ENOTEMPTY => "ENOTEMPTY",
        ENOSPC => "ENOSPC",
        EIO => "EIO",
        EACCES => "EACCES",
        _ => "errno",
    }
}

fn op_text(op: &Op) -> String {
    let text = Step {
        time: (0, 0),
        op: op.clone(),
    }
    .to_string();
    text.split_once(' ')
        .map(|(_, rest)| rest.to_string())
        .unwrap_or(text)
}

use std::collections::BTreeMap;

use redoxfs::{
    ALLOC_GC_THRESHOLD, ALLOC_LIST_ENTRIES, Disk, FileSystem, HEADER_RING, Header, KeySlot, Node,
    Salt, Transaction, TreeData, TreePtr,
};
use syscall::error::{EEXIST, EINVAL, EIO, EISDIR, ENOENT, ENOSPC, ENOTDIR, ENOTEMPTY, EACCES, Error, Result};

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

#[derive(Clone, Copy)]
struct Handle {
    model: NodeId,
    fs: u32,
}

pub struct Engine {
    pub profile: Profile,
    fs: Option<FileSystem<SimDisk>>,
    pub model: Model,
    handles: [Option<Handle>; MAX_HANDLES as usize],
    open_counts: BTreeMap<u32, u32>,
    pending_crash: Option<(u32, CrashKind, u64)>,
    pending_read_error: Option<u32>,
    pending_enumerate: Option<u64>,
    password: Option<String>,
    lost: Option<disk::Lost>,
    touched: Vec<NodeId>,
    fatal: Option<String>,
    durable: bool,
    pub stats: Stats,
    pub now: Time,
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
        seed_entropy(seed);
        let password = profile.encrypted.then(|| FIRST_PASSWORD.to_string());
        let disk = SimDisk::new(vec![0; profile.disk_bytes as usize], profile.durability);
        let mut fs = FileSystem::create(
            disk,
            password.as_deref().map(str::as_bytes),
            START_TIME.0,
            START_TIME.1,
        )
        .map_err(|err| format!("mkfs failed: {err}"))?;
        fs.disk.guard = Some(Guard::new(fs.allocator().clone(), fs.header.generation()));
        fs.barriers = profile.barriers;
        let mut model = Model::new(START_TIME, TreePtr::<Node>::root().id());
        model.key_slots[0] = password.clone();
        let mut engine = Engine {
            profile,
            fs: Some(fs),
            model,
            handles: [None; MAX_HANDLES as usize],
            open_counts: BTreeMap::new(),
            pending_crash: None,
            pending_read_error: None,
            pending_enumerate: None,
            password,
            touched: Vec::new(),
            fatal: None,
            durable: true,
            lost: None,
            stats: Stats::default(),
            now: START_TIME,
        };
        engine.check_durable(true, true)?;
        Ok(engine)
    }

    fn fs(&mut self) -> &mut FileSystem<SimDisk> {
        self.fs.as_mut().expect("mounted file system")
    }

    pub fn open_handles(&self) -> Vec<u8> {
        (0..MAX_HANDLES)
            .filter(|&slot| self.handles[slot as usize].is_some())
            .collect()
    }

    pub fn free_handle(&self) -> Option<u8> {
        (0..MAX_HANDLES).find(|&slot| self.handles[slot as usize].is_none())
    }

    pub fn handle_node(&self, slot: u8) -> Option<NodeId> {
        self.handles.get(slot as usize).copied().flatten().map(|handle| handle.model)
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
                (*handle as usize) < self.handles.len() && self.handles[*handle as usize].is_none()
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
                let fs = self.model.node(id).fs_id.expect("open node has a file system id");
                self.handles[*handle as usize] = Some(Handle { model: id, fs });
            }),
            Op::Close(handle) => {
                let id = self.handle_node(*handle).expect("open handle");
                self.model.close(id);
                self.handles[*handle as usize] = None;
                Ok(())
            }
            Op::SetKeySlot { slot, password } => {
                self.model.key_slots[*slot as usize] = Some(password.clone());
                Ok(())
            }
            Op::ClearKeySlot(slot) => {
                self.model.key_slots[*slot as usize] = None;
                Ok(())
            }
            Op::Remount => {
                self.model.close_all();
                self.handles = [None; MAX_HANDLES as usize];
                Ok(())
            }
            Op::Reformat => {
                self.model = Model::new(now, TreePtr::<Node>::root().id());
                self.model.key_slots[0] = self.profile.encrypted.then(|| FIRST_PASSWORD.to_string());
                self.handles = [None; MAX_HANDLES as usize];
                Ok(())
            }
            Op::Crash { .. } | Op::ReadError { .. } | Op::Enumerate { .. } => Ok(()),
        };
        assert!(result.is_ok(), "model rejected an operation it had accepted");
    }

    fn handle_ptr(&self, target: &Target) -> Option<TreePtr<Node>> {
        match target {
            Target::Handle(slot) => self.handles[*slot as usize].map(|handle| TreePtr::new(handle.fs)),
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
                let count = self.open_counts.entry(ptr.id()).or_insert(0);
                *count += 1;
                if *count == 1 {
                    self.fs().tx(|tx| tx.on_open_node(ptr))?;
                }
                Ok(Some(ptr.id()))
            }
            Op::Close(slot) => {
                self.fs().disk.disarm_read_error();
                let fs_id = self.handles[*slot as usize].expect("open handle").fs;
                let count = self.open_counts.get_mut(&fs_id).expect("open count");
                *count -= 1;
                if *count == 0 {
                    self.open_counts.remove(&fs_id);
                    self.fs().tx(|tx| tx.on_close_node(TreePtr::new(fs_id)))?;
                }
                Ok(None)
            }
            Op::SetKeySlot { slot, password } => {
                let current = self.password.clone().expect("encrypted file system password");
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
                self.fs().disk.disarm_read_error();
                self.handles = [None; MAX_HANDLES as usize];
                let open: Vec<u32> = self.open_counts.keys().copied().collect();
                self.open_counts.clear();
                for fs_id in open {
                    self.fs().tx(|tx| tx.on_close_node(TreePtr::new(fs_id)))?;
                    self.refresh_guard();
                }
                self.fs().cleanup()?;
                self.fs().disk.flush()?;
                self.refresh_guard();
                let fs = self.fs.take().expect("mounted file system");
                let password = self.password.clone();
                let mut fs = match mount(fs.disk, password.as_deref()) {
                    Ok(fs) => fs,
                    Err(err) => {
                        self.fatal = Some(format!(
                            "remount: the file system does not mount after a clean unmount: {err}"
                        ));
                        return Err(err);
                    }
                };
                fs.disk.guard = Some(Guard::new(fs.allocator().clone(), fs.header.generation()));
                self.fs = Some(fs);
                let cleaned = self.fs().cleanup();
                let barriers = self.profile.barriers;
                self.fs().barriers = barriers;
                cleaned?;
                self.durable = true;
                Ok(None)
            }
            Op::Reformat => {
                self.fs().disk.disarm_read_error();
                self.handles = [None; MAX_HANDLES as usize];
                self.open_counts.clear();
                let fs = self.fs.take().expect("mounted file system");
                let mut disk = fs.disk;
                disk.guard = None;
                let password = self.profile.encrypted.then(|| FIRST_PASSWORD.to_string());
                match FileSystem::create(disk, password.as_deref().map(str::as_bytes), now.0, now.1) {
                    Ok(mut fs) => {
                        fs.disk.guard = Some(Guard::new(fs.allocator().clone(), fs.header.generation()));
                        fs.barriers = self.profile.barriers;
                        self.durable = true;
                        self.fs = Some(fs);
                        self.password = password;
                        Ok(Some(TreePtr::<Node>::root().id()))
                    }
                    Err(err) => {
                        self.lost = disk::take_lost();
                        if self.lost.is_none() {
                            self.fatal = Some(format!("reformat failed without a power loss: {err}"));
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
            .is_none_or(|guard| guard.free_generation != generation);
        if stale {
            let free = fs.allocator().clone();
            fs.disk.guard = Some(Guard::new(free, generation));
        }
    }

    fn take_violations(&mut self) -> std::result::Result<(), String> {
        let fs = self.fs.as_mut().expect("mounted file system");
        if fs.disk.violations.is_empty() {
            return Ok(());
        }
        Err(fs.disk.violations.join("; "))
    }

    fn sync_passwords(&mut self) {
        if !self.profile.encrypted {
            return;
        }
        let valid = self
            .password
            .as_ref()
            .is_some_and(|password| self.model.key_slots.iter().any(|slot| slot.as_ref() == Some(password)));
        if !valid {
            self.password = self.model.key_slots.iter().flatten().next().cloned();
        }
    }

    pub fn step(&mut self, step: &Step) -> std::result::Result<(), String> {
        match &step.op {
            Op::Crash { after, kind, salt } => {
                self.pending_crash = Some((*after, *kind, *salt));
                return Ok(());
            }
            Op::ReadError { nth } => {
                self.pending_read_error = Some(*nth);
                return Ok(());
            }
            Op::Enumerate { salt } => {
                if !self.profile.encrypted {
                    self.pending_enumerate = Some(*salt);
                }
                return Ok(());
            }
            _ => {}
        }
        self.now = step.time;
        let op = &step.op;
        if !self.applicable(op) {
            self.pending_crash = None;
            self.pending_read_error = None;
            self.pending_enumerate = None;
            return Ok(());
        }
        self.stats.ops += 1;
        let crash = self.pending_crash.take();
        let read_error = self.pending_read_error.take();
        let enumerate = self.pending_enumerate.take();
        let before = (crash.is_some() || enumerate.is_some()).then(|| self.model.clone());
        let data = match op {
            Op::Write { len, pattern, .. } => pattern.bytes(*len as usize),
            _ => Vec::new(),
        };
        let expected = self.expected(op);
        {
            let fs = self.fs();
            if let Some((after, _, _)) = crash {
                fs.disk.arm_crash(u64::from(after));
            }
            if let Some(nth) = read_error {
                fs.disk.arm_read_error(u64::from(nth));
            }
            if enumerate.is_some() {
                fs.disk.start_recording();
            }
        }
        let read_errors_before = self.fs().disk.read_errors;
        let writes_before = self.fs().disk.writes;
        let generation_before = self.fs().header.generation();
        let uuid_before = self.fs().header.uuid();
        let actual = self.run_fs(op, &data).map_err(errno);
        if let Some(fatal) = self.fatal.take() {
            return Err(fatal);
        }
        let (crashed, read_error_fired) = match self.fs.as_mut() {
            Some(fs) => {
                fs.disk.disarm_crash();
                fs.disk.disarm_read_error();
                (fs.disk.crashed, fs.disk.read_errors != read_errors_before)
            }
            None => (true, false),
        };
        if read_error_fired {
            self.stats.read_errors += 1;
        }
        if crashed {
            let before = before.expect("model before a crash");
            let mut after = before.clone();
            if expected.is_ok() {
                std::mem::swap(&mut self.model, &mut after);
                self.apply(op, &data, None);
                std::mem::swap(&mut self.model, &mut after);
            }
            let (_, kind, salt) = crash.expect("armed crash");
            return self.crash(kind, salt, vec![before, after], op);
        }
        let outcome = self.judge(op, expected, actual.clone(), read_error_fired)?;
        let wrote = self.fs().disk.writes - writes_before;
        let refused = !outcome && expected.err().is_some() && expected.err() == actual.err();
        let unchanged = matches!(op, Op::Fsync(_)) && actual.is_ok();
        if wrote != 0 && (refused || unchanged) {
            return Err(format!(
                "{step}: wrote {wrote} blocks although it changed nothing"
            ));
        }
        if outcome || matches!(op, Op::Remount) {
            let created = match (op, &actual) {
                (Op::Create(_) | Op::Mkdir(_), Ok(created)) => *created,
                _ => None,
            };
            self.apply(op, &data, created);
        } else {
            self.touched.clear();
        }
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
        if let Some(salt) = enumerate {
            let recording = self.fs().disk.stop_recording().expect("recording");
            self.enumerate(&recording, salt, before.as_ref().expect("model before"), op)?;
        }
        self.refresh_guard();
        if let Some((_, kind, salt)) = crash {
            let model = self.model.clone();
            return self.crash(kind, salt, vec![model], op);
        }
        let generation_after = self.fs().header.generation();
        let threshold = generation_after / ALLOC_GC_THRESHOLD * ALLOC_GC_THRESHOLD;
        let squashed = (outcome && matches!(op, Op::Remount | Op::Reformat))
            || (threshold >= ALLOC_GC_THRESHOLD && generation_before <= threshold && threshold < generation_after);
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
        if read_error_fired && actual.is_err() {
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
        kind: CrashKind,
        salt: u64,
        candidates: Vec<Model>,
        op: &Op,
    ) -> std::result::Result<(), String> {
        self.stats.crashes += 1;
        let lost = match self.fs.take() {
            Some(fs) => fs.disk.into_lost(),
            None => self.lost.take().expect("disk lost in a power loss"),
        };
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
                let after = candidates.into_iter().last().expect("model after the reformat");
                return self.format_lost_disk(image, after);
            }
            return Err(format!(
                "{context}: the file system does not mount: {}",
                last_error.map_or("no password".to_string(), |err| err.to_string())
            ));
        };
        self.stats.crash_mounts += 1;
        let snapshot = check::snapshot(&mut fs).map_err(|err| format!("{context}: walk failed: {err}"))?;
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
                    match check::compare_contents(&mut fs, &snapshot, candidate, &matching, &mut all) {
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
        self.handles = [None; MAX_HANDLES as usize];
        self.open_counts.clear();
        self.password = password;
        self.sync_passwords();
        fs.disk.guard = Some(Guard::new(fs.allocator().clone(), fs.header.generation()));
        fs.cleanup()
            .map_err(|err| format!("{context}: cleanup after mount failed: {err}"))?;
        self.fs = Some(fs);
        self.take_violations()
            .map_err(|err| format!("{context}: cleanup: {err}"))?;
        self.refresh_guard();
        self.check_durable(true, true)
            .map_err(|err| format!("{context}: after cleanup: {err}"))
    }

    fn format_lost_disk(&mut self, image: Vec<u8>, model: Model) -> std::result::Result<(), String> {
        let now = self.now;
        let password = self.profile.encrypted.then(|| FIRST_PASSWORD.to_string());
        let disk = SimDisk::new(image, self.profile.durability);
        let mut fs = FileSystem::create(disk, password.as_deref().map(str::as_bytes), now.0, now.1)
            .map_err(|err| format!("formatting a disk left unformatted by a power loss failed: {err}"))?;
        fs.disk.guard = Some(Guard::new(fs.allocator().clone(), fs.header.generation()));
        self.fs = Some(fs);
        self.model = model;
        self.handles = [None; MAX_HANDLES as usize];
        self.open_counts.clear();
        self.password = password;
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
                    return Err(format!("{}: the file system does not mount: {err}", boundary.label));
                }
            };
            mounted.read_only = true;
            let snapshot = check::snapshot(&mut mounted)
                .map_err(|err| format!("{}: walk failed: {err}", boundary.label))?;
            if !snapshot.problems.is_empty() {
                return Err(format!("{}: {}", boundary.label, snapshot.problems.join("; ")));
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
        let durable = std::mem::take(&mut self.durable);
        let live_only = if self.profile.barriers {
            let unflushed = self.fs.as_ref().expect("mounted file system").disk.unflushed();
            if unflushed != 0 {
                return Err(format!(
                    "the operation returned with {unflushed} writes still in the volatile cache"
                ));
            }
            let every = if self.profile.encrypted {
                ENCRYPTED_MOUNT_EVERY
            } else {
                MOUNT_EVERY
            };
            !full && !squashed && self.stats.ops % every != 0
        } else {
            !durable
        };
        if live_only {
            let model = &self.model;
            let fs = self.fs.as_mut().expect("mounted file system");
            let writes = fs.disk.writes;
            let snapshot = check::snapshot(fs).map_err(|err| format!("live walk failed: {err}"))?;
            verify(fs, &snapshot, model, full, &touched)?;
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
        let live_free = disk::free_ranges(fs.allocator());
        let live_layout = fs.allocator().levels().clone();
        let view = View::new(fs.disk.media(), None, BTreeMap::new());
        let mut mounted = mount(view, password.as_deref())
            .map_err(|err| format!("the durable state does not mount: {err}"))?;
        mounted.read_only = true;
        let snapshot = check::snapshot(&mut mounted)
            .map_err(|err| format!("durable walk failed: {err}"))?;
        if snapshot.generation != live_generation {
            return Err(format!(
                "the disk holds generation {} after the file system committed generation {live_generation}",
                snapshot.generation
            ));
        }
        let rebuilt_free = disk::free_ranges(mounted.allocator());
        if rebuilt_free != live_free {
            let only_live: Vec<_> = live_free.iter().filter(|range| !rebuilt_free.contains(range)).take(4).collect();
            let only_rebuilt: Vec<_> = rebuilt_free.iter().filter(|range| !live_free.contains(range)).take(4).collect();
            return Err(format!(
                "the free set rebuilt from the allocator log differs from the running allocator: running only {only_live:?}, rebuilt only {only_rebuilt:?}"
            ));
        }
        if mounted.allocator().levels() != &live_layout {
            self.stats.layout_differences += 1;
        }
        if squashed {
            let entries: usize = mounted.allocator().levels().iter().map(|level| level.len()).sum();
            let bound = (entries + SQUASH_SLACK_ENTRIES).div_ceil(ALLOC_LIST_ENTRIES) + 2;
            if snapshot.alloc_blocks > bound {
                return Err(format!(
                    "the allocator log keeps {} blocks after a squash, more than {bound}",
                    snapshot.alloc_blocks
                ));
            }
        }
        verify(&mut mounted, &snapshot, &self.model, full, &touched)?;
        if mounted.disk.refused_writes != 0 {
            return Err(format!(
                "a read-only mount tried {} writes",
                mounted.disk.refused_writes
            ));
        }
        Ok(())
    }

    pub fn finish(&mut self) -> std::result::Result<(), String> {
        if !self.profile.barriers {
            self.fs()
                .disk
                .flush()
                .map_err(|err| format!("the flush at unmount failed: {err}"))?;
            self.durable = true;
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

fn key_slots_match(header: &Header, slots: &[Option<String>]) -> std::result::Result<(), String> {
    let stored = header.key_slots;
    for (index, slot) in slots.iter().enumerate() {
        match slot {
            None => {
                if !stored[index].is_empty() {
                    return Err(format!("key slot {index} should be empty"));
                }
            }
            Some(password) => {
                if stored[index].is_empty() {
                    return Err(format!("key slot {index} should hold {password}"));
                }
                match header.unlock(password.as_bytes()) {
                    Some((found, _)) if slots[found].as_deref() == Some(password.as_str()) => {}
                    _ => return Err(format!("{password} does not open key slot {index}")),
                }
            }
        }
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
) -> std::result::Result<(), String> {
    if !snapshot.problems.is_empty() {
        return Err(snapshot.problems.join("; "));
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
    let text = Step { time: (0, 0), op: op.clone() }.to_string();
    text.split_once(' ').map(|(_, rest)| rest.to_string()).unwrap_or(text)
}

use std::collections::{BTreeMap, BTreeSet};
use std::ops::{Deref, DerefMut};

use redoxfs::{
    AllocList, BlockLevel, BlockList, BlockPtr, BlockRaw, BlockTrait, DirList, Disk, FileSystem,
    HEADER_RING, Node, RecordRaw, ReleaseList, Transaction, Tree, TreeList, TreePtr,
};
use syscall::error::{EIO, Error, Result};

use crate::model::{Kind, MNode, Model, NodeId, ROOT, Time};

pub const MAX_CHAIN: usize = 1 << 16;
pub const MAX_NODES: usize = 1 << 16;
pub const MAX_HTREE_LEVELS: usize = 8;
const TREE_LIST_ENTRIES: usize = 254;
const LIST_ENTRIES: u64 = 256;
const LEVEL0_RECORDS: usize = 128;
const HTREE_ENTRY_BYTES: usize = 20;
const HTREE_ENTRIES: usize = 4096 / HTREE_ENTRY_BYTES;

#[derive(Clone, Copy)]
#[repr(C, packed)]
struct HTreeEntry {
    hash: [u8; 4],
    ptr: BlockPtr<BlockRaw>,
}

#[repr(C, packed)]
struct HTreeBlock {
    entries: [HTreeEntry; HTREE_ENTRIES],
    padding: [u8; 4096 - HTREE_ENTRIES * HTREE_ENTRY_BYTES],
}

unsafe impl BlockTrait for HTreeBlock {
    fn empty(level: BlockLevel) -> Option<Self> {
        if level.bytes() != 4096 {
            return None;
        }
        Some(HTreeBlock {
            entries: [HTreeEntry {
                hash: [0; 4],
                ptr: BlockPtr::default(),
            }; HTREE_ENTRIES],
            padding: [0; 4096 - HTREE_ENTRIES * HTREE_ENTRY_BYTES],
        })
    }
}

impl Deref for HTreeBlock {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self as *const HTreeBlock as *const u8, 4096) }
    }
}

impl DerefMut for HTreeBlock {
    fn deref_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self as *mut HTreeBlock as *mut u8, 4096) }
    }
}

fn level_number(level: BlockLevel) -> u32 {
    level.blocks::<u64>().trailing_zeros()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SKind {
    File,
    Dir,
    Other(u16),
}

pub struct SNode {
    pub kind: SKind,
    pub size: u64,
    pub links: u32,
    pub mtime: Time,
    pub atime: Time,
    pub entries: Vec<(String, u32)>,
    pub inline: Option<Vec<u8>>,
    pub records: Vec<(u64, BlockPtr<RecordRaw>)>,
    pub record_bytes: u64,
}

pub struct Snapshot {
    pub generation: u64,
    pub root: u32,
    pub nodes: BTreeMap<u32, SNode>,
    pub release: BTreeSet<u32>,
    pub problems: Vec<String>,
    pub alloc_blocks: usize,
}

struct Walker {
    total: u64,
    used: Vec<u8>,
    problems: Vec<String>,
}

const USED_TREE: u8 = 1;
const USED_NODE: u8 = 2;
const USED_DATA: u8 = 3;
const USED_ALLOC: u8 = 4;
const USED_RELEASE: u8 = 5;

fn use_name(kind: u8) -> &'static str {
    match kind {
        USED_TREE => "tree",
        USED_NODE => "node",
        USED_DATA => "data",
        USED_ALLOC => "allocator log",
        USED_RELEASE => "release list",
        _ => "unknown",
    }
}

impl Walker {
    fn mark<T>(&mut self, ptr: BlockPtr<T>, kind: u8, owner: &str) -> u64 {
        let addr = ptr.addr();
        let count = addr.level().blocks::<u64>();
        for block in addr.index()..addr.index() + count {
            if block <= HEADER_RING || block >= self.total {
                self.problems.push(format!(
                    "{owner} points at block {block}, outside the allocatable range"
                ));
                continue;
            }
            let previous = self.used[block as usize];
            if previous != 0 {
                self.problems.push(format!(
                    "block {block} is used twice: {} and {} ({owner})",
                    use_name(previous),
                    use_name(kind)
                ));
            }
            self.used[block as usize] = kind;
        }
        count
    }
}

fn list_ptrs<D: Disk>(
    tx: &mut Transaction<D>,
    ptr: BlockPtr<BlockRaw>,
) -> Result<[BlockPtr<BlockRaw>; 256]> {
    let list = tx.read_block::<BlockList<BlockRaw>>(unsafe { ptr.cast() })?;
    Ok(list.data().ptrs)
}

fn walk_file<D: Disk>(
    tx: &mut Transaction<D>,
    walker: &mut Walker,
    id: u32,
    node: &Node,
    snode: &mut SNode,
) -> Result<u64> {
    let mut blocks = 0;
    let level_data = node.level_data().expect("file without inline data");
    let owner = format!("file node {id}");
    let mut stack: Vec<(BlockPtr<BlockRaw>, u32, u64)> = Vec::new();
    let level0 = level_data.level0;
    for (index, ptr) in level0.iter().enumerate() {
        if !ptr.is_null() {
            stack.push((unsafe { ptr.cast() }, 0, index as u64));
        }
    }
    let mut base = LEVEL0_RECORDS as u64;
    let level1 = level_data.level1;
    let level2 = level_data.level2;
    let level3 = level_data.level3;
    let level4 = level_data.level4;
    let tables: [(&[BlockPtr<BlockRaw>], u32); 4] = [
        (
            unsafe { std::mem::transmute::<&[_], &[BlockPtr<BlockRaw>]>(&level1[..]) },
            1,
        ),
        (
            unsafe { std::mem::transmute::<&[_], &[BlockPtr<BlockRaw>]>(&level2[..]) },
            2,
        ),
        (
            unsafe { std::mem::transmute::<&[_], &[BlockPtr<BlockRaw>]>(&level3[..]) },
            3,
        ),
        (
            unsafe { std::mem::transmute::<&[_], &[BlockPtr<BlockRaw>]>(&level4[..]) },
            4,
        ),
    ];
    for (table, depth) in tables {
        let span = LIST_ENTRIES.pow(depth);
        for (index, ptr) in table.iter().enumerate() {
            if !ptr.is_null() {
                stack.push((*ptr, depth, base + index as u64 * span));
            }
        }
        base += table.len() as u64 * span;
    }
    while let Some((ptr, depth, first)) = stack.pop() {
        if depth == 0 {
            blocks += walker.mark(ptr, USED_DATA, &owner);
            snode.records.push((first, unsafe { ptr.cast() }));
            continue;
        }
        blocks += walker.mark(ptr, USED_DATA, &owner);
        let ptrs = list_ptrs(tx, ptr)?;
        let span = LIST_ENTRIES.pow(depth - 1);
        let mut empty = true;
        for (index, child) in ptrs.iter().enumerate() {
            if !child.is_null() {
                empty = false;
                stack.push((*child, depth - 1, first + index as u64 * span));
            }
        }
        if empty {
            walker
                .problems
                .push(format!("{owner} keeps an empty index block"));
        }
    }
    snode.records.sort_by_key(|(index, _)| *index);
    let record_count = snode.size.div_ceil(snode.record_bytes);
    for (index, _) in snode.records.iter() {
        if *index >= record_count {
            walker.problems.push(format!(
                "{owner} keeps record {index} beyond its size {}",
                snode.size
            ));
        }
    }
    Ok(blocks)
}

fn walk_dir<D: Disk>(
    tx: &mut Transaction<D>,
    walker: &mut Walker,
    id: u32,
    node: &Node,
    snode: &mut SNode,
) -> Result<u64> {
    let owner = format!("directory node {id}");
    let level_data = node.level_data().expect("directory with inline data");
    let level0 = level_data.level0;
    let mut blocks = 0;
    let first_extra = if level0[0].is_marker() { 2 } else { 0 };
    let mut stray = level0[first_extra..].iter().any(|ptr| !ptr.is_null());
    stray |= level_data.level1.iter().any(|ptr| !ptr.is_null());
    stray |= level_data.level2.iter().any(|ptr| !ptr.is_null());
    stray |= level_data.level3.iter().any(|ptr| !ptr.is_null());
    stray |= level_data.level4.iter().any(|ptr| !ptr.is_null());
    if stray {
        walker
            .problems
            .push(format!("{owner} has pointers outside its directory index"));
    }
    if !level0[0].is_marker() {
        if !level0[1].is_null() {
            walker
                .problems
                .push(format!("{owner} has a directory list without a marker"));
        }
        if snode.size != 0 {
            walker
                .problems
                .push(format!("{owner} has no entries but size {}", snode.size));
        }
        return Ok(0);
    }
    let levels = level_number(level0[0].addr().level()) as usize;
    if levels > MAX_HTREE_LEVELS {
        walker
            .problems
            .push(format!("{owner} has {levels} index levels"));
        return Ok(0);
    }
    let mut stack: Vec<(BlockPtr<BlockRaw>, usize)> = vec![(unsafe { level0[1].cast() }, levels)];
    let mut dir_blocks = 0u64;
    while let Some((ptr, remaining)) = stack.pop() {
        if ptr.is_null() {
            walker
                .problems
                .push(format!("{owner} has a null directory index root"));
            continue;
        }
        blocks += walker.mark(ptr, USED_DATA, &owner);
        dir_blocks += 1;
        if remaining == 0 {
            let list = tx.read_block::<DirList>(unsafe { ptr.cast() })?;
            if list.data().is_empty() {
                walker
                    .problems
                    .push(format!("{owner} keeps an empty directory list"));
            }
            for entry in list.data().entries() {
                match entry.name() {
                    Some(name) => snode
                        .entries
                        .push((name.to_string(), entry.node_ptr().id())),
                    None => walker
                        .problems
                        .push(format!("{owner} has an entry whose name is not UTF-8")),
                }
            }
        } else {
            let block = tx.read_block::<HTreeBlock>(unsafe { ptr.cast() })?;
            let entries = block.data().entries;
            let mut any = false;
            for entry in entries.iter() {
                let child = entry.ptr;
                if !child.is_null() {
                    any = true;
                    stack.push((child, remaining - 1));
                }
            }
            if !any {
                walker
                    .problems
                    .push(format!("{owner} keeps an empty index block"));
            }
        }
    }
    if snode.size != dir_blocks * 4096 {
        walker.problems.push(format!(
            "{owner} has size {} but {dir_blocks} directory blocks",
            snode.size
        ));
    }
    Ok(blocks)
}

pub fn snapshot<D: Disk>(fs: &mut FileSystem<D>) -> Result<Snapshot> {
    let total = fs.header.size() / 4096;
    let mut walker = Walker {
        total,
        used: vec![0; total as usize],
        problems: Vec::new(),
    };
    let mut nodes = BTreeMap::new();
    let mut release = BTreeSet::new();
    let generation = fs.header.generation();
    let mut alloc_blocks = 0;
    fs.tx(|tx| {
        let header_tree = tx.header.tree;
        walker.mark(header_tree, USED_TREE, "header tree");
        let l3 = tx.read_block::<Tree>(header_tree)?;
        let l3_ptrs = l3.data().ptrs;
        for i3 in 0..TREE_LIST_ENTRIES {
            let p2 = l3_ptrs[i3];
            let full3 = l3.data().branch_is_full(i3);
            if p2.is_null() {
                if full3 {
                    walker
                        .problems
                        .push(format!("tree slot {i3} is marked full but empty"));
                }
                continue;
            }
            walker.mark(p2, USED_TREE, "tree level 2");
            let l2 = tx.read_block(p2)?;
            if full3 != l2.data().tree_list_is_full() {
                walker
                    .problems
                    .push(format!("tree slot {i3} has a wrong full flag"));
            }
            let l2_ptrs = l2.data().ptrs;
            for i2 in 0..TREE_LIST_ENTRIES {
                let p1 = l2_ptrs[i2];
                let full2 = l2.data().branch_is_full(i2);
                if p1.is_null() {
                    if full2 {
                        walker
                            .problems
                            .push(format!("tree slot {i3}.{i2} is marked full but empty"));
                    }
                    continue;
                }
                walker.mark(p1, USED_TREE, "tree level 1");
                let l1 = tx.read_block(p1)?;
                if full2 != l1.data().tree_list_is_full() {
                    walker
                        .problems
                        .push(format!("tree slot {i3}.{i2} has a wrong full flag"));
                }
                let l1_ptrs = l1.data().ptrs;
                for i1 in 0..TREE_LIST_ENTRIES {
                    let p0 = l1_ptrs[i1];
                    let full1 = l1.data().branch_is_full(i1);
                    if p0.is_null() {
                        if full1 {
                            walker
                                .problems
                                .push(format!("tree slot {i3}.{i2}.{i1} is marked full but empty"));
                        }
                        continue;
                    }
                    walker.mark(p0, USED_TREE, "tree level 0");
                    let l0: redoxfs::BlockData<TreeList<BlockRaw>> = tx.read_block(p0)?;
                    if full1 != l0.data().tree_list_is_full() {
                        walker
                            .problems
                            .push(format!("tree slot {i3}.{i2}.{i1} has a wrong full flag"));
                    }
                    let l0_ptrs = l0.data().ptrs;
                    for i0 in 0..TREE_LIST_ENTRIES {
                        let pn = l0_ptrs[i0];
                        let id = TreePtr::<Node>::from_indexes((i3, i2, i1, i0)).id();
                        let full0 = l0.data().branch_is_full(i0);
                        if pn.is_null() {
                            if full0 && id != 0 {
                                walker.problems.push(format!(
                                    "tree slot of node {id} is marked full but empty"
                                ));
                            }
                            continue;
                        }
                        if !full0 {
                            walker.problems.push(format!(
                                "tree slot of node {id} is used but not marked full"
                            ));
                        }
                        if nodes.len() >= MAX_NODES {
                            return Err(Error::new(EIO));
                        }
                        walker.mark(pn, USED_NODE, "node");
                        let block = tx.read_block::<Node>(unsafe { pn.cast() })?;
                        let node = block.data();
                        let mode = node.mode() & Node::MODE_TYPE;
                        let kind = match mode {
                            Node::MODE_FILE => SKind::File,
                            Node::MODE_DIR => SKind::Dir,
                            other => SKind::Other(other),
                        };
                        let mut snode = SNode {
                            kind,
                            size: node.size(),
                            links: node.links(),
                            mtime: node.mtime(),
                            atime: node.atime(),
                            entries: Vec::new(),
                            inline: None,
                            records: Vec::new(),
                            record_bytes: node.record_level().bytes(),
                        };
                        let mut counted = 1;
                        if let Some(inline) = node.inline_data() {
                            if kind == SKind::Dir {
                                walker
                                    .problems
                                    .push(format!("directory node {id} has inline data"));
                            }
                            snode.inline = Some(inline.to_vec());
                        } else if kind == SKind::Dir {
                            counted += walk_dir(tx, &mut walker, id, node, &mut snode)?;
                        } else {
                            counted += walk_file(tx, &mut walker, id, node, &mut snode)?;
                        }
                        if counted != node.blocks() {
                            walker.problems.push(format!(
                                "node {id} counts {} blocks but uses {counted}",
                                node.blocks()
                            ));
                        }
                        nodes.insert(id, snode);
                    }
                }
            }
        }
        let mut alloc_ptr = tx.header.alloc;
        let mut chain = 0;
        while !alloc_ptr.is_null() {
            chain += 1;
            if chain > MAX_CHAIN {
                walker
                    .problems
                    .push("allocator log is too long".to_string());
                break;
            }
            walker.mark(alloc_ptr, USED_ALLOC, "allocator log");
            let alloc = tx.read_block::<AllocList>(alloc_ptr)?;
            alloc_ptr = alloc.data().prev;
        }
        alloc_blocks = chain;
        let mut release_ptr = tx.header.release;
        chain = 0;
        while !release_ptr.is_null() {
            chain += 1;
            if chain > MAX_CHAIN {
                walker.problems.push("release list is too long".to_string());
                break;
            }
            walker.mark(release_ptr, USED_RELEASE, "release list");
            let list = tx.read_block::<ReleaseList>(release_ptr)?;
            let entries = list.data().entries;
            for entry in entries.iter() {
                if !entry.is_null() && !release.insert(entry.id()) {
                    walker
                        .problems
                        .push(format!("node {} is in the release list twice", entry.id()));
                }
            }
            release_ptr = list.data().prev;
        }
        Ok(())
    })?;
    let mut free = vec![false; total as usize];
    for (level, starts) in fs.allocator().levels().iter().enumerate() {
        let size = 1u64 << level;
        for &start in starts.iter() {
            if start % size != 0 {
                walker
                    .problems
                    .push(format!("free range {start}+{size} is not aligned"));
            }
            for block in start..start + size {
                if block <= HEADER_RING || block >= total {
                    walker.problems.push(format!(
                        "free block {block} is outside the allocatable range"
                    ));
                    continue;
                }
                if free[block as usize] {
                    walker.problems.push(format!("block {block} is free twice"));
                }
                if walker.used[block as usize] != 0 {
                    walker.problems.push(format!(
                        "block {block} is both free and used by the {}",
                        use_name(walker.used[block as usize])
                    ));
                }
                free[block as usize] = true;
            }
        }
    }
    let mut leaked = Vec::new();
    for block in HEADER_RING + 1..total {
        if walker.used[block as usize] == 0 && !free[block as usize] {
            leaked.push(block);
        }
    }
    if !leaked.is_empty() {
        walker.problems.push(format!(
            "{} blocks are neither free nor used, first {:?}",
            leaked.len(),
            &leaked[..leaked.len().min(8)]
        ));
    }
    Ok(Snapshot {
        generation,
        root: TreePtr::<Node>::root().id(),
        nodes,
        release,
        problems: walker.problems,
        alloc_blocks,
    })
}

pub struct Matching {
    pub ids: BTreeMap<NodeId, u32>,
}

fn kind_matches(model: Kind, snapshot: SKind) -> bool {
    matches!(
        (model, snapshot),
        (Kind::File, SKind::File) | (Kind::Dir, SKind::Dir)
    )
}

pub fn compare(
    snap: &Snapshot,
    model: &Model,
    released_allowed: bool,
) -> std::result::Result<Matching, String> {
    let mut ids: BTreeMap<NodeId, u32> = BTreeMap::new();
    let mut back: BTreeMap<u32, NodeId> = BTreeMap::new();
    let mut assign = |model_id: NodeId,
                      fs_id: u32,
                      node: &MNode|
     -> std::result::Result<bool, String> {
        if let Some(expected) = node.fs_id {
            if expected != fs_id {
                return Err(format!(
                    "model node {model_id} is file system node {expected} but the path names node {fs_id}"
                ));
            }
        }
        if let Some(&known) = ids.get(&model_id) {
            if known != fs_id {
                return Err(format!(
                    "model node {model_id} appears as file system nodes {known} and {fs_id}"
                ));
            }
            return Ok(false);
        }
        if let Some(&other) = back.get(&fs_id) {
            return Err(format!(
                "file system node {fs_id} stands for model nodes {other} and {model_id}"
            ));
        }
        ids.insert(model_id, fs_id);
        back.insert(fs_id, model_id);
        Ok(true)
    };
    assign(ROOT, snap.root, model.node(ROOT))?;
    let mut stack: Vec<(NodeId, u32, String)> = vec![(ROOT, snap.root, "/".to_string())];
    let mut visited = 0usize;
    while let Some((model_id, fs_id, path)) = stack.pop() {
        visited += 1;
        if visited > MAX_NODES {
            return Err("directory walk exceeds the node limit".to_string());
        }
        let m = model.node(model_id);
        let s = snap
            .nodes
            .get(&fs_id)
            .ok_or_else(|| format!("{path}: names node {fs_id}, which is not in the tree"))?;
        if !kind_matches(m.kind, s.kind) {
            return Err(format!("{path}: expected {:?}, found {:?}", m.kind, s.kind));
        }
        if let Some(mtime) = m.mtime {
            if mtime != s.mtime {
                return Err(format!("{path}: mtime {:?}, expected {:?}", s.mtime, mtime));
            }
        }
        if let Some(atime) = m.atime {
            if atime != s.atime {
                return Err(format!("{path}: atime {:?}, expected {:?}", s.atime, atime));
            }
        }
        match m.kind {
            Kind::File => {
                if m.size != s.size {
                    return Err(format!("{path}: size {}, expected {}", s.size, m.size));
                }
                if m.links() != s.links {
                    return Err(format!("{path}: {} links, expected {}", s.links, m.links()));
                }
            }
            Kind::Dir => {
                if s.links != 1 {
                    return Err(format!("{path}: directory has {} links", s.links));
                }
                let mut found: BTreeMap<&str, u32> = BTreeMap::new();
                for (name, id) in s.entries.iter() {
                    if found.insert(name.as_str(), *id).is_some() {
                        return Err(format!("{path}: entry {name} appears twice"));
                    }
                }
                let expected: BTreeSet<&str> = m.entries.keys().map(String::as_str).collect();
                let actual: BTreeSet<&str> = found.keys().copied().collect();
                if expected != actual {
                    let missing: Vec<_> = expected.difference(&actual).collect();
                    let extra: Vec<_> = actual.difference(&expected).collect();
                    return Err(format!(
                        "{path}: entries differ, missing {missing:?}, unexpected {extra:?}"
                    ));
                }
                for (name, &child) in m.entries.iter() {
                    let child_fs = found[name.as_str()];
                    if assign(child, child_fs, model.node(child))? {
                        let child_path = if path == "/" {
                            format!("/{name}")
                        } else {
                            format!("{path}/{name}")
                        };
                        stack.push((child, child_fs, child_path));
                    }
                }
            }
        }
    }
    for orphan in model.orphans() {
        let node = model.node(orphan);
        let Some(fs_id) = node.fs_id else {
            return Err(format!(
                "open unlinked model node {orphan} has no file system node"
            ));
        };
        if !snap.release.contains(&fs_id) {
            if (released_allowed || node.is_release_pending()) && !snap.nodes.contains_key(&fs_id) {
                continue;
            }
            return Err(format!(
                "open unlinked node {fs_id} is not in the release list"
            ));
        }
        let s = snap
            .nodes
            .get(&fs_id)
            .ok_or_else(|| format!("open unlinked node {fs_id} is not in the tree"))?;
        if s.links != 0 || s.size != node.size {
            return Err(format!(
                "open unlinked node {fs_id} has {} links and size {}, expected 0 and {}",
                s.links, s.size, node.size
            ));
        }
        assign(orphan, fs_id, node)?;
    }
    for fs_id in snap.release.iter() {
        if !back.contains_key(fs_id) {
            return Err(format!(
                "node {fs_id} is in the release list but nothing holds it open"
            ));
        }
    }
    for fs_id in snap.nodes.keys() {
        if !back.contains_key(fs_id) {
            return Err(format!(
                "node {fs_id} is in the tree but no directory names it"
            ));
        }
    }
    Ok(Matching { ids })
}

fn read_record<D: Disk>(tx: &mut Transaction<D>, ptr: BlockPtr<RecordRaw>) -> Result<Vec<u8>> {
    let record = tx.read_block::<RecordRaw>(ptr)?;
    match ptr.addr().decomp_level() {
        None => Ok(record.data().to_vec()),
        Some(level) => {
            let raw = record.data();
            let len = raw[0] as usize | ((raw[1] as usize) << 8);
            let mut out = vec![0; level.bytes() as usize];
            if 2 + len > raw.len() {
                return Err(Error::new(EIO));
            }
            lz4_flex::decompress_into(&raw[2..2 + len], &mut out).map_err(|_| Error::new(EIO))?;
            Ok(out)
        }
    }
}

pub fn compare_contents<D: Disk>(
    fs: &mut FileSystem<D>,
    snap: &Snapshot,
    model: &Model,
    matching: &Matching,
    which: &mut dyn Iterator<Item = NodeId>,
) -> std::result::Result<u64, String> {
    let mut checked = 0;
    let mut problem = None;
    let result = fs.tx(|tx| {
        for model_id in which {
            let Some(m) = model.nodes.get(&model_id) else {
                continue;
            };
            if m.kind != Kind::File {
                continue;
            }
            let Some(&fs_id) = matching.ids.get(&model_id) else {
                continue;
            };
            let s = &snap.nodes[&fs_id];
            checked += 1;
            if let Some(inline) = s.inline.as_ref() {
                let size = s.size as usize;
                let stored = size.min(inline.len());
                let mut expected = vec![0; stored];
                m.read(0, &mut expected);
                if inline[..stored] != expected[..] {
                    problem = Some(format!(
                        "node {fs_id}: inline content differs from the model"
                    ));
                    return Ok(());
                }
                if inline[stored..].iter().any(|&byte| byte != 0) {
                    problem = Some(format!(
                        "node {fs_id}: stale bytes after the end of its inline data"
                    ));
                    return Ok(());
                }
                let beyond = m.chunks.iter().any(|(&chunk, data)| {
                    let start = chunk as usize * crate::model::CHUNK;
                    let from = inline.len().saturating_sub(start).min(crate::model::CHUNK);
                    start + crate::model::CHUNK > inline.len()
                        && data[from..].iter().any(|&byte| byte != 0)
                });
                if beyond {
                    problem = Some(format!(
                        "node {fs_id}: model data past the inline area of a file stored inline"
                    ));
                    return Ok(());
                }
                continue;
            }
            let record_bytes = s.record_bytes;
            let mut indexes: BTreeSet<u64> = s.records.iter().map(|(index, _)| *index).collect();
            for &chunk in m.chunks.keys() {
                indexes.insert(chunk * crate::model::CHUNK as u64 / record_bytes);
            }
            let present: BTreeMap<u64, BlockPtr<RecordRaw>> = s.records.iter().copied().collect();
            for index in indexes {
                let start = index * record_bytes;
                if start >= s.size {
                    continue;
                }
                let in_file = (s.size - start).min(record_bytes) as usize;
                let mut expected = vec![0; in_file];
                m.read(start, &mut expected);
                let actual = match present.get(&index) {
                    Some(ptr) => read_record(tx, *ptr)?,
                    None => Vec::new(),
                };
                let shared = actual.len().min(in_file);
                if actual[..shared] != expected[..shared]
                    || expected[shared..].iter().any(|&byte| byte != 0)
                {
                    let first = (0..in_file)
                        .find(|&i| actual.get(i).copied().unwrap_or(0) != expected[i])
                        .unwrap_or(0);
                    problem = Some(format!(
                        "node {fs_id}: record {index} differs from the model at byte {}",
                        start + first as u64
                    ));
                    return Ok(());
                }
                if actual.len() > in_file && actual[in_file..].iter().any(|&byte| byte != 0) {
                    problem = Some(format!(
                        "node {fs_id}: stale bytes after the end of the file in record {index}"
                    ));
                    return Ok(());
                }
            }
        }
        Ok(())
    });
    if let Err(err) = result {
        return Err(format!("reading file contents failed: {err}"));
    }
    match problem {
        Some(problem) => Err(problem),
        None => Ok(checked),
    }
}

pub fn find_all<D: Disk>(
    fs: &mut FileSystem<D>,
    snap: &Snapshot,
) -> std::result::Result<(), String> {
    let mut problem = None;
    let result = fs.tx(|tx| {
        for (&id, node) in snap.nodes.iter() {
            if node.kind != SKind::Dir {
                continue;
            }
            for (name, child) in node.entries.iter() {
                match tx.find_node(TreePtr::new(id), name) {
                    Ok(found) if found.id() == *child => {}
                    Ok(found) => {
                        problem = Some(format!(
                            "looking up {name} in node {id} finds node {} instead of {child}",
                            found.id()
                        ));
                        return Ok(());
                    }
                    Err(err) => {
                        problem = Some(format!("looking up {name} in node {id} fails: {err}"));
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    });
    if let Err(err) = result {
        return Err(format!("directory lookups failed: {err}"));
    }
    match problem {
        Some(problem) => Err(problem),
        None => Ok(()),
    }
}

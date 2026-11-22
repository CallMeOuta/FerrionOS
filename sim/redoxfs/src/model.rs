use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::rc::Rc;

use syscall::error::{EACCES, EEXIST, EINVAL, EISDIR, ENOENT, ENOTDIR, ENOTEMPTY};

pub type NodeId = u64;
pub type Time = (u64, u32);
pub type Errno = i32;

pub const ROOT: NodeId = 0;
pub const CHUNK: usize = 4096;
pub const KEY_SLOTS: usize = 64;
pub const NAME_MAX: usize = 252;
pub const MAX_DEPTH: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Life {
    Linked { links: NonZeroU32, open: u32 },
    OpenUnlinked { open: NonZeroU32 },
    ReleasePending,
}

#[derive(Clone)]
pub struct MNode {
    pub kind: Kind,
    pub size: u64,
    pub chunks: BTreeMap<u64, Rc<[u8; CHUNK]>>,
    pub entries: BTreeMap<String, NodeId>,
    pub mtime: Option<Time>,
    pub atime: Option<Time>,
    pub life: Life,
    pub fs_id: Option<u32>,
}

impl MNode {
    fn new(kind: Kind, now: Time) -> MNode {
        MNode {
            kind,
            size: 0,
            chunks: BTreeMap::new(),
            entries: BTreeMap::new(),
            mtime: Some(now),
            atime: Some(now),
            life: Life::Linked {
                links: NonZeroU32::new(1).expect("nonzero link"),
                open: 0,
            },
            fs_id: None,
        }
    }

    pub fn links(&self) -> u32 {
        match self.life {
            Life::Linked { links, .. } => links.get(),
            Life::OpenUnlinked { .. } | Life::ReleasePending => 0,
        }
    }

    pub fn open_count(&self) -> u32 {
        match self.life {
            Life::Linked { open, .. } => open,
            Life::OpenUnlinked { open } => open.get(),
            Life::ReleasePending => 0,
        }
    }

    pub fn is_release_pending(&self) -> bool {
        matches!(self.life, Life::ReleasePending)
    }

    pub fn read(&self, offset: u64, out: &mut [u8]) {
        assert!(offset + out.len() as u64 <= self.size.max(offset));
        out.fill(0);
        let mut done = 0usize;
        while done < out.len() {
            let position = offset + done as u64;
            let chunk = position / CHUNK as u64;
            let within = (position % CHUNK as u64) as usize;
            let len = (CHUNK - within).min(out.len() - done);
            if let Some(data) = self.chunks.get(&chunk) {
                out[done..done + len].copy_from_slice(&data[within..within + len]);
            }
            done += len;
        }
    }

    fn write(&mut self, offset: u64, data: &[u8]) {
        let mut done = 0usize;
        while done < data.len() {
            let position = offset + done as u64;
            let chunk = position / CHUNK as u64;
            let within = (position % CHUNK as u64) as usize;
            let len = (CHUNK - within).min(data.len() - done);
            let entry = self
                .chunks
                .entry(chunk)
                .or_insert_with(|| Rc::new([0; CHUNK]));
            Rc::make_mut(entry)[within..within + len].copy_from_slice(&data[done..done + len]);
            done += len;
        }
        self.size = self.size.max(offset + data.len() as u64);
    }

    fn truncate(&mut self, len: u64) {
        if len < self.size {
            let keep = len.div_ceil(CHUNK as u64);
            self.chunks.retain(|&chunk, _| chunk < keep);
            let within = (len % CHUNK as u64) as usize;
            if within != 0 {
                if let Some(entry) = self.chunks.get_mut(&(len / CHUNK as u64)) {
                    Rc::make_mut(entry)[within..].fill(0);
                }
            }
        }
        self.size = len;
    }
}

#[derive(Clone)]
pub struct Model {
    pub nodes: BTreeMap<NodeId, MNode>,
    next: NodeId,
    pub key_slots: Vec<Option<String>>,
}

pub type Path = Vec<String>;

pub fn show(path: &[String]) -> String {
    let mut text = String::new();
    for part in path {
        text.push('/');
        text.push_str(part);
    }
    if text.is_empty() {
        text.push('/');
    }
    text
}

pub fn parse_path(text: &str) -> Path {
    text.split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

impl Model {
    pub fn new(now: Time, root_fs_id: u32) -> Model {
        let mut root = MNode::new(Kind::Dir, now);
        root.fs_id = Some(root_fs_id);
        let mut nodes = BTreeMap::new();
        nodes.insert(ROOT, root);
        Model {
            nodes,
            next: ROOT + 1,
            key_slots: vec![None; KEY_SLOTS],
        }
    }

    pub fn node(&self, id: NodeId) -> &MNode {
        &self.nodes[&id]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut MNode {
        self.nodes.get_mut(&id).expect("model node")
    }

    pub fn lookup(&self, path: &[String]) -> Result<NodeId, Errno> {
        assert!(path.len() <= MAX_DEPTH);
        let mut current = ROOT;
        for part in path {
            let node = self.node(current);
            if node.kind != Kind::Dir {
                return Err(ENOTDIR);
            }
            current = *node.entries.get(part).ok_or(ENOENT)?;
        }
        Ok(current)
    }

    pub fn parent(&self, path: &[String]) -> Result<(NodeId, String), Errno> {
        let (name, parent_path) = path.split_last().ok_or(EINVAL)?;
        let parent = self.lookup(parent_path)?;
        if self.node(parent).kind != Kind::Dir {
            return Err(ENOTDIR);
        }
        Ok((parent, name.clone()))
    }

    fn drop_link(&mut self, id: NodeId) {
        let next = match self.node(id).life {
            Life::Linked { links, open } => match NonZeroU32::new(links.get() - 1) {
                Some(links) => Some(Life::Linked { links, open }),
                None => NonZeroU32::new(open).map(|open| Life::OpenUnlinked { open }),
            },
            Life::OpenUnlinked { .. } | Life::ReleasePending => {
                panic!("drop_link on unlinked node {id}");
            }
        };
        match next {
            Some(life) => self.node_mut(id).life = life,
            None => {
                self.nodes.remove(&id);
            }
        }
    }

    pub fn is_within(&self, ancestor: NodeId, target: NodeId) -> bool {
        let mut stack = vec![ancestor];
        let mut visited = BTreeSet::new();
        while let Some(current) = stack.pop() {
            if current == target {
                return true;
            }
            if !visited.insert(current) {
                continue;
            }
            for &child in self.node(current).entries.values() {
                if self.node(child).kind == Kind::Dir {
                    stack.push(child);
                }
            }
        }
        false
    }

    pub fn check_create(&self, path: &[String]) -> Result<(NodeId, String), Errno> {
        let (parent, name) = self.parent(path)?;
        if self.node(parent).entries.contains_key(&name) {
            return Err(EEXIST);
        }
        if name.contains(':') || name.len() > NAME_MAX {
            return Err(EINVAL);
        }
        Ok((parent, name))
    }

    pub fn create(&mut self, path: &[String], kind: Kind, now: Time) -> Result<NodeId, Errno> {
        let (parent, name) = self.check_create(path)?;
        let id = self.next;
        self.next += 1;
        self.nodes.insert(id, MNode::new(kind, now));
        let parent_node = self.node_mut(parent);
        parent_node.entries.insert(name, id);
        parent_node.mtime = Some(now);
        Ok(id)
    }

    pub fn file_target(&self, path: &[String]) -> Result<NodeId, Errno> {
        let id = self.lookup(path)?;
        if self.node(id).kind == Kind::Dir {
            return Err(EISDIR);
        }
        Ok(id)
    }

    pub fn write(&mut self, id: NodeId, offset: u64, data: &[u8]) {
        let node = self.node_mut(id);
        assert_eq!(node.kind, Kind::File);
        node.write(offset, data);
        node.mtime = None;
    }

    pub fn truncate(&mut self, id: NodeId, len: u64) {
        let node = self.node_mut(id);
        assert_eq!(node.kind, Kind::File);
        if node.size != len {
            node.mtime = None;
        }
        node.truncate(len);
    }

    pub fn check_unlink(
        &self,
        path: &[String],
        dir: bool,
    ) -> Result<(NodeId, String, NodeId), Errno> {
        let child = self.lookup(path)?;
        let (parent, name) = self.parent(path)?;
        let node = self.node(child);
        if dir {
            if node.kind != Kind::Dir {
                return Err(ENOTDIR);
            }
            if !node.entries.is_empty() {
                return Err(ENOTEMPTY);
            }
        } else if node.kind == Kind::Dir {
            return Err(EISDIR);
        }
        Ok((parent, name, child))
    }

    pub fn unlink(&mut self, path: &[String], dir: bool, now: Time) -> Result<(), Errno> {
        let (parent, name, child) = self.check_unlink(path, dir)?;
        let parent_node = self.node_mut(parent);
        parent_node.entries.remove(&name);
        parent_node.mtime = Some(now);
        self.drop_link(child);
        Ok(())
    }

    pub fn check_rename(
        &self,
        from: &[String],
        to: &[String],
        noreplace: bool,
    ) -> Result<(NodeId, String, NodeId, NodeId, String, Option<NodeId>), Errno> {
        let orig = self.lookup(from)?;
        let (orig_parent, orig_name) = self.parent(from)?;
        let (new_parent, new_name) = self.parent(to)?;
        let existing = self.node(new_parent).entries.get(&new_name).copied();
        if noreplace && existing.is_some() {
            return Err(EEXIST);
        }
        let orig_kind = self.node(orig).kind;
        if let Some(target) = existing {
            let target_node = self.node(target);
            if target_node.kind == Kind::Dir {
                if orig_kind != Kind::Dir {
                    return Err(EISDIR);
                }
                if !target_node.entries.is_empty() {
                    return Err(ENOTEMPTY);
                }
            } else if orig_kind == Kind::Dir {
                return Err(ENOTDIR);
            }
        }
        if orig_kind == Kind::Dir && self.is_within(orig, new_parent) {
            return Err(EINVAL);
        }
        if existing.is_none() && (new_name.contains(':') || new_name.len() > NAME_MAX) {
            return Err(EINVAL);
        }
        Ok((orig, orig_name, orig_parent, new_parent, new_name, existing))
    }

    pub fn rename(
        &mut self,
        from: &[String],
        to: &[String],
        noreplace: bool,
        now: Time,
    ) -> Result<(), Errno> {
        let (orig, orig_name, orig_parent, new_parent, new_name, existing) =
            self.check_rename(from, to, noreplace)?;
        if existing != Some(orig) {
            if let Some(target) = existing {
                self.node_mut(new_parent).entries.remove(&new_name);
                self.drop_link(target);
            }
            self.node_mut(new_parent).entries.insert(new_name, orig);
            self.node_mut(orig_parent).entries.remove(&orig_name);
        }
        self.node_mut(orig_parent).mtime = Some(now);
        self.node_mut(new_parent).mtime = Some(now);
        Ok(())
    }

    pub fn check_link(
        &self,
        from: &[String],
        to: &[String],
    ) -> Result<(NodeId, NodeId, String), Errno> {
        let orig = self.lookup(from)?;
        let (new_parent, new_name) = self.parent(to)?;
        let orig_kind = self.node(orig).kind;
        assert_eq!(orig_kind, Kind::File);
        if let Some(&target) = self.node(new_parent).entries.get(&new_name) {
            if self.node(target).kind == Kind::Dir {
                return Err(EACCES);
            }
            return Err(EEXIST);
        }
        if new_name.contains(':') || new_name.len() > NAME_MAX {
            return Err(EINVAL);
        }
        Ok((orig, new_parent, new_name))
    }

    pub fn link(&mut self, from: &[String], to: &[String], now: Time) -> Result<(), Errno> {
        let (orig, new_parent, new_name) = self.check_link(from, to)?;
        let parent_node = self.node_mut(new_parent);
        parent_node.entries.insert(new_name, orig);
        parent_node.mtime = Some(now);
        match &mut self.node_mut(orig).life {
            Life::Linked { links, .. } => {
                *links = NonZeroU32::new(links.get() + 1).expect("link count overflow");
            }
            Life::OpenUnlinked { .. } | Life::ReleasePending => {
                panic!("link on unlinked node {orig}");
            }
        }
        Ok(())
    }

    pub fn set_times(&mut self, id: NodeId, atime: Time, mtime: Time) {
        let node = self.node_mut(id);
        node.atime = Some(atime);
        node.mtime = Some(mtime);
    }

    pub fn open(&mut self, id: NodeId) {
        match &mut self.node_mut(id).life {
            Life::Linked { open, .. } => *open += 1,
            Life::OpenUnlinked { open } => {
                *open = NonZeroU32::new(open.get() + 1).expect("open count overflow");
            }
            Life::ReleasePending => panic!("open on release-pending node {id}"),
        }
    }

    pub fn close(&mut self, id: NodeId, released: bool) {
        let next = match self.node(id).life {
            Life::Linked { links, open } => {
                assert!(open > 0);
                Some(Life::Linked {
                    links,
                    open: open - 1,
                })
            }
            Life::OpenUnlinked { open } => match NonZeroU32::new(open.get() - 1) {
                Some(open) => Some(Life::OpenUnlinked { open }),
                None => (!released).then_some(Life::ReleasePending),
            },
            Life::ReleasePending => panic!("close on release-pending node {id}"),
        };
        match next {
            Some(life) => self.node_mut(id).life = life,
            None => {
                self.nodes.remove(&id);
            }
        }
    }

    pub fn clear_release_pending(&mut self) {
        self.nodes
            .retain(|_, node| !matches!(node.life, Life::ReleasePending));
    }

    pub fn close_all(&mut self) {
        let mut remove = Vec::new();
        for (&id, node) in self.nodes.iter_mut() {
            match node.life {
                Life::Linked { links, .. } => node.life = Life::Linked { links, open: 0 },
                Life::OpenUnlinked { .. } | Life::ReleasePending => {
                    assert_ne!(id, ROOT);
                    remove.push(id);
                }
            }
        }
        for id in remove {
            self.nodes.remove(&id);
        }
    }

    pub fn orphans(&self) -> Vec<NodeId> {
        self.nodes
            .iter()
            .filter(|(id, node)| {
                **id != ROOT
                    && matches!(node.life, Life::OpenUnlinked { .. } | Life::ReleasePending)
            })
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn paths(&self, limit: usize) -> Vec<(Path, NodeId)> {
        let mut result = Vec::new();
        let mut stack: Vec<(Path, NodeId)> = vec![(Vec::new(), ROOT)];
        while let Some((path, id)) = stack.pop() {
            if result.len() >= limit {
                break;
            }
            let node = self.node(id);
            if node.kind == Kind::Dir {
                for (name, &child) in node.entries.iter().rev() {
                    let mut child_path = path.clone();
                    child_path.push(name.clone());
                    stack.push((child_path, child));
                }
            }
            result.push((path, id));
        }
        result
    }

    pub fn bytes(&self) -> u64 {
        self.nodes
            .values()
            .map(|node| node.chunks.len() as u64 * CHUNK as u64)
            .sum()
    }
}

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use crate::links::{Disk, Entry, Outside, Tree, disk_entry, leads_outside};
use crate::scope::{MAX_PATH, join, relative, within};

pub const MAX_ENTRIES: usize = 1 << 20;
pub const WHITEOUTS: &str = "whiteouts";
pub const LAYER: &str = "layer";
pub const LOWER: &str = "lower";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lookup {
    Upper,
    Lower,
    Missing,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overlay {
    upper: BTreeSet<String>,
    whiteouts: BTreeSet<String>,
    opaque: BTreeSet<String>,
    origin_inodes: BTreeMap<String, usize>,
}

fn ancestors(rel: &str) -> impl Iterator<Item = &str> {
    let mut cut = Some(rel.len());
    std::iter::from_fn(move || {
        let end = cut?;
        let item = &rel[..end];
        cut = rel[..end].rfind('/');
        Some(item)
    })
}

pub fn rel_parent(rel: &str) -> &str {
    match rel.rfind('/') {
        Some(i) => &rel[..i],
        None => "",
    }
}

fn subtree_set(set: &BTreeSet<String>, rel: &str, include_self: bool) -> Vec<String> {
    let mut out = Vec::new();
    if include_self && set.contains(rel) {
        out.push(rel.to_owned());
    }
    let below = format!("{rel}/");
    for p in set.range(below.clone()..) {
        if !p.starts_with(&below) {
            break;
        }
        out.push(p.clone());
    }
    out
}

fn subtree_map(
    map: &BTreeMap<String, usize>,
    rel: &str,
    include_self: bool,
) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    if include_self && let Some(&v) = map.get(rel) {
        out.push((rel.to_owned(), v));
    }
    let below = format!("{rel}/");
    for (p, &v) in map.range(below.clone()..) {
        if !p.starts_with(&below) {
            break;
        }
        out.push((p.clone(), v));
    }
    out
}

fn remove_subtree_set(set: &mut BTreeSet<String>, rel: &str, include_self: bool) -> Vec<String> {
    let items = subtree_set(set, rel, include_self);
    for p in &items {
        set.remove(p);
    }
    items
}

fn remove_subtree_map(
    map: &mut BTreeMap<String, usize>,
    rel: &str,
    include_self: bool,
) -> Vec<(String, usize)> {
    let items = subtree_map(map, rel, include_self);
    for (p, _) in &items {
        map.remove(p);
    }
    items
}

pub struct CreatedUndo {
    was_upper: bool,
    removed_whiteout: bool,
    added_opaque: bool,
}

impl CreatedUndo {
    pub fn persisted_changed(&self) -> bool {
        self.removed_whiteout || self.added_opaque
    }
}

pub struct RemovedUndo {
    rel: String,
    removed_upper: Vec<String>,
    removed_whiteouts: Vec<String>,
    removed_opaque: Vec<String>,
    removed_origins: Vec<(String, usize)>,
    added_whiteout: bool,
}

impl RemovedUndo {
    pub fn persisted_changed(&self) -> bool {
        self.added_whiteout || !self.removed_whiteouts.is_empty() || !self.removed_opaque.is_empty()
    }
}

pub struct RenameUndo {
    from: String,
    to: String,
    noop: bool,
    saved_upper: Vec<String>,
    saved_whiteouts: Vec<String>,
    saved_opaque: Vec<String>,
    saved_origins: Vec<(String, usize)>,
    persisted_changed: bool,
}

impl RenameUndo {
    pub fn persisted_changed(&self) -> bool {
        self.persisted_changed
    }
}

impl Overlay {
    pub fn has_upper(&self, rel: &str) -> bool {
        self.upper.contains(rel)
    }

    pub fn upper_len(&self) -> usize {
        self.upper.len()
    }

    pub fn can_add_upper(&self, rel: &str) -> Result<(), i32> {
        if !self.upper.contains(rel) && self.upper.len() >= MAX_ENTRIES {
            return Err(crate::scope::EIO);
        }
        Ok(())
    }

    pub fn check_invariants(&self) {
        assert!(self.upper.len() <= MAX_ENTRIES);
        assert!(self.whiteouts.len() <= MAX_ENTRIES);
        debug_assert!(self.whiteouts.is_disjoint(&self.upper));
        debug_assert!(self.opaque.is_subset(&self.upper));
        debug_assert!(self.opaque.is_disjoint(&self.whiteouts));
        debug_assert!(self.origin_inodes.keys().all(|k| self.upper.contains(k)));
    }

    pub fn set_origin_inode(&mut self, rel: &str, inode: usize) {
        assert!(self.upper.contains(rel));
        self.origin_inodes.insert(rel.to_owned(), inode);
        self.check_invariants();
    }

    pub fn origin_inode(&self, rel: &str) -> Option<usize> {
        self.origin_inodes.get(rel).copied()
    }

    pub fn copy_up_done(&mut self, rel: &str) -> Result<(), i32> {
        self.can_add_upper(rel)?;
        assert!(!self.whiteouts.contains(rel));
        self.upper.insert(rel.to_owned());
        self.check_invariants();
        Ok(())
    }

    pub fn mark_opaque(&mut self, rel: &str) {
        assert!(self.upper.contains(rel));
        assert!(!self.whiteouts.contains(rel));
        self.opaque.insert(rel.to_owned());
    }

    pub fn lower_hidden(&self, rel: &str) -> bool {
        if rel.is_empty() {
            return false;
        }
        ancestors(rel).any(|a| self.whiteouts.contains(a))
            || ancestors(rel).skip(1).any(|a| self.opaque.contains(a))
    }

    pub fn lookup(&self, rel: &str, lower_exists: impl FnOnce() -> bool) -> Lookup {
        if rel.is_empty() || self.upper.contains(rel) {
            return Lookup::Upper;
        }
        if self.lower_hidden(rel) {
            return Lookup::Missing;
        }
        if lower_exists() {
            Lookup::Lower
        } else {
            Lookup::Missing
        }
    }

    pub fn lower_listed(&self, dir: &str) -> bool {
        !self.lower_hidden(dir) && !self.opaque.contains(dir)
    }

    pub fn created_undo(&mut self, rel: &str, dir: bool) -> Result<CreatedUndo, i32> {
        self.can_add_upper(rel)?;
        let was_upper = !self.upper.insert(rel.to_owned());
        let removed_whiteout = self.whiteouts.remove(rel);
        let added_opaque = if removed_whiteout && dir {
            self.opaque.insert(rel.to_owned())
        } else {
            false
        };
        assert!(self.upper.contains(rel));
        assert!(!self.whiteouts.contains(rel));
        self.check_invariants();
        Ok(CreatedUndo {
            was_upper,
            removed_whiteout,
            added_opaque,
        })
    }

    pub fn undo_created(&mut self, rel: &str, undo: CreatedUndo) {
        if undo.added_opaque {
            self.opaque.remove(rel);
        }
        if undo.removed_whiteout {
            self.whiteouts.insert(rel.to_owned());
        }
        if !undo.was_upper {
            self.upper.remove(rel);
            self.origin_inodes.remove(rel);
        }
        self.check_invariants();
    }

    pub fn created(&mut self, rel: &str, dir: bool) -> Result<(), i32> {
        self.created_undo(rel, dir).map(|_| ())
    }

    pub fn removed_undo(&mut self, rel: &str, lower_had: bool) -> Result<RemovedUndo, i32> {
        let removed_whiteouts = subtree_set(&self.whiteouts, rel, false);
        if lower_had
            && !self.whiteouts.contains(rel)
            && self.whiteouts.len() - removed_whiteouts.len() >= MAX_ENTRIES
        {
            return Err(crate::scope::EIO);
        }
        for p in &removed_whiteouts {
            self.whiteouts.remove(p);
        }
        let removed_upper = remove_subtree_set(&mut self.upper, rel, true);
        let removed_opaque = remove_subtree_set(&mut self.opaque, rel, true);
        let removed_origins = remove_subtree_map(&mut self.origin_inodes, rel, true);
        let added_whiteout = if lower_had {
            self.whiteouts.insert(rel.to_owned())
        } else {
            false
        };
        assert!(!self.upper.contains(rel));
        assert!(!self.opaque.contains(rel));
        self.check_invariants();
        Ok(RemovedUndo {
            rel: rel.to_owned(),
            removed_upper,
            removed_whiteouts,
            removed_opaque,
            removed_origins,
            added_whiteout,
        })
    }

    pub fn undo_removed(&mut self, undo: RemovedUndo) {
        if undo.added_whiteout {
            self.whiteouts.remove(&undo.rel);
        }
        for p in undo.removed_upper {
            self.upper.insert(p);
        }
        for p in undo.removed_whiteouts {
            self.whiteouts.insert(p);
        }
        for p in undo.removed_opaque {
            self.opaque.insert(p);
        }
        for (p, ino) in undo.removed_origins {
            self.origin_inodes.insert(p, ino);
        }
        self.check_invariants();
    }

    pub fn removed(&mut self, rel: &str, lower_had: bool) -> Result<(), i32> {
        self.removed_undo(rel, lower_had).map(|_| ())
    }

    pub fn moved(&mut self, from: &str, to: &str) {
        let moved = remove_subtree_set(&mut self.upper, from, true);
        for p in moved {
            let dst = format!("{to}{}", &p[from.len()..]);
            self.whiteouts.remove(&dst);
            self.upper.insert(dst);
        }
        let moved_opaque = remove_subtree_set(&mut self.opaque, from, true);
        let _ = remove_subtree_set(&mut self.opaque, to, true);
        for p in moved_opaque {
            let dst = format!("{to}{}", &p[from.len()..]);
            self.opaque.insert(dst);
        }
        let moved_origins = remove_subtree_map(&mut self.origin_inodes, from, true);
        let _ = remove_subtree_map(&mut self.origin_inodes, to, true);
        for (p, ino) in moved_origins {
            let dst = format!("{to}{}", &p[from.len()..]);
            self.origin_inodes.insert(dst, ino);
        }
        self.check_invariants();
    }

    fn pair_slice_set(set: &BTreeSet<String>, a: &str, b: &str) -> Vec<String> {
        let mut out = subtree_set(set, a, true);
        out.extend(subtree_set(set, b, true));
        out.sort();
        out.dedup();
        out
    }

    fn pair_slice_map(map: &BTreeMap<String, usize>, a: &str, b: &str) -> Vec<(String, usize)> {
        let mut out = subtree_map(map, a, true);
        out.extend(subtree_map(map, b, true));
        out.sort_by(|x, y| x.0.cmp(&y.0));
        out.dedup_by(|x, y| x.0 == y.0);
        out
    }

    pub fn rename_undo(
        &mut self,
        from: &str,
        to: &str,
        is_dir: bool,
        whiteout_old: bool,
        opaque_new: bool,
    ) -> Result<RenameUndo, i32> {
        if opaque_new {
            assert!(is_dir);
        }
        if from == to {
            self.check_invariants();
            return Ok(RenameUndo {
                from: from.to_owned(),
                to: to.to_owned(),
                noop: true,
                saved_upper: Vec::new(),
                saved_whiteouts: Vec::new(),
                saved_opaque: Vec::new(),
                saved_origins: Vec::new(),
                persisted_changed: false,
            });
        }
        let saved_upper = Self::pair_slice_set(&self.upper, from, to);
        let saved_whiteouts = Self::pair_slice_set(&self.whiteouts, from, to);
        let saved_opaque = Self::pair_slice_set(&self.opaque, from, to);
        let saved_origins = Self::pair_slice_map(&self.origin_inodes, from, to);
        let to_was_whited_out = self.whiteouts.contains(to);
        self.moved(from, to);
        let res = self.removed(from, whiteout_old).and_then(|()| {
            self.created(to, is_dir)?;
            if opaque_new || (is_dir && to_was_whited_out) {
                self.mark_opaque(to);
            }
            Ok(())
        });
        if let Err(e) = res {
            self.undo_rename(RenameUndo {
                from: from.to_owned(),
                to: to.to_owned(),
                noop: false,
                saved_upper,
                saved_whiteouts,
                saved_opaque,
                saved_origins,
                persisted_changed: false,
            });
            return Err(e);
        }
        let new_whiteouts = Self::pair_slice_set(&self.whiteouts, from, to);
        let new_opaque = Self::pair_slice_set(&self.opaque, from, to);
        let persisted_changed = new_whiteouts != saved_whiteouts || new_opaque != saved_opaque;
        self.check_invariants();
        Ok(RenameUndo {
            from: from.to_owned(),
            to: to.to_owned(),
            noop: false,
            saved_upper,
            saved_whiteouts,
            saved_opaque,
            saved_origins,
            persisted_changed,
        })
    }

    pub fn undo_rename(&mut self, undo: RenameUndo) {
        if undo.noop {
            return;
        }
        let _ = remove_subtree_set(&mut self.upper, &undo.from, true);
        let _ = remove_subtree_set(&mut self.upper, &undo.to, true);
        let _ = remove_subtree_set(&mut self.whiteouts, &undo.from, true);
        let _ = remove_subtree_set(&mut self.whiteouts, &undo.to, true);
        let _ = remove_subtree_set(&mut self.opaque, &undo.from, true);
        let _ = remove_subtree_set(&mut self.opaque, &undo.to, true);
        let _ = remove_subtree_map(&mut self.origin_inodes, &undo.from, true);
        let _ = remove_subtree_map(&mut self.origin_inodes, &undo.to, true);
        for p in undo.saved_upper {
            self.upper.insert(p);
        }
        for p in undo.saved_whiteouts {
            self.whiteouts.insert(p);
        }
        for p in undo.saved_opaque {
            self.opaque.insert(p);
        }
        for (p, ino) in undo.saved_origins {
            self.origin_inodes.insert(p, ino);
        }
        self.check_invariants();
    }

    pub fn rename(
        &mut self,
        from: &str,
        to: &str,
        is_dir: bool,
        whiteout_old: bool,
        opaque_new: bool,
    ) -> Result<(), i32> {
        self.rename_undo(from, to, is_dir, whiteout_old, opaque_new)
            .map(|_| ())
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (tag, set) in [(b'D', &self.whiteouts), (b'O', &self.opaque)] {
            for p in set {
                out.push(tag);
                out.extend_from_slice(p.as_bytes());
                out.push(0);
            }
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Overlay, String> {
        let mut overlay = Overlay::default();
        for record in bytes.split(|&b| b == 0).filter(|r| !r.is_empty()) {
            let (tag, path) = record.split_first().expect("filtered empty records");
            let path =
                std::str::from_utf8(path).map_err(|_| "a whiteout path is not UTF-8".to_owned())?;
            if path.len() > MAX_PATH
                || path.starts_with('/')
                || path.split('/').any(|c| c == ".." || c == ".")
            {
                return Err(format!("bad whiteout path {path:?}"));
            }
            let set = match tag {
                b'D' => &mut overlay.whiteouts,
                b'O' => &mut overlay.opaque,
                _ => return Err(format!("bad whiteout record {record:?}")),
            };
            if set.len() >= MAX_ENTRIES {
                return Err("too many whiteouts".into());
            }
            set.insert(path.to_owned());
        }
        Ok(overlay)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    Added(String),
    Modified(String),
    Deleted(String),
}

impl Change {
    pub fn line(&self) -> String {
        match self {
            Change::Added(p) => format!("A {p}"),
            Change::Modified(p) => format!("M {p}"),
            Change::Deleted(p) => format!("D {p}"),
        }
    }
}

struct Merged<'a> {
    lower: &'a str,
    layer: &'a Path,
    overlay: &'a Overlay,
}

impl Tree for Merged<'_> {
    fn entry(&self, path: &str) -> Entry {
        if path == self.lower || !within(path, self.lower) {
            return Disk.entry(path);
        }
        let rel = relative(path, self.lower);
        match disk_entry(&self.layer.join(rel)) {
            Entry::Missing if self.overlay.lower_hidden(rel) => Entry::Missing,
            Entry::Missing => Disk.entry(path),
            found => found,
        }
    }
}

pub struct Session {
    pub lower: PathBuf,
    pub layer: PathBuf,
    pub overlay: Overlay,
}

fn read_tree(base: &Path, rel: &str, out: &mut Vec<String>) -> io::Result<()> {
    let dir = if rel.is_empty() {
        base.to_owned()
    } else {
        base.join(rel)
    };
    let mut names: Vec<String> = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a file name is not UTF-8"))?;
        names.push(name);
    }
    names.sort();
    for name in names {
        if out.len() >= MAX_ENTRIES {
            return Err(io::Error::other("the layer has too many entries"));
        }
        let child = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        let meta = fs::symlink_metadata(base.join(&child))?;
        out.push(child.clone());
        if meta.is_dir() {
            read_tree(base, &child, out)?;
        }
    }
    Ok(())
}

fn same_entry(a: &Path, b: &Path) -> io::Result<bool> {
    let (ma, mb) = (fs::symlink_metadata(a)?, fs::symlink_metadata(b)?);
    if ma.file_type() != mb.file_type() || ma.mode() & 0o7777 != mb.mode() & 0o7777 {
        return Ok(false);
    }
    if ma.file_type().is_symlink() {
        return Ok(fs::read_link(a)? == fs::read_link(b)?);
    }
    if ma.is_dir() {
        return Ok(true);
    }
    if ma.len() != mb.len() {
        return Ok(false);
    }
    Ok(fs::read(a)? == fs::read(b)?)
}

impl Session {
    pub fn open(state: &Path) -> io::Result<Session> {
        let lower = fs::read_to_string(state.join(LOWER))?;
        let overlay = match fs::read(state.join(WHITEOUTS)) {
            Ok(bytes) => Overlay::decode(&bytes).map_err(io::Error::other)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Overlay::default(),
            Err(e) => return Err(e),
        };
        Ok(Session {
            lower: PathBuf::from(lower.trim_end_matches('\n')),
            layer: state.join(LAYER),
            overlay,
        })
    }

    pub fn changes(&self) -> io::Result<Vec<Change>> {
        let mut changes = Vec::new();
        for p in &self.overlay.whiteouts {
            if fs::symlink_metadata(self.lower.join(p)).is_ok()
                && fs::symlink_metadata(self.layer.join(p)).is_err()
            {
                changes.push(Change::Deleted(p.clone()));
            }
        }
        for p in &self.overlay.opaque {
            let mut lower = Vec::new();
            if fs::symlink_metadata(self.lower.join(p)).is_ok_and(|m| m.is_dir()) {
                read_tree(&self.lower.join(p), "", &mut lower)?;
            }
            for q in lower {
                if fs::symlink_metadata(self.layer.join(p).join(&q)).is_err() {
                    changes.push(Change::Deleted(format!("{p}/{q}")));
                }
            }
        }
        let mut upper = Vec::new();
        read_tree(&self.layer, "", &mut upper)?;
        for p in upper {
            let lower = self.lower.join(&p);
            let hidden = self.overlay.lower_hidden(&p);
            match fs::symlink_metadata(&lower) {
                Ok(_) if !hidden => {
                    if !same_entry(&lower, &self.layer.join(&p))? {
                        changes.push(Change::Modified(p));
                    }
                }
                Ok(_) => changes.push(Change::Modified(p)),
                Err(_) => changes.push(Change::Added(p)),
            }
        }
        changes.sort_by(|a, b| key(a).cmp(key(b)));
        Ok(changes)
    }

    pub fn outside_links(&self) -> io::Result<Vec<(String, Outside)>> {
        let lower = self
            .lower
            .to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "the folder is not UTF-8"))?;
        let merged = Merged {
            lower,
            layer: &self.layer,
            overlay: &self.overlay,
        };
        let mut upper = Vec::new();
        read_tree(&self.layer, "", &mut upper)?;
        let mut out = Vec::new();
        for p in upper {
            let at = self.layer.join(&p);
            if !fs::symlink_metadata(&at)?.file_type().is_symlink() {
                continue;
            }
            let target = fs::read_link(&at)?;
            let found = match target.to_str() {
                Some(text) => leads_outside(&merged, &join(lower, rel_parent(&p)), text, lower),
                None => Some(Outside {
                    target: target.to_string_lossy().into_owned(),
                    resolved: None,
                }),
            };
            if let Some(found) = found {
                out.push((p, found));
            }
        }
        Ok(out)
    }

    pub fn apply(&self) -> io::Result<()> {
        let mut deletions: Vec<&String> = self
            .overlay
            .whiteouts
            .iter()
            .chain(self.overlay.opaque.iter())
            .collect();
        deletions.sort_by_key(|p| std::cmp::Reverse(p.len()));
        for p in deletions {
            remove_any(&self.lower.join(p))?;
        }
        let mut upper = Vec::new();
        read_tree(&self.layer, "", &mut upper)?;
        for p in upper {
            let from = self.layer.join(&p);
            let to = self.lower.join(&p);
            let meta = fs::symlink_metadata(&from)?;
            let existing = fs::symlink_metadata(&to).ok();
            if meta.is_dir() {
                match existing {
                    Some(m) if m.is_dir() => {}
                    Some(_) => {
                        remove_any(&to)?;
                        fs::create_dir(&to)?;
                    }
                    None => fs::create_dir(&to)?,
                }
                fs::set_permissions(&to, fs::Permissions::from_mode(meta.mode() & 0o7777))?;
            } else if meta.file_type().is_symlink() {
                if existing.is_some() {
                    remove_any(&to)?;
                }
                symlink(fs::read_link(&from)?, &to)?;
            } else {
                if existing.as_ref().is_some_and(|m| !m.is_file()) {
                    remove_any(&to)?;
                }
                fs::write(&to, fs::read(&from)?)?;
                fs::set_permissions(&to, fs::Permissions::from_mode(meta.mode() & 0o7777))?;
            }
        }
        Ok(())
    }
}

fn key(change: &Change) -> &str {
    match change {
        Change::Added(p) | Change::Modified(p) | Change::Deleted(p) => p,
    }
}

fn remove_any(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

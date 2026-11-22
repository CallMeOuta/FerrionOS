use std::collections::BTreeSet;
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
    pub upper: BTreeSet<String>,
    pub whiteouts: BTreeSet<String>,
    pub opaque: BTreeSet<String>,
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

fn under(path: &str, base: &str) -> bool {
    path.len() > base.len() && path.starts_with(base) && path.as_bytes()[base.len()] == b'/'
}

impl Overlay {
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

    pub fn created(&mut self, rel: &str, dir: bool) -> Result<(), i32> {
        if self.upper.len() >= MAX_ENTRIES {
            return Err(crate::scope::EIO);
        }
        self.upper.insert(rel.to_owned());
        if self.whiteouts.remove(rel) && dir {
            self.opaque.insert(rel.to_owned());
        }
        Ok(())
    }

    pub fn removed(&mut self, rel: &str, lower_had: bool) -> Result<(), i32> {
        self.upper.remove(rel);
        let below = format!("{rel}/");
        self.upper.retain(|p| !p.starts_with(&below));
        self.whiteouts.retain(|p| !p.starts_with(&below));
        self.opaque.retain(|p| p != rel && !p.starts_with(&below));
        if lower_had {
            if self.whiteouts.len() >= MAX_ENTRIES {
                return Err(crate::scope::EIO);
            }
            self.whiteouts.insert(rel.to_owned());
        }
        Ok(())
    }

    pub fn moved(&mut self, from: &str, to: &str) {
        let moved: Vec<String> = self
            .upper
            .iter()
            .filter(|p| p.as_str() == from || under(p, from))
            .cloned()
            .collect();
        for p in moved {
            self.upper.remove(&p);
            self.upper.insert(format!("{to}{}", &p[from.len()..]));
        }
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

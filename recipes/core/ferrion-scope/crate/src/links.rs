use std::collections::VecDeque;
use std::fs;
use std::path::Path;

use crate::scope::{MAX_LINKS, MAX_QUEUE, Walked, parent, retarget, within};

pub const MAX_RESOLVE_STEPS: usize = MAX_QUEUE * (MAX_LINKS + 1);

fn components(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|c| !c.is_empty())
}

fn path_of(stack: &[String]) -> String {
    if stack.is_empty() {
        return "/".to_owned();
    }
    let mut out = String::new();
    for c in stack {
        out.push('/');
        out.push_str(c);
    }
    out
}

pub enum Entry {
    Missing,
    Plain,
    Link(Option<String>),
}

pub trait Tree {
    fn entry(&self, path: &str) -> Entry;
}

pub struct Disk;

pub fn disk_entry(path: &Path) -> Entry {
    match fs::symlink_metadata(path) {
        Err(_) => Entry::Missing,
        Ok(meta) if meta.file_type().is_symlink() => Entry::Link(
            fs::read_link(path)
                .ok()
                .and_then(|t| t.to_str().map(str::to_owned)),
        ),
        Ok(_) => Entry::Plain,
    }
}

impl Tree for Disk {
    fn entry(&self, path: &str) -> Entry {
        disk_entry(Path::new(path))
    }
}

pub fn resolve_in(tree: &impl Tree, dir: &str, target: &str) -> Option<String> {
    let target = match retarget(target) {
        Walked::Path(t) => t,
        Walked::External(e) => return Some(e),
    };
    let mut stack: Vec<String> = if target.starts_with('/') {
        Vec::new()
    } else {
        components(dir).map(str::to_owned).collect()
    };
    let mut queue: VecDeque<String> = components(&target).map(str::to_owned).collect();
    let mut links = 0;
    let mut lexical = false;
    for _ in 0..MAX_RESOLVE_STEPS {
        let Some(c) = queue.pop_front() else {
            return Some(path_of(&stack));
        };
        match c.as_str() {
            "." => continue,
            ".." => {
                stack.pop();
                continue;
            }
            _ => {}
        }
        stack.push(c);
        if lexical {
            continue;
        }
        let next = match tree.entry(&path_of(&stack)) {
            Entry::Missing => {
                lexical = true;
                continue;
            }
            Entry::Plain => continue,
            Entry::Link(next) => next?,
        };
        links += 1;
        if links > MAX_LINKS {
            return None;
        }
        stack.pop();
        match retarget(&next) {
            Walked::External(mut e) => {
                for rest in &queue {
                    e.push('/');
                    e.push_str(rest);
                }
                return Some(e);
            }
            Walked::Path(t) => {
                if t.starts_with('/') {
                    stack.clear();
                }
                for c in components(&t).collect::<Vec<_>>().into_iter().rev() {
                    queue.push_front(c.to_owned());
                }
                if queue.len() > MAX_QUEUE {
                    return None;
                }
            }
        }
    }
    None
}

pub struct Outside {
    pub target: String,
    pub resolved: Option<String>,
}

impl Outside {
    pub fn describe(&self) -> String {
        match &self.resolved {
            Some(resolved) => format!("{} (to {resolved})", self.target),
            None => format!("{} (it does not resolve)", self.target),
        }
    }
}

pub fn leads_outside(tree: &impl Tree, dir: &str, target: &str, root: &str) -> Option<Outside> {
    let resolved = resolve_in(tree, dir, target);
    match &resolved {
        Some(r) if within(r, root) => None,
        _ => Some(Outside {
            target: target.to_owned(),
            resolved,
        }),
    }
}

pub fn link_leads_outside(link: &str, root: &str) -> Option<Outside> {
    let meta = fs::symlink_metadata(link).ok()?;
    if !meta.file_type().is_symlink() {
        return None;
    }
    let target = fs::read_link(link).ok()?;
    match target.to_str() {
        Some(text) => leads_outside(&Disk, parent(link), text, root),
        None => Some(Outside {
            target: target.to_string_lossy().into_owned(),
            resolved: None,
        }),
    }
}

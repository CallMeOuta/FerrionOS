use std::collections::VecDeque;

pub const MAX_GRANTS: usize = 64;
pub const MAX_SOCKET_GRANTS: usize = 4;
pub const MAX_HIDDEN: usize = 8;
pub const MAX_PATH: usize = 4096;
pub const MAX_NAME: usize = 255;
pub const MAX_COMPONENTS: usize = 512;
pub const MAX_LINKS: usize = 40;
pub const MAX_QUEUE: usize = 2 * MAX_COMPONENTS;
pub const MAX_SPEC: usize = 64 * 1024;

pub const ENOENT: i32 = 2;
pub const EIO: i32 = 5;
pub const EACCES: i32 = 13;
pub const EEXIST: i32 = 17;
pub const EXDEV: i32 = 18;
pub const ENOTDIR: i32 = 20;
pub const EISDIR: i32 = 21;
pub const EINVAL: i32 = 22;
pub const EROFS: i32 = 30;
pub const ENAMETOOLONG: i32 = 36;
pub const ENOTEMPTY: i32 = 39;
pub const ELOOP: i32 = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    ReadOnly,
    ReadWrite,
    Clone,
    Socket,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub virt: String,
    pub real: String,
    pub access: Access,
    pub layer: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scope {
    pub grants: Vec<Grant>,
    pub hidden: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    Skeleton,
    Inside { grant: usize, rel: String },
    Outside,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    Symlink,
}

pub trait View {
    fn kind(&mut self, scope: &Scope, grant: usize, rel: &str) -> Result<Kind, i32>;
    fn readlink(&mut self, scope: &Scope, grant: usize, rel: &str) -> Result<String, i32>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Walked {
    Path(String),
    External(String),
}

pub fn is_canonical(path: &str) -> bool {
    if path == "/" {
        return true;
    }
    path.len() <= MAX_PATH
        && path.starts_with('/')
        && !path.ends_with('/')
        && path[1..]
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && c.len() <= MAX_NAME)
}

pub fn within(path: &str, base: &str) -> bool {
    base == "/"
        || path == base
        || (path.len() > base.len()
            && path.starts_with(base)
            && path.as_bytes()[base.len()] == b'/')
}

pub fn join(base: &str, rel: &str) -> String {
    if rel.is_empty() {
        base.to_owned()
    } else if base == "/" {
        format!("/{rel}")
    } else {
        format!("{base}/{rel}")
    }
}

pub fn relative<'a>(path: &'a str, base: &str) -> &'a str {
    if base == "/" {
        &path[1..]
    } else if path.len() == base.len() {
        ""
    } else {
        &path[base.len() + 1..]
    }
}

pub fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &path[..i],
    }
}

pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or("")
}

pub fn retarget(target: &str) -> Walked {
    match target.strip_prefix("/scheme/") {
        Some(rest) => {
            let (scheme, sub) = rest.split_once('/').unwrap_or((rest, ""));
            if scheme == "file" {
                Walked::Path(format!("/{sub}"))
            } else {
                Walked::External(target.to_owned())
            }
        }
        None => Walked::Path(target.to_owned()),
    }
}

pub const MAX_DIR_CACHE: usize = 8192;

#[derive(Debug, Default)]
pub struct DirCache {
    dirs: std::collections::BTreeSet<String>,
}

impl DirCache {
    pub fn contains(&self, real: &str) -> bool {
        self.dirs.contains(real)
    }

    pub fn insert(&mut self, real: String) {
        if self.dirs.len() >= MAX_DIR_CACHE {
            self.dirs.clear();
        }
        self.dirs.insert(real);
        debug_assert!(self.dirs.len() <= MAX_DIR_CACHE);
    }

    pub fn forget(&mut self, real: &str) {
        self.dirs.remove(real);
        let below = format!("{real}/");
        let gone: Vec<String> = self
            .dirs
            .range(below.clone()..)
            .take_while(|p| p.starts_with(&below))
            .cloned()
            .collect();
        for p in gone {
            self.dirs.remove(&p);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.dirs.iter()
    }
}

fn components(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|c| !c.is_empty())
}

fn stack_path(stack: &[String]) -> String {
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

impl Scope {
    pub fn parse(spec: &[u8]) -> Result<Scope, String> {
        if spec.len() > MAX_SPEC {
            return Err(format!(
                "a scope of {} bytes is over {MAX_SPEC}",
                spec.len()
            ));
        }
        let text = std::str::from_utf8(spec).map_err(|_| "the scope is not UTF-8".to_owned())?;
        let mut scope = Scope::default();
        let mut socket_grants = 0usize;
        for line in text.split('\n').filter(|l| !l.is_empty()) {
            let fields: Vec<&str> = line.split('\0').collect();
            match fields[..] {
                ["hide", path] => {
                    if !is_canonical(path) || scope.hidden.len() >= MAX_HIDDEN {
                        return Err(format!("bad hidden path {path:?}"));
                    }
                    scope.hidden.push(path.to_owned());
                }
                ["grant", access, virt, real, ref layer @ ..] => {
                    let access = match access {
                        "ro" => Access::ReadOnly,
                        "rw" => Access::ReadWrite,
                        "clone" => Access::Clone,
                        "socket" => Access::Socket,
                        other => return Err(format!("unknown access {other:?}")),
                    };
                    let layer = match (access, layer) {
                        (Access::Clone, [layer]) if is_canonical(layer) => {
                            Some((*layer).to_owned())
                        }
                        (Access::Clone, _) => {
                            return Err(format!("clone grant {virt:?} needs one layer path"));
                        }
                        (_, []) => None,
                        _ => return Err(format!("grant {virt:?} has extra fields")),
                    };
                    if !is_canonical(virt) || !is_canonical(real) {
                        return Err(format!("grant {virt:?} -> {real:?} is not canonical"));
                    }
                    if scope.grants.len() >= MAX_GRANTS {
                        return Err(format!("more than {MAX_GRANTS} grants"));
                    }
                    if access == Access::Socket {
                        if virt == "/" || real == "/" {
                            return Err("socket grant cannot be root".to_owned());
                        }
                        if socket_grants >= MAX_SOCKET_GRANTS {
                            return Err(format!("more than {MAX_SOCKET_GRANTS} socket grants"));
                        }
                        socket_grants += 1;
                    }
                    if scope.grants.iter().any(|g| g.virt == virt) {
                        return Err(format!("{virt:?} is granted twice"));
                    }
                    scope.grants.push(Grant {
                        virt: virt.to_owned(),
                        real: real.to_owned(),
                        access,
                        layer,
                    });
                }
                _ => return Err(format!("bad scope line {line:?}")),
            }
        }
        assert!(socket_grants <= MAX_SOCKET_GRANTS);
        Ok(scope)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = String::new();
        for hidden in &self.hidden {
            out.push_str(&format!("hide\0{hidden}\n"));
        }
        for g in &self.grants {
            let access = match g.access {
                Access::ReadOnly => "ro",
                Access::ReadWrite => "rw",
                Access::Clone => "clone",
                Access::Socket => "socket",
            };
            out.push_str(&format!("grant\0{access}\0{}\0{}", g.virt, g.real));
            if let Some(layer) = &g.layer {
                out.push('\0');
                out.push_str(layer);
            }
            out.push('\n');
        }
        out.into_bytes()
    }

    pub fn classify(&self, vpath: &str) -> Place {
        debug_assert!(is_canonical(vpath));
        let mut best: Option<usize> = None;
        for (i, g) in self.grants.iter().enumerate() {
            let matched = if g.access == Access::Socket {
                vpath == g.virt
            } else {
                within(vpath, &g.virt)
            };
            if matched && best.is_none_or(|b| self.grants[b].virt.len() < g.virt.len()) {
                best = Some(i);
            }
        }
        if let Some(i) = best {
            let g = &self.grants[i];
            let rel = relative(vpath, &g.virt).to_owned();
            let real = join(&g.real, &rel);
            let hidden = self
                .hidden
                .iter()
                .any(|h| within(&real, h) && !within(&g.real, h));
            if !hidden {
                return Place::Inside { grant: i, rel };
            }
        }
        if self.ancestor_of_grant(vpath) {
            return Place::Skeleton;
        }
        Place::Outside
    }

    pub fn ancestor_of_grant(&self, vpath: &str) -> bool {
        self.grants
            .iter()
            .any(|g| g.virt != vpath && within(&g.virt, vpath))
    }

    pub fn skeleton_children(&self, vpath: &str) -> Vec<String> {
        let mut names: Vec<String> = self
            .grants
            .iter()
            .filter(|g| g.virt != vpath && within(&g.virt, vpath))
            .filter_map(|g| {
                components(relative(&g.virt, vpath))
                    .next()
                    .map(str::to_owned)
            })
            .collect();
        names.sort();
        names.dedup();
        names
    }

    pub fn real_path(&self, grant: usize, rel: &str) -> String {
        join(&self.grants[grant].real, rel)
    }

    pub fn protects(&self, real: &str) -> bool {
        debug_assert!(is_canonical(real));
        self.hidden.iter().any(|h| within(h, real))
            || self.grants.iter().any(|g| {
                within(&g.real, real) || g.layer.as_deref().is_some_and(|l| within(l, real))
            })
    }

    pub fn resolve<V: View>(
        &self,
        view: &mut V,
        start: &str,
        path: &str,
        follow_last: bool,
        links: &mut usize,
    ) -> Result<Walked, i32> {
        if path.len() > MAX_PATH || start.len() > MAX_PATH {
            return Err(ENAMETOOLONG);
        }
        let mut stack: Vec<String> = Vec::new();
        let mut queue: VecDeque<String> = if path.starts_with('/') {
            components(path).map(str::to_owned).collect()
        } else {
            components(start)
                .chain(components(path))
                .map(str::to_owned)
                .collect()
        };
        if queue.len() > MAX_QUEUE {
            return Err(ENAMETOOLONG);
        }
        while let Some(c) = queue.pop_front() {
            match c.as_str() {
                "." => continue,
                ".." => {
                    stack.pop();
                    continue;
                }
                _ => {}
            }
            if c.len() > MAX_NAME {
                return Err(ENAMETOOLONG);
            }
            stack.push(c);
            if stack.len() > MAX_COMPONENTS || queue.len() > MAX_QUEUE {
                return Err(ENAMETOOLONG);
            }
            let last = queue.is_empty();
            if last && !follow_last {
                break;
            }
            let vpath = stack_path(&stack);
            let (grant, rel) = match self.classify(&vpath) {
                Place::Outside => return Err(ENOENT),
                Place::Skeleton => continue,
                Place::Inside { grant, rel } => (grant, rel),
            };
            match view.kind(self, grant, &rel) {
                Ok(Kind::Dir) => {}
                Ok(Kind::File) if last => {}
                Ok(Kind::File) => return Err(ENOTDIR),
                Err(ENOENT) if last || self.ancestor_of_grant(&vpath) => {}
                Err(e) => return Err(e),
                Ok(Kind::Symlink) => {
                    *links += 1;
                    if *links > MAX_LINKS {
                        return Err(ELOOP);
                    }
                    let target = view.readlink(self, grant, &rel)?;
                    if target.is_empty() || target.len() > MAX_PATH {
                        return Err(ENOENT);
                    }
                    stack.pop();
                    let target = match retarget(&target) {
                        Walked::External(mut external) => {
                            for rest in &queue {
                                external.push('/');
                                external.push_str(rest);
                            }
                            return Ok(Walked::External(external));
                        }
                        Walked::Path(target) => target,
                    };
                    if target.starts_with('/') {
                        stack.clear();
                    }
                    for c in components(&target).collect::<Vec<_>>().into_iter().rev() {
                        queue.push_front(c.to_owned());
                    }
                    if queue.len() > MAX_QUEUE {
                        return Err(ENAMETOOLONG);
                    }
                }
            }
        }
        let out = stack_path(&stack);
        debug_assert!(is_canonical(&out));
        Ok(Walked::Path(out))
    }
}

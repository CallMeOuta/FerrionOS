use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use ferrion_scope::scope::{
    Access, DirCache, ELOOP, ENOENT, ENOTDIR, Grant, Kind, MAX_LINKS, Place, Scope, View, Walked,
    join, retarget, within,
};

const NAMES: [&str; 6] = ["a", "b", "c", "dd", "e", "t"];
const MAX_NODES: usize = 400;
const MAX_DEPTH: usize = 5;
const QUERIES_PER_STEP: usize = 8;
const MAX_HELD: usize = 16;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

#[derive(Clone, Debug)]
enum Node {
    Dir(BTreeMap<String, usize>),
    File,
    Link(String),
}

#[derive(Clone, Default)]
struct Tree {
    nodes: Vec<Node>,
}

fn parts(path: &str) -> Vec<&str> {
    path.split('/').filter(|c| !c.is_empty()).collect()
}

impl Tree {
    fn new() -> Tree {
        Tree {
            nodes: vec![Node::Dir(BTreeMap::new())],
        }
    }

    fn child(&self, dir: usize, name: &str) -> Option<usize> {
        match &self.nodes[dir] {
            Node::Dir(children) => children.get(name).copied(),
            _ => None,
        }
    }

    fn strict(&self, path: &str) -> Result<Option<usize>, String> {
        let mut at = 0;
        let comps = parts(path);
        for (i, c) in comps.iter().enumerate() {
            match self.child(at, c) {
                Some(next) => {
                    if i + 1 < comps.len() && !matches!(self.nodes[next], Node::Dir(_)) {
                        return Err(format!("{path}: component {c} is not a folder"));
                    }
                    at = next;
                }
                None => {
                    if i + 1 < comps.len() && !matches!(self.nodes[at], Node::Dir(_)) {
                        return Err(format!("{path}: walks through a non-folder"));
                    }
                    return Ok(None);
                }
            }
        }
        Ok(Some(at))
    }

    fn ids_on(&self, path: &str) -> Vec<usize> {
        let mut at = 0;
        let mut ids = vec![0];
        for c in parts(path) {
            match self.child(at, c) {
                Some(next) => {
                    ids.push(next);
                    at = next;
                }
                None => break,
            }
        }
        ids
    }

    fn insert(&mut self, path: &str, node: Node) -> bool {
        if self.nodes.len() >= MAX_NODES {
            return false;
        }
        let comps = parts(path);
        let Some((name, dirs)) = comps.split_last() else {
            return false;
        };
        let Ok(Some(parent)) = self.strict(&format!("/{}", dirs.join("/"))) else {
            return false;
        };
        let id = self.nodes.len();
        match &mut self.nodes[parent] {
            Node::Dir(children) if !children.contains_key(*name) => {
                children.insert((*name).to_owned(), id);
            }
            _ => return false,
        }
        self.nodes.push(node);
        true
    }

    fn detach(&mut self, path: &str) -> Option<usize> {
        let comps = parts(path);
        let (name, dirs) = comps.split_last()?;
        let parent = self.strict(&format!("/{}", dirs.join("/"))).ok()??;
        match &mut self.nodes[parent] {
            Node::Dir(children) => children.remove(*name),
            _ => None,
        }
    }

    fn attach(&mut self, path: &str, id: usize) -> bool {
        let comps = parts(path);
        let Some((name, dirs)) = comps.split_last() else {
            return false;
        };
        let Ok(Some(parent)) = self.strict(&format!("/{}", dirs.join("/"))) else {
            return false;
        };
        match &mut self.nodes[parent] {
            Node::Dir(children) if !children.contains_key(*name) => {
                children.insert((*name).to_owned(), id);
                true
            }
            _ => false,
        }
    }

    fn walk_all(&self, at: usize, path: String, out: &mut Vec<(String, usize)>) {
        out.push((path.clone(), at));
        if let Node::Dir(children) = &self.nodes[at] {
            for (name, id) in children {
                self.walk_all(*id, join(&path, name), out);
            }
        }
    }

    fn graft(
        &mut self,
        from: &Tree,
        at: usize,
        real: &str,
        hidden: &[String],
        keep_hidden: bool,
    ) -> usize {
        let node = match &from.nodes[at] {
            Node::Dir(children) => {
                let mut copied = BTreeMap::new();
                for (name, id) in children {
                    let path = join(real, name);
                    if !keep_hidden && hidden.iter().any(|h| within(&path, h)) {
                        continue;
                    }
                    copied.insert(
                        name.clone(),
                        self.graft(from, *id, &path, hidden, keep_hidden),
                    );
                }
                Node::Dir(copied)
            }
            other => other.clone(),
        };
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    fn resolve(&self, start: &str, path: &str, follow_last: bool) -> Result<Walked, i32> {
        let mut stack: Vec<String> = if path.starts_with('/') {
            Vec::new()
        } else {
            parts(start).iter().map(|s| s.to_string()).collect()
        };
        let mut queue: std::collections::VecDeque<String> =
            parts(path).iter().map(|s| s.to_string()).collect();
        let mut links = 0;
        while let Some(c) = queue.pop_front() {
            match c.as_str() {
                "." => continue,
                ".." => {
                    stack.pop();
                    continue;
                }
                _ => {}
            }
            stack.push(c);
            let last = queue.is_empty();
            if last && !follow_last {
                break;
            }
            let here = format!("/{}", stack.join("/"));
            let node = match self.strict(&here) {
                Ok(Some(node)) => node,
                Ok(None) if last => break,
                Ok(None) => return Err(ENOENT),
                Err(_) => unreachable!("the oracle walks folders only"),
            };
            match &self.nodes[node] {
                Node::Dir(_) => {}
                Node::File if last => {}
                Node::File => return Err(ENOTDIR),
                Node::Link(target) => {
                    links += 1;
                    if links > MAX_LINKS {
                        return Err(ELOOP);
                    }
                    if target.is_empty() {
                        return Err(ENOENT);
                    }
                    stack.pop();
                    let target = match retarget(target) {
                        Walked::External(mut e) => {
                            for rest in &queue {
                                e.push('/');
                                e.push_str(rest);
                            }
                            return Ok(Walked::External(e));
                        }
                        Walked::Path(t) => t,
                    };
                    if target.starts_with('/') {
                        stack.clear();
                    }
                    for c in parts(&target).into_iter().rev() {
                        queue.push_front(c.to_owned());
                    }
                }
            }
        }
        Ok(Walked::Path(if stack.is_empty() {
            "/".into()
        } else {
            format!("/{}", stack.join("/"))
        }))
    }
}

struct SimView<'a> {
    real: &'a Tree,
    cache: &'a RefCell<DirCache>,
    violations: &'a RefCell<Vec<String>>,
}

impl View for SimView<'_> {
    fn kind(&mut self, scope: &Scope, grant: usize, rel: &str) -> Result<Kind, i32> {
        let g = &scope.grants[grant];
        let real = join(&g.real, rel);
        if !within(&real, &g.real)
            || scope
                .hidden
                .iter()
                .any(|h| within(&real, h) && !within(&g.real, h))
        {
            self.violations
                .borrow_mut()
                .push(format!("looked up {real} outside grant {}", g.real));
        }
        if self.cache.borrow().contains(&real) {
            return Ok(Kind::Dir);
        }
        let node = match self.real.strict(&real) {
            Ok(Some(node)) => node,
            Ok(None) => return Err(ENOENT),
            Err(e) => {
                self.violations
                    .borrow_mut()
                    .push(format!("unverified walk: {e}"));
                return Err(ENOENT);
            }
        };
        Ok(match &self.real.nodes[node] {
            Node::Dir(_) => {
                self.cache.borrow_mut().insert(real);
                Kind::Dir
            }
            Node::File => Kind::File,
            Node::Link(_) => Kind::Symlink,
        })
    }

    fn readlink(&mut self, scope: &Scope, grant: usize, rel: &str) -> Result<String, i32> {
        let real = join(&scope.grants[grant].real, rel);
        match self.real.strict(&real) {
            Ok(Some(node)) => match &self.real.nodes[node] {
                Node::Link(t) => Ok(t.clone()),
                _ => Err(22),
            },
            _ => Err(ENOENT),
        }
    }
}

fn random_path(rng: &mut Rng, depth: usize) -> String {
    let n = 1 + rng.below(depth);
    (0..n)
        .map(|_| NAMES[rng.below(NAMES.len())])
        .collect::<Vec<_>>()
        .iter()
        .fold(String::new(), |acc, c| format!("{acc}/{c}"))
}

fn random_target(rng: &mut Rng, real: &Tree) -> String {
    match rng.below(8) {
        0 => "..".into(),
        1 => "../..".into(),
        2 => "/".into(),
        3 => "/scheme/null".into(),
        4 => format!("/scheme/file{}", random_existing(rng, real)),
        5 => NAMES[rng.below(NAMES.len())].into(),
        6 => format!("../{}", NAMES[rng.below(NAMES.len())]),
        _ => random_existing(rng, real),
    }
}

fn random_existing(rng: &mut Rng, real: &Tree) -> String {
    let mut all = Vec::new();
    real.walk_all(0, "/".into(), &mut all);
    all[rng.below(all.len())].0.clone()
}

fn build(rng: &mut Rng) -> Tree {
    let mut tree = Tree::new();
    for _ in 0..60 {
        let path = random_path(rng, MAX_DEPTH);
        let node = match rng.below(10) {
            0..=4 => Node::Dir(BTreeMap::new()),
            5..=7 => Node::File,
            _ => Node::Link(String::new()),
        };
        tree.insert(&path, node);
    }
    let mut all = Vec::new();
    tree.walk_all(0, "/".into(), &mut all);
    for (_, id) in all {
        if matches!(tree.nodes[id], Node::Link(_)) {
            tree.nodes[id] = Node::Link(random_target(rng, &tree));
        }
    }
    tree
}

fn canonical_entries(tree: &Tree) -> Vec<(String, usize)> {
    let mut all = Vec::new();
    tree.walk_all(0, "/".into(), &mut all);
    all.retain(|(path, _)| tree.strict(path).is_ok());
    all
}

fn make_scope(rng: &mut Rng, real: &Tree) -> Option<Scope> {
    let entries = canonical_entries(real);
    let dirs: Vec<&String> = entries
        .iter()
        .filter(|(p, id)| p != "/" && matches!(real.nodes[*id], Node::Dir(_)))
        .map(|(p, _)| p)
        .collect();
    if dirs.len() < 3 {
        return None;
    }
    let mut scope = Scope::default();
    for _ in 0..1 + rng.below(3) {
        let path = dirs[rng.below(dirs.len())].clone();
        if scope.grants.iter().any(|g| g.virt == path) {
            continue;
        }
        let access = if rng.chance(50) {
            Access::ReadWrite
        } else {
            Access::ReadOnly
        };
        scope.grants.push(Grant {
            virt: path.clone(),
            real: path,
            access,
            layer: None,
        });
    }
    if rng.chance(50) {
        let real_dir = dirs[rng.below(dirs.len())].clone();
        if !scope
            .grants
            .iter()
            .any(|g| within(&real_dir, &g.real) || within(&g.real, &real_dir))
        {
            scope.grants.push(Grant {
                virt: "/zz-alias".into(),
                real: real_dir,
                access: Access::ReadWrite,
                layer: None,
            });
        }
    }
    if rng.chance(50) {
        let hidden = dirs[rng.below(dirs.len())].clone();
        if !scope.grants.iter().any(|g| g.real == hidden) {
            scope.hidden.push(hidden);
        }
    }
    let files: Vec<&String> = entries
        .iter()
        .filter(|(p, id)| p != "/" && matches!(real.nodes[*id], Node::File))
        .map(|(p, _)| p)
        .collect();
    if !files.is_empty() && rng.chance(50) {
        let real_sock = files[rng.below(files.len())].clone();
        let rw_virts: Vec<String> = scope
            .grants
            .iter()
            .filter(|g| g.access == Access::ReadWrite)
            .map(|g| g.virt.clone())
            .collect();
        let virt = if !rw_virts.is_empty() && rng.chance(50) {
            join(&rw_virts[rng.below(rw_virts.len())], "zz-sock")
        } else {
            "/zz-run/zz-sock".to_owned()
        };
        if !scope.grants.iter().any(|g| g.virt == virt)
            && !scope.hidden.iter().any(|h| within(&real_sock, h))
        {
            scope.grants.push(Grant {
                virt,
                real: real_sock,
                access: Access::Socket,
                layer: None,
            });
        }
    }
    let mut seen = BTreeSet::new();
    scope.grants.retain(|g| seen.insert(g.virt.clone()));
    Some(scope)
}

fn oracle_view(real: &Tree, scope: &Scope) -> Tree {
    let mut view = Tree::new();
    let mut order: Vec<&Grant> = scope.grants.iter().collect();
    order.sort_by_key(|g| g.virt.len());
    for g in order {
        let comps = parts(&g.virt);
        let mut at = String::new();
        for c in &comps[..comps.len() - 1] {
            at = format!("{at}/{c}");
            if view.strict(&at).ok().flatten().is_none() {
                view.insert(&at, Node::Dir(BTreeMap::new()));
            }
        }
        view.detach(&g.virt);
        let Ok(Some(src)) = real.strict(&g.real) else {
            continue;
        };
        let keep_hidden = scope.hidden.iter().any(|h| within(&g.real, h));
        let copied = view.graft(real, src, &g.real, &scope.hidden, keep_hidden);
        view.attach(&g.virt, copied);
    }
    view
}

fn mutate(rng: &mut Rng, real: &mut Tree, scope: &Scope, cache: &RefCell<DirCache>) {
    let writable: Vec<&Grant> = scope
        .grants
        .iter()
        .filter(|g| g.access == Access::ReadWrite)
        .collect();
    if writable.is_empty() {
        return;
    }
    let g = writable[rng.below(writable.len())];
    let entries: Vec<String> = canonical_entries(real)
        .into_iter()
        .map(|(p, _)| p)
        .filter(|p| within(p, &g.real) && p != &g.real)
        .collect();
    let fresh = format!(
        "{}/{}",
        if entries.is_empty() || rng.chance(30) {
            g.real.clone()
        } else {
            entries[rng.below(entries.len())].clone()
        },
        NAMES[rng.below(NAMES.len())]
    );
    match rng.below(5) {
        0 => {
            real.insert(&fresh, Node::Dir(BTreeMap::new()));
        }
        1 => {
            let target = random_target(rng, real);
            real.insert(&fresh, Node::Link(target));
        }
        2 => {
            real.insert(&fresh, Node::File);
        }
        3 if !entries.is_empty() => {
            let victim = entries[rng.below(entries.len())].clone();
            if scope.protects(&victim) {
                return;
            }
            real.detach(&victim);
            cache.borrow_mut().forget(&victim);
        }
        _ if !entries.is_empty() => {
            let from = entries[rng.below(entries.len())].clone();
            if within(&fresh, &from) || scope.protects(&from) || scope.protects(&fresh) {
                return;
            }
            if let Some(id) = real.detach(&from) {
                if !real.attach(&fresh, id) {
                    real.attach(&from, id);
                } else {
                    cache.borrow_mut().forget(&from);
                    cache.borrow_mut().forget(&fresh);
                }
            }
        }
        _ => {}
    }
}

fn random_query(rng: &mut Rng) -> String {
    let n = rng.below(7);
    let mut out = if rng.chance(60) {
        String::from("/")
    } else {
        String::new()
    };
    for i in 0..n {
        if i > 0 {
            out.push('/');
        }
        out.push_str(match rng.below(10) {
            0 | 1 => "..",
            2 => ".",
            _ => NAMES[rng.below(NAMES.len())],
        });
    }
    out
}

fn run(seed: u64, steps: usize) -> Result<usize, String> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut real = build(&mut rng);
    let Some(scope) = make_scope(&mut rng, &real) else {
        return Ok(0);
    };
    let cache = RefCell::new(DirCache::default());
    let violations = RefCell::new(Vec::new());
    let hidden_ids: Vec<(String, usize)> = scope
        .hidden
        .iter()
        .filter_map(|h| real.strict(h).ok().flatten().map(|id| (h.clone(), id)))
        .collect();
    let pinned: Vec<(String, usize)> = scope
        .hidden
        .iter()
        .chain(scope.grants.iter().map(|g| &g.real))
        .filter_map(|p| real.strict(p).ok().flatten().map(|id| (p.clone(), id)))
        .collect();
    let mut held: Vec<String> = Vec::new();
    let mut checked = 0;
    for step in 0..steps {
        let view = oracle_view(&real, &scope);
        let start_candidates: Vec<String> = canonical_entries(&view)
            .into_iter()
            .filter(|(_, id)| matches!(view.nodes[*id], Node::Dir(_)))
            .map(|(p, _)| p)
            .collect();
        let keep = start_candidates[rng.below(start_candidates.len())].clone();
        if held.len() < MAX_HELD {
            held.push(keep);
        } else {
            let slot = rng.below(MAX_HELD);
            held[slot] = keep;
        }
        for _ in 0..QUERIES_PER_STEP {
            let stale = rng.chance(30);
            let start = if stale {
                held[rng.below(held.len())].clone()
            } else {
                start_candidates[rng.below(start_candidates.len())].clone()
            };
            let path = random_query(&mut rng);
            let follow = rng.chance(50);
            let mut links = 0;
            let mut sim = SimView {
                real: &real,
                cache: &cache,
                violations: &violations,
            };
            let got = scope.resolve(&mut sim, &start, &path, follow, &mut links);
            let want = if stale && !path.starts_with('/') {
                let joined = if path.is_empty() {
                    start.clone()
                } else {
                    join(&start, &path)
                };
                view.resolve("/", &joined, follow)
            } else {
                view.resolve(&start, &path, follow)
            };
            let got_visible = match &got {
                Ok(Walked::Path(p)) if follow && scope.classify(p) == Place::Outside => Err(ENOENT),
                other => other.clone(),
            };
            let want_visible = match &want {
                Ok(Walked::Path(p))
                    if follow
                        && view.strict(p).ok().flatten().is_none()
                        && scope.classify(p) == Place::Outside =>
                {
                    Err(ENOENT)
                }
                other => other.clone(),
            };
            if got_visible != want_visible {
                return Err(format!(
                    "seed {seed} step {step}: resolve({start:?}, {path:?}, follow {follow}) gave {got:?}, the folder view gives {want:?}; scope {scope:?}"
                ));
            }
            if let Ok(Walked::Path(p)) = &got
                && follow
                && let Place::Inside { grant, rel } = scope.classify(p)
            {
                let real_path = join(&scope.grants[grant].real, &rel);
                if real.strict(&real_path).is_err() {
                    return Err(format!(
                        "seed {seed} step {step}: {p} maps to {real_path}, which walks through a link"
                    ));
                }
                let on_path = real.ids_on(&real_path);
                for (h, id) in &hidden_ids {
                    if on_path.contains(id) && !within(&scope.grants[grant].real, h) {
                        return Err(format!(
                            "seed {seed} step {step}: {p} maps to {real_path}, which reaches the hidden folder {h} by another name"
                        ));
                    }
                }
            }
            checked += 1;
        }
        if let Some(v) = violations.borrow().first() {
            return Err(format!("seed {seed} step {step}: {v}"));
        }
        for p in cache.borrow().iter() {
            match real.strict(p) {
                Ok(Some(id)) if matches!(real.nodes[id], Node::Dir(_)) => {}
                _ => {
                    return Err(format!(
                        "seed {seed} step {step}: the folder cache holds {p}, which is no longer a folder"
                    ));
                }
            }
        }
        mutate(&mut rng, &mut real, &scope, &cache);
        for (p, id) in &pinned {
            if real.strict(p) != Ok(Some(*id)) {
                return Err(format!(
                    "seed {seed} step {step}: {p} no longer names the folder it named when the session started"
                ));
            }
        }
    }
    Ok(checked)
}

const MAX_REGRESSIONS: usize = 1024;

fn regressions() -> Result<Vec<(u64, usize)>, String> {
    let text = std::fs::read_to_string("regressions.txt")
        .map_err(|e| format!("cannot read regressions.txt: {e}"))?;
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        if out.len() >= MAX_REGRESSIONS {
            return Err(format!(
                "regressions.txt holds more than {MAX_REGRESSIONS} seeds"
            ));
        }
        let mut fields = line.split_whitespace();
        match (
            fields.next().and_then(|f| f.parse().ok()),
            fields.next().and_then(|f| f.parse().ok()),
        ) {
            (Some(seed), Some(steps)) => out.push((seed, steps)),
            _ => return Err(format!("regressions.txt: bad line {line:?}")),
        }
    }
    Ok(out)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seeds: u64 = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(100);
    let steps: usize = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(100);
    let first: u64 = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(0);
    let replays = match regressions() {
        Ok(replays) => replays,
        Err(e) => {
            println!("FAIL {e}");
            std::process::exit(1);
        }
    };
    let mut total = 0;
    let runs = replays
        .iter()
        .copied()
        .chain((first..first + seeds).map(|seed| (seed, steps)));
    for (seed, steps) in runs {
        match run(seed, steps) {
            Ok(n) => total += n,
            Err(e) => {
                println!("FAIL {e}");
                std::process::exit(1);
            }
        }
    }
    println!(
        "PASS {} regression seeds and {seeds} seeds of {steps} steps, {total} resolutions agreed with the folder view",
        replays.len()
    );
}

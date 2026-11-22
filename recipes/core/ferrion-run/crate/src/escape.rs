use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::Hasher;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MAX_TREE: usize = 1 << 20;
const RACE_ROUNDS: usize = 2000;
const MAX_DISK_ENTRIES: usize = 64;

fn read_marker(path: impl AsRef<Path>, marker: &str) -> Result<(), String> {
    let mut text = String::new();
    fs::File::open(path.as_ref())
        .and_then(|mut f| f.read_to_string(&mut text))
        .map_err(|e| e.to_string())?;
    if text.contains(marker) {
        Ok(())
    } else {
        Err("read something else".into())
    }
}

fn report(name: &str, result: Result<(), String>) {
    match result {
        Ok(()) => println!("ROUTE {name} REACHED"),
        Err(e) => println!("ROUTE {name} REFUSED {e}"),
    }
}

fn depth(path: &Path) -> usize {
    path.components().count()
}

fn up(scope: &Path, n: usize, secret: &Path) -> PathBuf {
    let mut p = scope.to_owned();
    for _ in 0..n {
        p.push("..");
    }
    p.join(secret.strip_prefix("/").unwrap_or(secret))
}

fn libc_err() -> String {
    std::io::Error::last_os_error().to_string()
}

fn openat_read(dir: &Path, rel: &Path, marker: &str) -> Result<(), String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let d = CString::new(dir.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let r = CString::new(rel.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let dirfd = unsafe { libc::open(d.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
    if dirfd < 0 {
        return Err(libc_err());
    }
    let fd = unsafe { libc::openat(dirfd, r.as_ptr(), libc::O_RDONLY) };
    let result = if fd < 0 {
        Err(libc_err())
    } else {
        let mut buf = vec![0u8; 4096];
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        unsafe { libc::close(fd) };
        if n > 0 && String::from_utf8_lossy(&buf[..n as usize]).contains(marker) {
            Ok(())
        } else {
            Err("read something else".into())
        }
    };
    unsafe { libc::close(dirfd) };
    result
}

fn fresh(path: &Path) {
    let _ = fs::remove_file(path);
}

fn race(scope: &Path, outside: &Path, secret_name: &str, marker: &str) -> Result<(), String> {
    let dir = scope.join("race-dir");
    let real = scope.join("race-real");
    let _ = fs::remove_dir_all(&real);
    fresh(&dir);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir(&real).map_err(|e| e.to_string())?;
    fs::write(real.join(secret_name), "decoy").map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    let swapper = {
        let (stop, dir, real, outside) =
            (stop.clone(), dir.clone(), real.clone(), outside.to_owned());
        std::thread::spawn(move || {
            for _ in 0..RACE_ROUNDS {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let _ = fs::rename(&real, &dir);
                let _ = fs::rename(&dir, &real);
                let _ = symlink(&outside, &dir);
                let _ = fs::remove_file(&dir);
            }
            stop.store(true, Ordering::Relaxed);
        })
    };
    let target = dir.join(secret_name);
    let mut reached = false;
    let deadline = Instant::now() + Duration::from_secs(30);
    while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
        if read_marker(&target, marker).is_ok() {
            reached = true;
            break;
        }
    }
    stop.store(true, Ordering::Relaxed);
    let _ = swapper.join();
    fresh(&dir);
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&real);
    if reached {
        Ok(())
    } else {
        Err(format!("{RACE_ROUNDS} swaps, never read the secret"))
    }
}

struct Stale {
    base: PathBuf,
    moved: PathBuf,
    inner: PathBuf,
}

fn stale_setup(scope: &Path, tag: &str, outside: &Path, name: &str) -> Result<Stale, String> {
    let leaf = outside
        .file_name()
        .ok_or("the secret's folder has no name")?;
    let base = scope.join(tag);
    let moved = scope.join(format!("{tag}-moved"));
    fresh(&base);
    let _ = fs::remove_dir_all(&base);
    let _ = fs::remove_dir_all(&moved);
    let inner = base.join(leaf);
    fs::create_dir_all(&inner).map_err(|e| format!("mkdir: {e}"))?;
    fs::write(inner.join(name), "decoy").map_err(|e| format!("decoy: {e}"))?;
    Ok(Stale { base, moved, inner })
}

fn stale_swap(stale: &Stale, outside: &Path) -> Result<(), String> {
    let above = outside
        .parent()
        .ok_or("the secret's folder has no parent")?;
    fs::rename(&stale.base, &stale.moved).map_err(|e| format!("rename: {e}"))?;
    symlink(above, &stale.base).map_err(|e| format!("symlink: {e}"))
}

fn stale_cleanup(stale: &Stale) {
    fresh(&stale.base);
    let _ = fs::remove_dir_all(&stale.moved);
    let _ = fs::remove_dir_all(&stale.base);
}

fn open_raw(path: &Path, flags: i32) -> Result<i32, String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let fd = unsafe { libc::open(c.as_ptr(), flags) };
    if fd < 0 { Err(libc_err()) } else { Ok(fd) }
}

fn read_fd_marker(fd: i32, marker: &str) -> Result<(), String> {
    let mut buf = vec![0u8; 4096];
    let n = unsafe { libc::pread(fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
    if n < 0 {
        return Err(libc_err());
    }
    if String::from_utf8_lossy(&buf[..n as usize]).contains(marker) {
        Ok(())
    } else {
        Err("read something else".into())
    }
}

fn stale_dir(scope: &Path, outside: &Path, name: &str, marker: &str) -> Result<(), String> {
    let stale = stale_setup(scope, "stale-d", outside, name)?;
    let result = (|| {
        let dirfd = open_raw(&stale.inner, libc::O_RDONLY | libc::O_DIRECTORY)?;
        let found = stale_swap(&stale, outside).and_then(|()| {
            use std::ffi::CString;
            let c = CString::new(name).map_err(|e| e.to_string())?;
            let fd = unsafe { libc::openat(dirfd, c.as_ptr(), libc::O_RDONLY) };
            if fd < 0 {
                return Err(libc_err());
            }
            let read = read_fd_marker(fd, marker);
            unsafe { libc::close(fd) };
            read
        });
        unsafe { libc::close(dirfd) };
        found
    })();
    stale_cleanup(&stale);
    result
}

fn flinkat(fd: i32, dirfd: i32, name: &str) -> Result<(), String> {
    use syscall::data::StdFsCallMeta;
    use syscall::flag::{CallFlags, StdFsCallKind};
    let fds = [fd as usize, dirfd as usize];
    syscall::call_wo(
        &fds[..],
        name.as_bytes(),
        CallFlags::STD_FS,
        &StdFsCallMeta::new(StdFsCallKind::Flinkat, 0, 0),
    )
    .map(|_| ())
    .map_err(|e| format!("flinkat: {e}"))
}

fn stale_link(scope: &Path, outside: &Path, name: &str, marker: &str) -> Result<(), String> {
    let stale = stale_setup(scope, "stale-l", outside, name)?;
    let stolen = scope.join("stale-l-stolen");
    fresh(&stolen);
    let result = (|| {
        let fd = open_raw(&stale.inner.join(name), libc::O_RDONLY)?;
        let dirfd = open_raw(scope, libc::O_RDONLY | libc::O_DIRECTORY);
        let found = dirfd.and_then(|dirfd| {
            let linked = stale_swap(&stale, outside)
                .and_then(|()| flinkat(fd, dirfd, "stale-l-stolen"))
                .and_then(|()| read_marker(&stolen, marker));
            unsafe { libc::close(dirfd) };
            linked
        });
        unsafe { libc::close(fd) };
        found
    })();
    fresh(&stolen);
    stale_cleanup(&stale);
    result
}

pub fn clone_escape(dir: &Path, outside: &Path) -> i32 {
    let folder = dir.join("clone-d");
    let file = folder.join("clone-planted");
    let result = (|| -> Result<(), String> {
        let fd = open_raw(&file, libc::O_RDONLY)?;
        let swapped = fs::remove_dir_all(&folder)
            .map_err(|e| format!("remove: {e}"))
            .and_then(|()| symlink(outside, &folder).map_err(|e| format!("symlink: {e}")))
            .and_then(|()| {
                if unsafe { libc::fchmod(fd, 0o600) } != 0 {
                    Err(format!("fchmod: {}", libc_err()))
                } else {
                    Ok(())
                }
            });
        unsafe { libc::close(fd) };
        swapped
    })();
    report("clone-stale", result);
    0
}

fn other_sessions(home: &Path) -> Result<(), String> {
    let own = std::env::var_os("HOME").and_then(|h| {
        Path::new(&h)
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_owned())
    });
    let entries = fs::read_dir(home.join(".local/state/ferrion-run")).map_err(|e| e.to_string())?;
    let others = entries
        .filter_map(|e| e.ok())
        .filter(|e| Some(e.file_name()) != own)
        .count();
    if others == 0 {
        Err("lists no other session".into())
    } else {
        Ok(())
    }
}

fn holds_foreign_table() -> Result<(), String> {
    use crate::launch::{TableHandle, current_thread_fd, file_table, table_handle};
    let thread_fd = current_thread_fd();
    for fd in file_table(thread_fd)? {
        if table_handle(fd, thread_fd)? == TableHandle::Foreign {
            let copy = libredox::call::dup(fd, b"copy")
                .map_err(|e| format!("copy of the file table at {fd:#x}: {e}"))?;
            let _ = libredox::call::close(copy);
            return Ok(());
        }
    }
    Err("holds no other process's file table".into())
}

pub fn foreign_filetable() -> i32 {
    report("foreign-filetable", holds_foreign_table());
    0
}

pub fn race_swap(scope: &Path, outside: &Path, seconds: u64) -> i32 {
    let dir = scope.join("race2-dir");
    let real = scope.join("race2-real");
    let _ = fs::create_dir(&real);
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut rounds = 0usize;
    while Instant::now() < deadline && rounds < 1_000_000 {
        let _ = fs::rename(&real, &dir);
        let _ = fs::rename(&dir, &real);
        let _ = symlink(outside, &dir);
        let _ = fs::remove_file(&dir);
        rounds += 1;
    }
    println!("race-swap: {rounds} rounds");
    0
}

pub fn race_read(scope: &Path, secret_name: &str, marker: &str, seconds: u64) -> i32 {
    let target = scope.join("race2-dir").join(secret_name);
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut tries = 0usize;
    let mut reached = false;
    while Instant::now() < deadline && tries < 1_000_000 {
        tries += 1;
        if read_marker(&target, marker).is_ok() {
            reached = true;
            break;
        }
    }
    report(
        "race-sessions",
        if reached {
            Ok(())
        } else {
            Err(format!("{tries} reads, never the secret"))
        },
    );
    0
}

pub fn escape(scope: &Path, secret: &Path, marker: &str, victim: &Path, home: &Path) -> i32 {
    let outside = secret.parent().unwrap_or(Path::new("/"));
    let name = secret
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let n = depth(scope);
    report("start-folder", read_marker(&name, marker));
    report("absolute", read_marker(secret, marker));
    report(
        "scheme-path",
        read_marker(format!("/scheme/file{}", secret.display()), marker),
    );
    report("dotdot", read_marker(up(scope, n - 1, secret), marker));
    report("dotdot-many", read_marker(up(scope, 64, secret), marker));
    report(
        "stat",
        fs::symlink_metadata(secret)
            .map(|_| ())
            .map_err(|e| e.to_string()),
    );
    let links: [(&str, PathBuf, PathBuf); 6] = [
        ("symlink-absolute", scope.join("l-abs"), secret.to_owned()),
        (
            "symlink-relative",
            scope.join("l-rel"),
            up(Path::new(""), n - 1, secret),
        ),
        ("symlink-dir", scope.join("l-dir"), outside.to_owned()),
        ("symlink-root", scope.join("l-root"), PathBuf::from("/")),
        (
            "symlink-scheme",
            scope.join("l-sch"),
            PathBuf::from(format!("/scheme/file{}", secret.display())),
        ),
        ("symlink-chain", scope.join("l-c1"), PathBuf::from("l-c2")),
    ];
    fresh(&scope.join("l-c2"));
    let _ = symlink(secret, scope.join("l-c2"));
    for (route, link, target) in &links {
        fresh(link);
        let made = symlink(target, link);
        let path = match *route {
            "symlink-dir" => link.join(&name),
            "symlink-root" => link.join(secret.strip_prefix("/").unwrap_or(secret)),
            _ => link.clone(),
        };
        report(
            route,
            made.map_err(|e| format!("symlink: {e}"))
                .and_then(|()| read_marker(&path, marker)),
        );
    }
    report(
        "symlink-dir-write",
        fs::write(scope.join("l-dir").join("written-through-link"), marker)
            .map_err(|e| e.to_string()),
    );
    let _ = fs::remove_file(outside.join("written-through-link"));
    report(
        "openat-dotdot",
        openat_read(scope, &up(Path::new(""), n - 1, secret), marker),
    );
    report("openat-absolute", openat_read(scope, secret, marker));
    report(
        "chdir-up",
        (|| {
            std::env::set_current_dir(scope).map_err(|e| e.to_string())?;
            for _ in 0..n + 2 {
                std::env::set_current_dir("..").map_err(|e| e.to_string())?;
            }
            read_marker(secret.strip_prefix("/").unwrap_or(secret), marker)
        })(),
    );
    let _ = std::env::set_current_dir(scope);
    report(
        "list-outside",
        fs::read_dir(outside)
            .map_err(|e| e.to_string())
            .and_then(|d| {
                if d.filter_map(|e| e.ok())
                    .any(|e| e.file_name().to_string_lossy() == name)
                {
                    Ok(())
                } else {
                    Err("listed without the secret".into())
                }
            }),
    );
    let plant = scope.join("plant.txt");
    let _ = fs::write(&plant, "planted");
    let hl = scope.join("hardlink-in");
    fresh(&hl);
    report(
        "hardlink-in",
        fs::hard_link(secret, &hl)
            .map_err(|e| e.to_string())
            .and_then(|()| read_marker(&hl, marker)),
    );
    fresh(&hl);
    let out_link = outside.join("hardlink-out");
    report(
        "hardlink-out",
        fs::hard_link(&plant, &out_link).map_err(|e| e.to_string()),
    );
    fresh(&out_link);
    let stolen = scope.join("stolen");
    report(
        "rename-in",
        fs::rename(secret, &stolen).map_err(|e| e.to_string()),
    );
    let _ = fs::rename(&stolen, secret);
    let moved = outside.join("renamed-out");
    report(
        "rename-out",
        fs::rename(&plant, &moved).map_err(|e| e.to_string()),
    );
    let _ = fs::rename(&moved, &plant);
    let written = outside.join("written-out");
    report(
        "write-out",
        fs::write(&written, "x").map_err(|e| e.to_string()),
    );
    fresh(&written);
    report(
        "delete-out",
        fs::remove_file(victim).map_err(|e| e.to_string()),
    );
    report("race-in-session", race(scope, outside, &name, marker));
    report("stale-dir", stale_dir(scope, outside, &name, marker));
    report("stale-link", stale_link(scope, outside, &name, marker));
    report(
        "inherited-fd",
        (|| {
            let mut buf = [0u8; 4096];
            let n = unsafe { libc::pread(3, buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n <= 0 {
                return Err(libc_err());
            }
            if String::from_utf8_lossy(&buf[..n as usize]).contains(marker) {
                Ok(())
            } else {
                Err("read something else".into())
            }
        })(),
    );
    report(
        "home-config",
        read_marker(home.join(".ssh").join("id_probe"), marker),
    );
    report(
        "env-home",
        match std::env::var_os("HOME") {
            Some(h) => read_marker(Path::new(&h).join(".ssh").join("id_probe"), marker),
            None => Err("HOME unset".into()),
        },
    );
    report(
        "env-token",
        match std::env::var("PROBE_TOKEN") {
            Ok(v) if v.contains(marker) => Ok(()),
            _ => Err("PROBE_TOKEN absent".into()),
        },
    );
    report("sessions-folder", other_sessions(home));
    for scheme in [
        "sudo",
        "file-scope/new",
        "linux-session/new",
        "pty/ptmx",
        "debug",
        "shm",
        "log",
    ] {
        let route = format!("scheme-{}", scheme.split('/').next().unwrap_or(scheme));
        report(
            &route,
            fs::OpenOptions::new()
                .read(true)
                .open(format!("/scheme/{scheme}"))
                .map(|_| ())
                .map_err(|e| e.to_string()),
        );
    }
    raw_disk()
}

pub fn raw_disk() -> i32 {
    let disks: Vec<String> = fs::read_dir("/scheme")
        .map(|d| {
            d.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("disk"))
                .collect()
        })
        .unwrap_or_default();
    report(
        "raw-disk",
        (|| {
            let mut tried = Vec::new();
            for disk in &disks {
                let entries = match fs::read_dir(format!("/scheme/{disk}")) {
                    Ok(entries) => entries,
                    Err(e) => {
                        tried.push(format!("{disk}: {e}"));
                        continue;
                    }
                };
                for entry in entries.filter_map(|e| e.ok()).take(MAX_DISK_ENTRIES) {
                    let mut block = [0u8; 512];
                    match fs::File::open(entry.path()).and_then(|mut f| f.read(&mut block)) {
                        Ok(n) if n > 0 => return Ok(()),
                        Ok(_) => tried.push(format!("{}: empty", entry.path().display())),
                        Err(e) => tried.push(format!("{}: {e}", entry.path().display())),
                    }
                }
            }
            Err(format!(
                "no disk read in /scheme ({} disk schemes named; {})",
                disks.len(),
                tried.join(", ")
            ))
        })(),
    );
    0
}

pub fn schemes() -> i32 {
    let mut names: Vec<String> = fs::read_dir("/scheme")
        .map(|d| {
            d.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    println!("SCHEMES {}", names.join(" "));
    0
}

fn hash_tree(
    base: &Path,
    rel: &Path,
    h: &mut DefaultHasher,
    count: &mut usize,
) -> Result<(), String> {
    let dir = base.join(rel);
    let mut names: Vec<_> = fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .collect();
    names.sort();
    for name in names {
        *count += 1;
        if *count > MAX_TREE {
            return Err("tree too large".into());
        }
        let rel = rel.join(&name);
        let path = base.join(&rel);
        let meta = fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        h.write(rel.to_string_lossy().as_bytes());
        h.write_u32(meta.mode());
        if meta.file_type().is_symlink() {
            h.write(
                fs::read_link(&path)
                    .map_err(|e| e.to_string())?
                    .to_string_lossy()
                    .as_bytes(),
            );
        } else if meta.is_dir() {
            hash_tree(base, &rel, h, count)?;
        } else {
            h.write(&fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?);
        }
    }
    Ok(())
}

pub fn tree_hash(dir: &Path) -> i32 {
    let mut h = DefaultHasher::new();
    let mut count = 0;
    match hash_tree(dir, Path::new(""), &mut h, &mut count) {
        Ok(()) => {
            println!("TREE {:016x} {count}", h.finish());
            0
        }
        Err(e) => {
            println!("TREE error {e}");
            1
        }
    }
}

pub fn home_report() -> i32 {
    let home = std::env::var("HOME").unwrap_or_default();
    let entries = fs::read_dir(&home)
        .map(|d| d.count())
        .map_err(|e| e.to_string());
    let tmp = fs::read_dir("/tmp")
        .map(|d| d.count())
        .map_err(|e| e.to_string());
    let writable = fs::write(Path::new(&home).join(".probe-write"), "x").is_ok();
    let vars: Vec<String> = std::env::vars().map(|(k, _)| k).collect();
    println!(
        "HOME {home} entries {entries:?} tmp {tmp:?} writable {writable} vars {}",
        vars.join(",")
    );
    0
}

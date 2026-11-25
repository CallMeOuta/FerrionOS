use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use ferrion_scope::links::link_leads_outside;
use ferrion_scope::overlay::{LAYER, LOWER, Session};
use ferrion_scope::proxy::END_SESSION;
use ferrion_scope::scope::{Access, Grant, Scope, within};
use libredox::call;
use libredox::flag::{F_SETFD, O_CLOEXEC, O_RDWR};

use crate::sessions::{CLONE_PREFIX, PID, clone_dirs, state_root};
use crate::{
    FOLDER_CAPABILITIES, GrantKind, LaunchPolicy, ManifestPolicy, Options, PASSED_VARIABLES,
    SYSTEM_PATHS, check_pass_fd, check_stdio, child_schemes, format_user_dirs,
    is_valid_wayland_display, new_handles, parse_snapshot, to_close,
};

const SCOPE_SERVICE: &str = "/scheme/file-scope/new";
const MAX_REPORT: usize = 16 << 20;
const REPORT_CHUNK: usize = 4096;
const MAX_TABLE_BYTES: usize = 1 << 20;
const TABLE_CHUNK: usize = 4096;
const MAX_WAIT_INTERRUPTS: usize = 1 << 16;

unsafe extern "C" {
    fn redox_cur_procfd_v0() -> usize;
    fn redox_cur_thrfd_v0() -> usize;
}

fn path_of(fd: usize) -> Result<String, String> {
    let mut buf = [0u8; 4096];
    let len = call::fpath(fd, &mut buf).map_err(|e| format!("fpath of descriptor {fd}: {e}"))?;
    Ok(String::from_utf8_lossy(&buf[..len]).into_owned())
}

pub fn current_thread_fd() -> usize {
    unsafe { redox_cur_thrfd_v0() }
}

fn read_listing(handle: usize) -> Result<Vec<usize>, String> {
    let mut bytes = Vec::new();
    let mut buf = [0u8; TABLE_CHUNK];
    for _ in 0..MAX_TABLE_BYTES / TABLE_CHUNK {
        match call::read(handle, &mut buf) {
            Ok(0) => return parse_snapshot(&bytes),
            Ok(n) => bytes.extend_from_slice(&buf[..n]),
            Err(e) => return Err(format!("cannot read the file table: {e}")),
        }
    }
    Err(format!("a file table of more than {MAX_TABLE_BYTES} bytes"))
}

pub fn file_table(thread_fd: usize) -> Result<Vec<usize>, String> {
    let handle = call::dup(thread_fd, b"filetable-binary")
        .map_err(|e| format!("cannot read the file table: {e}"))?;
    let listed = read_listing(handle);
    let _ = call::close(handle);
    let mut table = listed?;
    table.retain(|fd| *fd != handle);
    Ok(table)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableHandle {
    Own,
    Foreign,
    Other,
}

pub fn table_handle(fd: usize, thread_fd: usize) -> Result<TableHandle, String> {
    let Ok(path) = path_of(fd) else {
        return Ok(TableHandle::Other);
    };
    if !path.starts_with("/scheme/kernel.proc") {
        return Ok(TableHandle::Other);
    }
    let stat = call::fstat(fd).map_err(|e| format!("fstat of descriptor {fd}: {e}"))?;
    if stat.st_size == 0 {
        return Ok(TableHandle::Other);
    }
    let mut own = file_table(thread_fd)?;
    let Ok(fresh) = call::dup(fd, b"refresh") else {
        return Ok(TableHandle::Foreign);
    };
    let listed = read_listing(fresh);
    let _ = call::close(fresh);
    let Ok(mut listed) = listed else {
        return Ok(TableHandle::Foreign);
    };
    own.sort_unstable();
    listed.sort_unstable();
    Ok(if listed == own {
        TableHandle::Own
    } else {
        TableHandle::Foreign
    })
}

fn caller_schemes() -> Result<Vec<String>, String> {
    let entries = fs::read_dir("/scheme").map_err(|e| format!("cannot list /scheme: {e}"))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot list /scheme: {e}"))?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| format!("scheme name {name:?} is not UTF-8"))?;
        names.push(name.to_owned());
    }
    Ok(names)
}

fn runtime_file_table(table: &[usize], skip: &[usize], thread_fd: usize) -> Result<usize, String> {
    let mut own = Vec::new();
    for &fd in table.iter().filter(|fd| !skip.contains(fd)) {
        if table_handle(fd, thread_fd)? == TableHandle::Own {
            own.push(fd);
        }
    }
    match own[..] {
        [fd] => Ok(fd),
        [] => Err("cannot find the runtime's file-table handle".into()),
        _ => Err(format!(
            "more than one handle lists this process's file table: {own:?}"
        )),
    }
}

fn text(path: &Path) -> Result<String, String> {
    let s = path
        .to_str()
        .ok_or_else(|| format!("{} is not UTF-8", path.display()))?;
    if s.contains(['\n', '\0']) {
        return Err(format!("{s:?} holds a newline"));
    }
    Ok(s.to_owned())
}

fn private_dir(path: &Path) -> Result<(), String> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))
}

fn validate_display_socket() -> Result<String, String> {
    let runtime_raw = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| "bad-display-socket: XDG_RUNTIME_DIR is not set".to_owned())?;
    let wayland_raw = std::env::var_os("WAYLAND_DISPLAY")
        .ok_or_else(|| "bad-display-socket: WAYLAND_DISPLAY is not set".to_owned())?;
    let wayland_name = wayland_raw
        .to_str()
        .ok_or_else(|| "bad-display-socket: WAYLAND_DISPLAY is not UTF-8".to_owned())?;
    if !is_valid_wayland_display(wayland_name) {
        return Err(format!(
            "bad-display-socket: WAYLAND_DISPLAY {wayland_name:?} must match wayland-[0-9]{{1,3}}"
        ));
    }
    let runtime_dir = fs::canonicalize(&runtime_raw)
        .map_err(|e| format!("bad-display-socket: cannot resolve XDG_RUNTIME_DIR: {e}"))?;
    let rt_meta = fs::symlink_metadata(&runtime_dir)
        .map_err(|e| format!("bad-display-socket: cannot stat XDG_RUNTIME_DIR: {e}"))?;
    let uid = unsafe { libc::geteuid() };
    if rt_meta.file_type().is_symlink() || !rt_meta.is_dir() {
        return Err("bad-display-socket: XDG_RUNTIME_DIR is not a directory".to_owned());
    }
    if rt_meta.uid() != uid {
        return Err(format!(
            "bad-display-socket: XDG_RUNTIME_DIR is not owned by uid {uid}"
        ));
    }
    if rt_meta.mode() & 0o777 != 0o700 {
        return Err(format!(
            "bad-display-socket: XDG_RUNTIME_DIR mode is {:o}, expected 700",
            rt_meta.mode() & 0o777
        ));
    }
    let socket_path = runtime_dir.join(wayland_name);
    let sock_meta = fs::symlink_metadata(&socket_path).map_err(|e| {
        format!(
            "bad-display-socket: cannot stat {}: {e}",
            socket_path.display()
        )
    })?;
    if sock_meta.file_type().is_symlink() {
        return Err("bad-display-socket: Wayland socket must not be a symlink".to_owned());
    }
    if (sock_meta.mode() & (libc::S_IFMT as u32)) != (libc::S_IFSOCK as u32) {
        return Err("bad-display-socket: Wayland path is not a UNIX socket".to_owned());
    }
    if sock_meta.uid() != uid {
        return Err(format!(
            "bad-display-socket: Wayland socket is not owned by uid {uid}"
        ));
    }
    let socket_text = text(&socket_path)?;
    if let Some(wm_raw) = std::env::var_os("FERRION_WM_SOCKET") {
        let wm_matches = fs::canonicalize(&wm_raw).ok().as_deref() == Some(socket_path.as_path())
            || wm_raw.to_str() == Some(socket_text.as_str());
        if wm_matches {
            return Err(
                "bad-display-socket: Wayland path must not be FERRION_WM_SOCKET".to_owned(),
            );
        }
    }
    Ok(socket_text)
}

fn attach_display_socket(
    dir: &SessionDir,
    scope: &mut Scope,
    real_socket: String,
) -> Result<String, String> {
    let run_path = dir.path().join("run");
    private_dir(&run_path)?;
    let run_text = text(&run_path)?;
    scope.grants.push(Grant {
        virt: run_text.clone(),
        real: run_text.clone(),
        access: Access::ReadWrite,
        layer: None,
    });
    scope.grants.push(Grant {
        virt: format!("{run_text}/wayland-0"),
        real: real_socket,
        access: Access::Socket,
        layer: None,
    });
    Ok(run_text)
}

fn ensure_owned_dir_component(parent: &Path, name: &str, uid: u32) -> Result<PathBuf, String> {
    let child = parent.join(name);
    match fs::symlink_metadata(&child) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Err(create_err) = fs::DirBuilder::new().mode(0o700).create(&child)
                && create_err.kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(format!("cannot create {}: {create_err}", child.display()));
            }
        }
        Err(e) => {
            return Err(format!(
                "unsafe-app-home: cannot stat {}: {e}",
                child.display()
            ));
        }
        Ok(_) => {}
    }
    let meta = fs::symlink_metadata(&child)
        .map_err(|e| format!("unsafe-app-home: cannot stat {}: {e}", child.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!("unsafe-app-home: {} is a symlink", child.display()));
    }
    if !meta.is_dir() {
        return Err(format!(
            "unsafe-app-home: {} is not a directory",
            child.display()
        ));
    }
    if meta.uid() != uid {
        return Err(format!(
            "unsafe-app-home: {} is not owned by uid {uid}",
            child.display()
        ));
    }
    Ok(child)
}

fn ensure_app_home(caller_home: &Path, id: &str) -> Result<(PathBuf, PathBuf), String> {
    let uid = unsafe { libc::geteuid() };
    let cur = ensure_owned_dir_component(caller_home, ".local", uid)?;
    let cur = ensure_owned_dir_component(&cur, "share", uid)?;
    let cur = ensure_owned_dir_component(&cur, "ferrion", uid)?;
    let apps_root = ensure_owned_dir_component(&cur, "apps", uid)?;
    let app_dir = ensure_owned_dir_component(&apps_root, id, uid)?;
    let app_home = ensure_owned_dir_component(&app_dir, "home", uid)?;
    Ok((apps_root, app_home))
}

fn write_user_dirs_in_scope(app_home: &str, content: &str) -> Result<(), String> {
    let uid = unsafe { libc::geteuid() };
    let config_dir = ensure_owned_dir_component(Path::new(app_home), ".config", uid)?;
    let target = config_dir.join("user-dirs.dirs");
    match fs::symlink_metadata(&target) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_file() {
                return Err(format!(
                    "unsafe-app-home: {} is a symlink or not a regular file",
                    target.display()
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!(
                "unsafe-app-home: cannot stat {}: {e}",
                target.display()
            ));
        }
    }
    let tmp = config_dir.join(format!(".user-dirs.dirs.tmp.{}", process::id()));
    if let Ok(meta) = fs::symlink_metadata(&tmp) {
        if meta.file_type().is_symlink() {
            return Err(format!("unsafe-app-home: {} is a symlink", tmp.display()));
        }
        let _ = fs::remove_file(&tmp);
    }
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)
            .map_err(|e| format!("unsafe-app-home: cannot create {}: {e}", tmp.display()))?;
        file.write_all(content.as_bytes())
            .map_err(|e| format!("unsafe-app-home: cannot write {}: {e}", tmp.display()))?;
    }
    fs::rename(&tmp, &target).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("unsafe-app-home: cannot replace {}: {e}", target.display())
    })
}

enum Life {
    Preparing,
    Ran { clones: usize },
}

struct SessionDir {
    dir: PathBuf,
    life: Life,
}

impl SessionDir {
    fn create(dir: PathBuf) -> Result<Self, String> {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        Ok(Self {
            dir,
            life: Life::Preparing,
        })
    }

    fn path(&self) -> &Path {
        &self.dir
    }

    fn write_pid(&self) -> Result<(), String> {
        fs::write(self.dir.join(PID), process::id().to_string())
            .map_err(|e| format!("cannot write {}: {e}", self.dir.display()))
    }

    fn mark_ran(&mut self, clones: usize) {
        assert!(matches!(self.life, Life::Preparing));
        self.life = Life::Ran { clones };
    }

    fn has_run(&self) -> bool {
        matches!(self.life, Life::Ran { .. })
    }
}

impl Drop for SessionDir {
    fn drop(&mut self) {
        match self.life {
            Life::Preparing | Life::Ran { clones: 0 } => {
                let _ = fs::remove_dir_all(&self.dir);
            }
            Life::Ran { .. } => {
                let _ = fs::remove_dir_all(self.dir.join("home"));
                let _ = fs::remove_dir_all(self.dir.join("tmp"));
                let _ = fs::remove_dir_all(self.dir.join("run"));
                let _ = fs::remove_file(self.dir.join(PID));
            }
        }
    }
}

struct Prepared {
    dir: SessionDir,
    home: String,
    run_dir: Option<String>,
    scope: Scope,
    clones: usize,
    xdg_entries: Vec<(&'static str, String)>,
    user_dirs: Option<String>,
}

fn prepare_manifest_session(policy: &ManifestPolicy) -> Result<Prepared, String> {
    let display_socket = if policy.allow_display && std::env::var_os("WAYLAND_DISPLAY").is_some() {
        Some(validate_display_socket()?)
    } else {
        None
    };
    let caller_home_raw = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let caller_home =
        fs::canonicalize(&caller_home_raw).map_err(|e| format!("cannot resolve HOME: {e}"))?;
    let caller_home_text = text(&caller_home)?;
    let root = state_root()?;
    private_dir(&root)?;
    let root =
        fs::canonicalize(&root).map_err(|e| format!("cannot resolve {}: {e}", root.display()))?;
    let root_text = text(&root)?;
    let (apps_root, app_home_path) = ensure_app_home(&caller_home, &policy.id)?;
    let apps_root_text = text(&apps_root)?;
    let app_home_text = text(&app_home_path)?;

    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let id = format!("{seconds}-{}", process::id());
    let dir = SessionDir::create(root.join(&id))?;
    dir.write_pid()?;
    let tmp = dir.path().join("tmp");
    private_dir(&tmp)?;

    let mut scope = Scope {
        grants: Vec::new(),
        hidden: vec![root_text.clone(), apps_root_text.clone()],
    };
    if policy.allow_home {
        scope.grants.push(Grant {
            virt: caller_home_text.clone(),
            real: caller_home_text.clone(),
            access: Access::ReadWrite,
            layer: None,
        });
    }
    let uid = unsafe { libc::geteuid() };
    let mut xdg_entries = Vec::new();
    for &(_cap, folder_name, xdg_var) in &policy.granted_folders {
        let candidate = caller_home.join(folder_name);
        let Ok(meta) = fs::symlink_metadata(&candidate) else {
            continue;
        };
        if meta.file_type().is_symlink() || !meta.is_dir() || meta.uid() != uid {
            continue;
        }
        let Ok(real) = fs::canonicalize(&candidate) else {
            continue;
        };
        if real != candidate {
            continue;
        }
        let real_text = text(&real)?;
        if within(&real_text, &root_text) || within(&real_text, &apps_root_text) {
            continue;
        }
        if !scope.grants.iter().any(|g| g.virt == real_text) {
            scope.grants.push(Grant {
                virt: real_text.clone(),
                real: real_text.clone(),
                access: Access::ReadWrite,
                layer: None,
            });
        }
        xdg_entries.push((xdg_var, real_text));
    }
    for system in SYSTEM_PATHS {
        if fs::symlink_metadata(system).is_ok() && !scope.grants.iter().any(|g| g.virt == system) {
            scope.grants.push(Grant {
                virt: system.into(),
                real: system.into(),
                access: Access::ReadOnly,
                layer: None,
            });
        }
    }
    scope.grants.push(Grant {
        virt: app_home_text.clone(),
        real: app_home_text.clone(),
        access: Access::ReadWrite,
        layer: None,
    });
    if !scope.grants.iter().any(|g| g.virt == "/tmp") {
        scope.grants.push(Grant {
            virt: "/tmp".into(),
            real: text(&tmp)?,
            access: Access::ReadWrite,
            layer: None,
        });
    }
    let run_dir = match display_socket {
        Some(real_socket) => Some(attach_display_socket(&dir, &mut scope, real_socket)?),
        None => None,
    };
    let user_dirs = Some(format_user_dirs(&xdg_entries));
    Ok(Prepared {
        dir,
        home: app_home_text,
        run_dir,
        scope,
        clones: 0,
        xdg_entries,
        user_dirs,
    })
}

fn prepare_legacy_session(options: &Options) -> Result<Prepared, String> {
    let display_socket = if options.display {
        Some(validate_display_socket()?)
    } else {
        None
    };
    let root = state_root()?;
    private_dir(&root)?;
    let root =
        fs::canonicalize(&root).map_err(|e| format!("cannot resolve {}: {e}", root.display()))?;
    let root_text = text(&root)?;
    let mut validated: Vec<(GrantKind, String, u32)> = Vec::with_capacity(options.grants.len());
    for (kind, path) in &options.grants {
        let real = fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let real_text = text(&real)?;
        if within(&real_text, &root_text) {
            return Err(format!("{real_text} is inside ferrion-run's own sessions"));
        }
        let meta = fs::metadata(&real).map_err(|e| format!("{real_text}: {e}"))?;
        if *kind != GrantKind::ReadOnly && !meta.is_dir() {
            return Err(format!("{real_text} is not a folder"));
        }
        if validated.iter().any(|(_, v, _)| v == &real_text) {
            return Err(format!("{real_text} is given twice"));
        }
        validated.push((*kind, real_text, meta.permissions().mode() & 0o7777));
    }
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let id = format!("{seconds}-{}", process::id());
    let dir = SessionDir::create(root.join(&id))?;
    dir.write_pid()?;
    let home = dir.path().join("home");
    let tmp = dir.path().join("tmp");
    private_dir(&home)?;
    private_dir(&tmp)?;
    let mut scope = Scope {
        grants: Vec::new(),
        hidden: vec![root_text],
    };
    let mut clones = 0;
    for (kind, real_text, mode) in validated {
        let (access, layer) = match kind {
            GrantKind::Files => (Access::ReadWrite, None),
            GrantKind::ReadOnly => (Access::ReadOnly, None),
            GrantKind::Clone => {
                let clone = dir.path().join(format!("{CLONE_PREFIX}{clones}"));
                clones += 1;
                private_dir(&clone.join(LAYER))?;
                fs::set_permissions(clone.join(LAYER), fs::Permissions::from_mode(mode))
                    .map_err(|e| format!("cannot set the layer's mode: {e}"))?;
                fs::write(clone.join(LOWER), &real_text)
                    .map_err(|e| format!("cannot write {}: {e}", clone.display()))?;
                (Access::Clone, Some(text(&clone.join(LAYER))?))
            }
        };
        scope.grants.push(Grant {
            virt: real_text.clone(),
            real: real_text,
            access,
            layer,
        });
    }
    for system in SYSTEM_PATHS {
        if fs::symlink_metadata(system).is_ok() && !scope.grants.iter().any(|g| g.virt == system) {
            scope.grants.push(Grant {
                virt: system.into(),
                real: system.into(),
                access: Access::ReadOnly,
                layer: None,
            });
        }
    }
    let home = text(&home)?;
    scope.grants.push(Grant {
        virt: home.clone(),
        real: home.clone(),
        access: Access::ReadWrite,
        layer: None,
    });
    if !scope.grants.iter().any(|g| g.virt == "/tmp") {
        scope.grants.push(Grant {
            virt: "/tmp".into(),
            real: text(&tmp)?,
            access: Access::ReadWrite,
            layer: None,
        });
    }
    let run_dir = match display_socket {
        Some(real_socket) => Some(attach_display_socket(&dir, &mut scope, real_socket)?),
        None => None,
    };
    Ok(Prepared {
        dir,
        home,
        run_dir,
        scope,
        clones,
        xdg_entries: Vec::new(),
        user_dirs: None,
    })
}

struct ProxySession {
    session: Option<usize>,
    root: Option<usize>,
}

impl ProxySession {
    fn open(scope: &Scope) -> Result<Self, String> {
        let ctl = call::open(SCOPE_SERVICE, O_RDWR | O_CLOEXEC, 0)
            .map_err(|e| format!("cannot reach ferrion-scoped at {SCOPE_SERVICE}: {e}"))?;
        let session = call::openat(ctl, "session", O_RDWR | O_CLOEXEC, 0);
        let _ = call::close(ctl);
        let session = session.map_err(|e| format!("ferrion-scoped refused a session: {e}"))?;
        let mut guard = Self {
            session: Some(session),
            root: None,
        };
        let spec = scope.encode();
        let written = call::write(session, &spec)
            .map_err(|e| format!("cannot hand the scope to the proxy: {e}"))?;
        if written != spec.len() {
            return Err("the proxy took part of the scope".into());
        }
        let root = call::openat(session, "root", O_CLOEXEC, 0)
            .map_err(|e| format!("the proxy refused the scope: {e}"))?;
        guard.root = Some(root);
        Ok(guard)
    }

    fn root(&self) -> usize {
        self.root.expect("proxy session root is open")
    }

    fn close_root(&mut self) {
        if let Some(root) = self.root.take() {
            let _ = call::close(root);
        }
    }

    fn end(mut self) -> (Result<(), libredox::error::Error>, Vec<u8>) {
        self.close_root();
        let session = self.session.take().expect("proxy session is open");
        let ended = call::write(session, END_SESSION).map(|_| ());
        let report = read_report(session);
        let _ = call::close(session);
        (ended, report)
    }
}

impl Drop for ProxySession {
    fn drop(&mut self) {
        self.close_root();
        if let Some(session) = self.session.take() {
            let _ = call::write(session, END_SESSION);
            let _ = call::close(session);
        }
    }
}

fn child_env(
    options: &Options,
    home: &str,
    run_dir: Option<&str>,
    manifest_mode: bool,
    xdg_entries: &[(&'static str, String)],
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = PASSED_VARIABLES
        .iter()
        .filter_map(|name| std::env::var_os(name).map(|v| (OsString::from(name), v)))
        .collect();
    for name in &options.env {
        if let Some(value) = std::env::var_os(name) {
            env.retain(|(n, _)| n != name);
            env.push((name.clone(), value));
        }
    }
    if manifest_mode {
        for &(_, _, xdg_var) in &FOLDER_CAPABILITIES {
            env.retain(|(n, _)| n != xdg_var);
        }
        for (xdg_var, path) in xdg_entries {
            env.push((OsString::from(xdg_var), OsString::from(path)));
        }
    }
    env.retain(|(n, _)| {
        n != "HOME"
            && n != "TMPDIR"
            && n != "FERRION_WM_SOCKET"
            && n != "XDG_RUNTIME_DIR"
            && n != "WAYLAND_DISPLAY"
    });
    env.push(("HOME".into(), home.into()));
    env.push(("TMPDIR".into(), "/tmp".into()));
    if let Some(run_dir) = run_dir {
        env.push(("XDG_RUNTIME_DIR".into(), run_dir.into()));
        env.push(("WAYLAND_DISPLAY".into(), "wayland-0".into()));
    }
    env
}

fn enter(
    options: &Options,
    exec_path: &OsStr,
    namespace: usize,
    start: &Path,
    env: &[(OsString, OsString)],
    home: &str,
    user_dirs: Option<&str>,
) -> String {
    let result = (|| -> Result<(), String> {
        let old = call::setns(namespace).map_err(|e| format!("cannot enter the namespace: {e}"))?;
        call::close(old).map_err(|e| format!("cannot close the caller's namespace: {e}"))?;
        if let Some(content) = user_dirs {
            write_user_dirs_in_scope(home, content)?;
        }
        let (proc_fd, thread_fd) = unsafe { (redox_cur_procfd_v0(), redox_cur_thrfd_v0()) };
        let before = file_table(thread_fd)?;
        std::env::set_current_dir(start)
            .map_err(|e| format!("cannot enter {} in the session: {e}", start.display()))?;
        let table = file_table(thread_fd)?;
        let cwd_fd = match new_handles(&before, &table)[..] {
            [fd] => fd,
            ref other => {
                return Err(format!(
                    "expected one new working-directory handle, found {other:?}"
                ));
            }
        };
        let namespace_fd =
            call::getns().map_err(|e| format!("cannot read the namespace handle: {e}"))?;
        let mut skip = Vec::with_capacity(5);
        skip.extend_from_slice(&[proc_fd, thread_fd, namespace_fd, cwd_fd]);
        if let Some(fd) = options.pass_fd {
            if skip.contains(&fd) {
                return Err(format!(
                    "--pass-fd {fd} collides with a runtime, namespace or cwd handle"
                ));
            }
            skip.push(fd);
        }
        let file_table_fd = runtime_file_table(&table, &skip, thread_fd)?;
        let mut keep = Vec::with_capacity(6);
        keep.extend_from_slice(&[proc_fd, thread_fd, file_table_fd, namespace_fd, cwd_fd]);
        if let Some(fd) = options.pass_fd {
            if keep.contains(&fd) {
                return Err(format!(
                    "--pass-fd {fd} collides with a runtime, namespace or cwd handle"
                ));
            }
            call::fcntl(fd, F_SETFD as usize, 0)
                .map_err(|e| format!("cannot keep passed descriptor {fd} across exec: {e}"))?;
            keep.push(fd);
        }
        for fd in to_close(&table, &keep) {
            call::close(fd).map_err(|e| format!("cannot close inherited descriptor {fd}: {e}"))?;
        }
        let left = to_close(&file_table(thread_fd)?, &keep);
        if !left.is_empty() {
            return Err(format!("inherited descriptors still open: {left:?}"));
        }
        Ok(())
    })();
    if let Err(reason) = result {
        return reason;
    }
    let error = process::Command::new(exec_path)
        .args(&options.args)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .exec();
    format!("cannot run {}: {error}", exec_path.to_string_lossy())
}

fn read_report(session: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; REPORT_CHUNK];
    for _ in 0..MAX_REPORT / REPORT_CHUNK {
        match call::read(session, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
        }
    }
    out
}

fn report_links(report: &[u8], prepared: &Prepared) {
    let own = prepared.dir.path().to_string_lossy();
    for line in String::from_utf8_lossy(report).lines() {
        if let Some(more) = line.strip_prefix('+') {
            eprintln!(
                "ferrion-run: the session wrote {more} more symlinks than ferrion-run lists; look for symlinks in its folders with `find DIR -type l`"
            );
            continue;
        }
        if within(line, &own) {
            continue;
        }
        let root = prepared
            .scope
            .grants
            .iter()
            .filter(|g| g.access == Access::ReadWrite && within(line, &g.real))
            .map(|g| g.real.as_str())
            .max_by_key(|r| r.len());
        let Some(root) = root else { continue };
        if let Some(outside) = link_leads_outside(line, root) {
            eprintln!(
                "ferrion-run: the session left a symlink that leads outside {root}: {line} -> {}",
                outside.describe()
            );
        }
    }
}

fn report_layer_links(dir: &Path, clones: usize) {
    for n in 0..clones {
        let clone = dir.join(format!("{CLONE_PREFIX}{n}"));
        let Ok(session) = Session::open(&clone) else {
            continue;
        };
        let Ok(found) = session.outside_links() else {
            continue;
        };
        let lower = session.lower.display();
        for (rel, outside) in found {
            eprintln!(
                "ferrion-run: apply would write a symlink that leads outside {lower}: {lower}/{rel} -> {}",
                outside.describe()
            );
        }
    }
}

fn wait(pid: libc::pid_t) -> Result<i32, String> {
    let mut status = 0;
    for _ in 0..MAX_WAIT_INTERRUPTS {
        let r = unsafe { libc::waitpid(pid, &mut status, 0) };
        if r == pid {
            return Ok(if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else if libc::WIFSIGNALED(status) {
                128 + libc::WTERMSIG(status)
            } else {
                EXIT_UNKNOWN
            });
        }
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EINTR) {
            return Err(format!("cannot wait for the program: {e}"));
        }
    }
    Err(format!(
        "waiting for the program was interrupted {MAX_WAIT_INTERRUPTS} times"
    ))
}

const EXIT_UNKNOWN: i32 = 126;

pub enum Outcome {
    Refused(String),
    Exited(i32),
}

pub fn run(options: &Options, policy: &LaunchPolicy) -> Outcome {
    for fd in 0..3 {
        match path_of(fd) {
            Ok(path) => {
                if let Err(reason) = check_stdio(fd, &path) {
                    return Outcome::Refused(reason);
                }
            }
            Err(_) if call::fstat(fd).is_err() => {}
            Err(e) => return Outcome::Refused(e),
        }
    }
    if let Some(fd) = options.pass_fd {
        match path_of(fd) {
            Ok(path) => {
                if let Err(reason) = check_pass_fd(fd, &path) {
                    return Outcome::Refused(reason);
                }
            }
            Err(e) => return Outcome::Refused(format!("--pass-fd {fd}: {e}")),
        }
    }
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => return Outcome::Refused(format!("cannot read the working directory: {e}")),
    };
    let mut prepared = match policy {
        LaunchPolicy::Manifest(manifest_policy) => {
            match prepare_manifest_session(manifest_policy) {
                Ok(prepared) => prepared,
                Err(reason) => return Outcome::Refused(reason),
            }
        }
        LaunchPolicy::Legacy { .. } => match prepare_legacy_session(options) {
            Ok(prepared) => prepared,
            Err(reason) => return Outcome::Refused(reason),
        },
    };
    assert!(prepared.dir.path().join(PID).is_file());
    let outcome = launch(options, policy, &cwd, &mut prepared);
    if prepared.dir.has_run() {
        match clone_dirs(prepared.dir.path()) {
            Ok(actual) if actual.len() != prepared.clones => {
                eprintln!(
                    "ferrion-run: expected {} clone layers in {}, found {}",
                    prepared.clones,
                    prepared.dir.path().display(),
                    actual.len()
                );
            }
            Err(e) => {
                eprintln!(
                    "ferrion-run: cannot inspect clone layers in {}: {e}",
                    prepared.dir.path().display()
                );
            }
            Ok(_) => {}
        }
        if prepared.clones > 0
            && let Some(id) = prepared.dir.path().file_name()
        {
            report_layer_links(prepared.dir.path(), prepared.clones);
            let id = id.to_string_lossy();
            eprintln!(
                "ferrion-run: session {id} keeps its changes in its layer; see `ferrion-run diff {id}`, then `ferrion-run apply {id}` or `ferrion-run discard {id}`"
            );
        }
    }
    outcome
}

fn launch(
    options: &Options,
    policy: &LaunchPolicy,
    cwd: &Path,
    prepared: &mut Prepared,
) -> Outcome {
    let (withhold_network, allow_sudo, pty, allow_audio, manifest_mode) = match policy {
        LaunchPolicy::Manifest(p) => (!p.allow_network, false, false, p.allow_audio, true),
        LaunchPolicy::Legacy {
            withhold_network,
            allow_sudo,
        } => (*withhold_network, *allow_sudo, options.pty, false, false),
    };
    let schemes = match caller_schemes() {
        Ok(caller) => child_schemes(&caller, withhold_network, allow_sudo, pty, allow_audio),
        Err(reason) => return Outcome::Refused(reason),
    };
    let mut proxy = match ProxySession::open(&prepared.scope) {
        Ok(proxy) => proxy,
        Err(reason) => return Outcome::Refused(reason),
    };
    let names: Vec<ioslice::IoSlice> = schemes
        .iter()
        .map(|s| ioslice::IoSlice::new(s.as_bytes()))
        .collect();
    let namespace = match call::mkns(&names) {
        Ok(ns) => ns,
        Err(e) => return Outcome::Refused(format!("cannot create the namespace: {e}")),
    };
    if let Err(e) = call::register_scheme_to_ns(namespace, "file", proxy.root()) {
        let _ = call::close(namespace);
        return Outcome::Refused(format!("cannot give the session its file scheme: {e}"));
    }
    let start = match cwd.to_str() {
        Some(c)
            if prepared
                .scope
                .grants
                .iter()
                .any(|g| g.virt != "/tmp" && within(c, &g.virt)) =>
        {
            cwd.to_owned()
        }
        _ => PathBuf::from(&prepared.home),
    };
    let env = child_env(
        options,
        &prepared.home,
        prepared.run_dir.as_deref(),
        manifest_mode,
        &prepared.xdg_entries,
    );
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        let _ = call::close(namespace);
        return Outcome::Refused(format!(
            "cannot start the program: {}",
            std::io::Error::last_os_error()
        ));
    }
    let exec_path = match policy {
        LaunchPolicy::Manifest(p) => p.resolved_exec.as_os_str(),
        LaunchPolicy::Legacy { .. } => options.program.as_os_str(),
    };
    if pid == 0 {
        let reason = enter(
            options,
            exec_path,
            namespace,
            &start,
            &env,
            &prepared.home,
            prepared.user_dirs.as_deref(),
        );
        eprintln!("ferrion-run: {reason}");
        unsafe { libc::_exit(crate::EXIT_REFUSED) };
    }
    prepared.dir.mark_ran(prepared.clones);
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGQUIT, libc::SIG_IGN);
    }
    let _ = call::close(namespace);
    proxy.close_root();
    let status = wait(pid);
    let (ended, report) = proxy.end();
    if let Err(e) = ended {
        eprintln!("ferrion-run: the proxy did not end the session: {e}");
    }
    report_links(&report, prepared);
    match status {
        Ok(code) => Outcome::Exited(code),
        Err(reason) => Outcome::Refused(reason),
    }
}

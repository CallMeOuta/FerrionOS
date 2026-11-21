//! The launch on FerrionOS: checks the standard descriptors, moves this process into a new
//! namespace, closes every inherited descriptor but the runtime's own, and executes the
//! program in place. No thread is started, so the file table cannot change underneath.

use std::fs;
use std::os::unix::process::CommandExt;
use std::process;

use libredox::call;

use crate::{Options, check_stdio, child_schemes, new_handles, parse_snapshot, to_close};

// relibc's handles for this process and this thread (relibc/src/platform/redox/libredox.rs,
// marked ABI-unstable there, so tied to the relibc rev pinned in recipes/core/relibc).
unsafe extern "C" {
    fn redox_cur_procfd_v0() -> usize;
    fn redox_cur_thrfd_v0() -> usize;
}

fn path_of(fd: usize) -> Result<String, String> {
    let mut buf = [0u8; 4096];
    let len = call::fpath(fd, &mut buf).map_err(|e| format!("fpath of descriptor {fd}: {e}"))?;
    Ok(String::from_utf8_lossy(&buf[..len]).into_owned())
}

/// Every descriptor in this process's file table, posix and upper (tagged) alike, read from
/// a fresh `filetable-binary` handle on the current thread. The snapshot handle itself is
/// left out and closed before returning.
fn file_table(thread_fd: usize) -> Result<Vec<usize>, String> {
    let handle = call::dup(thread_fd, b"filetable-binary")
        .map_err(|e| format!("cannot read the file table: {e}"))?;
    let mut bytes = Vec::new();
    let mut buf = [0u8; 4096];
    let read = loop {
        match call::read(handle, &mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => bytes.extend_from_slice(&buf[..n]),
            Err(e) => break Err(format!("cannot read the file table: {e}")),
        }
    };
    let _ = call::close(handle);
    read?;
    let mut table = parse_snapshot(&bytes)?;
    table.retain(|fd| *fd != handle);
    Ok(table)
}

/// The scheme names of the caller's namespace, from its `/scheme` listing.
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

/// relibc's handle on this process's file table: the one kernel `proc` handle besides the
/// thread's whose size is not zero (only file-table handles carry a snapshot, so a size).
/// More than one candidate means an inherited handle looks the same, and the launch is
/// refused rather than guessed.
fn runtime_file_table(table: &[usize], skip: &[usize]) -> Result<usize, String> {
    let mut found = Vec::new();
    for &fd in table.iter().filter(|fd| !skip.contains(fd)) {
        let Ok(path) = path_of(fd) else { continue };
        if !path.starts_with("/scheme/kernel.proc") {
            continue;
        }
        let stat = call::fstat(fd).map_err(|e| format!("fstat of descriptor {fd}: {e}"))?;
        if stat.st_size > 0 {
            found.push(fd);
        }
    }
    match found[..] {
        [fd] => Ok(fd),
        [] => Err("cannot find the runtime's file-table handle".into()),
        _ => Err(format!(
            "more than one file-table handle is open: {found:?}"
        )),
    }
}

/// Runs the launch; returns only on failure, with the reason.
pub fn run(options: &Options, withhold_network: bool, allow_sudo: bool) -> String {
    match prepare(withhold_network, allow_sudo) {
        Ok(()) => {
            let error = process::Command::new(&options.program)
                .args(&options.args)
                .exec();
            format!("cannot run {}: {error}", options.program.to_string_lossy())
        }
        Err(reason) => reason,
    }
}

fn prepare(withhold_network: bool, allow_sudo: bool) -> Result<(), String> {
    // Standard input, output and error pass to the program; refuse a socket or a disk file.
    for fd in 0..3 {
        match path_of(fd) {
            Ok(path) => check_stdio(fd, &path)?,
            // A closed standard descriptor passes nothing.
            Err(_) if call::fstat(fd).is_err() => {}
            Err(e) => return Err(e),
        }
    }

    let cwd =
        std::env::current_dir().map_err(|e| format!("cannot read the working directory: {e}"))?;
    let schemes = child_schemes(&caller_schemes()?, withhold_network, allow_sudo);
    let names: Vec<ioslice::IoSlice> = schemes
        .iter()
        .map(|s| ioslice::IoSlice::new(s.as_bytes()))
        .collect();
    let namespace = call::mkns(&names).map_err(|e| format!("cannot create the namespace: {e}"))?;
    let old_namespace =
        call::setns(namespace).map_err(|e| format!("cannot enter the namespace: {e}"))?;
    call::close(old_namespace).map_err(|e| format!("cannot close the caller's namespace: {e}"))?;

    // SAFETY: plain getters of relibc's runtime state, no arguments.
    let (proc_fd, thread_fd) = unsafe { (redox_cur_procfd_v0(), redox_cur_thrfd_v0()) };

    // The working directory handle still comes from the caller's namespace; reopen it
    // through the new one and find the new handle by comparing the file table around it.
    let before = file_table(thread_fd)?;
    std::env::set_current_dir(&cwd)
        .map_err(|e| format!("cannot enter {} in the new namespace: {e}", cwd.display()))?;
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
    let file_table_fd = runtime_file_table(&table, &[proc_fd, thread_fd, namespace_fd, cwd_fd])?;

    // What the program keeps besides descriptors 0, 1 and 2: relibc's own runtime handles,
    // which exec passes to the new image and which no program code opened.
    let keep = [
        // The process handle: relibc's capability on this process (signals, ids, exit).
        proc_fd,
        // The thread handle: the current thread, through which exec replaces the image.
        thread_fd,
        // The file-table handle relibc keeps for this table; exec drops it itself.
        file_table_fd,
        // The new namespace, without the withheld schemes.
        namespace_fd,
        // The working directory, opened through the new namespace above.
        cwd_fd,
    ];
    for fd in to_close(&table, &keep) {
        // libredox's close goes through relibc, so its runtime table forgets the descriptor
        // too; that matters for upper (tagged) descriptors, which exec would otherwise pass.
        call::close(fd).map_err(|e| format!("cannot close inherited descriptor {fd}: {e}"))?;
    }
    let left = to_close(&file_table(thread_fd)?, &keep);
    if !left.is_empty() {
        return Err(format!("inherited descriptors still open: {left:?}"));
    }
    Ok(())
}

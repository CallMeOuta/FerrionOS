extern crate libc;
extern crate redoxfs;
#[cfg(target_os = "redox")]
extern crate syscall;
extern crate uuid;

use std::env;
use std::fs::File;
use std::io::{self, Read, Write};
#[cfg(target_os = "redox")]
use std::os::fd::AsRawFd;
use std::os::unix::io::{FromRawFd, RawFd};
use std::process;
use std::{ptr, slice, sync::atomic};

#[cfg(target_os = "redox")]
use std::{mem::MaybeUninit, ptr::addr_of_mut, sync::atomic::Ordering};

#[cfg(target_os = "redox")]
use redoxfs::ring::DiskRing;
use redoxfs::{Disk, DiskCache, DiskFile, FileSystem, mount};
use termion::input::TermRead;
use uuid::Uuid;

#[cfg(target_os = "redox")]
extern "C" fn unmount_handler(_s: usize) {
    redoxfs::IS_UMT.store(1, Ordering::SeqCst);
}

#[cfg(target_os = "redox")]
//set up a signal handler on redox, this implements unmounting. I have no idea what sa_flags is
//for, so I put 2. I don't think 0,0 is a valid sa_mask. I don't know what i'm doing here. When u
//send it a sigkill, it shuts off the filesystem
fn setsig() {
    // TODO: High-level wrapper like the nix crate?
    unsafe {
        let mut action = MaybeUninit::<libc::sigaction>::uninit();

        assert_eq!(
            libc::sigemptyset(addr_of_mut!((*action.as_mut_ptr()).sa_mask)),
            0
        );
        addr_of_mut!((*action.as_mut_ptr()).sa_flags).write(0);
        addr_of_mut!((*action.as_mut_ptr()).sa_sigaction)
            .write(unmount_handler as *const () as usize);

        assert_eq!(
            libc::sigaction(libc::SIGTERM, action.as_ptr(), core::ptr::null_mut()),
            0
        );
    }
}

#[cfg(not(target_os = "redox"))]
// on linux, this is implemented properly, so no need for this unscrupulous nonsense!
fn setsig() {}

fn fork() -> isize {
    unsafe { libc::fork() as isize }
}

fn pipe(pipes: &mut [i32; 2]) -> isize {
    unsafe { libc::pipe(pipes.as_mut_ptr()) as isize }
}

#[cfg(not(target_os = "redox"))]
fn capability_mode() {}

#[cfg(not(target_os = "redox"))]
fn bootloader_password() -> Option<Passphrase> {
    None
}

/// Overwrites a buffer that held a passphrase with zeros. The writes are volatile so the compiler
/// keeps them although nothing reads the buffer afterwards.
fn wipe(buf: &mut [u8]) {
    for byte in buf.iter_mut() {
        unsafe { ptr::write_volatile(byte, 0) };
    }
    atomic::compiler_fence(atomic::Ordering::SeqCst);
}

/// A passphrase, overwritten with zeros when dropped.
struct Passphrase(Vec<u8>);

impl Drop for Passphrase {
    fn drop(&mut self) {
        // The whole allocation, in case it ever held a longer value
        let capacity = self.0.capacity();
        wipe(unsafe { slice::from_raw_parts_mut(self.0.as_mut_ptr(), capacity) });
    }
}

#[cfg(target_os = "redox")]
fn capability_mode() {
    libredox::call::setrens(0, 0).expect("redoxfs: failed to enter null namespace");
}

/// Reads the passphrase the bootloader unlocked the disk with, then overwrites the bootloader's
/// page with zeros so that the passphrase does not stay in physical memory.
#[cfg(target_os = "redox")]
fn bootloader_password() -> Option<Passphrase> {
    use libredox::call::MmapArgs;

    let addr_env = env::var_os("REDOXFS_PASSWORD_ADDR")?;
    let size_env = env::var_os("REDOXFS_PASSWORD_SIZE")?;

    let addr = usize::from_str_radix(
        addr_env.to_str().expect("REDOXFS_PASSWORD_ADDR not valid"),
        16,
    )
    .expect("failed to parse REDOXFS_PASSWORD_ADDR");

    let size = usize::from_str_radix(
        size_env.to_str().expect("REDOXFS_PASSWORD_SIZE not valid"),
        16,
    )
    .expect("failed to parse REDOXFS_PASSWORD_SIZE");

    let mut password = Vec::with_capacity(size);
    unsafe {
        let aligned_size = size.next_multiple_of(syscall::PAGE_SIZE);

        let fd = libredox::Fd::open(
            "memory:physical",
            libredox::flag::O_RDWR | libredox::flag::O_CLOEXEC,
            0,
        )
        .expect("failed to open physical memory file");

        let password_map = libredox::call::mmap(MmapArgs {
            addr: core::ptr::null_mut(),
            length: aligned_size,
            prot: libredox::flag::PROT_READ | libredox::flag::PROT_WRITE,
            flags: libredox::flag::MAP_SHARED,
            fd: fd.raw(),
            offset: addr as u64,
        })
        .expect("failed to map REDOXFS_PASSWORD")
        .cast::<u8>();

        for i in 0..size {
            password.push(password_map.add(i).read());
        }
        wipe(slice::from_raw_parts_mut(password_map, size));

        let _ = libredox::call::munmap(password_map.cast(), aligned_size);
    }
    Some(Passphrase(password))
}

fn print_err_exit(err: impl AsRef<str>) -> ! {
    eprintln!("redoxfs: {}", err.as_ref());
    usage();
    process::exit(1)
}

fn print_usage_exit() -> ! {
    usage();
    process::exit(1)
}

fn usage() {
    eprintln!(
        "redoxfs [--no-daemon|-d] [--key-file PATH] [--read-only] [--flush-on-unmount] [--uuid] [disk or uuid] [mountpoint] [block in hex]"
    );
    eprintln!("redoxfs [--no-daemon|-d] --memory MIB [mountpoint]");
}

enum DiskId {
    Path(String),
    Uuid(Uuid),
    /// An image served without writing to it (FerrionOS, the Linux layer's root).
    ReadOnly(String),
    /// A new file system of this many bytes in memory, gone when the daemon exits.
    Memory(u64),
}

/// Opens the image at `path` for reading only. The disk is never written: transactions are
/// dropped instead of committed and the scheme refuses every change with EROFS.
fn filesystem_read_only(path: &str) -> Option<(String, FileSystem<DiskCache<Box<dyn Disk>>>)> {
    let disk: Box<dyn Disk> = match DiskFile::open_read_only(path) {
        Ok(disk) => Box::new(disk),
        Err(err) => {
            log::error!("failed to open image {} for reading: {}", path, err);
            return None;
        }
    };
    match redoxfs::FileSystem::open(DiskCache::new(disk), None, None, false) {
        Ok(mut filesystem) => {
            filesystem.read_only = true;
            Some((path.to_string(), filesystem))
        }
        Err(err) => {
            log::error!("failed to open filesystem {}: {}", path, err);
            None
        }
    }
}

/// Creates an empty file system of `size` bytes in memory whose root directory anyone may write
/// to, with the sticky bit, as `/tmp`.
fn filesystem_in_memory(size: u64) -> Option<(String, FileSystem<DiskCache<Box<dyn Disk>>>)> {
    let disk: Box<dyn Disk> = Box::new(redoxfs::DiskMemory::new(size));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let mut filesystem =
        match redoxfs::FileSystem::create(DiskCache::new(disk), None, now.as_secs(), now.subsec_nanos()) {
            Ok(filesystem) => filesystem,
            Err(err) => {
                log::error!("failed to create a file system in memory: {}", err);
                return None;
            }
        };
    let opened = filesystem.tx(|tx| {
        let mut root = tx.read_tree(redoxfs::TreePtr::<redoxfs::Node>::root())?;
        root.data_mut().set_mode(redoxfs::Node::MODE_DIR | 0o1777);
        tx.sync_tree(root)
    });
    if let Err(err) = opened {
        log::error!("failed to open the root of the file system in memory: {}", err);
        return None;
    }
    Some((format!("memory:{size}"), filesystem))
}

fn filesystem_by_path(
    path: &str,
    block_opt: Option<u64>,
    log_errors: bool,
    boot_password: Option<&[u8]>,
) -> Option<(String, FileSystem<DiskCache<Box<dyn Disk>>>)> {
    log::debug!("opening {}", path);
    let attempts = if boot_password.is_some() { 0 } else { 10 };
    for attempt in 0..=attempts {
        let typed;
        let password_opt = if attempt > 0 {
            eprint!("redoxfs: password: ");

            let password = io::stdin()
                .read_passwd(&mut io::stderr())
                .unwrap()
                .unwrap_or_default();

            eprintln!();

            if password.is_empty() {
                eprintln!("redoxfs: empty password, giving up");

                // Password is empty, exit loop
                break;
            }

            typed = Passphrase(password.into_bytes());
            Some(typed.0.as_slice())
        } else {
            boot_password
        };

        #[cfg(target_os = "redox")]
        let disk: Box<dyn Disk> = match DiskFile::open(path) {
            Ok(mut df) => {
                let disk_size = df.size().unwrap_or(0);
                match libredox::call::dup(df.file.as_raw_fd() as usize, b"uring") {
                    Ok(ring_fd) => {
                        let ring_fd = libredox::Fd::new(ring_fd);
                        Box::new(DiskRing::from_fd(ring_fd, df, disk_size).unwrap())
                    }

                    Err(err) => {
                        log::debug!(
                            "Ring communication not supported on {path} ({err}). Falling back to standard IO."
                        );
                        Box::new(df)
                    }
                }
            }
            Err(err) => {
                if log_errors {
                    log::error!("failed to open image {}: {}", path, err);
                }
                break;
            }
        };
        #[cfg(not(target_os = "redox"))]
        let disk: Box<dyn Disk> = match DiskFile::open(path) {
            Ok(disk) => Box::new(disk),
            Err(err) => {
                if log_errors {
                    log::error!("failed to open image {}: {}", path, err);
                }
                break;
            }
        };

        let disk = DiskCache::new(disk);
        match redoxfs::FileSystem::open(disk, password_opt, block_opt, true) {
            Ok(filesystem) => {
                log::debug!(
                    "opened filesystem on {} with uuid {}",
                    path,
                    Uuid::from_bytes(filesystem.header.uuid()).hyphenated()
                );

                return Some((path.to_string(), filesystem));
            }
            Err(err) => match err.errno {
                syscall::ENOKEY => {
                    if password_opt.is_some() {
                        eprintln!("redoxfs: incorrect password ({}/{})", attempt, attempts);
                    }
                }
                _ => {
                    if log_errors {
                        log::error!("failed to open filesystem {}: {}", path, err);
                    }
                    break;
                }
            },
        }
    }
    None
}

#[cfg(not(target_os = "redox"))]
fn filesystem_by_uuid(
    _uuid: &Uuid,
    _block_opt: Option<u64>,
    _boot_password: Option<&[u8]>,
) -> Option<(String, FileSystem<DiskCache<Box<dyn Disk>>>)> {
    None
}

#[cfg(target_os = "redox")]
fn filesystem_by_uuid(
    uuid: &Uuid,
    block_opt: Option<u64>,
    boot_password: Option<&[u8]>,
) -> Option<(String, FileSystem<DiskCache<Box<dyn Disk>>>)> {
    use std::fs;

    use redox_path::RedoxPath;

    match fs::read_dir("/scheme") {
        Ok(entries) => {
            for entry_res in entries {
                if let Ok(entry) = entry_res {
                    if let Some(disk) = entry.path().to_str() {
                        if RedoxPath::from_absolute(disk)
                            .unwrap_or(RedoxPath::from_absolute("/")?)
                            .is_scheme_category("disk")
                        {
                            log::debug!("found scheme {}", disk);
                            match fs::read_dir(disk) {
                                Ok(entries) => {
                                    for entry_res in entries {
                                        if let Ok(entry) = entry_res {
                                            if let Ok(path) =
                                                entry.path().into_os_string().into_string()
                                            {
                                                log::debug!("found path {}", path);
                                                if let Some((path, filesystem)) = filesystem_by_path(
                                                    &path,
                                                    block_opt,
                                                    true,
                                                    boot_password,
                                                ) {
                                                    if &filesystem.header.uuid() == uuid.as_bytes()
                                                    {
                                                        log::debug!(
                                                            "filesystem on {} matches uuid {}",
                                                            path,
                                                            uuid.hyphenated()
                                                        );
                                                        return Some((path, filesystem));
                                                    } else {
                                                        log::debug!(
                                                            "filesystem on {} does not match uuid {}",
                                                            path,
                                                            uuid.hyphenated()
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(err) => {
                                    log::debug!("failed to list '{}': {}", disk, err);
                                }
                            }
                        }
                    }
                }
            }
        }
        Err(err) => {
            log::error!("failed to list schemes: {}", err);
        }
    }

    None
}

fn daemon(
    disk_id: &DiskId,
    mountpoint: &str,
    block_opt: Option<u64>,
    mut write: Option<File>,
    key_file: Option<&str>,
    flush_on_unmount: bool,
) -> ! {
    setsig();

    // Read once: reading wipes the bootloader's copy, and the search below may try several disks
    let boot_password = match key_file {
        Some(path) => match redoxfs::key_tool::read_secret(Some(path), "") {
            Ok(mut secret) => Some(Passphrase(std::mem::take(&mut secret.0))),
            Err(err) => print_err_exit(format!("cannot read key file: {err}")),
        },
        None => bootloader_password(),
    };
    let boot_password_opt = boot_password.as_ref().map(|password| password.0.as_slice());
    let filesystem_opt = match *disk_id {
        DiskId::Path(ref path) => filesystem_by_path(path, block_opt, true, boot_password_opt),
        DiskId::Uuid(ref uuid) => filesystem_by_uuid(uuid, block_opt, boot_password_opt),
        DiskId::ReadOnly(ref path) => filesystem_read_only(path),
        DiskId::Memory(size) => filesystem_in_memory(size),
    };
    drop(boot_password);

    if let Some((path, mut filesystem)) = filesystem_opt {
        filesystem.barriers = !flush_on_unmount;
        match mount(filesystem, mountpoint, |mounted_path| {
            capability_mode();

            log::info!(
                "mounted filesystem on {} to {}",
                path,
                mounted_path.display()
            );

            if let Some(ref mut write) = write {
                let _ = write.write(&[0]);
            }
        }) {
            Ok(()) => {
                process::exit(0);
            }
            Err(err) => {
                log::error!("failed to mount {} to {}: {}", path, mountpoint, err);
            }
        }
    }

    match *disk_id {
        DiskId::Path(ref path) => {
            log::error!("not able to mount path {}", path);
        }
        DiskId::Uuid(ref uuid) => {
            log::error!("not able to mount uuid {}", uuid.hyphenated());
        }
        DiskId::ReadOnly(ref path) => {
            log::error!("not able to mount path {} for reading", path);
        }
        DiskId::Memory(size) => {
            log::error!("not able to mount {} bytes of memory", size);
        }
    }

    if let Some(ref mut write) = write {
        let _ = write.write(&[1]);
    }

    process::exit(1);
}

fn main() {
    env_logger::init();
    unsafe {
        if libc::setpriority(libc::PRIO_PROCESS, 0, -10) == -1 {
            log::error!("redoxfs: Failed to set nice value");
        } else {
            log::debug!("redoxfs: Successfully set process priority to 10");
        }
    }

    let mut args = env::args().skip(1);

    let mut daemonise = true;
    let mut disk_id: Option<DiskId> = None;
    let mut mountpoint: Option<String> = None;
    let mut block_opt: Option<u64> = None;
    let mut key_file = None;
    let mut read_only = false;
    let mut flush_on_unmount = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--no-daemon" | "-d" => daemonise = false,
            "--read-only" => read_only = true,
            "--flush-on-unmount" => flush_on_unmount = true,
            "--memory" if disk_id.is_none() => {
                let mib = args
                    .next()
                    .and_then(|mib| mib.parse::<u64>().ok())
                    .filter(|&mib| (1..=4096).contains(&mib))
                    .unwrap_or_else(|| print_err_exit("--memory needs a size in MiB, 1 to 4096"));
                disk_id = Some(DiskId::Memory(mib << 20));
            }
            "--key-file" => {
                key_file = Some(
                    args.next()
                        .unwrap_or_else(|| print_err_exit("missing key file")),
                )
            }

            "--uuid" if disk_id.is_none() => {
                disk_id = Some(DiskId::Uuid(
                    match args.next().as_deref().map(Uuid::parse_str) {
                        Some(Ok(uuid)) => uuid,
                        Some(Err(err)) => {
                            print_err_exit(format!("invalid uuid '{}': {}", arg, err))
                        }
                        None => print_err_exit("no uuid provided"),
                    },
                ));
            }

            disk if disk_id.is_none() => disk_id = Some(DiskId::Path(disk.to_owned())),

            mnt if disk_id.is_some() && mountpoint.is_none() => mountpoint = Some(mnt.to_owned()),

            opts if mountpoint.is_some() => match u64::from_str_radix(opts, 16) {
                Ok(block) => block_opt = Some(block),
                Err(err) => print_err_exit(format!("invalid block '{}': {}", opts, err)),
            },

            _ => print_usage_exit(),
        }
    }

    let disk_id = match disk_id {
        Some(DiskId::Path(path)) if read_only => DiskId::ReadOnly(path),
        Some(_) if read_only => print_err_exit("--read-only takes an image path"),
        Some(disk_id) => disk_id,
        None => print_err_exit("no disk provided"),
    };

    let Some(mountpoint) = mountpoint else {
        print_err_exit("no mountpoint provided");
    };

    if daemonise {
        let mut pipes = [0; 2];
        if pipe(&mut pipes) == 0 {
            let mut read = unsafe { File::from_raw_fd(pipes[0] as RawFd) };
            let write = unsafe { File::from_raw_fd(pipes[1] as RawFd) };

            let pid = fork();
            if pid == 0 {
                drop(read);

                daemon(
                    &disk_id,
                    &mountpoint,
                    block_opt,
                    Some(write),
                    key_file.as_deref(),
                    flush_on_unmount,
                );
            } else if pid > 0 {
                drop(write);

                let mut res = [0];
                read.read_exact(&mut res).unwrap();

                process::exit(res[0] as i32);
            } else {
                panic!("redoxfs: failed to fork: {:?}", io::Error::last_os_error());
            }
        } else {
            panic!(
                "redoxfs: failed to create pipe: {:?}",
                io::Error::last_os_error()
            );
        }
    } else {
        log::info!("running in foreground");
        daemon(
            &disk_id,
            &mountpoint,
            block_opt,
            None,
            key_file.as_deref(),
            flush_on_unmount,
        );
    }
}

extern crate redoxfs;
extern crate uuid;

use std::io::Read;
use std::{env, fs, process, time};

use redoxfs::{DiskFile, FileSystem};
use uuid::Uuid;

fn usage() -> ! {
    eprintln!("redoxfs-mkfs [--encrypt] [--key-file PATH] DISK [BOOTLOADER]");
    process::exit(1);
}

fn main() {
    env_logger::init();

    let mut encrypt = false;
    let mut disk_path_opt = None;
    let mut bootloader_path_opt = None;
    let mut key_file = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--encrypt" {
            encrypt = true;
        } else if arg == "--key-file" {
            key_file = Some(args.next().unwrap_or_else(|| usage()));
            encrypt = true;
        } else if disk_path_opt.is_none() {
            disk_path_opt = Some(arg);
        } else if bootloader_path_opt.is_none() {
            bootloader_path_opt = Some(arg);
        } else {
            eprintln!("redoxfs-mkfs: too many arguments provided");
            usage();
        }
    }

    let disk_path = if let Some(path) = disk_path_opt {
        path
    } else {
        eprintln!("redoxfs-mkfs: no disk image provided");
        usage();
    };

    let disk = match DiskFile::open(&disk_path) {
        Ok(disk) => disk,
        Err(err) => {
            eprintln!("redoxfs-mkfs: failed to open image {}: {}", disk_path, err);
            process::exit(1);
        }
    };

    let mut bootloader = vec![];
    if let Some(bootloader_path) = bootloader_path_opt {
        match fs::File::open(&bootloader_path) {
            Ok(mut file) => match file.read_to_end(&mut bootloader) {
                Ok(_) => (),
                Err(err) => {
                    eprintln!(
                        "redoxfs-mkfs: failed to read bootloader {}: {}",
                        bootloader_path, err
                    );
                    process::exit(1);
                }
            },
            Err(err) => {
                eprintln!(
                    "redoxfs-mkfs: failed to open bootloader {}: {}",
                    bootloader_path, err
                );
                process::exit(1);
            }
        }
    };

    let password_opt = if encrypt {
        if let Err(err) = redoxfs::require_seeded_random() {
            eprintln!("redoxfs-mkfs: cannot generate keys: {err}");
            process::exit(1);
        }
        match redoxfs::key_tool::read_secret(key_file.as_deref(), "redoxfs-mkfs: password") {
            Ok(password) => Some(password),
            Err(err) => {
                eprintln!("redoxfs-mkfs: {err}");
                process::exit(1);
            }
        }
    } else {
        None
    };

    let ctime = time::SystemTime::now()
        .duration_since(time::UNIX_EPOCH)
        .unwrap();
    match FileSystem::create_reserved(
        disk,
        password_opt.as_ref().map(|x| x.0.as_slice()),
        &bootloader,
        ctime.as_secs(),
        ctime.subsec_nanos(),
    ) {
        Ok(filesystem) => {
            let uuid = Uuid::from_bytes(filesystem.header.uuid());
            eprintln!(
                "redoxfs-mkfs: created filesystem on {}, reserved {} blocks, size {} MB, uuid {}",
                disk_path,
                filesystem.block,
                filesystem.header.size() / 1000 / 1000,
                uuid.hyphenated()
            );
        }
        Err(err) => {
            eprintln!(
                "redoxfs-mkfs: failed to create filesystem on {}: {}",
                disk_path, err
            );
            process::exit(1);
        }
    }
}

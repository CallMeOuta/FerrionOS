use std::collections::BTreeMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use redoxfs::{Access, Disk, DiskFile, FileSystem, KeySlot, Node, Salt, TreePtr};
use syscall::error::{EROFS, Result};

use crate::disk::{Durability, SimDisk, View};
use crate::rng::seed_entropy;

const DISK_BYTES: usize = 4 << 20;
const T0: (u64, u32) = (1_700_000_000, 0);
const T_OLD_READ: (u64, u32) = (1_700_007_200, 0);
const PASS: &[u8] = b"ferrion-readonly-sim-pass";

fn require_erofs<T>(label: &str, res: Result<T>) -> std::result::Result<(), String> {
    match res {
        Err(err) if err.errno == EROFS => Ok(()),
        Err(err) => Err(format!("{label}: expected EROFS, got errno={}", err.errno)),
        Ok(_) => Err(format!("{label}: expected EROFS, got Ok")),
    }
}

fn exercise_read_only<D: Disk>(
    fs: &mut FileSystem<D>,
    file_ptr: TreePtr<Node>,
    dir_ptr: TreePtr<Node>,
    slot1: KeySlot,
) -> std::result::Result<(), String> {
    assert!(fs.access.is_read_only());

    let mut buf = [0u8; 17];
    let n = fs
        .tx(|tx| tx.read_node(file_ptr, 0, &mut buf, T_OLD_READ.0, T_OLD_READ.1))
        .map_err(|err| format!("read_node with >1h atime failed: {err}"))?;
    if n != buf.len() || &buf != b"immutable-payload" {
        return Err(format!("read_node returned unexpected payload ({n} bytes)"));
    }

    let synced = fs
        .tx(|tx| tx.sync(false))
        .map_err(|err| format!("clean tx.sync(false) failed: {err}"))?;
    if synced {
        return Err("clean tx.sync(false) reported writes on ReadOnly".to_string());
    }

    require_erofs(
        "create_node(file)",
        fs.tx(|tx| {
            tx.create_node(
                TreePtr::root(),
                "new.txt",
                Node::MODE_FILE | 0o644,
                T0.0,
                T0.1,
            )
            .map(|_| ())
        }),
    )?;
    require_erofs(
        "create_node(dir)",
        fs.tx(|tx| {
            tx.create_node(
                TreePtr::root(),
                "newdir",
                Node::MODE_DIR | 0o755,
                T0.0,
                T0.1,
            )
            .map(|_| ())
        }),
    )?;
    require_erofs(
        "create_node(symlink)",
        fs.tx(|tx| {
            tx.create_node(
                TreePtr::root(),
                "sym",
                Node::MODE_SYMLINK | 0o777,
                T0.0,
                T0.1,
            )
            .map(|_| ())
        }),
    )?;
    require_erofs(
        "write_node",
        fs.tx(|tx| {
            tx.write_node(file_ptr, 0, b"mutated", T0.0, T0.1)
                .map(|_| ())
        }),
    )?;
    require_erofs(
        "truncate_node",
        fs.tx(|tx| tx.truncate_node(file_ptr, 4, T0.0, T0.1)),
    )?;
    require_erofs(
        "sync_tree(chmod/utimens)",
        fs.tx(|tx| {
            let mut node = tx.read_tree(file_ptr)?;
            node.data_mut().set_mode(Node::MODE_FILE | 0o600);
            node.data_mut().set_mtime(T_OLD_READ.0, 0);
            tx.sync_tree(node)
        }),
    )?;
    require_erofs(
        "link_node",
        fs.tx(|tx| tx.link_node(dir_ptr, "hardlink", file_ptr).map(|_| ())),
    )?;
    require_erofs(
        "rename_node",
        fs.tx(|tx| {
            tx.rename_node(TreePtr::root(), "hello.txt", dir_ptr, "moved.txt")
                .map(|_| ())
        }),
    )?;
    require_erofs(
        "remove_node",
        fs.tx(|tx| {
            tx.remove_node(TreePtr::root(), "other.txt", Node::MODE_FILE)
                .map(|_| ())
        }),
    )?;
    require_erofs("set_key_slot", fs.set_key_slot(1, slot1))?;

    fs.cleanup()
        .map_err(|err| format!("cleanup() on ReadOnly failed: {err}"))?;
    Ok(())
}

pub fn run_readonly() -> usize {
    seed_entropy(0x524f_5f53_494d_0003);

    let disk = SimDisk::new(vec![0u8; DISK_BYTES], Durability::Ordered);
    let mut rw_fs = match FileSystem::create(disk, Some(PASS), T0.0, T0.1) {
        Ok(fs) => fs,
        Err(err) => {
            eprintln!("FAIL readonly: create fs failed: {err}");
            return 1;
        }
    };
    let (file_ptr, dir_ptr) = match rw_fs.tx(|tx| {
        let dir = tx.create_node(TreePtr::root(), "sub", Node::MODE_DIR | 0o755, T0.0, T0.1)?;
        let file = tx.create_node(
            TreePtr::root(),
            "hello.txt",
            Node::MODE_FILE | 0o644,
            T0.0,
            T0.1,
        )?;
        tx.write_node(file.ptr(), 0, b"immutable-payload", T0.0, T0.1)?;
        let _other = tx.create_node(
            TreePtr::root(),
            "other.txt",
            Node::MODE_FILE | 0o644,
            T0.0,
            T0.1,
        )?;
        Ok((file.ptr(), dir.ptr()))
    }) {
        Ok(ptrs) => ptrs,
        Err(err) => {
            eprintln!("FAIL readonly: populate failed: {err}");
            return 1;
        }
    };
    let (_, keys) = rw_fs.header.unlock(PASS).expect("unlock header");
    let slot1 = KeySlot::new(b"ferrion-readonly-slot1", Salt::new().unwrap(), keys).unwrap();
    if let Err(err) = rw_fs.cleanup().and_then(|()| rw_fs.disk.flush()) {
        eprintln!("FAIL readonly: initial unmount failed: {err}");
        return 1;
    }

    let snapshot = rw_fs.disk.media().to_vec();

    let view = View::new(&snapshot, None, BTreeMap::new());
    let mut ro_view = match FileSystem::open(view, Some(PASS), None, false) {
        Ok(fs) => fs,
        Err(err) => {
            eprintln!("FAIL readonly: open on View failed: {err}");
            return 1;
        }
    };
    ro_view.access = Access::ReadOnly;
    if let Err(msg) = exercise_read_only(&mut ro_view, file_ptr, dir_ptr, slot1) {
        eprintln!("FAIL readonly (View): {msg}");
        return 1;
    }
    if ro_view.disk.refused_writes != 0 {
        eprintln!(
            "FAIL readonly (View): refused_writes={} (expected 0)",
            ro_view.disk.refused_writes
        );
        return 1;
    }

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let img_path = std::env::temp_dir().join(format!(
        "redoxfs-sim-readonly-{}-{stamp}.img",
        std::process::id()
    ));
    if let Err(err) = fs::write(&img_path, &snapshot) {
        eprintln!("FAIL readonly (DiskFile): write temp image: {err}");
        return 1;
    }
    let file_result = (|| -> std::result::Result<(), String> {
        let disk = DiskFile::open_read_only(&img_path)
            .map_err(|err| format!("open_read_only failed: {err}"))?;
        let mut ro_file = FileSystem::open(disk, Some(PASS), None, false)
            .map_err(|err| format!("FileSystem::open failed: {err}"))?;
        ro_file.access = Access::ReadOnly;
        exercise_read_only(&mut ro_file, file_ptr, dir_ptr, slot1)?;
        drop(ro_file);
        let after = fs::read(&img_path).map_err(|err| format!("re-read image: {err}"))?;
        if after != snapshot {
            return Err("image bytes changed after read-only mount and unmount".to_string());
        }
        Ok(())
    })();
    let _ = fs::remove_file(&img_path);
    if let Err(msg) = file_result {
        eprintln!("FAIL readonly (DiskFile): {msg}");
        return 1;
    }

    println!(
        "readonly: PASS (View + DiskFile::open_read_only, 10 mutating ops -> EROFS, >1h-atime read + clean sync + cleanup -> Ok, refused_writes=0, {} image bytes unchanged)",
        snapshot.len()
    );
    0
}

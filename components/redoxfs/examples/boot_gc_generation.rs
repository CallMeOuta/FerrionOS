use redoxfs::{ALLOC_GC_THRESHOLD, Disk, DiskFile, FileSystem, HEADER_RING};

fn main() {
    let path = std::env::args_os().nth(1).expect("usage: boot_gc_generation IMAGE");
    let mut fs = FileSystem::open(DiskFile::open(path).unwrap(), None, None, false).unwrap();
    assert!(!fs.header.encrypted(), "the fixture requires an unencrypted image");
    let previous = fs.header.generation();
    let generation = (previous / ALLOC_GC_THRESHOLD + 1) * ALLOC_GC_THRESHOLD;
    fs.header.generation = (generation - 1).into();
    assert_eq!(fs.header.update(None), generation);
    assert!(fs.header.valid());
    let block = fs.block + generation % HEADER_RING;
    let written = unsafe { fs.disk.write_at(block, &fs.header).unwrap() };
    assert_eq!(written, fs.header.len());
    let fs = FileSystem::open(fs.disk, None, None, false).unwrap();
    assert_eq!(fs.header.generation(), generation);
    println!("generation {previous} -> {generation}; header block {block}");
}

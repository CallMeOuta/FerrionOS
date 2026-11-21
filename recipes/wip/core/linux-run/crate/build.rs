// The runtime lives in the native window from 0x7000_0000_0000 up to the end of user memory;
// Linux code gets everything below it. relibc's loader maps a static executable at its link
// address and then raises the address space's mmap-min-addr past it, so linking the image there
// also puts relibc's heap, thread blocks and stacks in the window (src/main.rs checks that).
// `--no-relax` keeps libgcc's GOT loads of weak, undefined symbols (pthread_cancel) as GOT loads:
// relaxed, they become 32-bit absolute values, which cannot hold zero once the image is this high.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/entry.S");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("redox") {
        println!("cargo:rustc-link-arg-bins=-Wl,-Ttext-segment=0x700000000000");
        println!("cargo:rustc-link-arg-bins=-Wl,--no-relax");
    }
}

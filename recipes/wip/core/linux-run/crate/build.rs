// The runtime lives in the native window from 0x7000_0000_0000 up to the end of user memory;
// Linux code gets everything below it. relibc's loader maps a static executable at its link
// address and then raises the address space's mmap-min-addr past it, so linking the image there
// also puts relibc's heap, thread blocks and stacks in the window (src/main.rs checks that).
// `--no-relax` keeps libgcc's GOT loads of weak, undefined symbols (pthread_cancel) as GOT loads:
// relaxed, they become 32-bit absolute values, which cannot hold zero once the image is this high.
//
// src/thread.rs calls relibc's `redox_rt::signal::current_setsighandler_struct`, which has no C
// name: the recipe finds its mangled name in the libc.a this binary links and passes it in
// LINUX_RUN_SIGHANDLER_SYMBOL.
use std::io::Write;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/entry.S");
    println!("cargo:rerun-if-env-changed=LINUX_RUN_SIGHANDLER_SYMBOL");
    let ferrion_target = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("redox");
    if ferrion_target {
        println!("cargo:rustc-link-arg-bins=-Wl,-Ttext-segment=0x700000000000");
        println!("cargo:rustc-link-arg-bins=-Wl,--no-relax");
    }
    let symbol = std::env::var("LINUX_RUN_SIGHANDLER_SYMBOL").unwrap_or_default();
    if ferrion_target && symbol.is_empty() {
        panic!("LINUX_RUN_SIGHANDLER_SYMBOL is not set: the recipe finds it in relibc's libc.a");
    }
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("sighandler_symbol.rs");
    let mut f = std::fs::File::create(out).unwrap();
    writeln!(
        f,
        "unsafe extern \"Rust\" {{ #[link_name = \"{symbol}\"] fn relibc_setsighandler_struct() -> SetSighandlerData; }}"
    )
    .unwrap();
}

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};

use tpmd::tpm2::MAX_NONCE_LEN;

const PATH: &str = "/scheme/attest/statement";

fn usage() -> ! {
    eprintln!(
        "usage: attest-statement [NONCE]\n\
         Prints attestd's statement for NONCE, 1 to {MAX_NONCE_LEN} bytes in hexadecimal\n\
         (default: {MAX_NONCE_LEN} random bytes); root only."
    );
    std::process::exit(2);
}

fn parse_hex(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() || !text.len().is_multiple_of(2) || text.len() / 2 > MAX_NONCE_LEN {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

fn fail(what: &str, e: std::io::Error) -> ! {
    eprintln!("attest-statement: {what}: {e}");
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let nonce = match args.as_slice() {
        [] => {
            let mut nonce = vec![0; MAX_NONCE_LEN];
            std::fs::File::open("/scheme/rand")
                .and_then(|mut rand| rand.read_exact(&mut nonce))
                .unwrap_or_else(|e| fail("cannot read /scheme/rand", e));
            nonce
        }
        [hex] => parse_hex(hex).unwrap_or_else(|| usage()),
        _ => usage(),
    };
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(PATH)
        .unwrap_or_else(|e| fail(&format!("cannot open {PATH}"), e));
    file.write_all(&nonce)
        .unwrap_or_else(|e| fail("cannot send the nonce", e));
    file.seek(SeekFrom::Start(0))
        .unwrap_or_else(|e| fail("cannot read the statement", e));
    let mut statement = String::new();
    file.read_to_string(&mut statement)
        .unwrap_or_else(|e| fail("cannot read the statement", e));
    print!("{statement}");
}

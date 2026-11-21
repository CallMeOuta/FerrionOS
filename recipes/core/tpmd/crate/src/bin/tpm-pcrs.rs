//! tpm-pcrs: prints PCRs 7, 8 and 9 of the SHA-256 bank, as attestd reads them from the TPM.
//! Root only: attestd serves no other user.

const PATH: &str = "/scheme/attest/pcrs";

fn main() {
    if std::env::args().len() > 1 {
        eprintln!("usage: tpm-pcrs\nPrints PCRs 7, 8 and 9 (SHA-256) through attestd; root only.");
        std::process::exit(2);
    }
    match std::fs::read_to_string(PATH) {
        Ok(text) => print!("{text}"),
        Err(e) => {
            eprintln!("tpm-pcrs: cannot read {PATH}: {e}");
            std::process::exit(1);
        }
    }
}

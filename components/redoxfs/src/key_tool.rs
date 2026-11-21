use std::io::{self, Read, Write};
use termion::input::TermRead;

pub const KEY_CALL: u64 = 0x46534b455931;
pub const KEY_HEADER: u64 = 0;
pub const KEY_SET: u64 = 1;
pub const MAX_PASSPHRASE: usize = 4096;
pub const MAX_ROLL_RETRIES: usize = 3;
pub const EXIT_USAGE: i32 = 1;
pub const EXIT_UNCHANGED: i32 = 2;
pub const EXIT_APPLIED_PARTIAL: i32 = 3;
pub const EXIT_UNKNOWN: i32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Applied { complete: bool },
    Unchanged,
    Unknown,
}

impl Outcome {
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Applied { complete: true } => 0,
            Self::Unchanged => EXIT_UNCHANGED,
            Self::Applied { complete: false } => EXIT_APPLIED_PARTIAL,
            Self::Unknown => EXIT_UNKNOWN,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Applied { .. } => "applied",
            Self::Unchanged => "unchanged",
            Self::Unknown => "unknown",
        }
    }
}

/// Secret buffers are wiped on every return path.
pub struct Secret(pub Vec<u8>);

impl Drop for Secret {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            unsafe { core::ptr::write_volatile(byte, 0) };
        }
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    }
}

pub fn require_seeded_random() -> io::Result<()> {
    #[cfg(target_os = "redox")]
    {
        let status = std::fs::read_to_string("/scheme/rand/status")?;
        if status.lines().next() != Some("seeded: yes") {
            return Err(io::Error::other(
                "/scheme/rand/status must report seeded: yes before generating keys",
            ));
        }
    }
    Ok(())
}

pub fn read_secret(path: Option<&str>, prompt: &str) -> io::Result<Secret> {
    let mut secret = Secret(Vec::new());
    match path {
        Some(path) => {
            let mut input: Box<dyn Read> = if path == "-" {
                Box::new(io::stdin())
            } else {
                Box::new(std::fs::File::open(path)?)
            };
            input
                .by_ref()
                .take(MAX_PASSPHRASE as u64 + 1)
                .read_to_end(&mut secret.0)?;
        }
        None => {
            eprint!("{prompt}: ");
            io::stderr().flush()?;
            secret.0 = io::stdin()
                .read_passwd(&mut io::stderr())?
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "no passphrase"))?
                .into_bytes();
            eprintln!();
        }
    }
    if secret.0.is_empty() || secret.0.len() > MAX_PASSPHRASE {
        return Err(io::Error::other("passphrase must contain 1 to 4096 bytes"));
    }
    Ok(secret)
}

/// Removes `--key-file PATH|-` from `args` and reads the secret it names.
pub fn take_key_file(
    mut args: impl Iterator<Item = String>,
) -> io::Result<(Option<Secret>, Vec<String>)> {
    let mut path = None;
    let mut rest = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "--key-file" {
            if path.is_some() {
                return Err(io::Error::other("--key-file given twice"));
            }
            path = Some(
                args.next()
                    .ok_or_else(|| io::Error::other("--key-file needs a path"))?,
            );
        } else {
            rest.push(arg);
        }
    }
    let secret = match path {
        Some(path) => Some(read_secret(Some(&path), "")?),
        None => None,
    };
    Ok((secret, rest))
}

/// Eight checked six-digit groups encode 128 random bits; the password has no spaces.
pub fn recovery_key() -> io::Result<Secret> {
    require_seeded_random()?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(io::Error::other)?;
    let mut secret = Secret(Vec::with_capacity(48));
    for bytes in random.chunks_exact(2) {
        let mut value = u16::from_le_bytes([bytes[0], bytes[1]]) as u32 * 11;
        let mut digits = [b'0'; 6];
        for digit in digits.iter_mut().rev() {
            *digit += (value % 10) as u8;
            value /= 10;
        }
        secret.0.extend_from_slice(&digits);
        for byte in &mut digits {
            unsafe { core::ptr::write_volatile(byte, 0) };
        }
    }
    for byte in &mut random {
        unsafe { core::ptr::write_volatile(byte, 0) };
    }
    Ok(secret)
}

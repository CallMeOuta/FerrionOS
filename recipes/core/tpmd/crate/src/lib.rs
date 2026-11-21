#[cfg(target_os = "redox")]
pub mod service;
pub mod tpm2;

pub const BOOT_PCRS: [u8; 3] = [7, 8, 9];

pub const TPM_FD_ENV: &str = "TPMD_FD";

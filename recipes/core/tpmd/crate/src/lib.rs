//! FerrionOS's TPM 2.0 stack: `tpmd` drives the TPM, `attestd` holds the only handle to it,
//! and `tpm-pcrs` and `attest-statement` ask `attestd`.

#[cfg(target_os = "redox")]
pub mod service;
pub mod tpm2;

/// PCR 7 holds the Secure Boot state and keys the firmware measured. With measured boot,
/// FerrionOS's bootloader adds its verification key to PCR 7, the boot environment and entry
/// to PCR 8, and the manifest, kernel and initfs to PCR 9.
pub const BOOT_PCRS: [u8; 3] = [7, 8, 9];

/// The environment variable through which `tpmd` hands its TPM handle to `attestd`.
pub const TPM_FD_ENV: &str = "TPMD_FD";

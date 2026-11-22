use std::collections::BTreeMap;

use redox_scheme::scheme::SchemeSync;
use redox_scheme::{CallerCtx, OpenResult, Socket};
use sha2::{Digest, Sha256};
use syscall::data::Stat;
use syscall::error::{EACCES, EBADF, EINVAL, EIO, ENODEV, ENOENT, Error, Result};
use syscall::flag::MODE_FILE;
use syscall::schemev2::NewFdFlags;
use tpmd::{BOOT_PCRS, TPM_FD_ENV, service, tpm2};

const RESPONSE_MAX: usize = 4096;

struct Tpm {
    fd: usize,
}

impl Tpm {
    fn transact(&self, command: &[u8]) -> Result<Vec<u8>, String> {
        libredox::call::write(self.fd, command).map_err(|e| format!("tpmd: {e}"))?;
        let mut response = vec![0; RESPONSE_MAX];
        let len = libredox::call::read(self.fd, &mut response).map_err(|e| format!("tpmd: {e}"))?;
        response.truncate(len);
        Ok(response)
    }

    fn pcrs(&self) -> Result<Vec<[u8; tpm2::SHA256_LEN]>, String> {
        let response = self.transact(&tpm2::pcr_read(&BOOT_PCRS))?;
        tpm2::parse_pcr_read(&response, &BOOT_PCRS).map_err(|e| format!("TPM2_PCR_Read: {e}"))
    }

    fn flush_key(&self, key: u32) -> Result<(), String> {
        self.transact(&tpm2::flush_context(key))
            .and_then(|r| tpm2::parse_empty(&r).map_err(|e| format!("TPM2_FlushContext: {e}")))
    }

    fn with_transient_key<T>(&self, f: impl FnOnce(u32) -> Result<T, String>) -> Result<T, String> {
        let response = self.transact(&tpm2::create_attestation_key())?;
        let key = match tpm2::parse_create_primary(&response) {
            Ok(key) => key,
            Err(err) => {
                if tpm2::response_code(&response) == Some(tpm2::RC_SUCCESS)
                    && let Some(bytes) = response.get(tpm2::HEADER_LEN..tpm2::HEADER_LEN + 4)
                {
                    let leaked = u32::from_be_bytes(bytes.try_into().unwrap());
                    let _ = self.flush_key(leaked);
                }
                return Err(format!("TPM2_CreatePrimary: {err}"));
            }
        };
        let result = f(key);
        let flushed = self.flush_key(key);
        let value = result?;
        flushed?;
        Ok(value)
    }

    fn quote(&self, nonce: &[u8]) -> Result<tpm2::Quote, String> {
        self.with_transient_key(|key| {
            self.transact(&tpm2::quote(key, nonce, &BOOT_PCRS))
                .and_then(|r| {
                    tpm2::parse_quote(&r, &BOOT_PCRS).map_err(|e| format!("TPM2_Quote: {e}"))
                })
        })
    }
}

fn is_zero(digest: &[u8]) -> bool {
    digest.iter().all(|&b| b == 0)
}

fn statement(tpm: Option<&Tpm>, nonce: &[u8]) -> String {
    let mut out = format!(
        "FerrionOS attestation statement, format 1\nnonce: {}\n",
        tpm2::hex(nonce)
    );
    let Some(tpm) = tpm else {
        out += "tpm: absent\nverdict: no evidence\n";
        return out;
    };
    let evidence = tpm.pcrs().and_then(|pcrs| Ok((pcrs, tpm.quote(nonce)?)));
    let (pcrs, quote) = match evidence {
        Ok(evidence) => evidence,
        Err(e) => {
            eprintln!("attestd: {e}");
            out += "tpm: failed\nverdict: no evidence\n";
            return out;
        }
    };
    let mut hasher = Sha256::new();
    for pcr in &pcrs {
        hasher.update(pcr);
    }
    let consistent = quote.nonce == nonce && quote.pcr_digest == hasher.finalize().as_slice();
    let bootloader_measured = !is_zero(&pcrs[1]) && !is_zero(&pcrs[2]);
    out += "tpm: present\n";
    out += if consistent {
        "quote: matches the PCRs read\n"
    } else {
        "quote: does not match the PCRs read\n"
    };
    out += if bootloader_measured {
        "bootloader: measured\n"
    } else {
        "bootloader: not measured\n"
    };
    out += "appraisal: none (no Verifier yet)\n";
    out += match (consistent, bootloader_measured) {
        (false, _) => "verdict: inconsistent\n",
        (true, false) => "verdict: boot not measured\n",
        (true, true) => "verdict: boot measured\n",
    };
    out
}

enum Handle {
    Root,
    Pcrs(Vec<u8>),
    Statement(Option<Vec<u8>>),
}

struct AttestScheme {
    tpm: Option<Tpm>,
    handles: BTreeMap<usize, Handle>,
    next_id: usize,
}

impl AttestScheme {
    fn insert(&mut self, handle: Handle) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.handles.insert(id, handle);
        id
    }

    fn pcrs_text(&self) -> Result<Vec<u8>> {
        let tpm = self.tpm.as_ref().ok_or(Error::new(ENODEV))?;
        let pcrs = tpm.pcrs().map_err(|e| {
            eprintln!("attestd: {e}");
            Error::new(EIO)
        })?;
        Ok(BOOT_PCRS
            .iter()
            .zip(&pcrs)
            .map(|(index, value)| format!("PCR {index} sha256 {}\n", tpm2::hex(value)))
            .collect::<String>()
            .into_bytes())
    }
}

fn read_at(content: &[u8], buf: &mut [u8], offset: u64) -> usize {
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(content.len());
    let len = buf.len().min(content.len() - start);
    buf[..len].copy_from_slice(&content[start..start + len]);
    len
}

impl SchemeSync for AttestScheme {
    fn scheme_root(&mut self) -> Result<usize> {
        Ok(self.insert(Handle::Root))
    }

    fn openat(
        &mut self,
        dirfd: usize,
        path: &str,
        _flags: usize,
        _fcntl_flags: u32,
        ctx: &CallerCtx,
    ) -> Result<OpenResult> {
        if !matches!(self.handles.get(&dirfd), Some(Handle::Root)) {
            return Err(Error::new(EACCES));
        }
        if ctx.uid != 0 {
            return Err(Error::new(EACCES));
        }
        let handle = match path.trim_matches('/') {
            "pcrs" => Handle::Pcrs(self.pcrs_text()?),
            "statement" => Handle::Statement(None),
            _ => return Err(Error::new(ENOENT)),
        };
        Ok(OpenResult::ThisScheme {
            number: self.insert(handle),
            flags: NewFdFlags::POSITIONED,
        })
    }

    fn read(
        &mut self,
        id: usize,
        buf: &mut [u8],
        offset: u64,
        _fcntl_flags: u32,
        _ctx: &CallerCtx,
    ) -> Result<usize> {
        match self.handles.get(&id) {
            Some(Handle::Pcrs(text)) | Some(Handle::Statement(Some(text))) => {
                Ok(read_at(text, buf, offset))
            }
            Some(Handle::Statement(None)) => Err(Error::new(EINVAL)),
            Some(Handle::Root) | None => Err(Error::new(EBADF)),
        }
    }

    fn write(
        &mut self,
        id: usize,
        buf: &[u8],
        _offset: u64,
        _fcntl_flags: u32,
        _ctx: &CallerCtx,
    ) -> Result<usize> {
        if !matches!(self.handles.get(&id), Some(Handle::Statement(_))) {
            return Err(Error::new(EBADF));
        }
        if buf.is_empty() || buf.len() > tpm2::MAX_NONCE_LEN {
            return Err(Error::new(EINVAL));
        }
        let text = statement(self.tpm.as_ref(), buf).into_bytes();
        self.handles.insert(id, Handle::Statement(Some(text)));
        Ok(buf.len())
    }

    fn fcntl(&mut self, _id: usize, _cmd: usize, _arg: usize, _ctx: &CallerCtx) -> Result<usize> {
        Ok(0)
    }

    fn fstat(&mut self, id: usize, stat: &mut Stat, _ctx: &CallerCtx) -> Result<()> {
        let size = match self.handles.get(&id) {
            Some(Handle::Pcrs(text)) | Some(Handle::Statement(Some(text))) => text.len(),
            Some(_) => 0,
            None => return Err(Error::new(EBADF)),
        };
        stat.st_mode = MODE_FILE | 0o600;
        stat.st_size = size as u64;
        Ok(())
    }

    fn fpath(&mut self, id: usize, buf: &mut [u8], _ctx: &CallerCtx) -> Result<usize> {
        let path = match self.handles.get(&id) {
            Some(Handle::Root) => "/scheme/attest/",
            Some(Handle::Pcrs(_)) => "/scheme/attest/pcrs",
            Some(Handle::Statement(_)) => "/scheme/attest/statement",
            None => return Err(Error::new(EBADF)),
        };
        let len = path.len().min(buf.len());
        buf[..len].copy_from_slice(&path.as_bytes()[..len]);
        Ok(len)
    }

    fn on_close(&mut self, id: usize) {
        self.handles.remove(&id);
    }
}

fn main() {
    let notify = service::init_notify_fd().expect("attestd: start it through tpmd");
    let tpm = std::env::var(TPM_FD_ENV)
        .ok()
        .and_then(|fd| fd.parse().ok())
        .map(|fd| Tpm { fd });
    if let Some(tpm) = &tpm {
        service::set_inherit(tpm.fd, false).expect("attestd: cannot keep the TPM handle private");
    }

    let socket = Socket::create().expect("attestd: cannot create a scheme socket");
    let mut scheme = AttestScheme {
        tpm,
        handles: BTreeMap::new(),
        next_id: 1,
    };
    let root = scheme.scheme_root().unwrap();
    service::ready(notify, &socket, root).expect("attestd: cannot notify init");
    libredox::call::setrens(0, 0).expect("attestd: cannot enter the null namespace");
    eprintln!(
        "attestd: serving /scheme/attest, {}",
        if scheme.tpm.is_some() {
            "TPM present"
        } else {
            "no TPM"
        }
    );

    if let Err(e) = service::serve(&socket, &mut scheme) {
        eprintln!("attestd: {e}");
        std::process::exit(1);
    }
}

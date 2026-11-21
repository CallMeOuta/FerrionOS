//! The few TPM 2.0 commands FerrionOS sends, built and parsed by hand.
//!
//! Names and numbers follow the TCG TPM 2.0 Library specification, Part 2 (structures) and
//! Part 3 (commands). Every integer on the wire is big-endian.

use std::fmt;

pub const ST_NO_SESSIONS: u16 = 0x8001;
pub const ST_SESSIONS: u16 = 0x8002;
const ST_ATTEST_QUOTE: u16 = 0x8018;
const GENERATED_VALUE: u32 = 0xFF54_4347;

pub const CC_CREATE_PRIMARY: u32 = 0x0131;
pub const CC_STARTUP: u32 = 0x0144;
pub const CC_QUOTE: u32 = 0x0158;
pub const CC_FLUSH_CONTEXT: u32 = 0x0165;
pub const CC_GET_RANDOM: u32 = 0x017B;
pub const CC_PCR_READ: u32 = 0x017E;

pub const RC_SUCCESS: u32 = 0x000;
/// TPM_RC_INITIALIZE: the firmware never sent TPM2_Startup.
pub const RC_INITIALIZE: u32 = 0x100;

/// The null hierarchy: its seed changes at every TPM reset, so a key made under it is new on
/// every boot and links nothing across boots.
const RH_NULL: u32 = 0x4000_0007;
const RS_PW: u32 = 0x4000_0009;
const SU_CLEAR: u16 = 0x0000;

const ALG_SHA256: u16 = 0x000B;
const ALG_NULL: u16 = 0x0010;
const ALG_ECDSA: u16 = 0x0018;
const ALG_ECC: u16 = 0x0023;
const ECC_NIST_P256: u16 = 0x0003;

/// fixedTPM, fixedParent, sensitiveDataOrigin, userWithAuth, noDA, restricted, sign.
const AK_ATTRIBUTES: u32 = 0x0005_0472;

/// Size of a command or response header: tag, size, command or response code.
pub const HEADER_LEN: usize = 10;
pub const SHA256_LEN: usize = 32;
/// Largest qualifying data (nonce) a quote takes here: one SHA-256 digest.
pub const MAX_NONCE_LEN: usize = SHA256_LEN;

#[derive(Debug)]
pub enum Error {
    /// The TPM answered with this response code.
    Tpm(u32),
    /// The response is shorter than its fields or does not match the request.
    Malformed(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tpm(rc) => write!(f, "TPM response code {rc:#x}"),
            Self::Malformed(what) => write!(f, "malformed TPM response: {what}"),
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The command code in a command's header, if the buffer holds a whole header.
pub fn command_code(command: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(command.get(6..10)?.try_into().ok()?))
}

/// The size a command or response header declares.
pub fn declared_size(buf: &[u8]) -> Option<usize> {
    Some(u32::from_be_bytes(buf.get(2..6)?.try_into().ok()?) as usize)
}

/// The response code in a response's header.
pub fn response_code(response: &[u8]) -> Option<u32> {
    command_code(response)
}

struct Command(Vec<u8>);

impl Command {
    fn new(tag: u16, code: u32) -> Self {
        let mut buf = Vec::with_capacity(64);
        buf.extend_from_slice(&tag.to_be_bytes());
        buf.extend_from_slice(&[0; 4]);
        buf.extend_from_slice(&code.to_be_bytes());
        Self(buf)
    }
    fn u8(mut self, v: u8) -> Self {
        self.0.push(v);
        self
    }
    fn u16(mut self, v: u16) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn u32(mut self, v: u32) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    /// A TPM2B: a 16-bit size, then the bytes.
    fn sized(self, bytes: &[u8]) -> Self {
        let mut this = self.u16(bytes.len() as u16);
        this.0.extend_from_slice(bytes);
        this
    }
    /// The authorization area for one empty password: TPM_RS_PW, no nonce, no attributes and
    /// an empty password.
    fn password_auth(self) -> Self {
        self.u32(9).u32(RS_PW).u16(0).u8(0).u16(0)
    }
    /// TPML_PCR_SELECTION with one SHA-256 bank.
    fn pcr_selection(self, pcrs: &[u8]) -> Self {
        let select = pcr_bitmap(pcrs);
        self.u32(1)
            .u16(ALG_SHA256)
            .u8(3)
            .u8(select[0])
            .u8(select[1])
            .u8(select[2])
    }
    fn finish(mut self) -> Vec<u8> {
        let len = self.0.len() as u32;
        self.0[2..6].copy_from_slice(&len.to_be_bytes());
        self.0
    }
}

/// The 3-byte selection bitmap of PCRs 0 to 23.
fn pcr_bitmap(pcrs: &[u8]) -> [u8; 3] {
    let mut select = [0u8; 3];
    for &pcr in pcrs {
        select[usize::from(pcr / 8)] |= 1 << (pcr % 8);
    }
    select
}

pub fn startup_clear() -> Vec<u8> {
    Command::new(ST_NO_SESSIONS, CC_STARTUP)
        .u16(SU_CLEAR)
        .finish()
}

pub fn get_random(count: u16) -> Vec<u8> {
    Command::new(ST_NO_SESSIONS, CC_GET_RANDOM)
        .u16(count)
        .finish()
}

/// TPM2_PCR_Read of the given PCRs (each below 24) in the SHA-256 bank.
pub fn pcr_read(pcrs: &[u8]) -> Vec<u8> {
    Command::new(ST_NO_SESSIONS, CC_PCR_READ)
        .pcr_selection(pcrs)
        .finish()
}

/// TPM2_CreatePrimary of a restricted ECDSA P-256 signing key in the null hierarchy: an
/// attestation key that exists for one boot.
pub fn create_attestation_key() -> Vec<u8> {
    Command::new(ST_SESSIONS, CC_CREATE_PRIMARY)
        .u32(RH_NULL)
        .password_auth()
        // inSensitive: size 4, empty userAuth, empty data.
        .u16(4)
        .u16(0)
        .u16(0)
        // inPublic: TPMT_PUBLIC of 24 bytes.
        .u16(24)
        .u16(ALG_ECC)
        .u16(ALG_SHA256)
        .u32(AK_ATTRIBUTES)
        .u16(0) // authPolicy
        .u16(ALG_NULL) // symmetric
        .u16(ALG_ECDSA)
        .u16(ALG_SHA256)
        .u16(ECC_NIST_P256)
        .u16(ALG_NULL) // kdf
        .u16(0) // unique.x
        .u16(0) // unique.y
        .u16(0) // outsideInfo
        .u32(0) // creationPCR: no PCRs
        .finish()
}

/// TPM2_Quote of the given SHA-256 PCRs, signed by `key` over `nonce`.
pub fn quote(key: u32, nonce: &[u8], pcrs: &[u8]) -> Vec<u8> {
    Command::new(ST_SESSIONS, CC_QUOTE)
        .u32(key)
        .password_auth()
        .sized(nonce)
        .u16(ALG_NULL) // inScheme: the key's own scheme
        .pcr_selection(pcrs)
        .finish()
}

pub fn flush_context(handle: u32) -> Vec<u8> {
    Command::new(ST_NO_SESSIONS, CC_FLUSH_CONTEXT)
        .u32(handle)
        .finish()
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(len)
            .filter(|&end| end <= self.buf.len())
            .ok_or(Error::Malformed("truncated"))?;
        let bytes = &self.buf[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.bytes(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    fn sized(&mut self) -> Result<&'a [u8]> {
        let len = self.u16()?;
        self.bytes(usize::from(len))
    }
    /// A TPML_PCR_SELECTION that must select exactly `pcrs` in the SHA-256 bank.
    fn expect_selection(&mut self, pcrs: &[u8]) -> Result<()> {
        if self.u32()? != 1 || self.u16()? != ALG_SHA256 {
            return Err(Error::Malformed("unexpected PCR bank"));
        }
        let size = usize::from(self.u8()?);
        let select = self.bytes(size)?;
        let want = pcr_bitmap(pcrs);
        let mut got = [0u8; 3];
        for (i, &byte) in select.iter().enumerate() {
            match got.get_mut(i) {
                Some(slot) => *slot = byte,
                None if byte == 0 => {}
                None => return Err(Error::Malformed("unexpected PCR selection")),
            }
        }
        if got != want {
            return Err(Error::Malformed("unexpected PCR selection"));
        }
        Ok(())
    }
}

/// Checks the header of a response to a command sent with `tag` and returns a reader past it.
fn response(buf: &[u8], tag: u16) -> Result<Reader<'_>> {
    let mut r = Reader { buf, pos: 0 };
    let got_tag = r.u16()?;
    let size = r.u32()? as usize;
    let rc = r.u32()?;
    if size != buf.len() {
        return Err(Error::Malformed("size field"));
    }
    if rc != RC_SUCCESS {
        return Err(Error::Tpm(rc));
    }
    if got_tag != tag {
        return Err(Error::Malformed("tag"));
    }
    Ok(r)
}

/// Checks a response that carries no parameters, such as that of TPM2_Startup.
pub fn parse_empty(buf: &[u8]) -> Result<()> {
    response(buf, ST_NO_SESSIONS).map(|_| ())
}

pub fn parse_get_random(buf: &[u8]) -> Result<Vec<u8>> {
    Ok(response(buf, ST_NO_SESSIONS)?.sized()?.to_vec())
}

/// The SHA-256 values of `pcrs`, in the order asked.
pub fn parse_pcr_read(buf: &[u8], pcrs: &[u8]) -> Result<Vec<[u8; SHA256_LEN]>> {
    let mut r = response(buf, ST_NO_SESSIONS)?;
    r.u32()?; // pcrUpdateCounter
    r.expect_selection(pcrs)?;
    let count = r.u32()? as usize;
    if count != pcrs.len() {
        return Err(Error::Malformed("digest count"));
    }
    let mut sorted = pcrs.to_vec();
    sorted.sort_unstable();
    let mut by_index = Vec::with_capacity(count);
    for &pcr in &sorted {
        let digest: [u8; SHA256_LEN] = r
            .sized()?
            .try_into()
            .map_err(|_| Error::Malformed("digest size"))?;
        by_index.push((pcr, digest));
    }
    Ok(pcrs
        .iter()
        .map(|pcr| by_index.iter().find(|(i, _)| i == pcr).unwrap().1)
        .collect())
}

/// The handle of the key TPM2_CreatePrimary made.
pub fn parse_create_primary(buf: &[u8]) -> Result<u32> {
    response(buf, ST_SESSIONS)?.u32()
}

/// What a quote attests, from its TPMS_ATTEST; the signature is not checked here.
pub struct Quote {
    pub nonce: Vec<u8>,
    pub pcr_digest: Vec<u8>,
}

pub fn parse_quote(buf: &[u8], pcrs: &[u8]) -> Result<Quote> {
    let mut r = response(buf, ST_SESSIONS)?;
    r.u32()?; // parameterSize
    let attest = r.sized()?;
    let mut a = Reader {
        buf: attest,
        pos: 0,
    };
    if a.u32()? != GENERATED_VALUE {
        return Err(Error::Malformed("quote not generated by the TPM"));
    }
    if a.u16()? != ST_ATTEST_QUOTE {
        return Err(Error::Malformed("not a quote"));
    }
    a.sized()?; // qualifiedSigner
    let nonce = a.sized()?.to_vec();
    a.bytes(17)?; // clockInfo
    a.bytes(8)?; // firmwareVersion
    a.expect_selection(pcrs)?;
    let pcr_digest = a.sized()?.to_vec();
    Ok(Quote { nonce, pcr_digest })
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_random_matches_the_specification_example() {
        // TPM2_GetRandom(8): tag 8001, size 12, code 017B, bytesRequested 8.
        assert_eq!(
            get_random(8),
            [0x80, 0x01, 0, 0, 0, 12, 0, 0, 1, 0x7B, 0, 8]
        );
    }

    #[test]
    fn pcr_bitmap_of_7_8_9() {
        assert_eq!(pcr_bitmap(&[7, 8, 9]), [0x80, 0x03, 0x00]);
    }

    #[test]
    fn create_primary_sizes_add_up() {
        let cmd = create_attestation_key();
        assert_eq!(declared_size(&cmd), Some(cmd.len()));
        // header 10, handle 4, auth 13, inSensitive 6, inPublic 2 + 24, outsideInfo 2, creationPCR 4
        assert_eq!(cmd.len(), 10 + 4 + 13 + 6 + 26 + 2 + 4);
    }

    #[test]
    fn pcr_read_response_in_request_order() {
        let mut resp = vec![0x80, 0x01, 0, 0, 0, 0, 0, 0, 0, 0];
        resp.extend_from_slice(&5u32.to_be_bytes());
        resp.extend_from_slice(&[0, 0, 0, 1, 0, 0x0B, 3, 0x80, 0x03, 0x00]);
        resp.extend_from_slice(&3u32.to_be_bytes());
        for fill in [7u8, 8, 9] {
            resp.extend_from_slice(&[0, 32]);
            resp.extend_from_slice(&[fill; 32]);
        }
        let len = resp.len() as u32;
        resp[2..6].copy_from_slice(&len.to_be_bytes());
        let values = parse_pcr_read(&resp, &[9, 7, 8]).unwrap();
        assert_eq!(values, vec![[9; 32], [7; 32], [8; 32]]);
    }

    #[test]
    fn error_response_code() {
        let resp = [0x80, 0x01, 0, 0, 0, 10, 0, 0, 1, 0];
        assert!(matches!(parse_empty(&resp), Err(Error::Tpm(RC_INITIALIZE))));
    }
}

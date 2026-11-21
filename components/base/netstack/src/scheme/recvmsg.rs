//! The buffer of a RecvMsg call. The caller writes three sizes at its start: the room it
//! has for the sender's name, for the payload and for ancillary data. The reply replaces
//! them with the name's length and bytes, then the payload's length and bytes. Each length
//! is a little-endian `usize`.

use syscall::{Error, Result, EINVAL};

const WORD: usize = size_of::<usize>();

pub struct Request {
    pub name_len: usize,
    pub payload_len: usize,
}

impl Request {
    /// EINVAL when the buffer is shorter than the three sizes or than they add up to.
    pub fn parse(buf: &[u8]) -> Result<Self> {
        let word = |index: usize| {
            buf.get(index * WORD..(index + 1) * WORD)
                .map(|bytes| usize::from_le_bytes(bytes.try_into().unwrap()))
                .ok_or(Error::new(EINVAL))
        };
        let (name_len, payload_len, control_len) = (word(0)?, word(1)?, word(2)?);
        let total = [name_len, payload_len, control_len]
            .into_iter()
            .try_fold(3 * WORD, usize::checked_add);
        match total {
            Some(total) if total <= buf.len() => Ok(Self {
                name_len,
                payload_len,
            }),
            _ => Err(Error::new(EINVAL)),
        }
    }
}

/// Writes the name's length and bytes and returns where a payload of `payload_len` bytes
/// starts. EINVAL, with nothing written, when the reply would not fit in the buffer.
pub fn write_name(buf: &mut [u8], name: &[u8], payload_len: usize) -> Result<usize> {
    let start = 2 * WORD + name.len();
    if start
        .checked_add(payload_len)
        .is_none_or(|end| end > buf.len())
    {
        return Err(Error::new(EINVAL));
    }
    buf[..WORD].copy_from_slice(&name.len().to_le_bytes());
    buf[WORD..WORD + name.len()].copy_from_slice(name);
    Ok(start)
}

/// Writes the payload's length in front of the payload that starts at `start`, as returned
/// by [`write_name`].
pub fn write_payload_len(buf: &mut [u8], start: usize, payload_len: usize) {
    buf[start - WORD..start].copy_from_slice(&payload_len.to_le_bytes());
}

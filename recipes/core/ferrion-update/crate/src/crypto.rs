use std::fmt;
use std::str::FromStr;

use dryoc::classic::crypto_sign::{
    crypto_sign_detached, crypto_sign_seed_keypair, crypto_sign_verify_detached,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{HASH_BYTES, PUBLIC_KEY_BYTES, SIGNATURE_BYTES};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Blake3Hash(pub [u8; HASH_BYTES]);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PublicKey(pub [u8; PUBLIC_KEY_BYTES]);

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Signature(pub [u8; SIGNATURE_BYTES]);

#[derive(Clone)]
pub struct Keypair {
    pub public: PublicKey,
    secret: [u8; 64],
}

impl Keypair {
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        let (public, secret) = crypto_sign_seed_keypair(seed);
        Self {
            public: PublicKey(public),
            secret,
        }
    }

    pub fn sign(&self, message: &[u8]) -> Signature {
        let mut sig = [0u8; SIGNATURE_BYTES];
        crypto_sign_detached(&mut sig, message, &self.secret)
            .expect("Ed25519 signing with fixed buffers cannot fail");
        Signature(sig)
    }
}

impl Blake3Hash {
    pub fn hash_bytes(data: &[u8]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(data);
        Self(*hasher.finalize().as_bytes())
    }

    pub fn to_hex(&self) -> String {
        encode_hex(&self.0)
    }

    pub fn from_hex(hex: &str) -> Result<Self, String> {
        let bytes = decode_hex_32(hex)?;
        Ok(Self(bytes))
    }

    pub fn prefix_two(&self) -> String {
        format!("{:02x}", self.0[0])
    }
}

impl fmt::Display for Blake3Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "blake3:{}", self.to_hex())
    }
}

impl fmt::Debug for Blake3Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Blake3Hash({})", self)
    }
}

impl FromStr for Blake3Hash {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hex = s
            .strip_prefix("blake3:")
            .ok_or_else(|| format!("expected blake3:<64 hex>, got {s:?}"))?;
        Self::from_hex(hex)
    }
}

impl Serialize for Blake3Hash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Blake3Hash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

impl PublicKey {
    pub fn to_hex(&self) -> String {
        encode_hex(&self.0)
    }

    pub fn from_hex(hex: &str) -> Result<Self, String> {
        let bytes = decode_hex_32(hex)?;
        Ok(Self(bytes))
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ed25519:{}", self.to_hex())
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self)
    }
}

impl FromStr for PublicKey {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hex = s
            .strip_prefix("ed25519:")
            .ok_or_else(|| format!("expected ed25519:<64 hex>, got {s:?}"))?;
        Self::from_hex(hex)
    }
}

impl Serialize for PublicKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

impl Signature {
    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != SIGNATURE_BYTES {
            return None;
        }
        let mut out = [0u8; SIGNATURE_BYTES];
        out.copy_from_slice(bytes);
        Some(Self(out))
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signature({})", encode_hex(&self.0))
    }
}

pub fn verify_detached(signature: &Signature, message: &[u8], public_key: &PublicKey) -> bool {
    crypto_sign_verify_detached(&signature.0, message, &public_key.0).is_ok()
}

pub fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

pub fn decode_hex_32(hex: &str) -> Result<[u8; 32], String> {
    if hex.len() != 64 {
        return Err(format!(
            "expected 64 lowercase hex characters, got {}",
            hex.len()
        ));
    }
    let bytes = hex.as_bytes();
    let mut out = [0u8; 32];
    for i in 0..32 {
        let hi = hex_nibble(bytes[i * 2])?;
        let lo = hex_nibble(bytes[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Result<u8, String> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        _ => Err(format!("invalid lowercase hex character: 0x{b:02x}")),
    }
}

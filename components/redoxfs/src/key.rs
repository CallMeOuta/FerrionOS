use aes::{
    Aes128,
    cipher::{BlockDecrypt, BlockEncrypt, KeyInit},
};
use xts_mode::Xts128;

// The raw key, keep secret!
#[repr(transparent)]
pub struct Key([u8; 16]);

impl Key {
    /// Generate a random key
    #[cfg(feature = "std")]
    pub fn new() -> Result<Self, getrandom::Error> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes)?;
        Ok(Self(bytes))
    }

    pub fn encrypt(&self, password_aes: &Aes128) -> EncryptedKey {
        let mut block = aes::Block::from(self.0);
        password_aes.encrypt_block(&mut block);
        EncryptedKey(block.into())
    }

    pub fn into_aes(self) -> Aes128 {
        self.aes()
    }

    pub(crate) fn aes(&self) -> Aes128 {
        Aes128::new(&aes::Block::from(self.0))
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            unsafe { core::ptr::write_volatile(byte, 0) };
        }
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    }
}

/// The encrypted key, encrypted with AES using the salt and password
#[derive(Clone, Copy, Default)]
#[repr(transparent)]
pub struct EncryptedKey([u8; 16]);

impl EncryptedKey {
    pub fn decrypt(&self, password_aes: &Aes128) -> Key {
        let mut block = aes::Block::from(self.0);
        password_aes.decrypt_block(&mut block);
        Key(block.into())
    }
}

/// Salt used to prevent rainbow table attacks on the encryption password
#[derive(Clone, Copy, Default)]
#[repr(transparent)]
pub struct Salt([u8; 16]);

impl Salt {
    /// Generate a random salt
    #[cfg(feature = "std")]
    pub fn new() -> Result<Self, getrandom::Error> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes)?;
        Ok(Self(bytes))
    }
}

/// The key slot, containing the salt and encrypted key that are used with one password
#[derive(Clone, Copy, Default)]
#[repr(C, packed)]
pub struct KeySlot {
    salt: Salt,
    // Two keys for AES XTS 128
    encrypted_keys: (EncryptedKey, EncryptedKey),
}

impl KeySlot {
    pub fn is_empty(&self) -> bool {
        self.salt.0 == [0; 16]
            && self.encrypted_keys.0.0 == [0; 16]
            && self.encrypted_keys.1.0 == [0; 16]
    }

    pub fn to_bytes(&self) -> [u8; 48] {
        let mut bytes = [0; 48];
        bytes[..16].copy_from_slice(&self.salt.0);
        bytes[16..32].copy_from_slice(&self.encrypted_keys.0.0);
        bytes[32..].copy_from_slice(&self.encrypted_keys.1.0);
        bytes
    }

    pub fn from_bytes(bytes: &[u8; 48]) -> Self {
        Self {
            salt: Salt(bytes[..16].try_into().unwrap()),
            encrypted_keys: (
                EncryptedKey(bytes[16..32].try_into().unwrap()),
                EncryptedKey(bytes[32..].try_into().unwrap()),
            ),
        }
    }

    pub fn unwrap(&self, password: &[u8]) -> Result<(Key, Key), argon2::Error> {
        let aes = Self::password_aes(password, &self.salt)?;
        Ok((
            self.encrypted_keys.0.decrypt(&aes),
            self.encrypted_keys.1.decrypt(&aes),
        ))
    }

    /// Get the password AES key (generated from the password and salt, encrypts the real key)
    pub fn password_aes(password: &[u8], salt: &Salt) -> Result<Aes128, argon2::Error> {
        let mut key = Key([0; 16]);

        let mut params_builder = argon2::ParamsBuilder::new();
        params_builder.output_len(key.0.len())?;

        let argon2 = argon2::Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            params_builder.params()?,
        );

        argon2.hash_password_into(password, &salt.0, &mut key.0)?;

        Ok(key.into_aes())
    }

    /// Create a new key slot from a password, salt, and encryption key
    pub fn new(password: &[u8], salt: Salt, keys: (Key, Key)) -> Result<Self, argon2::Error> {
        let password_aes = Self::password_aes(password, &salt)?;
        Ok(Self {
            salt,
            encrypted_keys: (keys.0.encrypt(&password_aes), keys.1.encrypt(&password_aes)),
        })
    }

    /// Get the encryption cipher from this key slot
    pub fn cipher(&self, password: &[u8]) -> Result<Xts128<Aes128>, argon2::Error> {
        let password_aes = Self::password_aes(password, &self.salt)?;
        Ok(Xts128::new(
            self.encrypted_keys.0.decrypt(&password_aes).into_aes(),
            self.encrypted_keys.1.decrypt(&password_aes).into_aes(),
        ))
    }
}

pub fn key_slot_cas_accepts(current: [u8; 48], old: [u8; 48], new: [u8; 48]) -> bool {
    current == old || current == new
}

pub fn has_other_live_slot(slots: &[KeySlot; 64], index: usize) -> bool {
    slots
        .iter()
        .enumerate()
        .any(|(i, entry)| i != index && !entry.is_empty())
}

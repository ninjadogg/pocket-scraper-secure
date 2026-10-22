//! The vault: records are sealed to a public key as they are collected, so runs
//! never need the passphrase; reading them does.
//!
//! - Sealing: ephemeral X25519 → HKDF-SHA256 (salt = ephemeral ‖ recipient public
//!   key) → XChaCha20-Poly1305 with caller-supplied associated data.
//! - The X25519 secret is wrapped with XChaCha20-Poly1305 under a key derived from
//!   the passphrase with Argon2id.
//!
//! Wire formats are versioned by their first byte.

use crate::limits::{PASSPHRASE_MAX_BYTES, PASSPHRASE_MIN_CHARS};
use crate::random_bytes;
use crate::Error;
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, Zeroizing};

const SEAL_V1: u8 = 1;
const WRAP_INFO: &[u8] = b"pocket-scraper-secure/vault-wrap/v1";
const SEAL_INFO: &[u8] = b"pocket-scraper-secure/seal/v1";
const SIGNING_INFO: &[u8] = b"pocket-scraper-secure/vault-signing/v1";
const SEAL_OVERHEAD: usize = 1 + 32 + 24 + 16;

/// Argon2id cost. The defaults meet OWASP's minimum (19 MiB, 2 passes, 1 lane).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KdfParams {
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        KdfParams { memory_kib: 19 * 1024, iterations: 2, parallelism: 1 }
    }
}

impl KdfParams {
    fn check(&self) -> Result<Params, Error> {
        // Refuse headers that would make unlocking trivially cheap or absurdly costly.
        if !(19 * 1024..=1024 * 1024).contains(&self.memory_kib) || !(2..=10).contains(&self.iterations) || !(1..=4).contains(&self.parallelism) {
            return Err(Error::invalid("Vault key-derivation settings are out of range"));
        }
        Params::new(self.memory_kib, self.iterations, self.parallelism, Some(32)).map_err(|_| Error::invalid("Vault key-derivation settings are invalid"))
    }
}

/// Everything stored about the vault. Safe to keep in the clear: the secret key is
/// only present wrapped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct VaultHeader {
    pub version: u8,
    #[serde(with = "b64")]
    pub public_key: Vec<u8>,
    pub kdf: KdfParams,
    #[serde(with = "b64")]
    pub kdf_salt: Vec<u8>,
    /// nonce (24) ‖ XChaCha20-Poly1305(secret key)
    #[serde(with = "b64")]
    pub wrapped_secret: Vec<u8>,
    /// Ed25519 public key for recipe bundles, derived from the secret key. Empty in
    /// headers written before bundles existed; filled in at the next unlock.
    #[serde(default, with = "b64")]
    pub signing_key: Vec<u8>,
}

impl VaultHeader {
    pub fn public_key(&self) -> Result<[u8; 32], Error> {
        self.public_key.as_slice().try_into().map_err(|_| Error::invalid("Vault public key is malformed"))
    }
}

/// The unlocked secret key. Zeroized when dropped.
pub struct SecretKey(StaticSecret);

impl SecretKey {
    /// A fresh random key (the MCP server's one-time key for sealed answers).
    pub fn generate() -> SecretKey {
        SecretKey(StaticSecret::from(random_bytes::<32>()))
    }

    pub fn public_key(&self) -> [u8; 32] {
        PublicKey::from(&self.0).to_bytes()
    }

    /// The vault's Ed25519 key for signing recipe bundles, derived from the secret
    /// key, so it exists only while the vault is unlocked.
    pub fn signing_key(&self) -> ed25519_dalek::SigningKey {
        let secret = Zeroizing::new(self.0.to_bytes());
        let mut seed = Zeroizing::new([0u8; 32]);
        Hkdf::<Sha256>::new(None, secret.as_slice()).expand(SIGNING_INFO, seed.as_mut_slice()).expect("32 bytes is a valid HKDF length");
        ed25519_dalek::SigningKey::from_bytes(&seed)
    }

    /// The header with its signing public key filled in (headers made before bundles
    /// existed don't have it).
    pub fn complete_header(&self, header: &VaultHeader) -> VaultHeader {
        VaultHeader { signing_key: self.signing_key().verifying_key().to_bytes().to_vec(), ..header.clone() }
    }
}

pub fn check_passphrase(passphrase: &str) -> Result<(), Error> {
    if passphrase.chars().count() < PASSPHRASE_MIN_CHARS {
        return Err(Error::invalid(format!("Use a passphrase of at least {PASSPHRASE_MIN_CHARS} characters")));
    }
    if passphrase.len() > PASSPHRASE_MAX_BYTES {
        return Err(Error::invalid("Passphrase is too long"));
    }
    Ok(())
}

/// Creates a vault. Returns its header (to store) and the unlocked key.
pub fn create(passphrase: &str, kdf: KdfParams) -> Result<(VaultHeader, SecretKey), Error> {
    check_passphrase(passphrase)?;
    let secret = SecretKey(StaticSecret::from(random_bytes::<32>()));
    let header = wrap(&secret, passphrase, kdf)?;
    Ok((header, secret))
}

/// Re-wraps the same secret under a new passphrase; records stay readable. The key
/// itself does not change, so an old copy of the header still opens with the old
/// passphrase.
pub fn change_passphrase(header: &VaultHeader, old: &str, new: &str) -> Result<VaultHeader, Error> {
    check_passphrase(new)?;
    let secret = unlock(header, old)?;
    wrap(&secret, new, header.kdf)
}

fn wrap(secret: &SecretKey, passphrase: &str, kdf: KdfParams) -> Result<VaultHeader, Error> {
    let kdf_salt = random_bytes::<16>().to_vec();
    let public_key = secret.public_key().to_vec();
    let key = derive(passphrase, &kdf_salt, kdf)?;
    let nonce = random_bytes::<24>();
    let mut secret_bytes = Zeroizing::new(secret.0.to_bytes());
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_slice()).map_err(|_| Error::Crypto("Could not start encryption"))?;
    let ct = cipher
        .encrypt(&XNonce::from(nonce), Payload { msg: secret_bytes.as_slice(), aad: &wrap_aad(&public_key) })
        .map_err(|_| Error::Crypto("Could not wrap the vault key"))?;
    secret_bytes.zeroize();
    let mut wrapped_secret = nonce.to_vec();
    wrapped_secret.extend_from_slice(&ct);
    let signing_key = secret.signing_key().verifying_key().to_bytes().to_vec();
    Ok(VaultHeader { version: 1, public_key, kdf, kdf_salt, wrapped_secret, signing_key })
}

/// Unlocks the vault. A wrong passphrase and a tampered header look the same.
pub fn unlock(header: &VaultHeader, passphrase: &str) -> Result<SecretKey, Error> {
    if header.version != 1 || header.wrapped_secret.len() != 24 + 32 + 16 || header.kdf_salt.len() != 16 {
        return Err(Error::invalid("Vault header is malformed or from a newer version"));
    }
    let key = derive(passphrase, &header.kdf_salt, header.kdf)?;
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_slice()).map_err(|_| Error::Crypto("Could not start decryption"))?;
    let (nonce, ct) = header.wrapped_secret.split_at(24);
    let nonce: [u8; 24] = nonce.try_into().expect("24-byte nonce");
    let plain = Zeroizing::new(
        cipher
            .decrypt(&XNonce::from(nonce), Payload { msg: ct, aad: &wrap_aad(&header.public_key) })
            .map_err(|_| Error::Crypto("Wrong passphrase, or the vault was modified"))?,
    );
    let bytes: Zeroizing<[u8; 32]> = Zeroizing::new(plain.as_slice().try_into().map_err(|_| Error::Crypto("Vault key is malformed"))?);
    let secret = SecretKey(StaticSecret::from(*bytes));
    if secret.public_key().as_slice() != header.public_key.as_slice() {
        return Err(Error::Crypto("Vault key does not match its public key"));
    }
    if !header.signing_key.is_empty() && header.signing_key.as_slice() != secret.signing_key().verifying_key().as_bytes().as_slice() {
        return Err(Error::Crypto("Vault signing key does not match: the vault was modified"));
    }
    Ok(secret)
}

fn wrap_aad(public_key: &[u8]) -> Vec<u8> {
    [WRAP_INFO, public_key].concat()
}

fn derive(passphrase: &str, salt: &[u8], kdf: KdfParams) -> Result<Zeroizing<[u8; 32]>, Error> {
    check_passphrase(passphrase).map_err(|_| Error::Crypto("Wrong passphrase, or the vault was modified"))?;
    let params = kdf.check()?;
    let mut out = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, out.as_mut_slice())
        .map_err(|_| Error::Crypto("Could not derive the vault key"))?;
    Ok(out)
}

/// Seals `plaintext` to `recipient`. `aad` binds the ciphertext to its context
/// (recipe and record key), so a sealed record can't be moved to another record.
pub fn seal(recipient: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, Error> {
    let recipient = PublicKey::from(*recipient);
    let ephemeral = StaticSecret::from(random_bytes::<32>());
    let ephemeral_public = PublicKey::from(&ephemeral);
    let shared = ephemeral.diffie_hellman(&recipient);
    if !shared.was_contributory() {
        return Err(Error::Crypto("Vault public key is invalid"));
    }
    let key = seal_key(shared.as_bytes(), ephemeral_public.as_bytes(), recipient.as_bytes());
    let nonce = random_bytes::<24>();
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_slice()).map_err(|_| Error::Crypto("Could not start encryption"))?;
    let ct = cipher.encrypt(&XNonce::from(nonce), Payload { msg: plaintext, aad }).map_err(|_| Error::Crypto("Could not seal the record"))?;
    let mut out = Vec::with_capacity(SEAL_OVERHEAD + plaintext.len());
    out.push(SEAL_V1);
    out.extend_from_slice(ephemeral_public.as_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn open(secret: &SecretKey, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, Error> {
    if sealed.len() < SEAL_OVERHEAD || sealed[0] != SEAL_V1 {
        return Err(Error::Crypto("Record is malformed or from a newer version"));
    }
    let ephemeral: [u8; 32] = sealed[1..33].try_into().expect("32 bytes");
    let nonce: [u8; 24] = sealed[33..57].try_into().expect("24 bytes");
    let ephemeral = PublicKey::from(ephemeral);
    let shared = secret.0.diffie_hellman(&ephemeral);
    if !shared.was_contributory() {
        return Err(Error::Crypto("Record could not be opened"));
    }
    let key = seal_key(shared.as_bytes(), ephemeral.as_bytes(), &secret.public_key());
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_slice()).map_err(|_| Error::Crypto("Could not start decryption"))?;
    cipher
        .decrypt(&XNonce::from(nonce), Payload { msg: &sealed[57..], aad })
        .map_err(|_| Error::Crypto("Record could not be opened: it was modified or belongs elsewhere"))
}

fn seal_key(shared: &[u8; 32], ephemeral: &[u8; 32], recipient: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    let salt = [ephemeral.as_slice(), recipient.as_slice()].concat();
    let mut key = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(&salt), shared).expand(SEAL_INFO, key.as_mut_slice()).expect("32 bytes is a valid HKDF length");
    key
}

/// Bridge answers are sealed to the asking MCP process's one-time key, bound to the
/// request ID, so the mailbox never holds opened records.
fn reply_aad(id: &str) -> Vec<u8> {
    [b"pocket-scraper-secure/bridge-reply/v1\0".as_slice(), id.as_bytes()].concat()
}

pub fn seal_reply(recipient_b64: &str, id: &str, plaintext: &[u8]) -> Result<String, Error> {
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;
    let key: [u8; 32] = engine.decode(recipient_b64.trim()).ok().and_then(|k| k.try_into().ok()).ok_or_else(|| Error::invalid("The reply key is malformed"))?;
    Ok(engine.encode(seal(&key, &reply_aad(id), plaintext)?))
}

pub fn open_reply(secret: &SecretKey, id: &str, sealed_b64: &str) -> Result<Vec<u8>, Error> {
    use base64::Engine;
    let sealed = base64::engine::general_purpose::STANDARD.decode(sealed_b64).map_err(|_| Error::invalid("The sealed answer is malformed"))?;
    open(secret, &reply_aad(id), &sealed)
}

/// Associated data for a sealed record.
pub fn record_aad(recipe_id: &str, record_key: &str) -> Vec<u8> {
    [recipe_id.as_bytes(), &[0], record_key.as_bytes()].concat()
}

pub(crate) mod b64 {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_open_only_with_the_one_time_key_and_id() {
        use base64::Engine;
        let key = SecretKey::generate();
        let public = base64::engine::general_purpose::STANDARD.encode(key.public_key());
        let sealed = seal_reply(&public, "123-ab", b"{\"records\":[]}").unwrap();
        assert_eq!(open_reply(&key, "123-ab", &sealed).unwrap(), b"{\"records\":[]}");
        assert!(open_reply(&key, "123-ac", &sealed).is_err(), "bound to the request ID");
        assert!(open_reply(&SecretKey::generate(), "123-ab", &sealed).is_err());
        assert!(seal_reply("not a key", "x", b"").is_err());
    }

    const PASS: &str = "correct horse battery";

    #[test]
    fn records_seal_with_the_public_key_and_open_only_after_unlock() {
        let (header, _) = create(PASS, KdfParams::default()).unwrap();
        let header: VaultHeader = serde_json::from_str(&serde_json::to_string(&header).unwrap()).unwrap();
        let aad = record_aad("recipe", "key");
        let sealed = seal(&header.public_key().unwrap(), &aad, b"secret row").unwrap();
        assert_eq!(sealed.len(), SEAL_OVERHEAD + 10);
        let key = unlock(&header, PASS).unwrap();
        assert_eq!(open(&key, &aad, &sealed).unwrap(), b"secret row");
    }

    #[test]
    fn wrong_passphrase_tampering_and_moved_records_are_refused() {
        let (header, key) = create(PASS, KdfParams::default()).unwrap();
        assert_eq!(unlock(&header, "wrong horse battery").err(), Some(Error::Crypto("Wrong passphrase, or the vault was modified")));
        let mut tampered = header.clone();
        tampered.wrapped_secret[30] ^= 1;
        assert!(unlock(&tampered, PASS).is_err());
        let mut swapped = header.clone();
        swapped.public_key = create(PASS, KdfParams::default()).unwrap().0.public_key;
        assert!(unlock(&swapped, PASS).is_err(), "a swapped public key is detected");

        let aad = record_aad("recipe", "key-1");
        let mut sealed = seal(&header.public_key().unwrap(), &aad, b"row").unwrap();
        assert!(open(&key, &record_aad("recipe", "key-2"), &sealed).is_err(), "bound to its record");
        sealed[60] ^= 1;
        assert!(open(&key, &aad, &sealed).is_err());
        assert!(open(&key, &aad, &sealed[..10]).is_err());
    }

    #[test]
    fn weak_passphrases_and_cheap_kdf_settings_are_refused() {
        assert!(create("short", KdfParams::default()).is_err());
        assert!(create(PASS, KdfParams { memory_kib: 1024, iterations: 1, parallelism: 1 }).is_err());
        let (mut header, _) = create(PASS, KdfParams::default()).unwrap();
        header.kdf.memory_kib = 8;
        assert!(unlock(&header, PASS).is_err());
    }

    #[test]
    fn changing_the_passphrase_keeps_records_readable() {
        let (header, _) = create(PASS, KdfParams::default()).unwrap();
        let aad = record_aad("r", "k");
        let sealed = seal(&header.public_key().unwrap(), &aad, b"kept").unwrap();
        let changed = change_passphrase(&header, PASS, "a brand new passphrase").unwrap();
        assert_eq!(changed.public_key, header.public_key);
        assert!(unlock(&changed, PASS).is_err());
        assert_eq!(open(&unlock(&changed, "a brand new passphrase").unwrap(), &aad, &sealed).unwrap(), b"kept");
    }

    #[test]
    fn low_order_public_keys_are_refused() {
        assert!(seal(&[0u8; 32], b"", b"x").is_err());
    }
}

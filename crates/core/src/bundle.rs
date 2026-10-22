//! Teach once, run anywhere. A recipe leaves the browser as a *bundle* signed by the
//! vault (only while it is unlocked). Runners on other machines pin the vault's
//! signing key and refuse anything else. Their results come back as *batches*: sealed
//! records plus run status, signed by the runner's own key, which the vault's owner
//! enrolled. Runners never hold the passphrase or the vault's secret key.

use crate::limits::*;
use crate::protocol::{check_id, SealedRecord};
use crate::recipe::{Recipe, RecipeInput};
use crate::vault::{b64, SecretKey};
use crate::Error;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub const BUNDLE_FORMAT: &str = "pss-bundle-1";
pub const BATCH_FORMAT: &str = "pss-batch-1";
pub const BUNDLE_MAX: usize = 512 * 1024;
pub const BATCH_MAX: usize = 8 * 1024 * 1024;

/// The signed envelope used by both file types. The signature covers the format name
/// and the exact payload bytes, so a bundle can't be passed off as a batch.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    format: String,
    #[serde(with = "b64")]
    payload: Vec<u8>,
    #[serde(with = "b64")]
    signature: Vec<u8>,
}

fn signed_message(format: &str, payload: &[u8]) -> Vec<u8> {
    [format.as_bytes(), &[0], payload].concat()
}

fn seal_envelope(format: &str, key: &SigningKey, payload: Vec<u8>) -> String {
    let signature = key.sign(&signed_message(format, &payload)).to_bytes().to_vec();
    serde_json::to_string_pretty(&Envelope { format: format.into(), payload, signature }).expect("envelope serialises")
}

fn open_envelope(json: &str, format: &str, max: usize) -> Result<(Vec<u8>, Signature), Error> {
    if json.len() > max {
        return Err(Error::invalid(format!("The file is larger than {} KB", max / 1024)));
    }
    let envelope: Envelope = serde_json::from_str(json).map_err(|e| Error::invalid(format!("The file is malformed: {e}")))?;
    if envelope.format != format {
        return Err(Error::invalid(format!("Expected a {format} file, got {}", envelope.format)));
    }
    let signature = Signature::from_slice(&envelope.signature).map_err(|_| Error::Crypto("The file's signature is malformed"))?;
    Ok((envelope.payload, signature))
}

fn verifying_key(bytes: &[u8]) -> Result<VerifyingKey, Error> {
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| Error::Crypto("Signing key is malformed"))?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| Error::Crypto("Signing key is invalid"))
}

/// A short, human-comparable fingerprint of a public key, like `3f2a-9c41-07bd-e5f2-6a18`.
pub fn fingerprint(key: &[u8]) -> String {
    let hash = Sha256::digest(key);
    let hex = crate::hex(&hash[..10]);
    hex.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).expect("hex")).collect::<Vec<_>>().join("-")
}

fn check_timestamp(ts: &str) -> Result<(), Error> {
    if ts.is_empty() || ts.len() > 40 || !ts.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'-' | b':' | b'.' | b'T' | b'Z' | b'+')) {
        return Err(Error::invalid("Timestamp is malformed"));
    }
    Ok(())
}

// ── recipe bundles ───────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct BundlePayload {
    bundle_id: String,
    created_at: String,
    #[serde(with = "b64")]
    vault_signing_key: Vec<u8>,
    #[serde(with = "b64")]
    vault_public_key: Vec<u8>,
    run_timeout_ms: u64,
    recipe: serde_json::Value,
}

/// A verified bundle.
#[derive(Debug, Clone)]
pub struct Bundle {
    pub bundle_id: String,
    pub created_at: String,
    pub vault_signing_key: [u8; 32],
    pub vault_public_key: [u8; 32],
    pub run_timeout_ms: u64,
    pub recipe: Recipe,
}

pub fn sign_bundle(secret: &SecretKey, recipe: &Recipe, created_at: &str, run_timeout_ms: u64) -> Result<String, Error> {
    check_timestamp(created_at)?;
    if !(1_000..=RUN_TIMEOUT_MAX_MS).contains(&run_timeout_ms) {
        return Err(Error::invalid("Run timeout is out of range"));
    }
    let key = secret.signing_key();
    let payload = BundlePayload {
        bundle_id: crate::hex(&crate::random_bytes::<16>()),
        created_at: created_at.into(),
        vault_signing_key: key.verifying_key().to_bytes().to_vec(),
        vault_public_key: secret.public_key().to_vec(),
        run_timeout_ms,
        recipe: serde_json::from_str(&recipe.to_json()).expect("recipe JSON"),
    };
    Ok(seal_envelope(BUNDLE_FORMAT, &key, serde_json::to_vec(&payload).expect("payload serialises")))
}

/// Verifies a bundle. `pinned` is the vault signing key this runner already trusts;
/// `None` only when adding the first bundle, after the person compared fingerprints.
pub fn open_bundle(json: &str, pinned: Option<&[u8; 32]>) -> Result<Bundle, Error> {
    let (payload, signature) = open_envelope(json, BUNDLE_FORMAT, BUNDLE_MAX)?;
    let p: BundlePayload = serde_json::from_slice(&payload).map_err(|e| Error::invalid(format!("The bundle is malformed: {e}")))?;
    let key = verifying_key(&p.vault_signing_key)?;
    key.verify_strict(&signed_message(BUNDLE_FORMAT, &payload), &signature).map_err(|_| Error::Crypto("The bundle's signature is not valid: it was modified or not made by a vault"))?;
    if let Some(pinned) = pinned {
        if pinned != key.as_bytes() {
            return Err(Error::Crypto("The bundle was signed by a different vault than the one this runner trusts"));
        }
    }
    check_id(&p.bundle_id)?;
    check_timestamp(&p.created_at)?;
    if !(1_000..=RUN_TIMEOUT_MAX_MS).contains(&p.run_timeout_ms) {
        return Err(Error::invalid("Run timeout is out of range"));
    }
    let vault_public_key: [u8; 32] = p.vault_public_key.as_slice().try_into().map_err(|_| Error::invalid("Vault public key is malformed"))?;
    let input: RecipeInput = serde_json::from_value(p.recipe).map_err(|e| Error::invalid(format!("The bundle's recipe is malformed: {e}")))?;
    let recipe = Recipe::validate(input)?;
    Ok(Bundle { bundle_id: p.bundle_id, created_at: p.created_at, vault_signing_key: *key.as_bytes(), vault_public_key, run_timeout_ms: p.run_timeout_ms, recipe })
}

// ── runner identity and result batches ───────────────────────────────────────

/// A runner's own signing key. Kept by the runner (owner-only file); its public half
/// is enrolled in the vault owner's dashboard.
pub struct RunnerKey(SigningKey);

impl RunnerKey {
    pub fn generate() -> RunnerKey {
        let seed = Zeroizing::new(crate::random_bytes::<32>());
        RunnerKey(SigningKey::from_bytes(&seed))
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<RunnerKey, Error> {
        let seed: Zeroizing<[u8; 32]> = Zeroizing::new(bytes.try_into().map_err(|_| Error::invalid("Runner key file is malformed"))?);
        Ok(RunnerKey(SigningKey::from_bytes(&seed)))
    }
    pub fn to_bytes(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.to_bytes())
    }
    pub fn public(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchStatus {
    Success,
    Error,
    NeedsAttention,
}

/// One run's results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Batch {
    pub batch_id: String,
    #[serde(with = "b64")]
    pub runner_key: Vec<u8>,
    #[serde(with = "b64")]
    pub vault_public_key: Vec<u8>,
    pub recipe_id: String,
    pub run_id: String,
    pub started_at: String,
    pub finished_at: String,
    pub status: BatchStatus,
    pub row_count: usize,
    pub warnings: Vec<String>,
    #[serde(default)]
    pub error: Option<String>,
    pub records: Vec<SealedRecord>,
}

impl Batch {
    fn check(&self) -> Result<(), Error> {
        check_id(&self.batch_id)?;
        check_id(&self.recipe_id)?;
        check_id(&self.run_id)?;
        check_timestamp(&self.started_at)?;
        check_timestamp(&self.finished_at)?;
        if self.records.len() > RUN_ROWS_MAX || self.row_count > RUN_ROWS_MAX {
            return Err(Error::invalid("The batch has too many records"));
        }
        if self.warnings.len() > RUN_WARNINGS_MAX + 1 || self.warnings.iter().any(|w| w.len() > WARNING_BYTES_MAX) {
            return Err(Error::invalid("The batch has too many or too long warnings"));
        }
        if self.error.as_ref().is_some_and(|e| e.len() > WARNING_BYTES_MAX) {
            return Err(Error::invalid("The batch's error message is too long"));
        }
        if self.records.iter().any(|r| r.key.len() != 64 || !r.key.bytes().all(|b| b.is_ascii_hexdigit()) || r.sealed.len() > ROW_BYTES_MAX * 2) {
            return Err(Error::invalid("The batch has a malformed record"));
        }
        Ok(())
    }
}

pub fn sign_batch(runner: &RunnerKey, batch: &Batch) -> Result<String, Error> {
    if batch.runner_key.as_slice() != runner.public().as_slice() {
        return Err(Error::invalid("The batch names a different runner"));
    }
    batch.check()?;
    Ok(seal_envelope(BATCH_FORMAT, &runner.0, serde_json::to_vec(batch).expect("batch serialises")))
}

/// Verifies a batch: signed by one of the `enrolled` runners and sealed to this vault.
pub fn open_batch(json: &str, enrolled: &[[u8; 32]], vault_public_key: &[u8; 32]) -> Result<Batch, Error> {
    let (payload, signature) = open_envelope(json, BATCH_FORMAT, BATCH_MAX)?;
    let batch: Batch = serde_json::from_slice(&payload).map_err(|e| Error::invalid(format!("The batch is malformed: {e}")))?;
    let runner: [u8; 32] = batch.runner_key.as_slice().try_into().map_err(|_| Error::invalid("The batch's runner key is malformed"))?;
    if !enrolled.contains(&runner) {
        return Err(Error::Crypto("The batch comes from a runner that isn't enrolled"));
    }
    verifying_key(&runner)?.verify_strict(&signed_message(BATCH_FORMAT, &payload), &signature).map_err(|_| Error::Crypto("The batch's signature is not valid: it was modified"))?;
    if batch.vault_public_key.as_slice() != vault_public_key.as_slice() {
        return Err(Error::Crypto("The batch was sealed for a different vault"));
    }
    batch.check()?;
    Ok(batch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::{self, KdfParams};

    const PASS: &str = "correct horse battery";

    fn recipe() -> Recipe {
        Recipe::from_json(r#"{"id":"r1","name":"Quotes","url":"https://quotes.toscrape.com/","rowSelector":"div.quote","fields":[{"name":"quote","selector":"span.text"}],"nextSelector":"li.next > a","maxPages":3,"scheduleMinutes":60}"#).unwrap()
    }

    fn batch(runner: &RunnerKey, vault_key: &[u8; 32]) -> Batch {
        Batch {
            batch_id: "b1".into(),
            runner_key: runner.public().to_vec(),
            vault_public_key: vault_key.to_vec(),
            recipe_id: "r1".into(),
            run_id: "run-1".into(),
            started_at: "2026-10-22T12:00:00Z".into(),
            finished_at: "2026-10-22T12:00:05Z".into(),
            status: BatchStatus::Success,
            row_count: 1,
            warnings: vec![],
            error: None,
            records: vec![SealedRecord { key: "a".repeat(64), sealed: vec![1, 2, 3] }],
        }
    }

    #[test]
    fn bundles_verify_pin_and_reject_tampering() {
        let (header, secret) = vault::create(PASS, KdfParams::default()).unwrap();
        let json = sign_bundle(&secret, &recipe(), "2026-10-22T12:00:00Z", 180_000).unwrap();
        let b = open_bundle(&json, None).unwrap();
        assert_eq!(b.recipe, recipe());
        assert_eq!(b.vault_public_key, header.public_key().unwrap());
        assert_eq!(b.vault_signing_key.as_slice(), header.signing_key.as_slice());
        assert!(open_bundle(&json, Some(&b.vault_signing_key)).is_ok());

        let (_, other) = vault::create(PASS, KdfParams::default()).unwrap();
        let foreign = sign_bundle(&other, &recipe(), "2026-10-22T12:00:00Z", 180_000).unwrap();
        assert!(open_bundle(&foreign, Some(&b.vault_signing_key)).unwrap_err().to_string().contains("different vault"));

        // Change the recipe's URL inside the signed payload.
        let mut env: serde_json::Value = serde_json::from_str(&json).unwrap();
        let payload = base64_decode(env["payload"].as_str().unwrap());
        let tampered = String::from_utf8(payload).unwrap().replace("quotes.toscrape.com", "evil.example");
        env["payload"] = base64_encode(tampered.as_bytes()).into();
        assert!(open_bundle(&env.to_string(), None).unwrap_err().to_string().contains("not valid"));
        // A bundle is not a batch.
        assert!(open_batch(&json, &[], &[0; 32]).unwrap_err().to_string().contains("Expected a pss-batch-1"));
    }

    #[test]
    fn batches_need_an_enrolled_runner_and_this_vault() {
        let runner = RunnerKey::generate();
        let vault_key = [7u8; 32];
        let json = sign_batch(&runner, &batch(&runner, &vault_key)).unwrap();
        assert_eq!(open_batch(&json, &[runner.public()], &vault_key).unwrap(), batch(&runner, &vault_key));
        assert!(open_batch(&json, &[], &vault_key).unwrap_err().to_string().contains("isn't enrolled"));
        assert!(open_batch(&json, &[runner.public()], &[8u8; 32]).unwrap_err().to_string().contains("different vault"));

        // Another runner can't sign in this runner's name.
        let impostor = RunnerKey::generate();
        assert!(sign_batch(&impostor, &batch(&runner, &vault_key)).is_err());
        let mut env: serde_json::Value = serde_json::from_str(&json).unwrap();
        let forged = sign_batch(&impostor, &batch(&impostor, &vault_key)).unwrap();
        let forged_env: serde_json::Value = serde_json::from_str(&forged).unwrap();
        env["signature"] = forged_env["signature"].clone();
        assert!(open_batch(&env.to_string(), &[runner.public()], &vault_key).unwrap_err().to_string().contains("not valid"));

        let restored = RunnerKey::from_bytes(runner.to_bytes().as_slice()).unwrap();
        assert_eq!(restored.public(), runner.public());
        assert_eq!(fingerprint(&runner.public()).len(), 24);
    }

    #[test]
    fn oversized_or_malformed_batches_are_refused() {
        let runner = RunnerKey::generate();
        let mut b = batch(&runner, &[7; 32]);
        b.records[0].key = "not-hex".into();
        assert!(sign_batch(&runner, &b).is_err());
        assert!(open_batch(&"x".repeat(BATCH_MAX + 1), &[runner.public()], &[7; 32]).is_err());
    }

    #[test]
    fn unlock_refuses_a_swapped_signing_key_and_completes_old_headers() {
        let (header, secret) = vault::create(PASS, KdfParams::default()).unwrap();
        let mut swapped = header.clone();
        swapped.signing_key = RunnerKey::generate().public().to_vec();
        assert!(vault::unlock(&swapped, PASS).is_err());
        let mut old = header.clone();
        old.signing_key.clear();
        let old: vault::VaultHeader = serde_json::from_str(&serde_json::to_string(&old).unwrap().replace(r#","signingKey":"""#, "")).unwrap();
        let key = vault::unlock(&old, PASS).unwrap();
        assert_eq!(key.complete_header(&old), header);
        drop(secret);
    }

    fn base64_decode(s: &str) -> Vec<u8> {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.decode(s).unwrap()
    }
    fn base64_encode(b: &[u8]) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(b)
    }
}

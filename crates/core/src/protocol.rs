//! Messages between the extension and the native host, and sealing a run's rows into
//! records. Both directions are validated: Chrome only lets this extension start the
//! host, but neither side trusts the other's input.

use crate::limits::{HOST_MESSAGE_MAX, ID_MAX, RUN_TIMEOUT_MAX_MS};
use crate::records::{new_record_key, PlainRecord, Row};
use crate::recipe::{Recipe, RecipeInput};
use crate::vault::{self, b64};
use crate::Error;
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

/// Extension → host.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum Request {
    Hello { version: u32 },
    Run {
        run_id: String,
        recipe: RecipeInput,
        #[serde(with = "b64")]
        public_key: Vec<u8>,
        run_timeout_ms: u64,
        observed_at: String,
    },
    /// Open a visible browser on the site's profile so the person can sign in.
    OpenSession { url: String },
    /// Close it again; cookies stay in the profile.
    CloseSession { url: String },
}

/// A validated `Request::Run`.
pub struct RunRequest {
    pub run_id: String,
    pub recipe: Recipe,
    pub public_key: [u8; 32],
    pub run_timeout_ms: u64,
    pub observed_at: String,
}

impl Request {
    pub fn from_json(json: &[u8]) -> Result<Request, Error> {
        if json.len() > HOST_MESSAGE_MAX {
            return Err(Error::invalid("Message is larger than 1 MiB"));
        }
        serde_json::from_slice(json).map_err(|e| Error::invalid(format!("Message is malformed: {e}")))
    }
}

pub fn validate_run(run_id: String, recipe: RecipeInput, public_key: Vec<u8>, run_timeout_ms: u64, observed_at: String) -> Result<RunRequest, Error> {
    check_id(&run_id)?;
    check_timestamp(&observed_at)?;
    let public_key: [u8; 32] = public_key.try_into().map_err(|_| Error::invalid("Public key must be 32 bytes"))?;
    if !(1_000..=RUN_TIMEOUT_MAX_MS).contains(&run_timeout_ms) {
        return Err(Error::invalid("Run timeout is out of range"));
    }
    Ok(RunRequest { run_id, recipe: Recipe::validate(recipe)?, public_key, run_timeout_ms, observed_at })
}

/// Host → extension.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Response {
    Hello { version: u32, host_version: String },
    /// Part of a run's records; a run may send several before `RunDone`.
    RunChunk { run_id: String, records: Vec<SealedRecord> },
    RunDone { run_id: String, row_count: usize, warnings: Vec<String> },
    RunFailed { run_id: String, message: String, attention: bool },
    SessionOpened { origin: String },
    SessionClosed { origin: String },
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedRecord {
    pub key: String,
    #[serde(with = "b64")]
    pub sealed: Vec<u8>,
}

/// Seals every row of a run under a fresh random key. Duplicates are removed later,
/// by `records::duplicates`, once the vault is unlocked.
pub fn seal_rows(public_key: &[u8; 32], recipe_id: &str, run_id: &str, observed_at: &str, rows: &[Row]) -> Result<Vec<SealedRecord>, Error> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let key = new_record_key();
        let plain = PlainRecord { observed_at: observed_at.into(), run_id: run_id.into(), page_url: row.page_url.clone(), fields: row.fields.clone() };
        let json = serde_json::to_vec(&plain).expect("record serialises");
        let sealed = vault::seal(public_key, &vault::record_aad(recipe_id, &key), &json)?;
        out.push(SealedRecord { key, sealed });
    }
    Ok(out)
}

/// Splits records so each `RunChunk` stays under Chrome's 1 MiB message limit.
pub fn chunk(records: Vec<SealedRecord>, max_bytes: usize) -> Vec<Vec<SealedRecord>> {
    let mut chunks = vec![];
    let mut current = vec![];
    let mut size = 0;
    for r in records {
        // key + base64 + JSON punctuation
        let cost = r.key.len() + r.sealed.len().div_ceil(3) * 4 + 32;
        if !current.is_empty() && size + cost > max_bytes {
            chunks.push(std::mem::take(&mut current));
            size = 0;
        }
        size += cost;
        current.push(r);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

pub fn check_id(id: &str) -> Result<(), Error> {
    if id.is_empty() || id.len() > ID_MAX || !id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        return Err(Error::invalid("IDs must be 1–64 lower-case letters, digits or dashes"));
    }
    Ok(())
}

fn check_timestamp(ts: &str) -> Result<(), Error> {
    if ts.is_empty() || ts.len() > 40 || !ts.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'-' | b':' | b'.' | b'T' | b'Z' | b'+')) {
        return Err(Error::invalid("Timestamp is malformed"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::{create, open, record_aad, unlock, KdfParams};

    #[test]
    fn run_requests_parse_and_validate() {
        let json = serde_json::json!({
            "type": "run", "runId": "run-1", "observedAt": "2026-10-22T12:00:00.000Z", "runTimeoutMs": 180000,
            "publicKey": "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
            "recipe": { "id": "abc", "name": "t", "url": "https://example.org", "fields": [{ "name": "a", "selector": "x" }] }
        });
        let Request::Run { run_id, recipe, public_key, run_timeout_ms, observed_at } = Request::from_json(json.to_string().as_bytes()).unwrap() else { panic!() };
        let run = validate_run(run_id, recipe, public_key, run_timeout_ms, observed_at).unwrap();
        assert_eq!(run.recipe.origin, "https://example.org");
        assert_eq!(run.public_key, [1u8; 32]);

        let mut bad = json.clone();
        bad["shell"] = "rm -rf /".into();
        assert!(Request::from_json(bad.to_string().as_bytes()).is_err(), "unknown fields are refused");
        let mut bad = json.clone();
        bad["recipe"]["url"] = "file:///etc/passwd".into();
        let Request::Run { run_id, recipe, public_key, run_timeout_ms, observed_at } = Request::from_json(bad.to_string().as_bytes()).unwrap() else { panic!() };
        assert!(validate_run(run_id, recipe, public_key, run_timeout_ms, observed_at).is_err());
        assert!(Request::from_json(&vec![b' '; HOST_MESSAGE_MAX + 1]).is_err());
        assert!(check_id("../x").is_err());
    }

    #[test]
    fn every_row_seals_under_its_own_random_key_and_opens_with_the_vault() {
        let (header, _) = create("correct horse battery", KdfParams::default()).unwrap();
        let row = |v: &str| Row { page_url: "https://example.org/".into(), fields: [("a".to_string(), v.to_string())].into() };
        let sealed = seal_rows(&header.public_key().unwrap(), "recipe", "run-1", "2026-10-22T12:00:00Z", &[row("1"), row("2"), row("1")]).unwrap();
        assert_eq!(sealed.len(), 3, "repeats are kept");
        assert_ne!(sealed[0].key, sealed[2].key, "equal rows don't share a key");
        let key = unlock(&header, "correct horse battery").unwrap();
        let plain: PlainRecord = serde_json::from_slice(&open(&key, &record_aad("recipe", &sealed[1].key), &sealed[1].sealed).unwrap()).unwrap();
        assert_eq!(plain.fields["a"], "2");
        assert_eq!(plain.run_id, "run-1");
    }

    #[test]
    fn chunks_stay_under_the_limit() {
        let records: Vec<SealedRecord> = (0..100).map(|i| SealedRecord { key: format!("{i:064}"), sealed: vec![0; 30_000] }).collect();
        let chunks = chunk(records, HOST_MESSAGE_MAX - 4096);
        assert_eq!(chunks.iter().map(Vec::len).sum::<usize>(), 100);
        for c in &chunks {
            let json = serde_json::to_string(&Response::RunChunk { run_id: "r".into(), records: c.clone() }).unwrap();
            assert!(json.len() < HOST_MESSAGE_MAX, "{}", json.len());
        }
    }
}

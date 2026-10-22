//! WebAssembly bindings for the extension. Values cross the boundary as JSON strings
//! (validated on the Rust side) or base64; the unlocked vault key never does.

use pocket_scraper_core::plan::{Action, Observation, Run, RunOptions};
use pocket_scraper_core::protocol::{seal_rows as seal, SealedRecord};
use pocket_scraper_core::recipe::Recipe;
use pocket_scraper_core::records::{PlainRecord, Row};
use pocket_scraper_core::vault::{self, KdfParams, SecretKey, VaultHeader};
use pocket_scraper_core::{csv, limits};
use wasm_bindgen::prelude::*;

fn js(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

fn header(json: &str) -> Result<VaultHeader, JsError> {
    serde_json::from_str(json).map_err(|e| js(format!("Vault header is malformed: {e}")))
}

/// Validates and normalises a recipe; returns its JSON.
#[wasm_bindgen(js_name = validateRecipe)]
pub fn validate_recipe(json: &str) -> Result<String, JsError> {
    Ok(Recipe::from_json(json).map_err(js)?.to_json())
}

/// The recipe's origin (`scheme://host[:port]`), for host permissions.
#[wasm_bindgen(js_name = recipeOrigin)]
pub fn recipe_origin(json: &str) -> Result<String, JsError> {
    Ok(Recipe::from_json(json).map_err(js)?.origin)
}

/// One run of a recipe. The tab driver calls `start`, then `observe` with each
/// observation until the returned action is `finish` or `fail`.
#[wasm_bindgen]
pub struct Planner {
    run: Run,
}

#[wasm_bindgen]
impl Planner {
    #[wasm_bindgen(constructor)]
    pub fn new(recipe_json: &str, run_timeout_ms: f64, page_change_timeout_ms: f64) -> Result<Planner, JsError> {
        let recipe = Recipe::from_json(recipe_json).map_err(js)?;
        let clamp = |v: f64, max: u64| if v.is_finite() && v >= 1_000.0 { (v as u64).min(max) } else { 1_000 };
        let options = RunOptions {
            run_timeout_ms: clamp(run_timeout_ms, limits::RUN_TIMEOUT_MAX_MS),
            page_change_timeout_ms: clamp(page_change_timeout_ms, 120_000),
        };
        Ok(Planner { run: Run::new(recipe, options) })
    }

    pub fn start(&mut self, now_ms: f64) -> String {
        to_json(&self.run.start(now_ms as u64))
    }

    /// A malformed observation fails the run rather than throwing.
    pub fn observe(&mut self, observation_json: &str) -> String {
        if observation_json.len() > limits::RUN_BYTES_MAX * 2 {
            return to_json(&Action::Fail { message: "Driver observation is too large".into(), attention: false });
        }
        match serde_json::from_str::<Observation>(observation_json) {
            Ok(observation) => to_json(&self.run.observe(observation)),
            Err(e) => to_json(&Action::Fail { message: format!("Driver observation is malformed: {e}"), attention: false }),
        }
    }
}

fn to_json(action: &Action) -> String {
    serde_json::to_string(action).expect("action serialises")
}

/// Creates a vault; returns its header JSON. Unlock it to read records.
#[wasm_bindgen(js_name = createVault)]
pub fn create_vault(passphrase: &str) -> Result<String, JsError> {
    let (header, _secret) = vault::create(passphrase, KdfParams::default()).map_err(js)?;
    Ok(serde_json::to_string(&header).expect("header serialises"))
}

#[wasm_bindgen(js_name = changePassphrase)]
pub fn change_passphrase(header_json: &str, old: &str, new: &str) -> Result<String, JsError> {
    let header = vault::change_passphrase(&header(header_json)?, old, new).map_err(js)?;
    Ok(serde_json::to_string(&header).expect("header serialises"))
}

/// Seals a finished run's rows (JSON array) to the vault's public key. Returns a
/// JSON array of `{ key, sealed }` with `sealed` in base64.
#[wasm_bindgen(js_name = sealRows)]
pub fn seal_rows(header_json: &str, recipe_id: &str, run_id: &str, observed_at: &str, rows_json: &str) -> Result<String, JsError> {
    let header = header(header_json)?;
    pocket_scraper_core::protocol::check_id(recipe_id).map_err(js)?;
    pocket_scraper_core::protocol::check_id(run_id).map_err(js)?;
    let rows: Vec<Row> = serde_json::from_str(rows_json).map_err(|e| js(format!("Rows are malformed: {e}")))?;
    if rows.len() > limits::RUN_ROWS_MAX {
        return Err(js("Too many rows"));
    }
    let sealed = seal(&header.public_key().map_err(js)?, &header.record_salt().map_err(js)?, recipe_id, run_id, observed_at, &rows).map_err(js)?;
    Ok(serde_json::to_string(&sealed).expect("records serialise"))
}

/// Checks a sealed record received from the native host before it is stored.
#[wasm_bindgen(js_name = checkSealedRecords)]
pub fn check_sealed_records(json: &str) -> Result<String, JsError> {
    let records: Vec<SealedRecord> = serde_json::from_str(json).map_err(|e| js(format!("Records are malformed: {e}")))?;
    if records.len() > limits::RUN_ROWS_MAX || records.iter().any(|r| r.key.len() != 64 || !r.key.bytes().all(|b| b.is_ascii_hexdigit()) || r.sealed.len() > limits::RUN_BYTES_MAX) {
        return Err(js("Records are malformed"));
    }
    Ok(serde_json::to_string(&records).expect("records serialise"))
}

/// The unlocked vault. Holds the secret key in WASM memory only; `free()` (or
/// dropping the page) zeroizes it.
#[wasm_bindgen]
pub struct UnlockedVault {
    secret: SecretKey,
}

#[wasm_bindgen]
impl UnlockedVault {
    /// Slow on purpose (Argon2id). Fails on a wrong passphrase or modified header.
    pub fn unlock(header_json: &str, passphrase: &str) -> Result<UnlockedVault, JsError> {
        Ok(UnlockedVault { secret: vault::unlock(&header(header_json)?, passphrase).map_err(js)? })
    }

    /// Opens one sealed record (base64); returns its plaintext JSON.
    pub fn open(&self, recipe_id: &str, key: &str, sealed_b64: &str) -> Result<String, JsError> {
        let sealed: SealedRecord = serde_json::from_value(serde_json::json!({ "key": key, "sealed": sealed_b64 })).map_err(|_| js("Record is malformed"))?;
        let plain = vault::open(&self.secret, &vault::record_aad(recipe_id, &sealed.key), &sealed.sealed).map_err(js)?;
        let record: PlainRecord = serde_json::from_slice(&plain).map_err(|_| js("Record contents are malformed"))?;
        Ok(serde_json::to_string(&record).expect("record serialises"))
    }
}

/// Spreadsheet-safe CSV from opened records (JSON array) in the recipe's field order.
#[wasm_bindgen(js_name = toCsv)]
pub fn to_csv(field_order_json: &str, records_json: &str) -> Result<String, JsError> {
    let order: Vec<String> = serde_json::from_str(field_order_json).map_err(|e| js(format!("Field order is malformed: {e}")))?;
    let records: Vec<PlainRecord> = serde_json::from_str(records_json).map_err(|e| js(format!("Records are malformed: {e}")))?;
    Ok(csv::to_csv(&order, &records))
}

#[wasm_bindgen(js_name = minPassphraseChars)]
pub fn min_passphrase_chars() -> usize {
    limits::PASSPHRASE_MIN_CHARS
}

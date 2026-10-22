//! Extractor output (untrusted: the page controls the DOM it was read from), the
//! canonical row form, and record keys.

use crate::limits::*;
use crate::recipe::Recipe;
use crate::Error;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// One record: the page it came from and a value per recipe field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Row {
    pub page_url: String,
    pub fields: BTreeMap<String, String>,
}

impl Row {
    /// Stable JSON (sorted field names) used for record keys and change detection.
    pub fn canonical(&self) -> String {
        serde_json::to_string(self).expect("row serialises")
    }
    pub fn is_empty(&self) -> bool {
        self.fields.values().all(String::is_empty)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmptyField {
    pub name: String,
    pub selector: String,
}

/// What the in-page extractor returns, as JSON.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExtractOutput {
    pub page_url: String,
    pub rows: Vec<Row>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub empty_fields: Vec<EmptyField>,
}

/// Extractor output checked against the recipe that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extraction {
    pub page_url: String,
    pub rows: Vec<Row>,
    pub warnings: Vec<String>,
    pub empty_fields: Vec<EmptyField>,
}

impl Extraction {
    /// Change detection compares these between pages.
    pub fn signature(&self) -> Vec<String> {
        self.rows.iter().map(|r| serde_json::to_string(&r.fields).expect("fields serialise")).collect()
    }
}

impl ExtractOutput {
    pub fn from_json(json: &str, recipe: &Recipe) -> Result<Extraction, Error> {
        if json.len() > RUN_BYTES_MAX * 2 {
            return Err(Error::invalid("Extraction result is larger than 4 MB. Narrow the selection."));
        }
        let output: ExtractOutput = serde_json::from_str(json).map_err(|e| Error::invalid(format!("Extraction result is malformed: {e}")))?;
        output.validate(recipe)
    }

    pub fn validate(self, recipe: &Recipe) -> Result<Extraction, Error> {
        let names: BTreeSet<&str> = recipe.fields.iter().map(|f| f.name.as_str()).collect();
        if self.rows.len() > RUN_ROWS_MAX {
            return Err(Error::invalid("More than 5000 records. Select a smaller page section."));
        }
        check_url(&self.page_url)?;
        let mut bytes = 0;
        for row in &self.rows {
            check_url(&row.page_url)?;
            if row.fields.len() != names.len() || !row.fields.keys().all(|k| names.contains(k.as_str())) {
                return Err(Error::invalid("Extraction result does not match the recipe's fields"));
            }
            for value in row.fields.values() {
                if value.len() > VALUE_BYTES_MAX {
                    return Err(Error::invalid("A value is larger than 100 KB. Pick a narrower element."));
                }
                bytes += value.len();
            }
        }
        if bytes > RUN_BYTES_MAX {
            return Err(Error::invalid("Extraction exceeds 2 MB. Narrow the selection."));
        }
        if self.warnings.len() > EXTRACT_WARNINGS_MAX || self.warnings.iter().any(|w| w.len() > WARNING_BYTES_MAX) {
            return Err(Error::invalid("Extraction returned too many or too long warnings"));
        }
        if self.empty_fields.iter().any(|f| !names.contains(f.name.as_str())) {
            return Err(Error::invalid("Extraction reported an unknown field"));
        }
        Ok(Extraction { page_url: self.page_url, rows: self.rows, warnings: self.warnings, empty_fields: self.empty_fields })
    }
}

fn check_url(url: &str) -> Result<(), Error> {
    if url.len() > URL_MAX || url.chars().any(char::is_control) {
        return Err(Error::invalid("Extraction result has an invalid page address"));
    }
    Ok(())
}

/// Record key: equal rows of one recipe share a key, so stored records deduplicate
/// without decrypting. The per-vault salt keeps keys from being guessable.
pub fn record_key(record_salt: &[u8; 32], recipe_id: &str, row: &Row) -> String {
    let mut h = Sha256::new();
    h.update(record_salt);
    h.update(recipe_id.as_bytes());
    h.update([0u8]);
    h.update(row.canonical().as_bytes());
    crate::hex(&h.finalize())
}

/// The plaintext that gets sealed for each record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlainRecord {
    pub observed_at: String,
    pub run_id: String,
    pub page_url: String,
    pub fields: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe() -> Recipe {
        Recipe::from_json(r#"{"name":"t","url":"https://example.org","fields":[{"name":"a","selector":"x"},{"name":"b","selector":"y"}]}"#).unwrap()
    }
    fn output(rows: serde_json::Value) -> String {
        serde_json::json!({ "pageUrl": "https://example.org/", "rows": rows, "warnings": [], "emptyFields": [] }).to_string()
    }

    #[test]
    fn valid_output_passes_and_rows_have_stable_keys() {
        let e = ExtractOutput::from_json(&output(serde_json::json!([{ "pageUrl": "https://example.org/", "fields": { "b": "2", "a": "1" } }])), &recipe()).unwrap();
        assert_eq!(e.rows[0].canonical(), r#"{"pageUrl":"https://example.org/","fields":{"a":"1","b":"2"}}"#);
        let salt = [7u8; 32];
        let k1 = record_key(&salt, "r1", &e.rows[0]);
        assert_eq!(k1, record_key(&salt, "r1", &e.rows[0].clone()));
        assert_ne!(k1, record_key(&salt, "r2", &e.rows[0]));
        assert_ne!(k1, record_key(&[8u8; 32], "r1", &e.rows[0]));
        assert_eq!(e.signature(), vec![r#"{"a":"1","b":"2"}"#.to_string()]);
    }

    #[test]
    fn output_not_matching_the_recipe_is_refused() {
        let r = recipe();
        for rows in [
            serde_json::json!([{ "pageUrl": "u", "fields": { "a": "1" } }]),
            serde_json::json!([{ "pageUrl": "u", "fields": { "a": "1", "b": "2", "c": "3" } }]),
            serde_json::json!([{ "pageUrl": "u", "fields": { "a": "1", "z": "2" } }]),
            serde_json::json!([{ "pageUrl": "u", "fields": { "a": "x".repeat(VALUE_BYTES_MAX + 1), "b": "" } }]),
            serde_json::json!([{ "pageUrl": "u\u{0}", "fields": { "a": "1", "b": "2" } }]),
            serde_json::json!([{ "pageUrl": "u", "fields": { "a": "1", "b": "2" }, "extra": 1 }]),
        ] {
            assert!(ExtractOutput::from_json(&output(rows.clone()), &r).is_err(), "{rows} should be refused");
        }
        let many = serde_json::json!({ "pageUrl": "u", "rows": [], "warnings": vec!["w"; 31], "emptyFields": [] });
        assert!(ExtractOutput::from_json(&many.to_string(), &r).is_err());
        let unknown = serde_json::json!({ "pageUrl": "u", "rows": [], "emptyFields": [{ "name": "nope", "selector": "s" }] });
        assert!(ExtractOutput::from_json(&unknown.to_string(), &r).is_err());
    }
}

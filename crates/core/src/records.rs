//! Extractor output (untrusted: the page controls the DOM it was read from), the
//! canonical row form, and record keys.

use crate::limits::*;
use crate::recipe::Recipe;
use crate::Error;
use serde::{Deserialize, Serialize};
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
            let mut row_bytes = row.page_url.len();
            for value in row.fields.values() {
                if value.len() > VALUE_BYTES_MAX {
                    return Err(Error::invalid("A value is larger than 100 KB. Pick a narrower element."));
                }
                row_bytes += value.len();
            }
            if row_bytes > ROW_BYTES_MAX {
                return Err(Error::invalid("A record is larger than 512 KB. Pick narrower fields."));
            }
            bytes += row_bytes;
        }
        if bytes > RUN_BYTES_MAX {
            return Err(Error::invalid("Extraction exceeds 2 MB. Narrow the selection."));
        }
        if self.warnings.len() > EXTRACT_WARNINGS_MAX || self.warnings.iter().any(|w| w.len() > WARNING_BYTES_MAX) {
            return Err(Error::invalid("Extraction returned too many or too long warnings"));
        }
        let mut reported = BTreeSet::new();
        if self.empty_fields.iter().any(|f| !names.contains(f.name.as_str()) || !reported.insert(f.name.as_str()) || f.selector.len() > SELECTOR_MAX) {
            return Err(Error::invalid("Extraction reported an unknown or repeated field"));
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

/// A new record key: random, so stored keys say nothing about record contents.
/// (Content-derived keys would let anyone holding the profile confirm a guessed row.)
pub fn new_record_key() -> String {
    crate::hex(&crate::random_bytes::<32>())
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

/// Every run stores everything it collected, because duplicates can't be found
/// without decrypting. Once the vault is open, this picks the copies to delete: a
/// row is kept as many times as it appeared in any single run (repeats on one page
/// are real records), choosing the earliest copies.
pub fn duplicates(records: &[(String, PlainRecord)]) -> Vec<String> {
    let canonical = |r: &PlainRecord| Row { page_url: r.page_url.clone(), fields: r.fields.clone() }.canonical();
    let mut per_run: BTreeMap<(String, &str), usize> = BTreeMap::new();
    for (_, r) in records {
        *per_run.entry((canonical(r), r.run_id.as_str())).or_default() += 1;
    }
    let mut keep: BTreeMap<String, usize> = BTreeMap::new();
    for ((row, _), n) in per_run {
        let k = keep.entry(row).or_default();
        *k = (*k).max(n);
    }
    let mut order: Vec<&(String, PlainRecord)> = records.iter().collect();
    order.sort_by(|a, b| a.1.observed_at.cmp(&b.1.observed_at).then_with(|| a.0.cmp(&b.0)));
    let mut out = Vec::new();
    for (key, r) in order {
        let left = keep.get_mut(&canonical(r)).expect("counted above");
        if *left > 0 {
            *left -= 1;
        } else {
            out.push(key.clone());
        }
    }
    out
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
    fn valid_output_passes_and_keys_are_random() {
        let e = ExtractOutput::from_json(&output(serde_json::json!([{ "pageUrl": "https://example.org/", "fields": { "b": "2", "a": "1" } }])), &recipe()).unwrap();
        assert_eq!(e.rows[0].canonical(), r#"{"pageUrl":"https://example.org/","fields":{"a":"1","b":"2"}}"#);
        assert_eq!(e.signature(), vec![r#"{"a":"1","b":"2"}"#.to_string()]);
        let k = new_record_key();
        assert_eq!(k.len(), 64);
        assert_ne!(k, new_record_key());
    }

    #[test]
    fn duplicates_keep_each_row_as_often_as_one_run_saw_it() {
        let rec = |run: &str, at: &str, v: &str| PlainRecord { observed_at: at.into(), run_id: run.into(), page_url: "u".into(), fields: [("a".to_string(), v.to_string())].into() };
        let records = vec![
            ("k1".to_string(), rec("r1", "1", "x")),
            ("k2".to_string(), rec("r1", "1", "x")), // a real repeat within one run
            ("k3".to_string(), rec("r1", "1", "y")),
            ("k4".to_string(), rec("r2", "2", "x")), // seen again next run
            ("k5".to_string(), rec("r2", "2", "x")),
            ("k6".to_string(), rec("r2", "2", "x")), // three times in run 2
            ("k7".to_string(), rec("r2", "2", "z")),
        ];
        let mut gone = duplicates(&records);
        gone.sort();
        // x is kept three times (k1, k2, k4), y and z once.
        assert_eq!(gone, ["k5", "k6"]);
        assert!(duplicates(&[]).is_empty());
    }

    #[test]
    fn oversized_records_and_repeated_health_reports_are_refused() {
        let r = recipe();
        let big = serde_json::json!([{ "pageUrl": "u", "fields": { "a": "x".repeat(VALUE_BYTES_MAX), "b": "y".repeat(VALUE_BYTES_MAX) } }]);
        assert!(ExtractOutput::from_json(&output(big), &r).is_ok(), "200 KB is fine");
        let r30 = Recipe::from_json(&serde_json::json!({ "name": "t", "url": "https://example.org",
            "fields": (0..6).map(|i| serde_json::json!({ "name": format!("f{i}"), "selector": "x" })).collect::<Vec<_>>() }).to_string()).unwrap();
        let fields: serde_json::Map<String, serde_json::Value> = (0..6).map(|i| (format!("f{i}"), serde_json::json!("x".repeat(VALUE_BYTES_MAX)))).collect();
        let huge = serde_json::json!({ "pageUrl": "u", "rows": [{ "pageUrl": "u", "fields": fields }] });
        assert!(ExtractOutput::from_json(&huge.to_string(), &r30).unwrap_err().to_string().contains("512 KB"));
        let twice = serde_json::json!({ "pageUrl": "u", "rows": [], "emptyFields": [{ "name": "a", "selector": "x" }, { "name": "a", "selector": "x" }] });
        assert!(ExtractOutput::from_json(&twice.to_string(), &r).is_err());
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

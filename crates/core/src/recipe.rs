//! Recipes: what to collect and how to move through a site. Input comes from the
//! extension UI, storage or an import and is never trusted; `Recipe::validate` is
//! the only way to get a `Recipe`.

use crate::limits::*;
use crate::url::{self, WebUrl};
use crate::Error;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    #[default]
    Text,
    Href,
    Html,
    Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub name: String,
    pub selector: String,
    #[serde(default)]
    pub kind: FieldKind,
}

/// A click before extraction (cookie banners, "show all").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Action {
    pub selector: String,
    #[serde(default = "default_action_wait")]
    pub wait_ms: u32,
}
fn default_action_wait() -> u32 {
    700
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Step {
    Click {
        selector: String,
        #[serde(rename = "waitMs", default = "default_action_wait")]
        wait_ms: u32,
    },
    Wait {
        #[serde(rename = "waitMs")]
        wait_ms: u32,
    },
    Extract,
}

/// An ordered click / wait / extract sequence, repeated `repeat_count` times.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Workflow {
    pub steps: Vec<Step>,
    pub repeat_count: u32,
}

/// Recipe as it arrives from outside. Unknown fields are refused.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RecipeInput {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub url: String,
    pub fields: Vec<Field>,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub row_selector: String,
    #[serde(default)]
    pub next_selector: String,
    #[serde(default)]
    pub max_pages: Option<u32>,
    #[serde(default)]
    pub schedule_minutes: u32,
    #[serde(default)]
    pub workflow: Option<Workflow>,
    /// Run scheduled jobs in the native host's headless browser instead of a tab.
    #[serde(default)]
    pub use_native_host: bool,
}

/// A validated recipe. Serialises to the same shape `RecipeInput` accepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recipe {
    pub id: String,
    pub name: String,
    pub url: String,
    pub fields: Vec<Field>,
    pub actions: Vec<Action>,
    pub row_selector: String,
    pub next_selector: String,
    pub max_pages: u32,
    pub schedule_minutes: u32,
    pub workflow: Option<Workflow>,
    pub use_native_host: bool,
    /// Derived, not accepted from input.
    #[serde(skip)]
    pub origin: String,
}

impl Recipe {
    pub fn from_json(json: &str) -> Result<Recipe, Error> {
        if json.len() > 256 * 1024 {
            return Err(Error::invalid("Recipe is larger than 256 KB"));
        }
        let input: RecipeInput = serde_json::from_str(json).map_err(|e| Error::invalid(format!("Recipe is malformed: {e}")))?;
        Recipe::validate(input)
    }

    pub fn validate(input: RecipeInput) -> Result<Recipe, Error> {
        let id = match input.id {
            Some(id) => {
                if id.is_empty() || id.len() > ID_MAX || !id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
                    return Err(Error::invalid("Recipe ID must be 1–64 lower-case letters, digits or dashes"));
                }
                id
            }
            None => crate::hex(&crate::random_bytes::<16>()),
        };
        let name = text(&input.name, NAME_MAX, "Recipe name")?;
        let WebUrl { href, origin } = url::parse(&input.url)?;

        if input.fields.is_empty() || input.fields.len() > FIELDS_MAX {
            return Err(Error::invalid(format!("Choose 1–{FIELDS_MAX} fields")));
        }
        let mut names = BTreeSet::new();
        let mut fields = Vec::with_capacity(input.fields.len());
        for f in input.fields {
            let name = text(&f.name, FIELD_NAME_MAX, "Field name")?;
            if !names.insert(name.clone()) {
                return Err(Error::invalid(format!("Field names must be distinct: \"{name}\" is used twice")));
            }
            fields.push(Field { name, selector: selector(&f.selector, "Field selector")?, kind: f.kind });
        }

        if input.actions.len() > ACTIONS_MAX {
            return Err(Error::invalid(format!("At most {ACTIONS_MAX} click actions")));
        }
        let actions = input
            .actions
            .into_iter()
            .map(|a| Ok(Action { selector: selector(&a.selector, "Click action")?, wait_ms: a.wait_ms.min(ACTION_WAIT_MAX_MS) }))
            .collect::<Result<Vec<_>, Error>>()?;

        let row_selector = optional_selector(&input.row_selector, "Record container")?;
        let next_selector = optional_selector(&input.next_selector, "Next button")?;
        let max_pages = input.max_pages.unwrap_or(if next_selector.is_empty() { 1 } else { 5 });
        if !(1..=PAGES_MAX).contains(&max_pages) {
            return Err(Error::invalid(format!("Pages must be 1–{PAGES_MAX}")));
        }
        let schedule_minutes = input.schedule_minutes;
        if schedule_minutes != 0 && !(SCHEDULE_MIN_MINUTES..=SCHEDULE_MAX_MINUTES).contains(&schedule_minutes) {
            return Err(Error::invalid(format!("Schedules run every {SCHEDULE_MIN_MINUTES} minutes to 30 days, or are off")));
        }

        let workflow = match input.workflow {
            None => None,
            Some(w) => {
                if w.steps.is_empty() || w.steps.len() > STEPS_MAX {
                    return Err(Error::invalid(format!("Choose 1–{STEPS_MAX} workflow steps")));
                }
                if !(1..=REPEAT_MAX).contains(&w.repeat_count) {
                    return Err(Error::invalid(format!("Repeat count must be 1–{REPEAT_MAX}")));
                }
                if !w.steps.iter().any(|s| matches!(s, Step::Extract)) {
                    return Err(Error::invalid("A workflow needs at least one Extract step"));
                }
                let steps = w
                    .steps
                    .into_iter()
                    .map(|s| match s {
                        Step::Click { selector: sel, wait_ms } => Ok(Step::Click { selector: selector(&sel, "Workflow click")?, wait_ms: wait(wait_ms)? }),
                        Step::Wait { wait_ms } => Ok(Step::Wait { wait_ms: wait(wait_ms)? }),
                        Step::Extract => Ok(Step::Extract),
                    })
                    .collect::<Result<Vec<_>, Error>>()?;
                Some(Workflow { steps, repeat_count: w.repeat_count })
            }
        };

        Ok(Recipe { id, name, url: href, fields, actions, row_selector, next_selector, max_pages, schedule_minutes, workflow, use_native_host: input.use_native_host, origin })
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("recipe serialises")
    }
}

fn wait(ms: u32) -> Result<u32, Error> {
    if ms > STEP_WAIT_MAX_MS {
        return Err(Error::invalid(format!("Step waits must be 0–{STEP_WAIT_MAX_MS} ms")));
    }
    Ok(ms)
}

fn text(value: &str, max: usize, what: &str) -> Result<String, Error> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max {
        return Err(Error::invalid(format!("{what} must be 1–{max} characters")));
    }
    if value.chars().any(char::is_control) {
        return Err(Error::invalid(format!("{what} contains control characters")));
    }
    Ok(value.to_string())
}

/// CSS syntax is checked by the browser when the selector runs; here we bound size
/// and refuse characters no selector needs.
fn selector(value: &str, what: &str) -> Result<String, Error> {
    let value = value.trim();
    if value.is_empty() || value.len() > SELECTOR_MAX {
        return Err(Error::invalid(format!("{what} must be a CSS selector of 1–{SELECTOR_MAX} characters")));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err(Error::invalid(format!("{what} contains control characters")));
    }
    Ok(value.to_string())
}

fn optional_selector(value: &str, what: &str) -> Result<String, Error> {
    if value.trim().is_empty() { Ok(String::new()) } else { selector(value, what) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> serde_json::Value {
        serde_json::json!({ "name": " Jobs ", "url": "https://Example.org/jobs", "fields": [{ "name": "title", "selector": "a.job" }] })
    }
    fn parse(v: serde_json::Value) -> Result<Recipe, Error> {
        Recipe::from_json(&v.to_string())
    }

    #[test]
    fn a_minimal_recipe_gets_defaults_and_an_id() {
        let r = parse(base()).unwrap();
        assert_eq!(r.name, "Jobs");
        assert_eq!(r.origin, "https://example.org");
        assert_eq!(r.id.len(), 32);
        assert_eq!(r.max_pages, 1);
        assert_eq!(r.fields[0].kind, FieldKind::Text);
        assert!(r.workflow.is_none());
        // Round-trips through its own JSON.
        let again = Recipe::from_json(&r.to_json()).unwrap();
        assert_eq!(again, r);
    }

    #[test]
    fn next_selector_defaults_to_five_pages() {
        let mut v = base();
        v["nextSelector"] = "a.next".into();
        assert_eq!(parse(v).unwrap().max_pages, 5);
    }

    #[test]
    fn unknown_fields_and_bad_values_are_refused() {
        let cases: Vec<(&str, serde_json::Value)> = vec![
            ("unknown field", serde_json::json!({ "evil": true })),
            ("file url", serde_json::json!({ "url": "file:///etc/passwd" })),
            ("no fields", serde_json::json!({ "fields": [] })),
            ("duplicate fields", serde_json::json!({ "fields": [{ "name": "a", "selector": "x" }, { "name": "a", "selector": "y" }] })),
            ("empty selector", serde_json::json!({ "fields": [{ "name": "a", "selector": " " }] })),
            ("bad kind", serde_json::json!({ "fields": [{ "name": "a", "selector": "x", "kind": "script" }] })),
            ("bad id", serde_json::json!({ "id": "../../etc" })),
            ("too many pages", serde_json::json!({ "maxPages": 31 })),
            ("zero pages", serde_json::json!({ "maxPages": 0 })),
            ("schedule too often", serde_json::json!({ "scheduleMinutes": 5 })),
            ("empty workflow", serde_json::json!({ "workflow": { "steps": [], "repeatCount": 1 } })),
            ("no extract step", serde_json::json!({ "workflow": { "steps": [{ "type": "wait", "waitMs": 10 }], "repeatCount": 1 } })),
            ("long wait", serde_json::json!({ "workflow": { "steps": [{ "type": "wait", "waitMs": 30001 }, { "type": "extract" }], "repeatCount": 1 } })),
            ("repeat 0", serde_json::json!({ "workflow": { "steps": [{ "type": "extract" }], "repeatCount": 0 } })),
            ("unknown step", serde_json::json!({ "workflow": { "steps": [{ "type": "eval" }], "repeatCount": 1 } })),
            ("control chars", serde_json::json!({ "name": "a\u{7}b" })),
        ];
        for (what, patch) in cases {
            let mut v = base();
            for (k, val) in patch.as_object().unwrap() {
                v[k] = val.clone();
            }
            assert!(parse(v).is_err(), "{what} should be refused");
        }
        assert!(Recipe::from_json(&"x".repeat(300 * 1024)).is_err());
    }

    #[test]
    fn workflows_and_actions_are_kept_in_order_and_bounded() {
        let mut v = base();
        v["actions"] = serde_json::json!([{ "selector": "#accept", "waitMs": 99999 }]);
        v["workflow"] = serde_json::json!({ "steps": [{ "type": "click", "selector": "#next", "waitMs": 0 }, { "type": "wait", "waitMs": 150 }, { "type": "extract" }], "repeatCount": 3 });
        let r = parse(v).unwrap();
        assert_eq!(r.actions[0].wait_ms, ACTION_WAIT_MAX_MS, "action waits are clamped");
        let w = r.workflow.unwrap();
        assert_eq!(w.repeat_count, 3);
        assert_eq!(w.steps, vec![Step::Click { selector: "#next".into(), wait_ms: 0 }, Step::Wait { wait_ms: 150 }, Step::Extract]);
    }
}

//! Drives the Rust planner in headless Chrome. Page scripts (the shared extractor,
//! clicks) run in an isolated world, as they do in the extension, so page scripts
//! can't see or patch them.

use crate::browser::Browser;
use pocket_scraper_core::plan::{Action, Event, Observation, Run, RunOptions};
use pocket_scraper_core::recipe::Recipe;
use pocket_scraper_core::records::{ExtractOutput, Row};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// The same extractor the extension injects into tabs.
const EXTRACTOR: &str = include_str!("../../../extension/src/extract.js");
const NAVIGATION_TIMEOUT: Duration = Duration::from_secs(30);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const PAGE_CHANGE_TIMEOUT_MS: u64 = 20_000;

pub enum Outcome {
    Finished { rows: Vec<Row>, warnings: Vec<String> },
    Failed { message: String, attention: bool },
}

struct Page<'a> {
    browser: &'a mut Browser,
    session: String,
    nav_error: Option<String>,
}

pub fn run(browser: &mut Browser, recipe: Recipe, run_timeout_ms: u64) -> Outcome {
    let started = Instant::now();
    let hard_deadline = started + Duration::from_millis(run_timeout_ms + 5_000);
    let mut page = match Page::open(browser) {
        Ok(p) => p,
        Err(message) => return Outcome::Failed { message, attention: false },
    };
    let fields = json!({ "fields": recipe.fields, "rowSelector": recipe.row_selector });
    let mut planner = Run::new(recipe, RunOptions { run_timeout_ms, page_change_timeout_ms: PAGE_CHANGE_TIMEOUT_MS });
    let now = || started.elapsed().as_millis() as u64;
    let mut action = planner.start(now());
    loop {
        match action {
            Action::Finish { rows, warnings } => return Outcome::Finished { rows, warnings },
            Action::Fail { message, attention } => return Outcome::Failed { message, attention },
            _ => {}
        }
        if Instant::now() > hard_deadline {
            return Outcome::Failed { message: format!("Run timed out after {}s.", run_timeout_ms / 1000), attention: false };
        }
        let event = page.perform(&action, &fields);
        action = planner.observe(Observation { now_ms: now(), event });
    }
}

impl<'a> Page<'a> {
    fn open(browser: &'a mut Browser) -> Result<Page<'a>, String> {
        let t = Duration::from_secs(10);
        let target = browser.cdp.call(None, "Target.createTarget", json!({ "url": "about:blank" }), t)?;
        let target_id = target["targetId"].as_str().ok_or("The browser did not open a page")?.to_string();
        let attached = browser.cdp.call(None, "Target.attachToTarget", json!({ "targetId": target_id, "flatten": true }), t)?;
        let session = attached["sessionId"].as_str().ok_or("The browser did not attach to the page")?.to_string();
        let mut page = Page { browser, session, nav_error: None };
        page.call("Page.enable", json!({}), t)?;
        Ok(page)
    }

    fn call(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        self.browser.cdp.call(Some(&self.session), method, params, timeout)
    }

    /// Evaluates `expression` in a fresh isolated world of the main frame.
    fn evaluate(&mut self, expression: &str, timeout: Duration) -> Result<Value, String> {
        let tree = self.call("Page.getFrameTree", json!({}), Duration::from_secs(5))?;
        let frame_id = tree["frameTree"]["frame"]["id"].as_str().ok_or("The page has no main frame")?.to_string();
        let world = self.call("Page.createIsolatedWorld", json!({ "frameId": frame_id, "worldName": "pocket-scraper-secure", "grantUniveralAccess": false }), Duration::from_secs(5))?;
        let context = world["executionContextId"].as_i64().ok_or("Could not create an isolated world")?;
        let result = self.call("Runtime.evaluate", json!({ "expression": expression, "contextId": context, "returnByValue": true, "awaitPromise": true }), timeout)?;
        if let Some(ex) = result.get("exceptionDetails") {
            return Err(ex["exception"]["description"].as_str().or(ex["text"].as_str()).unwrap_or("Script failed").to_string());
        }
        Ok(result["result"]["value"].clone())
    }

    fn perform(&mut self, action: &Action, fields: &Value) -> Event {
        match action {
            Action::Navigate { url } => {
                self.nav_error = match self.call("Page.navigate", json!({ "url": url }), NAVIGATION_TIMEOUT) {
                    Ok(r) => r["errorText"].as_str().filter(|e| !e.is_empty()).map(|e| format!("Could not load the page: {e}")),
                    Err(e) => Some(e),
                };
                self.wait_loaded();
                let status = self.evaluate(STATUS_JS, Duration::from_secs(5)).ok().and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                Event::Navigated { status }
            }
            Action::Wait { ms } => {
                std::thread::sleep(Duration::from_millis(*ms as u64));
                Event::Waited {}
            }
            Action::Read => self.read(fields),
            Action::Click { selector, timeout_ms } => {
                let js = format!("({CLICK_JS})({}, {})", json!(selector), timeout_ms);
                match self.evaluate(&js, Duration::from_millis(*timeout_ms as u64 + 5_000)) {
                    Ok(Value::Bool(true)) => Event::Clicked {},
                    Ok(Value::String(message)) => Event::ClickFailed { message },
                    Ok(_) => Event::ClickFailed { message: format!("No clickable element matches {selector}") },
                    Err(message) => Event::ClickFailed { message },
                }
            }
            Action::ClickNext { selector } => match self.evaluate(&format!("({NEXT_JS})({})", json!(selector)), Duration::from_secs(10)) {
                Ok(Value::String(url_before)) => Event::NextClicked { url_before },
                _ => Event::NextUnavailable {},
            },
            Action::Finish { .. } | Action::Fail { .. } => unreachable!("handled by the caller"),
        }
    }

    fn read(&mut self, fields: &Value) -> Event {
        if let Some(error) = self.nav_error.clone() {
            return Event::Read { url: String::new(), status: 0, output: None, error: Some(error), challenge: false };
        }
        let source = EXTRACTOR.replacen("export function extractFields", "function extractFields", 1);
        let js = format!("(() => {{\n{source}\nreturn extractFields({fields});\n}})()");
        match self.evaluate(&js, READ_TIMEOUT) {
            Ok(v) => {
                let url = v["url"].as_str().unwrap_or("").to_string();
                let status = v["status"].as_u64().unwrap_or(0).min(999) as u16;
                let challenge = v["challenge"].as_bool().unwrap_or(false);
                let error = v["error"].as_str().map(str::to_string);
                // The output is untrusted page data; the planner validates it against the recipe.
                let (output, error) = match v.get("output").filter(|o| !o.is_null()) {
                    Some(o) => match serde_json::from_value::<ExtractOutput>(o.clone()) {
                        Ok(out) => (Some(out), error),
                        Err(e) => (None, Some(format!("Extraction result is malformed: {e}"))),
                    },
                    None => (None, error.or(Some("The page could not be read".into()))),
                };
                Event::Read { url, status, output, error, challenge }
            }
            Err(e) => Event::Read { url: String::new(), status: 0, output: None, error: Some(e), challenge: false },
        }
    }

    fn wait_loaded(&mut self) {
        let end = Instant::now() + NAVIGATION_TIMEOUT;
        while Instant::now() < end {
            if let Ok(Value::String(state)) = self.evaluate("document.readyState", Duration::from_secs(2)) {
                if state != "loading" {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

const STATUS_JS: &str = "Number(performance.getEntriesByType('navigation')[0]?.responseStatus) || 0";

const CLICK_JS: &str = r#"async (selector, timeoutMs) => {
  const end = Date.now() + timeoutMs;
  for (;;) {
    let el;
    try { el = document.querySelector(selector); } catch { return `Invalid selector: ${selector}`; }
    if (el instanceof HTMLElement && el.checkVisibility()) { el.click(); return true; }
    if (Date.now() > end) return `No visible element matches ${selector}`;
    await new Promise(r => setTimeout(r, 100));
  }
}"#;

const NEXT_JS: &str = r#"(selector) => {
  let el;
  try { el = document.querySelector(selector); } catch { return null; }
  if (!(el instanceof HTMLElement) || !el.checkVisibility()) return null;
  if (el.disabled || el.getAttribute('aria-disabled') === 'true') return null;
  const before = location.href;
  el.click();
  return before;
}"#;

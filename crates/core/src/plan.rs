//! The run planner: a state machine that decides every step of a run without
//! touching a browser. A driver (a Chrome tab, or headless Chrome in the native host)
//! performs each `Action` and reports what happened as an `Observation`.
//!
//! Rules carried over from Pocket Scraper's review fixes:
//! - After a Next click, wait for the records to change, be non-empty and read the
//!   same twice in a row before taking them (no half-drawn lists).
//! - Take rows relative to the previous page only: appended rows for "Load more",
//!   every row otherwise. Repeated values are real records and are kept.
//! - An error still present when the wait ends fails the run.
//! - Selector-health problems are reported once per run; warnings are capped.

use crate::limits::*;
use crate::recipe::{Recipe, Step};
use crate::records::{EmptyField, ExtractOutput, Extraction, Row};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunOptions {
    pub run_timeout_ms: u64,
    pub page_change_timeout_ms: u64,
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions { run_timeout_ms: RUN_TIMEOUT_DEFAULT_MS, page_change_timeout_ms: PAGE_CHANGE_TIMEOUT_MS }
    }
}

/// What the driver should do next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Action {
    /// Load the recipe's page; report `Navigated` with the document's HTTP status.
    Navigate { url: String },
    /// Click the first match; report `Clicked`, or `ClickFailed` after `timeout_ms`.
    Click { selector: String, timeout_ms: u32 },
    /// Click the first match only if it exists, is visible and enabled and not
    /// `aria-disabled="true"`; report `NextClicked` with the URL before the click,
    /// or `NextUnavailable`.
    ClickNext { selector: String },
    /// Wait, then report `Waited`.
    Wait { ms: u32 },
    /// Run the extractor on the current document; report `Read`.
    Read,
    /// The run succeeded. The driver closes its tab or browser.
    Finish { rows: Vec<Row>, warnings: Vec<String> },
    /// The run failed. `attention` means the website needs the person (sign-in,
    /// verification), so schedules for this site should pause.
    Fail { message: String, attention: bool },
}

/// What happened, and when (milliseconds on any monotonic clock).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Observation {
    pub now_ms: u64,
    pub event: Event,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum Event {
    /// `status` is the main document's HTTP status, or 0 when unknown.
    // Empty braces, not unit variants: serde only refuses unknown fields on structs.
    Navigated { status: u16 },
    Clicked {},
    ClickFailed { message: String },
    NextClicked { url_before: String },
    NextUnavailable {},
    Waited {},
    /// Exactly one of `output` / `error` is set. `challenge` marks a browser
    /// verification page (Cloudflare and similar).
    Read {
        url: String,
        status: u16,
        #[serde(default)]
        output: Option<ExtractOutput>,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        challenge: bool,
    },
}

impl Event {
    fn name(&self) -> &'static str {
        match self {
            Event::Navigated { .. } => "navigated",
            Event::Clicked {} => "clicked",
            Event::ClickFailed { .. } => "clickFailed",
            Event::NextClicked { .. } => "nextClicked",
            Event::NextUnavailable {} => "nextUnavailable",
            Event::Waited {} => "waited",
            Event::Read { .. } => "read",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Phase {
    Navigating,
    Settling,
    Action { index: usize, clicked: bool },
    FirstRead,
    ClickingNext { page: u32 },
    Probing { page: u32, url_before: String, deadline: u64, last: Option<Vec<String>>, last_error: Option<String>, reading: bool },
    Step { cycle: u32, index: usize, stage: StepStage },
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum StepStage {
    Clicking,
    ClickWaiting,
    Waiting,
    Reading,
}

struct Health {
    field: EmptyField,
    count: u32,
    first: String,
}

pub struct Run {
    recipe: Recipe,
    options: RunOptions,
    started_at: u64,
    phase: Phase,
    rows: Vec<Row>,
    bytes: usize,
    warnings: Vec<String>,
    health: BTreeMap<String, Health>,
    extractions: u32,
    visited: BTreeSet<String>,
    previous: Option<Vec<String>>,
}

type Outcome = Result<Action, Action>;

impl Run {
    pub fn new(recipe: Recipe, options: RunOptions) -> Run {
        Run {
            recipe,
            options,
            started_at: 0,
            phase: Phase::Navigating,
            rows: Vec::new(),
            bytes: 0,
            warnings: Vec::new(),
            health: BTreeMap::new(),
            extractions: 0,
            visited: BTreeSet::new(),
            previous: None,
        }
    }

    pub fn recipe(&self) -> &Recipe {
        &self.recipe
    }

    pub fn start(&mut self, now_ms: u64) -> Action {
        self.started_at = now_ms;
        self.phase = Phase::Navigating;
        Action::Navigate { url: self.recipe.url.clone() }
    }

    pub fn observe(&mut self, observation: Observation) -> Action {
        let action = self.step(observation).unwrap_or_else(|failure| failure);
        if matches!(action, Action::Finish { .. } | Action::Fail { .. }) {
            self.phase = Phase::Done;
        }
        action
    }

    fn step(&mut self, Observation { now_ms, event }: Observation) -> Outcome {
        if self.phase == Phase::Done {
            return Err(fail("The run has already finished", false));
        }
        if now_ms.saturating_sub(self.started_at) >= self.options.run_timeout_ms {
            return Err(fail(format!("Run timed out after {}s.", (self.options.run_timeout_ms + 500) / 1000), false));
        }
        let phase = std::mem::replace(&mut self.phase, Phase::Done);
        match (phase, event) {
            (Phase::Navigating, Event::Navigated { status }) => {
                check_status(status, "")?;
                if self.recipe.workflow.is_some() {
                    self.phase = Phase::Step { cycle: 0, index: 0, stage: StepStage::Clicking };
                    return self.next_step(0, 0);
                }
                self.phase = Phase::Settling;
                Ok(Action::Wait { ms: SETTLE_MS })
            }
            (Phase::Settling, Event::Waited {}) => Ok(self.next_action(0)),
            (Phase::Action { index, clicked: false }, Event::Clicked {}) => {
                self.phase = Phase::Action { index, clicked: true };
                Ok(Action::Wait { ms: self.recipe.actions[index].wait_ms })
            }
            (Phase::Action { index, clicked: false }, Event::ClickFailed { message }) => Err(fail(format!("Click action {}: {message}", index + 1), false)),
            (Phase::Action { index, clicked: true }, Event::Waited {}) => Ok(self.next_action(index + 1)),
            (Phase::FirstRead, Event::Read { url, status, output, error, challenge }) => {
                check_status(status, "Page 1: ")?;
                let extraction = self.extraction("Page 1", output, error, challenge)?;
                self.page(0, url, false, extraction)
            }
            (Phase::ClickingNext { .. }, Event::NextUnavailable {}) => self.finish(),
            (Phase::ClickingNext { page }, Event::NextClicked { url_before }) => {
                self.phase = Phase::Probing { page, url_before, deadline: now_ms + self.options.page_change_timeout_ms, last: None, last_error: None, reading: false };
                Ok(Action::Wait { ms: POLL_MS })
            }
            (Phase::Probing { page, url_before, deadline, last, last_error, reading: false }, Event::Waited {}) => {
                self.phase = Phase::Probing { page, url_before, deadline, last, last_error, reading: true };
                Ok(Action::Read)
            }
            (Phase::Probing { page, url_before, deadline, mut last, reading: true, .. }, Event::Read { url, status, output, error, challenge }) => {
                let label = format!("Page {}", page + 2);
                check_status(status, &format!("{label}: "))?;
                let last_error = match (output, error) {
                    (Some(output), _) => {
                        let extraction = output.validate(&self.recipe).map_err(|e| fail(format!("{label}: {e}"), false))?;
                        let current = extraction.signature();
                        let changed = !current.is_empty() && Some(&current) != self.previous.as_ref();
                        if changed && last.as_ref() == Some(&current) {
                            let in_place = url == url_before;
                            return self.page(page + 1, url, in_place, extraction);
                        }
                        last = Some(current);
                        None
                    }
                    (None, error) => {
                        if challenge {
                            return Err(fail(format!("{label}: {}", challenge_message()), true));
                        }
                        // Mid-navigation or mid-render; keep waiting.
                        last = None;
                        Some(error.unwrap_or_else(|| "The page could not be read".into()))
                    }
                };
                if now_ms >= deadline {
                    if let Some(error) = last_error {
                        return Err(fail(format!("{label}: {error}"), false));
                    }
                    self.warnings.push(format!(
                        "Next button did not load new records within {}s; stopping pagination.",
                        (self.options.page_change_timeout_ms + 500) / 1000
                    ));
                    return self.finish();
                }
                self.phase = Phase::Probing { page, url_before, deadline, last, last_error, reading: false };
                Ok(Action::Wait { ms: POLL_MS })
            }
            (Phase::Step { cycle, index, stage }, event) => self.workflow_event(cycle, index, stage, event),
            (phase, event) => Err(fail(format!("Driver reported {} while {}", event.name(), phase_name(&phase)), false)),
        }
    }

    fn next_action(&mut self, index: usize) -> Action {
        match self.recipe.actions.get(index) {
            Some(action) => {
                self.phase = Phase::Action { index, clicked: false };
                Action::Click { selector: action.selector.clone(), timeout_ms: ACTION_TIMEOUT_MS }
            }
            None => {
                self.phase = Phase::FirstRead;
                Action::Read
            }
        }
    }

    /// Takes one page's records and decides whether to go on.
    fn page(&mut self, page: u32, url: String, in_place: bool, extraction: Extraction) -> Outcome {
        if page > 0 && !in_place && self.visited.contains(&url) {
            self.warnings.push("Pagination repeated the same URL; stopping to avoid a loop.".into());
            return self.finish();
        }
        self.visited.insert(url);
        let label = format!("Page {}", page + 1);
        let current = extraction.signature();
        // "Load more" keeps earlier records and appends; take only the new ones.
        let skip = match &self.previous {
            Some(prev) if prev.len() < current.len() && current[..prev.len()] == prev[..] => prev.len(),
            _ => 0,
        };
        self.record(&label, extraction, skip)?;
        self.previous = Some(current);
        if page + 1 >= self.recipe.max_pages || self.recipe.next_selector.is_empty() {
            return self.finish();
        }
        self.phase = Phase::ClickingNext { page };
        Ok(Action::ClickNext { selector: self.recipe.next_selector.clone() })
    }

    fn workflow_event(&mut self, cycle: u32, index: usize, stage: StepStage, event: Event) -> Outcome {
        let steps = &self.recipe.workflow.as_ref().expect("workflow phase").steps;
        let label = step_label(cycle, index, &steps[index]);
        match (stage, event) {
            (StepStage::Clicking, Event::Clicked {}) => {
                let Step::Click { wait_ms, .. } = steps[index] else { unreachable!("clicking a non-click step") };
                self.phase = Phase::Step { cycle, index, stage: StepStage::ClickWaiting };
                Ok(Action::Wait { ms: wait_ms })
            }
            (StepStage::Clicking, Event::ClickFailed { message }) => Err(fail(format!("{label}: {message}"), false)),
            (StepStage::ClickWaiting | StepStage::Waiting, Event::Waited {}) => self.next_step(cycle, index + 1),
            (StepStage::Reading, Event::Read { status, output, error, challenge, .. }) => {
                check_status(status, &format!("{label}: "))?;
                let extraction = self.extraction(&label, output, error, challenge)?;
                self.record(&label, extraction, 0)?;
                self.next_step(cycle, index + 1)
            }
            (stage, event) => Err(fail(format!("Driver reported {} while at {label} ({stage:?})", event.name()), false)),
        }
    }

    fn next_step(&mut self, mut cycle: u32, mut index: usize) -> Outcome {
        let workflow = self.recipe.workflow.as_ref().expect("workflow");
        if index >= workflow.steps.len() {
            cycle += 1;
            index = 0;
        }
        if cycle >= workflow.repeat_count {
            return self.finish();
        }
        let (stage, action) = match &workflow.steps[index] {
            Step::Click { selector, .. } => (StepStage::Clicking, Action::Click { selector: selector.clone(), timeout_ms: ACTION_TIMEOUT_MS }),
            Step::Wait { wait_ms } => (StepStage::Waiting, Action::Wait { ms: *wait_ms }),
            Step::Extract => (StepStage::Reading, Action::Read),
        };
        self.phase = Phase::Step { cycle, index, stage };
        Ok(action)
    }

    fn extraction(&self, label: &str, output: Option<ExtractOutput>, error: Option<String>, challenge: bool) -> Result<Extraction, Action> {
        if challenge {
            return Err(fail(format!("{label}: {}", challenge_message()), true));
        }
        match output {
            Some(output) => output.validate(&self.recipe).map_err(|e| fail(format!("{label}: {e}"), false)),
            None => Err(fail(format!("{label}: {}", error.unwrap_or_else(|| "The page could not be read".into())), false)),
        }
    }

    fn record(&mut self, label: &str, extraction: Extraction, skip: usize) -> Result<(), Action> {
        self.extractions += 1;
        for field in extraction.empty_fields {
            let entry = self.health.entry(field.name.clone()).or_insert_with(|| Health { field, count: 0, first: label.to_string() });
            entry.count += 1;
        }
        self.warnings.extend(extraction.warnings.into_iter().map(|w| format!("{label}: {w}")));
        let batch: Vec<Row> = extraction.rows.into_iter().skip(skip).collect();
        if batch.is_empty() {
            self.warnings.push(format!("{label}: no nonempty records"));
        }
        for row in batch {
            self.bytes += row.page_url.len() + row.fields.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>();
            self.rows.push(row);
        }
        if self.rows.len() > RUN_ROWS_MAX || self.bytes > RUN_BYTES_MAX {
            return Err(fail("Run exceeds 5000 rows or 2 MB. Narrow the selection.", false));
        }
        Ok(())
    }

    fn finish(&mut self) -> Outcome {
        if self.rows.is_empty() {
            return Err(fail("No records collected. Check selectors, sign-in, and page content.", false));
        }
        let mut warnings: Vec<String> = self
            .health
            .values()
            .map(|h| {
                format!(
                    "Selector health: field \"{}\" ({}) was empty in {} of {} extraction(s), first at {}",
                    h.field.name, h.field.selector, h.count, self.extractions, h.first
                )
            })
            .collect();
        warnings.append(&mut self.warnings);
        if warnings.len() > RUN_WARNINGS_MAX {
            let extra = warnings.len() - RUN_WARNINGS_MAX;
            warnings.truncate(RUN_WARNINGS_MAX);
            warnings.push(format!("…and {extra} more warnings"));
        }
        Ok(Action::Finish { rows: std::mem::take(&mut self.rows), warnings })
    }
}

fn fail(message: impl Into<String>, attention: bool) -> Action {
    Action::Fail { message: message.into(), attention }
}

fn check_status(status: u16, prefix: &str) -> Result<(), Action> {
    if status >= 400 {
        return Err(fail(
            format!("{prefix}Website returned HTTP {status}. Scheduled runs use their own browser session; sign in through session setup if the site needs it."),
            matches!(status, 401 | 403 | 429),
        ));
    }
    Ok(())
}

fn challenge_message() -> &'static str {
    "Website is showing browser security verification instead of the requested content. Complete verification in session setup, then retry."
}

fn step_label(cycle: u32, index: usize, step: &Step) -> String {
    let kind = match step {
        Step::Click { .. } => "click",
        Step::Wait { .. } => "wait",
        Step::Extract => "extract",
    };
    format!("Repeat {}, step {} ({kind})", cycle + 1, index + 1)
}

fn phase_name(phase: &Phase) -> &'static str {
    match phase {
        Phase::Navigating => "navigating",
        Phase::Settling => "settling",
        Phase::Action { .. } => "running click actions",
        Phase::FirstRead => "reading the first page",
        Phase::ClickingNext { .. } => "clicking Next",
        Phase::Probing { .. } => "waiting for the next page",
        Phase::Step { .. } => "running the workflow",
        Phase::Done => "finished",
    }
}

#[cfg(test)]
mod tests;

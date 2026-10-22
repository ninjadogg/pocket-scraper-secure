use super::*;
use crate::records::EmptyField;
use std::collections::VecDeque;

fn recipe(extra: serde_json::Value) -> Recipe {
    let mut v = serde_json::json!({ "name": "t", "url": "https://example.org/list", "fields": [{ "name": "name", "selector": ".name" }] });
    for (k, val) in extra.as_object().unwrap() {
        v[k] = val.clone();
    }
    Recipe::from_json(&v.to_string()).unwrap()
}

/// What the page shows on one read.
#[derive(Clone)]
enum Frame {
    Rows(Vec<&'static str>),
    Error(&'static str),
    Challenge,
}
use Frame::*;

/// A fake website. `pages[i]` is what page i finally shows; `transients` are frames
/// shown on reads right after a Next click, before the final page.
struct Site {
    pages: Vec<Vec<&'static str>>,
    transients: Vec<Frame>,
    page: usize,
    queue: VecDeque<Frame>,
    url_changes: bool,
    /// Next stays clickable on the last page (a "Load more" that adds nothing).
    next_always: bool,
    /// Overrides the URL of a page index (for loop tests).
    url_override: Option<(usize, usize)>,
    status: u16,
    now: u64,
    empty_fields: Vec<EmptyField>,
    warnings_per_read: usize,
    click_fails: bool,
}

impl Site {
    fn new(pages: Vec<Vec<&'static str>>) -> Site {
        Site { pages, transients: vec![], page: 0, queue: VecDeque::new(), url_changes: false, next_always: false, url_override: None, status: 200, now: 0, empty_fields: vec![], warnings_per_read: 0, click_fails: false }
    }
    fn url(&self) -> String {
        let page = match self.url_override {
            Some((from, to)) if self.page == from => to,
            _ => self.page,
        };
        if self.url_changes { format!("https://example.org/list?page={}", page + 1) } else { "https://example.org/list".into() }
    }
    fn frame(&mut self) -> Frame {
        if let Some(f) = self.queue.pop_front() {
            return f;
        }
        Rows(self.pages[self.page].clone())
    }
    fn read(&mut self) -> Event {
        let url = self.url();
        match self.frame() {
            Rows(names) => Event::Read {
                url: url.clone(),
                status: self.status,
                output: Some(ExtractOutput {
                    page_url: url.clone(),
                    rows: names.iter().map(|n| Row { page_url: url.clone(), fields: [("name".to_string(), n.to_string())].into() }).collect(),
                    warnings: (0..self.warnings_per_read).map(|i| format!("Record {}: no matches for name", i + 1)).collect(),
                    empty_fields: self.empty_fields.clone(),
                }),
                error: None,
                challenge: false,
            },
            Error(message) => Event::Read { url, status: self.status, output: None, error: Some(message.into()), challenge: false },
            Challenge => Event::Read { url, status: self.status, output: None, error: Some("verification".into()), challenge: true },
        }
    }
    fn perform(&mut self, action: &Action) -> Event {
        self.now += 10;
        match action {
            Action::Navigate { .. } => Event::Navigated { status: self.status },
            Action::Wait { ms } => {
                self.now += *ms as u64;
                Event::Waited {}
            }
            Action::Read => self.read(),
            Action::Click { .. } if self.click_fails => Event::ClickFailed { message: "No element matches #missing".into() },
            Action::Click { .. } => {
                if self.page + 1 < self.pages.len() {
                    self.page += 1;
                }
                Event::Clicked {}
            }
            Action::ClickNext { .. } => {
                if self.page + 1 >= self.pages.len() && !self.next_always {
                    return Event::NextUnavailable {};
                }
                let url_before = self.url();
                if self.page + 1 < self.pages.len() {
                    self.page += 1;
                }
                self.queue = self.transients.iter().cloned().collect();
                Event::NextClicked { url_before }
            }
            Action::Finish { .. } | Action::Fail { .. } => unreachable!(),
        }
    }
}

fn drive(recipe: Recipe, site: &mut Site, options: RunOptions) -> Action {
    let mut run = Run::new(recipe, options);
    let mut action = run.start(site.now);
    for _ in 0..10_000 {
        if matches!(action, Action::Finish { .. } | Action::Fail { .. }) {
            return action;
        }
        let event = site.perform(&action);
        action = run.observe(Observation { now_ms: site.now, event });
    }
    panic!("run did not finish");
}

fn names(action: &Action) -> Vec<String> {
    match action {
        Action::Finish { rows, .. } => rows.iter().map(|r| r.fields["name"].clone()).collect(),
        other => panic!("expected Finish, got {other:?}"),
    }
}
fn warnings(action: &Action) -> Vec<String> {
    match action {
        Action::Finish { warnings, .. } => warnings.clone(),
        other => panic!("expected Finish, got {other:?}"),
    }
}
fn failure(action: &Action) -> (String, bool) {
    match action {
        Action::Fail { message, attention } => (message.clone(), *attention),
        other => panic!("expected Fail, got {other:?}"),
    }
}
fn fast() -> RunOptions {
    RunOptions { run_timeout_ms: 600_000, page_change_timeout_ms: 2_000 }
}

#[test]
fn a_single_page_is_read_once() {
    let mut site = Site::new(vec![vec!["A", "B"]]);
    let out = drive(recipe(serde_json::json!({})), &mut site, fast());
    assert_eq!(names(&out), ["A", "B"]);
    assert!(warnings(&out).is_empty());
}

#[test]
fn load_more_keeps_only_appended_records_and_stops_when_nothing_new_loads() {
    let mut site = Site::new(vec![vec!["1"], vec!["1", "2"]]);
    site.next_always = true;
    let out = drive(recipe(serde_json::json!({ "nextSelector": "#more", "maxPages": 5 })), &mut site, fast());
    assert_eq!(names(&out), ["1", "2"]);
    assert_eq!(warnings(&out), ["Next button did not load new records within 2s; stopping pagination."]);
}

#[test]
fn records_that_repeat_an_earlier_page_are_kept_and_paging_continues() {
    let mut site = Site::new(vec![vec!["A", "B"], vec!["B", "C"], vec!["D"]]);
    let out = drive(recipe(serde_json::json!({ "nextSelector": "#next" })), &mut site, fast());
    assert_eq!(names(&out), ["A", "B", "B", "C", "D"]);
    assert!(warnings(&out).is_empty());
}

#[test]
fn a_page_made_only_of_earlier_records_is_still_a_new_page() {
    let mut site = Site::new(vec![vec!["A", "B"], vec!["A"]]);
    let out = drive(recipe(serde_json::json!({ "nextSelector": "#next", "maxPages": 2 })), &mut site, fast());
    assert_eq!(names(&out), ["A", "B", "A"]);
}

#[test]
fn an_emptied_or_half_drawn_list_is_waited_out() {
    let mut site = Site::new(vec![vec!["A", "B"], vec!["C", "D"]]);
    site.transients = vec![Rows(vec![]), Rows(vec!["C"]), Error("No record containers matched")];
    let out = drive(recipe(serde_json::json!({ "nextSelector": "#next" })), &mut site, fast());
    assert_eq!(names(&out), ["A", "B", "C", "D"]);
    assert!(warnings(&out).is_empty());
}

#[test]
fn an_error_that_lasts_past_the_wait_fails_the_run() {
    let mut site = Site::new(vec![vec!["A"], vec!["B"]]);
    site.transients = vec![Error("No record containers matched"); 100];
    let out = drive(recipe(serde_json::json!({ "nextSelector": "#next" })), &mut site, fast());
    assert_eq!(failure(&out), ("Page 2: No record containers matched".into(), false));
}

#[test]
fn verification_while_paging_needs_attention() {
    let mut site = Site::new(vec![vec!["A"], vec!["B"]]);
    site.transients = vec![Challenge];
    let (message, attention) = failure(&drive(recipe(serde_json::json!({ "nextSelector": "#next" })), &mut site, fast()));
    assert!(message.starts_with("Page 2: Website is showing browser security verification"));
    assert!(attention);
}

#[test]
fn http_errors_fail_and_access_errors_need_attention() {
    for (status, attention) in [(403, true), (401, true), (429, true), (500, false), (404, false)] {
        let mut site = Site::new(vec![vec!["A"]]);
        site.status = status;
        let (message, needs) = failure(&drive(recipe(serde_json::json!({})), &mut site, fast()));
        assert!(message.contains(&format!("HTTP {status}")));
        assert_eq!(needs, attention, "{status}");
    }
}

#[test]
fn a_url_pager_that_returns_to_an_earlier_page_stops() {
    let mut site = Site::new(vec![vec!["A"], vec!["B"], vec!["C"]]);
    site.url_changes = true;
    site.url_override = Some((2, 0));
    let out = drive(recipe(serde_json::json!({ "nextSelector": "#next" })), &mut site, fast());
    assert_eq!(names(&out), ["A", "B"]);
    assert_eq!(warnings(&out), ["Pagination repeated the same URL; stopping to avoid a loop."]);
}

#[test]
fn workflows_replay_steps_and_extract_each_state() {
    let mut site = Site::new(vec![vec!["0"], vec!["1"], vec!["2"], vec!["3"]]);
    let r = recipe(serde_json::json!({ "workflow": { "steps": [{ "type": "click", "selector": "#next", "waitMs": 0 }, { "type": "wait", "waitMs": 150 }, { "type": "extract" }], "repeatCount": 3 } }));
    let out = drive(r, &mut site, fast());
    assert_eq!(names(&out), ["1", "2", "3"]);
}

#[test]
fn a_failed_click_names_the_step() {
    let mut site = Site::new(vec![vec!["0"], vec!["1"]]);
    site.click_fails = true;
    let r = recipe(serde_json::json!({ "workflow": { "steps": [{ "type": "extract" }, { "type": "click", "selector": "#missing" }, { "type": "extract" }], "repeatCount": 1 } }));
    assert_eq!(failure(&drive(r, &mut site, fast())).0, "Repeat 1, step 2 (click): No element matches #missing");
    let mut site = Site::new(vec![vec!["0"]]);
    site.click_fails = true;
    let r = recipe(serde_json::json!({ "actions": [{ "selector": "#missing" }] }));
    assert_eq!(failure(&drive(r, &mut site, fast())).0, "Click action 1: No element matches #missing");
}

#[test]
fn the_run_deadline_fails_the_run() {
    let mut site = Site::new(vec![vec!["A"]]);
    let r = recipe(serde_json::json!({ "workflow": { "steps": [{ "type": "wait", "waitMs": 30000 }, { "type": "extract" }], "repeatCount": 1 } }));
    let out = drive(r, &mut site, RunOptions { run_timeout_ms: 1_000, page_change_timeout_ms: 2_000 });
    assert_eq!(failure(&out), ("Run timed out after 1s.".into(), false));
}

#[test]
fn unexpected_driver_events_fail_safely() {
    let mut run = Run::new(recipe(serde_json::json!({})), fast());
    run.start(0);
    let out = run.observe(Observation { now_ms: 1, event: Event::Clicked {} });
    assert_eq!(failure(&out).0, "Driver reported clicked while navigating");
    let again = run.observe(Observation { now_ms: 2, event: Event::Navigated { status: 200 } });
    assert_eq!(failure(&again).0, "The run has already finished");
}

#[test]
fn nothing_collected_is_a_failure() {
    let mut site = Site::new(vec![vec![]]);
    assert!(failure(&drive(recipe(serde_json::json!({})), &mut site, fast())).0.starts_with("No records collected"));
}

#[test]
fn selector_health_is_reported_once_and_warnings_are_capped() {
    let mut site = Site::new(vec![vec!["a"]]);
    site.empty_fields = vec![EmptyField { name: "name".into(), selector: ".name".into() }];
    site.warnings_per_read = 30;
    let r = recipe(serde_json::json!({ "workflow": { "steps": [{ "type": "extract" }, { "type": "extract" }, { "type": "extract" }, { "type": "extract" }, { "type": "extract" }], "repeatCount": 1 } }));
    let w = warnings(&drive(r, &mut site, fast()));
    assert_eq!(w[0], "Selector health: field \"name\" (.name) was empty in 5 of 5 extraction(s), first at Repeat 1, step 1 (extract)");
    assert_eq!(w.iter().filter(|x| x.starts_with("Selector health")).count(), 1);
    assert_eq!(w.len(), 101);
    assert_eq!(w[100], "…and 51 more warnings");
}

#[test]
fn runs_over_5000_rows_fail() {
    let mut site = Site::new(vec![vec!["x"; 2600], vec!["y"; 2600], vec!["z"; 10]]);
    let out = drive(recipe(serde_json::json!({ "nextSelector": "#next" })), &mut site, fast());
    assert_eq!(failure(&out).0, "Run exceeds 5000 rows or 2 MB. Narrow the selection.");
}

#[test]
fn observations_parse_from_driver_json() {
    let json = r#"{"nowMs":5,"event":{"type":"read","url":"u","status":200,"output":{"pageUrl":"u","rows":[{"pageUrl":"u","fields":{"name":"A"}}]}}}"#;
    let obs: Observation = serde_json::from_str(json).unwrap();
    assert!(matches!(obs.event, Event::Read { .. }));
    assert!(serde_json::from_str::<Observation>(r#"{"nowMs":1,"event":{"type":"waited","extra":1}}"#).is_err());
    let action = serde_json::to_string(&Action::Click { selector: "#a".into(), timeout_ms: 5 }).unwrap();
    assert_eq!(action, r##"{"type":"click","selector":"#a","timeoutMs":5}"##);
}

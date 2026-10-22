//! Spreadsheet-safe CSV. Values a spreadsheet would run as a formula get a leading
//! tab; JSON export keeps original values.

use crate::records::PlainRecord;

/// `field_order` is the recipe's field order; fields it doesn't list follow, sorted.
pub fn to_csv(field_order: &[String], records: &[PlainRecord]) -> String {
    let mut names: Vec<String> = field_order.to_vec();
    for r in records {
        for k in r.fields.keys() {
            if !names.contains(k) {
                names.push(k.clone());
            }
        }
    }
    let mut out = String::new();
    let header: Vec<String> = ["observed_at", "page_url"].iter().map(|s| s.to_string()).chain(names.iter().cloned()).collect();
    push_line(&mut out, header.iter().map(String::as_str));
    for r in records {
        let row = [r.observed_at.as_str(), r.page_url.as_str()]
            .into_iter()
            .chain(names.iter().map(|n| r.fields.get(n).map(String::as_str).unwrap_or("")));
        out.push_str("\r\n");
        push_line(&mut out, row);
    }
    out
}

fn push_line<'a>(out: &mut String, cells: impl Iterator<Item = &'a str>) {
    for (i, cell) in cells.enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        if risky(cell) {
            out.push('\t');
        }
        out.push_str(&cell.replace('"', "\"\""));
        out.push('"');
    }
}

/// Starts (after whitespace or a BOM) with = + - @ or their full-width forms, or
/// starts with tab / CR / LF.
fn risky(cell: &str) -> bool {
    if cell.starts_with(['\t', '\r', '\n']) {
        return true;
    }
    let first = cell.chars().find(|c| !c.is_whitespace() && *c != '\u{FEFF}');
    matches!(first, Some('=' | '+' | '-' | '@' | '＝' | '＋' | '－' | '＠'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn rec(fields: &[(&str, &str)]) -> PlainRecord {
        PlainRecord {
            observed_at: "2026-10-22T12:00:00Z".into(),
            run_id: "r".into(),
            page_url: "https://example.org/".into(),
            fields: fields.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn formulas_are_defused_and_quotes_doubled() {
        let csv = to_csv(&["title".into(), "price".into()], &[rec(&[("title", "=1+1"), ("price", "say \"hi\"")]), rec(&[("title", " \u{FEFF}＠SUM(A1)"), ("price", "-5")])]);
        let lines: Vec<&str> = csv.split("\r\n").collect();
        assert_eq!(lines[0], r#""observed_at","page_url","title","price""#);
        assert_eq!(lines[1], "\"2026-10-22T12:00:00Z\",\"https://example.org/\",\"\t=1+1\",\"say \"\"hi\"\"\"");
        assert!(lines[2].contains("\"\t \u{FEFF}＠SUM(A1)\""));
        assert!(lines[2].ends_with("\"\t-5\""));
    }

    #[test]
    fn plain_values_are_unchanged_and_unknown_fields_follow() {
        let csv = to_csv(&["b".into()], &[rec(&[("a", "x"), ("b", "y")])]);
        assert_eq!(csv, "\"observed_at\",\"page_url\",\"b\",\"a\"\r\n\"2026-10-22T12:00:00Z\",\"https://example.org/\",\"y\",\"x\"");
    }
}

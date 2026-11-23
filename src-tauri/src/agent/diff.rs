//! Line diffs for the chat's diff cards.

use serde_json::{json, Value};
use similar::{ChangeTag, TextDiff};

/// Lines a card carries at most (the rest is counted, not sent).
const MAX_LINES: usize = 1200;
/// Characters of one line a card shows.
const MAX_LINE: usize = 400;

fn cut(s: &str) -> String {
    let s = s.trim_end_matches(['\n', '\r']);
    match s.char_indices().nth(MAX_LINE) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// The changes from `old` to `new` as hunks of lines with three lines of
/// context: `{hunks: [[[kind, oldLine, newLine, text]…]…], added, removed,
/// truncated}`, kind "+" | "-" | " ".
pub fn lines(old: &str, new: &str) -> Value {
    let diff = TextDiff::configure().timeout(std::time::Duration::from_millis(400)).diff_lines(old, new);
    let mut added = 0usize;
    let mut removed = 0usize;
    let mut sent = 0usize;
    let mut truncated = false;
    let mut hunks: Vec<Value> = Vec::new();
    for group in diff.grouped_ops(3) {
        let mut rows: Vec<Value> = Vec::new();
        for op in group {
            for change in diff.iter_changes(&op) {
                let (kind, a, b) = match change.tag() {
                    ChangeTag::Insert => {
                        added += 1;
                        ("+", None, change.new_index().map(|i| i + 1))
                    }
                    ChangeTag::Delete => {
                        removed += 1;
                        ("-", change.old_index().map(|i| i + 1), None)
                    }
                    ChangeTag::Equal => (" ", change.old_index().map(|i| i + 1), change.new_index().map(|i| i + 1)),
                };
                if sent < MAX_LINES {
                    rows.push(json!([kind, a, b, cut(change.value())]));
                    sent += 1;
                } else {
                    truncated = true;
                }
            }
        }
        if !rows.is_empty() {
            hunks.push(json!(rows));
        }
    }
    json!({ "hunks": hunks, "added": added, "removed": removed, "truncated": truncated })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diffs_count_and_number_lines() {
        let d = lines("a\nb\nc\n", "a\nB\nc\nd\n");
        assert_eq!(d["added"], 2);
        assert_eq!(d["removed"], 1);
        let rows = d["hunks"][0].as_array().unwrap();
        assert!(rows.iter().any(|r| r[0] == "-" && r[1] == 2 && r[3] == "b"));
        assert!(rows.iter().any(|r| r[0] == "+" && r[2] == 4 && r[3] == "d"));
        let new_file = lines("", "x\ny\n");
        assert_eq!((new_file["added"].as_u64(), new_file["removed"].as_u64()), (Some(2), Some(0)));
        assert_eq!(lines("same\n", "same\n")["hunks"].as_array().unwrap().len(), 0);
    }
}

//! The file tools: read, write, edit, delete, list, search. Paths are
//! relative to the session's folder (absolute ones work too; the permission
//! rules judge them before they get here).

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::{diff, paths};

/// Folders that can be rebuilt and are skipped by listing, searching and
/// checkpoints.
pub const SKIP_DIRS: [&str; 9] = ["node_modules", "target", ".venv", "venv", "__pycache__", ".next", ".turbo", ".gradle", ".git"];

/// A file read whole at most.
const MAX_READ: u64 = 10 * 1024 * 1024;
/// Lines one read returns by default.
const READ_LINES: usize = 2000;
/// Characters of one line a read returns.
const LINE_CHARS: usize = 2000;
/// What one read returns at most, in characters.
const READ_CHARS: usize = 100_000;
const LIST_MAX: usize = 1000;
const SEARCH_MAX: usize = 200;
/// Files bigger than this aren't searched.
const SEARCH_FILE_MAX: u64 = 5 * 1024 * 1024;
/// Entries a walk visits at most.
const WALK_MAX: usize = 200_000;

/// What a tool did: its answer for the model, whether it failed, and what the
/// card shows (a diff, a count).
pub struct Outcome {
    pub text: String,
    pub error: bool,
    pub detail: Option<Value>,
}

impl Outcome {
    fn fail(text: impl Into<String>) -> Self {
        Self { text: text.into(), error: true, detail: None }
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("")
}

/// A file's text: UTF-8 (a BOM is dropped), UTF-16 with a BOM, else None
/// (binary).
pub fn decode(bytes: &[u8]) -> Option<String> {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return Some(String::from_utf8_lossy(rest).into_owned());
    }
    for (bom, le) in [([0xFF, 0xFE], true), ([0xFE, 0xFF], false)] {
        if let Some(rest) = bytes.strip_prefix(&bom) {
            let units: Vec<u16> = rest.chunks_exact(2).map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) }).collect();
            return Some(String::from_utf16_lossy(&units));
        }
    }
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return None;
    }
    Some(String::from_utf8_lossy(bytes).into_owned())
}

fn read_text(path: &Path) -> Result<Option<String>, String> {
    let meta = std::fs::metadata(path).map_err(|e| io_error(path, e))?;
    if meta.is_dir() {
        return Err(format!("{} is a folder: list it instead", path.display()));
    }
    if meta.len() > MAX_READ {
        return Err(format!("{} is {} MB, too big to read whole", path.display(), meta.len() / (1024 * 1024)));
    }
    let bytes = std::fs::read(path).map_err(|e| io_error(path, e))?;
    Ok(decode(&bytes))
}

fn io_error(path: &Path, e: std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => format!("{} doesn't exist", path.display()),
        std::io::ErrorKind::PermissionDenied => format!("Windows refused access to {}", path.display()),
        _ => format!("{}: {e}", path.display()),
    }
}

fn cut_line(s: &str) -> String {
    match s.char_indices().nth(LINE_CHARS) {
        Some((i, _)) => format!("{}… [line cut]", &s[..i]),
        None => s.to_string(),
    }
}

pub fn read_file(folder: &Path, args: &Value) -> Outcome {
    let path = paths::resolve(folder, str_arg(args, "path"));
    let text = match read_text(&path) {
        Ok(Some(t)) => t,
        Ok(None) => return Outcome::fail(format!("{} is a binary file, not text.", paths::shown(&path, folder))),
        Err(e) => return Outcome::fail(e),
    };
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
    let limit = args.get("limit").and_then(Value::as_u64).map(|l| l as usize).unwrap_or(READ_LINES).clamp(1, 10_000);
    let total = text.lines().count();
    if total == 0 {
        return Outcome { text: format!("{} is empty.", paths::shown(&path, folder)), error: false, detail: Some(json!({ "lines": 0 })) };
    }
    if offset > total {
        return Outcome::fail(format!("{} has {total} lines; offset {offset} is past the end.", paths::shown(&path, folder)));
    }
    let mut out = String::new();
    let mut last = offset - 1;
    for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
        let row = format!("{:>6}\t{}\n", i + 1, cut_line(line));
        if out.len() + row.len() > READ_CHARS {
            break;
        }
        out.push_str(&row);
        last = i + 1;
    }
    if last < total {
        out.push_str(&format!("[{} lines in all; read on with offset {}]\n", total, last + 1));
    }
    Outcome { text: out, error: false, detail: Some(json!({ "from": offset, "to": last, "lines": total })) }
}

/// The file's line ending, to keep it when the model writes LF.
fn uses_crlf(text: &str) -> bool {
    text.contains("\r\n")
}

fn with_endings(text: &str, crlf: bool) -> String {
    let lf = text.replace("\r\n", "\n");
    if crlf {
        lf.replace('\n', "\r\n")
    } else {
        lf
    }
}

/// The file as it is now (empty when it doesn't exist) for a diff.
fn current(path: &Path) -> Result<Option<String>, String> {
    if !path.exists() {
        return Ok(None);
    }
    match read_text(path)? {
        Some(t) => Ok(Some(t)),
        None => Err(format!("{} is a binary file: it can't be written as text", path.display())),
    }
}

/// What write_file would change: `(new text, diff)`.
fn plan_write(path: &Path, args: &Value) -> Result<(String, Value), String> {
    let content = args.get("content").and_then(Value::as_str).ok_or("Give the file's content")?;
    let old = current(path)?;
    let crlf = old.as_deref().is_some_and(uses_crlf);
    let new = with_endings(content, crlf);
    let d = diff::lines(&old.unwrap_or_default().replace("\r\n", "\n"), &new.replace("\r\n", "\n"));
    Ok((new, d))
}

/// What edit_file would change: `(new text, diff, replacements)`.
fn plan_edit(path: &Path, args: &Value) -> Result<(String, Value, usize), String> {
    let old_text = str_arg(args, "old_text").replace("\r\n", "\n");
    let new_text = str_arg(args, "new_text").replace("\r\n", "\n");
    let all = args.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
    if old_text.is_empty() {
        return Err("old_text is empty: give the exact text to replace (use write_file for a new file)".into());
    }
    if old_text == new_text {
        return Err("old_text and new_text are the same: nothing would change".into());
    }
    let Some(text) = current(path)? else { return Err(format!("{} doesn't exist (use write_file to create it)", path.display())) };
    let crlf = uses_crlf(&text);
    let lf = text.replace("\r\n", "\n");
    let count = lf.matches(&old_text).count();
    if count == 0 {
        return Err(format!(
            "old_text wasn't found in {}. Read the file again and copy the text exactly (spaces and indentation included).",
            path.display()
        ));
    }
    if count > 1 && !all {
        return Err(format!("old_text appears {count} times in {}: add lines around it so it matches once, or set replace_all.", path.display()));
    }
    let new_lf = if all { lf.replace(&old_text, &new_text) } else { lf.replacen(&old_text, &new_text, 1) };
    let d = diff::lines(&lf, &new_lf);
    Ok((with_endings(&new_lf, crlf), d, if all { count } else { 1 }))
}

/// The diff a write or an edit would make, for its approval card.
pub fn preview(folder: &Path, tool: &str, args: &Value) -> Option<Value> {
    let path = paths::resolve(folder, str_arg(args, "path"));
    let shown = paths::shown(&path, folder);
    match tool {
        "write_file" => plan_write(&path, args).ok().map(|(_, d)| json!({ "path": shown, "diff": d, "created": !path.exists() })),
        "edit_file" => plan_edit(&path, args).ok().map(|(_, d, _)| json!({ "path": shown, "diff": d })),
        "delete_path" => Some(json!({ "path": shown, "folder": path.is_dir(), "files": if path.is_dir() { count_files(&path) } else { 1 } })),
        _ => None,
    }
}

fn write_all(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
    }
    std::fs::write(path, text.as_bytes()).map_err(|e| io_error(path, e))
}

pub fn write_file(folder: &Path, args: &Value) -> Outcome {
    let path = paths::resolve(folder, str_arg(args, "path"));
    if str_arg(args, "path").trim().is_empty() {
        return Outcome::fail("Give the file's path");
    }
    if path.is_dir() {
        return Outcome::fail(format!("{} is a folder", paths::shown(&path, folder)));
    }
    let created = !path.exists();
    let (new, d) = match plan_write(&path, args) {
        Ok(x) => x,
        Err(e) => return Outcome::fail(e),
    };
    if let Err(e) = write_all(&path, &new) {
        return Outcome::fail(e);
    }
    let shown = paths::shown(&path, folder);
    let lines = new.lines().count();
    let text = if created { format!("Created {shown} ({lines} lines).") } else { format!("Wrote {shown} ({lines} lines).") };
    Outcome { text, error: false, detail: Some(json!({ "path": shown, "diff": d, "created": created })) }
}

pub fn edit_file(folder: &Path, args: &Value) -> Outcome {
    let path = paths::resolve(folder, str_arg(args, "path"));
    let (new, d, n) = match plan_edit(&path, args) {
        Ok(x) => x,
        Err(e) => return Outcome::fail(e),
    };
    if let Err(e) = write_all(&path, &new) {
        return Outcome::fail(e);
    }
    let shown = paths::shown(&path, folder);
    let text = if n == 1 { format!("Edited {shown}.") } else { format!("Edited {shown}: {n} replacements.") };
    Outcome { text, error: false, detail: Some(json!({ "path": shown, "diff": d })) }
}

fn count_files(dir: &Path) -> usize {
    let mut n = 0;
    walk(dir, false, &mut |_, is_dir| {
        if !is_dir {
            n += 1;
        }
        true
    });
    n
}

pub fn delete_path(folder: &Path, args: &Value) -> Outcome {
    let raw = str_arg(args, "path");
    if raw.trim().is_empty() {
        return Outcome::fail("Give the path to delete");
    }
    let path = paths::resolve(folder, raw);
    let shown = paths::shown(&path, folder);
    if paths::within(folder, &path) {
        return Outcome::fail("That's the session's folder itself (or a folder holding it): it can't be deleted from here.");
    }
    if path.parent().is_none() {
        return Outcome::fail("That's a whole drive.");
    }
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) => return Outcome::fail(io_error(&path, e)),
    };
    // A link or junction is removed, never what it points to.
    let link = meta.file_type().is_symlink() || is_junction(&path);
    let (res, what) = if meta.is_dir() && !link {
        let n = count_files(&path);
        (std::fs::remove_dir_all(&path), format!("Deleted the folder {shown} ({n} files)."))
    } else if meta.is_dir() {
        (std::fs::remove_dir(&path), format!("Removed the link {shown}."))
    } else {
        (std::fs::remove_file(&path), format!("Deleted {shown}."))
    };
    match res {
        Ok(()) => Outcome { text: what, error: false, detail: Some(json!({ "path": shown })) },
        Err(e) => Outcome::fail(io_error(&path, e)),
    }
}

#[cfg(windows)]
fn is_junction(p: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const REPARSE: u32 = 0x400;
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_attributes() & REPARSE != 0)
}
#[cfg(not(windows))]
fn is_junction(_: &Path) -> bool {
    false
}

/// Visits files and folders under `root` (not into skipped folders, links or
/// junctions), depth first, sorted. `visit` returns false to stop.
pub fn walk(root: &Path, skip: bool, visit: &mut dyn FnMut(&Path, bool) -> bool) {
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<(PathBuf, bool)> = rd
            .filter_map(Result::ok)
            .filter_map(|e| {
                let ft = e.file_type().ok()?;
                let p = e.path();
                let is_dir = ft.is_dir() && !ft.is_symlink() && !is_junction(&p);
                Some((p, is_dir))
            })
            .collect();
        entries.sort_by(|a, b| b.0.file_name().cmp(&a.0.file_name()));
        for (p, is_dir) in entries.into_iter().rev() {
            seen += 1;
            if seen > WALK_MAX {
                return;
            }
            let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            if is_dir && skip && SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            if !visit(&p, is_dir) {
                return;
            }
            if is_dir {
                stack.push(p);
            }
        }
    }
}

/// A glob as a regex: `**` crosses folders, `*` and `?` don't, `{a,b}` picks.
/// A glob without a slash matches a file's name anywhere.
pub fn glob(pattern: &str) -> Option<regex::Regex> {
    let pattern = pattern.trim().replace('\\', "/");
    if pattern.is_empty() {
        return None;
    }
    let anywhere = !pattern.contains('/');
    let mut re = String::from("(?i)^");
    if anywhere {
        re.push_str("(?:.*/)?");
    }
    let chars: Vec<char> = pattern.trim_start_matches("./").chars().collect();
    let mut i = 0;
    let mut depth = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '*' if chars.get(i + 1) == Some(&'*') => {
                if chars.get(i + 2) == Some(&'/') {
                    re.push_str("(?:.*/)?");
                    i += 3;
                    continue;
                }
                re.push_str(".*");
                i += 2;
                continue;
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            '{' => {
                depth += 1;
                re.push_str("(?:");
            }
            '}' if depth > 0 => {
                depth -= 1;
                re.push(')');
            }
            ',' if depth > 0 => re.push('|'),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    re.push('$');
    regex::Regex::new(&re).ok()
}

fn rel(p: &Path, base: &Path) -> String {
    p.strip_prefix(base).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_else(|_| p.to_string_lossy().into_owned())
}

pub fn list(folder: &Path, args: &Value) -> Outcome {
    let raw = str_arg(args, "path");
    let base = if raw.trim().is_empty() { folder.to_path_buf() } else { paths::resolve(folder, raw) };
    if !base.is_dir() {
        return Outcome::fail(format!("{} isn't a folder", paths::shown(&base, folder)));
    }
    let pattern = str_arg(args, "glob");
    let matcher = if pattern.is_empty() { None } else { glob(pattern) };
    if !pattern.is_empty() && matcher.is_none() {
        return Outcome::fail(format!("{pattern} isn't a glob Relay can read"));
    }
    let mut out: Vec<String> = Vec::new();
    let mut more = false;
    walk(&base, true, &mut |p, is_dir| {
        let r = rel(p, &base);
        let keep = match &matcher {
            Some(m) => !is_dir && m.is_match(&r),
            None => true,
        };
        if keep {
            if out.len() >= LIST_MAX {
                more = true;
                return false;
            }
            out.push(if is_dir { format!("{r}/") } else { r });
        }
        true
    });
    out.sort_by_key(|s| s.to_lowercase());
    let head = paths::shown(&base, folder);
    let n = out.len();
    let mut text = if out.is_empty() {
        format!("Nothing{} in {head}.", if pattern.is_empty() { "" } else { " matches" })
    } else {
        format!("{head}:\n{}", out.join("\n"))
    };
    if more {
        text.push_str(&format!("\n[stopped at {LIST_MAX}: narrow it with a path or a glob]"));
    }
    text.push_str(&format!("\n(Skipped: {}.)", SKIP_DIRS.join(", ")));
    Outcome { text, error: false, detail: Some(json!({ "count": n, "more": more })) }
}

pub fn search(folder: &Path, args: &Value) -> Outcome {
    let pattern = str_arg(args, "pattern");
    if pattern.is_empty() {
        return Outcome::fail("Give a pattern to search for");
    }
    let case = if args.get("ignore_case").and_then(Value::as_bool).unwrap_or(false) { "(?i)" } else { "" };
    let re = match regex::RegexBuilder::new(&format!("{case}{pattern}")).size_limit(1 << 22).build() {
        Ok(r) => r,
        Err(e) => return Outcome::fail(format!("That isn't a valid regular expression: {e}")),
    };
    let raw = str_arg(args, "path");
    let base = if raw.trim().is_empty() { folder.to_path_buf() } else { paths::resolve(folder, raw) };
    let glob_pat = str_arg(args, "glob");
    let matcher = if glob_pat.is_empty() { None } else { glob(glob_pat) };
    let mut hits: Vec<String> = Vec::new();
    let mut files_hit = 0usize;
    let mut more = false;
    let mut check = |p: &Path, shown: String| -> bool {
        let Ok(meta) = std::fs::metadata(p) else { return true };
        if meta.len() > SEARCH_FILE_MAX {
            return true;
        }
        let Ok(bytes) = std::fs::read(p) else { return true };
        let Some(text) = decode(&bytes) else { return true };
        let mut any = false;
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                if hits.len() >= SEARCH_MAX {
                    more = true;
                    return false;
                }
                any = true;
                let line = line.trim();
                let line = match line.char_indices().nth(300) {
                    Some((j, _)) => format!("{}…", &line[..j]),
                    None => line.to_string(),
                };
                hits.push(format!("{shown}:{}: {line}", i + 1));
            }
        }
        if any {
            files_hit += 1;
        }
        true
    };
    if base.is_file() {
        check(&base, paths::shown(&base, folder));
    } else if base.is_dir() {
        walk(&base, true, &mut |p, is_dir| {
            if is_dir {
                return true;
            }
            let r = rel(p, &base);
            if matcher.as_ref().is_some_and(|m| !m.is_match(&r)) {
                return true;
            }
            check(p, paths::shown(p, folder))
        });
    } else {
        return Outcome::fail(format!("{} doesn't exist", paths::shown(&base, folder)));
    }
    let n = hits.len();
    let mut text = if hits.is_empty() { "No matches.".to_string() } else { hits.join("\n") };
    if more {
        text.push_str(&format!("\n[stopped at {SEARCH_MAX} matches: narrow the pattern, path or glob]"));
    }
    Outcome { text, error: false, detail: Some(json!({ "matches": n, "files": files_hit, "more": more })) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("relay-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        paths::real(&d)
    }

    #[test]
    fn globs_match_like_shells() {
        let g = glob("*.rs").unwrap();
        assert!(g.is_match("src/main.rs") && g.is_match("main.rs") && !g.is_match("main.rsx"));
        let g = glob("src/**/*.{ts,tsx}").unwrap();
        assert!(g.is_match("src/a.ts") && g.is_match("src/x/y/b.tsx") && !g.is_match("lib/a.ts"));
        let g = glob("docs/?.md").unwrap();
        assert!(g.is_match("docs/a.md") && !g.is_match("docs/ab.md"));
    }

    #[test]
    fn write_edit_read_list_search_delete() {
        let f = tmp("cycle");
        let w = write_file(&f, &json!({ "path": "src/app.txt", "content": "one\ntwo\nthree\n" }));
        assert!(!w.error, "{}", w.text);
        assert_eq!(w.detail.as_ref().unwrap()["diff"]["added"], 3);
        let e = edit_file(&f, &json!({ "path": "src/app.txt", "old_text": "two", "new_text": "TWO" }));
        assert!(!e.error, "{}", e.text);
        assert_eq!(std::fs::read_to_string(f.join("src/app.txt")).unwrap(), "one\nTWO\nthree\n");
        let missing = edit_file(&f, &json!({ "path": "src/app.txt", "old_text": "four", "new_text": "x" }));
        assert!(missing.error);
        let r = read_file(&f, &json!({ "path": "src/app.txt", "offset": 2, "limit": 1 }));
        assert!(r.text.contains("2\tTWO") && r.text.contains("read on with offset 3"), "{}", r.text);
        std::fs::create_dir_all(f.join("node_modules/x")).unwrap();
        std::fs::write(f.join("node_modules/x/i.txt"), "TWO").unwrap();
        let l = list(&f, &json!({}));
        assert!(l.text.contains("src/app.txt") && !l.text.contains("node_modules/x"), "{}", l.text);
        let s = search(&f, &json!({ "pattern": "two", "ignore_case": true }));
        assert!(s.text.contains("src/app.txt:2: TWO") && !s.text.contains("node_modules"), "{}", s.text);
        assert!(delete_path(&f, &json!({ "path": "." })).error);
        assert!(delete_path(&f, &json!({ "path": ".." })).error);
        let d = delete_path(&f, &json!({ "path": "src" }));
        assert!(!d.error && !f.join("src").exists(), "{}", d.text);
        let _ = std::fs::remove_dir_all(&f);
    }

    #[test]
    fn edits_keep_crlf_and_need_one_match() {
        let f = tmp("crlf");
        std::fs::write(f.join("w.txt"), "a\r\nb\r\na\r\n").unwrap();
        assert!(edit_file(&f, &json!({ "path": "w.txt", "old_text": "a", "new_text": "z" })).error);
        let e = edit_file(&f, &json!({ "path": "w.txt", "old_text": "b\na", "new_text": "B\nA" }));
        assert!(!e.error, "{}", e.text);
        assert_eq!(std::fs::read_to_string(f.join("w.txt")).unwrap(), "a\r\nB\r\nA\r\n");
        let all = edit_file(&f, &json!({ "path": "w.txt", "old_text": "A", "new_text": "Q", "replace_all": true }));
        assert!(!all.error);
        let _ = std::fs::remove_dir_all(&f);
    }

    #[test]
    fn text_decoding_spots_binary_and_utf16() {
        assert_eq!(decode(b"\xEF\xBB\xBFhi").as_deref(), Some("hi"));
        assert_eq!(decode(&[0xFF, 0xFE, b'h', 0, b'i', 0]).as_deref(), Some("hi"));
        assert!(decode(&[1, 2, 0, 3]).is_none());
    }
}

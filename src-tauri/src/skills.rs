//! Skills: folders of instructions (and files) a model loads when a task
//! calls for one, in the Agent Skills format. A skill is a folder holding
//! `SKILL.md` (YAML frontmatter with `name` and `description`, then markdown
//! instructions) and whatever else it needs: references, templates, scripts.
//!
//! Models see them a little at a time. The system prompt lists each enabled
//! skill's name and description; `skills__load` returns a skill's
//! instructions and its file list; `skills__read_file` reads one of its files;
//! `skills__run_script` runs one of its scripts (after the user says yes).
//!
//! Skills live in `<data dir>\skills\<id>\`, the folder name being the id.
//! Which are off, and whose scripts may run without asking, is in settings.

use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::Core;

/// The tool route's server id and name for the built-in skill tools.
pub const SERVER_ID: &str = "relay.skills";
pub const SERVER: &str = "Skills";
pub const LOAD: &str = "load";
pub const READ_FILE: &str = "read_file";
pub const RUN_SCRIPT: &str = "run_script";

const MAX_BODY: usize = 200_000;
const MAX_FILE: usize = 100_000;
const MAX_OUTPUT: usize = 24_000;
const MAX_LISTED: usize = 200;
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(300);
/// Script files Relay knows how to run.
const RUNNABLE: [&str; 9] = ["py", "js", "mjs", "cjs", "ps1", "sh", "bat", "cmd", "exe"];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    /// The folder's name.
    pub id: String,
    pub name: String,
    pub description: String,
    /// The instructions (SKILL.md without its frontmatter).
    pub body: String,
    pub enabled: bool,
    /// Its scripts run without asking.
    pub scripts_allowed: bool,
    /// Files besides SKILL.md, relative, with forward slashes.
    pub files: Vec<String>,
    /// The files among them Relay can run.
    pub scripts: Vec<String>,
    pub path: String,
    /// Why the skill can't be used, when it can't.
    pub error: Option<String>,
}

pub fn root(core: &Core) -> PathBuf {
    core.data_dir.join("skills")
}

/// Where scripts run: one folder for everything they write.
pub fn workspace(core: &Core) -> PathBuf {
    core.data_dir.join("workspace")
}

// ---- SKILL.md ---------------------------------------------------------------------

/// One top-level frontmatter key: its value, and its lines as written (kept
/// when Relay rewrites the file, so keys it doesn't know survive).
#[derive(Debug, Clone)]
struct Entry {
    key: String,
    value: String,
    raw: Vec<String>,
}

#[derive(Debug, Default)]
struct Doc {
    entries: Vec<Entry>,
    body: String,
}

impl Doc {
    fn get(&self, key: &str) -> Option<&str> {
        self.entries.iter().find(|e| e.key == key).map(|e| e.value.as_str())
    }
}

/// Splits SKILL.md into its frontmatter entries and the body. A file without
/// frontmatter is all body.
fn parse(text: &str) -> Doc {
    let text = text.trim_start_matches('\u{feff}');
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Doc { entries: Vec::new(), body: text.trim().to_string() };
    }
    let mut fm: Vec<&str> = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        fm.push(line);
    }
    if !closed {
        return Doc { entries: Vec::new(), body: text.trim().to_string() };
    }
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();

    let mut entries: Vec<Entry> = Vec::new();
    let mut i = 0;
    while i < fm.len() {
        let line = fm[i];
        i += 1;
        let top = !line.starts_with([' ', '\t']) && !line.trim().is_empty() && !line.trim_start().starts_with('#');
        let Some((key, rest)) = line.split_once(':').filter(|_| top) else {
            // A stray line: keep it with the entry before it.
            if let Some(last) = entries.last_mut() {
                last.raw.push(line.to_string());
            }
            continue;
        };
        let mut raw = vec![line.to_string()];
        let mut more: Vec<&str> = Vec::new();
        while i < fm.len() && (fm[i].starts_with([' ', '\t']) || fm[i].trim().is_empty()) {
            more.push(fm[i]);
            raw.push(fm[i].to_string());
            i += 1;
        }
        let rest = rest.trim();
        let value = if rest.starts_with('|') || rest.starts_with('>') {
            let kept: Vec<&str> = more.iter().map(|l| l.trim()).collect();
            if rest.starts_with('|') {
                kept.join("\n").trim().to_string()
            } else {
                kept.iter().filter(|l| !l.is_empty()).cloned().collect::<Vec<_>>().join(" ")
            }
        } else {
            let mut all = rest.to_string();
            for l in &more {
                if !l.trim().is_empty() {
                    all.push(' ');
                    all.push_str(l.trim());
                }
            }
            unquote(all.trim())
        };
        entries.push(Entry { key: key.trim().to_string(), value, raw });
    }
    Doc { entries, body }
}

fn unquote(v: &str) -> String {
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        let inner = &v[1..v.len() - 1];
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(o) => out.push(o),
                    None => {}
                }
            } else {
                out.push(c);
            }
        }
        return out;
    }
    if v.len() >= 2 && v.starts_with('\'') && v.ends_with('\'') {
        return v[1..v.len() - 1].replace("''", "'");
    }
    v.to_string()
}

/// A YAML scalar for a one-line value, quoted only when it has to be.
fn scalar(v: &str) -> String {
    let v = v.replace(['\r', '\n'], " ");
    let plain = !v.is_empty()
        && v.trim() == v
        && !v.contains(": ")
        && !v.contains(" #")
        && !v.ends_with(':')
        && !v.starts_with(['-', '?', ':', ',', '[', ']', '{', '}', '#', '&', '*', '!', '|', '>', '\'', '"', '%', '@', '`'])
        && !matches!(v.to_lowercase().as_str(), "true" | "false" | "null" | "yes" | "no" | "~");
    if plain {
        v
    } else {
        format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// SKILL.md with a new name, description and body; other keys stay as written.
fn render(previous: Option<&Doc>, name: &str, description: &str, body: &str) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format!("name: {}\n", scalar(name)));
    out.push_str(&format!("description: {}\n", scalar(description)));
    for e in previous.map(|d| d.entries.as_slice()).unwrap_or_default() {
        if e.key != "name" && e.key != "description" {
            for l in &e.raw {
                out.push_str(l);
                out.push('\n');
            }
        }
    }
    out.push_str("---\n\n");
    out.push_str(body.trim());
    out.push('\n');
    out
}

/// Skill names: lowercase letters, digits and hyphens, as the format asks.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// A folder name from anything: "My PDF tool" → "my-pdf-tool".
fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.trim_matches('-').chars().take(64).collect();
    if out.is_empty() {
        "skill".into()
    } else {
        out
    }
}

fn skill_md(dir: &Path) -> Option<PathBuf> {
    let direct = dir.join("SKILL.md");
    if direct.is_file() {
        return Some(direct);
    }
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_file() && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.eq_ignore_ascii_case("skill.md")))
}

/// The files in a skill folder (not SKILL.md, not hidden ones), relative.
fn files_of(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
        if depth > 5 || out.len() >= MAX_LISTED {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "__pycache__" || name == "node_modules" {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, depth + 1, out);
            } else if let Ok(rel) = p.strip_prefix(base) {
                let rel = rel.to_string_lossy().replace('\\', "/");
                if depth == 0 && rel.eq_ignore_ascii_case("skill.md") {
                    continue;
                }
                out.push(rel);
                if out.len() >= MAX_LISTED {
                    return;
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, 0, &mut out);
    out
}

fn runnable(path: &str) -> bool {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    RUNNABLE.contains(&ext.as_str())
}

fn read_skill(dir: &Path, disabled: &[String], allowed: &[String]) -> Option<Skill> {
    let id = dir.file_name()?.to_str()?.to_string();
    if id.starts_with('.') {
        return None;
    }
    let mut skill = Skill {
        enabled: !disabled.contains(&id),
        scripts_allowed: allowed.contains(&id),
        name: id.clone(),
        id,
        description: String::new(),
        body: String::new(),
        files: Vec::new(),
        scripts: Vec::new(),
        path: dir.to_string_lossy().to_string(),
        error: None,
    };
    let Some(md) = skill_md(dir) else {
        skill.error = Some("This folder has no SKILL.md".into());
        return Some(skill);
    };
    let text = match std::fs::read_to_string(&md) {
        Ok(t) => t,
        Err(e) => {
            skill.error = Some(format!("Couldn't read SKILL.md: {e}"));
            return Some(skill);
        }
    };
    let doc = parse(&text);
    if let Some(n) = doc.get("name").filter(|n| !n.trim().is_empty()) {
        skill.name = n.trim().to_string();
    }
    skill.description = doc.get("description").unwrap_or("").trim().to_string();
    skill.body = doc.body;
    skill.files = files_of(dir);
    skill.scripts = skill.files.iter().filter(|f| runnable(f)).cloned().collect();
    if skill.description.is_empty() {
        skill.error = Some("SKILL.md needs a description, so models know when to use it".into());
    }
    Some(skill)
}

pub fn list(core: &Core) -> Vec<Skill> {
    let s = core.prefs.get();
    let Ok(rd) = std::fs::read_dir(root(core)) else { return Vec::new() };
    let mut out: Vec<Skill> =
        rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).filter_map(|p| read_skill(&p, &s.disabled_skills, &s.skill_scripts)).collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// The skills models may use now.
pub fn usable(core: &Core) -> Vec<Skill> {
    list(core).into_iter().filter(|s| s.enabled && s.error.is_none()).collect()
}

/// The system prompt's part about skills; None without usable skills.
pub fn prompt_block(skills: &[Skill]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let scripts = skills.iter().any(|s| !s.scripts.is_empty());
    let mut out = String::from(
        "You have skills: instructions (and files) for particular kinds of tasks. When a request matches a skill's description, \
         call skills__load with the skill's name before you start, then follow its instructions. Load a skill only when it fits; \
         you don't have to mention skills otherwise.",
    );
    out.push_str(if scripts {
        " A skill's files can be read with skills__read_file, and its scripts run with skills__run_script (the user approves each run)."
    } else {
        " A skill's files can be read with skills__read_file."
    });
    out.push_str("\n\nSkills:");
    for s in skills {
        let d: String = s.description.chars().take(1024).collect();
        out.push_str(&format!("\n- {}: {}", s.name, d));
    }
    Some(out)
}

/// The built-in tools, for the request's `tools`.
pub fn tool_specs(skills: &[Skill]) -> Vec<(String, Value)> {
    let names: Vec<Value> = skills.iter().map(|s| json!(s.name)).collect();
    let mut out = vec![
        (
            LOAD.to_string(),
            json!({
                "type": "object",
                "properties": { "name": { "type": "string", "enum": names, "description": "The skill's name" } },
                "required": ["name"]
            }),
        ),
        (
            READ_FILE.to_string(),
            json!({
                "type": "object",
                "properties": {
                    "skill": { "type": "string", "enum": names, "description": "The skill's name" },
                    "path": { "type": "string", "description": "The file's path inside the skill, as skills__load lists it (e.g. reference/forms.md)" }
                },
                "required": ["skill", "path"]
            }),
        ),
    ];
    if skills.iter().any(|s| !s.scripts.is_empty()) {
        out.push((
            RUN_SCRIPT.to_string(),
            json!({
                "type": "object",
                "properties": {
                    "skill": { "type": "string", "enum": names, "description": "The skill's name" },
                    "path": { "type": "string", "description": "The script's path inside the skill (e.g. scripts/convert.py)" },
                    "args": { "type": "array", "items": { "type": "string" }, "description": "Command-line arguments" },
                    "input": { "type": "string", "description": "Optional text for the script's standard input" }
                },
                "required": ["skill", "path"]
            }),
        ));
    }
    out
}

pub fn tool_description(tool: &str) -> &'static str {
    match tool {
        LOAD => "Loads one of your skills: its instructions and the list of files it comes with. Call it before a task the skill covers.",
        READ_FILE => "Reads a file that comes with a skill (a reference, a template, a script's source).",
        _ => "Runs a script that comes with a skill, with the given arguments, and returns its output. The user approves each run. Scripts run in Relay's workspace folder; the SKILL_DIR environment variable holds the skill's folder.",
    }
}

/// Whether this skill's scripts run without asking.
pub fn scripts_allowed(core: &Core, name: &str) -> bool {
    let skills = usable(core);
    find(&skills, name).is_some_and(|s| s.scripts_allowed)
}

/// A skill's folder name from the name a model used.
pub fn id_of(core: &Core, name: &str) -> Option<String> {
    let skills = usable(core);
    find(&skills, name).map(|s| s.id.clone())
}

fn find<'a>(skills: &'a [Skill], name: &str) -> Option<&'a Skill> {
    let name = name.trim();
    skills.iter().find(|s| s.name == name).or_else(|| skills.iter().find(|s| s.id == name || s.name.eq_ignore_ascii_case(name)))
}

/// A path inside the skill folder, or why not.
fn inside(skill: &Skill, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim().replace('\\', "/");
    let rel = rel.trim_start_matches("./");
    if rel.is_empty() {
        return Err("Give a path inside the skill".into());
    }
    let p = Path::new(rel);
    if p.is_absolute() || p.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err("The path must stay inside the skill's folder".into());
    }
    let base = Path::new(&skill.path).canonicalize().map_err(|e| e.to_string())?;
    let full = base.join(p).canonicalize().map_err(|_| format!("{} has no file {rel}", skill.name))?;
    if !full.starts_with(&base) {
        return Err("The path must stay inside the skill's folder".into());
    }
    Ok(full)
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{cut}\n[… cut: longer than {max} characters]")
}

/// The skill tools' answers: (text, is it an error).
pub async fn call(core: &Core, tool: &str, args: &Value, token: &CancellationToken) -> Result<(String, bool), String> {
    let skills = usable(core);
    let arg = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let name = if tool == LOAD { arg("name") } else { arg("skill") };
    let Some(skill) = find(&skills, &name) else {
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        return Ok((format!("There's no skill called “{name}”. Skills: {}.", names.join(", ")), true));
    };
    match tool {
        LOAD => {
            let mut out = format!("# Skill: {}\n\nFolder: {}\n\n{}", skill.name, skill.path, clip(&skill.body, MAX_BODY));
            if !skill.files.is_empty() {
                out.push_str("\n\n---\nFiles in this skill (read them with skills__read_file");
                if !skill.scripts.is_empty() {
                    out.push_str(", run scripts with skills__run_script");
                }
                out.push_str("):\n");
                for f in &skill.files {
                    out.push_str(&format!("- {f}\n"));
                }
            }
            Ok((out, false))
        }
        READ_FILE => {
            let path = match inside(skill, &arg("path")) {
                Ok(p) => p,
                Err(e) => return Ok((e, true)),
            };
            if path.is_dir() {
                let base = Path::new(&skill.path).canonicalize().unwrap_or_default();
                let files: Vec<String> = files_of(&path)
                    .into_iter()
                    .map(|f| {
                        let full = path.join(&f);
                        full.strip_prefix(&base).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or(f)
                    })
                    .collect();
                return Ok((format!("A folder. Its files:\n{}", files.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n")), false));
            }
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            if bytes.iter().take(8192).any(|&b| b == 0) {
                return Ok((format!("{} is a binary file ({} bytes); it can't be shown as text.", arg("path"), bytes.len()), true));
            }
            Ok((clip(&String::from_utf8_lossy(&bytes), MAX_FILE), false))
        }
        RUN_SCRIPT => {
            let path = match inside(skill, &arg("path")) {
                Ok(p) => p,
                Err(e) => return Ok((e, true)),
            };
            let args: Vec<String> =
                args.get("args").and_then(Value::as_array).map(|a| a.iter().map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).collect()).unwrap_or_default();
            let input = Some(arg("input")).filter(|s| !s.is_empty());
            run(core, skill, &path, &args, input, token).await
        }
        _ => Err(format!("Unknown skill tool {tool}")),
    }
}

/// The program that runs a script, by its extension.
fn interpreter(path: &Path) -> Result<(String, Vec<String>), String> {
    let p = path.to_string_lossy().to_string();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    Ok(match ext.as_str() {
        "py" => ("python".into(), vec![p]),
        "js" | "mjs" | "cjs" => ("node".into(), vec![p]),
        "ps1" => ("powershell.exe".into(), vec!["-NoProfile".into(), "-NonInteractive".into(), "-ExecutionPolicy".into(), "Bypass".into(), "-File".into(), p]),
        "sh" => ("bash".into(), vec![p]),
        "bat" | "cmd" => ("cmd.exe".into(), vec!["/d".into(), "/c".into(), p]),
        "exe" => (p, vec![]),
        _ => return Err(format!("Relay doesn't know how to run .{ext} files. It runs .py, .js, .ps1, .sh, .bat, .cmd and .exe")),
    })
}

async fn run(core: &Core, skill: &Skill, path: &Path, args: &[String], input: Option<String>, token: &CancellationToken) -> Result<(String, bool), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (program, mut pre) = match interpreter(path) {
        Ok(x) => x,
        Err(e) => return Ok((e, true)),
    };
    pre.extend(args.iter().cloned());
    let cwd = workspace(core);
    std::fs::create_dir_all(&cwd).map_err(|e| e.to_string())?;

    let spawn = |program: &str, pre: &[String]| {
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(pre)
            .current_dir(&cwd)
            .env("SKILL_DIR", &skill.path)
            .env("RELAY_WORKSPACE", &cwd)
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        cmd.spawn()
    };
    let started = Instant::now();
    let mut child = match spawn(&program, &pre) {
        Ok(c) => c,
        // No `python` on PATH: the py launcher often is.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && program == "python" => spawn("py", &pre).map_err(|_| {
            "Python isn't installed (or isn't on PATH), so this script can't run. Tell the user they need Python to use it.".to_string()
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((format!("{program} isn't installed (or isn't on PATH), so this script can't run."), true));
        }
        Err(e) => return Err(format!("Couldn't start the script: {e}")),
    };
    #[cfg(windows)]
    if let Some(h) = child.raw_handle() {
        crate::mcp::adopt(h as isize);
    }
    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take().ok_or("No stdout")?;
    let mut stderr = child.stderr.take().ok_or("No stderr")?;
    let feed = async move {
        if let (Some(mut w), Some(text)) = (stdin.take(), input) {
            let _ = w.write_all(text.as_bytes()).await;
        }
        // Dropping stdin closes it: scripts reading it see the end.
    };
    let read_out = async move {
        let mut b = Vec::new();
        let _ = stdout.read_to_end(&mut b).await;
        b
    };
    let read_err = async move {
        let mut b = Vec::new();
        let _ = stderr.read_to_end(&mut b).await;
        b
    };
    let work = async { tokio::join!(feed, read_out, read_err, child.wait()) };
    let outcome = tokio::select! {
        r = work => Some(r),
        _ = token.cancelled() => None,
        _ = tokio::time::sleep(SCRIPT_TIMEOUT) => {
            return Ok((format!("The script ran longer than {} minutes and was stopped.", SCRIPT_TIMEOUT.as_secs() / 60), true));
        }
    };
    let Some((_, out, err, status)) = outcome else { return Err("Stopped".into()) };
    let code = status.ok().and_then(|s| s.code());
    let secs = started.elapsed().as_secs_f64();
    let out = String::from_utf8_lossy(&out).trim_end().to_string();
    let err = String::from_utf8_lossy(&err).trim_end().to_string();
    let mut text = format!(
        "Exit code {} after {secs:.1} s. It ran in {}.",
        code.map(|c| c.to_string()).unwrap_or_else(|| "unknown".into()),
        cwd.to_string_lossy()
    );
    let half = MAX_OUTPUT / 2;
    if !out.is_empty() {
        text.push_str(&format!("\n\nOutput:\n{}", clip(&out, if err.is_empty() { MAX_OUTPUT } else { half })));
    }
    if !err.is_empty() {
        text.push_str(&format!("\n\nErrors:\n{}", clip(&err, half)));
    }
    if out.is_empty() && err.is_empty() {
        text.push_str(" It printed nothing.");
    }
    Ok((text, code != Some(0)))
}

// ---- managing skills --------------------------------------------------------------

/// Creates a skill, or rewrites one (renaming its folder when its name changes).
pub fn save(core: &Core, id: Option<&str>, name: &str, description: &str, body: &str) -> Result<Skill, String> {
    let name = name.trim();
    if !valid_name(name) {
        return Err("A skill's name uses lowercase letters, digits and hyphens (like pdf-forms)".into());
    }
    let description = description.trim();
    if description.is_empty() {
        return Err("Add a description: it's how models know when to use the skill".into());
    }
    if description.chars().count() > 1024 {
        return Err("Keep the description under 1024 characters".into());
    }
    let root = root(core);
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let target = root.join(name);
    let (dir, previous) = match id {
        Some(id) => {
            let old = root.join(id);
            if !old.is_dir() {
                return Err("That skill was removed".into());
            }
            let previous = skill_md(&old).and_then(|p| std::fs::read_to_string(p).ok()).map(|t| parse(&t));
            if id != name {
                if target.exists() {
                    return Err(format!("There's already a skill called {name}"));
                }
                std::fs::rename(&old, &target).map_err(|e| format!("Couldn't rename the skill's folder: {e}"))?;
                rename_in_settings(core, id, name);
            }
            (target, previous)
        }
        None => {
            if target.exists() {
                return Err(format!("There's already a skill called {name}"));
            }
            std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            (target, None)
        }
    };
    // Write to the file that's there (whatever its case), or SKILL.md.
    let md = skill_md(&dir).unwrap_or_else(|| dir.join("SKILL.md"));
    crate::settings::write_atomic(&md, &render(previous.as_ref(), name, description, body)).map_err(|e| format!("Couldn't save SKILL.md: {e}"))?;
    let s = core.prefs.get();
    read_skill(&dir, &s.disabled_skills, &s.skill_scripts).ok_or_else(|| "Couldn't read the skill back".into())
}

fn rename_in_settings(core: &Core, from: &str, to: &str) {
    core.prefs.update(|s| {
        for list in [&mut s.disabled_skills, &mut s.skill_scripts] {
            for v in list.iter_mut() {
                if v == from {
                    *v = to.to_string();
                }
            }
        }
    });
}

pub fn delete(core: &Core, id: &str) -> Result<(), String> {
    if id.is_empty() || id.contains(['/', '\\']) || id == "." || id == ".." {
        return Err("That isn't a skill".into());
    }
    let dir = root(core).join(id);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("Couldn't remove the skill's folder: {e}"))?;
    }
    core.prefs.update(|s| {
        s.disabled_skills.retain(|v| v != id);
        s.skill_scripts.retain(|v| v != id);
    });
    Ok(())
}

pub fn set_flags(core: &Core, id: &str, enabled: Option<bool>, scripts_allowed: Option<bool>) {
    core.prefs.update(|s| {
        if let Some(on) = enabled {
            s.disabled_skills.retain(|v| v != id);
            if !on {
                s.disabled_skills.push(id.to_string());
            }
        }
        if let Some(on) = scripts_allowed {
            s.skill_scripts.retain(|v| v != id);
            if on {
                s.skill_scripts.push(id.to_string());
            }
        }
    });
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Imported {
    pub skill: Skill,
    /// It replaced a skill of the same name.
    pub replaced: bool,
}

/// Adds a skill from a folder, or from a .zip / .skill file. A skill with the
/// same name is replaced (that's how a skill gets updated).
pub fn import(core: &Core, source: &Path) -> Result<Imported, String> {
    let root = root(core);
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let staging = root.join(format!(".import-{}", crate::db::new_id()));
    let result = (|| {
        if source.is_dir() {
            let canon = source.canonicalize().map_err(|e| e.to_string())?;
            if canon.starts_with(root.canonicalize().map_err(|e| e.to_string())?) {
                return Err("That skill is already in Relay".to_string());
            }
            let base = find_skill_dir(source).ok_or("That folder has no SKILL.md")?;
            copy_dir(&base, &staging, &mut 0)?;
        } else {
            unzip(source, &staging)?;
        }
        let md = skill_md(&staging).ok_or("There's no SKILL.md in it")?;
        let doc = parse(&std::fs::read_to_string(&md).map_err(|e| e.to_string())?);
        let fallback = source.file_stem().and_then(|n| n.to_str()).unwrap_or("skill").to_string();
        let name = doc.get("name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&fallback);
        if doc.get("description").is_none_or(|d| d.trim().is_empty()) {
            return Err("Its SKILL.md has no description, so models wouldn't know when to use it".to_string());
        }
        let id = if valid_name(name) { name.to_string() } else { slug(name) };
        let target = root.join(&id);
        let replaced = target.exists();
        if replaced {
            std::fs::remove_dir_all(&target).map_err(|e| format!("Couldn't replace the old {id}: {e}"))?;
        }
        std::fs::rename(&staging, &target).map_err(|e| e.to_string())?;
        let s = core.prefs.get();
        let skill = read_skill(&target, &s.disabled_skills, &s.skill_scripts).ok_or("Couldn't read the skill back")?;
        Ok(Imported { skill, replaced })
    })();
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

/// The folder holding SKILL.md: this one, or the only skill folder inside it.
fn find_skill_dir(dir: &Path) -> Option<PathBuf> {
    if skill_md(dir).is_some() {
        return Some(dir.to_path_buf());
    }
    let subs: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir() && skill_md(p).is_some()).collect();
    (subs.len() == 1).then(|| subs[0].clone())
}

const MAX_FILES: usize = 5000;
const MAX_BYTES: u64 = 200 * 1024 * 1024;

fn copy_dir(from: &Path, to: &Path, count: &mut usize) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(from).map_err(|e| e.to_string())?.flatten() {
        let name = e.file_name();
        if name == ".git" || name == "node_modules" || name == "__pycache__" {
            continue;
        }
        *count += 1;
        if *count > MAX_FILES {
            return Err("That folder holds too many files to be a skill".into());
        }
        let p = e.path();
        let ft = e.file_type().map_err(|e| e.to_string())?;
        if ft.is_dir() {
            copy_dir(&p, &to.join(&name), count)?;
        } else if ft.is_file() {
            std::fs::copy(&p, to.join(&name)).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn unzip(file: &Path, to: &Path) -> Result<(), String> {
    use std::io::Read as _;
    let f = std::fs::File::open(file).map_err(|e| format!("Couldn't open it: {e}"))?;
    let mut zip = zip::ZipArchive::new(f).map_err(|_| "That isn't a skill folder or a .zip file".to_string())?;
    // The skill is where its SKILL.md is: the zip's top, or one folder down.
    let mut prefix: Option<PathBuf> = None;
    for i in 0..zip.len() {
        let Ok(entry) = zip.by_index(i) else { continue };
        let Some(path) = entry.enclosed_name() else { continue };
        if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.eq_ignore_ascii_case("skill.md")) {
            let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
            if prefix.as_ref().is_none_or(|p| parent.components().count() < p.components().count()) {
                prefix = Some(parent);
            }
        }
    }
    let prefix = prefix.ok_or("There's no SKILL.md in that file")?;
    if zip.len() > MAX_FILES {
        return Err("That file holds too many files to be a skill".into());
    }
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let Some(path) = entry.enclosed_name() else { continue };
        let Ok(rel) = path.strip_prefix(&prefix) else { continue };
        if rel.as_os_str().is_empty() || rel.components().any(|c| c.as_os_str() == "__MACOSX") {
            continue;
        }
        let out = to.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        total += entry.size();
        if total > MAX_BYTES {
            return Err("That file is too big to be a skill".into());
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut w = std::fs::File::create(&out).map_err(|e| e.to_string())?;
        std::io::copy(&mut (&mut entry).take(MAX_BYTES), &mut w).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_frontmatter() {
        let d = parse("---\nname: pdf-forms\ndescription: \"Fill PDF forms: text fields, checkboxes\"\nlicense: MIT\nmetadata:\n  author: me\n---\n\n# PDF forms\n\nDo it.\n");
        assert_eq!(d.get("name"), Some("pdf-forms"));
        assert_eq!(d.get("description"), Some("Fill PDF forms: text fields, checkboxes"));
        assert_eq!(d.body, "# PDF forms\n\nDo it.");
        let folded = parse("---\nname: x\ndescription: >\n  One line\n  and more.\n---\nBody");
        assert_eq!(folded.get("description"), Some("One line and more."));
        let plain = parse("---\r\nname: y\r\ndescription: Spans\r\n  two lines\r\n---\r\nB");
        assert_eq!(plain.get("description"), Some("Spans two lines"));
        assert_eq!(parse("No frontmatter").body, "No frontmatter");
    }

    #[test]
    fn rewrites_keep_other_keys() {
        let d = parse("---\nname: old\ndescription: d\nlicense: MIT\nmetadata:\n  author: me\n---\nBody");
        let out = render(Some(&d), "new-name", "Does: things", "New body");
        assert_eq!(out, "---\nname: new-name\ndescription: \"Does: things\"\nlicense: MIT\nmetadata:\n  author: me\n---\n\nNew body\n");
        let again = parse(&out);
        assert_eq!(again.get("description"), Some("Does: things"));
        assert_eq!(again.body, "New body");
    }

    #[test]
    fn names_and_paths() {
        assert!(valid_name("pdf-forms2"));
        assert!(!valid_name("PDF Forms") && !valid_name("-x") && !valid_name(""));
        assert_eq!(slug("My PDF  tool!"), "my-pdf-tool");
        assert!(runnable("scripts/fill.py") && !runnable("reference.md"));
        let dir = std::env::temp_dir().join(format!("relay-skill-{}", crate::db::new_id()));
        std::fs::create_dir_all(dir.join("ref")).unwrap();
        std::fs::write(dir.join("SKILL.md"), "---\nname: t\ndescription: d\n---\nb").unwrap();
        std::fs::write(dir.join("ref/a.md"), "a").unwrap();
        let s = read_skill(&dir, &[], &[]).unwrap();
        assert_eq!(s.files, vec!["ref/a.md".to_string()]);
        assert!(inside(&s, "ref/a.md").is_ok());
        assert!(inside(&s, "../x").is_err());
        assert!(inside(&s, "C:/Windows/win.ini").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prompt_lists_skills() {
        let s = Skill {
            id: "t".into(),
            name: "t".into(),
            description: "Does t".into(),
            body: String::new(),
            enabled: true,
            scripts_allowed: false,
            files: vec![],
            scripts: vec![],
            path: String::new(),
            error: None,
        };
        let p = prompt_block(std::slice::from_ref(&s)).unwrap();
        assert!(p.contains("- t: Does t") && !p.contains("run_script"));
        assert_eq!(tool_specs(&[s]).len(), 2);
        assert!(prompt_block(&[]).is_none());
    }
}

//! Agent sessions: chats with `kind = 'agent'` that work in one folder, picked
//! by the user or a workspace Relay makes (`<data>\workspaces\<session id>`).
//! See docs/design/agent-mode.md.

pub mod checkpoints;
pub mod diff;
pub mod files;
pub mod paths;
pub mod shell;

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

use crate::db::ChatSummary;
use crate::Core;

/// The route server of the agent tools (like `relay.web` and `relay.skills`).
pub const SERVER: &str = "Agent";
pub const SERVER_ID: &str = "relay.agent";

/// Permission modes: Ask every time, Accept edits, Auto, Bypass.
pub const MODES: [&str; 4] = ["ask", "edits", "auto", "bypass"];

/// A known mode, else "ask".
pub fn mode_of(m: Option<&str>) -> &'static str {
    MODES.iter().copied().find(|x| Some(*x) == m).unwrap_or("ask")
}

pub fn mode_name(mode: &str) -> &'static str {
    match mode {
        "edits" => "Accept edits",
        "auto" => "Auto",
        "bypass" => "Bypass",
        _ => "Ask every time",
    }
}

/// A session at work: its folder, and the places the rules know.
#[derive(Clone)]
pub struct Ctx {
    pub chat_id: String,
    pub folder: PathBuf,
    pub credentials: Vec<PathBuf>,
    pub scratch: Vec<PathBuf>,
}

impl Ctx {
    pub fn new(core: &Core, chat_id: &str, folder: &str) -> Self {
        Self {
            chat_id: chat_id.to_string(),
            folder: paths::real(Path::new(folder)),
            credentials: paths::credential_roots(&[core.data_dir.clone()]),
            scratch: paths::scratch_roots(),
        }
    }

    pub fn zone(&self, p: &Path) -> paths::Zone {
        paths::zone(p, &self.folder, &self.credentials, &self.scratch)
    }

    /// The session's mode now (it can change mid-reply).
    pub fn mode(&self, core: &Core) -> &'static str {
        let saved = core.db.chat_summary(&self.chat_id).ok().flatten().and_then(|c| c.mode);
        let fallback = core.prefs.get().agent_mode;
        mode_of(Some(saved.as_deref().unwrap_or(fallback.as_str())))
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("")
}

fn shell_schema() -> Value {
    json!({ "type": "object", "properties": {
        "command": { "type": "string", "description": "The command line to run." },
        "timeout": { "type": "integer", "description": "Seconds before it's stopped (default 120, at most 600)." },
        "background": { "type": "boolean", "description": "Keep it running in the background (a dev server, a watcher) and return at once with its first output." }
    }, "required": ["command"] })
}

/// The agent tools, in a fixed order (the prompt cache). Bash is offered
/// where Git Bash is installed, cmd when Settings turn it on.
pub fn tool_specs(cmd: bool) -> Vec<(&'static str, String, Value)> {
    let mut out: Vec<(&'static str, String, Value)> = file_specs().into_iter().map(|(n, d, s)| (n, d.to_string(), s)).collect();
    let (_, pwsh) = shell::powershell();
    let limits = "It runs in the session's folder, with no window and no input (don't start interactive programs or editors), and stops after 120 s unless you pass a timeout (up to 600). For servers and watchers that keep running, set background: true and look at them with processes.";
    out.push((
        "powershell",
        format!("Runs a command in {} and returns its output and exit code. {limits}", if pwsh { "PowerShell 7 (pwsh)" } else { "Windows PowerShell 5.1" }),
        shell_schema(),
    ));
    if shell::git_bash().is_some() {
        out.push(("bash", format!("Runs a command in Git Bash (bash with the usual Unix tools, on Windows; paths like /c/Users work) and returns its output and exit code. {limits}"), shell_schema()));
    }
    if cmd {
        out.push(("cmd", format!("Runs a command in cmd.exe and returns its output and exit code. {limits}"), shell_schema()));
    }
    out.push((
        "processes",
        "The session's background processes: list them, read one's recent output, or stop one.".to_string(),
        json!({ "type": "object", "properties": {
            "action": { "type": "string", "enum": ["list", "read", "stop"] },
            "id": { "type": "integer", "description": "The process, for read and stop." },
            "lines": { "type": "integer", "description": "How much recent output to read (characters, default 4000)." }
        }, "required": ["action"] }),
    ));
    out
}

fn file_specs() -> Vec<(&'static str, &'static str, Value)> {
    let path = |what: &str| json!({ "type": "string", "description": what });
    vec![
        (
            "read_file",
            "Reads a text file, with line numbers. Long files come in parts: pass offset (the first line, from 1) and limit (how many lines) to read on.",
            json!({ "type": "object", "properties": {
                "path": path("The file, relative to the session's folder (or a full path)."),
                "offset": { "type": "integer", "description": "The first line to read (from 1)." },
                "limit": { "type": "integer", "description": "How many lines (default 2000)." }
            }, "required": ["path"] }),
        ),
        (
            "write_file",
            "Creates a file, or replaces a file's whole content. Folders on the way are made. To change part of an existing file, use edit_file.",
            json!({ "type": "object", "properties": {
                "path": path("The file, relative to the session's folder."),
                "content": { "type": "string", "description": "The file's full new content." }
            }, "required": ["path", "content"] }),
        ),
        (
            "edit_file",
            "Replaces an exact piece of text in a file. old_text must match the file exactly (indentation included) and appear once, unless replace_all is true. Read the file first.",
            json!({ "type": "object", "properties": {
                "path": path("The file, relative to the session's folder."),
                "old_text": { "type": "string", "description": "The exact text to replace." },
                "new_text": { "type": "string", "description": "What goes in its place." },
                "replace_all": { "type": "boolean", "description": "Replace every occurrence (default false)." }
            }, "required": ["path", "old_text", "new_text"] }),
        ),
        (
            "delete_path",
            "Deletes a file, or a folder with everything in it.",
            json!({ "type": "object", "properties": {
                "path": path("The file or folder, relative to the session's folder.")
            }, "required": ["path"] }),
        ),
        (
            "list",
            "Lists files and folders under a path (the session's folder by default), optionally only files matching a glob like **/*.py or src/*.{ts,tsx}. Skips node_modules, target, .venv, venv, __pycache__, .next, .turbo, .gradle and .git.",
            json!({ "type": "object", "properties": {
                "path": path("A folder (default: the session's folder)."),
                "glob": { "type": "string", "description": "Only files matching this glob." }
            } }),
        ),
        (
            "search",
            "Searches files for a regular expression and returns path:line: text for each match. Skips the same folders as list.",
            json!({ "type": "object", "properties": {
                "pattern": { "type": "string", "description": "A regular expression (Rust syntax)." },
                "path": path("A folder or file to search in (default: the session's folder)."),
                "glob": { "type": "string", "description": "Only files matching this glob, like *.rs." },
                "ignore_case": { "type": "boolean" }
            }, "required": ["pattern"] }),
        ),
    ]
}

/// The agent's part of the system prompt. It changes only with the mode.
pub fn prompt(folder: &str, mode: &str, instructions: Option<&str>) -> String {
    let rules = match mode {
        "edits" => "File changes in the folder run right away; commands and anything outside the folder wait for the user's OK.",
        "auto" => "Auto mode: nothing waits for the user. Steps the rules don't allow are refused with a reason: don't try another way to do the same thing, carry on with the rest, and list what was blocked at the end.",
        "bypass" => "Bypass mode: your tool calls run without asking. Be careful: nothing stops a mistake (every change in the folder can still be undone by the user).",
        _ => "Each change and command waits for the user's OK. If they decline, ask what they'd like instead or carry on with something else.",
    };
    let mut out = format!(
        "You're working in the folder {folder} as a coding and writing agent, with tools to read, write and edit files there (and more). \
Work step by step until the task is done: look before you change things (list, search, read_file), make focused changes (edit_file for parts of a file, write_file for new files), \
run commands to build, test and check your work (prefer the file tools over shell commands for reading and editing files). Paths are relative to the folder. Don't claim to have changed a file unless a tool call did it. \
When you're done, say briefly what you did and anything left open.\n\n{rules}"
    );
    if let Some(text) = instructions.filter(|t| !t.trim().is_empty()) {
        out.push_str("\n\nThe folder's own instructions (from its AGENTS.md or CLAUDE.md):\n\n");
        out.push_str(text.trim());
    }
    out
}

/// What the folder's instruction file holds at most.
const MAX_INSTRUCTIONS: usize = 32 * 1024;

/// The folder's AGENTS.md, else its CLAUDE.md (cut at 32 KB, with a note).
pub fn read_instructions(folder: &Path) -> String {
    for name in ["AGENTS.md", "CLAUDE.md"] {
        let p = folder.join(name);
        if let Ok(bytes) = std::fs::read(&p) {
            let Some(text) = files::decode(&bytes) else { continue };
            if text.len() <= MAX_INSTRUCTIONS {
                return text;
            }
            let mut end = MAX_INSTRUCTIONS;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            return format!("{}\n\n[{name} goes on: it was cut at 32 KB. Read the rest with read_file if you need it.]", &text[..end]);
        }
    }
    String::new()
}

/// Why a step is refused, as the model reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub rule: String,
    pub why: String,
    pub instead: String,
}

impl Refusal {
    pub fn text(&self, mode: &str) -> String {
        format!(
            "Blocked by {} mode.\nRule: {}\nWhy: {}\nInstead: {}\nDon't try another way to do the same thing: carry on with the rest of the task.",
            mode_name(mode),
            self.rule,
            self.why,
            self.instead
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Run,
    /// Waits for the user, with a note for the card.
    Ask(Option<String>),
    Refuse(Refusal),
}

/// What a file tool touches, and how.
#[derive(Clone, Copy, PartialEq)]
enum Access {
    Read,
    Write,
    Delete,
}

fn access_of(tool: &str) -> Access {
    match tool {
        "write_file" | "edit_file" => Access::Write,
        "delete_path" => Access::Delete,
        _ => Access::Read,
    }
}

/// Decides a tool call in `mode` (see the spec's table).
pub fn judge(ctx: &Ctx, mode: &str, tool: &str, args: &Value) -> Verdict {
    use paths::Zone;
    if tool == "processes" {
        // Only the session's own background processes.
        return Verdict::Run;
    }
    if shell::Shell::of_tool(tool).is_some() {
        return match mode {
            "bypass" => Verdict::Run,
            "auto" => Verdict::Refuse(Refusal {
                rule: "commands".into(),
                why: "Auto mode's command rules aren't available in this version.".into(),
                instead: "use the file tools.".into(),
            }),
            _ => Verdict::Ask(None),
        };
    }
    let access = access_of(tool);
    let raw = str_arg(args, "path");
    let path = paths::resolve(&ctx.folder, raw);
    let zone = ctx.zone(&path);
    let shown = paths::shown(&path, &ctx.folder);
    if zone == Zone::Credential {
        let refusal = Refusal {
            rule: "reading credentials".into(),
            why: format!("{shown} is where keys, tokens or passwords are kept."),
            instead: "work without it, or ask the user to provide what you need.".into(),
        };
        return match mode {
            "auto" | "bypass" => Verdict::Refuse(refusal),
            _ => Verdict::Ask(Some(format!("{shown} holds keys or passwords."))),
        };
    }
    let outside = zone != Zone::Folder;
    match (mode, access) {
        ("bypass", _) => Verdict::Run,
        (_, Access::Read) if !outside => Verdict::Run,
        ("auto", Access::Read) => Verdict::Run,
        ("auto", Access::Write) if outside && zone != Zone::Scratch => Verdict::Refuse(Refusal {
            rule: "writing outside the folder".into(),
            why: format!("{shown} is outside {}.", ctx.folder.display()),
            instead: "write it inside the folder.".into(),
        }),
        ("auto", Access::Write) => Verdict::Run,
        ("auto", Access::Delete) if zone == Zone::Scratch => Verdict::Run,
        ("auto", Access::Delete) => Verdict::Refuse(Refusal {
            rule: "deleting".into(),
            why: format!("Auto mode doesn't delete files ({shown})."),
            instead: "leave it, and mention in your final message that it can be deleted.".into(),
        }),
        ("edits", Access::Write | Access::Delete) if !outside => Verdict::Run,
        (_, _) if outside => Verdict::Ask(Some(format!("{shown} is outside the session's folder."))),
        _ => Verdict::Ask(None),
    }
}

/// Tools that change things: a checkpoint is taken before each.
pub fn is_step(tool: &str) -> bool {
    matches!(tool, "write_file" | "edit_file" | "delete_path" | "powershell" | "bash" | "cmd")
}

/// Records the folder before a step. Returns the checkpoint's id, or why
/// there's none (the folder is too big, the store can't be written).
pub async fn checkpoint(core: &std::sync::Arc<Core>, ctx: &Ctx, message_id: &str, call_id: &str, label: &str) -> Result<i64, String> {
    let core = core.clone();
    let (chat, folder, m, c, l) = (ctx.chat_id.clone(), ctx.folder.clone(), message_id.to_string(), call_id.to_string(), label.to_string());
    tokio::task::spawn_blocking(move || checkpoints::take(&checkpoints::Store::of(&core), &chat, &folder, &m, &c, &l))
        .await
        .map_err(|e| e.to_string())?
}

/// What a call would change, for its approval card.
pub fn preview(ctx: &Ctx, tool: &str, args: &Value) -> Option<Value> {
    files::preview(&ctx.folder, tool, args)
}

/// Runs a shell tool. `tick` gets the output's end while it runs.
pub async fn run_command(
    ctx: &Ctx,
    which: shell::Shell,
    args: &Value,
    env: &shell::Env,
    token: &tokio_util::sync::CancellationToken,
    mut tick: impl FnMut(Value),
) -> (files::Outcome, bool) {
    let fail = |t: String| (files::Outcome { text: t, error: true, detail: None }, false);
    let command = str_arg(args, "command").trim().to_string();
    if command.is_empty() {
        return fail("Give the command to run".into());
    }
    let timeout = std::time::Duration::from_secs(args.get("timeout").and_then(Value::as_u64).unwrap_or(shell::DEFAULT_TIMEOUT).clamp(1, shell::MAX_TIMEOUT));
    let background = args.get("background").and_then(Value::as_bool).unwrap_or(false);
    let procs = shell::Processes::global();
    if background && procs.running(&ctx.chat_id) >= shell::MAX_BACKGROUND {
        return fail(format!("{} background processes are already running: stop one with processes first.", shell::MAX_BACKGROUND));
    }
    let run = match shell::start(which, &command, &ctx.folder, env) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    if background {
        let (id, out) = procs.adopt(&ctx.chat_id, which, &command, run);
        // A moment for the first lines (a server's address, an early error).
        for _ in 0..15 {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            if out.lock().unwrap().tail(200).lines().count() >= 2 || procs.read(&ctx.chat_id, id, 10).is_some_and(|r| !r.1) {
                break;
            }
        }
        let (text, running, code, _) = procs.read(&ctx.chat_id, id, 4000).unwrap_or_default();
        let state = if running { format!("Started in the background as process {id}.") } else { format!("Process {id} already ended (exit code {}).", code.map(|c| c.to_string()).unwrap_or_else(|| "unknown".into())) };
        let body = if text.trim().is_empty() { " No output yet.".to_string() } else { format!(" Its output so far:\n{}", text.trim_end()) };
        let detail = json!({ "shell": which.name(), "process": id, "output": text });
        return (files::Outcome { text: format!("{state}{body}\nRead more or stop it with processes."), error: !running && code != Some(0), detail: Some(detail) }, false);
    }
    let name = which.name();
    let (f, out, cancelled) = shell::wait(run, timeout, token, |tail| tick(json!({ "shell": name, "output": tail }))).await;
    let out = out.lock().unwrap();
    let (text, error) = shell::report(&f, &out, timeout);
    let detail = json!({ "shell": name, "output": out.tail(16_000), "exit": f.code, "timedOut": f.timed_out });
    (files::Outcome { text, error, detail: Some(detail) }, cancelled)
}

/// The processes tool.
pub fn processes(ctx: &Ctx, args: &Value) -> files::Outcome {
    let procs = shell::Processes::global();
    let id = args.get("id").and_then(Value::as_u64).map(|n| n as u32);
    let ok = |t: String| files::Outcome { text: t, error: false, detail: None };
    let fail = |t: String| files::Outcome { text: t, error: true, detail: None };
    match (str_arg(args, "action"), id) {
        ("list", _) | ("", None) => {
            let list = procs.list(&ctx.chat_id);
            if list.is_empty() {
                return ok("No background processes.".into());
            }
            let rows: Vec<String> = list
                .iter()
                .map(|p| {
                    let state = if p.running { "running".to_string() } else { format!("ended (exit code {})", p.code.map(|c| c.to_string()).unwrap_or_else(|| "unknown".into())) };
                    format!("{} · {} · {state} · {}", p.id, p.shell, p.command)
                })
                .collect();
            ok(rows.join("\n"))
        }
        ("read", Some(id)) => {
            let max = args.get("lines").and_then(Value::as_u64).unwrap_or(4000).clamp(100, 30_000) as usize;
            match procs.read(&ctx.chat_id, id, max) {
                Some((text, running, code, secs)) => {
                    let state = if running { format!("Process {id}, running for {secs:.0} s.") } else { format!("Process {id}, ended (exit code {}).", code.map(|c| c.to_string()).unwrap_or_else(|| "unknown".into())) };
                    ok(if text.trim().is_empty() { format!("{state} No output.") } else { format!("{state} Recent output:\n{}", text.trim_end()) })
                }
                None => fail(format!("There's no process {id} in this session.")),
            }
        }
        ("stop", Some(id)) => {
            if procs.stop(&ctx.chat_id, id) {
                ok(format!("Stopped process {id}."))
            } else {
                fail(format!("There's no process {id} in this session."))
            }
        }
        (_, None) => fail("Give the process's id.".into()),
        (other, _) => fail(format!("Unknown action {other}: use list, read or stop.")),
    }
}

/// Runs an agent tool: its answer, whether it failed, and its card's detail.
pub async fn call(ctx: &Ctx, tool: &str, args: &Value) -> files::Outcome {
    let folder = ctx.folder.clone();
    let tool = tool.to_string();
    let args = args.clone();
    // File work stays off the async threads.
    tokio::task::spawn_blocking(move || match tool.as_str() {
        "read_file" => files::read_file(&folder, &args),
        "write_file" => files::write_file(&folder, &args),
        "edit_file" => files::edit_file(&folder, &args),
        "delete_path" => files::delete_path(&folder, &args),
        "list" => files::list(&folder, &args),
        "search" => files::search(&folder, &args),
        other => files::Outcome { text: format!("There's no agent tool called {other}."), error: true, detail: None },
    })
    .await
    .unwrap_or_else(|e| files::Outcome { text: format!("The tool crashed: {e}"), error: true, detail: None })
}

pub fn workspaces_root(core: &Core) -> PathBuf {
    core.data_dir.join("workspaces")
}

/// A folder picked for a new session must be there.
pub fn check_folder(folder: &str) -> Result<(), String> {
    let path = Path::new(folder);
    if !path.is_absolute() {
        return Err("Pick a folder with a full path".into());
    }
    if !path.is_dir() {
        return Err(format!("{folder} isn't a folder that exists"));
    }
    Ok(())
}

/// Before a session's message goes out: its folder is there. Without a picked
/// folder Relay makes the session's workspace (on the first message, and
/// again if it was deleted since).
pub fn ensure_folder(core: &Core, chat: &ChatSummary) -> Result<(), String> {
    match &chat.folder {
        Some(folder) if !chat.folder_owned => {
            if Path::new(folder).is_dir() {
                Ok(())
            } else {
                Err(format!("This session's folder, {folder}, no longer exists"))
            }
        }
        _ => {
            let path = workspaces_root(core).join(&chat.id);
            std::fs::create_dir_all(&path).map_err(|e| format!("Couldn't make the session's workspace: {e}"))?;
            if chat.folder.is_none() {
                core.db.set_folder(&chat.id, &path.to_string_lossy(), true)?;
            }
            Ok(())
        }
    }
}

/// Deletes a deleted session's workspace: only one Relay made, and only
/// where Relay makes them.
pub fn remove_workspace(core: &Core, chat: &ChatSummary) -> Result<(), String> {
    let Some(folder) = chat.folder.as_deref().filter(|_| chat.folder_owned) else { return Ok(()) };
    let path = Path::new(folder);
    if !path.exists() {
        return Ok(());
    }
    let (Ok(path), Ok(root)) = (path.canonicalize(), workspaces_root(core).canonicalize()) else { return Ok(()) };
    if path.parent() != Some(root.as_path()) || path.file_name().and_then(|n| n.to_str()) != Some(chat.id.as_str()) {
        return Err("That folder isn't a workspace Relay made, so it stays".into());
    }
    std::fs::remove_dir_all(&path).map_err(|e| format!("The session is gone, but its workspace couldn't be deleted: {e}"))
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FolderInfo {
    pub name: String,
    pub exists: bool,
    /// The checked-out branch (or a short commit when detached), in a git repository.
    pub branch: Option<String>,
}

pub fn folder_info(path: &Path) -> FolderInfo {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
    let exists = path.is_dir();
    FolderInfo { name, exists, branch: if exists { git_branch(path) } else { None } }
}

/// Reads `.git/HEAD` in the folder or the nearest one above it (a `.git`
/// file, as in worktrees and submodules, points to the real directory).
pub fn git_branch(folder: &Path) -> Option<String> {
    let mut dir = Some(folder);
    while let Some(d) = dir {
        let dot = d.join(".git");
        let git_dir = if dot.is_dir() {
            Some(dot)
        } else if dot.is_file() {
            let text = read_small(&dot)?;
            let target = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim().to_string();
            let target = PathBuf::from(target);
            Some(if target.is_absolute() { target } else { d.join(target) })
        } else {
            None
        };
        if let Some(git_dir) = git_dir {
            return head_name(&read_small(&git_dir.join("HEAD"))?);
        }
        dir = d.parent();
    }
    None
}

fn head_name(head: &str) -> Option<String> {
    let head = head.trim();
    if let Some(r) = head.strip_prefix("ref:") {
        let r = r.trim();
        return Some(r.strip_prefix("refs/heads/").unwrap_or(r).to_string());
    }
    // Detached: a commit.
    (head.len() >= 7 && head.chars().all(|c| c.is_ascii_hexdigit())).then(|| head[..7].to_string())
}

/// A small text file (HEAD, a `.git` file): at most 4 KB.
fn read_small(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.take(4096).read_to_end(&mut buf).ok()?;
    String::from_utf8(buf).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Ctx {
        let folder = paths::real(&std::env::temp_dir().join("relay-judge-test"));
        Ctx {
            chat_id: "c".into(),
            folder,
            credentials: paths::credential_roots(&[]),
            scratch: paths::scratch_roots(),
        }
    }

    fn verdict(mode: &str, tool: &str, path: &str) -> &'static str {
        match judge(&ctx(), mode, tool, &json!({ "path": path })) {
            Verdict::Run => "run",
            Verdict::Ask(_) => "ask",
            Verdict::Refuse(_) => "refuse",
        }
    }

    #[test]
    fn modes_follow_the_table() {
        let docs = paths::home().unwrap().join("Documents").join("x.txt");
        let docs = docs.to_string_lossy();
        // Reads in the folder always run; outside, they ask in Ask modes.
        for m in MODES {
            assert_eq!(verdict(m, "read_file", "a.txt"), "run", "{m}");
        }
        assert_eq!(verdict("ask", "read_file", &docs), "ask");
        assert_eq!(verdict("auto", "read_file", &docs), "run");
        // Writes.
        assert_eq!(verdict("ask", "write_file", "a.txt"), "ask");
        assert_eq!(verdict("edits", "edit_file", "a.txt"), "run");
        assert_eq!(verdict("edits", "write_file", &docs), "ask");
        assert_eq!(verdict("auto", "write_file", &docs), "refuse");
        assert_eq!(verdict("bypass", "write_file", &docs), "run");
        // Deletes.
        assert_eq!(verdict("ask", "delete_path", "a.txt"), "ask");
        assert_eq!(verdict("edits", "delete_path", "a.txt"), "run");
        assert_eq!(verdict("auto", "delete_path", "a.txt"), "refuse");
        assert_eq!(verdict("bypass", "delete_path", "a.txt"), "run");
        // Credentials: asked in Ask modes, refused in Auto and Bypass.
        assert_eq!(verdict("ask", "read_file", "~/.ssh/id_rsa"), "ask");
        assert_eq!(verdict("auto", "read_file", "~/.ssh/id_rsa"), "refuse");
        assert_eq!(verdict("bypass", "read_file", "~/.aws/credentials"), "refuse");
        assert_eq!(mode_of(Some("weird")), "ask");
    }

    #[test]
    fn branch_names_come_from_head() {
        assert_eq!(head_name("ref: refs/heads/main\n"), Some("main".into()));
        assert_eq!(head_name("ref: refs/heads/feature/x"), Some("feature/x".into()));
        assert_eq!(head_name("0123456789abcdef0123456789abcdef01234567\n"), Some("0123456".into()));
        assert_eq!(head_name("garbage"), None);
    }

    #[test]
    fn branch_is_found_above_and_through_git_files() {
        let tmp = std::env::temp_dir().join(format!("relay-agent-test-{}", std::process::id()));
        let repo = tmp.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git").join("HEAD"), "ref: refs/heads/dev\n").unwrap();
        std::fs::create_dir_all(repo.join("src").join("deep")).unwrap();
        assert_eq!(git_branch(&repo.join("src").join("deep")), Some("dev".into()));

        // A worktree: `.git` is a file pointing elsewhere.
        let real = tmp.join("real-git");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("HEAD"), "ref: refs/heads/wt\n").unwrap();
        let wt = tmp.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", real.display())).unwrap();
        assert_eq!(git_branch(&wt), Some("wt".into()));

        let info = folder_info(&repo);
        assert_eq!(info, FolderInfo { name: "repo".into(), exists: true, branch: Some("dev".into()) });
        assert!(!folder_info(&tmp.join("missing")).exists);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

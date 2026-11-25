//! Auto mode's command rules. A command line is parsed into the commands it
//! runs (PowerShell by its own parser, see `psparse.rs`; Bash and cmd.exe by
//! the tokenizers here), and each command is checked against the rule table:
//! deleting, destructive git, writing outside the folder, reading
//! credentials, uploading, publishing, global installs, system changes and
//! hidden code. Nested shells and scripts the command runs are checked too.
//! A command Relay can't read is refused.
//!
//! Auto guards against accidents, not against a model trying to get around
//! it on purpose (see the spec's known limits).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::paths::{self, Zone};
use super::shell::Shell;
use super::Refusal;

/// One command as parsed: its name (lowercase, no folder, no `.exe`), the
/// name as written, its arguments, the files it writes by redirection and
/// those it reads by redirection.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cmd {
    pub name: String,
    pub raw: String,
    pub args: Vec<String>,
    pub writes: Vec<String>,
    pub reads: Vec<String>,
    /// The command's name is computed (`& $x`): can't be judged.
    pub dynamic: bool,
}

/// A .NET method call in PowerShell (`[IO.File]::Delete(...)`).
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub target: String,
    pub member: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    pub cmds: Vec<Cmd>,
    pub members: Vec<Member>,
}

/// More to check: a nested shell's command line, a script file, or code.
#[derive(Debug, Clone, PartialEq)]
pub enum Next {
    Line(Shell, String),
    Script(Shell, PathBuf),
    Code(Lang, String),
    CodeFile(Lang, PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Lang {
    Python,
    /// Python started with -I, -E or -S: Relay's guard doesn't load.
    PythonBare,
    Node,
}

/// What a check learned besides refusals.
#[derive(Debug, Default)]
pub struct Facts {
    /// A pip install runs: the workspace venv must exist first.
    pub pip_install: bool,
}

/// What the rules know: the folder and its zones, the mode, the folder's settings.
pub struct Rules<'a> {
    pub folder: &'a Path,
    pub credentials: &'a [PathBuf],
    pub scratch: &'a [PathBuf],
    /// Bypass: only the credential rule applies.
    pub bypass: bool,
    /// The folder's "Auto may push and publish".
    pub may_publish: bool,
    /// Relay's Python guard loads in this session's Pythons: their code is
    /// judged as it runs, not read before.
    pub python_guard: bool,
}

fn refuse(rule: &str, why: impl Into<String>, instead: &str) -> Refusal {
    Refusal { rule: rule.into(), why: why.into(), instead: instead.into() }
}

pub fn unreadable(what: &str) -> Refusal {
    refuse("unreadable command", format!("Relay couldn't read this command safely ({what})."), "write it as plain commands, one step at a time.")
}

// ---- names and arguments -----------------------------------------------------------------

/// "rm", "git", "remove-item" from "C:\x\RM.EXE", "git.exe", "Remove-Item".
pub fn base_name(raw: &str) -> String {
    let raw = raw.trim().trim_matches(['"', '\'']);
    let file = raw.rsplit(['\\', '/']).next().unwrap_or(raw).to_lowercase();
    for ext in [".exe", ".com"] {
        if let Some(stem) = file.strip_suffix(ext) {
            return stem.to_string();
        }
    }
    file
}

fn ext_of(raw: &str) -> String {
    let file = raw.trim_matches(['"', '\'']).rsplit(['\\', '/']).next().unwrap_or("").to_lowercase();
    file.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default()
}

/// Switches that never take a value (so the word after them is an argument).
const SWITCHES: &[&str] = &[
    "recurse", "force", "r", "rf", "fr", "f", "q", "s", "confirm", "whatif", "append", "nonewline", "passthru", "wait", "usebasicparsing", "noclobber", "verbose",
    "debug", "raw", "v", "i", "n", "p", "a", "l", "y", "nonewwindow", "u", "x", "z", "c", "t",
];

/// Splits arguments into positionals and named values (`-Path x`, `--out=x`).
/// A flag followed by a word takes it as its value unless it's a known
/// switch; values are also kept as candidates for path checks.
#[derive(Debug, Default)]
pub struct Args {
    pub pos: Vec<String>,
    pub named: HashMap<String, String>,
    pub flags: Vec<String>,
}

fn is_flag(a: &str, shell: Shell) -> bool {
    if a.len() > 1 && a.starts_with('-') && !a[1..].starts_with(|c: char| c.is_ascii_digit()) {
        return true;
    }
    // cmd's /s, /q, /MIR (not a path like /c/Users).
    shell == Shell::Cmd && a.starts_with('/') && a.len() > 1 && !a[1..].contains(['/', '\\'])
}

pub fn split_args(args: &[String], shell: Shell) -> Args {
    let mut out = Args::default();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if is_flag(a, shell) {
            let body = a.trim_start_matches(['-', '/']);
            if let Some((k, v)) = body.split_once(['=', ':']).filter(|(k, _)| k.len() > 1) {
                out.named.insert(k.to_lowercase(), v.to_string());
                out.flags.push(k.to_lowercase());
            } else {
                let k = body.to_lowercase();
                out.flags.push(k.clone());
                let takes = !SWITCHES.contains(&k.as_str()) && shell != Shell::Cmd;
                if takes && i + 1 < args.len() && !is_flag(&args[i + 1], shell) {
                    out.named.insert(k, args[i + 1].clone());
                    i += 1;
                } else {
                    out.named.entry(k).or_default();
                }
            }
        } else {
            out.pos.push(a.clone());
        }
        i += 1;
    }
    out
}

impl Args {
    fn has(&self, f: &str) -> bool {
        self.flags.iter().any(|x| x == f)
    }
    fn get(&self, keys: &[&str]) -> Option<&str> {
        keys.iter().find_map(|k| self.named.get(*k).map(String::as_str).filter(|v| !v.is_empty()))
    }
    /// Positionals and named values: anything that might be a path.
    fn words(&self) -> Vec<&str> {
        self.pos.iter().map(String::as_str).chain(self.named.values().map(String::as_str).filter(|v| !v.is_empty())).collect()
    }
}

/// Words that look like paths: anything with a slash, a drive, a dot, `~`
/// or a variable. Plain words (a branch name, "install") don't count.
fn pathish(w: &str) -> bool {
    let w = w.trim_matches(['"', '\'']);
    !w.is_empty() && !w.contains("://") && (w.contains(['\\', '/', '~', '%', '$']) || w.contains('.') || (w.len() >= 2 && w.as_bytes()[1] == b':'))
}

impl Rules<'_> {
    fn zone(&self, cwd: &Path, w: &str) -> (Zone, PathBuf) {
        let p = paths::resolve(cwd, w.trim_matches(['"', '\'']));
        (paths::zone(&p, self.folder, self.credentials, self.scratch), p)
    }

    fn shown(&self, p: &Path) -> String {
        paths::shown(p, self.folder)
    }

    /// Refuses reading anything in a credential location.
    fn no_credentials(&self, cwd: &Path, words: &[&str]) -> Result<(), Refusal> {
        for w in words {
            if !pathish(w) {
                continue;
            }
            let (z, p) = self.zone(cwd, w);
            if z == Zone::Credential {
                return Err(refuse("reading credentials", format!("{} is where keys, tokens or passwords are kept.", self.shown(&p)), "work without it, or ask the user for what you need."));
            }
        }
        Ok(())
    }

    /// Writes must stay in the folder or a scratch area.
    fn writes_inside(&self, cwd: &Path, targets: &[&str]) -> Result<(), Refusal> {
        for t in targets {
            let t = t.trim_matches(['"', '\'']);
            if t.is_empty() || t.eq_ignore_ascii_case("$null") || t.eq_ignore_ascii_case("nul") || t == "/dev/null" {
                continue;
            }
            let (z, p) = self.zone(cwd, t);
            match z {
                Zone::Folder | Zone::Scratch => {}
                Zone::Credential => return Err(refuse("writing outside the folder", format!("{} is a credential location.", self.shown(&p)), "write inside the folder.")),
                Zone::Protected => {
                    return Err(refuse(
                        "writing outside the folder",
                        format!("{} is outside {}.", p.display(), self.folder.display()),
                        "write it inside the folder, for example under .\\out.",
                    ))
                }
            }
        }
        Ok(())
    }

    /// Deleting is refused, except in scratch areas.
    fn deletes(&self, cwd: &Path, targets: &[&str], what: &str) -> Result<(), Refusal> {
        let targets: Vec<&&str> = targets.iter().filter(|t| !t.trim().is_empty()).collect();
        if !targets.is_empty() && targets.iter().all(|t| self.zone(cwd, t).0 == Zone::Scratch) {
            return Ok(());
        }
        let first = targets.first().map(|t| self.shown(&self.zone(cwd, t).1)).unwrap_or_else(|| "files".into());
        Err(refuse("deleting", format!("Auto mode doesn't delete files ({what} {first})."), "leave it, and mention in your final message what could be deleted."))
    }
}

// ---- the rule table ----------------------------------------------------------------------

const DELETE: &[&str] = &["remove-item", "rm", "del", "erase", "rd", "rmdir", "unlink", "shred", "srm", "clear-recyclebin", "clear-content", "rimraf", "trash", "del-cli"];
const WRITE_TARGET: &[&str] = &[
    "set-content", "add-content", "out-file", "new-item", "md", "mkdir", "touch", "tee", "tee-object", "export-csv", "export-clixml", "set-item", "ln", "mklink",
    "new-symboliclink",
];
const COPY_MOVE: &[&str] = &["copy-item", "cp", "copy", "xcopy", "robocopy", "move-item", "mv", "move", "ren", "rename", "rename-item", "rsync"];
const MOVES: &[&str] = &["move-item", "mv", "move", "ren", "rename", "rename-item"];
/// Commands that run the rest of their line as another command.
const WRAPPERS: &[&str] = &["sudo", "nohup", "time", "exec", "command", "builtin", "nice", "xargs", "timeout", "npx", "bunx", "call", "start", "env", "dotenv", "cross-env", "doas"];
const SYSTEM: &[&str] = &[
    "format", "format-volume", "diskpart", "bcdedit", "bootrec", "shutdown", "restart-computer", "stop-computer", "logoff", "new-service", "set-service",
    "stop-service", "start-service", "restart-service", "remove-service", "suspend-service", "register-scheduledtask", "unregister-scheduledtask",
    "set-scheduledtask", "new-netfirewallrule", "set-netfirewallrule", "remove-netfirewallrule", "new-localuser", "set-localuser", "remove-localuser",
    "add-localgroupmember", "set-executionpolicy", "takeown", "setx", "set-mppreference", "add-mppreference", "remove-mppreference", "clear-eventlog",
    "runas", "mountvol", "fsutil", "dism", "sfc", "manage-bde", "enable-psremoting", "disable-computerrestore", "vssadmin", "wevtutil", "pnputil",
];
const KILL: &[&str] = &["taskkill", "stop-process", "kill", "killall", "pkill", "tskill"];
const UPLOAD_TOOLS: &[&str] = &["scp", "sftp", "ftp", "pscp", "psftp"];
const INSTALLERS: &[&str] = &["winget", "choco", "scoop", "msiexec"];

/// Checks one command. `cwd` follows `cd`. Nested lines, scripts and code go
/// to `next`.
pub fn check(r: &Rules, cwd: &mut PathBuf, c: &Cmd, shell: Shell, next: &mut Vec<Next>, facts: &mut Facts) -> Result<(), Refusal> {
    if c.dynamic {
        return Err(unreadable("its command name is computed when it runs"));
    }
    // Reading credentials: refused in every mode that checks (Auto and Bypass).
    let all: Vec<&str> = c.args.iter().map(String::as_str).chain(c.reads.iter().map(String::as_str)).chain(c.writes.iter().map(String::as_str)).collect();
    r.no_credentials(cwd, &all)?;
    let name = c.name.as_str();
    if matches!(name, "cmdkey" | "vaultcmd" | "get-storedcredential") {
        return Err(refuse("reading credentials", "This reads Windows' saved passwords.", "ask the user for what you need."));
    }
    // A wrapper runs the rest: judge that.
    if WRAPPERS.contains(&name) {
        let args = strip_wrapper(name, &c.args);
        if let Some(inner) = args.first() {
            let sub = Cmd { name: base_name(inner), raw: inner.clone(), args: args[1..].to_vec(), writes: c.writes.clone(), reads: c.reads.clone(), dynamic: false };
            return check(r, cwd, &sub, shell, next, facts);
        }
        return Ok(());
    }
    // Where relative paths start.
    if matches!(name, "cd" | "set-location" | "sl" | "chdir" | "pushd" | "push-location") {
        if let Some(dir) = c.args.iter().find(|a| !is_flag(a, shell)) {
            *cwd = paths::resolve(cwd, dir);
        }
        return Ok(());
    }
    // Scripts the command runs are read and checked too.
    match ext_of(&c.raw).as_str() {
        "ps1" => next.push(Next::Script(Shell::PowerShell, paths::resolve(cwd, &c.raw))),
        "bat" | "cmd" => next.push(Next::Script(Shell::Cmd, paths::resolve(cwd, &c.raw))),
        "sh" => next.push(Next::Script(Shell::Bash, paths::resolve(cwd, &c.raw))),
        "py" => next.push(Next::CodeFile(Lang::Python, paths::resolve(cwd, &c.raw))),
        "js" | "mjs" | "cjs" => next.push(Next::CodeFile(Lang::Node, paths::resolve(cwd, &c.raw))),
        _ => {}
    }
    if r.bypass {
        return Ok(());
    }
    // Redirections write.
    let writes: Vec<&str> = c.writes.iter().map(String::as_str).collect();
    r.writes_inside(cwd, &writes)?;

    let a = split_args(&c.args, shell);
    match name {
        n if DELETE.contains(&n) => {
            // Clear-Content and Remove-Item on the registry are system changes.
            if a.words().iter().any(|w| registry(w)) {
                return Err(system(name));
            }
            return r.deletes(cwd, &a.words(), "it would delete");
        }
        "find" if a.has("delete") || c.args.iter().any(|x| x == "rm" || x == "-delete") => return r.deletes(cwd, &[], "find would delete"),
        "cipher" if a.has("w") => return Err(refuse("deleting", "cipher /w wipes free space.", "leave it.")),
        "robocopy" if a.has("mir") || a.has("purge") => return r.deletes(cwd, &a.pos.iter().skip(1).take(1).map(String::as_str).collect::<Vec<_>>(), "robocopy would remove files in"),
        n if WRITE_TARGET.contains(&n) => {
            if a.words().iter().any(|w| registry(w)) {
                return Err(system(name));
            }
            let targets: Vec<&str> = match a.get(&["path", "filepath", "literalpath", "target", "value"]) {
                Some(p) if n != "set-content" && n != "add-content" => vec![p],
                _ => a.pos.iter().map(String::as_str).chain(a.get(&["path", "filepath", "literalpath"])).collect(),
            };
            let targets: Vec<&str> = targets.into_iter().filter(|t| n == "mkdir" || n == "md" || n == "touch" || pathish(t) || !t.contains(' ')).collect();
            r.writes_inside(cwd, &targets)?;
        }
        n if COPY_MOVE.contains(&n) => {
            if n == "rsync" && a.pos.iter().any(|p| remote(p)) {
                return Err(upload("rsync to another computer"));
            }
            let (sources, dest): (Vec<&str>, Option<&str>) = match (a.get(&["destination", "dest", "destinationpath"]), n) {
                (Some(d), _) => (a.pos.iter().map(String::as_str).chain(a.get(&["path", "literalpath"])).collect(), Some(d)),
                (None, "robocopy") => (a.pos.first().map(String::as_str).into_iter().collect(), a.pos.get(1).map(String::as_str)),
                (None, "ren" | "rename" | "rename-item") => {
                    // A rename stays in the same folder.
                    let src = a.pos.first().map(String::as_str).or(a.get(&["path", "literalpath"]));
                    (src.into_iter().collect(), src)
                }
                _ => {
                    let mut pos: Vec<&str> = a.pos.iter().map(String::as_str).collect();
                    let d = pos.pop();
                    (pos, d)
                }
            };
            if let Some(d) = dest {
                r.writes_inside(cwd, &[d])?;
            }
            // Moving a file away deletes it where it was.
            if MOVES.contains(&n) {
                for s in sources {
                    let (z, p) = r.zone(cwd, s);
                    if z == Zone::Protected {
                        return Err(refuse("deleting", format!("Moving {} would remove it from outside the folder.", p.display()), "copy it into the folder instead."));
                    }
                }
            }
        }
        "sed" | "perl" if a.has("i") || c.args.iter().any(|x| x.starts_with("-i")) => {
            let targets: Vec<&str> = a.pos.iter().skip(1).map(String::as_str).collect();
            r.writes_inside(cwd, &targets)?;
        }
        "curl" | "curl.exe" => {
            if c.args.iter().any(|x| matches!(x.as_str(), "-T" | "--upload-file" | "-F" | "--form")) || data_from_file(&c.args) {
                return Err(upload("curl sending a file"));
            }
            if let Some(o) = a.get(&["o", "output"]) {
                r.writes_inside(cwd, &[o])?;
            }
        }
        "wget" => {
            if a.has("post-file") || a.has("body-file") {
                return Err(upload("wget sending a file"));
            }
            if let Some(o) = a.get(&["o", "output-document"]) {
                r.writes_inside(cwd, &[o])?;
            }
        }
        "invoke-webrequest" | "invoke-restmethod" | "iwr" | "irm" => {
            if a.has("infile") {
                return Err(upload("a request sending a file (-InFile)"));
            }
            if let Some(body) = a.get(&["body"]) {
                let b = body.to_lowercase();
                if ["get-content", "gc ", "readall", "cat ", "[io.file]"].iter().any(|k| b.contains(k)) {
                    return Err(upload("a request whose body is read from a file"));
                }
            }
            if let Some(o) = a.get(&["outfile"]) {
                r.writes_inside(cwd, &[o])?;
            }
        }
        n if UPLOAD_TOOLS.contains(&n) => return Err(upload(n)),
        "git" => git(r, &c.args)?,
        "gh" => gh(r, &a)?,
        "npm" | "pnpm" | "yarn" | "bun" => js_pm(r, cwd, name, &a, shell, next)?,
        "cargo" if a.pos.first().map(String::as_str) == Some("install") => return Err(global("cargo install")),
        "cargo" if a.pos.first().map(String::as_str) == Some("publish") => publishing(r, "cargo publish")?,
        "go" if a.pos.first().map(String::as_str) == Some("install") => return Err(global("go install")),
        "gem" if a.pos.first().map(String::as_str) == Some("install") => return Err(global("gem install")),
        "dotnet" if c.args.iter().any(|x| x == "-g" || x == "--global") => return Err(global("a global dotnet tool")),
        "dotnet" if c.args.windows(2).any(|w| w[0] == "nuget" && w[1] == "push") => publishing(r, "dotnet nuget push")?,
        "twine" if a.pos.first().map(String::as_str) == Some("upload") => publishing(r, "twine upload")?,
        "poetry" if a.pos.first().map(String::as_str) == Some("publish") => publishing(r, "poetry publish")?,
        "docker" | "podman" if a.pos.first().map(String::as_str) == Some("push") => publishing(r, "docker push")?,
        "conda" | "mamba" if matches!(a.pos.first().map(String::as_str), Some("install" | "create" | "remove" | "uninstall")) => {
            match a.get(&["p", "prefix"]) {
                Some(p) if r.zone(cwd, p).0 == Zone::Folder => {}
                _ => return Err(global("conda outside the folder (use -p .\\env)")),
            }
        }
        n if INSTALLERS.contains(&n) => return Err(global(n)),
        "pip" | "pip3" | "uv" => pip(r, cwd, &c.args, facts)?,
        "python" | "python3" | "py" | "pythonw" => python(r, cwd, &c.args, facts, next)?,
        "node" | "deno" | "tsx" | "ts-node" => node(cwd, &c.args, next),
        "powershell" | "pwsh" => nested_powershell(cwd, &c.args, next)?,
        "cmd" => {
            if let Some(i) = c.args.iter().position(|x| x.eq_ignore_ascii_case("/c") || x.eq_ignore_ascii_case("/k") || x.eq_ignore_ascii_case("/r")) {
                next.push(Next::Line(Shell::Cmd, c.args[i + 1..].join(" ")));
            }
        }
        "bash" | "sh" | "zsh" | "dash" => {
            if let Some(i) = c.args.iter().position(|x| x == "-c" || x == "-lc") {
                if let Some(line) = c.args.get(i + 1) {
                    next.push(Next::Line(Shell::Bash, line.clone()));
                }
            } else if let Some(script) = a.pos.first() {
                next.push(Next::Script(Shell::Bash, paths::resolve(cwd, script)));
            }
        }
        "wsl" => return Err(refuse("hidden code", "Commands run inside WSL aren't checked.", "run it with powershell or bash instead.")),
        "invoke-expression" | "iex" => {
            match c.args.first() {
                // A plain string: read it as PowerShell.
                Some(s) if !s.contains('$') && !s.contains('(') => next.push(Next::Line(Shell::PowerShell, s.clone())),
                _ => return Err(refuse("hidden code", "Invoke-Expression runs text built while it runs.", "run the commands directly.")),
            }
        }
        "add-type" => return Err(refuse("hidden code", "Add-Type compiles and runs code Relay can't read.", "use plain commands or a script file.")),
        "start-process" | "saps" => {
            if a.get(&["verb"]).is_some_and(|v| v.eq_ignore_ascii_case("runas")) {
                return Err(system("running as administrator"));
            }
            let file = a.get(&["filepath"]).or(a.pos.first().map(String::as_str)).unwrap_or("");
            let mut args: Vec<String> = Vec::new();
            if let Some(list) = a.get(&["argumentlist", "args"]) {
                args.extend(list.split([',', ' ']).map(|s| s.trim().trim_matches(['"', '\'']).to_string()).filter(|s| !s.is_empty()));
            } else {
                args.extend(a.pos.iter().skip(1).cloned());
            }
            if !file.is_empty() {
                let sub = Cmd { name: base_name(file), raw: file.to_string(), args, ..Default::default() };
                return check(r, cwd, &sub, shell, next, facts);
            }
        }
        "reg" if !matches!(a.pos.first().map(|s| s.to_lowercase()).as_deref(), Some("query" | "export")) => return Err(system("reg")),
        "set-itemproperty" | "new-itemproperty" | "remove-itemproperty" | "rename-itemproperty" | "sp" => return Err(system(name)),
        "sc" if !matches!(a.pos.first().map(|s| s.to_lowercase()).as_deref(), Some("query" | "qc" | "queryex" | "qdescription")) => return Err(system("sc")),
        "schtasks" if !a.has("query") => return Err(system("schtasks")),
        "netsh" if !c.args.iter().any(|x| x.eq_ignore_ascii_case("show")) => return Err(system("netsh")),
        "net" if matches!(a.pos.first().map(|s| s.to_lowercase()).as_deref(), Some("user" | "localgroup" | "share" | "stop" | "start" | "use")) => return Err(system("net")),
        "icacls" if c.args.iter().any(|x| ["/grant", "/deny", "/reset", "/setowner", "/remove", "/inheritance"].iter().any(|f| x.to_lowercase().starts_with(f))) => {
            return Err(system("icacls"))
        }
        n if SYSTEM.contains(&n) => return Err(system(n)),
        n if KILL.contains(&n) => {
            return Err(refuse("stopping processes", "Auto mode doesn't stop programs (one could be the user's).", "stop background processes you started with the processes tool."))
        }
        _ => {}
    }
    Ok(())
}

/// A .NET call: deleting, writing, the registry, compiling code.
pub fn check_member(r: &Rules, m: &Member) -> Result<(), Refusal> {
    let t = m.target.to_lowercase();
    let member = m.member.to_lowercase();
    if t.contains("protecteddata") && member.contains("unprotect") {
        return Err(refuse("reading credentials", "This decrypts data Windows protects for the user.", "ask the user for what you need."));
    }
    if r.bypass {
        return Ok(());
    }
    let io = t.contains("io.file") || t.contains("io.directory") || t.contains("io.fileinfo") || t.contains("io.directoryinfo");
    if member == "delete" || (io && member.starts_with("delete")) {
        return Err(refuse("deleting", format!("Auto mode doesn't delete files ({}).", m.text), "leave it, and mention what could be deleted."));
    }
    if t.contains("environment") && member == "setenvironmentvariable" && (m.text.contains("Machine") || m.text.contains("User")) {
        return Err(system("a lasting environment variable"));
    }
    if t.contains("registry") && (member.starts_with("set") || member.starts_with("delete") || member.starts_with("create")) {
        return Err(system("the registry"));
    }
    if t.contains("scriptblock") && member == "create" {
        return Err(refuse("hidden code", "A script block built from text runs code Relay can't read.", "run the commands directly."));
    }
    if io && (member.starts_with("write") || member.starts_with("append") || member == "copy" || member == "move" || member == "create" || member == "replace") {
        // The path is the first argument: only literal paths can be judged.
        let first = m.text.split_once('(').map(|(_, rest)| rest).unwrap_or("").split([',', ')']).next().unwrap_or("").trim().trim_matches(['"', '\'']);
        if first.is_empty() || first.starts_with('$') {
            return Ok(());
        }
        let p = paths::resolve(r.folder, first);
        if paths::zone(&p, r.folder, r.credentials, r.scratch) == Zone::Protected {
            return Err(refuse("writing outside the folder", format!("{} is outside {}.", p.display(), r.folder.display()), "write it inside the folder."));
        }
    }
    Ok(())
}

fn registry(w: &str) -> bool {
    let l = w.trim_matches(['"', '\'']).to_lowercase();
    ["hklm:", "hkcu:", "hkcr:", "hku:", "hkcc:", "registry::", "hkey_"].iter().any(|k| l.starts_with(k))
}

fn system(what: &str) -> Refusal {
    refuse("system changes", format!("{what} changes the computer's settings, services or accounts."), "tell the user what to change and why.")
}

fn upload(what: &str) -> Refusal {
    refuse("uploading", format!("{what} sends data to another computer."), "save the result in the folder and tell the user where it is.")
}

fn global(what: &str) -> Refusal {
    refuse("global installs", format!("{what} installs for the whole computer or user."), "install into the folder (pip into .venv, npm without -g), or tell the user what to install.")
}

fn remote(p: &str) -> bool {
    p.starts_with("rsync://") || p.find(':').is_some_and(|i| i > 1 && !p[..i].contains(['/', '\\']))
}

fn data_from_file(args: &[String]) -> bool {
    args.windows(2).any(|w| matches!(w[0].as_str(), "-d" | "--data" | "--data-binary" | "--data-raw" | "--data-urlencode" | "--json") && w[1].contains('@'))
        || args.iter().any(|a| (a.starts_with("--data") || a.starts_with("-d@")) && a.contains("=@") || a.starts_with("-d@"))
}

fn publishing(r: &Rules, what: &str) -> Result<(), Refusal> {
    if r.may_publish {
        return Ok(());
    }
    Err(refuse("publishing", format!("{what} publishes outside this computer."), "leave it for the user, or they can turn on \"Auto may push and publish\" for this folder."))
}

fn strip_wrapper<'a>(name: &str, args: &'a [String]) -> &'a [String] {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let assignment = a.contains('=') && !a.starts_with('-') && a.split('=').next().is_some_and(|k| k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        if a.starts_with('-') || (name != "xargs" && assignment) || (name == "timeout" && a.chars().next().is_some_and(|c| c.is_ascii_digit())) {
            i += 1;
            continue;
        }
        // start "" prog: the empty title.
        if name == "start" && a.trim_matches('"').is_empty() {
            i += 1;
            continue;
        }
        if name == "start" && a.starts_with('/') {
            i += 1;
            continue;
        }
        break;
    }
    &args[i..]
}

fn git(r: &Rules, args: &[String]) -> Result<(), Refusal> {
    // Skip global options: -C dir, -c k=v, --git-dir=…
    let mut i = 0;
    while i < args.len() && args[i].starts_with('-') {
        i += if matches!(args[i].as_str(), "-C" | "-c" | "--git-dir" | "--work-tree") { 2 } else { 1 };
    }
    let Some(sub) = args.get(i).map(|s| s.to_lowercase()) else { return Ok(()) };
    let rest: Vec<&str> = args[i + 1..].iter().map(String::as_str).collect();
    let has = |f: &str| rest.contains(&f);
    let destructive = |what: &str| Err(refuse("destructive git", format!("git {what} can throw away work."), "leave the repository as it is and tell the user."));
    if r.bypass {
        return Ok(());
    }
    match sub.as_str() {
        "clean" => destructive("clean"),
        "reset" if has("--hard") || has("--merge") => destructive("reset --hard"),
        "checkout" if has("--") || has(".") || has("-f") || has("--force") || has("-p") => destructive("checkout of files"),
        "switch" if has("--discard-changes") || has("-f") || has("--force") => destructive("switch --discard-changes"),
        "restore" if !(has("--staged") && !has("--worktree") && !has("-W")) => destructive("restore"),
        "stash" if matches!(rest.first(), Some(&"drop") | Some(&"clear")) => destructive("stash drop"),
        "branch" if has("-D") || (has("--delete") && has("--force")) => destructive("branch -D"),
        "filter-branch" | "filter-repo" => destructive(&sub),
        "gc" if rest.iter().any(|a| a.starts_with("--prune")) => destructive("gc --prune"),
        "prune" => destructive("prune"),
        "reflog" if has("expire") || has("delete") => destructive("reflog expire"),
        "update-ref" if has("-d") => destructive("update-ref -d"),
        "rm" if !has("--cached") => Err(refuse("deleting", "git rm deletes files.", "use git rm --cached to stop tracking a file, or leave it.")),
        "push" => {
            if rest.iter().any(|a| matches!(*a, "-f" | "--force" | "--force-with-lease" | "--mirror" | "--delete" | "-d") || a.starts_with("--force") || a.starts_with('+')) {
                return Err(refuse("destructive git", "A force push (or deleting a remote branch) can overwrite other people's work.", "leave it for the user."));
            }
            publishing(r, "git push")
        }
        _ => Ok(()),
    }
}

fn gh(r: &Rules, a: &Args) -> Result<(), Refusal> {
    let p: Vec<&str> = a.pos.iter().map(String::as_str).collect();
    match p.as_slice() {
        ["repo", "delete", ..] => Err(refuse("deleting", "gh repo delete deletes a repository.", "leave it for the user.")),
        ["pr", "create" | "merge" | "close" | "edit" | "comment" | "review", ..]
        | ["release", ..]
        | ["gist", "create" | "edit" | "delete", ..]
        | ["repo", "create" | "edit" | "fork" | "rename" | "archive", ..]
        | ["issue", "create" | "close" | "edit" | "comment" | "delete", ..]
        | ["secret" | "variable", "set" | "delete", ..]
        | ["workflow", "run" | "enable" | "disable", ..] => publishing(r, &format!("gh {}", p[..2.min(p.len())].join(" "))),
        ["api", ..] if a.get(&["x", "method"]).is_some_and(|m| !m.eq_ignore_ascii_case("get")) || a.has("f") || a.has("field") || a.has("raw-field") || a.has("input") => {
            publishing(r, "gh api with a request that changes things")
        }
        _ => Ok(()),
    }
}

/// npm / pnpm / yarn / bun: global installs, publishing, and package.json
/// scripts (their command lines are checked).
fn js_pm(r: &Rules, cwd: &Path, pm: &str, a: &Args, shell: Shell, next: &mut Vec<Next>) -> Result<(), Refusal> {
    let first = a.pos.first().map(String::as_str);
    if a.has("g") || a.has("global") || first == Some("global") || a.has("location") && a.get(&["location"]) == Some("global") {
        return Err(global(&format!("{pm} -g")));
    }
    if first == Some("publish") || (first == Some("npm") && a.pos.get(1).map(String::as_str) == Some("publish")) {
        return publishing(r, &format!("{pm} publish"));
    }
    if matches!(first, Some("dlx" | "exec" | "x")) {
        // pnpm dlx tool …: the tool runs.
        if let Some(tool) = a.pos.get(1) {
            let rest: Vec<String> = a.pos.iter().skip(2).cloned().collect();
            next.push(Next::Line(Shell::Bash, std::iter::once(tool.clone()).chain(rest).collect::<Vec<_>>().join(" ")));
        }
        return Ok(());
    }
    // A package.json script: `npm run x`, `npm test`, `pnpm x`, `yarn x`.
    let script = match first {
        Some("run" | "run-script") => a.pos.get(1).map(String::as_str),
        Some(s @ ("test" | "start" | "build" | "dev" | "lint")) => Some(s),
        Some(s) if pm != "npm" && !matches!(s, "install" | "i" | "add" | "remove" | "rm" | "update" | "up" | "ci" | "init" | "create") => Some(s),
        _ => None,
    };
    if let Some(name) = script {
        if let Ok(text) = std::fs::read_to_string(cwd.join("package.json")) {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&text) {
                for key in [format!("pre{name}"), name.to_string(), format!("post{name}")] {
                    if let Some(line) = pkg.pointer(&format!("/scripts/{key}")).and_then(|v| v.as_str()) {
                        // npm runs scripts with cmd.exe on Windows.
                        next.push(Next::Line(if shell == Shell::Bash { Shell::Bash } else { Shell::Cmd }, line.to_string()));
                    }
                }
            }
        }
    }
    let _ = r;
    Ok(())
}

fn pip(r: &Rules, cwd: &Path, args: &[String], facts: &mut Facts) -> Result<(), Refusal> {
    // `uv pip install …` and `pip install …`
    let args: Vec<&str> = args.iter().map(String::as_str).skip_while(|a| *a == "pip").collect();
    let Some(sub) = args.iter().find(|a| !a.starts_with('-')) else { return Ok(()) };
    if !matches!(*sub, "install" | "uninstall") {
        return Ok(());
    }
    if args.contains(&"--user") {
        return Err(global("pip install --user"));
    }
    for flag in ["--target", "-t", "--prefix", "--root"] {
        if let Some(i) = args.iter().position(|a| *a == flag) {
            if let Some(dir) = args.get(i + 1) {
                if r.zone(cwd, dir).0 != Zone::Folder {
                    return Err(global(&format!("pip install {flag} outside the folder")));
                }
            }
        }
    }
    facts.pip_install = true;
    Ok(())
}

fn python(r: &Rules, cwd: &Path, args: &[String], facts: &mut Facts, next: &mut Vec<Next>) -> Result<(), Refusal> {
    // -I, -E and -S skip PYTHONPATH or site: the guard doesn't load.
    let bare = args.iter().take_while(|a| a.starts_with('-') && *a != "-c" && *a != "-m").any(|a| !a.starts_with("--") && a[1..].chars().any(|c| matches!(c, 'I' | 'E' | 'S')));
    let lang = if bare { Lang::PythonBare } else { Lang::Python };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-c" => {
                if let Some(code) = args.get(i + 1) {
                    next.push(Next::Code(lang, code.clone()));
                }
                return Ok(());
            }
            "-m" => {
                let module = args.get(i + 1).map(String::as_str).unwrap_or("");
                if module == "pip" {
                    return pip(r, cwd, &args[i + 2..], facts);
                }
                if module == "venv" || module == "virtualenv" {
                    // A new environment must be inside the folder.
                    if let Some(dir) = args[i + 2..].iter().find(|a| !a.starts_with('-')) {
                        r.writes_inside(cwd, &[dir])?;
                    }
                }
                return Ok(());
            }
            s if s.starts_with('-') => {
                i += 1;
                continue;
            }
            script => {
                next.push(Next::CodeFile(lang, paths::resolve(cwd, script)));
                return Ok(());
            }
        }
    }
    Ok(())
}

fn node(cwd: &Path, args: &[String], next: &mut Vec<Next>) {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if matches!(a, "-e" | "--eval" | "-p" | "--print") {
            if let Some(code) = args.get(i + 1) {
                next.push(Next::Code(Lang::Node, code.clone()));
            }
            return;
        }
        if a.starts_with('-') || a == "run" {
            i += 1;
            continue;
        }
        next.push(Next::CodeFile(Lang::Node, paths::resolve(cwd, a)));
        return;
    }
}

fn nested_powershell(cwd: &Path, args: &[String], next: &mut Vec<Next>) -> Result<(), Refusal> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].to_lowercase();
        // -EncodedCommand and its short forms (-e, -ec, -enc…).
        if a == "-ec" || (a.len() >= 2 && "-encodedcommand".starts_with(&a)) {
            return Err(refuse("hidden code", "An encoded PowerShell command can't be read.", "run the commands directly."));
        }
        if a == "-command" || a == "-c" || (a.starts_with("-com") && "-command".starts_with(&a)) {
            next.push(Next::Line(Shell::PowerShell, args[i + 1..].join(" ")));
            return Ok(());
        }
        if a == "-file" || a == "-f" {
            if let Some(f) = args.get(i + 1) {
                next.push(Next::Script(Shell::PowerShell, paths::resolve(cwd, f)));
            }
            return Ok(());
        }
        if a.starts_with('-') {
            // -ExecutionPolicy Bypass, -WindowStyle Hidden: skip the value too.
            if matches!(a.as_str(), "-executionpolicy" | "-ep" | "-ex" | "-windowstyle" | "-w" | "-version" | "-v" | "-workingdirectory" | "-wd" | "-configurationname" | "-inputformat" | "-outputformat" | "-of" | "-if") {
                i += 1;
            }
            i += 1;
            continue;
        }
        // The rest is the command.
        next.push(Next::Line(Shell::PowerShell, args[i..].join(" ")));
        return Ok(());
    }
    Ok(())
}

// ---- a whole command line ------------------------------------------------------------------

/// How deep nested shells, scripts and code are followed.
const MAX_DEPTH: usize = 6;

/// Parses a line in its shell (PowerShell by its own parser: blocking).
pub fn parse_line(shell: Shell, line: &str) -> Result<Parsed, String> {
    match shell {
        Shell::PowerShell => super::psparse::parse(line),
        Shell::Bash => parse_bash(line),
        Shell::Cmd => parse_cmd(line),
    }
}

/// Reads a script or code file the command runs (at most 256 KB of text).
fn read_source(path: &Path) -> Result<String, Refusal> {
    let shown = path.display().to_string();
    let meta = std::fs::metadata(path).map_err(|_| refuse("unreadable command", format!("It runs {shown}, which isn't there to read yet."), "write the file first, then run it as its own command."))?;
    if meta.len() > super::psparse::MAX_SOURCE as u64 {
        return Err(unreadable(&format!("{shown} is longer than 256 KB")));
    }
    let bytes = std::fs::read(path).map_err(|e| unreadable(&format!("{shown} couldn't be read: {e}")))?;
    super::files::decode(&bytes).ok_or_else(|| unreadable(&format!("{shown} isn't text")))
}

/// Node finds `node server` as server.js, or server/index.js.
fn node_file(p: &Path) -> PathBuf {
    if p.is_file() {
        return p.to_path_buf();
    }
    for c in [p.with_extension("js"), p.join("index.js")] {
        if c.is_file() {
            return c;
        }
    }
    p.to_path_buf()
}

/// Judges a command line: every command it runs, and the nested lines,
/// scripts and code they lead to. Blocking (PowerShell's parser, file reads).
pub fn judge_line(r: &Rules, shell: Shell, line: &str) -> Result<Facts, Refusal> {
    judge_line_in(r, shell, line, r.folder)
}

/// `judge_line`, with relative paths starting in `cwd`.
pub fn judge_line_in(r: &Rules, shell: Shell, line: &str, cwd: &Path) -> Result<Facts, Refusal> {
    let mut facts = Facts::default();
    let mut queue: Vec<(Next, PathBuf, usize)> = vec![(Next::Line(shell, line.to_string()), cwd.to_path_buf(), 0)];
    let mut read = 0usize;
    while let Some((item, cwd, depth)) = queue.pop() {
        if depth > MAX_DEPTH {
            return Err(unreadable("commands nested too deeply"));
        }
        match item {
            Next::Line(sh, text) => {
                let parsed = parse_line(sh, &text).map_err(|e| unreadable(&e))?;
                for m in &parsed.members {
                    check_member(r, m)?;
                }
                let mut cwd = cwd;
                let mut next = Vec::new();
                for c in &parsed.cmds {
                    check(r, &mut cwd, c, sh, &mut next, &mut facts)?;
                }
                queue.extend(next.into_iter().map(|n| (n, cwd.clone(), depth + 1)));
            }
            Next::Script(sh, path) => {
                r.no_credentials(&cwd, &[&path.to_string_lossy()])?;
                let text = read_source(&path)?;
                read += text.len();
                if read > 4 * super::psparse::MAX_SOURCE {
                    return Err(unreadable("the scripts it runs are too long to read"));
                }
                queue.push((Next::Line(sh, text), cwd, depth + 1));
            }
            // Guarded Python is judged as it runs.
            Next::Code(Lang::Python, _) if r.python_guard => {}
            Next::Code(lang, code) => check_code(r, lang, &code)?,
            Next::CodeFile(lang, path) => {
                let path = if lang == Lang::Node { node_file(&path) } else { path };
                r.no_credentials(&cwd, &[&path.to_string_lossy()])?;
                if lang == Lang::Python && r.python_guard {
                    continue;
                }
                let text = read_source(&path)?;
                check_code(r, lang, &text)?;
            }
        }
    }
    Ok(facts)
}

// ---- code that runs: a static read ---------------------------------------------------------

/// Python and Node code read before it runs (Python's runtime guard, where it
/// loads, judges the rest as it runs).
pub fn check_code(r: &Rules, lang: Lang, code: &str) -> Result<(), Refusal> {
    if r.bypass {
        return Ok(());
    }
    let deletes: &[&str] = match lang {
        Lang::Python | Lang::PythonBare => &["os.remove(", "os.unlink(", "shutil.rmtree(", "os.rmdir(", "os.removedirs(", ".unlink(", ".rmdir(", "send2trash"],
        Lang::Node => &["fs.rm(", "fs.rmSync(", "fs.unlink(", "fs.unlinkSync(", "fs.rmdir(", "fs.rmdirSync(", ".rm(", "rimraf", "fs.promises.rm", "fs-extra", "unlinkSync(", "rmSync("],
    };
    if let Some(d) = deletes.iter().find(|d| code.contains(*d)) {
        return Err(refuse("deleting", format!("This code deletes files ({}).", d.trim_end_matches('(')), "leave the files, and mention what could be deleted."));
    }
    let spawns: &[&str] = match lang {
        Lang::Python | Lang::PythonBare => &["subprocess", "os.system(", "os.popen(", "os.exec", "os.spawn", "pty.spawn"],
        Lang::Node => &["child_process", "execSync(", "spawnSync(", "exec(", "spawn(", "execFile("],
    };
    if let Some(s) = spawns.iter().find(|s| code.contains(*s)) {
        return Err(refuse("hidden code", format!("This code starts other programs ({}), which Auto can't check from here.", s.trim_end_matches('(')), "run those commands with the shell tools instead."));
    }
    // Absolute paths in the code that point outside the folder, near a write.
    let writes = match lang {
        Lang::Python | Lang::PythonBare => ["open(", ".write_text(", ".write_bytes(", "shutil.copy", "shutil.move", ".rename("].iter().any(|w| code.contains(w)),
        Lang::Node => ["writeFile", "appendFile", "createWriteStream", "copyFile", "rename("].iter().any(|w| code.contains(w)),
    };
    if writes {
        for lit in string_literals(code) {
            if lit.len() > 2 && (lit.as_bytes()[1] == b':' || lit.starts_with("~") || lit.starts_with('/')) {
                let p = paths::resolve(r.folder, &lit);
                match paths::zone(&p, r.folder, r.credentials, r.scratch) {
                    Zone::Protected => {
                        return Err(refuse("writing outside the folder", format!("The code names {} (outside {}).", p.display(), r.folder.display()), "write inside the folder."))
                    }
                    Zone::Credential => return Err(refuse("reading credentials", format!("The code names {}.", p.display()), "work without it.")),
                    _ => {}
                }
            }
        }
    }
    for lit in string_literals(code) {
        if pathish(&lit) && paths::zone(&paths::resolve(r.folder, &lit), r.folder, r.credentials, r.scratch) == Zone::Credential {
            return Err(refuse("reading credentials", format!("The code names {lit}."), "work without it."));
        }
    }
    Ok(())
}

fn string_literals(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = code.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let q = chars[i];
        if q == '"' || q == '\'' || q == '`' {
            let mut s = String::new();
            let mut j = i + 1;
            while j < chars.len() && chars[j] != q && chars[j] != '\n' {
                if chars[j] == '\\' && j + 1 < chars.len() {
                    s.push(chars[j + 1]);
                    j += 2;
                    continue;
                }
                s.push(chars[j]);
                j += 1;
            }
            out.push(s.replace("\\\\", "\\"));
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

// ---- Bash ----------------------------------------------------------------------------------

/// Reads a Bash command line into its commands: quotes and escapes, `&&`,
/// `||`, `;`, `|`, `&`, redirections, `$( )` and backticks (whose commands
/// count too), `( )` and `{ }` groups, heredocs (their text isn't run).
pub fn parse_bash(src: &str) -> Result<Parsed, String> {
    let mut out = Parsed::default();
    bash_into(src, &mut out, 0)?;
    Ok(out)
}

fn bash_into(src: &str, out: &mut Parsed, depth: usize) -> Result<(), String> {
    if depth > 8 {
        return Err("commands nested too deeply".into());
    }
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut redirect: Option<bool> = None; // Some(true): the next word is written, Some(false): read
    let mut writes = Vec::new();
    let mut reads = Vec::new();
    let mut heredoc: Option<String> = None;
    let finish_word = |word: &mut String, in_word: &mut bool, words: &mut Vec<String>, redirect: &mut Option<bool>, writes: &mut Vec<String>, reads: &mut Vec<String>| {
        if *in_word {
            match redirect.take() {
                Some(true) => writes.push(std::mem::take(word)),
                Some(false) => reads.push(std::mem::take(word)),
                None => words.push(std::mem::take(word)),
            }
            *in_word = false;
        }
    };
    let flush = |words: &mut Vec<String>, writes: &mut Vec<String>, reads: &mut Vec<String>, out: &mut Parsed| {
        // VAR=value before a command only sets the environment.
        let start = words.iter().position(|w| !(w.contains('=') && w.split('=').next().is_some_and(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')))).unwrap_or(words.len());
        let ws: Vec<String> = words.drain(..).skip(start).collect();
        if let Some((first, rest)) = ws.split_first() {
            if !matches!(first.as_str(), "{" | "}" | "!" | "if" | "then" | "else" | "fi" | "do" | "done" | "while" | "for" | "in") {
                out.cmds.push(Cmd { name: base_name(first), raw: first.clone(), args: rest.to_vec(), writes: std::mem::take(writes), reads: std::mem::take(reads), dynamic: first.starts_with('$') });
                return;
            }
            // Keywords: the words after them still form a command.
            if rest.len() > 0 && matches!(first.as_str(), "then" | "else" | "do" | "!") {
                out.cmds.push(Cmd { name: base_name(&rest[0]), raw: rest[0].clone(), args: rest[1..].to_vec(), writes: std::mem::take(writes), reads: std::mem::take(reads), dynamic: rest[0].starts_with('$') });
                return;
            }
        }
        if !writes.is_empty() || !reads.is_empty() {
            out.cmds.push(Cmd { name: String::new(), raw: String::new(), args: Vec::new(), writes: std::mem::take(writes), reads: std::mem::take(reads), dynamic: false });
        }
    };
    while i < chars.len() {
        let c = chars[i];
        // A heredoc's body runs to its delimiter line.
        if c == '\n' {
            if let Some(delim) = heredoc.take() {
                finish_word(&mut word, &mut in_word, &mut words, &mut redirect, &mut writes, &mut reads);
                flush(&mut words, &mut writes, &mut reads, out);
                let rest: String = chars[i + 1..].iter().collect();
                let mut consumed = 0;
                let mut found = false;
                for line in rest.split_inclusive('\n') {
                    consumed += line.chars().count();
                    if line.trim() == delim {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return Err("a heredoc that never ends".into());
                }
                i += 1 + consumed;
                continue;
            }
        }
        match c {
            '\\' if i + 1 < chars.len() => {
                if chars[i + 1] != '\n' {
                    word.push(chars[i + 1]);
                    in_word = true;
                }
                i += 2;
            }
            '\'' => {
                let end = chars[i + 1..].iter().position(|&x| x == '\'').ok_or("an unclosed quote")? + i + 1;
                word.extend(&chars[i + 1..end]);
                in_word = true;
                i = end + 1;
            }
            '"' => {
                let mut j = i + 1;
                while j < chars.len() && chars[j] != '"' {
                    if chars[j] == '\\' && j + 1 < chars.len() {
                        word.push(chars[j + 1]);
                        j += 2;
                        continue;
                    }
                    if chars[j] == '$' && chars.get(j + 1) == Some(&'(') {
                        let end = close_paren(&chars, j + 1).ok_or("an unclosed $(")?;
                        bash_into(&chars[j + 2..end].iter().collect::<String>(), out, depth + 1)?;
                        word.push_str("$(…)");
                        j = end + 1;
                        continue;
                    }
                    if chars[j] == '`' {
                        let end = chars[j + 1..].iter().position(|&x| x == '`').ok_or("an unclosed backtick")? + j + 1;
                        bash_into(&chars[j + 1..end].iter().collect::<String>(), out, depth + 1)?;
                        word.push_str("`…`");
                        j = end + 1;
                        continue;
                    }
                    word.push(chars[j]);
                    j += 1;
                }
                if j >= chars.len() {
                    return Err("an unclosed quote".into());
                }
                in_word = true;
                i = j + 1;
            }
            '$' if chars.get(i + 1) == Some(&'(') => {
                let end = close_paren(&chars, i + 1).ok_or("an unclosed $(")?;
                let inner: String = chars[i + 2..end].iter().collect();
                // $(( arithmetic )) isn't a command.
                if !inner.starts_with('(') {
                    bash_into(&inner, out, depth + 1)?;
                }
                word.push_str("$(…)");
                in_word = true;
                i = end + 1;
            }
            '`' => {
                let end = chars[i + 1..].iter().position(|&x| x == '`').ok_or("an unclosed backtick")? + i + 1;
                bash_into(&chars[i + 1..end].iter().collect::<String>(), out, depth + 1)?;
                word.push_str("`…`");
                in_word = true;
                i = end + 1;
            }
            '#' if !in_word => {
                // A comment, to the end of the line.
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            ';' | '\n' | '|' | '&' | '(' | ')' => {
                // 2>&1, >&2, &>
                if c == '&' && chars.get(i + 1) == Some(&'>') {
                    finish_word(&mut word, &mut in_word, &mut words, &mut redirect, &mut writes, &mut reads);
                    let append = chars.get(i + 2) == Some(&'>');
                    redirect = Some(true);
                    i += if append { 3 } else { 2 };
                    continue;
                }
                finish_word(&mut word, &mut in_word, &mut words, &mut redirect, &mut writes, &mut reads);
                flush(&mut words, &mut writes, &mut reads, out);
                i += if (c == '|' || c == '&') && chars.get(i + 1) == Some(&c) { 2 } else { 1 };
            }
            '>' | '<' => {
                // A digit right before (2>) belongs to the operator.
                if in_word && word.chars().all(|d| d.is_ascii_digit()) {
                    word.clear();
                    in_word = false;
                }
                finish_word(&mut word, &mut in_word, &mut words, &mut redirect, &mut writes, &mut reads);
                let mut j = i + 1;
                if c == '<' && chars.get(j) == Some(&'<') {
                    // <<EOF, <<-EOF, <<'EOF', <<< "string"
                    j += 1;
                    if chars.get(j) == Some(&'<') {
                        i = j + 1;
                        continue;
                    }
                    if chars.get(j) == Some(&'-') {
                        j += 1;
                    }
                    while j < chars.len() && chars[j] == ' ' {
                        j += 1;
                    }
                    let start = j;
                    while j < chars.len() && !chars[j].is_whitespace() && !matches!(chars[j], ';' | '|' | '&') {
                        j += 1;
                    }
                    heredoc = Some(chars[start..j].iter().collect::<String>().trim_matches(['\'', '"']).to_string());
                    i = j;
                    continue;
                }
                if chars.get(j) == Some(&'>') || chars.get(j) == Some(&'|') {
                    j += 1;
                }
                // >&2, <&0: a stream, not a file.
                if chars.get(j) == Some(&'&') {
                    j += 1;
                    while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == '-') {
                        j += 1;
                    }
                    i = j;
                    continue;
                }
                redirect = Some(c == '>');
                i = j;
            }
            c if c.is_whitespace() => {
                finish_word(&mut word, &mut in_word, &mut words, &mut redirect, &mut writes, &mut reads);
                i += 1;
            }
            c => {
                word.push(c);
                in_word = true;
                i += 1;
            }
        }
    }
    if heredoc.is_some() {
        return Err("a heredoc that never ends".into());
    }
    finish_word(&mut word, &mut in_word, &mut words, &mut redirect, &mut writes, &mut reads);
    if redirect.is_some() {
        return Err("a redirection with no target".into());
    }
    flush(&mut words, &mut writes, &mut reads, out);
    Ok(())
}

fn close_paren(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut quote: Option<char> = None;
    let mut i = open;
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) if c == '\\' => i += 1,
            Some(_) => {}
            None => match c {
                '\'' | '"' => quote = Some(c),
                '\\' => i += 1,
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}

// ---- cmd.exe -------------------------------------------------------------------------------

/// Reads a cmd.exe line: `^` escapes, quotes, `&`, `&&`, `||`, `|`, `( )`,
/// redirections. `%VAR%` stays for path resolution to expand.
pub fn parse_cmd(src: &str) -> Result<Parsed, String> {
    let mut out = Parsed::default();
    let chars: Vec<char> = src.chars().collect();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quoted = false;
    let mut redirect: Option<bool> = None;
    let mut writes = Vec::new();
    let mut reads = Vec::new();
    let end_word = |word: &mut String, in_word: &mut bool, words: &mut Vec<String>, redirect: &mut Option<bool>, writes: &mut Vec<String>, reads: &mut Vec<String>| {
        if *in_word {
            match redirect.take() {
                Some(true) => writes.push(std::mem::take(word)),
                Some(false) => reads.push(std::mem::take(word)),
                None => words.push(std::mem::take(word)),
            }
            *in_word = false;
        }
    };
    let flush = |words: &mut Vec<String>, writes: &mut Vec<String>, reads: &mut Vec<String>, out: &mut Parsed| {
        let mut ws: Vec<String> = std::mem::take(words);
        // @echo off, @cmd
        if let Some(f) = ws.first_mut() {
            if let Some(rest) = f.strip_prefix('@') {
                *f = rest.to_string();
            }
        }
        ws.retain(|w| !w.is_empty());
        if let Some((first, rest)) = ws.split_first() {
            let l = first.to_lowercase();
            if !matches!(l.as_str(), "rem" | "::" | "echo." | "goto" | "setlocal" | "endlocal") && !l.starts_with(':') {
                out.cmds.push(Cmd { name: base_name(first), raw: first.clone(), args: rest.to_vec(), writes: std::mem::take(writes), reads: std::mem::take(reads), dynamic: first.starts_with('%') && first.ends_with('%') });
                return;
            }
        }
        if !writes.is_empty() {
            out.cmds.push(Cmd { writes: std::mem::take(writes), reads: std::mem::take(reads), ..Default::default() });
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if quoted {
            if c == '"' {
                quoted = false;
            } else {
                word.push(c);
            }
            i += 1;
            continue;
        }
        match c {
            '^' if i + 1 < chars.len() => {
                word.push(chars[i + 1]);
                in_word = true;
                i += 2;
            }
            '"' => {
                quoted = true;
                in_word = true;
                i += 1;
            }
            '&' | '|' | '(' | ')' | '\n' | '\r' => {
                end_word(&mut word, &mut in_word, &mut words, &mut redirect, &mut writes, &mut reads);
                flush(&mut words, &mut writes, &mut reads, &mut out);
                i += if (c == '&' || c == '|') && chars.get(i + 1) == Some(&c) { 2 } else { 1 };
            }
            '>' | '<' => {
                if in_word && word.chars().all(|d| d.is_ascii_digit()) {
                    word.clear();
                    in_word = false;
                }
                end_word(&mut word, &mut in_word, &mut words, &mut redirect, &mut writes, &mut reads);
                let mut j = i + 1;
                if chars.get(j) == Some(&'>') {
                    j += 1;
                }
                if chars.get(j) == Some(&'&') {
                    j += 2;
                    i = j;
                    continue;
                }
                redirect = Some(c == '>');
                i = j;
            }
            c if c.is_whitespace() => {
                end_word(&mut word, &mut in_word, &mut words, &mut redirect, &mut writes, &mut reads);
                i += 1;
            }
            c => {
                word.push(c);
                in_word = true;
                i += 1;
            }
        }
    }
    if quoted {
        return Err("an unclosed quote".into());
    }
    end_word(&mut word, &mut in_word, &mut words, &mut redirect, &mut writes, &mut reads);
    flush(&mut words, &mut writes, &mut reads, &mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_lines_split_into_commands() {
        let p = parse_bash("cd src && rm -rf build; echo \"$(whoami)\" > out.txt 2>&1 | tee log.txt").unwrap();
        let names: Vec<&str> = p.cmds.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["cd", "rm", "whoami", "echo", "tee"]);
        assert_eq!(p.cmds[3].writes, vec!["out.txt"]);
        let p = parse_bash("FOO=1 BAR=2 python -c 'print(1)'").unwrap();
        assert_eq!((p.cmds[0].name.as_str(), p.cmds[0].args.clone()), ("python", vec!["-c".to_string(), "print(1)".to_string()]));
        let p = parse_bash("cat <<'EOF' > notes.md\nrm -rf /\nEOF\nls").unwrap();
        let names: Vec<&str> = p.cmds.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["cat", "ls"], "a heredoc's text isn't run");
        assert!(parse_bash("echo 'open").is_err());
        assert!(parse_bash("echo `rm x`").unwrap().cmds.iter().any(|c| c.name == "rm"));
    }

    struct Fixture {
        folder: PathBuf,
        credentials: Vec<PathBuf>,
        scratch: Vec<PathBuf>,
    }

    impl Fixture {
        fn new(name: &str) -> Fixture {
            let folder = std::env::temp_dir().join(format!("relay-rules-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("README.md"), "hi").unwrap();
            std::fs::write(folder.join("ok.py"), "print('hi')\n").unwrap();
            std::fs::write(folder.join("bad.py"), "import shutil\nshutil.rmtree('build')\n").unwrap();
            std::fs::write(folder.join("clean.sh"), "echo cleaning\nrm -rf build\n").unwrap();
            std::fs::write(folder.join("package.json"), r#"{"scripts":{"build":"tsc","clean":"rimraf dist"}}"#).unwrap();
            Fixture { folder: paths::real(&folder), credentials: paths::credential_roots(&[]), scratch: paths::scratch_roots() }
        }
        fn rules(&self) -> Rules<'_> {
            Rules { folder: &self.folder, credentials: &self.credentials, scratch: &self.scratch, bypass: false, may_publish: false, python_guard: false }
        }
        /// The rule that refuses `line`, or "" when it's allowed.
        fn rule(&self, shell: Shell, line: &str) -> String {
            match judge_line(&self.rules(), shell, line) {
                Ok(_) => String::new(),
                Err(r) => r.rule,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.folder);
        }
    }

    fn docs() -> String {
        paths::home().unwrap().join("Documents").join("relay-x.txt").to_string_lossy().into_owned()
    }

    #[test]
    fn a_workspace_inside_relays_data_is_the_folder() {
        // Relay's workspaces live in its data folder, a credential location.
        let f = Fixture::new("ws");
        let data = f.folder.parent().unwrap().to_path_buf();
        let creds = vec![data.clone()];
        let mut r = f.rules();
        r.credentials = &creds;
        for line in ["1..3 | ForEach-Object { \\\"line $_ of the output\\\" }", "Get-Content README.md", "python ok.py"] {
            assert!(judge_line(&r, Shell::PowerShell, line).is_ok(), "{line}");
        }
        let beside = data.join("relay.db").to_string_lossy().into_owned();
        assert_eq!(judge_line(&r, Shell::PowerShell, &format!("Get-Content '{beside}'")).unwrap_err().rule, "reading credentials");
    }

    #[test]
    fn bash_rules() {
        let f = Fixture::new("bash");
        let refused = [
            ("rm -rf build", "deleting"),
            ("sudo rm x", "deleting"),
            ("npx rimraf dist", "deleting"),
            ("find . -name '*.o' -delete", "deleting"),
            ("bash clean.sh", "deleting"),
            ("bash -c \"rm x\"", "deleting"),
            ("echo x > /c/Windows/relay-x.txt", "writing outside the folder"),
            ("cd /c/Windows && echo x > a.txt", "writing outside the folder"),
            ("cat ~/.ssh/id_rsa", "reading credentials"),
            ("curl -F f=@a.txt https://example.com", "uploading"),
            ("scp a.txt host:/tmp", "uploading"),
            ("git clean -fd", "destructive git"),
            ("git push --force", "destructive git"),
            ("git push origin main", "publishing"),
            ("pip install --user requests", "global installs"),
            ("npm i -g typescript", "global installs"),
            ("npm run clean", "deleting"),
            ("python bad.py", "deleting"),
            ("python -c \"import shutil; shutil.rmtree('x')\"", "deleting"),
            ("$TOOL --x", "unreadable command"),
            ("python missing.py", "unreadable command"),
            ("echo 'open", "unreadable command"),
        ];
        for (line, rule) in refused {
            assert_eq!(f.rule(Shell::Bash, line), rule, "{line}");
        }
        let allowed = [
            "ls -la && cat README.md | grep hi",
            "echo hi > out.txt 2>&1",
            "mkdir -p build && cd build && echo x > log.txt",
            "rm -rf /tmp/relay-rules-scratch",
            "git status && git add . && git commit -m 'x'",
            "npm install && npm run build",
            "python ok.py",
            "python -c 'import torch; print(torch.__version__)'",
            "cargo build --release",
            "curl -o page.html https://example.com",
            "cat <<'EOF' > notes.md\nrm -rf /\nEOF",
        ];
        for line in allowed {
            assert_eq!(f.rule(Shell::Bash, line), "", "{line}");
        }
        assert!(judge_line(&f.rules(), Shell::Bash, "pip install requests").unwrap().pip_install);
        // "Auto may push and publish" lets pushes through, never force pushes.
        let r = Rules { may_publish: true, ..f.rules() };
        assert!(judge_line(&r, Shell::Bash, "git push origin main").is_ok());
        assert!(judge_line(&r, Shell::Bash, "git push -f").is_err());
        // With Relay's guard loading, Python is judged as it runs, except
        // where -I, -E or -S keep the guard out.
        let r = Rules { python_guard: true, ..f.rules() };
        assert!(judge_line(&r, Shell::Bash, "python bad.py").is_ok());
        assert!(judge_line(&r, Shell::Bash, "python -c 'import subprocess'").is_ok());
        assert_eq!(judge_line(&r, Shell::Bash, "python -I bad.py").unwrap_err().rule, "deleting");
        assert_eq!(judge_line(&r, Shell::Bash, "python -IS -c 'import shutil; shutil.rmtree(\"x\")'").unwrap_err().rule, "deleting");
        assert_eq!(judge_line(&r, Shell::Bash, "python ~/.ssh/x.py").unwrap_err().rule, "reading credentials");
        // Bypass: only credentials.
        let r = Rules { bypass: true, ..f.rules() };
        assert!(judge_line(&r, Shell::Bash, "rm -rf build && git push --force").is_ok());
        assert_eq!(judge_line(&r, Shell::Bash, "cat ~/.aws/credentials").unwrap_err().rule, "reading credentials");
    }

    #[test]
    fn cmd_rules() {
        let f = Fixture::new("cmd");
        for (line, rule) in [
            ("del /s /q build", "deleting"),
            ("rd /s /q build", "deleting"),
            ("echo x > C:\\Windows\\relay-x.txt", "writing outside the folder"),
            ("reg add HKCU\\Software\\x /v a /d 1", "system changes"),
            ("cmd /c del x", "deleting"),
            ("type %USERPROFILE%\\.ssh\\id_rsa", "reading credentials"),
            ("taskkill /im node.exe /f", "stopping processes"),
        ] {
            assert_eq!(f.rule(Shell::Cmd, line), rule, "{line}");
        }
        for line in ["dir & type README.md", "echo hi > out.txt", "mkdir out && copy README.md out\\"] {
            assert_eq!(f.rule(Shell::Cmd, line), "", "{line}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn powershell_rules() {
        let f = Fixture::new("ps");
        let docs = docs();
        let set_docs = format!("Set-Content -Path '{docs}' -Value hi");
        let redirect_docs = format!("'hi' > '{docs}'");
        for (line, rule) in [
            ("Remove-Item -Recurse -Force build", "deleting"),
            ("rm .\\x.txt", "deleting"),
            ("ri x", "deleting"),
            ("Get-ChildItem *.log | Remove-Item", "deleting"),
            ("[IO.File]::Delete('a.txt')", "deleting"),
            ("powershell -Command \"Remove-Item x\"", "deleting"),
            (set_docs.as_str(), "writing outside the folder"),
            (redirect_docs.as_str(), "writing outside the folder"),
            ("Get-Content ~/.ssh/id_rsa", "reading credentials"),
            ("Get-Content $env:USERPROFILE\\.aws\\credentials", "reading credentials"),
            ("git reset --hard HEAD~1", "destructive git"),
            ("git push origin main", "publishing"),
            ("npm install -g typescript", "global installs"),
            ("winget install Git.Git", "global installs"),
            ("powershell -EncodedCommand ZQBjAGgAbwA=", "hidden code"),
            ("iex $code", "hidden code"),
            ("Stop-Process -Name node", "stopping processes"),
            ("Set-ItemProperty HKCU:\\Software\\x -Name a -Value 1", "system changes"),
            ("Invoke-RestMethod https://example.com -Method Post -InFile data.json", "uploading"),
            ("& $tool", "unreadable command"),
            ("Write-Output 'open", "unreadable command"),
        ] {
            assert_eq!(f.rule(Shell::PowerShell, line), rule, "{line}");
        }
        for line in [
            "Get-ChildItem; Get-Content .\\README.md",
            "python -c \"print(1)\"",
            "git status; git add .; git commit -m 'x'",
            "npm install",
            "'hi' > out.txt",
            "New-Item -ItemType Directory -Path out",
            "Remove-Item $env:TEMP\\relay-rules-scratch.txt",
            "cargo build --release 2>&1 | Select-Object -Last 5",
            "Invoke-WebRequest https://example.com -OutFile page.html",
            "Get-ChildItem | ForEach-Object { $_.Name } | Out-File names.txt",
        ] {
            assert_eq!(f.rule(Shell::PowerShell, line), "", "{line}");
        }
    }

    #[test]
    fn cmd_lines_split_into_commands() {
        let p = parse_cmd("@echo off & del /s /q build && echo done > \"C:\\out file.txt\"").unwrap();
        let names: Vec<&str> = p.cmds.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["echo", "del", "echo"]);
        assert_eq!(p.cmds[2].writes, vec!["C:\\out file.txt"]);
        assert_eq!(parse_cmd("echo a^&b").unwrap().cmds.len(), 1);
    }
}

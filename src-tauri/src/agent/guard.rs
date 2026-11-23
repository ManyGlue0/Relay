//! The Python guard (Auto mode): `pyguard/relay_guard.py` installs an audit
//! hook that applies the path map while Python runs, and asks Relay before a
//! new process starts. Commands in Auto get `PYTHONPATH` pointing at Relay's
//! `sitecustomize.py` (which loads the guard, then the one it shadows) and
//! `RELAY_GUARD` (the session's zones, the judge's port, the session's
//! token). The guard's folder is in Relay's data folder, a credential
//! location: commands can't read or change it.
//!
//! A probe checks, once per interpreter, that the guard really loads
//! (embeddable Pythons with a `._pth` file ignore `PYTHONPATH`); where it
//! doesn't, Python code is only read before it runs.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

use super::rules::{self, Rules};
use super::shell::Shell;
use super::Ctx;
use crate::Core;

const GUARD_PY: &str = include_str!("../../pyguard/relay_guard.py");
const SITE_PY: &str = include_str!("../../pyguard/sitecustomize.py");

/// Where the guard's files are written.
pub fn dir(core: &Core) -> PathBuf {
    core.data_dir.join("pyguard")
}

/// Writes the guard's files (when missing or changed).
fn write_files(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for (name, text) in [("relay_guard.py", GUARD_PY), ("sitecustomize.py", SITE_PY)] {
        let p = dir.join(name);
        if std::fs::read_to_string(&p).ok().as_deref() != Some(text) {
            std::fs::write(&p, text)?;
        }
    }
    Ok(())
}

struct Session {
    chat_id: String,
    folder: PathBuf,
    credentials: Vec<PathBuf>,
    scratch: Vec<PathBuf>,
    may_publish: bool,
}

pub struct Guard {
    port: u16,
    /// token → the session it belongs to.
    sessions: Mutex<HashMap<String, Session>>,
    /// chat id → its token.
    tokens: Mutex<HashMap<String, String>>,
    /// interpreter → whether the guard loads in it.
    probed: Mutex<HashMap<PathBuf, bool>>,
}

static GUARD: OnceLock<Option<Guard>> = OnceLock::new();
static CORE: OnceLock<Arc<Core>> = OnceLock::new();

/// The guard's judge, started on first use. None when it couldn't start
/// (then Python is only read before it runs).
pub fn global(core: &Arc<Core>) -> Option<&'static Guard> {
    let _ = CORE.set(core.clone());
    GUARD
        .get_or_init(|| {
            write_files(&dir(core)).ok()?;
            let listener = TcpListener::bind("127.0.0.1:0").ok()?;
            let port = listener.local_addr().ok()?.port();
            std::thread::spawn(move || {
                for conn in listener.incoming().flatten() {
                    std::thread::spawn(move || serve(conn));
                }
            });
            Some(Guard { port, sessions: Mutex::new(HashMap::new()), tokens: Mutex::new(HashMap::new()), probed: Mutex::new(HashMap::new()) })
        })
        .as_ref()
}

fn serve(conn: TcpStream) {
    let _ = conn.set_read_timeout(Some(Duration::from_secs(10)));
    let mut line = Vec::new();
    if BufReader::new(&conn).take(1 << 20).read_until(b'\n', &mut line).is_err() {
        return;
    }
    let answer = judge(&String::from_utf8_lossy(&line));
    let mut conn = conn;
    let _ = conn.write_all(format!("{answer}\n").as_bytes());
}

/// One question from a guarded Python: may this process start?
fn judge(request: &str) -> Value {
    let Ok(req) = serde_json::from_str::<Value>(request.trim()) else {
        return json!({ "ok": false, "text": "Relay couldn't read the guard's question." });
    };
    let (Some(guard), Some(core)) = (GUARD.get().and_then(Option::as_ref), CORE.get()) else {
        return json!({ "ok": false, "text": "Relay's guard isn't running." });
    };
    let token = req["token"].as_str().unwrap_or("");
    let command = req["command"].as_str().unwrap_or("").to_string();
    let sessions = guard.sessions.lock().unwrap_or_else(|e| e.into_inner());
    let Some(s) = sessions.get(token) else {
        return json!({ "ok": false, "text": "Blocked by Relay's Auto mode: this Python's session has ended." });
    };
    let r = Rules { folder: &s.folder, credentials: &s.credentials, scratch: &s.scratch, bypass: false, may_publish: s.may_publish, python_guard: true };
    let cwd = req["cwd"].as_str().map(|c| super::paths::resolve(&s.folder, c)).unwrap_or_else(|| s.folder.clone());
    match rules::judge_line_in(&r, Shell::Cmd, &command, &cwd) {
        Ok(_) => json!({ "ok": true }),
        Err(refusal) => {
            let chat = s.chat_id.clone();
            drop(sessions);
            let entry = json!({
                "tool": "cmd",
                "args": { "command": command },
                "rule": refusal.rule,
                "why": refusal.why,
                "mode": "auto",
                "at": crate::db::now_ms(),
                "messageId": "",
                "callId": "",
                "ran": false,
                "from": "python",
            });
            let _ = core.db.update_blocked(&chat, |l| l.push(entry));
            json!({ "ok": false, "text": format!("{}\n(Raised by Relay's guard when Python tried to start a process.)", refusal.text("auto")) })
        }
    }
}

impl Guard {
    /// The session's token, with its zones brought up to date.
    fn register(&self, ctx: &Ctx, may_publish: bool) -> String {
        let token = self.tokens.lock().unwrap_or_else(|e| e.into_inner()).entry(ctx.chat_id.clone()).or_insert_with(|| format!("{}{}", crate::db::new_id(), crate::db::new_id())).clone();
        self.sessions.lock().unwrap_or_else(|e| e.into_inner()).insert(
            token.clone(),
            Session { chat_id: ctx.chat_id.clone(), folder: ctx.folder.clone(), credentials: ctx.credentials.clone(), scratch: ctx.scratch.clone(), may_publish },
        );
        token
    }

    /// The variables that load the guard in a command's Pythons.
    pub fn env(&self, core: &Core, ctx: &Ctx, may_publish: bool) -> super::shell::Env {
        let token = self.register(ctx, may_publish);
        let cfg = json!({
            "folder": ctx.folder,
            "credentials": ctx.credentials,
            "scratch": ctx.scratch,
            "port": self.port,
            "token": token,
        });
        let dir = dir(core);
        let pythonpath = match std::env::var("PYTHONPATH") {
            Ok(p) if !p.is_empty() => format!("{};{p}", dir.display()),
            _ => dir.display().to_string(),
        };
        vec![("PYTHONPATH".into(), Some(pythonpath)), ("RELAY_GUARD".into(), Some(cfg.to_string()))]
    }

    /// Whether the guard loads in the Python a command would find first on
    /// `path` (asked once per interpreter; no Python: nothing to guard).
    pub fn loads(&self, core: &Core, ctx: &Ctx, path: &str) -> bool {
        let Some(exe) = find_python(path) else { return true };
        if let Some(known) = self.probed.lock().unwrap_or_else(|e| e.into_inner()).get(&exe) {
            return *known;
        }
        let env = self.env(core, ctx, false);
        let ok = probe(&exe, &env, &ctx.folder);
        self.probed.lock().unwrap_or_else(|e| e.into_inner()).insert(exe, ok);
        ok
    }

    /// Ends a deleted session's token.
    pub fn forget(&self, chat_id: &str) {
        if let Some(t) = self.tokens.lock().unwrap_or_else(|e| e.into_inner()).remove(chat_id) {
            self.sessions.lock().unwrap_or_else(|e| e.into_inner()).remove(&t);
        }
    }
}

/// The python.exe a command would run (Microsoft Store aliases skipped: they
/// can open the Store).
pub fn find_python(path: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|d| !d.to_string_lossy().to_lowercase().contains("windowsapps"))
        .map(|d| d.join("python.exe"))
        .find(|f| f.is_file())
}

fn probe(exe: &Path, env: &super::shell::Env, cwd: &Path) -> bool {
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["-c", "import sys; sys.stdout.write('yes' if 'relay_guard' in sys.modules else 'no')"])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    for (k, v) in env {
        if let Some(v) = v {
            cmd.env(k, v);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let Ok(mut child) = cmd.spawn() else { return false };
    let t0 = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if t0.elapsed() > Duration::from_secs(20) => {
                let _ = child.kill();
                return false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return false,
        }
    }
    let mut out = String::new();
    if let Some(mut so) = child.stdout.take() {
        let _ = so.read_to_string(&mut out);
    }
    out.trim() == "yes"
}

/// A deleted session's Pythons can't ask anymore.
pub fn forget_session(chat_id: &str) {
    if let Some(Some(g)) = GUARD.get() {
        g.forget(chat_id);
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// Needs a Python on PATH (skipped without one).
    #[test]
    fn the_guard_judges_python_as_it_runs() {
        let Some(python) = find_python(&std::env::var("PATH").unwrap_or_default()) else { return };
        let tmp = std::env::temp_dir().join(format!("relay-guard-test-{}", std::process::id()));
        let (dir, work) = (tmp.join("guard"), tmp.join("work"));
        write_files(&dir).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        // A stand-in for Relay's judge: only "echo" may start.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut c in listener.incoming().flatten() {
                let mut line = String::new();
                let _ = BufReader::new(&c).read_line(&mut line);
                let ok = line.contains("echo");
                let _ = c.write_all(format!("{}\n", json!({ "ok": ok, "text": "refused by the test" })).as_bytes());
            }
        });
        let home = super::super::paths::home().unwrap();
        let cfg = json!({ "folder": work, "credentials": [home.join(".ssh")], "scratch": [std::env::temp_dir().join("relay-guard-scratch")], "port": port, "token": "t" });
        let env: crate::agent::shell::Env = vec![("PYTHONPATH".into(), Some(dir.display().to_string())), ("RELAY_GUARD".into(), Some(cfg.to_string()))];
        assert!(probe(&python, &env, &work), "the guard loads");
        let code = r#"
import os, subprocess
def t(f):
    try:
        f(); return "ok"
    except PermissionError:
        return "blocked"
open("in.txt", "w").write("x")
docs = os.path.join(os.path.expanduser("~"), "Documents", "relay-guard-test.txt")
print(t(lambda: open(docs, "w").write("x")), t(lambda: os.remove("in.txt")), t(lambda: open(os.path.expanduser("~/.ssh/config")).read()),
      t(lambda: subprocess.run("cmd /c echo hi", capture_output=True)), t(lambda: os.system("del in.txt")))
"#;
        let mut cmd = std::process::Command::new(&python);
        cmd.args(["-c", code]).current_dir(&work);
        for (k, v) in &env {
            cmd.env(k, v.as_deref().unwrap());
        }
        let out = cmd.output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(text.trim(), "blocked blocked blocked ok blocked", "{}", String::from_utf8_lossy(&out.stderr));
        assert!(work.join("in.txt").is_file(), "the delete didn't happen");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

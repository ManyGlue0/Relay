//! Shell tools: PowerShell (always), Git Bash (when installed), cmd.exe (a
//! setting). Commands run in the session's folder, without a window, each in
//! a job object of its own (closing it ends the command and everything it
//! started), with a time limit. Output is kept capped and streamed to the
//! card while the command runs. `background: true` keeps a command running
//! (a dev server, a watcher) as one of the session's processes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

/// Output a command keeps (the last part, like a terminal's scrollback).
const KEEP: usize = 1024 * 1024;
/// Output the model gets at most (the start and the end).
const FOR_MODEL: usize = 30_000;
/// Running background processes a session may have.
pub const MAX_BACKGROUND: usize = 5;
pub const DEFAULT_TIMEOUT: u64 = 120;
pub const MAX_TIMEOUT: u64 = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    PowerShell,
    Bash,
    Cmd,
}

impl Shell {
    pub fn of_tool(tool: &str) -> Option<Shell> {
        match tool {
            "powershell" => Some(Shell::PowerShell),
            "bash" => Some(Shell::Bash),
            "cmd" => Some(Shell::Cmd),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Shell::PowerShell => "PowerShell",
            Shell::Bash => "Bash",
            Shell::Cmd => "cmd",
        }
    }
}

fn on_path(exe: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(exe)).find(|f| f.is_file()))
}

/// PowerShell 7 (`pwsh`) when installed, else Windows PowerShell.
pub fn powershell() -> (PathBuf, bool) {
    static FOUND: OnceLock<(PathBuf, bool)> = OnceLock::new();
    FOUND
        .get_or_init(|| match on_path("pwsh.exe") {
            Some(p) => (p, true),
            None => {
                let sys = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
                (sys.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"), false)
            }
        })
        .clone()
}

/// Git for Windows' bash (never WSL's `System32\bash.exe`).
pub fn git_bash() -> Option<PathBuf> {
    static FOUND: OnceLock<Option<PathBuf>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            let mut candidates: Vec<PathBuf> = Vec::new();
            if let Some(git) = on_path("git.exe") {
                if let Some(root) = git.parent().and_then(Path::parent) {
                    candidates.push(root.join(r"bin\bash.exe"));
                }
            }
            for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
                if let Some(p) = std::env::var_os(var) {
                    candidates.push(PathBuf::from(p).join(r"Git\bin\bash.exe"));
                }
            }
            if let Some(l) = std::env::var_os("LOCALAPPDATA") {
                candidates.push(PathBuf::from(l).join(r"Programs\Git\bin\bash.exe"));
            }
            candidates.into_iter().find(|p| p.is_file())
        })
        .clone()
}

/// Removes terminal colors and cursor moves, and keeps what a line ends as
/// after carriage returns (progress bars).
pub fn clean(text: &str) -> String {
    static ANSI: OnceLock<regex::Regex> = OnceLock::new();
    let re = ANSI.get_or_init(|| regex::Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[()][A-Za-z0-9]|\x1b[=>]").unwrap());
    let stripped = re.replace_all(text, "");
    stripped
        .split('\n')
        .map(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            line.rsplit('\r').next().unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The last `KEEP` bytes of a command's output.
#[derive(Default)]
pub struct Output {
    text: String,
    /// Bytes dropped from the start.
    dropped: usize,
    /// A UTF-8 sequence split across reads.
    pending: Vec<u8>,
}

impl Output {
    fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        let valid = match std::str::from_utf8(&self.pending) {
            Ok(_) => self.pending.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => self.pending.len(),
        };
        let chunk: Vec<u8> = self.pending.drain(..valid).collect();
        self.text.push_str(&String::from_utf8_lossy(&chunk));
        if self.text.len() > KEEP {
            let mut cut = self.text.len() - KEEP;
            while !self.text.is_char_boundary(cut) {
                cut += 1;
            }
            self.text.drain(..cut);
            self.dropped += cut;
        }
    }

    /// The end of the output, cleaned, at most `max` characters.
    pub fn tail(&self, max: usize) -> String {
        let t = clean(&self.text);
        let n = t.chars().count();
        if n <= max {
            return t;
        }
        t.chars().skip(n - max).collect()
    }

    /// For the model: the start and the end when it's long.
    pub fn for_model(&self) -> String {
        let t = clean(&self.text);
        let n = t.chars().count();
        let head = if self.dropped > 0 { format!("[the first {} bytes of output were dropped]\n", self.dropped) } else { String::new() };
        if n <= FOR_MODEL {
            return format!("{head}{t}");
        }
        let start: String = t.chars().take(FOR_MODEL / 3).collect();
        let end: String = t.chars().skip(n - FOR_MODEL * 2 / 3).collect();
        format!("{head}{start}\n[… {} characters cut …]\n{end}", n - FOR_MODEL)
    }
}

/// A job object that ends the processes in it when dropped.
pub struct Job(isize);

impl Job {
    #[cfg(windows)]
    fn new() -> Option<Job> {
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info as *const _ as *const _, std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32);
            Some(Job(job as isize))
        }
    }
    #[cfg(windows)]
    fn adopt(&self, process: isize) {
        unsafe {
            windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(self.0 as _, process as _);
        }
    }
    #[cfg(not(windows))]
    fn new() -> Option<Job> {
        None
    }
    #[cfg(not(windows))]
    fn adopt(&self, _: isize) {}
}

impl Drop for Job {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0 as _);
        }
    }
}

// SAFETY: a job handle is a kernel handle, usable from any thread.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

fn command_for(shell: Shell, command: &str) -> Result<tokio::process::Command, String> {
    let mut cmd = match shell {
        Shell::PowerShell => {
            use base64::Engine;
            let (exe, _) = powershell();
            // UTF-8 out (without a BOM: Windows PowerShell's [Encoding]::UTF8
            // puts one before text piped into programs), no progress bars; the
            // exit code of the last native command, or 1 when a cmdlet failed.
            let script = format!(
                "$ProgressPreference = 'SilentlyContinue'\n$OutputEncoding = [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false\n{command}\nif ($LASTEXITCODE) {{ exit $LASTEXITCODE }} elseif (-not $?) {{ exit 1 }}"
            );
            let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
            let mut c = tokio::process::Command::new(exe);
            c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand"]);
            c.arg(base64::engine::general_purpose::STANDARD.encode(utf16));
            c
        }
        Shell::Bash => {
            let exe = git_bash().ok_or("Git Bash isn't installed")?;
            let mut c = tokio::process::Command::new(exe);
            c.args(["-c", command]);
            c.env("CHERE_INVOKING", "1");
            c
        }
        Shell::Cmd => {
            let sys = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
            let mut c = tokio::process::Command::new(sys.join(r"System32\cmd.exe"));
            #[cfg(windows)]
            {
                // cmd reads its own line: passed as it is, inside /s /c "…".
                c.raw_arg(format!("/d /s /c \"{command}\""));
            }
            #[cfg(not(windows))]
            c.args(["/c", command]);
            c
        }
    };
    cmd.env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUNBUFFERED", "1")
        .env("PYTHONUTF8", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("NO_COLOR", "1")
        .env("RELAY_AGENT", "1");
    Ok(cmd)
}

/// Changes to a command's environment: a value to set, or None to remove.
pub type Env = Vec<(String, Option<String>)>;

/// A command started: its output as it grows, its job, its process.
pub struct Running {
    pub output: Arc<Mutex<Output>>,
    pub job: Option<Job>,
    pub child: tokio::process::Child,
    pub started: Instant,
}

pub fn start(shell: Shell, command: &str, cwd: &Path, env: &Env) -> Result<Running, String> {
    let mut cmd = command_for(shell, command)?;
    cmd.current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in env {
        match v {
            Some(v) => {
                cmd.env(k, v);
            }
            None => {
                cmd.env_remove(k);
            }
        }
    }
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = cmd.spawn().map_err(|e| format!("Couldn't start {}: {e}", shell.name()))?;
    let job = Job::new();
    #[cfg(windows)]
    if let (Some(j), Some(h)) = (&job, child.raw_handle()) {
        j.adopt(h as isize);
    }
    let output = Arc::new(Mutex::new(Output::default()));
    for pipe in [child.stdout.take().map(|p| Box::new(p) as Box<dyn tokio::io::AsyncRead + Unpin + Send>), child.stderr.take().map(|p| Box::new(p) as Box<dyn tokio::io::AsyncRead + Unpin + Send>)].into_iter().flatten() {
        let out = output.clone();
        tokio::spawn(async move {
            let mut pipe = pipe;
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                match pipe.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => out.lock().unwrap().push(&buf[..n]),
                }
            }
        });
    }
    Ok(Running { output, job, child, started: Instant::now() })
}

/// How a command that ran to the end (or didn't) went.
pub struct Finished {
    pub code: Option<i32>,
    pub timed_out: bool,
    pub secs: f64,
}

/// Waits for a command, calling `tick` with its output's end about every
/// 300 ms. Stops it (its whole process tree) on time-out or cancel.
pub async fn wait(mut run: Running, timeout: Duration, token: &CancellationToken, mut tick: impl FnMut(&str)) -> (Finished, Arc<Mutex<Output>>, bool) {
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut every = tokio::time::interval(Duration::from_millis(300));
    let mut last = String::new();
    let (code, timed_out, cancelled) = loop {
        tokio::select! {
            status = run.child.wait() => break (status.ok().and_then(|s| s.code()), false, false),
            _ = &mut deadline => break (None, true, false),
            _ = token.cancelled() => break (None, false, true),
            _ = every.tick() => {
                let now = run.output.lock().unwrap().tail(16_000);
                if now != last {
                    tick(&now);
                    last = now;
                }
            }
        }
    };
    // Ends the command and anything it left running.
    drop(run.job.take());
    let _ = run.child.start_kill();
    // The pipes' last bytes.
    tokio::time::sleep(Duration::from_millis(60)).await;
    let secs = run.started.elapsed().as_secs_f64();
    (Finished { code, timed_out, secs }, run.output.clone(), cancelled)
}

/// What the model reads after a command.
pub fn report(f: &Finished, out: &Output, timeout: Duration) -> (String, bool) {
    let body = out.for_model();
    let head = if f.timed_out {
        format!("Stopped after {} s (the time limit). Pass a larger timeout (up to {MAX_TIMEOUT}), or run it with background: true.", timeout.as_secs())
    } else {
        format!("Exit code {} after {:.1} s.", f.code.map(|c| c.to_string()).unwrap_or_else(|| "unknown".into()), f.secs)
    };
    let text = if body.trim().is_empty() { format!("{head} No output.") } else { format!("{head}\n{}", body.trim_end()) };
    (text, f.timed_out || f.code != Some(0))
}

// ---- background processes ---------------------------------------------------------

pub struct Process {
    pub id: u32,
    pub command: String,
    pub shell: Shell,
    pub started: Instant,
    pub started_ms: i64,
    pub output: Arc<Mutex<Output>>,
    pub state: Arc<Mutex<Option<Option<i32>>>>,
    job: Option<Job>,
}

#[derive(Default)]
pub struct Processes {
    by_session: Mutex<HashMap<String, Vec<Process>>>,
    next: Mutex<u32>,
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProcessView {
    pub id: u32,
    pub command: String,
    pub shell: String,
    pub started: i64,
    pub running: bool,
    pub code: Option<i32>,
}

impl Processes {
    pub fn global() -> &'static Processes {
        static P: OnceLock<Processes> = OnceLock::new();
        P.get_or_init(Processes::default)
    }

    pub fn running(&self, session: &str) -> usize {
        self.by_session.lock().unwrap().get(session).map(|l| l.iter().filter(|p| p.state.lock().unwrap().is_none()).count()).unwrap_or(0)
    }

    /// Keeps a started command running in the background.
    pub fn adopt(&self, session: &str, shell: Shell, command: &str, mut run: Running) -> (u32, Arc<Mutex<Output>>) {
        let id = {
            let mut n = self.next.lock().unwrap();
            *n += 1;
            *n
        };
        let state: Arc<Mutex<Option<Option<i32>>>> = Arc::new(Mutex::new(None));
        let output = run.output.clone();
        let job = run.job.take();
        let st = state.clone();
        tokio::spawn(async move {
            let code = run.child.wait().await.ok().and_then(|s| s.code());
            *st.lock().unwrap() = Some(code);
        });
        let p = Process { id, command: command.to_string(), shell, started: run.started, started_ms: crate::db::now_ms(), output: output.clone(), state, job };
        self.by_session.lock().unwrap().entry(session.to_string()).or_default().push(p);
        (id, output)
    }

    pub fn list(&self, session: &str) -> Vec<ProcessView> {
        self.by_session
            .lock()
            .unwrap()
            .get(session)
            .map(|l| {
                l.iter()
                    .map(|p| {
                        let st = *p.state.lock().unwrap();
                        ProcessView { id: p.id, command: p.command.clone(), shell: p.shell.name().into(), started: p.started_ms, running: st.is_none(), code: st.flatten() }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn read(&self, session: &str, id: u32, max: usize) -> Option<(String, bool, Option<i32>, f64)> {
        let map = self.by_session.lock().unwrap();
        let p = map.get(session)?.iter().find(|p| p.id == id)?;
        let st = *p.state.lock().unwrap();
        let text = p.output.lock().unwrap().tail(max);
        Some((text, st.is_none(), st.flatten(), p.started.elapsed().as_secs_f64()))
    }

    /// Stops a process (and everything it started). False: no such process.
    pub fn stop(&self, session: &str, id: u32) -> bool {
        let mut map = self.by_session.lock().unwrap();
        let Some(list) = map.get_mut(session) else { return false };
        let Some(p) = list.iter_mut().find(|p| p.id == id) else { return false };
        drop(p.job.take());
        let mut st = p.state.lock().unwrap();
        if st.is_none() {
            *st = Some(None);
        }
        true
    }

    /// A deleted session's processes end.
    pub fn stop_all(&self, session: &str) {
        if let Some(list) = self.by_session.lock().unwrap().remove(session) {
            drop(list);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_cleaned_and_capped() {
        assert_eq!(clean("\x1b[32mok\x1b[0m\r\n50%\r100%\ndone"), "ok\n100%\ndone");
        let mut o = Output::default();
        o.push("é".as_bytes().split_at(1).0);
        o.push("é".as_bytes().split_at(1).1);
        assert_eq!(o.tail(10), "é");
        let mut big = Output::default();
        big.push(&vec![b'a'; KEEP + 100]);
        assert_eq!(big.dropped, 100);
        assert!(big.for_model().contains("characters cut"));
    }

    #[tokio::test]
    async fn powershell_runs_in_the_folder_and_reports() {
        let dir = std::env::temp_dir();
        let run = start(Shell::PowerShell, "Write-Output \"hi $((Get-Location).Path.Length -gt 0)\"; cmd /c exit 3", &dir, &Vec::new()).unwrap();
        let token = CancellationToken::new();
        let (f, out, _) = wait(run, Duration::from_secs(60), &token, |_| {}).await;
        let (text, failed) = report(&f, &out.lock().unwrap(), Duration::from_secs(60));
        assert!(text.contains("hi True"), "{text}");
        assert_eq!(f.code, Some(3), "{text}");
        assert!(failed);
    }

    #[tokio::test]
    async fn piping_into_programs_adds_no_bom() {
        let dir = std::env::temp_dir();
        let run = start(Shell::PowerShell, "'World' | findstr W", &dir, &Vec::new()).unwrap();
        let token = CancellationToken::new();
        let (_, out, _) = wait(run, Duration::from_secs(60), &token, |_| {}).await;
        let text = out.lock().unwrap().tail(100);
        assert_eq!(text.trim(), "World", "{text:?}");
    }

    #[tokio::test]
    async fn time_outs_stop_the_whole_tree() {
        let dir = std::env::temp_dir();
        let run = start(Shell::PowerShell, "Start-Sleep -Seconds 30", &dir, &Vec::new()).unwrap();
        let token = CancellationToken::new();
        let t0 = Instant::now();
        let (f, _, _) = wait(run, Duration::from_secs(2), &token, |_| {}).await;
        assert!(f.timed_out && t0.elapsed() < Duration::from_secs(10));
    }
}

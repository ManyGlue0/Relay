//! PowerShell command lines read by PowerShell's own parser, for Auto's rules.
//! One PowerShell process stays running (started on first use, with no window
//! and in its own kill-on-close job): it reads a base64 line, parses it with
//! `[Parser]::ParseInput`, and answers with one line of JSON: the commands
//! (aliases resolved), what they redirect into, and the .NET method calls.
//! Parsing never runs anything.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;

use super::rules::{base_name, Cmd, Member, Parsed};
use super::shell::{self, Job};

/// The longest command line read (a script file too).
pub const MAX_SOURCE: usize = 256 * 1024;

const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$utf8 = New-Object System.Text.UTF8Encoding $false
[Console]::OutputEncoding = $utf8
$aliases = @{}
foreach ($a in Get-Alias) { $aliases[$a.Name] = $a.Definition }
$L = 'System.Management.Automation.Language'
$TString = [type]"$L.StringConstantExpressionAst"
$TExpand = [type]"$L.ExpandableStringExpressionAst"
$TConst = [type]"$L.ConstantExpressionAst"
$TArray = [type]"$L.ArrayLiteralAst"
$TCmd = [type]"$L.CommandAst"
$TExpr = [type]"$L.CommandExpressionAst"
$TParam = [type]"$L.CommandParameterAst"
$TFile = [type]"$L.FileRedirectionAst"
$TMember = [type]"$L.InvokeMemberExpressionAst"
function Add-Value($list, $e) {
  if ($e -is $TArray) { foreach ($x in $e.Elements) { Add-Value $list $x }; return }
  if ($e -is $TString -or $e -is $TExpand) { [void]$list.Add([string]$e.Value); return }
  if ($e -is $TConst) { [void]$list.Add([string]$e.Value); return }
  [void]$list.Add($e.Extent.Text)
}
$reader = [Console]::In
while ($true) {
  $line = $reader.ReadLine()
  if ($null -eq $line) { break }
  try {
    $src = $utf8.GetString([Convert]::FromBase64String($line))
    $tokens = $null; $errs = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseInput($src, [ref]$tokens, [ref]$errs)
    if ($errs.Count -gt 0) {
      $res = @{ error = [string]$errs[0].Message }
    } else {
      $cmds = New-Object System.Collections.ArrayList
      $nodes = $ast.FindAll({ param($n) ($n -is $TCmd) -or (($n -is $TExpr) -and $n.Redirections.Count -gt 0) }, $true)
      foreach ($c in $nodes) {
        $writes = New-Object System.Collections.Generic.List[string]
        foreach ($r in $c.Redirections) { if ($r -is $TFile) { Add-Value $writes $r.Location } }
        if ($c -is $TExpr) {
          [void]$cmds.Add(@{ name = ''; raw = ''; argv = @(); writes = @($writes); dynamic = $false })
          continue
        }
        $first = $c.CommandElements[0]
        $dynamic = -not (($first -is $TString) -or ($first -is $TExpand))
        $raw = if ($dynamic) { $first.Extent.Text } else { [string]$first.Value }
        $name = $raw
        if (-not $dynamic -and $aliases.ContainsKey($raw)) { $name = $aliases[$raw] }
        $argv = New-Object System.Collections.Generic.List[string]
        for ($i = 1; $i -lt $c.CommandElements.Count; $i++) {
          $e = $c.CommandElements[$i]
          if ($e -is $TParam) {
            [void]$argv.Add('-' + $e.ParameterName)
            if ($e.Argument) { Add-Value $argv $e.Argument }
          } else { Add-Value $argv $e }
        }
        [void]$cmds.Add(@{ name = $name; raw = $raw; argv = @($argv); writes = @($writes); dynamic = $dynamic })
      }
      $members = New-Object System.Collections.ArrayList
      foreach ($m in $ast.FindAll({ param($n) $n -is $TMember }, $true)) {
        $member = if ($m.Member -is $TString) { [string]$m.Member.Value } else { $m.Member.Extent.Text }
        [void]$members.Add(@{ target = $m.Expression.Extent.Text; member = $member; text = $m.Extent.Text })
      }
      $res = @{ cmds = @($cmds); members = @($members) }
    }
  } catch {
    $res = @{ error = [string]$_ }
  }
  [Console]::Out.WriteLine((ConvertTo-Json -InputObject $res -Compress -Depth 6))
  [Console]::Out.Flush()
}
"#;

struct Proc {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    _job: Option<Job>,
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn spawn() -> Result<Proc, String> {
    use base64::Engine;
    let (exe, _) = shell::powershell();
    let utf16: Vec<u8> = SCRIPT.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let mut cmd = Command::new(exe);
    cmd.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand"])
        .arg(base64::engine::general_purpose::STANDARD.encode(utf16))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| format!("PowerShell's parser didn't start: {e}"))?;
    let job = Job::new();
    #[cfg(windows)]
    if let Some(j) = &job {
        use std::os::windows::io::AsRawHandle;
        j.adopt(child.as_raw_handle() as isize);
    }
    let stdin = child.stdin.take().ok_or("PowerShell's parser has no input")?;
    let stdout = child.stdout.take().ok_or("PowerShell's parser has no output")?;
    let (tx, lines) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut buf = Vec::new();
            // One answer per line; a line is capped (limits: outside data).
            match Read::take(&mut reader, crate::limits::MAX_JSON as u64).read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(String::from_utf8_lossy(&buf).trim().to_string()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    Ok(Proc { child, stdin, lines, _job: job })
}

static PROC: Mutex<Option<Proc>> = Mutex::new(None);

/// Parses a PowerShell command line (blocking: call it off the async threads).
/// Err: why it can't be read (a syntax error, or no parser).
pub fn parse(src: &str) -> Result<Parsed, String> {
    use base64::Engine;
    if src.len() > MAX_SOURCE {
        return Err("it's longer than 256 KB".into());
    }
    let mut guard = PROC.lock().unwrap_or_else(|e| e.into_inner());
    // Two tries: the process may have ended since the last use.
    for attempt in 0..2 {
        if guard.is_none() {
            *guard = Some(spawn()?);
        }
        let p = guard.as_mut().unwrap();
        let line = base64::engine::general_purpose::STANDARD.encode(src.as_bytes());
        // An answer left over from a timed-out request is dropped.
        while p.lines.try_recv().is_ok() {}
        if writeln!(p.stdin, "{line}").and_then(|_| p.stdin.flush()).is_err() {
            *guard = None;
            continue;
        }
        // The first answer waits for PowerShell to start.
        let wait = Duration::from_secs(if attempt == 0 { 20 } else { 30 });
        match p.lines.recv_timeout(wait) {
            Ok(answer) => return from_json(&answer),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                *guard = None;
                return Err("PowerShell's parser didn't answer".into());
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                *guard = None;
            }
        }
    }
    Err("PowerShell's parser stopped".into())
}

/// Ends the parser (when Relay quits).
pub fn stop() {
    *PROC.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// A list from JSON, or one item (older PowerShells write one-item lists as the item).
fn list(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::Null) | None => Vec::new(),
        Some(x) => vec![x.clone()],
    }
}

fn strings(v: Option<&Value>) -> Vec<String> {
    list(v).into_iter().map(|x| match x {
        Value::String(s) => s,
        other => other.to_string(),
    }).collect()
}

fn from_json(answer: &str) -> Result<Parsed, String> {
    let v: Value = serde_json::from_str(answer).map_err(|_| "PowerShell's parser gave an answer Relay couldn't read".to_string())?;
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        return Err(e.trim().to_string());
    }
    let mut out = Parsed::default();
    for c in list(v.get("cmds")) {
        let raw = c.get("raw").and_then(Value::as_str).unwrap_or("").to_string();
        let name = c.get("name").and_then(Value::as_str).unwrap_or("");
        let name = base_name(name);
        // "$tool" or "$(…)" as the name: computed when it runs.
        let dynamic = c.get("dynamic").and_then(Value::as_bool).unwrap_or(false) || raw.starts_with('$') || raw.contains("$(");
        out.cmds.push(Cmd { name, raw, args: strings(c.get("argv")), writes: strings(c.get("writes")), reads: Vec::new(), dynamic });
    }
    for m in list(v.get("members")) {
        let s = |k: &str| m.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        out.members.push(Member { target: s("target"), member: s("member"), text: s("text") });
    }
    Ok(out)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn powershell_lines_are_parsed() {
        let p = parse("rm -r -fo .\\build; Get-ChildItem | % { $_.Name } > list.txt; [IO.File]::Delete('x.txt')").unwrap();
        let names: Vec<&str> = p.cmds.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["remove-item", "get-childitem", "foreach-object"]);
        assert_eq!(p.cmds[0].args, vec!["-r", "-fo", ".\\build"]);
        assert_eq!(p.cmds[2].writes, vec!["list.txt"]);
        assert!(p.members.iter().any(|m| m.member == "Delete" && m.target == "[IO.File]"));
        let p = parse("& $tool -x").unwrap();
        assert!(p.cmds[0].dynamic);
        let p = parse("'one' > out.txt").unwrap();
        assert_eq!(p.cmds[0].writes, vec!["out.txt"]);
        assert!(parse("echo 'open").is_err(), "a syntax error can't be judged");
        let p = parse("Remove-Item -Path a,b").unwrap();
        assert_eq!(p.cmds[0].args, vec!["-Path", "a", "b"]);
    }
}

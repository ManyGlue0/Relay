//! A small Model Context Protocol client: tool servers over stdio (a child
//! process speaking newline-delimited JSON-RPC) or Streamable HTTP (JSON-RPC
//! POSTs answered with JSON or an event stream).
//!
//! Servers connect when Relay starts (and when enabled or edited). Child
//! processes join a job object that kills them when Relay exits, and start
//! without a console window.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

use crate::db::McpServer;

const PROTOCOL: &str = "2025-06-18";
const INIT_TIMEOUT: Duration = Duration::from_secs(45);
const CALL_TIMEOUT: Duration = Duration::from_secs(180);
/// Tool output beyond this is cut, so one call can't fill the context.
pub const MAX_RESULT: usize = 24_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    pub id: String,
    /// "off" | "connecting" | "ready" | "error"
    pub state: String,
    pub error: Option<String>,
    pub tools: Vec<ToolInfo>,
    pub server_name: Option<String>,
    pub server_version: Option<String>,
}

/// Secrets for a server: environment values (stdio) and a bearer token (HTTP).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Secrets {
    pub env: HashMap<String, String>,
    pub token: String,
}

pub fn load_secrets(id: &str) -> Secrets {
    crate::secrets::get(&format!("Relay/mcp/{id}")).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn save_secrets(id: &str, s: &Secrets) -> Result<(), String> {
    if s.env.is_empty() && s.token.is_empty() {
        return crate::secrets::delete(&format!("Relay/mcp/{id}"));
    }
    crate::secrets::set(&format!("Relay/mcp/{id}"), &serde_json::to_string(s).unwrap_or_default())
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

enum Transport {
    Stdio {
        stdin: tokio::sync::Mutex<tokio::process::ChildStdin>,
        child: Mutex<Option<tokio::process::Child>>,
        pending: Pending,
    },
    Http {
        url: String,
        token: String,
        session: Mutex<Option<String>>,
    },
}

struct Conn {
    transport: Transport,
    next_id: AtomicU64,
}

pub struct Mcp {
    app: AppHandle,
    conns: Mutex<HashMap<String, Arc<Conn>>>,
    status: Mutex<HashMap<String, ServerStatus>>,
    /// Bumped on every (re)connect, so a slow old attempt can't overwrite a newer one.
    generation: Mutex<HashMap<String, u64>>,
}

fn http() -> &'static reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).build().unwrap_or_default())
}

impl Mcp {
    pub fn new(app: AppHandle) -> Arc<Self> {
        Arc::new(Self { app, conns: Mutex::default(), status: Mutex::default(), generation: Mutex::default() })
    }

    fn set_status(&self, st: ServerStatus) {
        self.status.lock().unwrap().insert(st.id.clone(), st.clone());
        let _ = self.app.emit("mcp-status", &st);
    }

    pub fn statuses(&self) -> Vec<ServerStatus> {
        self.status.lock().unwrap().values().cloned().collect()
    }

    pub fn status_of(&self, id: &str) -> Option<ServerStatus> {
        self.status.lock().unwrap().get(id).cloned()
    }

    /// Connects (or reconnects) a server; disabled servers just disconnect.
    pub fn connect(self: &Arc<Self>, server: McpServer) {
        self.disconnect(&server.id);
        if !server.enabled {
            self.set_status(ServerStatus { id: server.id.clone(), state: "off".into(), ..Default::default() });
            return;
        }
        let gen = {
            let mut g = self.generation.lock().unwrap();
            let n = g.entry(server.id.clone()).or_insert(0);
            *n += 1;
            *n
        };
        self.set_status(ServerStatus { id: server.id.clone(), state: "connecting".into(), ..Default::default() });
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            let id = server.id.clone();
            let result = tokio::time::timeout(INIT_TIMEOUT, me.open(&server)).await.unwrap_or_else(|_| Err("The server didn't answer in time".into()));
            if me.generation.lock().unwrap().get(&id) != Some(&gen) {
                return; // a newer attempt took over
            }
            match result {
                Ok((conn, st)) => {
                    me.conns.lock().unwrap().insert(id, conn);
                    me.set_status(st);
                }
                Err(e) => me.set_status(ServerStatus { id, state: "error".into(), error: Some(e), ..Default::default() }),
            }
        });
    }

    pub fn disconnect(&self, id: &str) {
        if let Some(conn) = self.conns.lock().unwrap().remove(id) {
            conn.close();
        }
        self.generation.lock().unwrap().entry(id.to_string()).and_modify(|g| *g += 1);
    }

    pub fn forget(&self, id: &str) {
        self.disconnect(id);
        self.status.lock().unwrap().remove(id);
    }

    pub fn shutdown(&self) {
        for (_, conn) in self.conns.lock().unwrap().drain() {
            conn.close();
        }
    }

    async fn open(self: &Arc<Self>, server: &McpServer) -> Result<(Arc<Conn>, ServerStatus), String> {
        let secrets = load_secrets(&server.id);
        let conn = match server.kind.as_str() {
            "stdio" => self.spawn(server, &secrets)?,
            "http" => {
                let url = server.url.trim().to_string();
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    return Err("The address should start with http:// or https://".into());
                }
                Arc::new(Conn {
                    transport: Transport::Http { url, token: secrets.token.clone(), session: Mutex::new(None) },
                    next_id: AtomicU64::new(1),
                })
            }
            other => return Err(format!("Unknown server type “{other}”")),
        };
        let init = conn
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL,
                    "capabilities": {},
                    "clientInfo": { "name": "Relay", "version": env!("CARGO_PKG_VERSION") }
                }),
                INIT_TIMEOUT,
            )
            .await;
        let init = match init {
            Ok(v) => v,
            Err(e) => {
                conn.close();
                return Err(e);
            }
        };
        conn.notify("notifications/initialized", json!({})).await;
        let tools = match conn.list_tools().await {
            Ok(t) => t,
            Err(e) => {
                conn.close();
                return Err(e);
            }
        };
        let st = ServerStatus {
            id: server.id.clone(),
            state: "ready".into(),
            error: None,
            tools,
            server_name: init.pointer("/serverInfo/name").and_then(Value::as_str).map(str::to_string),
            server_version: init.pointer("/serverInfo/version").and_then(Value::as_str).map(str::to_string),
        };
        Ok((conn, st))
    }

    fn spawn(self: &Arc<Self>, server: &McpServer, secrets: &Secrets) -> Result<Arc<Conn>, String> {
        let command = server.command.trim();
        if command.is_empty() {
            return Err("Add the command that starts the server".into());
        }
        let mut cmd = command_for(command, &server.args);
        cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);
        if !server.cwd.trim().is_empty() {
            cmd.current_dir(server.cwd.trim());
        }
        for (k, v) in &secrets.env {
            cmd.env(k, v);
        }
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = cmd.spawn().map_err(|e| format!("Couldn't start “{command}”: {e}"))?;
        #[cfg(windows)]
        if let Some(h) = child.raw_handle() {
            job::adopt(h as isize);
        }
        let stdin = child.stdin.take().ok_or("No stdin")?;
        let stdout = child.stdout.take().ok_or("No stdout")?;
        let stderr = child.stderr.take().ok_or("No stderr")?;
        let pending: Pending = Arc::default();
        let conn = Arc::new(Conn {
            transport: Transport::Stdio { stdin: tokio::sync::Mutex::new(stdin), child: Mutex::new(Some(child)), pending: pending.clone() },
            next_id: AtomicU64::new(1),
        });

        // stderr: keep the tail, for a useful error if the server dies.
        let tail = Arc::new(Mutex::new(String::new()));
        let tail_w = tail.clone();
        tauri::async_runtime::spawn(async move {
            let mut stderr = BufReader::new(stderr);
            while let Ok(Some(line)) = crate::limits::next_line(&mut stderr, 64 * 1024).await {
                let mut t = tail_w.lock().unwrap();
                t.push_str(&line);
                t.push('\n');
                if t.len() > 4000 {
                    let cut = t.len() - 3000;
                    let cut = (cut..t.len()).find(|&i| t.is_char_boundary(i)).unwrap_or(t.len());
                    t.drain(..cut);
                }
            }
        });

        // stdout: responses, server requests and notifications.
        let (me, weak, id) = (self.clone(), Arc::downgrade(&conn), server.id.clone());
        tauri::async_runtime::spawn(async move {
            let mut stdout = BufReader::new(stdout);
            while let Ok(Some(line)) = crate::limits::next_line(&mut stdout, crate::limits::MAX_JSON).await {
                let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else { continue };
                let Some(conn) = weak.upgrade() else { break };
                conn.handle_incoming(&me, &id, msg).await;
            }
            // The process ended.
            for (_, tx) in pending.lock().unwrap().drain() {
                let _ = tx.send(Err("The server stopped".into()));
            }
            let still_ours = me.conns.lock().unwrap().get(&id).is_some_and(|c| weak.upgrade().is_some_and(|w| Arc::ptr_eq(c, &w)));
            if still_ours {
                me.conns.lock().unwrap().remove(&id);
                let t = tail.lock().unwrap().trim().to_string();
                let last = t.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();
                let error = if last.is_empty() { "The server stopped".to_string() } else { format!("The server stopped: {last}") };
                me.set_status(ServerStatus { id, state: "error".into(), error: Some(error), ..Default::default() });
            }
        });
        Ok(conn)
    }

    async fn refresh_tools(self: &Arc<Self>, id: &str) {
        let Some(conn) = self.conns.lock().unwrap().get(id).cloned() else { return };
        if let Ok(tools) = conn.list_tools().await {
            let mut st = self.status_of(id).unwrap_or_default();
            st.tools = tools;
            self.set_status(st);
        }
    }

    /// Calls a tool. Returns the text for the model and whether the tool said
    /// it failed.
    pub async fn call(&self, server_id: &str, tool: &str, arguments: Value) -> Result<(String, bool), String> {
        let conn = self.conns.lock().unwrap().get(server_id).cloned().ok_or("That tool server isn't connected")?;
        let result = conn.request("tools/call", json!({ "name": tool, "arguments": arguments }), CALL_TIMEOUT).await?;
        let is_error = result.get("isError").and_then(Value::as_bool).unwrap_or(false);
        Ok((result_text(&result), is_error))
    }
}

/// What a tool returned, as text for the model.
pub fn result_text(result: &Value) -> String {
    let mut out = Vec::new();
    for item in result.get("content").and_then(Value::as_array).into_iter().flatten() {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => out.push(item.get("text").and_then(Value::as_str).unwrap_or("").to_string()),
            Some("image") => out.push("[an image, not shown]".into()),
            Some("audio") => out.push("[audio, not shown]".into()),
            Some("resource") => {
                let r = item.get("resource");
                match r.and_then(|r| r.get("text")).and_then(Value::as_str) {
                    Some(t) => out.push(t.to_string()),
                    None => out.push(format!("[resource {}]", r.and_then(|r| r.get("uri")).and_then(Value::as_str).unwrap_or(""))),
                }
            }
            Some("resource_link") => {
                out.push(format!("[link {}]", item.get("uri").and_then(Value::as_str).unwrap_or("")));
            }
            _ => {}
        }
    }
    if out.is_empty() {
        if let Some(s) = result.get("structuredContent") {
            out.push(s.to_string());
        }
    }
    let text = out.join("\n");
    if text.chars().count() > MAX_RESULT {
        let cut: String = text.chars().take(MAX_RESULT).collect();
        format!("{cut}\n[… cut: the result was longer than {MAX_RESULT} characters]")
    } else {
        text
    }
}

/// `npx`, `uvx` and friends are batch files on Windows: run anything that
/// isn't plainly an .exe through cmd.
fn command_for(command: &str, args: &[String]) -> tokio::process::Command {
    let lower = command.to_lowercase();
    if cfg!(windows) && !lower.ends_with(".exe") {
        let mut c = tokio::process::Command::new("cmd.exe");
        c.arg("/d").arg("/c").arg(command).args(args);
        c
    } else {
        let mut c = tokio::process::Command::new(command);
        c.args(args);
        c
    }
}

impl Conn {
    fn close(&self) {
        if let Transport::Stdio { child, pending, .. } = &self.transport {
            if let Some(mut c) = child.lock().unwrap().take() {
                let _ = c.start_kill();
            }
            for (_, tx) in pending.lock().unwrap().drain() {
                let _ = tx.send(Err("The server was disconnected".into()));
            }
        }
    }

    async fn list_tools(&self) -> Result<Vec<ToolInfo>, String> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..50 {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let page = self.request("tools/list", params, INIT_TIMEOUT).await?;
            for t in page.get("tools").and_then(Value::as_array).into_iter().flatten() {
                if let Ok(tool) = serde_json::from_value::<ToolInfo>(t.clone()) {
                    tools.push(tool);
                }
            }
            cursor = page.get("nextCursor").and_then(Value::as_str).map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    async fn notify(&self, method: &str, params: Value) {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let _ = self.send_raw(&msg).await;
    }

    async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        match &self.transport {
            Transport::Stdio { pending, .. } => {
                let (tx, rx) = oneshot::channel();
                pending.lock().unwrap().insert(id, tx);
                if let Err(e) = self.send_raw(&msg).await {
                    pending.lock().unwrap().remove(&id);
                    return Err(e);
                }
                match tokio::time::timeout(timeout, rx).await {
                    Ok(Ok(r)) => r,
                    Ok(Err(_)) => Err("The server stopped".into()),
                    Err(_) => {
                        pending.lock().unwrap().remove(&id);
                        Err(format!("“{method}” took longer than {} s", timeout.as_secs()))
                    }
                }
            }
            Transport::Http { .. } => tokio::time::timeout(timeout, self.http_request(id, &msg))
                .await
                .unwrap_or_else(|_| Err(format!("“{method}” took longer than {} s", timeout.as_secs()))),
        }
    }

    async fn send_raw(&self, msg: &Value) -> Result<(), String> {
        match &self.transport {
            Transport::Stdio { stdin, .. } => {
                let mut line = msg.to_string();
                line.push('\n');
                let mut w = stdin.lock().await;
                w.write_all(line.as_bytes()).await.map_err(|e| format!("Couldn't talk to the server: {e}"))?;
                w.flush().await.map_err(|e| format!("Couldn't talk to the server: {e}"))
            }
            Transport::Http { .. } => self.http_post(msg).await.map(|_| ()),
        }
    }

    fn http_headers(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let Transport::Http { token, session, .. } = &self.transport else { return req };
        let mut req = req
            .header(reqwest::header::ACCEPT, "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL);
        if let Some(s) = session.lock().unwrap().clone() {
            req = req.header("Mcp-Session-Id", s);
        }
        if !token.is_empty() {
            req = req.bearer_auth(token);
        }
        req
    }

    async fn http_post(&self, msg: &Value) -> Result<reqwest::Response, String> {
        let Transport::Http { url, session, .. } = &self.transport else { unreachable!() };
        let resp = self
            .http_headers(http().post(url).json(msg))
            .send()
            .await
            .map_err(|e| if e.is_connect() { format!("Couldn't reach {url}") } else { e.to_string() })?;
        if let Some(s) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            *session.lock().unwrap() = Some(s.to_string());
        }
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED || resp.status() == reqwest::StatusCode::FORBIDDEN {
            return Err(format!("{}: the server wants a valid token", resp.status().as_u16()));
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = crate::limits::text_start(resp, crate::limits::MAX_ERROR).await;
            let body = body.trim();
            return Err(if body.is_empty() || body.len() > 300 { format!("The server answered {status}") } else { format!("{status}: {body}") });
        }
        Ok(resp)
    }

    async fn http_request(&self, id: u64, msg: &Value) -> Result<Value, String> {
        let resp = self.http_post(msg).await?;
        let is_sse = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|c| c.contains("text/event-stream"));
        if !is_sse {
            let v: Value = crate::limits::json(resp, crate::limits::MAX_JSON).await.map_err(|e| format!("The server's answer: {e}"))?;
            let items = if let Value::Array(a) = v { a } else { vec![v] };
            for item in items {
                if item.get("id").and_then(Value::as_u64) == Some(id) {
                    return rpc_result(item);
                }
            }
            return Err("The server didn't answer the request".into());
        }
        let mut stream = resp.bytes_stream();
        let mut lines = crate::limits::Lines::default();
        let mut data = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("The connection dropped: {e}"))?;
            for line in lines.push(&chunk, crate::limits::MAX_JSON)? {
                let line = line.trim_end_matches('\r');
                if line.is_empty() {
                    if let Ok(v) = serde_json::from_str::<Value>(&data) {
                        if v.get("id").and_then(Value::as_u64) == Some(id) && (v.get("result").is_some() || v.get("error").is_some()) {
                            return rpc_result(v);
                        }
                        if let (Some(rid), Some(method)) = (v.get("id").cloned(), v.get("method").and_then(Value::as_str)) {
                            let _ = self.send_raw(&answer_server_request(rid, method)).await;
                        }
                    }
                    data.clear();
                } else if let Some(d) = line.strip_prefix("data:") {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(d.trim_start());
                    if data.len() > crate::limits::MAX_JSON {
                        return Err("The server sent a message longer than 32 MB".into());
                    }
                }
            }
        }
        Err("The server closed the stream without answering".into())
    }

    async fn handle_incoming(&self, mcp: &Arc<Mcp>, server_id: &str, msg: Value) {
        let Transport::Stdio { pending, .. } = &self.transport else { return };
        let has_method = msg.get("method").and_then(Value::as_str).map(str::to_string);
        match (msg.get("id").cloned(), has_method) {
            (Some(id), None) => {
                if let Some(n) = id.as_u64() {
                    if let Some(tx) = pending.lock().unwrap().remove(&n) {
                        let _ = tx.send(rpc_result(msg));
                    }
                }
            }
            (Some(id), Some(method)) => {
                let _ = self.send_raw(&answer_server_request(id, &method)).await;
            }
            (None, Some(method)) if method == "notifications/tools/list_changed" => {
                let (mcp, id) = (mcp.clone(), server_id.to_string());
                tauri::async_runtime::spawn(async move { mcp.refresh_tools(&id).await });
            }
            _ => {}
        }
    }
}

/// Relay offers no client features (sampling, roots, elicitation): it answers
/// pings and declines the rest.
fn answer_server_request(id: Value, method: &str) -> Value {
    if method == "ping" {
        json!({ "jsonrpc": "2.0", "id": id, "result": {} })
    } else {
        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "Relay doesn't support this" } })
    }
}

fn rpc_result(msg: Value) -> Result<Value, String> {
    if let Some(e) = msg.get("error") {
        return Err(e.get("message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| e.to_string()));
    }
    Ok(msg.get("result").cloned().unwrap_or(Value::Null))
}

/// The name a tool goes by for the model: `server__tool`, in the characters
/// and length every API accepts.
pub fn tool_alias(server: &str, tool: &str) -> String {
    let clean = |s: &str| -> String {
        let s: String = s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
        s.trim_matches('_').to_string()
    };
    let server = clean(&server.to_lowercase());
    let server = if server.is_empty() { "mcp".to_string() } else { server.chars().take(20).collect() };
    let mut name = format!("{server}__{}", clean(tool));
    name.truncate(64);
    name
}

/// Reads a Claude Desktop style `{"mcpServers": {...}}` (or just the inner
/// object) into servers and their secrets.
pub fn parse_import(text: &str) -> Result<Vec<(McpServer, Secrets)>, String> {
    let v: Value = serde_json::from_str(text.trim()).map_err(|e| format!("That isn't valid JSON: {e}"))?;
    let map = v.get("mcpServers").or_else(|| v.get("servers")).unwrap_or(&v).as_object().ok_or("Expected an object of servers")?;
    let mut out = Vec::new();
    for (name, cfg) in map {
        let mut env = HashMap::new();
        for (k, val) in cfg.get("env").and_then(Value::as_object).into_iter().flatten() {
            if let Some(s) = val.as_str() {
                env.insert(k.clone(), s.to_string());
            }
        }
        let url = cfg.get("url").or_else(|| cfg.get("serverUrl")).and_then(Value::as_str).unwrap_or("").to_string();
        let command = cfg.get("command").and_then(Value::as_str).unwrap_or("").to_string();
        if url.is_empty() && command.is_empty() {
            return Err(format!("“{name}” has neither a command nor a url"));
        }
        let mut token = String::new();
        if let Some(auth) = cfg.pointer("/headers/Authorization").and_then(Value::as_str) {
            token = auth.strip_prefix("Bearer ").unwrap_or(auth).to_string();
        }
        let args = cfg
            .get("args")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let server = McpServer {
            id: crate::db::new_id(),
            name: name.clone(),
            kind: if url.is_empty() { "stdio".into() } else { "http".into() },
            command,
            args,
            cwd: cfg.get("cwd").and_then(Value::as_str).unwrap_or("").to_string(),
            url,
            enabled: !cfg.get("disabled").and_then(Value::as_bool).unwrap_or(false),
            tool_policy: HashMap::new(),
            default_policy: "ask".into(),
            env_keys: env.keys().cloned().collect(),
            has_token: !token.is_empty(),
            sort: 0,
        };
        out.push((server, Secrets { env, token }));
    }
    Ok(out)
}

/// Puts a process in the job that ends with Relay (skill scripts use it too).
#[cfg(windows)]
pub fn adopt(process: isize) {
    job::adopt(process);
}

#[cfg(windows)]
mod job {
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// One job for all tool servers: when Relay exits, Windows closes the job
    /// and ends every process in it (and their children).
    fn handle() -> isize {
        static JOB: OnceLock<isize> = OnceLock::new();
        *JOB.get_or_init(|| unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return 0;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            job as isize
        })
    }

    pub fn adopt(process: isize) {
        let job = handle();
        if job != 0 {
            unsafe {
                AssignProcessToJobObject(job as _, process as _);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_are_api_safe() {
        assert_eq!(tool_alias("File System", "read_file"), "file_system__read_file");
        assert_eq!(tool_alias("", "x.y"), "mcp__x_y");
        assert!(tool_alias(&"a".repeat(80), &"b".repeat(80)).len() <= 64);
    }

    #[test]
    fn results_become_text() {
        let r = json!({ "content": [{ "type": "text", "text": "one" }, { "type": "image", "data": "…" }] });
        assert_eq!(result_text(&r), "one\n[an image, not shown]");
        assert_eq!(result_text(&json!({ "structuredContent": { "n": 1 } })), r#"{"n":1}"#);
    }

    #[test]
    fn imports_claude_desktop_config() {
        let cfg = r#"{"mcpServers":{"fs":{"command":"npx","args":["-y","@modelcontextprotocol/server-filesystem","C:\\x"],"env":{"K":"v"}},"remote":{"url":"https://x.dev/mcp","headers":{"Authorization":"Bearer t0k"}}}}"#;
        let mut got = parse_import(cfg).unwrap();
        got.sort_by(|a, b| a.0.name.cmp(&b.0.name));
        assert_eq!(got[0].0.kind, "stdio");
        assert_eq!(got[0].0.args.len(), 3);
        assert_eq!(got[0].1.env["K"], "v");
        assert_eq!(got[1].0.kind, "http");
        assert_eq!(got[1].1.token, "t0k");
        assert!(parse_import("{").is_err());
    }
}

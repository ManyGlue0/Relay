//! Sending messages and streaming replies. The backend owns the whole reply:
//! it stores the message, talks to the model, runs the tools it asks for
//! (with the user's OK), sends the UI small batches as they arrive, and stores
//! the result however the reply ends. The UI can switch chats or reload
//! mid-reply and nothing is lost.
//!
//! A reply is a list of parts (reasoning, text, tool calls). The UI gets
//! `stream-delta` events holding operations on those parts, numbered by `seq`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::attachments::{self, Attachment};
use crate::db::{Caps, Message, Preset, Provider};
use crate::llm::{self, End, Event, ThinkSplitter};
use crate::skills::{self, Skill};
use crate::Core;

/// The UI hears about new text at most this often (a frame or two).
const BATCH: Duration = Duration::from_millis(40);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Part {
    Reasoning {
        text: String,
        /// How long the model thought, once it moved on.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ms: Option<u64>,
    },
    Text {
        text: String,
    },
    Tool {
        id: String,
        server_id: String,
        server: String,
        tool: String,
        /// The arguments as the model wrote them (JSON text).
        arguments: String,
        /// "approval" | "running" | "done" | "error" | "denied"
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ms: Option<u64>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "op", rename_all = "camelCase")]
enum Op {
    /// Adds text to the last part of this kind, or starts one.
    Append { kind: &'static str, text: String },
    /// A tool part, whole (it replaces the one with the same id).
    Tool { part: Part },
    /// The reasoning part at `index` ended after `ms`.
    Thought { index: usize, ms: u64 },
}

fn apply(parts: &mut Vec<Part>, op: &Op) {
    match op {
        Op::Append { kind, text } => match (parts.last_mut(), *kind) {
            (Some(Part::Text { text: t }), "text") | (Some(Part::Reasoning { text: t, .. }), "reasoning") => t.push_str(text),
            (_, "text") => parts.push(Part::Text { text: text.clone() }),
            _ => parts.push(Part::Reasoning { text: text.clone(), ms: None }),
        },
        Op::Tool { part } => {
            let Part::Tool { id, .. } = part else { return };
            match parts.iter_mut().find(|p| matches!(p, Part::Tool { id: i, .. } if i == id)) {
                Some(slot) => *slot = part.clone(),
                None => parts.push(part.clone()),
            }
        }
        Op::Thought { index, ms } => {
            if let Some(Part::Reasoning { ms: slot, .. }) = parts.get_mut(*index) {
                *slot = Some(*ms);
            }
        }
    }
}

/// A reply in progress: its cancel switch, its parts so far (so a chat
/// opened mid-reply shows everything), and tool calls waiting for the user.
struct Live {
    chat_id: String,
    token: CancellationToken,
    parts: Vec<Part>,
    /// How many batches the UI has been sent. A chat loaded mid-reply carries
    /// it, so the UI can tell which batches it already has.
    seq: u64,
    approvals: HashMap<String, oneshot::Sender<String>>,
}

#[derive(Default)]
pub struct Streams {
    running: Mutex<HashMap<String, Live>>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Delta<'a> {
    chat_id: &'a str,
    message_id: &'a str,
    seq: u64,
    ops: &'a [Op],
    thinking: bool,
}

impl Streams {
    fn start(&self, message_id: &str, chat_id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        let live = Live { chat_id: chat_id.to_string(), token: token.clone(), parts: Vec::new(), seq: 0, approvals: HashMap::new() };
        self.running.lock().unwrap().insert(message_id.to_string(), live);
        token
    }

    fn end(&self, message_id: &str) {
        self.running.lock().unwrap().remove(message_id);
    }

    pub fn stop_chat(&self, chat_id: &str) {
        for live in self.running.lock().unwrap().values() {
            if live.chat_id == chat_id {
                live.token.cancel();
            }
        }
    }

    /// Chats with a reply in progress.
    pub fn active_chats(&self) -> Vec<String> {
        let mut chats: Vec<String> = self.running.lock().unwrap().values().map(|l| l.chat_id.clone()).collect();
        chats.sort();
        chats.dedup();
        chats
    }

    /// Fills in the parts so far of replies still streaming.
    pub fn overlay(&self, messages: &mut [Message]) {
        let running = self.running.lock().unwrap();
        for m in messages.iter_mut().filter(|m| m.status == "streaming") {
            if let Some(live) = running.get(&m.id) {
                m.parts = Some(serde_json::to_value(&live.parts).unwrap_or(Value::Null));
                m.seq = live.seq;
            }
        }
    }

    /// Applies a batch to the live parts and sends it to the UI, under one
    /// lock so a chat loaded meanwhile sees either both or neither.
    fn push(&self, app: &AppHandle, message_id: &str, ops: &[Op], thinking: bool) {
        if ops.is_empty() {
            return;
        }
        let mut running = self.running.lock().unwrap();
        let Some(live) = running.get_mut(message_id) else { return };
        for op in ops {
            apply(&mut live.parts, op);
        }
        live.seq += 1;
        let _ = app.emit("stream-delta", Delta { chat_id: &live.chat_id, message_id, seq: live.seq, ops, thinking });
    }

    fn wait_for(&self, message_id: &str, call_id: &str) -> Option<oneshot::Receiver<String>> {
        let (tx, rx) = oneshot::channel();
        let mut running = self.running.lock().unwrap();
        running.get_mut(message_id)?.approvals.insert(call_id.to_string(), tx);
        Some(rx)
    }

    /// The user's answer to a tool call waiting for approval.
    pub fn decide(&self, message_id: &str, call_id: &str, decision: &str) -> bool {
        let tx = self.running.lock().unwrap().get_mut(message_id).and_then(|l| l.approvals.remove(call_id));
        match tx {
            Some(tx) => tx.send(decision.to_string()).is_ok(),
            None => false,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sent {
    pub user: Option<Message>,
    pub assistant: Message,
}

fn core(app: &AppHandle) -> Arc<Core> {
    app.state::<Arc<Core>>().inner().clone()
}

/// The first message, shortened: the title until (or instead of) a generated one.
pub fn provisional_title(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("New chat");
    let line = line.trim_start_matches(['#', '>', '-', '*', ' ']);
    let mut out: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        if let Some(i) = out.rfind(' ').filter(|&i| i > 30) {
            out.truncate(i);
        }
        out.push('…');
    }
    out
}

/// The files the composer attached, by id.
fn load_files(core: &Core, ids: &[String]) -> Result<Vec<Attachment>, String> {
    let root = attachments::root(&core.data_dir);
    ids.iter().map(|id| attachments::load(&root, id)).collect()
}

/// A message's text for titles: what was typed, or else what was attached.
fn title_source(text: &str, files: &[Attachment]) -> String {
    match (text.trim().is_empty(), files.is_empty()) {
        (_, true) => text.to_string(),
        (true, false) => attachments::describe(files),
        (false, false) => format!("{text}\n[Attached: {}]", attachments::describe(files)),
    }
}

/// Adds the user's message to the end of the chat and starts the reply.
pub fn send(app: &AppHandle, chat_id: &str, text: &str, files: &[String], provider_id: &str, model: &str) -> Result<Sent, String> {
    let core = core(app);
    let chat = core.db.chat(chat_id)?.ok_or("That chat no longer exists")?;
    let files = load_files(&core, files)?;
    core.db.set_chat_model(chat_id, provider_id, model)?;
    let user = core.db.add_message(chat_id, chat.leaf_id.as_deref(), "user", text, &files, None, None, "done")?;
    if chat.summary.title.is_empty() {
        let source = if text.trim().is_empty() { attachments::describe(&files) } else { text.to_string() };
        core.db.set_title(chat_id, &provisional_title(&source), true)?;
    }
    let assistant = start_reply(app, chat_id, &user.id, provider_id, model)?;
    crate::holster::publish(app);
    Ok(Sent { user: Some(user), assistant })
}

/// An edited user message: a new branch beside the original, with a new reply.
/// `files`: the attachments the edited message keeps (ids of the original's).
pub fn edit(app: &AppHandle, chat_id: &str, message_id: &str, text: &str, files: &[String], provider_id: &str, model: &str) -> Result<Sent, String> {
    let core = core(app);
    let original = core.db.message(message_id)?.ok_or("That message no longer exists")?;
    if original.role != "user" || original.chat_id != chat_id {
        return Err("Only your own messages can be edited".into());
    }
    let kept: Vec<Attachment> = original.attachments.iter().filter(|a| files.contains(&a.id)).cloned().collect();
    core.streams.stop_chat(chat_id);
    core.db.set_chat_model(chat_id, provider_id, model)?;
    let user = core.db.add_message(chat_id, original.parent_id.as_deref(), "user", text, &kept, None, None, "done")?;
    let assistant = start_reply(app, chat_id, &user.id, provider_id, model)?;
    crate::holster::publish(app);
    Ok(Sent { user: Some(user), assistant })
}

/// Another answer to the same message (with the same or another model).
pub fn regenerate(app: &AppHandle, chat_id: &str, message_id: &str, provider_id: &str, model: &str) -> Result<Sent, String> {
    let core = core(app);
    let reply = core.db.message(message_id)?.ok_or("That message no longer exists")?;
    let parent = match reply.role.as_str() {
        "assistant" => reply.parent_id.ok_or("That reply has nothing to answer")?,
        "user" => reply.id,
        _ => return Err("That message can't be answered again".into()),
    };
    core.streams.stop_chat(chat_id);
    core.db.set_chat_model(chat_id, provider_id, model)?;
    let assistant = start_reply(app, chat_id, &parent, provider_id, model)?;
    Ok(Sent { user: None, assistant })
}

/// "Monday, 3 November 2026, 14:05": a moment (Unix ms) in the user's local time.
fn local_time(ms: i64) -> String {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
        use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
        // FILETIME counts 100 ns steps from 1601.
        let ticks = (ms.max(0) as u64 + 11_644_473_600_000) * 10_000;
        let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
        let mut utc: SYSTEMTIME = std::mem::zeroed();
        let mut t: SYSTEMTIME = std::mem::zeroed();
        if FileTimeToSystemTime(&ft, &mut utc) == 0 || SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut t) == 0 {
            return String::new();
        }
        return format_time(t.wYear as i32, t.wMonth as u32, t.wDay as u32, t.wHour as u32, t.wMinute as u32);
    }
    #[allow(unreachable_code)]
    String::new()
}

fn format_time(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> String {
    const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    const MONTHS: [&str; 12] =
        ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    // Sakamoto's weekday (0 = Sunday).
    const OFFSETS: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let m = month.clamp(1, 12);
    let y = if m < 3 { year - 1 } else { year };
    let weekday = (y + y / 4 - y / 100 + y / 400 + OFFSETS[(m - 1) as usize] + day as i32).rem_euclid(7);
    format!("{}, {} {} {}, {:02}:{:02}", DAYS[weekday as usize], day, MONTHS[(m - 1) as usize], year, hour, minute)
}

/// Each user message starts with this: when it was sent. Old messages keep
/// their stamp, so the start of the conversation never changes between
/// requests and servers can reuse their prompt cache.
fn stamped(m: &Message) -> String {
    let when = local_time(m.created_at);
    if when.is_empty() {
        m.content.clone()
    } else {
        format!("[{when}]\n{}", m.content)
    }
}

/// The system prompt: who the user is, how messages carry the date, the
/// preset, the chat's own. It stays the same for the whole chat (nothing in it
/// changes per message), so a server's prompt cache keeps working.
pub fn system_prompt(core: &Core, preset: Option<&Preset>, chat_prompt: &str) -> String {
    let s = core.prefs.get();
    let mut blocks = Vec::new();
    if s.share_profile {
        if let Some(p) = s.profile.as_prompt() {
            blocks.push(p);
        }
    }
    if s.share_date && cfg!(windows) {
        blocks.push(
            "Each message from the user starts with when they sent it, in their local time, like [Monday, 3 November 2026, 14:05]. \
             Use it for \"today\", \"now\" and deadlines; don't repeat it in your replies."
                .into(),
        );
    }
    if let Some(p) = preset.filter(|p| !p.system_prompt.trim().is_empty()) {
        blocks.push(p.system_prompt.trim().to_string());
    }
    if !chat_prompt.trim().is_empty() {
        blocks.push(chat_prompt.trim().to_string());
    }
    blocks.join("\n\n")
}

/// Where attachments are, and what the model takes of them.
struct Files {
    root: std::path::PathBuf,
    caps: Caps,
}

/// What `model` takes besides text: what was detected (or, before that, what
/// its name suggests), with the user's word over it.
pub fn caps_for(p: &Provider, model: &str) -> Caps {
    p.caps.get(model).copied().unwrap_or_else(|| llm::guess_caps(&p.base_url, model)).with_user(p.caps_user.get(model))
}

/// How stored messages become the API's.
#[derive(Clone, Copy, Default)]
struct History<'a> {
    /// Tool calls as tool messages (else summarized as text).
    tools: bool,
    /// Reasoning as `reasoning_content`.
    reasoning: bool,
    /// User messages start with when they were sent.
    stamps: bool,
    /// Attachments go as themselves; None: named in words only.
    files: Option<&'a Files>,
}

fn text_part(t: impl Into<String>) -> Value {
    json!({ "type": "text", "text": t.into() })
}

/// A user message for the API. Without files it's the same string it always
/// was (so earlier messages never change, and the prompt cache holds). With
/// files: the time, the files, then the text.
fn user_message(m: &Message, opts: History) -> Value {
    let content = if opts.stamps { stamped(m) } else { m.content.clone() };
    if m.attachments.is_empty() {
        return json!({ "role": "user", "content": content });
    }
    let mut parts = Vec::new();
    let when = if opts.stamps { local_time(m.created_at) } else { String::new() };
    if !when.is_empty() {
        parts.push(text_part(format!("[{when}]")));
    }
    match opts.files {
        Some(f) => parts.extend(attachments::content_parts(&f.root, &m.attachments, f.caps)),
        None => parts.push(text_part(format!("[Attached: {}]", attachments::describe(&m.attachments)))),
    }
    if !m.content.trim().is_empty() {
        parts.push(text_part(m.content.clone()));
    }
    // Only text (text files, documents): one string, which every server takes.
    if parts.iter().all(|p| p["type"] == "text") {
        let joined = parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n\n");
        return json!({ "role": "user", "content": joined });
    }
    json!({ "role": "user", "content": parts })
}

/// An assistant message, with its reasoning as `reasoning_content` when there is some.
fn assistant_message(content: Value, reasoning: &str) -> Value {
    let mut m = json!({ "role": "assistant", "content": content });
    if !reasoning.trim().is_empty() {
        m["reasoning_content"] = json!(reasoning.trim());
    }
    m
}

/// Puts each assistant round's stored Anthropic blocks (`messages.native`) on
/// its message as `_native`, found by the `_relay` tag `history_messages` adds.
fn attach_natives(core: &Core, messages: &mut [Value]) {
    let ids: Vec<String> = messages.iter().filter_map(|m| m.pointer("/_relay/id").and_then(Value::as_str).map(str::to_string)).collect();
    if ids.is_empty() {
        return;
    }
    let natives = core.db.natives(&ids).unwrap_or_default();
    for m in messages.iter_mut() {
        let (Some(id), Some(round)) = (m.pointer("/_relay/id").and_then(Value::as_str), m.pointer("/_relay/round").and_then(Value::as_u64)) else { continue };
        if let Some(blocks) = natives.get(id).and_then(|n| n.pointer(&format!("/rounds/{round}"))).filter(|b| b.is_array()) {
            m["_native"] = blocks.clone();
        }
    }
}

/// Marks an assistant message with the stored message and the round it came from.
fn tag(mut msg: Value, id: &str, round: &mut usize) -> Value {
    msg["_relay"] = json!({ "id": id, "round": *round });
    *round += 1;
    msg
}

/// Turns stored messages into the API's messages. With `tools` off, earlier
/// tool calls are summarized as text (some servers reject tool messages
/// without tool definitions). With `reasoning`, each assistant message carries
/// the thinking that led to it (the server's template decides what to keep).
fn history_messages(history: &[Message], opts: History) -> Vec<Value> {
    let History { tools, reasoning, .. } = opts;
    let mut out = Vec::new();
    for m in history {
        if m.role == "user" {
            out.push(user_message(m, opts));
            continue;
        }
        if m.role != "assistant" {
            continue;
        }
        let parts: Vec<Part> = m.parts.clone().and_then(|p| serde_json::from_value(p).ok()).unwrap_or_default();
        // Each assistant message out is one round of the reply (one request).
        let mut round = 0usize;
        if parts.is_empty() {
            // A reply that failed before saying anything isn't part of the conversation.
            if !m.content.trim().is_empty() {
                out.push(tag(assistant_message(json!(m.content), if reasoning { &m.reasoning } else { "" }), &m.id, &mut round));
            }
            continue;
        }
        let mut text = String::new();
        let mut thought = String::new();
        let mut calls: Vec<Value> = Vec::new();
        let mut results: Vec<Value> = Vec::new();
        // A round ends with its tool calls: they go out with the text and thinking before them.
        let flush = |text: &mut String, thought: &mut String, calls: &mut Vec<Value>, results: &mut Vec<Value>, out: &mut Vec<Value>, round: &mut usize| {
            if calls.is_empty() {
                return;
            }
            let content = if text.trim().is_empty() { Value::Null } else { json!(text.trim()) };
            let mut msg = assistant_message(content, thought);
            msg["tool_calls"] = json!(calls.clone());
            out.push(tag(msg, &m.id, round));
            out.append(results);
            calls.clear();
            text.clear();
            thought.clear();
        };
        for p in &parts {
            match p {
                Part::Reasoning { text: t, .. } => {
                    flush(&mut text, &mut thought, &mut calls, &mut results, &mut out, &mut round);
                    if reasoning {
                        if !thought.is_empty() {
                            thought.push_str("\n\n");
                        }
                        thought.push_str(t.trim());
                    }
                }
                Part::Text { text: t } => {
                    flush(&mut text, &mut thought, &mut calls, &mut results, &mut out, &mut round);
                    text.push_str(t);
                }
                // The server's own tools ran inside its reply: no round of their own.
                Part::Tool { server_id, tool, arguments, result, .. } if server_id == crate::anthropic::HOSTED => {
                    let clipped: String = result.clone().unwrap_or_default().chars().take(1000).collect();
                    text.push_str(&format!("
[Used {tool} with {arguments}: {clipped}]
"));
                }
                Part::Tool { id, server, tool, arguments, status, result, .. } => {
                    let answer = match status.as_str() {
                        "denied" => "The user declined this tool call.".to_string(),
                        _ => result.clone().unwrap_or_else(|| "The tool call didn't finish.".into()),
                    };
                    if tools {
                        let name = crate::mcp::tool_alias(server, tool);
                        calls.push(json!({ "id": id, "type": "function", "function": { "name": name, "arguments": arguments } }));
                        results.push(json!({ "role": "tool", "tool_call_id": id, "content": answer }));
                    } else {
                        let clipped: String = answer.chars().take(2000).collect();
                        text.push_str(&format!("\n[Called {tool} ({server}) with {arguments}: {clipped}]\n"));
                    }
                }
            }
        }
        flush(&mut text, &mut thought, &mut calls, &mut results, &mut out, &mut round);
        if !text.trim().is_empty() {
            out.push(tag(assistant_message(json!(text.trim()), &thought), &m.id, &mut round));
        }
    }
    out
}

/// A tool the model may call: where it lives, and its policy.
#[derive(Clone)]
struct ToolRoute {
    server_id: String,
    server: String,
    tool: String,
    policy: String,
}

/// The tools a reply may use: the built-in skill tools first (when there are
/// skills), then every tool of the ready MCP servers that isn't off.
/// The tools a chat offers, in a fixed order (skills, Relay's web tools, then
/// MCP servers in their Settings order): each switch adds its own.
fn tool_specs(core: &Core, skills: &[Skill], web: bool, mcp: bool) -> (Vec<Value>, HashMap<String, ToolRoute>) {
    let mut specs = Vec::new();
    let mut routes = HashMap::new();
    if !skills.is_empty() {
        for (tool, schema) in skills::tool_specs(skills) {
            let alias = crate::mcp::tool_alias(skills::SERVER, &tool);
            specs.push(json!({
                "type": "function",
                "function": { "name": alias, "description": skills::tool_description(&tool), "parameters": schema }
            }));
            // Reading is harmless; a script asks unless its skill is trusted (decided per call).
            let policy = if tool == skills::RUN_SCRIPT { "script" } else { "allow" };
            routes.insert(
                alias,
                ToolRoute { server_id: skills::SERVER_ID.into(), server: skills::SERVER.into(), tool, policy: policy.into() },
            );
        }
    }
    if web {
        for (tool, description, schema) in crate::web::tool_specs() {
            let alias = crate::mcp::tool_alias(crate::web::SERVER, tool);
            specs.push(json!({
                "type": "function",
                "function": { "name": alias, "description": description, "parameters": schema }
            }));
            // The switch is the permission: searching and reading pages don't ask.
            routes.insert(
                alias,
                ToolRoute { server_id: crate::web::SERVER_ID.into(), server: crate::web::SERVER.into(), tool: tool.into(), policy: "allow".into() },
            );
        }
    }
    // In the servers' order from Settings, so the tool list is the same every
    // time (a changed list would spoil the server's prompt cache).
    let servers = if mcp { core.db.mcp_servers().unwrap_or_default() } else { Vec::new() };
    for server in servers {
        if !server.enabled {
            continue;
        }
        let Some(st) = core.mcp.status_of(&server.id).filter(|st| st.state == "ready") else { continue };
        for t in &st.tools {
            let policy = server.tool_policy.get(&t.name).cloned().unwrap_or_else(|| server.default_policy.clone());
            if policy == "off" {
                continue;
            }
            let alias = crate::mcp::tool_alias(&server.name, &t.name);
            if routes.contains_key(&alias) {
                continue;
            }
            let schema = if t.input_schema.is_object() { t.input_schema.clone() } else { json!({ "type": "object", "properties": {} }) };
            specs.push(json!({
                "type": "function",
                "function": { "name": alias, "description": t.description.chars().take(1024).collect::<String>(), "parameters": schema }
            }));
            routes.insert(alias, ToolRoute { server_id: server.id.clone(), server: server.name.clone(), tool: t.name.clone(), policy });
        }
    }
    (specs, routes)
}

/// Starts an assistant reply under `parent` (a user message).
pub fn start_reply(app: &AppHandle, chat_id: &str, parent: &str, provider_id: &str, model: &str) -> Result<Message, String> {
    let core = core(app);
    let provider = core.db.provider(provider_id)?.ok_or("That provider was removed. Pick another model.")?;
    let chat = core.db.chat(chat_id)?.ok_or("That chat no longer exists")?;
    // A new reply (sent, edited or asked again) brings an archived chat back.
    if chat.summary.archived {
        core.db.set_archived(chat_id, false)?;
    }
    let history = core.db.path_to(parent)?;
    let settings = core.prefs.get();
    let preset = chat.summary.preset_id.as_deref().and_then(|id| core.db.preset(id).ok().flatten());
    // The chat's own choice wins, then the preset's, then the default.
    let effort = chat
        .summary
        .effort
        .clone()
        .or_else(|| preset.as_ref().and_then(|p| p.effort.clone()))
        .unwrap_or_else(|| settings.default_effort.clone());
    let skills = if chat.summary.skills { skills::usable(&core) } else { Vec::new() };
    // Claude on Anthropic's API searches with Anthropic's own tools (`_web` below).
    let native_api = provider.api == "anthropic";
    let (specs, routes) = tool_specs(&core, &skills, chat.summary.web && !native_api, chat.summary.tools);

    let mut messages: Vec<Value> = Vec::new();
    let mut system = system_prompt(&core, preset.as_ref(), &chat.system_prompt);
    if let Some(block) = skills::prompt_block(&skills) {
        if !system.is_empty() {
            system.push_str("\n\n");
        }
        system.push_str(&block);
    }
    if !system.is_empty() {
        messages.push(json!({ "role": "system", "content": system }));
    }
    let files = Files { root: attachments::root(&core.data_dir), caps: caps_for(&provider, model) };
    let opts = History { tools: !specs.is_empty(), reasoning: settings.send_reasoning, stamps: settings.share_date, files: Some(&files) };
    messages.extend(history_messages(&history, opts));
    if native_api {
        attach_natives(&core, &mut messages);
    }

    let mut body = json!({ "model": model });
    if let Some(p) = &preset {
        if let Some(t) = p.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(t) = p.top_p {
            body["top_p"] = json!(t);
        }
        if let Some(t) = p.max_tokens {
            body["max_tokens"] = json!(t);
        }
    }
    apply_effort(&mut body, &effort);
    if native_api {
        // For `anthropic`, which builds its own request (never sent as they are).
        body["_effort"] = json!(effort);
        body["_cache"] = json!(settings.anthropic_cache);
        body["_web"] = json!(chat.summary.web);
    }

    // The first exchange names the chat (unless the user named it already).
    let users: Vec<&Message> = history.iter().filter(|m| m.role == "user").collect();
    let title = if users.len() == 1 { title_plan(&core, chat_id, &provider, model) } else { None };
    let first_user = users.first().map(|m| title_source(&m.content, &m.attachments)).unwrap_or_default();

    let assistant = core.db.add_message(chat_id, Some(parent), "assistant", "", &[], Some(provider_id), Some(model), "streaming")?;
    let token = core.streams.start(&assistant.id, chat_id);
    let job = Job {
        app: app.clone(),
        api: provider.api.clone(),
        base: provider.base_url,
        key: crate::secrets::key(&provider.id),
        body,
        messages,
        specs,
        routes,
        max_rounds: settings.tool_rounds.clamp(1, 50),
        send_reasoning: settings.send_reasoning,
        title,
        first_user,
        token,
        chat_id: chat_id.to_string(),
        message_id: assistant.id.clone(),
    };
    tauri::async_runtime::spawn(job.run());
    Ok(assistant)
}

/// The ways servers take a thinking budget. All of these are optional
/// fields: a server that rejects them gets the request again without.
pub fn apply_effort(body: &mut Value, effort: &str) {
    match effort {
        "off" => {
            body["reasoning_effort"] = json!("none");
            body["chat_template_kwargs"] = json!({ "enable_thinking": false });
            body["think"] = json!(false);
        }
        e => {
            let e = normalize_effort(e);
            body["reasoning_effort"] = json!(e);
            body["chat_template_kwargs"] = json!({ "enable_thinking": true, "reasoning_effort": e });
        }
    }
}

/// The efforts Relay knows: off, low, medium, high, xhigh, max. Anything else
/// (such as the old "auto") is medium.
pub fn normalize_effort(e: &str) -> &'static str {
    match e {
        "off" => "off",
        "low" => "low",
        "high" => "high",
        "xhigh" => "xhigh",
        "max" => "max",
        _ => "medium",
    }
}

struct Job {
    app: AppHandle,
    /// "openai" or "anthropic".
    api: String,
    base: String,
    key: Option<String>,
    body: Value,
    messages: Vec<Value>,
    specs: Vec<Value>,
    routes: HashMap<String, ToolRoute>,
    max_rounds: u32,
    /// Tool rounds carry the thinking that led to them.
    send_reasoning: bool,
    /// Who names the chat, and whether before or after this reply.
    title: Option<TitlePlan>,
    /// The chat's first message, for the title.
    first_user: String,
    token: CancellationToken,
    chat_id: String,
    message_id: String,
}

#[derive(Default)]
struct CallAcc {
    id: String,
    name: String,
    arguments: String,
}

/// The reply's parts as they grow, the batch waiting to go to the UI, and
/// when the reasoning in progress started.
struct Tracker {
    app: AppHandle,
    message_id: String,
    parts: Vec<Part>,
    pending: Vec<Op>,
    last_emit: Instant,
    thinking_since: Option<(usize, Instant)>,
    answered: bool,
}

impl Tracker {
    fn op(&mut self, op: Op) {
        // Anything after reasoning ends that reasoning: note how long it took.
        let is_reasoning = matches!(op, Op::Append { kind: "reasoning", .. });
        if !is_reasoning {
            self.end_thought();
        }
        apply(&mut self.parts, &op);
        if is_reasoning && self.thinking_since.is_none() {
            self.thinking_since = Some((self.parts.len() - 1, Instant::now()));
        }
        if matches!(op, Op::Append { kind: "text", .. }) {
            self.answered = true;
        }
        self.pending.push(op);
    }

    fn end_thought(&mut self) {
        if let Some((index, since)) = self.thinking_since.take() {
            let thought = Op::Thought { index, ms: since.elapsed().as_millis() as u64 };
            apply(&mut self.parts, &thought);
            self.pending.push(thought);
        }
    }

    fn flush(&mut self, force: bool) {
        if self.pending.is_empty() || (!force && self.last_emit.elapsed() < BATCH) {
            return;
        }
        let thinking = self.thinking_since.is_some();
        core(&self.app).streams.push(&self.app, &self.message_id, &self.pending, thinking);
        self.pending.clear();
        self.last_emit = Instant::now();
    }

    fn set_tool(&mut self, part: Part) {
        self.op(Op::Tool { part });
        self.flush(true);
    }
}

fn set_tool_state(part: &mut Part, status: &str, result: Option<String>, ms: Option<u64>) {
    if let Part::Tool { status: s, result: r, ms: m, .. } = part {
        *s = status.into();
        *r = result;
        *m = ms;
    }
}

impl Job {
    async fn run(self) {
        let core = core(&self.app);
        let mut started = Instant::now();
        let mut first_token: Option<Instant> = None;
        let mut gen_secs = 0.0f64;
        let mut prompt_tokens: Option<u64> = None;
        let mut completion_tokens: u64 = 0;
        let mut have_usage = false;
        let mut finish: Option<String> = None;
        let mut outcome: Result<End, String> = Ok(End::Done);
        let mut tr = Tracker {
            app: self.app.clone(),
            message_id: self.message_id.clone(),
            parts: Vec::new(),
            pending: Vec::new(),
            last_emit: Instant::now(),
            thinking_since: None,
            answered: false,
        };
        let mut messages = self.messages.clone();
        // Anthropic's blocks, one list per round, stored with the reply.
        let mut natives: Vec<Value> = Vec::new();
        // The last round paused (Anthropic's long server tools): the next one continues it.
        let mut paused = false;

        // A title from the same server comes first: asked after the reply, it
        // would push this conversation out of the server's prompt cache.
        if let Some(plan) = self.title.as_ref().filter(|p| p.before) {
            tokio::select! {
                _ = make_title(&self.app, &core, &self.chat_id, plan, &self.first_user, None) => {}
                _ = self.token.cancelled() => {}
            }
            // The reply's timings (time to first token) start now, not with the title.
            started = Instant::now();
        }

        'rounds: for round in 0..=self.max_rounds {
            let mut body = self.body.clone();
            body["messages"] = json!(messages);
            // The last round must answer with words.
            if !self.specs.is_empty() && round < self.max_rounds {
                body["tools"] = json!(self.specs);
            }
            if round == self.max_rounds {
                body["_web"] = json!(false);
            }
            let mut split = ThinkSplitter::default();
            let mut calls: Vec<CallAcc> = Vec::new();
            let mut round_text = String::new();
            let mut round_thought = String::new();
            let mut round_first: Option<Instant> = None;
            let mut round_usage: Option<(u64, u64)> = None;
            let mut round_native: Option<Vec<Value>> = None;
            let mut hosted: HashMap<String, Part> = HashMap::new();

            let result = llm::stream_chat(&self.api, &self.base, self.key.as_deref(), body, &self.token, |event| {
                match event {
                    Event::Content(text) => {
                        round_first.get_or_insert_with(Instant::now);
                        let (c, r) = split.feed(&text);
                        if !r.is_empty() {
                            round_thought.push_str(&r);
                            tr.op(Op::Append { kind: "reasoning", text: r });
                        }
                        if !c.is_empty() {
                            round_text.push_str(&c);
                            tr.op(Op::Append { kind: "text", text: c });
                        }
                    }
                    Event::Reasoning(text) => {
                        round_first.get_or_insert_with(Instant::now);
                        round_thought.push_str(&text);
                        tr.op(Op::Append { kind: "reasoning", text });
                    }
                    Event::ToolCall { index, id, name, arguments } => {
                        round_first.get_or_insert_with(Instant::now);
                        while calls.len() <= index {
                            calls.push(CallAcc::default());
                        }
                        let c = &mut calls[index];
                        if let Some(id) = id {
                            c.id = id;
                        }
                        if let Some(name) = name {
                            c.name = name;
                        }
                        c.arguments.push_str(&arguments);
                    }
                    Event::Usage { prompt, completion } => round_usage = Some((prompt, completion)),
                    Event::Finish(reason) => finish = Some(reason),
                    Event::Native(blocks) => round_native = Some(blocks),
                    Event::Hosted { id, name, input, result } => {
                        round_first.get_or_insert_with(Instant::now);
                        let part = hosted.entry(id.clone()).or_insert_with(|| Part::Tool {
                            id: id.clone(),
                            server_id: crate::anthropic::HOSTED.into(),
                            server: "Anthropic".into(),
                            tool: name.clone(),
                            arguments: "{}".into(),
                            status: "running".into(),
                            result: None,
                            ms: None,
                        });
                        if let Part::Tool { tool, arguments, status, result: res, .. } = part {
                            if !name.is_empty() {
                                *tool = name;
                            }
                            if let Some(i) = input {
                                *arguments = i;
                            }
                            if let Some((text, failed)) = result {
                                *status = if failed { "error".into() } else { "done".into() };
                                *res = Some(text);
                            }
                        }
                        tr.set_tool(part.clone());
                    }
                }
                tr.flush(false);
            })
            .await;

            let (c, r) = split.finish();
            if !r.is_empty() {
                round_thought.push_str(&r);
                tr.op(Op::Append { kind: "reasoning", text: r });
            }
            if !c.is_empty() {
                round_text.push_str(&c);
                tr.op(Op::Append { kind: "text", text: c });
            }
            tr.flush(true);
            if let Some(t) = round_first {
                first_token.get_or_insert(t);
                gen_secs += t.elapsed().as_secs_f64();
            }
            if let Some((p, c)) = round_usage {
                // The last round's prompt is the whole conversation: that's the context in use.
                prompt_tokens = Some(p);
                completion_tokens += c;
                have_usage = true;
            }
            // A paused round and the one continuing it are one turn: their blocks go back together.
            let blocks = round_native.take();
            if let Some(b) = &blocks {
                match natives.last_mut().and_then(Value::as_array_mut).filter(|_| paused) {
                    Some(last) => last.extend(b.iter().cloned()),
                    None => natives.push(json!(b)),
                }
            }
            paused = false;
            match result {
                Ok(End::Done) => {}
                other => {
                    outcome = other;
                    break;
                }
            }
            let calls: Vec<CallAcc> = calls.into_iter().filter(|c| !c.name.is_empty()).collect();
            if calls.is_empty() {
                if finish.as_deref() == Some("pause_turn") && round < self.max_rounds {
                    if let Some(b) = blocks {
                        // Send what came so far: the server picks up where it stopped.
                        messages.push(json!({ "role": "assistant", "content": round_text, "_native": b }));
                        paused = true;
                        continue;
                    }
                }
                break;
            }

            // The model asked for tools: run them (with the user's OK), then ask again.
            let ids: Vec<String> =
                calls.iter().enumerate().map(|(i, c)| if c.id.is_empty() { format!("call_{round}_{i}") } else { c.id.clone() }).collect();
            let tool_calls: Vec<Value> = calls
                .iter()
                .zip(&ids)
                .map(|(c, id)| json!({ "id": id, "type": "function", "function": { "name": c.name, "arguments": c.arguments } }))
                .collect();
            let content = if round_text.trim().is_empty() { Value::Null } else { json!(round_text) };
            let mut msg = assistant_message(content, if self.send_reasoning { &round_thought } else { "" });
            msg["tool_calls"] = json!(tool_calls);
            if let Some(b) = blocks {
                msg["_native"] = json!(b);
            }
            messages.push(msg);

            for (c, id) in calls.iter().zip(&ids) {
                let route = self.routes.get(&c.name).cloned();
                let arguments = if c.arguments.trim().is_empty() { "{}".to_string() } else { c.arguments.clone() };
                let mut part = Part::Tool {
                    id: id.clone(),
                    server_id: route.as_ref().map(|r| r.server_id.clone()).unwrap_or_default(),
                    server: route.as_ref().map(|r| r.server.clone()).unwrap_or_default(),
                    tool: route.as_ref().map(|r| r.tool.clone()).unwrap_or_else(|| c.name.clone()),
                    arguments: arguments.clone(),
                    status: "running".into(),
                    result: None,
                    ms: None,
                };
                let Some(route) = route else {
                    let msg = format!("There's no tool called {}.", c.name);
                    set_tool_state(&mut part, "error", Some(msg.clone()), None);
                    tr.set_tool(part);
                    messages.push(json!({ "role": "tool", "tool_call_id": id, "content": msg }));
                    continue;
                };
                let args: Value = serde_json::from_str(&arguments).unwrap_or_else(|_| json!({}));
                let builtin = route.server_id == skills::SERVER_ID;
                let script_skill = args.get("skill").and_then(Value::as_str).unwrap_or("").to_string();
                let allowed = match route.policy.as_str() {
                    "allow" => true,
                    "script" => skills::scripts_allowed(&core, &script_skill),
                    _ => false,
                };
                if !allowed {
                    set_tool_state(&mut part, "approval", None, None);
                    tr.set_tool(part.clone());
                    let decision = match core.streams.wait_for(&self.message_id, id) {
                        Some(rx) => tokio::select! {
                            d = rx => d.unwrap_or_else(|_| "deny".into()),
                            _ = self.token.cancelled() => "cancel".into(),
                        },
                        None => "deny".into(),
                    };
                    if decision == "always" && builtin {
                        if let Some(id) = skills::id_of(&core, &script_skill) {
                            skills::set_flags(&core, &id, None, Some(true));
                            let _ = self.app.emit("skills-changed", ());
                        }
                    } else if decision == "always" {
                        if let Ok(Some(mut s)) = core.db.mcp_server(&route.server_id) {
                            s.tool_policy.insert(route.tool.clone(), "allow".into());
                            let _ = core.db.save_mcp_server(&s);
                            let _ = self.app.emit("mcp-servers-changed", ());
                        }
                    }
                    if decision == "cancel" {
                        set_tool_state(&mut part, "denied", None, None);
                        tr.set_tool(part);
                        outcome = Ok(End::Cancelled);
                        break 'rounds;
                    }
                    if decision != "once" && decision != "always" {
                        set_tool_state(&mut part, "denied", None, None);
                        tr.set_tool(part);
                        messages.push(json!({ "role": "tool", "tool_call_id": id, "content": "The user declined this tool call." }));
                        continue;
                    }
                }
                set_tool_state(&mut part, "running", None, None);
                tr.set_tool(part.clone());
                let t0 = Instant::now();
                let work = async {
                    if builtin {
                        skills::call(&core, &route.tool, &args, &self.token).await
                    } else if route.server_id == crate::web::SERVER_ID {
                        crate::web::call(&route.tool, &args).await
                    } else {
                        core.mcp.call(&route.server_id, &route.tool, args.clone()).await
                    }
                };
                let res = tokio::select! {
                    r = work => Some(r),
                    _ = self.token.cancelled() => None,
                };
                let ms = Some(t0.elapsed().as_millis() as u64);
                let answer = match res {
                    None => {
                        set_tool_state(&mut part, "error", Some("Stopped".into()), ms);
                        tr.set_tool(part);
                        outcome = Ok(End::Cancelled);
                        break 'rounds;
                    }
                    Some(Ok((text, is_error))) => {
                        set_tool_state(&mut part, if is_error { "error" } else { "done" }, Some(text.clone()), ms);
                        tr.set_tool(part);
                        text
                    }
                    Some(Err(e)) => {
                        set_tool_state(&mut part, "error", Some(e.clone()), ms);
                        tr.set_tool(part);
                        format!("The tool failed: {e}")
                    }
                };
                messages.push(json!({ "role": "tool", "tool_call_id": id, "content": answer }));
            }
        }

        // Close a reasoning part the stream ended in.
        tr.end_thought();
        tr.flush(true);
        let parts = std::mem::take(&mut tr.parts);
        let answered = tr.answered;
        core.streams.end(&self.message_id);

        let content: String = parts.iter().filter_map(|p| if let Part::Text { text } = p { Some(text.as_str()) } else { None }).collect();
        let reasoning: String = parts
            .iter()
            .filter_map(|p| if let Part::Reasoning { text, .. } = p { Some(text.as_str()) } else { None })
            .collect::<Vec<_>>()
            .join("\n\n");
        let tool_count = parts.iter().filter(|p| matches!(p, Part::Tool { .. })).count();
        let ended = Instant::now();
        let (status, error) = match &outcome {
            Ok(End::Done) if !answered && reasoning.trim().is_empty() && tool_count == 0 => {
                ("error", Some("The model sent back an empty reply.".to_string()))
            }
            Ok(End::Done) => ("done", None),
            Ok(End::Cancelled) => ("stopped", None),
            Err(e) => ("error", Some(e.clone())),
        };
        if !have_usage {
            completion_tokens = ((content.chars().count() + reasoning.chars().count()) as f64 / 4.0).round() as u64;
        }
        let think_ms: u64 = parts.iter().filter_map(|p| if let Part::Reasoning { ms, .. } = p { *ms } else { None }).sum();
        let stats = json!({
            "ttftMs": first_token.map(|t| t.duration_since(started).as_millis() as u64),
            "thinkMs": if think_ms > 0 { Some(think_ms) } else { None },
            "totalMs": ended.duration_since(started).as_millis() as u64,
            "promptTokens": prompt_tokens,
            "completionTokens": completion_tokens,
            "estimated": !have_usage,
            "tokensPerSec": if gen_secs > 0.05 && completion_tokens > 0 { Some(completion_tokens as f64 / gen_secs) } else { None },
            "finishReason": finish,
            "toolCalls": tool_count,
        });
        let parts_json = serde_json::to_value(&parts).unwrap_or(Value::Null);
        if let Err(e) = core.db.finish_message(&self.message_id, &content, &reasoning, &parts_json, status, error.as_deref(), &stats) {
            eprintln!("chat: couldn't store the reply: {e}");
        }
        if !natives.is_empty() {
            let _ = core.db.set_native(&self.message_id, &json!({ "rounds": natives }));
        }
        if let Ok(Some(chat)) = core.db.chat(&self.chat_id) {
            if let Some(m) = chat.messages.into_iter().find(|m| m.id == self.message_id) {
                let _ = self.app.emit("stream-end", &m);
            }
        }
        crate::holster::publish(&self.app);

        if status == "done" {
            crate::holster::reply_finished(&self.app, &self.chat_id, &content);
            if let Some(plan) = self.title.as_ref().filter(|p| !p.before) {
                make_title(&self.app, &core, &self.chat_id, plan, &self.first_user, Some(&content)).await;
            }
        }
    }
}

/// Who names a chat and when: the model from Settings (or the chat's own),
/// before the first reply when it runs on the chat's server, after it
/// otherwise (then the reply helps, and no cache is at stake).
struct TitlePlan {
    api: String,
    base: String,
    key: Option<String>,
    model: String,
    before: bool,
}

fn title_plan(core: &Core, chat_id: &str, chat_provider: &Provider, chat_model: &str) -> Option<TitlePlan> {
    let s = core.prefs.get();
    if !s.auto_titles || !core.db.title_is_provisional(chat_id).unwrap_or(false) {
        return None;
    }
    let picked = s.title_model.as_ref().and_then(|r| core.db.provider(&r.provider_id).ok().flatten().map(|p| (p, r.model.clone())));
    let (provider, model) = picked.unwrap_or_else(|| (chat_provider.clone(), chat_model.to_string()));
    let before = same_server(&provider.base_url, &chat_provider.base_url);
    Some(TitlePlan { key: crate::secrets::key(&provider.id), api: provider.api.clone(), base: provider.base_url, model, before })
}

fn same_server(a: &str, b: &str) -> bool {
    let host = |u: &str| llm::normalize_base(u).to_lowercase().replace("://localhost", "://127.0.0.1");
    host(a) == host(b)
}

/// Asks for a short title and sets it (unless the user renamed the chat meanwhile).
async fn make_title(app: &AppHandle, core: &Core, chat_id: &str, plan: &TitlePlan, user: &str, reply: Option<&str>) {
    let clip = |s: &str| s.chars().take(1500).collect::<String>();
    let convo = match reply {
        Some(r) => format!("User:\n{}\n\nAssistant:\n{}\n\nTitle:", clip(user), clip(r)),
        None => format!("User:\n{}\n\nTitle:", clip(user)),
    };
    let body = json!({
        "model": plan.model,
        "messages": [
            { "role": "system", "content": "You name conversations. Reply with a title of 2 to 6 words that says what the conversation is about. No quotes, no punctuation at the end, no emoji. Use the language of the conversation." },
            { "role": "user", "content": convo }
        ],
        "temperature": 0.3,
        "stream": false
    });
    // Reasoning models would think at length about a title. Servers that take
    // chat template options (llama.cpp, vLLM, SGLang) can skip that; the rest
    // reject the field, and get the plain request instead. A title the reply
    // waits for gets less time.
    // A title is a few words: capped, a model can't go on to answer the
    // whole message (small ones do, and the reply would wait for it).
    let mut quick = body.clone();
    quick["chat_template_kwargs"] = json!({ "enable_thinking": false });
    quick["max_tokens"] = json!(32);
    let limit = Duration::from_secs(if reply.is_none() { 30 } else { 90 });
    let attempt = |b: Value| tokio::time::timeout(limit, llm::complete(&plan.api, &plan.base, plan.key.as_deref(), b));
    // Rejected (the server doesn't take those fields), or empty (a model that
    // thought anyway and used up the cap): ask plainly.
    let title = match attempt(quick).await {
        Ok(Ok(t)) if !clean_title(&t).is_empty() => t,
        Ok(_) => match attempt(body).await {
            Ok(Ok(t)) => t,
            _ => return,
        },
        Err(_) => return,
    };
    let title = clean_title(&title);
    // The user may have renamed it while the model was thinking.
    if title.is_empty() || !core.db.title_is_provisional(chat_id).unwrap_or(false) {
        return;
    }
    if core.db.set_generated_title(chat_id, &title).is_ok() {
        if let Ok(Some(summary)) = core.db.chat_summary(chat_id) {
            let _ = app.emit("chat-updated", &summary);
        }
        crate::holster::publish(app);
    }
}

pub fn clean_title(raw: &str) -> String {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let line = line.strip_prefix("Title:").unwrap_or(line).trim();
    let line = line.trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | '*' | '#' | '“' | '”' | '«' | '»') || c.is_whitespace());
    let line = line.trim_end_matches(['.', '!', ':', ';']);
    line.chars().take(80).collect::<String>().trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles() {
        assert_eq!(provisional_title("\n\n  # How do I sort a Vec?\nmore"), "How do I sort a Vec?");
        let long = provisional_title(&"word ".repeat(40));
        assert!(long.ends_with('…') && long.chars().count() <= 61);
        assert_eq!(clean_title("\"Sorting vectors in Rust.\"\n"), "Sorting vectors in Rust");
        assert_eq!(clean_title("Title: **Trip to Rome**"), "Trip to Rome");
    }

    #[test]
    fn parts_grow_in_order() {
        let mut parts = Vec::new();
        apply(&mut parts, &Op::Append { kind: "reasoning", text: "hm".into() });
        apply(&mut parts, &Op::Append { kind: "reasoning", text: "m".into() });
        apply(&mut parts, &Op::Thought { index: 0, ms: 900 });
        apply(&mut parts, &Op::Append { kind: "text", text: "Hi".into() });
        let tool = |status: &str| Part::Tool {
            id: "c1".into(),
            server_id: "s".into(),
            server: "fs".into(),
            tool: "read".into(),
            arguments: "{}".into(),
            status: status.into(),
            result: None,
            ms: None,
        };
        apply(&mut parts, &Op::Tool { part: tool("approval") });
        apply(&mut parts, &Op::Tool { part: tool("done") });
        apply(&mut parts, &Op::Append { kind: "text", text: "Done".into() });
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], Part::Reasoning { text: "hmm".into(), ms: Some(900) });
        assert!(matches!(&parts[2], Part::Tool { status, .. } if status == "done"));
        let v = serde_json::to_value(&parts[2]).unwrap();
        assert_eq!(v["type"], "tool");
        assert_eq!(v["serverId"], "s");
    }

    #[test]
    fn effort_fields() {
        let mut b = json!({});
        apply_effort(&mut b, "auto");
        assert_eq!(b["reasoning_effort"], "medium");
        apply_effort(&mut b, "xhigh");
        assert_eq!(b["reasoning_effort"], "xhigh");
        assert_eq!(b["chat_template_kwargs"]["reasoning_effort"], "xhigh");
        let mut b = json!({});
        apply_effort(&mut b, "off");
        assert_eq!(b["chat_template_kwargs"]["enable_thinking"], false);
        for k in b.as_object().unwrap().keys() {
            assert!(llm::OPTIONAL_FIELDS.contains(&k.as_str()), "{k} must be optional");
        }
    }

    fn msg(role: &str, content: &str, parts: Option<Value>) -> Message {
        Message {
            id: "x".into(),
            chat_id: "c".into(),
            parent_id: None,
            role: role.into(),
            content: content.into(),
            reasoning: String::new(),
            parts,
            provider_id: None,
            model: None,
            status: "done".into(),
            error: None,
            stats: None,
            created_at: 0,
            attachments: vec![],
            seq: 0,
            siblings: vec![],
        }
    }

    #[test]
    fn history_rebuilds_tool_rounds() {
        let m = |role: &str, content: &str, parts: Option<Value>| Message {
            id: "x".into(),
            chat_id: "c".into(),
            parent_id: None,
            role: role.into(),
            content: content.into(),
            reasoning: String::new(),
            parts,
            provider_id: None,
            model: None,
            status: "done".into(),
            error: None,
            stats: None,
            created_at: 0,
            attachments: vec![],
            seq: 0,
            siblings: vec![],
        };
        let parts = json!([
            { "type": "reasoning", "text": "plan" },
            { "type": "text", "text": "Let me look." },
            { "type": "tool", "id": "c1", "serverId": "s", "server": "fs", "tool": "read", "arguments": "{}", "status": "done", "result": "data" },
            { "type": "text", "text": "It says data." }
        ]);
        let h = vec![m("user", "read it", None), m("assistant", "Let me look.It says data.", Some(parts)), m("assistant", "", None)];
        let out = history_messages(&h, History { tools: true, ..Default::default() });
        assert_eq!(out.len(), 4);
        assert_eq!(out[1]["tool_calls"][0]["function"]["name"], "fs__read");
        assert_eq!(out[1]["content"], "Let me look.");
        assert!(out[1].get("reasoning_content").is_none());
        assert_eq!(out[2]["role"], "tool");
        assert_eq!(out[3]["content"], "It says data.");
        let flat = history_messages(&h[..2], History::default());
        assert_eq!(flat.len(), 2);
        assert!(flat[1]["content"].as_str().unwrap().contains("[Called read (fs)"));
    }

    #[test]
    fn history_sends_reasoning_per_round() {
        let parts = json!([
            { "type": "reasoning", "text": "plan" },
            { "type": "text", "text": "Let me look." },
            { "type": "tool", "id": "c1", "serverId": "s", "server": "fs", "tool": "read", "arguments": "{}", "status": "done", "result": "data" },
            { "type": "reasoning", "text": "got it" },
            { "type": "text", "text": "It says data." }
        ]);
        let mut a = msg("assistant", "Let me look.It says data.", Some(parts));
        a.reasoning = "plangot it".into();
        let mut old = msg("assistant", "Hi.", None);
        old.reasoning = "greet".into();
        let h = vec![msg("user", "hi", None), old, msg("user", "read it", None), a];
        let out = history_messages(&h, History { tools: true, reasoning: true, ..Default::default() });
        assert_eq!(out[1]["reasoning_content"], "greet");
        assert_eq!(out[3]["reasoning_content"], "plan");
        assert_eq!(out[3]["tool_calls"][0]["id"], "c1");
        assert_eq!(out[4]["role"], "tool");
        assert_eq!(out[5]["reasoning_content"], "got it");
        // Tools off: one message, all of its thinking.
        let flat = history_messages(&h, History { reasoning: true, ..Default::default() });
        assert_eq!(flat[3]["reasoning_content"], "plan\n\ngot it");
    }

    #[test]
    fn titles_wait_only_on_the_same_server() {
        assert!(same_server("http://localhost:8080/v1", "http://127.0.0.1:8080/v1/"));
        assert!(same_server("http://LOCALHOST:1234/v1", "http://localhost:1234/v1"));
        assert!(!same_server("http://localhost:1234/v1", "http://localhost:11434/v1"));
        assert!(!same_server("https://openrouter.ai/api/v1", "http://localhost:8080/v1"));
    }

    #[test]
    fn user_messages_carry_their_own_time() {
        assert_eq!(format_time(2026, 11, 3, 9, 5), "Tuesday, 3 November 2026, 09:05");
        assert_eq!(format_time(2024, 2, 29, 23, 59), "Thursday, 29 February 2024, 23:59");
        assert_eq!(format_time(2000, 1, 1, 0, 0), "Saturday, 1 January 2000, 00:00");
        let mut u = msg("user", "hello", None);
        u.created_at = 1_793_700_000_000;
        let h = vec![u];
        let opts = History { stamps: true, ..Default::default() };
        let a = history_messages(&h, opts);
        let text = a[0]["content"].as_str().unwrap();
        if cfg!(windows) {
            assert!(text.starts_with('[') && text.ends_with("]\nhello"), "{text}");
        }
        // The same message reads the same on every request.
        assert_eq!(a, history_messages(&h, opts));
        assert_eq!(history_messages(&h, History::default())[0]["content"], "hello");
    }

    #[test]
    fn user_files_go_with_the_message() {
        let dir = std::env::temp_dir().join(format!("relay-chat-{}", crate::db::new_id()));
        let root = attachments::root(&dir);
        let note = attachments::add_bytes(&root, "note.md", b"# Plan", false, None).unwrap();
        let png = attachments::add_bytes(&root, "dot.png", b"\x89PNG\r\n\x1a\n", false, None).unwrap();
        let files = Files { root: root.clone(), caps: Caps { vision: true, ..Default::default() } };
        let opts = History { files: Some(&files), ..Default::default() };

        // Only text files: still one string.
        let mut u = msg("user", "Summarize", None);
        u.attachments = vec![note.clone()];
        let out = history_messages(&[u.clone()], opts);
        assert_eq!(out[0]["content"], "<file name=\"note.md\">\n# Plan\n</file>\n\nSummarize");

        // An image: parts, the file before the text.
        u.attachments = vec![png, note];
        let out = history_messages(&[u.clone()], opts);
        let parts = out[0]["content"].as_array().unwrap();
        assert_eq!(parts[0]["type"], "image_url");
        assert!(parts[0]["image_url"]["url"].as_str().unwrap().starts_with("data:image/png;base64,"));
        assert_eq!(parts[2]["text"], "Summarize");

        // Without the folder (tests, titles): named only.
        let named = history_messages(&[u], History::default());
        assert_eq!(named[0]["content"], "[Attached: dot.png, note.md]\n\nSummarize");
        let _ = std::fs::remove_dir_all(dir);
    }
}

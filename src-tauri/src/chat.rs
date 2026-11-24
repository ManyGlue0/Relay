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
        /// What the card shows besides the result (an agent tool's diff, a count).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<Value>,
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
    /// A reply starts: `parts` is what it begins with (empty, or the parts of
    /// a reply that goes on).
    fn start(&self, message_id: &str, chat_id: &str, parts: Vec<Part>) -> CancellationToken {
        let token = CancellationToken::new();
        let live = Live { chat_id: chat_id.to_string(), token: token.clone(), parts, seq: 0, approvals: HashMap::new() };
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

fn core_arc(app: &AppHandle) -> Arc<Core> {
    core(app)
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
                // A summary: what came before goes, the summary takes its place.
                Part::Tool { server_id, tool, status, detail, .. } if server_id == crate::agent::SERVER_ID && tool == "summarize" => {
                    let summary = detail.as_ref().and_then(|d| d.get("summary")).and_then(Value::as_str).filter(|_| status == "done");
                    if let Some(summary) = summary {
                        flush(&mut text, &mut thought, &mut calls, &mut results, &mut out, &mut round);
                        let keep = detail.as_ref().and_then(|d| d.get("keepUser")).and_then(Value::as_bool).unwrap_or(false);
                        let kept = if keep { out.iter().rposition(|m| m["role"] == "user").map(|i| out.remove(i)) } else { None };
                        out.clear();
                        match kept {
                            Some(mut u) => {
                                prefix_user(&mut u, &summary_intro(summary, true));
                                out.push(u);
                            }
                            None => out.push(json!({ "role": "user", "content": summary_intro(summary, false) })),
                        }
                    }
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
/// `agent`: an Agent session's mode (its tools come first); in a chat, asking
/// the user and the to-do list come with the MCP switch.
fn tool_specs(core: &Core, agent: Option<&str>, skills: &[Skill], web: bool, mcp: bool) -> (Vec<Value>, HashMap<String, ToolRoute>) {
    let mut specs = Vec::new();
    let mut routes = HashMap::new();
    let mut own: Vec<(&'static str, String, Value)> = Vec::new();
    if agent.is_some() {
        own.extend(crate::agent::tool_specs(core.prefs.get().agent_cmd));
    }
    if agent.is_some() || mcp {
        own.extend(crate::agent::interact_specs(!matches!(agent, Some("auto" | "bypass"))));
    }
    {
        for (tool, description, schema) in own {
            let alias = crate::mcp::tool_alias(crate::agent::SERVER, tool);
            specs.push(json!({
                "type": "function",
                "function": { "name": alias, "description": description, "parameters": schema }
            }));
            // Decided per call, by the session's mode.
            routes.insert(
                alias,
                ToolRoute { server_id: crate::agent::SERVER_ID.into(), server: crate::agent::SERVER.into(), tool: tool.into(), policy: "agent".into() },
            );
        }
    }
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
/// A reply's request before it's sent: the conversation, the fields, the tools.
struct Prepared {
    provider: Provider,
    body: Value,
    messages: Vec<Value>,
    specs: Vec<Value>,
    routes: HashMap<String, ToolRoute>,
    settings: crate::settings::Settings,
    /// An Agent session's folder and rules.
    agent: Option<crate::agent::Ctx>,
    /// The model's context window, when known (Agent sessions summarize near it).
    context: Option<u64>,
    /// The conversation already fills most of the context: summarize before replying.
    summarize_first: bool,
}

/// Agent sessions summarize when a request fills this much of the context.
const SUMMARIZE_AT: f64 = 0.85;

/// Tool rounds one reply may take in an Agent session (a chat's come from Settings).
const AGENT_ROUNDS: u32 = 200;

impl Prepared {
    fn max_rounds(&self) -> u32 {
        if self.agent.is_some() {
            AGENT_ROUNDS
        } else {
            self.settings.tool_rounds.clamp(1, 50)
        }
    }
}

/// Builds the request for a reply to `history` (which ends with the message
/// to answer, or with the reply that goes on).
fn prepare(core: &Core, chat: &crate::db::Chat, history: &[Message], provider_id: &str, model: &str) -> Result<Prepared, String> {
    let provider = core.db.provider(provider_id)?.ok_or("That provider was removed. Pick another model.")?;
    let settings = core.prefs.get();
    let preset = chat.summary.preset_id.as_deref().and_then(|id| core.db.preset(id).ok().flatten());
    // The chat's own choice wins, then the preset's, then the default.
    let effort = chat
        .summary
        .effort
        .clone()
        .or_else(|| preset.as_ref().and_then(|p| p.effort.clone()))
        .unwrap_or_else(|| settings.default_effort.clone());
    let skills = if chat.summary.skills { skills::usable(core) } else { Vec::new() };
    let native_api = provider.api == "anthropic";
    // Claude on Anthropic's own API searches with Anthropic's server tools
    // (`_web` below). Servers that copy the Messages API don't have those, so
    // their models get Relay's web tools, like every other model.
    let hosted_web = native_api && crate::anthropic::is_first_party(&provider.base_url);
    let agent = if chat.summary.kind == "agent" {
        let folder = chat.summary.folder.as_deref().ok_or("This session has no folder yet")?;
        Some(crate::agent::Ctx::new(core, &chat.summary.id, folder))
    } else {
        None
    };
    let mode = agent.as_ref().map(|ctx| ctx.mode(core));
    let (specs, routes) = tool_specs(core, mode, &skills, chat.summary.web && !hosted_web, chat.summary.tools);

    let mut messages: Vec<Value> = Vec::new();
    let mut system = system_prompt(core, preset.as_ref(), &chat.system_prompt);
    if let Some(ctx) = &agent {
        // The folder's instructions are read once, when the session starts, and
        // kept: the request's start stays the same (the prompt cache).
        let instructions = match core.db.instructions(&chat.summary.id)? {
            Some(text) => text,
            None => {
                let text = crate::agent::read_instructions(&ctx.folder);
                core.db.set_instructions(&chat.summary.id, &text)?;
                // The folder as the session starts, said once and kept the same way.
                let note = crate::agent::start_note(&ctx.folder, ctx.mode(core));
                core.db.set_start_note(&chat.summary.id, note.as_deref())?;
                text
            }
        };
        let start_note = core.db.start_note(&chat.summary.id)?;
        let block = crate::agent::prompt(&ctx.folder.to_string_lossy(), ctx.mode(core), Some(&instructions), start_note.as_deref());
        system = if system.is_empty() { block } else { format!("{block}\n\n{system}") };
    }
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
    messages.extend(history_messages(history, opts));
    if native_api {
        attach_natives(core, &mut messages);
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
        body["_web"] = json!(chat.summary.web && hosted_web);
    }
    let context = provider.contexts.get(model).copied().filter(|c| *c > 0);
    // The last reply's request plus its answer: what the next request starts from.
    let used = history.iter().rev().find(|m| m.role == "assistant" && m.stats.is_some()).and_then(|m| m.stats.as_ref()).and_then(context_of);
    // Not again right after a summary that didn't bring it down (the system
    // prompt and tools may fill most of a small context on their own).
    let just_summarized = history.iter().rev().find(|m| m.role == "assistant").and_then(|m| m.parts.as_ref()).is_some_and(|p| p.to_string().contains("\"tool\":\"summarize\""));
    let summarize_first = agent.is_some() && !just_summarized && matches!((used, context), (Some(u), Some(c)) if u as f64 >= SUMMARIZE_AT * c as f64);
    Ok(Prepared { provider, body, messages, specs, routes, settings, agent, context, summarize_first })
}

pub fn start_reply(app: &AppHandle, chat_id: &str, parent: &str, provider_id: &str, model: &str) -> Result<Message, String> {
    let core = core(app);
    let chat = core.db.chat(chat_id)?.ok_or("That chat no longer exists")?;
    let history = core.db.path_to(parent)?;
    let p = prepare(&core, &chat, &history, provider_id, model)?;
    // A new reply (sent, edited or asked again) brings an archived chat back.
    if chat.summary.archived {
        core.db.set_archived(chat_id, false)?;
    }

    // The first exchange names the chat (unless the user named it already).
    let users: Vec<&Message> = history.iter().filter(|m| m.role == "user").collect();
    let title = if users.len() == 1 { title_plan(&core, chat_id, &p.provider, model) } else { None };
    let first_user = users.first().map(|m| title_source(&m.content, &m.attachments)).unwrap_or_default();

    let assistant = core.db.add_message(chat_id, Some(parent), "assistant", "", &[], Some(provider_id), Some(model), "streaming")?;
    let token = core.streams.start(&assistant.id, chat_id, Vec::new());
    let max_rounds = p.max_rounds();
    let job = Job {
        app: app.clone(),
        api: p.provider.api.clone(),
        base: p.provider.base_url,
        key: crate::secrets::key(&p.provider.id),
        body: p.body,
        messages: p.messages,
        specs: p.specs,
        routes: p.routes,
        max_rounds,
        send_reasoning: p.settings.send_reasoning,
        agent: p.agent.clone(),
        context: p.context,
        summarize_first: p.summarize_first,
        title,
        first_user,
        token,
        chat_id: chat_id.to_string(),
        message_id: assistant.id.clone(),
        resume: None,
    };
    tauri::async_runtime::spawn(job.run());
    Ok(assistant)
}

/// How a reply that didn't finish goes on.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Pickup {
    /// It stopped in the middle of its answer: the model goes on from that text.
    Text,
    /// It stopped between steps (after a tool, or while thinking): the next step runs.
    Fresh,
}

/// A tool call that never finished: stopped while it waited for the user or ran.
fn unfinished_tool(p: &Part) -> bool {
    matches!(p, Part::Tool { status, result, .. }
        if matches!(status.as_str(), "approval" | "question" | "running" | "denied") || (status == "error" && result.as_deref() == Some("Stopped")))
}

/// Trims a reply that didn't finish to where it can go on: tool calls that
/// never finished go (the model can ask again), and so does thinking cut off
/// halfway (the model thinks again).
fn trim_for_continue(parts: &mut Vec<Part>) -> Pickup {
    while parts.last().is_some_and(unfinished_tool) {
        parts.pop();
    }
    if matches!(parts.last(), Some(Part::Reasoning { .. })) {
        parts.pop();
    }
    match parts.last() {
        Some(Part::Text { text }) if !text.trim().is_empty() => Pickup::Text,
        _ => Pickup::Fresh,
    }
}

/// The text of the reply's last round: what comes after its last tool call.
fn trailing_text(parts: &[Part]) -> String {
    let start = parts.iter().rposition(|p| matches!(p, Part::Tool { .. })).map_or(0, |i| i + 1);
    parts[start..].iter().filter_map(|p| if let Part::Text { text } = p { Some(text.as_str()) } else { None }).collect()
}

/// The rounds of a reply that ended with tool calls (each one request):
/// `history_messages` sends each as one assistant message.
fn tool_rounds(parts: &[Part]) -> usize {
    let mut rounds = 0;
    let mut in_calls = false;
    for p in parts {
        let call = matches!(p, Part::Tool { server_id, .. } if server_id != crate::anthropic::HOSTED);
        if call && !in_calls {
            rounds += 1;
        }
        if call || matches!(p, Part::Reasoning { .. } | Part::Text { .. }) {
            in_calls = call;
        }
    }
    rounds
}

/// Continue: a reply that didn't finish (stopped, failed, or cut off at the
/// length limit) goes on in the same message, with the model that wrote it.
/// Servers that can go on from a half-written answer get one (thinking off,
/// so templates take it); Claude, and servers that refuse it, get the answer
/// so far plus a note asking to go on (`llm::CONTINUE_NOTE`).
pub fn continue_reply(app: &AppHandle, chat_id: &str, message_id: &str) -> Result<(), String> {
    let core = core(app);
    let chat = core.db.chat(chat_id)?.ok_or("That chat no longer exists")?;
    let reply = core.db.message(message_id)?.ok_or("That message no longer exists")?;
    if reply.role != "assistant" || reply.chat_id != chat_id {
        return Err("Only a reply can be continued".into());
    }
    if reply.status == "streaming" || core.streams.active_chats().iter().any(|c| c == chat_id) {
        return Err("Wait for the reply in progress to finish".into());
    }
    let (Some(provider_id), Some(model)) = (reply.provider_id.clone(), reply.model.clone()) else {
        return Err("That reply doesn't say which model wrote it".into());
    };
    let mut parts: Vec<Part> = reply.parts.clone().and_then(|p| serde_json::from_value(p).ok()).unwrap_or_default();
    if parts.is_empty() && !reply.content.trim().is_empty() {
        parts.push(Part::Text { text: reply.content.clone() });
    }
    let pickup = trim_for_continue(&mut parts);

    // The conversation up to and with the reply as it now stands.
    let mut history = core.db.path_to(message_id)?;
    if let Some(last) = history.last_mut().filter(|m| m.id == message_id) {
        last.parts = Some(serde_json::to_value(&parts).unwrap_or(Value::Null));
        if parts.is_empty() {
            last.content.clear();
        }
    }
    let mut p = prepare(&core, &chat, &history, &provider_id, &model)?;
    let mut first_body = None;
    if pickup == Pickup::Text {
        if p.provider.api == "anthropic" {
            // Claude takes no half-written answer: it gets the note.
            p.messages.push(llm::continue_note());
        } else {
            // The request ends with the answer so far, as it was written.
            if let Some(last) = p.messages.last_mut().filter(|m| m["role"] == "assistant") {
                last["content"] = json!(trailing_text(&parts).trim_start());
                if let Some(o) = last.as_object_mut() {
                    o.remove("reasoning_content");
                }
            }
            let mut b = p.body.clone();
            // Thinking is over by the answer; templates refuse a half-written answer with it on.
            apply_effort(&mut b, "off");
            b["_prefill"] = json!(true);
            // vLLM's switches for going on from the last message.
            b["continue_final_message"] = json!(true);
            b["add_generation_prompt"] = json!(false);
            first_body = Some(b);
        }
    }
    if chat.summary.archived {
        core.db.set_archived(chat_id, false)?;
    }

    // Anthropic blocks of the rounds that stay; the round that goes on is stored without.
    let complete = tool_rounds(&parts);
    let mut natives: Vec<Value> = core
        .db
        .natives(&[message_id.to_string()])?
        .get(message_id)
        .and_then(|n| n.get("rounds"))
        .and_then(Value::as_array)
        .map(|r| r.iter().take(complete).cloned().collect())
        .unwrap_or_default();
    natives.resize(complete, Value::Null);

    core.db.restart_message(message_id, &serde_json::to_value(&parts).unwrap_or(Value::Null))?;
    let token = core.streams.start(message_id, chat_id, parts.clone());
    let max_rounds = p.max_rounds();
    let job = Job {
        app: app.clone(),
        api: p.provider.api.clone(),
        base: p.provider.base_url,
        key: crate::secrets::key(&p.provider.id),
        body: p.body,
        messages: p.messages,
        specs: p.specs,
        routes: p.routes,
        max_rounds,
        send_reasoning: p.settings.send_reasoning,
        agent: p.agent.clone(),
        context: p.context,
        summarize_first: false,
        title: None,
        first_user: String::new(),
        token,
        chat_id: chat_id.to_string(),
        message_id: message_id.to_string(),
        resume: Some(Resume {
            prefill: first_body.as_ref().map(|_| trailing_text(&parts).trim_start().to_string()),
            parts,
            stats: reply.stats.clone(),
            natives,
            merged: pickup == Pickup::Text,
            first_body,
        }),
    };
    tauri::async_runtime::spawn(job.run());
    crate::holster::publish(app);
    Ok(())
}

/// The context a reply left: its last request plus that request's answer
/// (`contextTokens`). Replies from before it was recorded have only the last
/// request (`promptTokens`), next to every round's answers added up, which
/// were mostly in that request already.
pub(crate) fn context_of(stats: &Value) -> Option<u64> {
    stats.get("contextTokens").and_then(Value::as_u64).or_else(|| stats.get("promptTokens").and_then(Value::as_u64))
}

/// A continued reply's stats: time and tokens of both parts added up.
fn merge_stats(old: &Value, mut new: Value) -> Value {
    for k in ["totalMs", "completionTokens"] {
        let sum = old.get(k).and_then(Value::as_u64).unwrap_or(0) + new.get(k).and_then(Value::as_u64).unwrap_or(0);
        new[k] = json!(sum);
    }
    if let Some(t) = old.get("ttftMs").filter(|t| t.is_u64()) {
        new["ttftMs"] = t.clone();
    }
    if old.get("estimated").and_then(Value::as_bool) == Some(true) {
        new["estimated"] = json!(true);
    }
    new
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
    /// Continue: the reply goes on from what it has.
    resume: Option<Resume>,
    /// An Agent session's folder and rules.
    agent: Option<crate::agent::Ctx>,
    context: Option<u64>,
    summarize_first: bool,
}

/// What a continued reply starts from.
struct Resume {
    parts: Vec<Part>,
    stats: Option<Value>,
    /// Anthropic blocks of the rounds kept.
    natives: Vec<Value>,
    /// The first round goes on from cut-off text: stored, they're one round,
    /// so its blocks (which lack that text) aren't kept.
    merged: bool,
    /// The first request, when it ends with the half-written answer.
    first_body: Option<Value>,
    /// That half-written answer (the first request's last message).
    prefill: Option<String>,
}

#[derive(Default)]
struct CallAcc {
    id: String,
    name: String,
    arguments: String,
}

/// Tool calls one round may hold. The index comes from the server: a huge
/// one must not make Relay allocate its way out of memory.
const MAX_CALLS: usize = 128;

/// Adds a piece of a tool call (calls arrive split across chunks, by index).
/// Pieces for an index past `MAX_CALLS` are dropped.
fn add_call_piece(calls: &mut Vec<CallAcc>, index: usize, id: Option<String>, name: Option<String>, arguments: &str) {
    if index >= MAX_CALLS {
        return;
    }
    if calls.len() <= index {
        calls.resize_with(index + 1, CallAcc::default);
    }
    let c = &mut calls[index];
    if let Some(id) = id {
        c.id = id;
    }
    if let Some(name) = name {
        c.name = name;
    }
    c.arguments.push_str(arguments);
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

fn set_detail(part: &mut Part, value: Option<Value>) {
    if let Part::Tool { detail, .. } = part {
        *detail = value;
    }
}

fn part_id(part: &Part) -> &str {
    match part {
        Part::Tool { id, .. } => id,
        _ => "",
    }
}

fn set_tool_state(part: &mut Part, status: &str, result: Option<String>, ms: Option<u64>) {
    if let Part::Tool { status: s, result: r, ms: m, .. } = part {
        *s = status.into();
        *r = result;
        *m = ms;
    }
}

/// A whole answer without its inline thinking (`<think>…</think>`).
fn strip_think(text: &str) -> String {
    let mut split = ThinkSplitter::default();
    let (mut content, _) = split.feed(text);
    content.push_str(&split.finish().0);
    content.trim().to_string()
}

/// What the model is asked when a session is summarized.
const SUMMARY_REQUEST: &str = "Summarize this session so the work can continue from your summary alone, without the conversation. Include: the user's goals and requests (their exact words where they matter), decisions made and why, the files created, changed or deleted (paths) and what changed in each, commands that mattered and their results, problems met and how they were solved, the current state, and exactly what's left to do next. Don't repeat file contents: they can be read again. Plain text, short sections.";

/// The summary as the model reads it, before the new message or in place of the conversation.
fn summary_intro(summary: &str, before_new_message: bool) -> String {
    let end = if before_new_message { "The user's new message follows." } else { "Carry on with the task from where you left off." };
    format!("[This session was summarized to save context. The summary of the work so far:]\n\n{}\n\n[End of the summary. Files may have changed since: read them again before editing them. {end}]", summary.trim())
}

/// Puts text before a user message's content (a string or a list of parts).
fn prefix_user(msg: &mut Value, text: &str) {
    match msg.get_mut("content") {
        Some(Value::String(c)) => *c = format!("{text}\n\n{c}"),
        Some(Value::Array(parts)) => parts.insert(0, json!({ "type": "text", "text": text })),
        _ => msg["content"] = json!(text),
    }
}

/// Cuts long text to its start and end.
fn clip_middle(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    let head: String = chars[..max / 3].iter().collect();
    let tail: String = chars[chars.len() - (max - max / 3)..].iter().collect();
    format!("{head}\n[… {} characters cut …]\n{tail}", chars.len() - max)
}

/// The conversation as plain text for the summary: tool outputs cut short,
/// files read not repeated, at most `max` characters (the start and the end kept).
fn transcript(messages: &[Value], max: usize) -> String {
    let mut names: HashMap<String, String> = HashMap::new();
    let mut out = String::new();
    let text_of = |c: &Value| -> String {
        match c {
            Value::String(s) => s.clone(),
            Value::Array(parts) => parts
                .iter()
                .map(|p| match p.get("type").and_then(Value::as_str) {
                    Some("text") => p.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
                    Some(other) => format!("[{other}]"),
                    None => String::new(),
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        }
    };
    for m in messages {
        match m.get("role").and_then(Value::as_str) {
            Some("user") => out.push_str(&format!("\n\nUSER:\n{}", text_of(&m["content"]))),
            Some("assistant") => {
                let text = text_of(&m["content"]);
                if !text.trim().is_empty() {
                    out.push_str(&format!("\n\nASSISTANT:\n{}", text.trim()));
                }
                for c in m.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
                    let name = c.pointer("/function/name").and_then(Value::as_str).unwrap_or("").to_string();
                    let args = c.pointer("/function/arguments").and_then(Value::as_str).unwrap_or("");
                    out.push_str(&format!("\n\nTOOL CALL {name}: {}", clip_middle(args, 600)));
                    if let Some(id) = c.get("id").and_then(Value::as_str) {
                        names.insert(id.to_string(), name);
                    }
                }
            }
            Some("tool") => {
                let name = m.get("tool_call_id").and_then(Value::as_str).and_then(|id| names.get(id)).cloned().unwrap_or_default();
                let result = text_of(&m["content"]);
                let shown = if name.ends_with("read_file") { "[the file's contents: omitted]".to_string() } else { clip_middle(&result, 1500) };
                out.push_str(&format!("\n\nTOOL RESULT:\n{shown}"));
            }
            _ => {}
        }
    }
    clip_middle(out.trim(), max)
}

/// Old tool outputs give way first when a reply nears the context's end
/// (all but the last 6). Returns whether anything was dropped.
fn drop_old_outputs(messages: &mut [Value]) -> bool {
    let tools: Vec<usize> = messages.iter().enumerate().filter(|(_, m)| m["role"] == "tool").map(|(i, _)| i).collect();
    let mut dropped = false;
    for &i in tools.iter().take(tools.len().saturating_sub(6)) {
        if messages[i]["content"].as_str().is_some_and(|c| c.len() > 300) {
            messages[i]["content"] = json!("[Output dropped to save context: run the tool again if you need it.]");
            dropped = true;
        }
    }
    dropped
}

impl Job {
    /// The reply waits for the user (or stops waiting): the sidebar marks the
    /// chat, and Holster says so when Relay isn't in front.
    fn waiting(&self, on: bool, what: &str) {
        let _ = self.app.emit("chat-waiting", json!({ "chatId": self.chat_id, "waiting": on }));
        if on {
            crate::holster::waiting(&self.app, &self.chat_id, what);
        }
    }

    /// Summarizes the session (shown as a "Summarized here" part) and goes on
    /// from the summary: before the new message (`keep_user`), or in place of
    /// the whole conversation, mid-reply. On failure the messages stay as they were.
    async fn summarize(&self, tr: &mut Tracker, messages: &mut Vec<Value>, keep_user: bool) {
        let mut part = Part::Tool {
            id: format!("summary_{}", crate::db::new_id()),
            server_id: crate::agent::SERVER_ID.into(),
            server: crate::agent::SERVER.into(),
            tool: "summarize".into(),
            arguments: "{}".into(),
            status: "running".into(),
            result: None,
            ms: None,
            detail: Some(json!({ "keepUser": keep_user })),
        };
        tr.set_tool(part.clone());
        let t0 = Instant::now();
        let system = messages.first().filter(|m| m["role"] == "system").cloned();
        let mut input: Vec<Value> = messages.iter().filter(|m| m["role"] != "system").cloned().collect();
        let kept = if keep_user && input.last().is_some_and(|m| m["role"] == "user") { input.pop() } else { None };
        // About 3 characters a token, half the context for the transcript.
        let max = (self.context.unwrap_or(32_000) as usize).saturating_mul(3) / 2;
        let mut body = self.body.clone();
        if let Some(o) = body.as_object_mut() {
            o.remove("tools");
        }
        body["_web"] = json!(false);
        body["messages"] = json!([
            { "role": "system", "content": "You summarize work sessions between a user and a coding agent, so the agent can carry on from the summary." },
            { "role": "user", "content": format!("The session so far:\n\n{}\n\n---\n\n{SUMMARY_REQUEST}", transcript(&input, max.max(8_000))) }
        ]);
        let res = tokio::select! {
            r = crate::llm::complete(&self.api, &self.base, self.key.as_deref(), body) => Some(r),
            _ = self.token.cancelled() => None,
        };
        let ms = Some(t0.elapsed().as_millis() as u64);
        let summary = match res {
            Some(Ok(text)) => {
                let t = strip_think(&text);
                if t.is_empty() {
                    Err("The summary came back empty.".to_string())
                } else {
                    Ok(t)
                }
            }
            Some(Err(e)) => Err(format!("Couldn't summarize the session: {e}")),
            None => Err("Stopped".into()),
        };
        match summary {
            Ok(summary) => {
                let mut next = Vec::new();
                next.extend(system);
                match kept {
                    Some(mut u) => {
                        prefix_user(&mut u, &summary_intro(&summary, true));
                        next.push(u);
                    }
                    None => next.push(json!({ "role": "user", "content": summary_intro(&summary, false) })),
                }
                *messages = next;
                set_detail(&mut part, Some(json!({ "keepUser": keep_user, "summary": summary })));
                set_tool_state(&mut part, "done", Some("Summarized the session so far.".into()), ms);
            }
            Err(e) => set_tool_state(&mut part, "error", Some(e), ms),
        }
        tr.set_tool(part);
    }

    /// An agent tool call: judged by the session's mode, asked, refused or
    /// run. Returns what goes back to the model, or None when the reply was
    /// stopped meanwhile.
    async fn run_agent_tool(&self, core: &Core, tr: &mut Tracker, part: &mut Part, tool: &str, args: &Value) -> Option<String> {
        // Asking and the to-do list work in chats too.
        if tool == "todo" {
            let out = crate::agent::todo(args);
            set_detail(part, out.detail);
            set_tool_state(part, "done", Some(out.text.clone()), None);
            tr.set_tool(part.clone());
            return Some(out.text);
        }
        if tool == "ask_user" {
            let questions = match crate::agent::questions(args) {
                Ok(q) => q,
                Err(e) => {
                    set_tool_state(part, "error", Some(e.clone()), None);
                    tr.set_tool(part.clone());
                    return Some(e);
                }
            };
            set_detail(part, Some(json!({ "questions": questions })));
            set_tool_state(part, "question", None, None);
            tr.set_tool(part.clone());
            let t0 = Instant::now();
            let first = questions.pointer("/0/question").and_then(Value::as_str).unwrap_or("").to_string();
            self.waiting(true, &format!("A question for you: {first}"));
            let answer = match core.streams.wait_for(&self.message_id, part_id(part)) {
                Some(rx) => tokio::select! {
                    d = rx => d.ok(),
                    _ = self.token.cancelled() => {
                        self.waiting(false, "");
                        set_tool_state(part, "denied", None, None);
                        tr.set_tool(part.clone());
                        return None;
                    }
                },
                None => None,
            };
            self.waiting(false, "");
            let answers: Option<Value> = answer.as_deref().and_then(|a| serde_json::from_str(a).ok()).filter(Value::is_array);
            let text = crate::agent::answers_text(&questions, answers.as_ref());
            set_detail(part, Some(json!({ "questions": questions, "answers": answers })));
            set_tool_state(part, "done", Some(text.clone()), Some(t0.elapsed().as_millis() as u64));
            tr.set_tool(part.clone());
            return Some(text);
        }
        let Some(ctx) = self.agent.as_ref() else {
            set_tool_state(part, "error", Some("Agent tools only work in Agent sessions.".into()), None);
            tr.set_tool(part.clone());
            return Some("Agent tools only work in Agent sessions.".into());
        };
        let mode = ctx.mode(core);
        let which = crate::agent::shell::Shell::of_tool(tool);
        let (verdict, facts) = match which {
            Some(w) => crate::agent::judge_command(&core_arc(&self.app), ctx, mode, w, args).await,
            None => (crate::agent::judge(ctx, mode, tool, args), Default::default()),
        };
        // Auto's Python packages go into the folder's .venv: made first.
        let verdict = match verdict {
            crate::agent::Verdict::Run if mode == "auto" && facts.pip_install => match crate::agent::ensure_venv(&ctx.folder).await {
                Ok(()) => crate::agent::Verdict::Run,
                Err(r) => crate::agent::Verdict::Refuse(r),
            },
            v => v,
        };
        match verdict {
            crate::agent::Verdict::Refuse(r) => {
                let mut text = r.text(mode);
                // A second refusal under the same rule in this reply: drop that sub-task.
                let again = tr.parts.iter().any(|p| matches!(p, Part::Tool { status, detail: Some(d), .. } if status == "blocked" && d.pointer("/blocked/rule").and_then(Value::as_str) == Some(r.rule.as_str())));
                if again {
                    text.push_str("\nThis rule has refused a step twice now: drop that sub-task, note it for your final message, and carry on with the rest.");
                }
                set_detail(part, Some(json!({ "blocked": { "rule": r.rule, "why": r.why } })));
                set_tool_state(part, "blocked", Some(text.clone()), None);
                tr.set_tool(part.clone());
                let entry = json!({
                    "tool": tool,
                    "args": args,
                    "rule": r.rule,
                    "why": r.why,
                    "mode": mode,
                    "at": crate::db::now_ms(),
                    "messageId": self.message_id,
                    "callId": part_id(part),
                    "ran": false,
                });
                if let Ok(list) = core.db.update_blocked(&ctx.chat_id, |l| l.push(entry)) {
                    let _ = self.app.emit("agent-blocked", json!({ "chatId": ctx.chat_id, "count": list.iter().filter(|e| e["ran"] != true).count() }));
                }
                return Some(text);
            }
            crate::agent::Verdict::Ask(note) => {
                let mut detail = crate::agent::preview(ctx, tool, args).unwrap_or_else(|| json!({}));
                if let Some(n) = note {
                    detail["note"] = json!(n);
                }
                set_detail(part, Some(detail));
                set_tool_state(part, "approval", None, None);
                tr.set_tool(part.clone());
                let what = match args.get("command").and_then(Value::as_str) {
                    Some(c) => format!("Wants to run: {c}"),
                    None => format!("Wants to {} {}", tool.replace('_', " "), args.get("path").and_then(Value::as_str).unwrap_or("")),
                };
                self.waiting(true, what.trim());
                let decision = match core.streams.wait_for(&self.message_id, part_id(part)) {
                    Some(rx) => tokio::select! {
                        d = rx => d.unwrap_or_else(|_| "deny".into()),
                        _ = self.token.cancelled() => "cancel".into(),
                    },
                    None => "deny".into(),
                };
                self.waiting(false, "");
                match decision.as_str() {
                    "cancel" => {
                        set_tool_state(part, "denied", None, None);
                        tr.set_tool(part.clone());
                        return None;
                    }
                    "edits" => {
                        // "Accept edits from now": the session's mode changes.
                        let _ = core.db.set_chat_options(&ctx.chat_id, &crate::db::ChatOptions { mode: Some("edits".into()), ..Default::default() });
                        if let Ok(Some(c)) = core.db.chat_summary(&ctx.chat_id) {
                            let _ = self.app.emit("chat-updated", &c);
                        }
                    }
                    "once" | "always" => {}
                    _ => {
                        set_tool_state(part, "denied", None, None);
                        tr.set_tool(part.clone());
                        return Some("The user declined this tool call.".into());
                    }
                }
            }
            crate::agent::Verdict::Run => {}
        }
        set_tool_state(part, "running", None, None);
        tr.set_tool(part.clone());
        // Before a change, the folder is recorded: "Restore to before this".
        let mut saved = json!({});
        if crate::agent::is_step(tool) {
            let label = format!("Before {tool}");
            match crate::agent::checkpoint(&core_arc(&self.app), ctx, &self.message_id, part_id(part), &label).await {
                Ok(id) => saved["checkpoint"] = json!(id),
                Err(e) => saved["unprotected"] = json!(e),
            }
        }
        let t0 = Instant::now();
        let res = if let Some(which) = which {
            // Auto: secrets out of the environment (allowed ones masked), the venv on PATH.
            let (env, masks) = if mode == "auto" { crate::agent::auto_env(&core_arc(&self.app), ctx) } else { Default::default() };
            // The card shows the output as it comes.
            let base = saved.clone();
            let (mut out, cancelled) = crate::agent::run_command(ctx, which, args, &env, &self.token, |live| {
                let mut d = base.clone();
                if let (Some(d), Some(l)) = (d.as_object_mut(), live.as_object()) {
                    for (k, v) in l {
                        let v = match v.as_str() {
                            Some(s) if !masks.is_empty() => json!(crate::agent::mask(s, &masks)),
                            _ => v.clone(),
                        };
                        d.insert(k.clone(), v);
                    }
                }
                set_detail(part, Some(d));
                tr.set_tool(part.clone());
            })
            .await;
            if mode == "auto" {
                // Relay's Python guard may have refused processes meanwhile.
                if let Ok(list) = core.db.blocked(&ctx.chat_id) {
                    let _ = self.app.emit("agent-blocked", json!({ "chatId": ctx.chat_id, "count": list.iter().filter(|e| e["ran"] != true).count() }));
                }
            }
            if !masks.is_empty() {
                out.text = crate::agent::mask(&out.text, &masks);
                if let Some(o) = out.detail.as_mut().and_then(|d| d.get_mut("output")) {
                    if let Some(s) = o.as_str() {
                        *o = json!(crate::agent::mask(s, &masks));
                    }
                }
            }
            (!cancelled).then_some(out)
        } else if tool == "processes" {
            let out = if args.get("action").and_then(Value::as_str) == Some("wait") {
                crate::agent::wait_process(ctx, args, &self.token).await
            } else {
                Some(crate::agent::processes(ctx, args))
            };
            let Some(mut out) = out else {
                set_tool_state(part, "error", Some("Stopped".into()), Some(t0.elapsed().as_millis() as u64));
                tr.set_tool(part.clone());
                return None;
            };
            if mode == "auto" {
                let (_, masks) = crate::agent::auto_env(&core_arc(&self.app), ctx);
                out.text = crate::agent::mask(&out.text, &masks);
            }
            Some(out)
        } else {
            tokio::select! {
                r = crate::agent::call(ctx, tool, args) => Some(r),
                _ = self.token.cancelled() => None,
            }
        };
        let ms = Some(t0.elapsed().as_millis() as u64);
        let Some(out) = res else {
            set_tool_state(part, "error", Some("Stopped".into()), ms);
            tr.set_tool(part.clone());
            return None;
        };
        let mut detail = out.detail.unwrap_or_else(|| json!({}));
        if let (Some(d), Some(extra)) = (detail.as_object_mut(), saved.as_object()) {
            for (k, v) in extra {
                d.insert(k.clone(), v.clone());
            }
        }
        if detail.as_object().is_some_and(|d| !d.is_empty()) {
            set_detail(part, Some(detail));
        }
        set_tool_state(part, if out.error { "error" } else { "done" }, Some(out.text.clone()), ms);
        tr.set_tool(part.clone());
        Some(out.text)
    }

    async fn run(self) {
        let core = core(&self.app);
        let mut started = Instant::now();
        let mut first_token: Option<Instant> = None;
        let mut gen_secs = 0.0f64;
        let mut prompt_tokens: Option<u64> = None;
        let mut completion_tokens: u64 = 0;
        // The last round's request plus its answer: the context the next request starts from.
        let mut context_tokens: Option<u64> = None;
        let mut have_usage = false;
        let mut finish: Option<String> = None;
        let mut outcome: Result<End, String> = Ok(End::Done);
        let resume = self.resume.as_ref();
        let parts = resume.map(|r| r.parts.clone()).unwrap_or_default();
        let answered = parts.iter().any(|p| matches!(p, Part::Text { text } if !text.trim().is_empty()));
        let mut tr = Tracker {
            app: self.app.clone(),
            message_id: self.message_id.clone(),
            parts,
            pending: Vec::new(),
            last_emit: Instant::now(),
            thinking_since: None,
            answered,
        };
        let mut messages = self.messages.clone();
        // Anthropic's blocks, one list per round, stored with the reply.
        let mut natives: Vec<Value> = resume.map(|r| r.natives.clone()).unwrap_or_default();
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

        // Near the context's end: summarize the session first, keeping the new message.
        // After a summary, the next round's prompt is the new floor: another
        // summary waits until the conversation has grown well past it.
        let mut fresh = false;
        let mut floor: Option<u64> = None;
        if self.summarize_first && !self.token.is_cancelled() {
            self.summarize(&mut tr, &mut messages, true).await;
            fresh = true;
        }
        // Old tool outputs were dropped this reply (the step before summarizing).
        let mut trimmed = false;
        // The prompt size last acted on (each round's usage is checked once).
        let mut checked: Option<u64> = None;

        'rounds: for round in 0..=self.max_rounds {
            // A long reply nearing the context's end: old tool outputs go first,
            // then the session is summarized and the reply carries on from that.
            if round > 0 && self.agent.is_some() {
                if let (Some(c), Some(p)) = (self.context, prompt_tokens) {
                    if fresh && checked != Some(p) {
                        floor = Some(p);
                        fresh = false;
                        checked = Some(p);
                    }
                    let grown = floor.map_or(true, |f| p > f + c.saturating_sub(f) / 2);
                    if p as f64 >= SUMMARIZE_AT * c as f64 && checked != Some(p) && grown {
                        checked = Some(p);
                        if trimmed || !drop_old_outputs(&mut messages) {
                            self.summarize(&mut tr, &mut messages, false).await;
                            trimmed = false;
                            fresh = true;
                        } else {
                            trimmed = true;
                        }
                    }
                }
            }
            let mut body = match resume.and_then(|r| r.first_body.as_ref()).filter(|_| round == 0) {
                Some(first) => first.clone(),
                None => self.body.clone(),
            };
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
            // Going on from cut-off text, the model's thinking (how to pick up) stays
            // out of the message: it would split the text in two on screen.
            let quiet = round == 0 && resume.is_some_and(|r| r.merged);

            let result = llm::stream_chat(&self.api, &self.base, self.key.as_deref(), body, &self.token, |event| {
                match event {
                    Event::Content(text) => {
                        round_first.get_or_insert_with(Instant::now);
                        let (c, r) = split.feed(&text);
                        if !r.is_empty() && !quiet {
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
                        if !quiet {
                            round_thought.push_str(&text);
                            tr.op(Op::Append { kind: "reasoning", text });
                        }
                    }
                    Event::ToolCall { index, id, name, arguments } => {
                        round_first.get_or_insert_with(Instant::now);
                        add_call_piece(&mut calls, index, id, name, &arguments);
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
                            detail: None,
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
            if !r.is_empty() && !quiet {
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
                context_tokens = Some(p + c);
                have_usage = true;
                // The composer's meter follows the reply round by round.
                let _ = self.app.emit("chat-context", json!({ "chatId": self.chat_id, "tokens": p + c }));
            }
            // A paused round and the one continuing it are one turn: their blocks go back together.
            let blocks = round_native.take();
            if let Some(b) = &blocks {
                match natives.last_mut().and_then(Value::as_array_mut).filter(|_| paused) {
                    Some(last) => last.extend(b.iter().cloned()),
                    None if round == 0 && resume.is_some_and(|r| r.merged) => natives.push(Value::Null),
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
            // A first round that went on from a half-written answer: that answer and
            // what followed are one message from here on.
            if let Some(prefix) = resume.and_then(|r| r.prefill.as_ref()).filter(|_| round == 0) {
                if messages.last().is_some_and(|m| m["role"] == "assistant") {
                    messages.pop();
                }
                round_text = format!("{prefix}{round_text}");
            }
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
                    detail: None,
                };
                let Some(route) = route else {
                    let msg = format!("There's no tool called {}.", c.name);
                    set_tool_state(&mut part, "error", Some(msg.clone()), None);
                    tr.set_tool(part);
                    messages.push(json!({ "role": "tool", "tool_call_id": id, "content": msg }));
                    continue;
                };
                let args: Value = serde_json::from_str(&arguments).unwrap_or_else(|_| json!({}));
                if route.server_id == crate::agent::SERVER_ID {
                    match self.run_agent_tool(&core, &mut tr, &mut part, &route.tool, &args).await {
                        Some(answer) => {
                            messages.push(json!({ "role": "tool", "tool_call_id": id, "content": answer }));
                            continue;
                        }
                        None => {
                            outcome = Ok(End::Cancelled);
                            break 'rounds;
                        }
                    }
                }
                let builtin = route.server_id == skills::SERVER_ID;
                let script_skill = args.get("skill").and_then(Value::as_str).unwrap_or("").to_string();
                let allowed = match route.policy.as_str() {
                    "allow" => true,
                    "script" => skills::scripts_allowed(&core, &script_skill),
                    _ => false,
                };
                // In an Agent session: Bypass runs everything, Auto never waits
                // (what would ask is refused).
                let agent_mode = self.agent.as_ref().map(|c| c.mode(&core));
                let allowed = allowed || agent_mode == Some("bypass");
                if !allowed && agent_mode == Some("auto") {
                    let r = crate::agent::Refusal {
                        rule: "tools that ask".into(),
                        why: format!("{} asks before it runs, and nobody is there to answer in Auto.", route.server),
                        instead: "carry on without it, and mention in your final message what it would have done.".into(),
                    };
                    let text = r.text("auto");
                    part = {
                        let mut p = part;
                        set_detail(&mut p, Some(json!({ "blocked": { "rule": r.rule, "why": r.why } })));
                        set_tool_state(&mut p, "blocked", Some(text.clone()), None);
                        p
                    };
                    tr.set_tool(part);
                    messages.push(json!({ "role": "tool", "tool_call_id": id, "content": text }));
                    continue;
                }
                if !allowed {
                    set_tool_state(&mut part, "approval", None, None);
                    tr.set_tool(part.clone());
                    self.waiting(true, &format!("Wants to use {} ({})", route.tool, route.server));
                    let decision = match core.streams.wait_for(&self.message_id, id) {
                        Some(rx) => tokio::select! {
                            d = rx => d.unwrap_or_else(|_| "deny".into()),
                            _ = self.token.cancelled() => "cancel".into(),
                        },
                        None => "deny".into(),
                    };
                    self.waiting(false, "");
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
            "contextTokens": context_tokens,
            "estimated": !have_usage,
            "tokensPerSec": if gen_secs > 0.05 && completion_tokens > 0 { Some(completion_tokens as f64 / gen_secs) } else { None },
            "finishReason": finish,
            "toolCalls": tool_count,
        });
        let stats = match resume.and_then(|r| r.stats.as_ref()) {
            Some(old) => merge_stats(old, stats),
            None => stats,
        };
        let parts_json = serde_json::to_value(&parts).unwrap_or(Value::Null);
        if let Err(e) = core.db.finish_message(&self.message_id, &content, &reasoning, &parts_json, status, error.as_deref(), &stats) {
            eprintln!("chat: couldn't store the reply: {e}");
        }
        if !natives.is_empty() || resume.is_some() {
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
    fn tool_call_indexes_are_capped() {
        let mut calls = Vec::new();
        add_call_piece(&mut calls, 1, Some("c1".into()), Some("add".into()), "{\"a\"");
        add_call_piece(&mut calls, 1, None, None, ":1}");
        assert_eq!(calls.len(), 2);
        assert_eq!((calls[1].id.as_str(), calls[1].name.as_str(), calls[1].arguments.as_str()), ("c1", "add", "{\"a\":1}"));
        // A server-sent index of billions is ignored, not allocated.
        add_call_piece(&mut calls, 4_000_000_000, Some("x".into()), Some("boom".into()), "{}");
        add_call_piece(&mut calls, usize::MAX, None, None, "");
        assert_eq!(calls.len(), 2);
    }

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
            detail: None,
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
    fn context_is_the_last_request_and_its_answer() {
        // Every round's answers added up are mostly inside the last request already.
        let s = json!({ "promptTokens": 190_000, "completionTokens": 153_000, "contextTokens": 190_400 });
        assert_eq!(context_of(&s), Some(190_400));
        let old = json!({ "promptTokens": 190_000, "completionTokens": 153_000 });
        assert_eq!(context_of(&old), Some(190_000));
        assert_eq!(context_of(&json!({})), None);
    }

    #[test]
    fn summaries_replace_what_came_before() {
        let summary = |keep: bool| json!({ "type": "tool", "id": "s1", "serverId": crate::agent::SERVER_ID, "server": "Agent", "tool": "summarize", "arguments": "{}", "status": "done", "result": "Summarized", "detail": { "keepUser": keep, "summary": "Built the site." } });
        let tool = json!({ "type": "tool", "id": "c1", "serverId": crate::agent::SERVER_ID, "server": "Agent", "tool": "list", "arguments": "{}", "status": "done", "result": "a.txt" });
        let opts = History { tools: true, ..Default::default() };
        // Before a new message: the message stays, after the summary.
        let h = vec![msg("user", "old task", None), msg("assistant", "done", None), msg("user", "new task", None), msg("assistant", "ok", Some(json!([summary(true), { "type": "text", "text": "ok" }])))];
        let out = history_messages(&h, opts);
        assert_eq!(out.len(), 2, "{out:?}");
        let first = out[0]["content"].as_str().unwrap();
        assert!(first.starts_with("[This session was summarized") && first.contains("Built the site.") && first.ends_with("new task"), "{first}");
        assert_eq!(out[1]["content"], "ok");
        // Mid-reply: the summary stands in for everything, the rounds after it follow.
        let parts = json!([tool.clone(), summary(false), { "type": "text", "text": "Next." }, tool, { "type": "text", "text": "Done." }]);
        let h = vec![msg("user", "task", None), msg("assistant", "", Some(parts))];
        let out = history_messages(&h, opts);
        assert_eq!(out[0]["role"], "user");
        assert!(out[0]["content"].as_str().unwrap().contains("Carry on with the task"));
        assert_eq!(out[1]["content"], "Next.");
        assert_eq!(out[2]["role"], "tool");
        assert_eq!(out[3]["content"], "Done.");
        assert_eq!(out.len(), 4);
        // A summary that failed changes nothing.
        let failed = json!([{ "type": "tool", "id": "s1", "serverId": crate::agent::SERVER_ID, "server": "Agent", "tool": "summarize", "arguments": "{}", "status": "error", "result": "no" }, { "type": "text", "text": "hi" }]);
        let out = history_messages(&[msg("user", "a", None), msg("assistant", "hi", Some(failed))], opts);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["content"], "a");
    }

    #[test]
    fn transcripts_cut_outputs_and_files() {
        let msgs = vec![
            json!({ "role": "user", "content": "fix it" }),
            json!({ "role": "assistant", "content": null, "tool_calls": [{ "id": "1", "type": "function", "function": { "name": "agent__read_file", "arguments": "{\"path\":\"a.rs\"}" } }, { "id": "2", "type": "function", "function": { "name": "agent__powershell", "arguments": "{}" } }] }),
            json!({ "role": "tool", "tool_call_id": "1", "content": "SECRET FILE BODY" }),
            json!({ "role": "tool", "tool_call_id": "2", "content": "x".repeat(5000) }),
        ];
        let t = transcript(&msgs, 100_000);
        assert!(t.contains("USER:
fix it") && t.contains("TOOL CALL agent__read_file"));
        assert!(!t.contains("SECRET FILE BODY"));
        assert!(t.contains("characters cut"));
        assert!(transcript(&msgs, 500).chars().count() < 600);
        let mut m = vec![json!({ "role": "tool", "content": "y".repeat(400) }); 8];
        assert!(drop_old_outputs(&mut m));
        assert_eq!(m.iter().filter(|x| x["content"].as_str().unwrap().starts_with("[Output dropped")).count(), 2);
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

    fn call(id: &str, status: &str, result: Option<&str>) -> Part {
        Part::Tool {
            id: id.into(),
            server_id: "s".into(),
            server: "fs".into(),
            tool: "read".into(),
            arguments: "{}".into(),
            status: status.into(),
            result: result.map(str::to_string),
            ms: None,
            detail: None,
        }
    }

    #[test]
    fn continue_goes_on_from_text_or_the_next_step() {
        let text = |t: &str| Part::Text { text: t.into() };
        let thought = |t: &str| Part::Reasoning { text: t.into(), ms: None };

        // Cut off mid-answer: go on from the text.
        let mut parts = vec![thought("hmm"), text("The answer is")];
        assert_eq!(trim_for_continue(&mut parts), Pickup::Text);
        assert_eq!(parts.len(), 2);

        // Stopped while thinking: the half thought goes, the model thinks again.
        let mut parts = vec![thought("half a")];
        assert_eq!(trim_for_continue(&mut parts), Pickup::Fresh);
        assert!(parts.is_empty());

        // Stopped at a tool: unfinished calls go, finished ones stay.
        let mut parts = vec![text("Looking"), call("a", "done", Some("ok")), call("b", "error", Some("Stopped")), call("c", "approval", None)];
        assert_eq!(trim_for_continue(&mut parts), Pickup::Fresh);
        assert_eq!(parts.len(), 2);
        assert_eq!(tool_rounds(&parts), 1);

        // A tool that failed on its own is part of the reply.
        let mut parts = vec![call("a", "error", Some("No such file"))];
        assert_eq!(trim_for_continue(&mut parts), Pickup::Fresh);
        assert_eq!(parts.len(), 1);
    }

    #[test]
    fn rounds_and_trailing_text_follow_history_messages() {
        let text = |t: &str| Part::Text { text: t.into() };
        let parts = vec![text("a"), call("1", "done", Some("x")), call("2", "done", Some("y")), text("b"), call("3", "done", Some("z")), text("c "), text("d")];
        assert_eq!(tool_rounds(&parts), 2);
        assert_eq!(trailing_text(&parts), "c d");
        assert_eq!(trailing_text(&[text("only")]), "only");
        // history_messages sends one assistant message per tool round, then the text.
        let m = msg("assistant", "", Some(serde_json::to_value(&parts).unwrap()));
        let out = history_messages(&[m], History { tools: true, ..History::default() });
        let assistants = out.iter().filter(|m| m["role"] == "assistant").count();
        assert_eq!(assistants, tool_rounds(&parts) + 1);
    }

    #[test]
    fn continued_stats_add_up() {
        let old = json!({ "totalMs": 1000, "completionTokens": 50, "ttftMs": 200, "estimated": false });
        let new = json!({ "totalMs": 500, "completionTokens": 20, "ttftMs": 90, "estimated": true, "finishReason": "stop" });
        let s = merge_stats(&old, new);
        assert_eq!(s["totalMs"], 1500);
        assert_eq!(s["completionTokens"], 70);
        assert_eq!(s["ttftMs"], 200);
        assert_eq!(s["estimated"], true);
        assert_eq!(s["finishReason"], "stop");
    }
}

//! Anthropic's Messages API, for providers set to "Anthropic". Relay builds
//! every request in the OpenAI shape (`chat.rs`); this turns it into a
//! Messages request and the server's stream back into `llm::Event`s.
//!
//! - Thinking is adaptive with a readable summary (`display: summarized`),
//!   its depth set by `output_config.effort`, fitted to what the model says it
//!   supports (`/v1/models` capabilities). Older models get a token budget.
//! - A reply's blocks come back as `Event::Native`; Relay stores them per
//!   round and sends them back unchanged (`_native` on an assistant message),
//!   because thinking only counts with its signature.
//! - Prompt caching: a marker on the system prompt and the request's automatic
//!   breakpoint, both with the TTL from Settings.
//! - Web search: Anthropic's own `web_search` and `web_fetch` tools, shown as
//!   tool cards (`Event::Hosted`).
//! - A 400 or 422 gets the request again with fewer extras, step by step; the
//!   step that worked is remembered for the session (per address and model).
//!
//! Relay's private request keys (`_effort`, `_cache`, `_web`, `_native`)
//! never reach a server: `llm` strips them on the OpenAI path.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::llm::{self, End, Event};

pub const VERSION: &str = "2026-10-21";
/// Lets a request ask the API to drop thinking blocks its history no longer
/// matches (a changed system prompt or tool list) instead of failing.
const BINDING_BETA: &str = "thinking-binding-controls-2026-08-01";
/// The server id of Anthropic's own tools, on their tool cards.
pub const HOSTED: &str = "anthropic.hosted";
/// Output tokens a reply may use when neither the preset nor the model says.
const DEFAULT_MAX_TOKENS: u64 = 32_000;
/// A reply streams, so it may be long; still, no more than this.
const MAX_TOKENS_CAP: u64 = 64_000;
/// The extras dropped step by step after a rejection (see `build`).
const LAST_STEP: u8 = 4;

/// Anthropic's own API, which has the server tools (web search and fetch)
/// and the first-party extras; servers that copy the Messages API don't.
pub fn is_first_party(base: &str) -> bool {
    llm::normalize_base(base).contains("api.anthropic.com")
}

fn request(base: &str, path: &str, key: Option<&str>) -> reqwest::RequestBuilder {
    let mut req = llm::http().post(format!("{}{path}", llm::normalize_base(base))).header("anthropic-version", VERSION);
    if let Some(key) = key.filter(|k| !k.is_empty()) {
        req = req.header("x-api-key", key);
        // Servers that copy the Messages API often want the usual header.
        if !is_first_party(base) {
            req = req.bearer_auth(key);
        }
    }
    req
}

// ---- what a model supports ----------------------------------------------------------

/// What `/v1/models` says about a model, kept for the session.
#[derive(Clone, Debug, Default)]
pub struct Info {
    pub max_tokens: Option<u64>,
    /// None: the server didn't say.
    pub adaptive: Option<bool>,
    pub budget: Option<bool>,
    /// The effort levels it takes; None: the server didn't say.
    pub efforts: Option<Vec<String>>,
}

fn info_from(m: &Value) -> Info {
    let sup = |p: &str| m.pointer(p).and_then(Value::as_bool);
    let efforts = m.pointer("/capabilities/effort").and_then(Value::as_object).map(|e| {
        if e.get("supported").and_then(Value::as_bool) == Some(false) {
            return Vec::new();
        }
        ["low", "medium", "high", "xhigh", "max"]
            .into_iter()
            .filter(|l| e.get(*l).and_then(|v| v.get("supported")).and_then(Value::as_bool) == Some(true))
            .map(str::to_string)
            .collect()
    });
    Info {
        max_tokens: m.get("max_tokens").and_then(Value::as_u64),
        adaptive: sup("/capabilities/thinking/types/adaptive/supported"),
        budget: sup("/capabilities/thinking/types/enabled/supported"),
        efforts,
    }
}

async fn info(base: &str, key: Option<&str>, model: &str) -> Info {
    static CACHE: OnceLock<Mutex<HashMap<String, Info>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let memo = format!("{}|{model}", llm::normalize_base(base));
    if let Some(i) = cache.lock().unwrap().get(&memo) {
        return i.clone();
    }
    let url = format!("{}/models/{model}", llm::normalize_base(base));
    let mut req = llm::http().get(url).timeout(Duration::from_secs(6)).header("anthropic-version", VERSION);
    if let Some(k) = key.filter(|k| !k.is_empty()) {
        req = req.header("x-api-key", k);
    }
    let found = match req.send().await {
        Ok(r) if r.status().is_success() => crate::limits::json(r, crate::limits::MAX_JSON).await.ok().map(|v| info_from(&v)),
        _ => None,
    };
    let i = found.unwrap_or_default();
    cache.lock().unwrap().insert(memo, i.clone());
    i
}

// ---- the request ----------------------------------------------------------------------

/// The Messages request for Relay's OpenAI-shaped `body`. `step` drops extras
/// after rejections: 1 a gentler effort (and thinking on where "off" was
/// refused), 2 no history binding and the basic web tools, 3 no prompt
/// caching, 4 no thinking settings and no web tools.
pub fn build(base: &str, body: &Value, info: &Info, step: u8) -> (Value, Vec<&'static str>) {
    let model = body.get("model").cloned().unwrap_or(Value::Null);
    let raw = body.get("messages").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut tools: Vec<Value> = body
        .get("tools")
        .and_then(Value::as_array)
        .map(|specs| {
            specs
                .iter()
                .filter_map(|s| {
                    let f = s.get("function")?;
                    Some(json!({
                        "name": f.get("name")?,
                        "description": f.get("description").cloned().unwrap_or(json!("")),
                        "input_schema": f.get("parameters").cloned().unwrap_or(json!({ "type": "object", "properties": {} })),
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    let has_client_tools = !tools.is_empty();
    let web = body.get("_web").and_then(Value::as_bool).unwrap_or(false) && step < 4;
    if web {
        let (search, fetch) = if step >= 2 { ("web_search_20250305", "web_fetch_20250910") } else { ("web_search_20260209", "web_fetch_20260209") };
        tools.push(json!({ "type": search, "name": "web_search" }));
        tools.push(json!({ "type": fetch, "name": "web_fetch" }));
    }
    let (system, messages) = convert(&raw, has_client_tools);

    let ttl = body.get("_cache").and_then(Value::as_str).unwrap_or("5m");
    let cache = if ttl == "1h" { json!({ "type": "ephemeral", "ttl": "1h" }) } else { json!({ "type": "ephemeral" }) };
    let caching = step < 3;

    let max_tokens = body
        .get("max_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| info.max_tokens.map(|m| m.min(MAX_TOKENS_CAP)).unwrap_or(DEFAULT_MAX_TOKENS));
    let mut req = json!({ "model": model, "max_tokens": max_tokens, "messages": messages, "stream": true });
    if !system.is_empty() {
        let mut block = json!({ "type": "text", "text": system });
        if caching {
            block["cache_control"] = cache.clone();
        }
        req["system"] = json!([block]);
    }
    if !tools.is_empty() {
        req["tools"] = json!(tools);
    }
    if caching {
        req["cache_control"] = cache;
    }

    let mut betas = Vec::new();
    let effort = llm_effort(body.get("_effort").and_then(Value::as_str).unwrap_or("medium"));
    // Adaptive thinking unless the model says it only takes a budget.
    let adaptive = info.adaptive.unwrap_or(info.budget != Some(true));
    if step < 4 && adaptive {
        // Adaptive thinking, with a readable summary and the effort as its depth.
        if effort == "off" && step < 1 {
            req["thinking"] = json!({ "type": "disabled" });
        } else {
            let mut thinking = json!({ "type": "adaptive", "display": "summarized" });
            if step < 2 && is_first_party(base) {
                thinking["block_binding"] = json!({ "prefix_mismatch_behavior": "drop_block" });
                betas.push(BINDING_BETA);
            }
            req["thinking"] = thinking;
            let wanted = if effort == "off" { "low" } else { effort };
            let wanted = if step >= 1 && matches!(wanted, "xhigh" | "max") { "high" } else { wanted };
            if let Some(level) = fit_effort(wanted, info.efforts.as_deref()) {
                req["output_config"] = json!({ "effort": level });
            }
        }
    } else if step < 4 && info.budget == Some(true) && effort != "off" {
        // Older models think within a token budget.
        let budget = match effort {
            "low" => 2_048,
            "medium" => 6_000,
            "high" => 16_000,
            _ => 32_000,
        }
        .min(max_tokens.saturating_sub(1024))
        .max(1024);
        if max_tokens > 2048 {
            req["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
        }
    }
    (req, betas)
}

fn llm_effort(e: &str) -> &'static str {
    crate::chat::normalize_effort(e)
}

/// The level the model takes nearest below the one wanted (or above, when
/// nothing is below). None: the model takes no effort setting.
fn fit_effort(wanted: &str, supported: Option<&[String]>) -> Option<String> {
    let order = ["low", "medium", "high", "xhigh", "max"];
    let Some(list) = supported else { return Some(wanted.to_string()) };
    if list.is_empty() {
        return None;
    }
    let at = order.iter().position(|l| *l == wanted).unwrap_or(1);
    order[..=at].iter().rev().chain(order[at + 1..].iter()).find(|l| list.iter().any(|s| s == *l)).map(|l| l.to_string())
}

/// Relay's OpenAI-shaped messages as (system prompt, Messages API messages).
/// An assistant message carrying `_native` (the blocks its server sent) goes
/// back as those blocks, unless they call tools this request doesn't define.
pub fn convert(raw: &[Value], has_tools: bool) -> (String, Vec<Value>) {
    let mut system = String::new();
    let mut out: Vec<Value> = Vec::new();
    let push = |out: &mut Vec<Value>, role: &str, blocks: Vec<Value>| {
        if blocks.is_empty() {
            return;
        }
        if let Some(last) = out.last_mut().filter(|m| m["role"] == role) {
            if let Some(arr) = last["content"].as_array_mut() {
                arr.extend(blocks);
                return;
            }
        }
        out.push(json!({ "role": role, "content": blocks }));
    };
    for m in raw {
        match m.get("role").and_then(Value::as_str).unwrap_or("") {
            "system" => {
                let t = text_of(&m["content"]);
                if !t.trim().is_empty() {
                    if !system.is_empty() {
                        system.push_str("\n\n");
                    }
                    system.push_str(t.trim());
                }
            }
            "user" => {
                let blocks = user_blocks(&m["content"]);
                push(&mut out, "user", blocks);
            }
            "assistant" => {
                let native = m.get("_native").and_then(Value::as_array).filter(|blocks| {
                    !blocks.is_empty() && (has_tools || !blocks.iter().any(|b| b["type"] == "tool_use"))
                });
                let blocks = match native {
                    Some(n) => n.clone(),
                    None => assistant_blocks(m, has_tools),
                };
                push(&mut out, "assistant", blocks);
            }
            "tool" => {
                let id = m.get("tool_call_id").and_then(Value::as_str).unwrap_or("");
                let content = text_of(&m["content"]);
                let block = json!({ "type": "tool_result", "tool_use_id": id, "content": if content.is_empty() { "(no output)".to_string() } else { content } });
                push(&mut out, "user", vec![block]);
            }
            _ => {}
        }
    }
    // The conversation starts with the user.
    if out.first().is_some_and(|m| m["role"] != "user") {
        out.insert(0, json!({ "role": "user", "content": [{ "type": "text", "text": "(The conversation continues.)" }] }));
    }
    (system, out)
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

fn text_block(t: &str) -> Option<Value> {
    (!t.trim().is_empty()).then(|| json!({ "type": "text", "text": t }))
}

/// A `data:` URL's (media type, base64 data).
fn data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let media = meta.strip_suffix(";base64")?;
    Some((media.to_string(), data.to_string()))
}

fn user_blocks(content: &Value) -> Vec<Value> {
    let Value::Array(parts) = content else {
        return text_block(&text_of(content)).into_iter().collect();
    };
    let mut out = Vec::new();
    for p in parts {
        match p.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => out.extend(text_block(p.get("text").and_then(Value::as_str).unwrap_or(""))),
            "image_url" => {
                let url = p.pointer("/image_url/url").and_then(Value::as_str).unwrap_or("");
                if let Some((media, data)) = data_url(url) {
                    out.push(json!({ "type": "image", "source": { "type": "base64", "media_type": media, "data": data } }));
                } else if url.starts_with("http") {
                    out.push(json!({ "type": "image", "source": { "type": "url", "url": url } }));
                }
            }
            "file" => {
                let name = p.pointer("/file/filename").and_then(Value::as_str).unwrap_or("file");
                let url = p.pointer("/file/file_data").and_then(Value::as_str).unwrap_or("");
                match data_url(url) {
                    Some((media, data)) if media == "application/pdf" => {
                        out.push(json!({ "type": "document", "source": { "type": "base64", "media_type": media, "data": data }, "title": name }))
                    }
                    _ => out.extend(text_block(&format!("[{name} is attached, in a format this model can't read]"))),
                }
            }
            "input_audio" => out.extend(text_block("[A sound recording is attached; this model can't hear it]")),
            "video_url" => out.extend(text_block("[A video is attached; this model can't watch it]")),
            _ => {}
        }
    }
    out
}

fn assistant_blocks(m: &Value, has_tools: bool) -> Vec<Value> {
    let mut out: Vec<Value> = text_block(&text_of(&m["content"])).into_iter().collect();
    for c in m.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
        let name = c.pointer("/function/name").and_then(Value::as_str).unwrap_or("");
        let args = c.pointer("/function/arguments").and_then(Value::as_str).unwrap_or("{}");
        let input: Value = serde_json::from_str(args).ok().filter(Value::is_object).unwrap_or_else(|| json!({}));
        if has_tools {
            out.push(json!({ "type": "tool_use", "id": c.get("id").cloned().unwrap_or(json!("")), "name": name, "input": input }));
        }
    }
    out
}

// ---- the stream ---------------------------------------------------------------------------

/// Endpoints (address + model) and the step their requests start at, learned
/// from rejections this session.
fn learned() -> &'static Mutex<HashMap<String, u8>> {
    static M: OnceLock<Mutex<HashMap<String, u8>>> = OnceLock::new();
    M.get_or_init(Default::default)
}

pub async fn stream_chat(base: &str, key: Option<&str>, body: Value, cancel: &CancellationToken, mut on: impl FnMut(Event)) -> Result<End, String> {
    let model = body.get("model").and_then(Value::as_str).unwrap_or("").to_string();
    let info = if is_first_party(base) { info(base, key, &model).await } else { Info::default() };
    let memo = format!("{}|{model}", llm::normalize_base(base));
    let mut step = learned().lock().unwrap().get(&memo).copied().unwrap_or(0);
    let (mut resp, mut first_error) = (None, None);
    while step <= LAST_STEP {
        let (req, betas) = build(base, &body, &info, step);
        let mut r = request(base, "/messages", key).json(&req);
        if !betas.is_empty() {
            r = r.header("anthropic-beta", betas.join(","));
        }
        let sent = tokio::select! {
            r = r.send() => r.map_err(|e| llm::connect_error(e, base))?,
            _ = cancel.cancelled() => return Ok(End::Cancelled),
        };
        let status = sent.status();
        if status.is_success() {
            if step > 0 {
                learned().lock().unwrap().insert(memo.clone(), step);
            }
            resp = Some(sent);
            break;
        }
        let why = llm::failure(sent).await;
        if !llm::is_rejection(status) {
            return Err(why);
        }
        first_error.get_or_insert(why);
        step += 1;
    }
    let Some(resp) = resp else { return Err(first_error.unwrap_or_else(|| "The server refused the request".into())) };

    let mut acc = Acc::default();
    let mut stream = resp.bytes_stream();
    let mut lines = crate::limits::Lines::default();
    loop {
        let chunk = tokio::select! {
            c = llm::next_chunk(&mut stream) => c?,
            _ = cancel.cancelled() => return Ok(End::Cancelled),
        };
        let Some(chunk) = chunk else { break };
        for line in lines.push(chunk.as_ref(), crate::limits::MAX_JSON)? {
            if let Some(data) = line.trim().strip_prefix("data:") {
                if acc.event(data.trim(), &mut on)? {
                    return Ok(End::Done);
                }
            }
        }
    }
    // A stream that ended without message_stop: keep what came.
    acc.end(&mut on);
    Ok(End::Done)
}

/// A one-off answer (chat titles): the text of a short streamed reply.
pub async fn complete(base: &str, key: Option<&str>, mut body: Value) -> Result<String, String> {
    body["_effort"] = json!("low");
    // Current models always think a little first: a title's 32 tokens wouldn't fit.
    let asked = body.get("max_tokens").and_then(Value::as_u64).unwrap_or(0);
    body["max_tokens"] = json!(asked.max(4_000));
    let mut text = String::new();
    let token = CancellationToken::new();
    stream_chat(base, key, body, &token, |e| {
        if let Event::Content(t) = e {
            text.push_str(&t);
        }
    })
    .await?;
    Ok(text)
}

/// Content blocks one reply may hold (the server says which index each is).
const MAX_BLOCKS: usize = 1024;

/// The reply as it streams: its blocks (sent back later) and the pieces of
/// JSON that tool inputs arrive in.
#[derive(Default)]
struct Acc {
    blocks: Vec<Value>,
    json: HashMap<usize, String>,
    /// Block index -> tool call index (Relay's own tools).
    calls: HashMap<usize, usize>,
    hosted: HashSet<usize>,
    input_tokens: u64,
    output_tokens: u64,
    stop: Option<String>,
    ended: bool,
}

impl Acc {
    /// Handles one event's data. `Ok(true)` at the end of the message.
    fn event(&mut self, data: &str, on: &mut impl FnMut(Event)) -> Result<bool, String> {
        let Ok(v) = serde_json::from_str::<Value>(data) else { return Ok(false) };
        let index = v.get("index").and_then(Value::as_u64).map(|i| i as usize);
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_start" => {
                let u = v.pointer("/message/usage").cloned().unwrap_or(Value::Null);
                let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                self.input_tokens = n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens");
                self.output_tokens = n("output_tokens");
            }
            "content_block_start" => {
                // The index and the block come from the server: a huge index
                // or a block that isn't an object is dropped (writing keys into
                // a string or number would panic).
                let Some(i) = index.filter(|i| *i < MAX_BLOCKS) else { return Ok(false) };
                let Some(block) = v.get("content_block").filter(|b| b.is_object()).cloned() else { return Ok(false) };
                if self.blocks.len() <= i {
                    self.blocks.resize(i + 1, Value::Null);
                }
                match block.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text" => {
                        if let Some(t) = block.get("text").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                            on(Event::Content(t.to_string()));
                        }
                    }
                    "thinking" => {
                        if let Some(t) = block.get("thinking").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                            on(Event::Reasoning(t.to_string()));
                        }
                    }
                    "tool_use" => {
                        let n = self.calls.len();
                        self.calls.insert(i, n);
                        on(Event::ToolCall {
                            index: n,
                            id: block.get("id").and_then(Value::as_str).map(str::to_string),
                            name: block.get("name").and_then(Value::as_str).map(str::to_string),
                            arguments: String::new(),
                        });
                    }
                    "server_tool_use" => {
                        self.hosted.insert(i);
                        on(Event::Hosted {
                            id: block.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                            name: block.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                            input: None,
                            result: None,
                        });
                    }
                    t if t.ends_with("_tool_result") => {
                        let id = block.get("tool_use_id").and_then(Value::as_str).unwrap_or("").to_string();
                        let (text, failed) = summarize_result(&block);
                        on(Event::Hosted { id, name: String::new(), input: None, result: Some((text, failed)) });
                    }
                    _ => {}
                }
                self.blocks[i] = block;
            }
            "content_block_delta" => {
                let Some(i) = index.filter(|i| *i < self.blocks.len()) else { return Ok(false) };
                let d = v.get("delta").cloned().unwrap_or(Value::Null);
                let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                match d.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text_delta" => {
                        let t = s("text");
                        append(&mut self.blocks[i], "text", &t);
                        on(Event::Content(t));
                    }
                    "thinking_delta" => {
                        let t = s("thinking");
                        append(&mut self.blocks[i], "thinking", &t);
                        on(Event::Reasoning(t));
                    }
                    "signature_delta" => set_key(&mut self.blocks[i], "signature", json!(s("signature"))),
                    "input_json_delta" => {
                        let piece = s("partial_json");
                        self.json.entry(i).or_default().push_str(&piece);
                        if let Some(&n) = self.calls.get(&i) {
                            on(Event::ToolCall { index: n, id: None, name: None, arguments: piece });
                        }
                    }
                    "citations_delta" => {
                        if let Some(c) = d.get("citation") {
                            let b = &mut self.blocks[i];
                            if !b.get("citations").is_some_and(Value::is_array) {
                                set_key(b, "citations", json!([]));
                            }
                            if let Some(list) = b.get_mut("citations").and_then(Value::as_array_mut) {
                                list.push(c.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let Some(i) = index.filter(|i| *i < self.blocks.len()) else { return Ok(false) };
                if let Some(text) = self.json.remove(&i) {
                    let input: Value = serde_json::from_str(&text).ok().filter(Value::is_object).unwrap_or_else(|| json!({}));
                    set_key(&mut self.blocks[i], "input", input.clone());
                    if self.hosted.contains(&i) {
                        let b = &self.blocks[i];
                        on(Event::Hosted {
                            id: b.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                            name: b.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                            input: Some(input.to_string()),
                            result: None,
                        });
                    }
                }
            }
            "message_delta" => {
                if let Some(r) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop = Some(r.to_string());
                }
                if let Some(n) = v.pointer("/usage/output_tokens").and_then(Value::as_u64) {
                    self.output_tokens = n;
                }
                // Some servers send the input only at the end.
                if self.input_tokens == 0 {
                    let u = v.get("usage").cloned().unwrap_or(Value::Null);
                    let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                    self.input_tokens = n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens");
                }
            }
            "message_stop" => {
                self.end(on);
                return Ok(true);
            }
            "error" => {
                let msg = v.pointer("/error/message").and_then(Value::as_str).unwrap_or("The server stopped with an error");
                return Err(msg.to_string());
            }
            _ => {}
        }
        Ok(false)
    }

    fn end(&mut self, on: &mut impl FnMut(Event)) {
        if self.ended {
            return;
        }
        self.ended = true;
        on(Event::Usage { prompt: self.input_tokens, completion: self.output_tokens });
        let finish = match self.stop.as_deref() {
            Some("end_turn") | Some("stop_sequence") | None => "stop",
            Some("tool_use") => "tool_calls",
            Some("max_tokens") => "length",
            Some(other) => other,
        };
        on(Event::Finish(finish.to_string()));
        let blocks: Vec<Value> = std::mem::take(&mut self.blocks).into_iter().filter(|b| !b.is_null()).collect();
        if !blocks.is_empty() {
            on(Event::Native(blocks));
        }
    }
}

fn append(block: &mut Value, key: &str, text: &str) {
    let cur = block.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    set_key(block, key, json!(cur + text));
}

/// `block[key] = value`, for blocks that are objects (or still empty); never
/// panics on a value the server sent in another shape.
fn set_key(block: &mut Value, key: &str, value: Value) {
    if block.is_null() {
        *block = json!({});
    }
    if let Some(obj) = block.as_object_mut() {
        obj.insert(key.to_string(), value);
    }
}

/// A web search's or page read's result as a few lines for its tool card.
fn summarize_result(block: &Value) -> (String, bool) {
    let content = block.get("content").cloned().unwrap_or(Value::Null);
    if let Some(code) = content.get("error_code").and_then(Value::as_str) {
        return (format!("Failed: {}", code.replace('_', " ")), true);
    }
    match content {
        Value::Array(items) => {
            let lines: Vec<String> = items
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let title = r.get("title").and_then(Value::as_str).unwrap_or("");
                    let url = r.get("url").and_then(Value::as_str).unwrap_or("");
                    format!("{}. {title}\n   {url}", i + 1)
                })
                .collect();
            if lines.is_empty() {
                ("No results".into(), false)
            } else {
                (lines.join("\n"), false)
            }
        }
        Value::Object(_) => {
            let url = content.get("url").and_then(Value::as_str).unwrap_or("");
            let title = content.pointer("/content/title").and_then(Value::as_str).unwrap_or("");
            let text = content.pointer("/content/source/data").and_then(Value::as_str).unwrap_or("");
            let preview: String = text.chars().take(1500).collect();
            (format!("{title}\n{url}\n\n{preview}").trim().to_string(), false)
        }
        _ => (String::new(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opus() -> Info {
        Info {
            max_tokens: Some(128_000),
            adaptive: Some(true),
            budget: Some(false),
            efforts: Some(["low", "medium", "high", "xhigh", "max"].map(String::from).to_vec()),
        }
    }

    #[test]
    fn builds_a_messages_request() {
        let body = json!({
            "model": "claude-opus-5-5",
            "_effort": "high",
            "_cache": "1h",
            "_web": true,
            "messages": [
                { "role": "system", "content": "Be brief." },
                { "role": "user", "content": [
                    { "type": "text", "text": "What's this?" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } },
                    { "type": "file", "file": { "filename": "a.pdf", "file_data": "data:application/pdf;base64,BBBB" } }
                ]},
                { "role": "assistant", "content": null, "tool_calls": [{ "id": "t1", "type": "function", "function": { "name": "web__search", "arguments": "{\"query\":\"x\"}" } }] },
                { "role": "tool", "tool_call_id": "t1", "content": "found" },
                { "role": "assistant", "content": "Done.", "_native": [{ "type": "thinking", "thinking": "hm", "signature": "sig" }, { "type": "text", "text": "Done." }] },
                { "role": "user", "content": "Thanks" }
            ],
            "tools": [{ "type": "function", "function": { "name": "web__search", "description": "d", "parameters": { "type": "object" } } }]
        });
        let (req, betas) = build("https://api.anthropic.com/v1", &body, &opus(), 0);
        assert_eq!(betas, vec![BINDING_BETA]);
        assert_eq!(req["system"][0]["text"], "Be brief.");
        assert_eq!(req["system"][0]["cache_control"]["ttl"], "1h");
        assert_eq!(req["cache_control"]["ttl"], "1h");
        assert_eq!(req["max_tokens"], 64_000);
        assert_eq!(req["thinking"]["type"], "adaptive");
        assert_eq!(req["thinking"]["display"], "summarized");
        assert_eq!(req["output_config"]["effort"], "high");
        let m = req["messages"].as_array().unwrap();
        assert_eq!(m.len(), 5);
        assert_eq!(m[0]["content"][1]["source"]["media_type"], "image/png");
        assert_eq!(m[0]["content"][2]["type"], "document");
        assert_eq!(m[1]["content"][0]["type"], "tool_use");
        assert_eq!(m[1]["content"][0]["input"]["query"], "x");
        assert_eq!(m[2]["content"][0]["type"], "tool_result");
        assert_eq!(m[3]["content"][0]["signature"], "sig");
        let tools = req["tools"].as_array().unwrap();
        assert_eq!(tools[0]["input_schema"]["type"], "object");
        assert_eq!(tools[1]["type"], "web_search_20260209");
        assert!(req.get("_effort").is_none() && req.get("_web").is_none());
    }

    #[test]
    fn steps_drop_extras() {
        let body = json!({ "model": "m", "_effort": "max", "_web": true, "messages": [{ "role": "user", "content": "hi" }] });
        let (r1, _) = build("https://api.anthropic.com/v1", &body, &opus(), 1);
        assert_eq!(r1["output_config"]["effort"], "high");
        let (r2, b2) = build("https://api.anthropic.com/v1", &body, &opus(), 2);
        assert!(b2.is_empty() && r2["thinking"].get("block_binding").is_none());
        assert_eq!(r2["tools"][0]["type"], "web_search_20250305");
        let (r3, _) = build("https://api.anthropic.com/v1", &body, &opus(), 3);
        assert!(r3.get("cache_control").is_none());
        let (r4, _) = build("https://api.anthropic.com/v1", &body, &opus(), 4);
        assert!(r4.get("thinking").is_none() && r4.get("tools").is_none());
    }

    #[test]
    fn effort_off_and_older_models() {
        let body = json!({ "model": "m", "_effort": "off", "messages": [{ "role": "user", "content": "hi" }] });
        let (r, _) = build("https://api.anthropic.com/v1", &body, &opus(), 0);
        assert_eq!(r["thinking"]["type"], "disabled");
        // Refused (thinking can't be off): thinking at the lowest effort.
        let (r, _) = build("https://api.anthropic.com/v1", &body, &opus(), 1);
        assert_eq!(r["output_config"]["effort"], "low");
        let haiku = Info { max_tokens: Some(64_000), adaptive: Some(false), budget: Some(true), efforts: Some(Vec::new()) };
        let body = json!({ "model": "claude-haiku-4-5", "_effort": "high", "messages": [{ "role": "user", "content": "hi" }] });
        let (r, _) = build("https://api.anthropic.com/v1", &body, &haiku, 0);
        assert_eq!(r["thinking"]["type"], "enabled");
        assert_eq!(r["thinking"]["budget_tokens"], 16_000);
        assert!(r.get("output_config").is_none());
        // A server that copies the API: no binding beta, no first-party extras.
        let (_, betas) = build("http://localhost:8080/v1", &body, &Info::default(), 0);
        assert!(betas.is_empty());
    }

    #[test]
    fn fits_effort() {
        let some = ["low", "medium", "high"].map(String::from).to_vec();
        assert_eq!(fit_effort("max", Some(&some)).as_deref(), Some("high"));
        assert_eq!(fit_effort("medium", None).as_deref(), Some("medium"));
        assert_eq!(fit_effort("high", Some(&[])), None);
    }

    #[test]
    fn native_tool_use_needs_tools() {
        let raw = vec![
            json!({ "role": "user", "content": "hi" }),
            json!({ "role": "assistant", "content": "ok", "_native": [{ "type": "tool_use", "id": "a", "name": "x", "input": {} }] }),
        ];
        let (_, m) = convert(&raw, false);
        assert_eq!(m[1]["content"], json!([{ "type": "text", "text": "ok" }]));
    }

    #[test]
    fn malformed_streams_neither_panic_nor_allocate() {
        let mut acc = Acc::default();
        let lines = [
            // A block that isn't an object, then deltas that write keys into it.
            r#"{"type":"content_block_start","index":0,"content_block":"x"}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"S"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"citations_delta","citation":{"url":"u"}}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"t"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            // An index of billions.
            r#"{"type":"content_block_start","index":4000000000,"content_block":{"type":"text","text":"no"}}"#,
            r#"{"type":"content_block_start","index":18446744073709551615,"content_block":{"type":"tool_use","id":"t","name":"n"}}"#,
        ];
        for l in lines {
            acc.event(l, &mut |_| {}).unwrap();
        }
        assert_eq!(acc.blocks.len(), 2);
        assert_eq!(acc.blocks[0]["signature"], "S");
    }

    #[test]
    fn reads_the_stream() {
        let mut acc = Acc::default();
        let mut events = Vec::new();
        let lines = [
            r#"{"type":"message_start","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":90,"output_tokens":1}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me see"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"SIG"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"server_tool_use","id":"srv1","name":"web_search","input":{}}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"query\":\"rust\"}"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"web_search_tool_result","tool_use_id":"srv1","content":[{"type":"web_search_result","title":"Rust","url":"https://rust-lang.org","encrypted_content":"x"}]}}"#,
            r#"{"type":"content_block_stop","index":2}"#,
            r#"{"type":"content_block_start","index":3,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":3,"delta":{"type":"text_delta","text":"Hi"}}"#,
            r#"{"type":"content_block_stop","index":3}"#,
            r#"{"type":"content_block_start","index":4,"content_block":{"type":"tool_use","id":"tu1","name":"skills__load","input":{}}}"#,
            r#"{"type":"content_block_delta","index":4,"delta":{"type":"input_json_delta","partial_json":"{\"name\":"}}"#,
            r#"{"type":"content_block_delta","index":4,"delta":{"type":"input_json_delta","partial_json":"\"pdf\"}"}}"#,
            r#"{"type":"content_block_stop","index":4}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":42}}"#,
            r#"{"type":"message_stop"}"#,
        ];
        let mut done = false;
        for l in lines {
            done = acc.event(l, &mut |e| events.push(e)).unwrap();
        }
        assert!(done);
        let mut text = String::new();
        let mut thought = String::new();
        let mut args = String::new();
        let mut native = Vec::new();
        let mut usage = (0, 0);
        let mut finish = String::new();
        let mut hosted = Vec::new();
        for e in events {
            match e {
                Event::Content(t) => text.push_str(&t),
                Event::Reasoning(t) => thought.push_str(&t),
                Event::ToolCall { arguments, .. } => args.push_str(&arguments),
                Event::Usage { prompt, completion } => usage = (prompt, completion),
                Event::Finish(f) => finish = f,
                Event::Native(b) => native = b,
                Event::Hosted { input, result, .. } => hosted.push((input, result)),
            }
        }
        assert_eq!((text.as_str(), thought.as_str(), args.as_str()), ("Hi", "Let me see", r#"{"name":"pdf"}"#));
        assert_eq!(usage, (100, 42));
        assert_eq!(finish, "tool_calls");
        assert_eq!(native.len(), 5);
        assert_eq!(native[0]["signature"], "SIG");
        assert_eq!(native[1]["input"]["query"], "rust");
        assert_eq!(native[4]["input"]["name"], "pdf");
        assert_eq!(hosted.len(), 3);
        assert_eq!(hosted[1].0.as_deref(), Some(r#"{"query":"rust"}"#));
        assert!(hosted[2].1.as_ref().unwrap().0.contains("rust-lang.org"));
    }
}

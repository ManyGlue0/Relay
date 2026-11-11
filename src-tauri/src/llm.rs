//! The OpenAI-compatible wire protocol: listing models and streaming chat
//! completions over server-sent events. Ollama, LM Studio, llama.cpp, vLLM,
//! OpenAI, OpenRouter and friends all speak it.

use std::sync::OnceLock;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("Relay/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default()
    })
}

pub fn normalize_base(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

fn request(method: reqwest::Method, base: &str, path: &str, key: Option<&str>) -> reqwest::RequestBuilder {
    let mut req = client().request(method, format!("{}{path}", normalize_base(base)));
    if let Some(key) = key.filter(|k| !k.is_empty()) {
        req = req.bearer_auth(key);
        if normalize_base(base).contains("api.anthropic.com") {
            req = req.header("x-api-key", key).header("anthropic-version", "2023-06-01");
        }
    }
    req
}

/// A readable reason from a failed response: the API's own message when it
/// sent one, else the status.
async fn failure(resp: reqwest::Response) -> String {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let message = serde_json::from_str::<Value>(&body).ok().and_then(|v| {
        v.pointer("/error/message")
            .or_else(|| v.get("error").filter(|e| e.is_string()))
            .or_else(|| v.get("message"))
            .or_else(|| v.get("detail"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let reason = status.canonical_reason().unwrap_or("Error");
    match message {
        Some(m) if !m.trim().is_empty() => format!("{} {reason}: {}", status.as_u16(), m.trim()),
        _ if !body.trim().is_empty() && body.len() < 300 => format!("{} {reason}: {}", status.as_u16(), body.trim()),
        _ => format!("{} {reason}", status.as_u16()),
    }
}

fn connect_error(e: reqwest::Error, base: &str) -> String {
    if e.is_connect() {
        format!("Couldn't reach {}. Is the server running?", normalize_base(base))
    } else if e.is_timeout() {
        "The server took too long to answer".into()
    } else {
        e.to_string()
    }
}

/// Why a model list didn't come: the server never answered, or it answered
/// without one (an error, or not JSON).
enum ListError {
    Unreachable(String),
    Answered(String),
}

/// The ids the endpoint offers, sorted.
pub async fn list_models(base: &str, key: Option<&str>) -> Result<Vec<String>, String> {
    models_of(base, key, Duration::from_secs(15)).await.map_err(|e| match e {
        ListError::Unreachable(m) | ListError::Answered(m) => m,
    })
}

/// Is the server there right now, and what does it list? `(false, None)`:
/// unreachable. `(true, None)`: it answered but lists no models (some
/// servers don't), so models added by name count as available.
pub async fn probe(base: &str, key: Option<&str>) -> (bool, Option<Vec<String>>) {
    match models_of(base, key, Duration::from_secs(5)).await {
        Ok(ids) => (true, Some(ids)),
        Err(ListError::Unreachable(_)) => (false, None),
        Err(ListError::Answered(_)) => (true, None),
    }
}

async fn models_of(base: &str, key: Option<&str>, timeout: Duration) -> Result<Vec<String>, ListError> {
    let resp = request(reqwest::Method::GET, base, "/models", key)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| ListError::Unreachable(connect_error(e, base)))?;
    if !resp.status().is_success() {
        let not_found = resp.status() == reqwest::StatusCode::NOT_FOUND;
        let mut msg = failure(resp).await;
        if not_found && !normalize_base(base).ends_with("/v1") {
            msg.push_str(". Most servers want the address to end in /v1.");
        }
        return Err(ListError::Answered(msg));
    }
    let body: Value = resp.json().await.map_err(|_| ListError::Answered("The server's model list wasn't JSON".to_string()))?;
    let list = body.get("data").or_else(|| body.get("models")).and_then(Value::as_array).cloned().unwrap_or_default();
    let mut ids: Vec<String> = list
        .iter()
        .filter_map(|m| m.get("id").or_else(|| m.get("name")).and_then(Value::as_str).map(str::to_string))
        .collect();
    ids.sort_by_key(|s| s.to_lowercase());
    ids.dedup();
    Ok(ids)
}

pub enum Event {
    Content(String),
    Reasoning(String),
    /// A piece of a tool call: calls arrive split across chunks, keyed by index.
    ToolCall { index: usize, id: Option<String>, name: Option<String>, arguments: String },
    Usage { prompt: u64, completion: u64 },
    Finish(String),
}

pub enum End {
    Done,
    Cancelled,
}

/// Fields Relay adds that some servers reject. A 400 or 422 gets the request
/// again without them.
pub const OPTIONAL_FIELDS: &[&str] = &["stream_options", "reasoning_effort", "chat_template_kwargs", "think"];

/// Endpoints (address + model) known to reject the optional fields: later
/// requests leave them out from the start instead of failing first.
fn plain_only() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static SET: OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    SET.get_or_init(Default::default)
}

/// Endpoints known to reject `reasoning_content` in earlier messages.
fn no_reasoning() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static SET: OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    SET.get_or_init(Default::default)
}

/// Takes `reasoning_content` off every message; true if there was any.
fn strip_reasoning(body: &mut Value) -> bool {
    let mut any = false;
    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        for m in messages {
            if let Some(obj) = m.as_object_mut() {
                any |= obj.remove("reasoning_content").is_some();
            }
        }
    }
    any
}

fn strip_optional(body: &mut Value) {
    strip_reasoning(body);
    if let Some(obj) = body.as_object_mut() {
        for f in OPTIONAL_FIELDS {
            obj.remove(*f);
        }
    }
}

/// What a retry that worked says about the endpoint.
#[derive(Clone, Copy, PartialEq)]
enum Lesson {
    Nothing,
    NoReasoning,
    Plain,
}

fn is_rejection(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::BAD_REQUEST || status == reqwest::StatusCode::UNPROCESSABLE_ENTITY
}

/// Efforts above "high" are newer; a server that refuses one gets "high".
fn lower_effort(body: &mut Value) -> bool {
    let too_high = matches!(body.get("reasoning_effort").and_then(Value::as_str), Some("xhigh" | "max"));
    if too_high {
        body["reasoning_effort"] = json!("high");
        if let Some(k) = body.get_mut("chat_template_kwargs").and_then(Value::as_object_mut) {
            k.insert("reasoning_effort".into(), json!("high"));
        }
    }
    too_high
}

/// Streams one chat completion, calling `on` for each piece as it arrives.
pub async fn stream_chat(
    base: &str,
    key: Option<&str>,
    mut body: Value,
    cancel: &CancellationToken,
    mut on: impl FnMut(Event),
) -> Result<End, String> {
    body["stream"] = json!(true);
    body["stream_options"] = json!({ "include_usage": true });
    let memo = format!("{}|{}", normalize_base(base), body.get("model").and_then(Value::as_str).unwrap_or(""));
    if plain_only().lock().unwrap().contains(&memo) {
        strip_optional(&mut body);
    } else if no_reasoning().lock().unwrap().contains(&memo) {
        strip_reasoning(&mut body);
    }
    let send = |body: &Value| request(reqwest::Method::POST, base, "/chat/completions", key).json(body).send();

    let mut resp = tokio::select! {
        r = send(&body) => r.map_err(|e| connect_error(e, base))?,
        _ = cancel.cancelled() => return Ok(End::Cancelled),
    };
    let status = resp.status();
    if is_rejection(status) {
        let first = failure(resp).await;
        // First a lower effort (the server may only know up to "high"), then
        // without earlier reasoning, then none of the extras.
        let mut tries = Vec::new();
        if lower_effort(&mut body) {
            tries.push((body.clone(), Lesson::Nothing));
        }
        let mut bare = body.clone();
        if strip_reasoning(&mut bare) {
            tries.push((bare.clone(), Lesson::NoReasoning));
        }
        if OPTIONAL_FIELDS.iter().any(|f| bare.get(*f).is_some()) {
            let mut plain = bare.clone();
            strip_optional(&mut plain);
            tries.push((plain, Lesson::Plain));
        }
        let mut last = first.clone();
        let mut ok = None;
        for (attempt, lesson) in &tries {
            let r = tokio::select! {
                r = send(attempt) => r.map_err(|e| connect_error(e, base))?,
                _ = cancel.cancelled() => return Ok(End::Cancelled),
            };
            if r.status().is_success() {
                match lesson {
                    Lesson::Plain => plain_only().lock().unwrap().insert(memo.clone()),
                    Lesson::NoReasoning => no_reasoning().lock().unwrap().insert(memo.clone()),
                    Lesson::Nothing => false,
                };
                ok = Some(r);
                break;
            }
            let status = r.status();
            last = failure(r).await;
            if !is_rejection(status) {
                break;
            }
        }
        resp = match ok {
            Some(r) => r,
            None => return Err(if tries.is_empty() { first } else { last }),
        };
    } else if !status.is_success() {
        return Err(failure(resp).await);
    }

    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let chunk = tokio::select! {
            c = stream.next() => c,
            _ = cancel.cancelled() => return Ok(End::Cancelled),
        };
        let Some(chunk) = chunk else { break };
        let chunk = chunk.map_err(|e| format!("The connection dropped: {e}"))?;
        buf.extend_from_slice(&chunk);
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            if parse_line(line.trim(), &mut on)? {
                return Ok(End::Done);
            }
        }
    }
    let rest = String::from_utf8_lossy(&buf).to_string();
    parse_line(rest.trim(), &mut on)?;
    Ok(End::Done)
}

/// Handles one SSE line. `Ok(true)` at `[DONE]`.
fn parse_line(line: &str, on: &mut impl FnMut(Event)) -> Result<bool, String> {
    let Some(data) = line.strip_prefix("data:") else { return Ok(false) };
    let data = data.trim();
    if data == "[DONE]" {
        return Ok(true);
    }
    let Ok(v) = serde_json::from_str::<Value>(data) else { return Ok(false) };
    if let Some(err) = v.get("error") {
        let msg = err.get("message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| err.to_string());
        return Err(msg);
    }
    if let Some(choice) = v.get("choices").and_then(|c| c.get(0)) {
        let delta = choice.get("delta").or_else(|| choice.get("message"));
        if let Some(delta) = delta {
            // DeepSeek, vLLM and LM Studio say `reasoning_content`; Ollama and OpenRouter `reasoning`.
            for field in ["reasoning_content", "reasoning"] {
                if let Some(r) = delta.get(field).and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    on(Event::Reasoning(r.to_string()));
                    break;
                }
            }
            if let Some(c) = delta.get("content").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                on(Event::Content(c.to_string()));
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for (i, call) in calls.iter().enumerate() {
                    let index = call.get("index").and_then(Value::as_u64).map(|v| v as usize).unwrap_or(i);
                    let f = call.get("function");
                    let arguments = match f.and_then(|f| f.get("arguments")) {
                        Some(Value::String(a)) => a.clone(),
                        Some(Value::Null) | None => String::new(),
                        // Some servers send the arguments as an object.
                        Some(other) => other.to_string(),
                    };
                    on(Event::ToolCall {
                        index,
                        id: call.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
                        name: f.and_then(|f| f.get("name")).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
                        arguments,
                    });
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            on(Event::Finish(reason.to_string()));
        }
    }
    if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
        let prompt = u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0);
        let completion = u.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0);
        if prompt > 0 || completion > 0 {
            on(Event::Usage { prompt, completion });
        }
    }
    Ok(false)
}

/// One non-streaming completion, for small jobs like titles.
pub async fn complete(base: &str, key: Option<&str>, body: Value) -> Result<String, String> {
    let resp = request(reqwest::Method::POST, base, "/chat/completions", key)
        .timeout(Duration::from_secs(120))
        .json(&body)
        .send()
        .await
        .map_err(|e| connect_error(e, base))?;
    if !resp.status().is_success() {
        return Err(failure(resp).await);
    }
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    let text = v.pointer("/choices/0/message/content").and_then(Value::as_str).unwrap_or("").to_string();
    let mut split = ThinkSplitter::default();
    let (mut content, _) = split.feed(&text);
    content.push_str(&split.finish().0);
    Ok(content)
}

// ---- context window ---------------------------------------------------------------

/// The server's root: the base address without a trailing `/v1`.
fn root_of(base: &str) -> String {
    let b = normalize_base(base);
    b.strip_suffix("/v1").unwrap_or(&b).to_string()
}

fn as_ctx(v: Option<&Value>) -> Option<u64> {
    v.and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).filter(|&n| n >= 256)
}

async fn get_json(url: String, key: Option<&str>) -> Option<Value> {
    let mut r = client().get(url).timeout(Duration::from_secs(4));
    if let Some(k) = key.filter(|k| !k.is_empty()) {
        r = r.bearer_auth(k);
    }
    let resp = r.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<Value>().await.ok()
}

/// Asks the server how much context `model` has, the ways the common servers
/// tell: the model list (vLLM, OpenRouter, llama.cpp), llama.cpp's `/props`,
/// LM Studio's `/api/v0/models`, and Ollama's `/api/show`.
pub async fn detect_context(base: &str, key: Option<&str>, model: &str) -> Option<u64> {
    if let Some(v) = get_json(format!("{}/models", normalize_base(base)), key).await {
        let list = v.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
        if let Some(m) = list.iter().find(|m| m.get("id").and_then(Value::as_str) == Some(model)) {
            for path in ["/context_length", "/max_model_len", "/max_context_length", "/context_window", "/meta/n_ctx", "/meta/n_ctx_train", "/top_provider/context_length"] {
                if let Some(n) = as_ctx(m.pointer(path)) {
                    return Some(n);
                }
            }
        }
    }
    let root = root_of(base);
    if let Some(v) = get_json(format!("{root}/props"), key).await {
        if let Some(n) = as_ctx(v.pointer("/default_generation_settings/n_ctx")).or_else(|| as_ctx(v.get("n_ctx"))) {
            return Some(n);
        }
    }
    if let Some(v) = get_json(format!("{root}/api/v0/models/{model}"), key).await {
        if let Some(n) = as_ctx(v.get("loaded_context_length")).or_else(|| as_ctx(v.get("max_context_length"))) {
            return Some(n);
        }
    }
    let mut show = client().post(format!("{root}/api/show")).timeout(Duration::from_secs(4)).json(&json!({ "model": model }));
    if let Some(k) = key.filter(|k| !k.is_empty()) {
        show = show.bearer_auth(k);
    }
    let v = match show.send().await {
        Ok(r) if r.status().is_success() => r.json::<Value>().await.ok()?,
        _ => return None,
    };
    // A num_ctx the user set wins over what the model could do.
    let set = v.get("parameters").and_then(Value::as_str).and_then(|p| {
        p.lines().find_map(|l| l.trim().strip_prefix("num_ctx").and_then(|n| n.trim().parse::<u64>().ok()))
    });
    if set.is_some() {
        return set;
    }
    v.get("model_info")
        .and_then(Value::as_object)
        .and_then(|info| info.iter().find(|(k, _)| k.ends_with(".context_length")).and_then(|(_, v)| as_ctx(Some(v))))
}

// ---- <think> tags ------------------------------------------------------------------

/// Many local models (Qwen, DeepSeek R1 distills…) put their reasoning inline
/// as `<think>…</think>` at the start of the content. This moves it to the
/// reasoning channel, even when a tag is split across chunks.
#[derive(Default)]
pub struct ThinkSplitter {
    state: ThinkState,
    held: String,
}

#[derive(Default, PartialEq)]
enum ThinkState {
    #[default]
    Start,
    Thinking,
    Answer,
}

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";

impl ThinkSplitter {
    /// Returns (content, reasoning) to pass on now.
    pub fn feed(&mut self, piece: &str) -> (String, String) {
        self.held.push_str(piece);
        let mut content = String::new();
        let mut reasoning = String::new();
        loop {
            match self.state {
                ThinkState::Answer => {
                    content.push_str(&std::mem::take(&mut self.held));
                    break;
                }
                ThinkState::Start => {
                    let trimmed = self.held.trim_start();
                    if trimmed.starts_with(OPEN) {
                        self.held = trimmed[OPEN.len()..].to_string();
                        self.state = ThinkState::Thinking;
                    } else if trimmed.is_empty() || OPEN.starts_with(trimmed) {
                        break; // could still become <think>
                    } else {
                        self.state = ThinkState::Answer;
                    }
                }
                ThinkState::Thinking => {
                    if let Some(i) = self.held.find(CLOSE) {
                        reasoning.push_str(&self.held[..i]);
                        self.held = self.held[i + CLOSE.len()..].trim_start().to_string();
                        self.state = ThinkState::Answer;
                    } else {
                        // Keep a tail that might be the start of </think>.
                        let keep = (1..CLOSE.len()).rev().find(|&n| self.held.ends_with(&CLOSE[..n])).unwrap_or(0);
                        let cut = self.held.len() - keep;
                        reasoning.push_str(&self.held[..cut]);
                        self.held = self.held[cut..].to_string();
                        break;
                    }
                }
            }
        }
        (content, reasoning)
    }

    /// The stream ended: whatever is held goes out.
    pub fn finish(&mut self) -> (String, String) {
        let held = std::mem::take(&mut self.held);
        match self.state {
            ThinkState::Thinking => (String::new(), held),
            _ => (held, String::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(pieces: &[&str]) -> (String, String) {
        let mut s = ThinkSplitter::default();
        let (mut c, mut r) = (String::new(), String::new());
        for p in pieces {
            let (a, b) = s.feed(p);
            c += &a;
            r += &b;
        }
        let (a, b) = s.finish();
        (c + &a, r + &b)
    }

    #[test]
    fn splits_think_tags_across_chunks() {
        assert_eq!(run(&["<thi", "nk>plan", " it</th", "ink>\n\nAnswer"]), ("Answer".into(), "plan it".into()));
        assert_eq!(run(&["Hello ", "<think>not reasoning</think>"]), ("Hello <think>not reasoning</think>".into(), "".into()));
        assert_eq!(run(&["<", "b>bold</b>"]), ("<b>bold</b>".into(), "".into()));
        assert_eq!(run(&["  <think>unfinished"]), ("".into(), "unfinished".into()));
        assert_eq!(run(&["a < b"]), ("a < b".into(), "".into()));
    }

    #[test]
    fn parses_sse_lines() {
        let mut got = Vec::new();
        let mut on = |e: Event| match e {
            Event::Content(c) => got.push(format!("c:{c}")),
            Event::Reasoning(r) => got.push(format!("r:{r}")),
            Event::Usage { prompt, completion } => got.push(format!("u:{prompt}/{completion}")),
            Event::Finish(f) => got.push(format!("f:{f}")),
            Event::ToolCall { index, id, name, arguments } => got.push(format!("t:{index}:{id:?}:{name:?}:{arguments}")),
        };
        parse_line(r#"data: {"choices":[{"delta":{"reasoning_content":"hmm"}}]}"#, &mut on).unwrap();
        parse_line(r#"data: {"choices":[{"delta":{"content":"Hi"},"finish_reason":"stop"}]}"#, &mut on).unwrap();
        parse_line(r#"data: {"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2}}"#, &mut on).unwrap();
        parse_line(": keep-alive", &mut on).unwrap();
        assert!(parse_line("data: [DONE]", &mut on).unwrap());
        assert!(parse_line(r#"data: {"error":{"message":"overloaded"}}"#, &mut on).is_err());
        parse_line(r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"web__search","arguments":"{\"q\""}}]}}]}"#, &mut on).unwrap();
        parse_line(r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":1}"}}]}}]}"#, &mut on).unwrap();
        assert_eq!(
            got,
            ["r:hmm", "c:Hi", "f:stop", "u:5/2", "t:0:Some(\"c1\"):Some(\"web__search\"):{\"q\"", "t:0:None:None::1}"]
        );
        assert_eq!(root_of("http://localhost:8080/v1/"), "http://localhost:8080");
    }
}

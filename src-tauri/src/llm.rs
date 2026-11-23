//! The OpenAI-compatible wire protocol: listing models and streaming chat
//! completions over server-sent events. Ollama, LM Studio, llama.cpp, vLLM,
//! OpenAI, OpenRouter and friends all speak it.

use std::sync::OnceLock;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::db::Caps;

/// The shared HTTP client (for `anthropic`).
pub(crate) fn http() -> &'static reqwest::Client {
    client()
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("Relay/", env!("CARGO_PKG_VERSION")))
            .redirect(same_origin_redirects())
            .build()
            .unwrap_or_default()
    })
}

/// Requests carry an API key (`Authorization`, and `x-api-key` for
/// Anthropic, which reqwest would keep on a redirect to another host), so a
/// server may only redirect within its own address: same scheme, host and
/// port. APIs don't redirect elsewhere; a server that does gets an error.
pub(crate) fn same_origin_redirects() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let from = attempt.previous().first().map(origin);
        if attempt.previous().len() >= 5 {
            attempt.error("too many redirects")
        } else if from.is_some_and(|f| f != origin(attempt.url())) {
            let to = attempt.url().host_str().unwrap_or("").to_string();
            attempt.error(format!("the server sent Relay to another address ({to}); use that address instead"))
        } else {
            attempt.follow()
        }
    })
}

/// Scheme, host and port: where a key may go.
fn origin(url: &reqwest::Url) -> (String, String, Option<u16>) {
    let host = url.host_str().unwrap_or("").to_lowercase().replace("localhost", "127.0.0.1");
    (url.scheme().to_string(), host, url.port_or_known_default())
}

/// Whether two base addresses are the same server (scheme, host and port).
pub fn same_host(a: &str, b: &str) -> bool {
    match (reqwest::Url::parse(&normalize_base(a)), reqwest::Url::parse(&normalize_base(b))) {
        (Ok(a), Ok(b)) => origin(&a) == origin(&b),
        _ => false,
    }
}

/// The address without spaces, a trailing slash, or a pasted endpoint
/// (`…/chat/completions`, `…/messages`, `…/models`): requests add their own.
pub fn normalize_base(url: &str) -> String {
    let mut b = url.trim().trim_end_matches('/');
    for end in ["/chat/completions", "/messages", "/models"] {
        if b.len() > end.len() && b.is_char_boundary(b.len() - end.len()) && b[b.len() - end.len()..].eq_ignore_ascii_case(end) {
            b = b[..b.len() - end.len()].trim_end_matches('/');
            break;
        }
    }
    b.to_string()
}

/// The base address requests build on, from what the user typed (the UI's
/// `apiBase` mirrors it). A pasted endpoint loses its last part; an address
/// with no path gets `/v1`, where every local server and OpenAI keep it; and
/// for the Anthropic API a path that doesn't end in a version gets `/v1`,
/// because Anthropic-compatible services publish the address the SDK adds
/// `/v1/messages` to (`https://api.z.ai/api/anthropic`).
pub fn api_base(api: &str, url: &str) -> String {
    let b = normalize_base(url);
    let Ok(parsed) = reqwest::Url::parse(&b) else { return b };
    let last = parsed.path_segments().and_then(|mut s| s.next_back()).unwrap_or("");
    let versioned = last.len() > 1 && last.starts_with(['v', 'V']) && last[1..].starts_with(|c: char| c.is_ascii_digit());
    let bare = parsed.path().trim_matches('/').is_empty();
    if parsed.query().is_none() && (bare || (api == "anthropic" && !versioned)) {
        format!("{b}/v1")
    } else {
        b
    }
}

fn request(method: reqwest::Method, base: &str, path: &str, key: Option<&str>) -> reqwest::RequestBuilder {
    let mut req = client().request(method, format!("{}{path}", normalize_base(base)));
    if let Some(key) = key.filter(|k| !k.is_empty()) {
        req = req.bearer_auth(key);
        if normalize_base(base).contains("api.anthropic.com") {
            req = req.header("x-api-key", key).header("anthropic-version", crate::anthropic::VERSION);
        }
    }
    req
}

/// A readable reason from a failed response: the API's own message when it
/// sent one, else the status.
pub(crate) async fn failure(resp: reqwest::Response) -> String {
    let status = resp.status();
    let body = crate::limits::text_start(resp, crate::limits::MAX_ERROR).await;
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

pub(crate) fn connect_error(e: reqwest::Error, base: &str) -> String {
    if e.is_connect() {
        format!("Couldn't reach {}. Is the server running?", normalize_base(base))
    } else if e.is_timeout() {
        "The server took too long to answer".into()
    } else {
        crate::web::error_chain(&e)
    }
}

/// Why a model list didn't come: the server never answered, or it answered
/// without one (an error, or not JSON).
enum ListError {
    Unreachable(String),
    Answered(String),
}

/// The ids the endpoint offers, sorted.
pub async fn list_models(api: &str, base: &str, key: Option<&str>) -> Result<Vec<String>, String> {
    models_of(api, base, key, Duration::from_secs(15)).await.map_err(|e| match e {
        ListError::Unreachable(m) | ListError::Answered(m) => m,
    })
}

/// Is the server there right now, and what does it list? `(false, None)`:
/// unreachable. `(true, None)`: it answered but lists no models (some
/// servers don't), so models added by name count as available.
pub async fn probe(api: &str, base: &str, key: Option<&str>) -> (bool, Option<Vec<String>>) {
    match models_of(api, base, key, Duration::from_secs(5)).await {
        Ok(ids) => (true, Some(ids)),
        Err(ListError::Unreachable(_)) => (false, None),
        Err(ListError::Answered(_)) => (true, None),
    }
}

async fn models_of(api: &str, base: &str, key: Option<&str>, timeout: Duration) -> Result<Vec<String>, ListError> {
    let mut req = request(reqwest::Method::GET, base, "/models", key);
    // Servers that copy the Messages API take its headers for the list too.
    if api == "anthropic" && !normalize_base(base).contains("api.anthropic.com") {
        req = req.header("anthropic-version", crate::anthropic::VERSION);
        if let Some(key) = key.filter(|k| !k.is_empty()) {
            req = req.header("x-api-key", key);
        }
    }
    let resp = req
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
    let body: Value = crate::limits::json(resp, crate::limits::MAX_JSON)
        .await
        .map_err(|_| ListError::Answered("The server's model list wasn't JSON (or was too long)".to_string()))?;
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
    /// The reply's blocks as an Anthropic server sent them (at the end).
    Native(Vec<Value>),
    /// One of the server's own tools (Anthropic's web search): its call, then
    /// its input, then its result (text, failed).
    Hosted { id: String, name: String, input: Option<String>, result: Option<(String, bool)> },
}

/// Removes Relay's own keys (`_effort`, `_native`, …) before a request goes
/// to an OpenAI-compatible server.
fn strip_private(body: &mut Value) {
    if let Some(obj) = body.as_object_mut() {
        obj.retain(|k, _| !k.starts_with('_'));
    }
    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        for m in messages {
            if let Some(obj) = m.as_object_mut() {
                obj.retain(|k, _| !k.starts_with('_'));
            }
        }
    }
}

pub enum End {
    Done,
    Cancelled,
}

/// Fields Relay adds that some servers reject. A 400 or 422 gets the request
/// again without them.
pub const OPTIONAL_FIELDS: &[&str] =
    &["stream_options", "reasoning_effort", "chat_template_kwargs", "think", "continue_final_message", "add_generation_prompt"];

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

/// Endpoints that refused a half-written answer to go on from (Continue):
/// later ones get the note from the start.
fn no_prefill() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static SET: OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    SET.get_or_init(Default::default)
}

/// What Continue says to a model that can't be handed its half-written answer
/// to go on from (Claude, OpenAI's API, servers that refuse one).
pub const CONTINUE_NOTE: &str = "Your previous message was cut off. Continue it from exactly where it stopped, mid-sentence if that's where it ended. Don't repeat anything you already wrote and don't add an introduction.";

/// The note, as the user message that follows the answer so far.
pub fn continue_note() -> Value {
    json!({ "role": "user", "content": CONTINUE_NOTE })
}

/// Whether the server is an engine known to go on from a half-written answer
/// at the end of the conversation: llama.cpp (it answers `/props`), vLLM (its
/// models are `owned_by: vllm`; `continue_final_message` asks it to) or
/// Ollama (`/api/version`). Others may take one without an error and quietly
/// start a new answer after it (OpenAI's API does), so they get the note.
/// Asked once per address.
async fn continues_answers(base: &str, key: Option<&str>) -> bool {
    static SEEN: OnceLock<std::sync::Mutex<std::collections::HashMap<String, bool>>> = OnceLock::new();
    let seen = SEEN.get_or_init(Default::default);
    let root = root_of(base);
    if let Some(&known) = seen.lock().unwrap().get(&root) {
        return known;
    }
    let llama = get_json(format!("{root}/props"), key).await.is_some_and(|v| v.get("default_generation_settings").is_some());
    let known = llama
        || get_json(format!("{}/models", normalize_base(base)), key).await.is_some_and(|v| {
            v.get("data").and_then(Value::as_array).is_some_and(|l| l.iter().any(|m| m.get("owned_by").and_then(Value::as_str) == Some("vllm")))
        })
        || get_json(format!("{root}/api/version"), key).await.is_some_and(|v| v.get("version").is_some());
    seen.lock().unwrap().insert(root, known);
    known
}

/// A Continue request that ended with the half-written answer, asking with
/// the note instead.
fn ask_to_continue(body: &Value) -> Value {
    let mut b = body.clone();
    if let Some(o) = b.as_object_mut() {
        o.remove("continue_final_message");
        o.remove("add_generation_prompt");
    }
    if let Some(m) = b.get_mut("messages").and_then(Value::as_array_mut) {
        m.push(continue_note());
    }
    b
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
    NoPrefill,
}

pub(crate) fn is_rejection(status: reqwest::StatusCode) -> bool {
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
/// Streams a reply in the provider's protocol: `api` is "anthropic" for the
/// Messages API, else Chat Completions.
pub async fn stream_chat(
    api: &str,
    base: &str,
    key: Option<&str>,
    mut body: Value,
    cancel: &CancellationToken,
    mut on: impl FnMut(Event),
) -> Result<End, String> {
    if api == "anthropic" {
        return crate::anthropic::stream_chat(base, key, body, cancel, on).await;
    }
    // Continue: the request ends with the half-written answer to go on from.
    let prefill = body.get("_prefill").and_then(Value::as_bool).unwrap_or(false);
    strip_private(&mut body);
    body["stream"] = json!(true);
    body["stream_options"] = json!({ "include_usage": true });
    let memo = format!("{}|{}", normalize_base(base), body.get("model").and_then(Value::as_str).unwrap_or(""));
    // Where the server refuses it, the same request asks with the note.
    let mut asking = prefill.then(|| ask_to_continue(&body));
    let refused = no_prefill().lock().unwrap().contains(&memo);
    if prefill && (refused || !continues_answers(base, key).await) {
        body = asking.take().unwrap_or(body);
    }
    if plain_only().lock().unwrap().contains(&memo) {
        strip_optional(&mut body);
        asking.iter_mut().for_each(strip_optional);
    } else if no_reasoning().lock().unwrap().contains(&memo) {
        strip_reasoning(&mut body);
        asking.iter_mut().for_each(|a| {
            strip_reasoning(a);
        });
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
        // Last, a Continue asks with the note instead of the half-written answer.
        if let Some(a) = asking.take() {
            let mut plain = a.clone();
            strip_optional(&mut plain);
            tries.push((a, Lesson::NoPrefill));
            tries.push((plain, Lesson::NoPrefill));
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
                    Lesson::NoPrefill => no_prefill().lock().unwrap().insert(memo.clone()),
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
    let mut lines = crate::limits::Lines::default();
    loop {
        let chunk = tokio::select! {
            c = next_chunk(&mut stream) => c?,
            _ = cancel.cancelled() => return Ok(End::Cancelled),
        };
        let Some(chunk) = chunk else { break };
        for line in lines.push(chunk.as_ref(), crate::limits::MAX_JSON)? {
            if parse_line(line.trim(), &mut on)? {
                return Ok(End::Done);
            }
        }
    }
    parse_line(lines.rest().trim(), &mut on)?;
    Ok(End::Done)
}

/// The next piece of a reply stream; an error when the connection drops or
/// the server goes quiet for `limits::IDLE`.
pub(crate) async fn next_chunk<S, B>(stream: &mut S) -> Result<Option<B>, String>
where
    S: futures_util::Stream<Item = reqwest::Result<B>> + Unpin,
{
    match tokio::time::timeout(crate::limits::IDLE, stream.next()).await {
        Err(_) => Err(format!("The server stopped sending for {} minutes", crate::limits::IDLE.as_secs() / 60)),
        Ok(None) => Ok(None),
        Ok(Some(Ok(b))) => Ok(Some(b)),
        Ok(Some(Err(e))) => Err(format!("The connection dropped: {e}")),
    }
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
pub async fn complete(api: &str, base: &str, key: Option<&str>, mut body: Value) -> Result<String, String> {
    if api == "anthropic" {
        return crate::anthropic::complete(base, key, body).await;
    }
    strip_private(&mut body);
    let resp = request(reqwest::Method::POST, base, "/chat/completions", key)
        .timeout(Duration::from_secs(120))
        .json(&body)
        .send()
        .await
        .map_err(|e| connect_error(e, base))?;
    if !resp.status().is_success() {
        return Err(failure(resp).await);
    }
    let v: Value = crate::limits::json(resp, crate::limits::MAX_JSON).await?;
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
    crate::limits::json(resp, crate::limits::MAX_JSON).await.ok()
}

/// Asks the server how much context `model` has, the ways the common servers
/// tell: the model list (vLLM, OpenRouter, llama.cpp), llama.cpp's `/props`,
/// LM Studio's `/api/v0/models`, and Ollama's `/api/show`.
pub async fn detect_context(base: &str, key: Option<&str>, model: &str) -> Option<u64> {
    if let Some(v) = get_json(format!("{}/models", normalize_base(base)), key).await {
        let list = v.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
        if let Some(m) = list.iter().find(|m| m.get("id").and_then(Value::as_str) == Some(model)) {
            for path in ["/context_length", "/max_input_tokens", "/max_model_len", "/max_context_length", "/context_window", "/meta/n_ctx", "/meta/n_ctx_train", "/top_provider/context_length"] {
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
        Ok(r) if r.status().is_success() => crate::limits::json(r, crate::limits::MAX_JSON).await.ok()?,
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

// ---- what a model takes ---------------------------------------------------------------

/// What `model` takes besides text, the ways servers tell: OpenRouter's model
/// list (`architecture.input_modalities`), a `capabilities` list, llama.cpp's
/// `/props` (`modalities`), LM Studio's `/api/v0/models` (`type: "vlm"`), and
/// Ollama's `/api/show` (`capabilities`). What a server doesn't say comes from
/// the name (`guess_caps`). None: no server said anything.
pub async fn detect_caps(base: &str, key: Option<&str>, model: &str) -> Option<Caps> {
    let guess = guess_caps(base, model);
    let from_list = |list: &[Value]| -> Caps {
        let has = |k: &str| list.iter().any(|x| x.as_str().is_some_and(|s| s.eq_ignore_ascii_case(k)));
        let vision = has("image") || has("vision");
        Caps { vision, audio: has("audio"), video: has("video") || (vision && guess.video), pdf: has("file") || guess.pdf }
    };
    if let Some(v) = model_list(base, key).await {
        let list = v.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
        if let Some(m) = list.iter().find(|m| m.get("id").and_then(Value::as_str) == Some(model)) {
            if let Some(mods) = m.pointer("/architecture/input_modalities").and_then(Value::as_array) {
                return Some(from_list(mods));
            }
            if let Some(caps) = m.get("capabilities").and_then(Value::as_array) {
                return Some(from_list(caps));
            }
            // Anthropic's: a tree with `supported` at each leaf. Claude reads PDFs.
            if let Some(vision) = m.pointer("/capabilities/image_input/supported").and_then(Value::as_bool) {
                let pdf = m.pointer("/capabilities/pdf_input/supported").and_then(Value::as_bool).unwrap_or(true);
                return Some(Caps { vision, audio: false, video: false, pdf });
            }
        }
    }
    let root = root_of(base);
    if let Some(m) = get_json(format!("{root}/props"), key).await.and_then(|v| v.get("modalities").cloned()) {
        let vision = m.get("vision").and_then(Value::as_bool).unwrap_or(false);
        let audio = m.get("audio").and_then(Value::as_bool).unwrap_or(false);
        return Some(Caps { vision, audio, video: vision && guess.video, pdf: guess.pdf });
    }
    if let Some(kind) = get_json(format!("{root}/api/v0/models/{model}"), key).await.and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_string)) {
        let vision = kind == "vlm";
        return Some(Caps { vision, audio: guess.audio, video: vision && guess.video, pdf: guess.pdf });
    }
    let mut show = client().post(format!("{root}/api/show")).timeout(Duration::from_secs(4)).json(&json!({ "model": model }));
    if let Some(k) = key.filter(|k| !k.is_empty()) {
        show = show.bearer_auth(k);
    }
    if let Ok(r) = show.send().await {
        if r.status().is_success() {
            if let Some(caps) = crate::limits::json(r, crate::limits::MAX_JSON).await.ok().and_then(|v| v.get("capabilities").and_then(Value::as_array).cloned()) {
                return Some(from_list(&caps));
            }
        }
    }
    None
}

/// The server's full model list, kept for a minute: asking about every model
/// of a big list (OpenRouter's) shouldn't fetch it each time.
async fn model_list(base: &str, key: Option<&str>) -> Option<Value> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Instant;
    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, Value)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let url = format!("{}/models", normalize_base(base));
    if let Some((at, v)) = cache.lock().unwrap().get(&url) {
        if at.elapsed() < Duration::from_secs(60) {
            return Some(v.clone());
        }
    }
    let v = get_json(url.clone(), key).await?;
    cache.lock().unwrap().insert(url, (Instant::now(), v.clone()));
    Some(v)
}

/// What a model's name (and its server) suggests it takes, for servers that
/// don't say. The user can correct it per model.
pub fn guess_caps(base: &str, model: &str) -> Caps {
    let m = model.to_lowercase();
    let name = m.rsplit('/').next().unwrap_or(&m);
    let host = normalize_base(base).to_lowercase();
    let any = |words: &[&str]| words.iter().any(|w| m.contains(w));
    let hosted = any(&["gpt-4o", "gpt-4.1", "gpt-5", "chatgpt-4o", "claude", "gemini", "grok-4", "grok-2-vision", "grok-vision", "llama-4", "pixtral", "mistral-medium", "mistral-small-3"])
        || name.starts_with("o3")
        || name.starts_with("o4");
    let vision = hosted
        || any(&[
            "vision", "-vl", "_vl", "vl-", "llava", "moondream", "minicpm-v", "gemma-3", "gemma3", "internvl", "cogvlm", "glm-4v", "glm-4.1v",
            "glm-4.5v", "omni", "smolvlm", "paligemma", "molmo", "multimodal", "kimi-vl", "ovis", "-vlm",
        ]);
    let audio = any(&["audio", "omni", "voxtral", "gemma-3n", "gemma3n", "ultravox", "phi-4-multimodal", "gemini"]);
    let video = any(&["gemini", "qwen2-vl", "qwen2.5-vl", "qwen3-vl", "omni", "video", "internvl", "llava-onevision", "minicpm-v"]);
    // OpenRouter reads PDFs for every model; OpenAI for its vision models.
    let pdf = host.contains("openrouter.ai") || host.contains("api.anthropic.com") || (host.contains("api.openai.com") && vision);
    Caps { vision, audio, video, pdf }
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

    #[test]
    fn guesses_what_models_take() {
        let local = "http://localhost:1234/v1";
        assert!(guess_caps(local, "lfm3-vl-2b").vision);
        assert!(!guess_caps(local, "qwen3-8b").vision);
        let omni = guess_caps(local, "Qwen2.5-Omni-7B");
        assert!(omni.vision && omni.audio && omni.video);
        assert!(guess_caps(local, "voxtral-mini").audio);
        assert!(guess_caps("https://openrouter.ai/api/v1", "meta-llama/llama-3.1-8b").pdf);
        assert!(guess_caps("https://api.openai.com/v1", "gpt-4o-mini").pdf);
        assert!(guess_caps("https://api.openai.com/v1", "o3").vision);
        assert!(!guess_caps(local, "deepseek-r1-distill-qwen3").vision);
    }

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

    /// A one-shot HTTP server on loopback that answers with `head` + `body`.
    async fn serve_once(response: Vec<u8>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 64 * 1024];
                let _ = sock.read(&mut buf).await;
                // In small pieces, so lines and characters split across chunks.
                for piece in response.chunks(7) {
                    let _ = sock.write_all(piece).await;
                    let _ = sock.flush().await;
                }
            }
        });
        format!("http://127.0.0.1:{port}/v1")
    }

    #[tokio::test]
    async fn streams_a_reply_end_to_end() {
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"Héllo\"}}]}\n\n\
                   data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":99999999999,\"function\":{\"name\":\"x\"}}]}}]}\n\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\" wörld\"},\"finish_reason\":\"stop\"}]}\n\n\
                   data: [DONE]\n\n";
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{sse}");
        let base = serve_once(resp.into_bytes()).await;
        let mut text = String::new();
        let mut calls = 0;
        let end = stream_chat("openai", &base, None, json!({ "model": "m", "messages": [] }), &CancellationToken::new(), |e| match e {
            Event::Content(c) => text.push_str(&c),
            Event::ToolCall { .. } => calls += 1,
            _ => {}
        })
        .await;
        assert!(matches!(end, Ok(End::Done)));
        assert_eq!(text, "Héllo wörld");
        // Passed on as is; the reply loop drops the index (chat::add_call_piece).
        assert_eq!(calls, 1);
    }

    #[tokio::test]
    async fn error_bodies_are_read_only_in_part() {
        let body = "x".repeat(1024 * 1024);
        let resp = format!("HTTP/1.1 500 Internal Server Error\r\nConnection: close\r\n\r\n{body}");
        let base = serve_once(resp.into_bytes()).await;
        let err = list_models("openai", &base, None).await.unwrap_err();
        assert_eq!(err, "500 Internal Server Error");
    }

    #[tokio::test]
    async fn keys_never_follow_a_redirect_elsewhere() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // Records what reaches the "other" server.
        let other = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let other_port = other.local_addr().unwrap().port();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = other.accept().await {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
                let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n{\"data\":[]}").await;
            }
        });
        let resp = format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:{other_port}/v1/models\r\nContent-Length: 0\r\n\r\n");
        let base = serve_once(resp.into_bytes()).await;
        let err = list_models("openai", &base, Some("sk-secret")).await.unwrap_err();
        assert!(err.contains("another address"), "{err}");
        assert!(rx.try_recv().is_err(), "the other server was reached");
        assert!(same_host("http://localhost:1234/v1", "http://127.0.0.1:1234"));
        assert!(!same_host("https://api.example.com/v1", "http://api.example.com/v1"));
        assert!(!same_host("https://api.example.com/v1", "https://evil.example/v1"));
    }

    #[test]
    fn private_keys_stay_home() {
        let mut body = json!({ "model": "m", "_effort": "high", "_web": true, "messages": [{ "role": "assistant", "content": "x", "_native": [], "_relay": {} }] });
        strip_private(&mut body);
        assert_eq!(body, json!({ "model": "m", "messages": [{ "role": "assistant", "content": "x" }] }));
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
            Event::Native(_) | Event::Hosted { .. } => got.push("anthropic only".into()),
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

    #[test]
    fn addresses_take_any_form() {
        // Pasted endpoints lose their last part.
        assert_eq!(api_base("anthropic", "https://api.anthropic.com/v1/messages"), "https://api.anthropic.com/v1");
        assert_eq!(api_base("openai", "http://localhost:1234/v1/chat/completions/"), "http://localhost:1234/v1");
        assert_eq!(api_base("openai", " https://openrouter.ai/api/v1/models "), "https://openrouter.ai/api/v1");
        // Anthropic-compatible services publish the address without the version.
        assert_eq!(api_base("anthropic", "https://api.z.ai/api/anthropic"), "https://api.z.ai/api/anthropic/v1");
        assert_eq!(api_base("anthropic", "https://api.anthropic.com"), "https://api.anthropic.com/v1");
        assert_eq!(api_base("anthropic", "https://api.anthropic.com/v1"), "https://api.anthropic.com/v1");
        // OpenAI-compatible paths vary, so only an address with no path gets /v1.
        assert_eq!(api_base("openai", "http://localhost:11434"), "http://localhost:11434/v1");
        assert_eq!(api_base("openai", "https://api.z.ai/api/paas/v4"), "https://api.z.ai/api/paas/v4");
        assert_eq!(api_base("openai", "https://example.com/openai"), "https://example.com/openai");
        // Stored addresses that ended in an endpoint still work.
        assert_eq!(normalize_base("http://x/v1/messages"), "http://x/v1");
        assert_eq!(normalize_base("http://x/messages"), "http://x");
    }

    #[test]
    fn continue_falls_back_to_asking() {
        let body = json!({
            "model": "m",
            "messages": [{ "role": "user", "content": "hi" }, { "role": "assistant", "content": "Hello wor" }],
            "continue_final_message": true,
            "add_generation_prompt": false,
        });
        let asked = ask_to_continue(&body);
        assert!(asked.get("continue_final_message").is_none() && asked.get("add_generation_prompt").is_none());
        let m = asked["messages"].as_array().unwrap();
        assert_eq!(m.len(), 3);
        assert_eq!(m[1]["content"], "Hello wor");
        assert_eq!(m[2], continue_note());
        // The vLLM switches are dropped with the other extras on a 400.
        for f in ["continue_final_message", "add_generation_prompt"] {
            assert!(OPTIONAL_FIELDS.contains(&f));
        }
    }
}

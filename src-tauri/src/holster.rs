//! Holster integration: Relay is one of the dock's tiles, in window mode.
//!
//! - A loopback-only listener serves `/holster/health`, `/holster/notify`
//!   (what happened to a notification) and `/holster/open-chat` (a click on a
//!   chat in the preview).
//! - The manifest goes to `%APPDATA%\Holster\apps\relay.json` on every start,
//!   only when Holster is installed.
//! - The preview is a page on disk in `<data dir>\holster-preview\`, next to a
//!   `state.json` snapshot of the newest chats.
//! - When a reply finishes while Relay isn't in front, Holster shows a
//!   notification ("Open" goes straight to that chat). If Relay is muted in
//!   Holster, the tab glows instead.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::{
    body::Bytes,
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::settings::write_if_changed;
use crate::Core;

pub const ID: &str = "relay";
const NAME: &str = "Relay";
const PREFERRED_PORT: u16 = 47378;
/// Pitch black: the tile Relay's pastel mark sits on.
const COLOR: &str = "#000000";
/// The mark (two bubbles), drawn untinted on the tile.
const ICON_SVG: &str = include_str!("../icons/relay-icon.svg");
const PREVIEW_HTML: &str = include_str!("../holster-preview/index.html");
const PREVIEW_WIDTH: u32 = 300;
const PREVIEW_ROWS: usize = 3;

static PORT: AtomicU16 = AtomicU16::new(0);

/// Card height for `rows` chats: padding 14 + head 16 + gap 10 + rows (50
/// each, 6 apart) + padding 14. An empty list shows one quiet row.
fn preview_height(rows: usize) -> u32 {
    let n = rows.clamp(1, PREVIEW_ROWS) as u32;
    14 + 16 + 10 + n * 50 + (n - 1) * 6 + 14
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HolsterStatus {
    pub installed: bool,
    pub registered: bool,
    pub error: Option<String>,
    pub warnings: Vec<String>,
    pub port: u16,
    /// Relay Dev: runs beside the installed Relay and leaves its tile alone.
    pub side_by_side: bool,
}

struct Holster {
    dir: PathBuf,
    status: Mutex<HolsterStatus>,
    /// Chats with a notification out in Holster.
    cards: Mutex<HashSet<String>>,
    /// The preview's row count when the manifest was last written.
    rows: Mutex<usize>,
}

static HOLSTER: OnceLock<Arc<Holster>> = OnceLock::new();

fn holster_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Holster")).filter(|d| d.is_dir())
}

/// Holster's API base from `holster.json`, read fresh every time: its port
/// changes on every start.
fn holster_api() -> Option<String> {
    let text = std::fs::read_to_string(holster_dir()?.join("holster.json")).ok()?;
    let api = serde_json::from_str::<Value>(&text).ok()?.get("api")?.as_str()?.trim_end_matches('/').to_string();
    api.starts_with("http://127.0.0.1:").then_some(api)
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| reqwest::Client::builder().timeout(Duration::from_millis(1500)).no_proxy().build().unwrap_or_default())
}

/// Starts the listener, writes the preview and registers with Holster.
pub fn start(app: AppHandle, data_dir: &Path, register: bool) {
    let me = Arc::new(Holster {
        dir: data_dir.join("holster-preview"),
        status: Mutex::new(HolsterStatus::default()),
        cards: Mutex::new(HashSet::new()),
        rows: Mutex::new(0),
    });
    me.status.lock().unwrap().side_by_side = !register;
    let _ = HOLSTER.set(me.clone());
    let _ = std::fs::create_dir_all(&me.dir);
    for (name, text) in [("index.html", PREVIEW_HTML), ("icon.svg", ICON_SVG)] {
        if let Err(e) = write_if_changed(&me.dir.join(name), text) {
            eprintln!("holster: couldn't write {name}: {e}");
        }
    }
    tauri::async_runtime::spawn(async move {
        let port = match serve(app.clone()).await {
            Ok(port) => port,
            Err(e) => {
                eprintln!("holster: couldn't listen on loopback: {e}");
                return;
            }
        };
        PORT.store(port, Ordering::SeqCst);
        me.status.lock().unwrap().port = port;
        publish(&app);
        if register {
            me.register(port).await;
        }
    });
}

pub fn status() -> HolsterStatus {
    let mut st = HOLSTER.get().map(|h| h.status.lock().unwrap().clone()).unwrap_or_default();
    st.installed = holster_dir().is_some();
    st
}

/// Markdown, flattened to one line of plain text.
pub fn plain_snippet(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut in_fence = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        // Code, tables and display math don't read as a sentence.
        if in_fence || t.is_empty() || t.starts_with('|') || t.starts_with("$$") || t.starts_with(r"\[") {
            continue;
        }
        let t = t.trim_start_matches(['#', '>', '-', '*', '+', ' ']);
        let t: String = t.chars().filter(|c| !matches!(c, '*' | '`' | '_' | '~')).collect();
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(t.trim());
        if out.chars().count() > max {
            break;
        }
    }
    clip(&out, max)
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max - 1).collect::<String>().trim_end().to_string() + "…"
    }
}

/// Rewrites `state.json` for the preview: the newest chats.
pub fn publish(app: &AppHandle) {
    let Some(me) = HOLSTER.get() else { return };
    let Some(core) = app.try_state::<Arc<Core>>() else { return };
    let streaming = core.streams.active_chats();
    let recent = core.db.recent(PREVIEW_ROWS).unwrap_or_default();
    let chats: Vec<Value> = recent
        .iter()
        .map(|(c, last)| {
            json!({
                "id": c.id,
                "title": c.title,
                "snippet": plain_snippet(last, 120),
                "model": c.model,
                "at": c.updated_at,
                "streaming": streaming.contains(&c.id),
            })
        })
        .collect();
    let state = json!({ "version": 1, "port": PORT.load(Ordering::SeqCst), "chats": chats });
    if let Err(e) = write_if_changed(&me.dir.join("state.json"), &state.to_string()) {
        eprintln!("holster: couldn't write the preview state: {e}");
    }
    // The card's height follows the row count; the manifest carries it.
    let rows = chats.len();
    let changed = {
        let mut last = me.rows.lock().unwrap();
        let changed = (*last).clamp(1, PREVIEW_ROWS) != rows.clamp(1, PREVIEW_ROWS);
        *last = rows;
        changed
    };
    let port = PORT.load(Ordering::SeqCst);
    if changed && port != 0 && me.status.lock().unwrap().registered {
        let me = me.clone();
        tauri::async_runtime::spawn(async move { me.register(port).await });
    }
}

/// A reply finished. If Relay isn't in front, say so through Holster.
pub fn reply_finished(app: &AppHandle, chat_id: &str, content: &str) {
    let Some(core) = app.try_state::<Arc<Core>>() else { return };
    let prefs = core.prefs.get();
    if HOLSTER.get().is_none() || crate::window::is_active() || !(prefs.notify_on_reply || prefs.holster_glow) {
        return;
    }
    if !prefs.notify_on_reply {
        tauri::async_runtime::spawn(async { glow().await });
        return;
    }
    let glow_fallback = prefs.holster_glow;
    let title = core.db.chat_summary(chat_id).ok().flatten().map(|c| c.title).filter(|t| !t.is_empty());
    let card = json!({
        "id": card_id(chat_id),
        "title": clip(title.as_deref().unwrap_or("Reply ready"), 120),
        "body": plain_snippet(content, 220),
        "buttons": [{ "id": "open", "label": "Open", "primary": true, "open": true }],
        "timeout": 12,
    });
    let chat_id = chat_id.to_string();
    tauri::async_runtime::spawn(async move {
        let Some(api) = holster_api() else { return };
        let shown = match client().post(format!("{api}/apps/{ID}/notify")).json(&card).send().await {
            Ok(resp) if resp.status().is_success() => {
                let body: Value = crate::limits::json(resp, crate::limits::MAX_ERROR).await.unwrap_or(Value::Null);
                !body.get("muted").and_then(Value::as_bool).unwrap_or(false)
            }
            _ => false,
        };
        if shown {
            if let Some(me) = HOLSTER.get() {
                me.cards.lock().unwrap().insert(chat_id);
            }
        } else if glow_fallback {
            glow().await;
        }
    });
}

/// Holster's tab glows in Relay's color.
async fn glow() {
    let Some(api) = holster_api() else { return };
    let _ = client().post(format!("{api}/apps/{ID}/signal")).json(&json!({ "kind": "activity" })).send().await;
}

/// The user is looking at this chat in Relay: take its notification back.
pub fn seen(chat_id: &str) {
    let Some(me) = HOLSTER.get() else { return };
    if !me.cards.lock().unwrap().remove(chat_id) {
        return;
    }
    let id = card_id(chat_id);
    tauri::async_runtime::spawn(async move {
        let Some(api) = holster_api() else { return };
        let _ = client().delete(format!("{api}/apps/{ID}/notify/{id}")).send().await;
    });
}

fn card_id(chat_id: &str) -> String {
    format!("reply-{}", chat_id.chars().take(58).collect::<String>())
}

fn open_chat(app: &AppHandle, chat_id: String) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        crate::window::show(&handle);
        let _ = handle.emit("open-chat", chat_id);
    });
}

/// What happened to a notification (Holster tells `notify.url`).
fn on_card_event(app: &AppHandle, event: &Value) {
    let Some(chat) = event.get("id").and_then(Value::as_str).and_then(|id| id.strip_prefix("reply-")) else { return };
    if let Some(me) = HOLSTER.get() {
        me.cards.lock().unwrap().remove(chat);
    }
    if event.get("event").and_then(Value::as_str) == Some("action") && event.get("button").and_then(Value::as_str) == Some("open") {
        open_chat(app, chat.to_string());
    }
}

impl Holster {
    async fn register(&self, port: u16) {
        let Some(dir) = holster_dir() else { return };
        let apps = dir.join("apps");
        let exe = std::env::current_exe().ok();
        let base = format!("http://127.0.0.1:{port}/holster");
        let rows = *self.rows.lock().unwrap();
        let manifest = json!({
            "schema": 1,
            "id": ID,
            "name": NAME,
            "icon": { "path": self.dir.join("icon.svg"), "size": "64%" },
            "color": COLOR,
            "health": format!("{base}/health"),
            "preview": { "path": self.dir.join("index.html"), "width": PREVIEW_WIDTH, "height": preview_height(rows) },
            "open": { "mode": "window" },
            "launch": { "exe": exe, "args": [] },
            "notify": { "url": format!("{base}/notify") },
        });
        let text = serde_json::to_string_pretty(&manifest).unwrap_or_default() + "\n";
        let written = std::fs::create_dir_all(&apps).and_then(|_| write_if_changed(&apps.join(format!("{ID}.json")), &text));
        if let Err(e) = written {
            eprintln!("holster: couldn't write the manifest: {e}");
            self.status.lock().unwrap().error = Some(format!("Couldn't register with Holster: {e}"));
            return;
        }
        self.status.lock().unwrap().registered = true;

        let Some(api) = holster_api() else { return };
        let Ok(resp) = client().post(format!("{api}/apps/{ID}/reload")).send().await else { return };
        let ok = resp.status().is_success();
        let body: Value = crate::limits::json(resp, crate::limits::MAX_ERROR).await.unwrap_or(Value::Null);
        let mut st = self.status.lock().unwrap();
        st.warnings = body
            .get("warnings")
            .and_then(Value::as_array)
            .map(|w| w.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        st.error = if ok {
            None
        } else {
            Some(body.get("error").and_then(Value::as_str).unwrap_or("Holster rejected the manifest").to_string())
        };
        for w in &st.warnings {
            eprintln!("holster: {w}");
        }
        if let Some(e) = &st.error {
            eprintln!("holster: {e}");
        }
    }
}

// ---- loopback listener -------------------------------------------------------------

async fn serve(app: AppHandle) -> std::io::Result<u16> {
    // Loopback only. The preferred port keeps the manifest stable.
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", PREFERRED_PORT)).await {
        Ok(l) => l,
        Err(_) => tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?,
    };
    let port = listener.local_addr()?.port();
    let card_app = app.clone();
    let router = Router::new()
        .route("/holster/health", get(|| async { "ok" }))
        .route(
            "/holster/notify",
            post(move |body: Bytes| {
                let app = card_app.clone();
                async move {
                    if let Ok(event) = serde_json::from_slice::<Value>(&body) {
                        on_card_event(&app, &event);
                    }
                    StatusCode::NO_CONTENT
                }
            }),
        )
        .route(
            "/holster/open-chat",
            post(move |body: Bytes| {
                let app = app.clone();
                async move {
                    let id = serde_json::from_slice::<Value>(&body)
                        .ok()
                        .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_string));
                    match id {
                        Some(id) if id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric()) => {
                            open_chat(&app, id);
                            StatusCode::NO_CONTENT
                        }
                        _ => StatusCode::BAD_REQUEST,
                    }
                }
            }),
        )
        .fallback(|| async { (StatusCode::NOT_FOUND, "Not found") })
        .layer(middleware::from_fn_with_state(port, guard));
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(port)
}

fn is_loopback_origin(origin: &str) -> bool {
    ["http://127.0.0.1", "http://localhost"].iter().any(|base| {
        origin
            .strip_prefix(base)
            .is_some_and(|rest| rest.is_empty() || (rest.starts_with(':') && rest[1..].bytes().all(|b| b.is_ascii_digit())))
    })
}

/// DNS-rebinding and cross-site guard: the Host must be this loopback port,
/// and a browser request must come from a loopback page.
async fn guard(State(port): State<u16>, req: Request, next: Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    let host_ok = host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}");
    let origin_ok = req
        .headers()
        .get(header::ORIGIN)
        .map(|o| o.to_str().is_ok_and(is_loopback_origin))
        .unwrap_or(true);
    if !host_ok || !origin_ok {
        return (StatusCode::FORBIDDEN, "Forbidden").into_response();
    }
    let mut resp = next.run(req).await;
    // The preview page (served by Holster on another loopback port) calls us.
    resp.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, header::HeaderValue::from_static("*"));
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_origins() {
        assert!(is_loopback_origin("http://127.0.0.1:52710"));
        assert!(!is_loopback_origin("http://127.0.0.1.evil.com"));
        assert!(!is_loopback_origin("null"));
    }

    #[test]
    fn snippets() {
        assert_eq!(plain_snippet("# Title\n\n```rust\nfn x() {}\n```\n- **bold** item", 100), "Title bold item");
        assert_eq!(plain_snippet("**Compare**

| a | b |
|---|---|
| 1 | 2 |

Done.", 100), "Compare Done.");
        assert_eq!(clip("abcdefghijkl", 5), "abcd…");
        assert_eq!(card_id("abc"), "reply-abc");
    }

    #[test]
    fn heights() {
        assert_eq!(preview_height(0), preview_height(1));
        assert_eq!(preview_height(9), preview_height(3));
    }
}

//! SQLite storage: providers, presets, MCP servers, chats and messages, plus
//! full-text search and usage stats.
//!
//! Messages form a tree (`parent_id`), so edits and regenerations branch; a
//! chat's `leaf_id` is the message at the end of the branch on screen, and the
//! conversation is the path from the root to it.
//!
//! An assistant message's `parts` is what happened in order: reasoning, text,
//! and tool calls with their results. `content` and `reasoning` hold the same
//! text flattened, for search and for older rows that have no parts.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::attachments::Attachment;

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub fn new_id() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut rng = rand::rng();
    (0..12).map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    /// The models the endpoint listed last time it was asked.
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub models_at: i64,
    /// Models the user added: typed in, or (with `show_all` off) ticked in the server's list.
    #[serde(default)]
    pub custom_models: Vec<String>,
    /// Show every model the server lists. Off: only `custom_models`.
    #[serde(default = "yes")]
    pub show_all: bool,
    /// "brand:openai", "lucide:cpu" or a small `data:image/…` URL. None: chosen from the address.
    #[serde(default)]
    pub icon: Option<String>,
    /// Context window per model: detected from the server, or set by the user.
    #[serde(default)]
    pub contexts: HashMap<String, u64>,
    /// What each model takes besides text, as the server said (detected once, then kept).
    #[serde(default)]
    pub caps: HashMap<String, Caps>,
    /// What the user said a model takes, over what was detected: model -> ability -> yes or no.
    #[serde(default)]
    pub caps_user: HashMap<String, HashMap<String, bool>>,
    #[serde(default)]
    pub has_key: bool,
    #[serde(default)]
    pub sort: i64,
    /// The protocol: "openai" (Chat Completions) or "anthropic" (Messages).
    #[serde(default = "openai")]
    pub api: String,
}

fn openai() -> String {
    "openai".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preset {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub system_prompt: String,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<u64>,
    /// "off" | "low" | "medium" | "high" | "xhigh" | "max"; None leaves the chat's choice.
    pub effort: Option<String>,
    #[serde(default)]
    pub sort: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    #[serde(default)]
    pub id: String,
    pub name: String,
    /// "stdio" | "http"
    pub kind: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub url: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Per tool: "ask" | "allow" | "off". Tools not listed use `default_policy`.
    #[serde(default)]
    pub tool_policy: HashMap<String, String>,
    #[serde(default = "ask")]
    pub default_policy: String,
    /// Environment variable names (their values live in Credential Manager).
    #[serde(default)]
    pub env_keys: Vec<String>,
    /// An HTTP bearer token is saved (in Credential Manager).
    #[serde(default)]
    pub has_token: bool,
    #[serde(default)]
    pub sort: i64,
}

/// What a model takes besides text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Caps {
    pub vision: bool,
    pub audio: bool,
    pub video: bool,
    /// PDFs as files (the server reads the pages); else Relay sends their text.
    pub pdf: bool,
}

impl Caps {
    pub const ABILITIES: [&'static str; 4] = ["vision", "audio", "video", "pdf"];

    pub fn set(&mut self, ability: &str, on: bool) {
        match ability {
            "vision" => self.vision = on,
            "audio" => self.audio = on,
            "video" => self.video = on,
            "pdf" => self.pdf = on,
            _ => {}
        }
    }

    /// These, with the user's word applied over them.
    pub fn with_user(mut self, user: Option<&HashMap<String, bool>>) -> Caps {
        for (ability, on) in user.into_iter().flatten() {
            self.set(ability, *on);
        }
        self
    }
}

fn yes() -> bool {
    true
}
fn ask() -> String {
    "ask".into()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSummary {
    pub id: String,
    pub title: String,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub pinned: bool,
    pub preset_id: Option<String>,
    /// None: the default from settings.
    pub effort: Option<String>,
    /// MCP tools (the column kept its first name, from when it covered skills too).
    pub tools: bool,
    pub skills: bool,
    pub web: bool,
    /// Out of the main list (in the sidebar's Archived group).
    pub archived: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Chat {
    #[serde(flatten)]
    pub summary: ChatSummary,
    pub system_prompt: String,
    pub leaf_id: Option<String>,
    /// The path from the first message to `leaf_id`.
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    pub chat_id: String,
    pub parent_id: Option<String>,
    pub role: String,
    pub content: String,
    pub reasoning: String,
    pub parts: Option<Value>,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    /// "done" | "streaming" | "stopped" | "error"
    pub status: String,
    pub error: Option<String>,
    pub stats: Option<Value>,
    pub created_at: i64,
    /// Files sent with a user message (see attachments.rs).
    pub attachments: Vec<Attachment>,
    /// Streaming replies only: the batches already in `parts` (see chat.rs).
    pub seq: u64,
    /// This message and its alternatives (same parent), oldest first.
    pub siblings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub chat_id: String,
    pub title: String,
    pub updated_at: i64,
    /// Matched text with the hits wrapped in \u{1}…\u{2}.
    pub snippet: String,
    pub archived: bool,
}

pub struct Db {
    conn: Mutex<Connection>,
}

const SCHEMA_V1: &str = r#"
CREATE TABLE providers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    base_url TEXT NOT NULL,
    models TEXT NOT NULL DEFAULT '[]',
    models_at INTEGER NOT NULL DEFAULT 0,
    custom_models TEXT NOT NULL DEFAULT '[]',
    has_key INTEGER NOT NULL DEFAULT 0,
    sort INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE TABLE chats (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL DEFAULT '',
    title_auto INTEGER NOT NULL DEFAULT 1,
    provider_id TEXT,
    model TEXT,
    system_prompt TEXT NOT NULL DEFAULT '',
    leaf_id TEXT,
    pinned INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX chats_updated ON chats(updated_at DESC);
CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    parent_id TEXT,
    role TEXT NOT NULL,
    content TEXT NOT NULL DEFAULT '',
    reasoning TEXT NOT NULL DEFAULT '',
    provider_id TEXT,
    model TEXT,
    status TEXT NOT NULL DEFAULT 'done',
    error TEXT,
    stats TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX messages_chat ON messages(chat_id);
CREATE INDEX messages_parent ON messages(parent_id);
CREATE VIRTUAL TABLE messages_fts USING fts5(
    content,
    message_id UNINDEXED,
    chat_id UNINDEXED,
    tokenize = 'unicode61 remove_diacritics 2'
);
"#;

/// v6: the provider's protocol, per-chat Skills and Web search switches (the
/// old Tools switch covered skills, so they start as it was), and a reply's
/// blocks as an Anthropic server sent them (thinking goes back signed).
const SCHEMA_V6: &str = r#"
ALTER TABLE providers ADD COLUMN api TEXT NOT NULL DEFAULT 'openai';
UPDATE providers SET api = 'anthropic' WHERE base_url LIKE '%api.anthropic.com%';
ALTER TABLE chats ADD COLUMN skills INTEGER NOT NULL DEFAULT 1;
ALTER TABLE chats ADD COLUMN web INTEGER NOT NULL DEFAULT 0;
UPDATE chats SET skills = tools;
ALTER TABLE messages ADD COLUMN native TEXT;
"#;

/// v5: files on messages, archived chats, and what each model takes
/// (as detected, and as the user said).
const SCHEMA_V5: &str = r#"
ALTER TABLE messages ADD COLUMN attachments TEXT;
ALTER TABLE chats ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;
ALTER TABLE providers ADD COLUMN caps TEXT NOT NULL DEFAULT '{}';
ALTER TABLE providers ADD COLUMN caps_user TEXT NOT NULL DEFAULT '{}';
"#;

/// v4: a provider's icon (a logo, a plain icon, or an image); NULL picks one from the address.
const SCHEMA_V4: &str = r#"
ALTER TABLE providers ADD COLUMN icon TEXT;
"#;

/// v3: a provider can show only the models the user picked from its list.
const SCHEMA_V3: &str = r#"
ALTER TABLE providers ADD COLUMN show_all INTEGER NOT NULL DEFAULT 1;
"#;

const SCHEMA_V2: &str = r#"
ALTER TABLE messages ADD COLUMN parts TEXT;
ALTER TABLE chats ADD COLUMN preset_id TEXT;
ALTER TABLE chats ADD COLUMN effort TEXT;
ALTER TABLE chats ADD COLUMN tools INTEGER NOT NULL DEFAULT 1;
ALTER TABLE providers ADD COLUMN contexts TEXT NOT NULL DEFAULT '{}';
CREATE TABLE presets (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    system_prompt TEXT NOT NULL DEFAULT '',
    temperature REAL,
    top_p REAL,
    max_tokens INTEGER,
    effort TEXT,
    sort INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE TABLE mcp_servers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    command TEXT NOT NULL DEFAULT '',
    args TEXT NOT NULL DEFAULT '[]',
    cwd TEXT NOT NULL DEFAULT '',
    url TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 1,
    tool_policy TEXT NOT NULL DEFAULT '{}',
    default_policy TEXT NOT NULL DEFAULT 'ask',
    env_keys TEXT NOT NULL DEFAULT '[]',
    has_token INTEGER NOT NULL DEFAULT 0,
    sort INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
"#;

/// `chats.title_auto`: who named the chat.
const TITLE_USER: i64 = 0;
const TITLE_PROVISIONAL: i64 = 1;
const TITLE_GENERATED: i64 = 2;

const MESSAGE_COLS: &str =
    "id, chat_id, parent_id, role, content, reasoning, provider_id, model, status, error, stats, created_at, parts, attachments";

fn message_from(row: &Row) -> rusqlite::Result<Message> {
    let stats: Option<String> = row.get(10)?;
    let parts: Option<String> = row.get(12)?;
    let attachments: Option<String> = row.get(13)?;
    Ok(Message {
        id: row.get(0)?,
        chat_id: row.get(1)?,
        parent_id: row.get(2)?,
        role: row.get(3)?,
        content: row.get(4)?,
        reasoning: row.get(5)?,
        provider_id: row.get(6)?,
        model: row.get(7)?,
        status: row.get(8)?,
        error: row.get(9)?,
        stats: stats.and_then(|s| serde_json::from_str(&s).ok()),
        created_at: row.get(11)?,
        parts: parts.and_then(|s| serde_json::from_str(&s).ok()),
        attachments: attachments.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
        seq: 0,
        siblings: Vec::new(),
    })
}

const SUMMARY_COLS: &str = "id, title, provider_id, model, created_at, updated_at, pinned, preset_id, effort, tools, archived, skills, web";

fn summary_from(row: &Row) -> rusqlite::Result<ChatSummary> {
    Ok(ChatSummary {
        id: row.get(0)?,
        title: row.get(1)?,
        provider_id: row.get(2)?,
        model: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
        pinned: row.get::<_, i64>(6)? != 0,
        preset_id: row.get(7)?,
        effort: row.get(8)?,
        tools: row.get::<_, i64>(9)? != 0,
        archived: row.get::<_, i64>(10)? != 0,
        skills: row.get::<_, i64>(11)? != 0,
        web: row.get::<_, i64>(12)? != 0,
    })
}

fn json_of<T: serde::de::DeserializeOwned + Default>(text: String) -> T {
    serde_json::from_str(&text).unwrap_or_default()
}

fn text_of<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// Turns what the user typed into an FTS5 query: every word must appear,
/// the last one as a prefix (so results show while typing).
pub fn fts_query(input: &str) -> Option<String> {
    let words: Vec<String> = input
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{}\"", w.replace('"', "")))
        .collect();
    if words.is_empty() {
        return None;
    }
    let n = words.len();
    Some(words.into_iter().enumerate().map(|(i, w)| if i + 1 == n { format!("{w}*") } else { w }).collect::<Vec<_>>().join(" "))
}

/// Settings for a chat that aren't its messages.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatOptions {
    pub preset_id: Option<String>,
    pub effort: Option<String>,
    pub tools: Option<bool>,
    pub skills: Option<bool>,
    pub web: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub provider_id: Option<String>,
    pub model: String,
    pub replies: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub avg_tokens_per_sec: Option<f64>,
    pub avg_ttft_ms: Option<f64>,
    pub tool_calls: i64,
    pub errors: i64,
    pub last_used: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayUsage {
    /// Local date, "YYYY-MM-DD".
    pub day: String,
    pub replies: i64,
    pub tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub chats: i64,
    pub messages: i64,
    pub replies: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub tool_calls: i64,
    pub models: Vec<ModelUsage>,
    pub days: Vec<DayUsage>,
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn memory() -> Self {
        Self::init(Connection::open_in_memory().unwrap()).unwrap()
    }

    fn init(conn: Connection) -> anyhow::Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version < 1 {
            conn.execute_batch(&format!("BEGIN; {SCHEMA_V1} PRAGMA user_version = 1; COMMIT;"))?;
        }
        if version < 2 {
            conn.execute_batch(&format!("BEGIN; {SCHEMA_V2} PRAGMA user_version = 2; COMMIT;"))?;
        }
        if version < 3 {
            conn.execute_batch(&format!("BEGIN; {SCHEMA_V3} PRAGMA user_version = 3; COMMIT;"))?;
        }
        if version < 4 {
            conn.execute_batch(&format!("BEGIN; {SCHEMA_V4} PRAGMA user_version = 4; COMMIT;"))?;
        }
        if version < 5 {
            conn.execute_batch(&format!("BEGIN; {SCHEMA_V5} PRAGMA user_version = 5; COMMIT;"))?;
        }
        if version < 6 {
            conn.execute_batch(&format!("BEGIN; {SCHEMA_V6} PRAGMA user_version = 6; COMMIT;"))?;
        }
        // A reply that was streaming when Relay closed will never finish.
        conn.execute("UPDATE messages SET status = 'stopped' WHERE status = 'streaming'", [])?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, String> {
        let conn = self.conn.lock().unwrap();
        f(&conn).map_err(|e| e.to_string())
    }

    // ---- providers --------------------------------------------------------------

    pub fn providers(&self) -> Result<Vec<Provider>, String> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT id, name, base_url, models, models_at, custom_models, has_key, sort, contexts, show_all, icon, caps, caps_user, api
                 FROM providers ORDER BY sort, created_at",
            )?;
            let rows = st.query_map([], |r| {
                Ok(Provider {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    base_url: r.get(2)?,
                    models: json_of(r.get(3)?),
                    models_at: r.get(4)?,
                    custom_models: json_of(r.get(5)?),
                    has_key: r.get::<_, i64>(6)? != 0,
                    sort: r.get(7)?,
                    contexts: json_of(r.get(8)?),
                    show_all: r.get::<_, i64>(9)? != 0,
                    icon: r.get(10)?,
                    caps: json_of(r.get(11)?),
                    caps_user: json_of(r.get(12)?),
                    api: r.get(13)?,
                })
            })?;
            rows.collect()
        })
    }

    pub fn provider(&self, id: &str) -> Result<Option<Provider>, String> {
        Ok(self.providers()?.into_iter().find(|p| p.id == id))
    }

    pub fn save_provider(&self, p: &Provider) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "INSERT INTO providers (id, name, base_url, models, models_at, custom_models, has_key, sort, created_at, contexts, show_all, icon, caps, caps_user, api)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                 ON CONFLICT(id) DO UPDATE SET name = ?2, base_url = ?3, models = ?4, models_at = ?5,
                    custom_models = ?6, has_key = ?7, sort = ?8, contexts = ?10, show_all = ?11, icon = ?12,
                    caps = ?13, caps_user = ?14, api = ?15",
                params![
                    p.id,
                    p.name,
                    p.base_url,
                    text_of(&p.models),
                    p.models_at,
                    text_of(&p.custom_models),
                    p.has_key as i64,
                    p.sort,
                    now_ms(),
                    text_of(&p.contexts),
                    p.show_all as i64,
                    p.icon,
                    text_of(&p.caps),
                    text_of(&p.caps_user),
                    p.api
                ],
            )?;
            Ok(())
        })
    }

    pub fn delete_provider(&self, id: &str) -> Result<(), String> {
        self.with(|c| c.execute("DELETE FROM providers WHERE id = ?1", [id]).map(|_| ()))
    }

    // ---- presets ----------------------------------------------------------------------

    pub fn presets(&self) -> Result<Vec<Preset>, String> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT id, name, system_prompt, temperature, top_p, max_tokens, effort, sort FROM presets ORDER BY sort, created_at",
            )?;
            let rows = st.query_map([], |r| {
                Ok(Preset {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    system_prompt: r.get(2)?,
                    temperature: r.get(3)?,
                    top_p: r.get(4)?,
                    max_tokens: r.get::<_, Option<i64>>(5)?.map(|v| v.max(0) as u64),
                    effort: r.get(6)?,
                    sort: r.get(7)?,
                })
            })?;
            rows.collect()
        })
    }

    pub fn preset(&self, id: &str) -> Result<Option<Preset>, String> {
        Ok(self.presets()?.into_iter().find(|p| p.id == id))
    }

    pub fn save_preset(&self, p: &Preset) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "INSERT INTO presets (id, name, system_prompt, temperature, top_p, max_tokens, effort, sort, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET name = ?2, system_prompt = ?3, temperature = ?4, top_p = ?5,
                    max_tokens = ?6, effort = ?7, sort = ?8",
                params![p.id, p.name, p.system_prompt, p.temperature, p.top_p, p.max_tokens.map(|v| v as i64), p.effort, p.sort, now_ms()],
            )?;
            Ok(())
        })
    }

    pub fn delete_preset(&self, id: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute("UPDATE chats SET preset_id = NULL WHERE preset_id = ?1", [id])?;
            c.execute("DELETE FROM presets WHERE id = ?1", [id]).map(|_| ())
        })
    }

    // ---- MCP servers ----------------------------------------------------------------------

    pub fn mcp_servers(&self) -> Result<Vec<McpServer>, String> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT id, name, kind, command, args, cwd, url, enabled, tool_policy, default_policy, env_keys, has_token, sort
                 FROM mcp_servers ORDER BY sort, created_at",
            )?;
            let rows = st.query_map([], |r| {
                Ok(McpServer {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    kind: r.get(2)?,
                    command: r.get(3)?,
                    args: json_of(r.get(4)?),
                    cwd: r.get(5)?,
                    url: r.get(6)?,
                    enabled: r.get::<_, i64>(7)? != 0,
                    tool_policy: json_of(r.get(8)?),
                    default_policy: r.get(9)?,
                    env_keys: json_of(r.get(10)?),
                    has_token: r.get::<_, i64>(11)? != 0,
                    sort: r.get(12)?,
                })
            })?;
            rows.collect()
        })
    }

    pub fn mcp_server(&self, id: &str) -> Result<Option<McpServer>, String> {
        Ok(self.mcp_servers()?.into_iter().find(|s| s.id == id))
    }

    pub fn save_mcp_server(&self, s: &McpServer) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "INSERT INTO mcp_servers (id, name, kind, command, args, cwd, url, enabled, tool_policy, default_policy, env_keys, has_token, sort, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                 ON CONFLICT(id) DO UPDATE SET name = ?2, kind = ?3, command = ?4, args = ?5, cwd = ?6, url = ?7,
                    enabled = ?8, tool_policy = ?9, default_policy = ?10, env_keys = ?11, has_token = ?12, sort = ?13",
                params![
                    s.id,
                    s.name,
                    s.kind,
                    s.command,
                    text_of(&s.args),
                    s.cwd,
                    s.url,
                    s.enabled as i64,
                    text_of(&s.tool_policy),
                    s.default_policy,
                    text_of(&s.env_keys),
                    s.has_token as i64,
                    s.sort,
                    now_ms()
                ],
            )?;
            Ok(())
        })
    }

    pub fn delete_mcp_server(&self, id: &str) -> Result<(), String> {
        self.with(|c| c.execute("DELETE FROM mcp_servers WHERE id = ?1", [id]).map(|_| ()))
    }

    // ---- chats ------------------------------------------------------------------------

    pub fn chats(&self) -> Result<Vec<ChatSummary>, String> {
        self.with(|c| {
            let mut st = c.prepare(&format!("SELECT {SUMMARY_COLS} FROM chats ORDER BY pinned DESC, updated_at DESC"))?;
            let rows = st.query_map([], summary_from)?;
            rows.collect()
        })
    }

    pub fn chat_summary(&self, id: &str) -> Result<Option<ChatSummary>, String> {
        self.with(|c| c.query_row(&format!("SELECT {SUMMARY_COLS} FROM chats WHERE id = ?1"), [id], summary_from).optional())
    }

    pub fn chat(&self, id: &str) -> Result<Option<Chat>, String> {
        let head = self.with(|c| {
            c.query_row(
                &format!("SELECT {SUMMARY_COLS}, system_prompt, leaf_id FROM chats WHERE id = ?1"),
                [id],
                // Right after the summary's columns.
                |r| Ok((summary_from(r)?, r.get::<_, String>(13)?, r.get::<_, Option<String>>(14)?)),
            )
            .optional()
        })?;
        let Some((summary, system_prompt, leaf_id)) = head else { return Ok(None) };
        let mut messages = match &leaf_id {
            Some(leaf) => self.path_to(leaf)?,
            None => Vec::new(),
        };
        self.fill_siblings(id, &mut messages)?;
        Ok(Some(Chat { summary, system_prompt, leaf_id, messages }))
    }

    fn fill_siblings(&self, chat_id: &str, messages: &mut [Message]) -> Result<(), String> {
        let all: Vec<(String, Option<String>)> = self.with(|c| {
            let mut st = c.prepare("SELECT id, parent_id FROM messages WHERE chat_id = ?1 ORDER BY created_at, rowid")?;
            let rows = st.query_map([chat_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        })?;
        for m in messages.iter_mut() {
            m.siblings = all.iter().filter(|(_, p)| *p == m.parent_id).map(|(id, _)| id.clone()).collect();
        }
        Ok(())
    }

    /// The messages from the root down to `leaf`, in order.
    pub fn path_to(&self, leaf: &str) -> Result<Vec<Message>, String> {
        self.with(|c| {
            let cols = MESSAGE_COLS.split(", ").map(|col| format!("m.{col}")).collect::<Vec<_>>().join(", ");
            let mut st = c.prepare(&format!(
                "WITH RECURSIVE up(id, depth) AS (
                    SELECT ?1, 0
                    UNION ALL
                    SELECT m.parent_id, up.depth + 1 FROM messages m JOIN up ON m.id = up.id
                    WHERE m.parent_id IS NOT NULL AND up.depth < 10000
                 )
                 SELECT {cols} FROM up JOIN messages m ON m.id = up.id ORDER BY up.depth DESC"
            ))?;
            let rows = st.query_map([leaf], message_from)?;
            rows.collect()
        })
    }

    /// The newest message at the bottom of `id`'s subtree: where a branch
    /// picked from a sibling switcher leads.
    pub fn deepest_leaf(&self, id: &str) -> Result<String, String> {
        let mut cur = id.to_string();
        loop {
            let next: Option<String> = self.with(|c| {
                c.query_row(
                    "SELECT id FROM messages WHERE parent_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                    [&cur],
                    |r| r.get(0),
                )
                .optional()
            })?;
            match next {
                Some(n) => cur = n,
                None => return Ok(cur),
            }
        }
    }

    pub fn create_chat(&self, provider_id: Option<&str>, model: Option<&str>, opts: &ChatOptions) -> Result<ChatSummary, String> {
        let id = new_id();
        let now = now_ms();
        self.with(|c| {
            c.execute(
                "INSERT INTO chats (id, provider_id, model, preset_id, effort, tools, skills, web, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                params![
                    id,
                    provider_id,
                    model,
                    opts.preset_id,
                    opts.effort,
                    opts.tools.unwrap_or(true) as i64,
                    opts.skills.unwrap_or(true) as i64,
                    opts.web.unwrap_or(false) as i64,
                    now
                ],
            )?;
            Ok(())
        })?;
        self.chat_summary(&id)?.ok_or_else(|| "The new chat disappeared".into())
    }

    pub fn set_chat_options(&self, id: &str, opts: &ChatOptions) -> Result<(), String> {
        self.with(|c| {
            if let Some(p) = &opts.preset_id {
                let p = if p.is_empty() { None } else { Some(p.as_str()) };
                c.execute("UPDATE chats SET preset_id = ?2 WHERE id = ?1", params![id, p])?;
            }
            if let Some(e) = &opts.effort {
                let e = if e.is_empty() { None } else { Some(e.as_str()) };
                c.execute("UPDATE chats SET effort = ?2 WHERE id = ?1", params![id, e])?;
            }
            if let Some(t) = opts.tools {
                c.execute("UPDATE chats SET tools = ?2 WHERE id = ?1", params![id, t as i64])?;
            }
            if let Some(t) = opts.skills {
                c.execute("UPDATE chats SET skills = ?2 WHERE id = ?1", params![id, t as i64])?;
            }
            if let Some(t) = opts.web {
                c.execute("UPDATE chats SET web = ?2 WHERE id = ?1", params![id, t as i64])?;
            }
            Ok(())
        })
    }

    /// The title the user typed (never replaced), or a provisional one (the
    /// first message, until the model names the chat).
    pub fn set_title(&self, id: &str, title: &str, provisional: bool) -> Result<(), String> {
        let kind = if provisional { TITLE_PROVISIONAL } else { TITLE_USER };
        self.with(|c| c.execute("UPDATE chats SET title = ?2, title_auto = ?3 WHERE id = ?1", params![id, title, kind]).map(|_| ()))
    }

    /// A title the model suggested: final, like the user's.
    pub fn set_generated_title(&self, id: &str, title: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute("UPDATE chats SET title = ?2, title_auto = ?3 WHERE id = ?1", params![id, title, TITLE_GENERATED]).map(|_| ())
        })
    }

    /// Still on its provisional title, so the model may name it.
    pub fn title_is_provisional(&self, id: &str) -> Result<bool, String> {
        self.with(|c| {
            c.query_row("SELECT title_auto FROM chats WHERE id = ?1", [id], |r| r.get::<_, i64>(0)).map(|v| v == TITLE_PROVISIONAL)
        })
    }

    pub fn set_chat_model(&self, id: &str, provider_id: &str, model: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute("UPDATE chats SET provider_id = ?2, model = ?3 WHERE id = ?1", params![id, provider_id, model]).map(|_| ())
        })
    }

    pub fn set_pinned(&self, id: &str, pinned: bool) -> Result<(), String> {
        self.with(|c| c.execute("UPDATE chats SET pinned = ?2 WHERE id = ?1", params![id, pinned as i64]).map(|_| ()))
    }

    pub fn set_archived(&self, id: &str, archived: bool) -> Result<(), String> {
        self.with(|c| c.execute("UPDATE chats SET archived = ?2 WHERE id = ?1", params![id, archived as i64]).map(|_| ()))
    }

    /// The attachments messages still hold: in `chat_id`, or (None) anywhere.
    pub fn attachment_ids(&self, chat_id: Option<&str>) -> Result<HashSet<String>, String> {
        let lists: Vec<String> = self.with(|c| {
            let mut st = c.prepare("SELECT attachments FROM messages WHERE attachments IS NOT NULL AND (?1 IS NULL OR chat_id = ?1)")?;
            let rows = st.query_map([chat_id], |r| r.get(0))?;
            rows.collect()
        })?;
        Ok(lists.iter().filter_map(|l| serde_json::from_str::<Vec<Attachment>>(l).ok()).flatten().map(|a| a.id).collect())
    }

    pub fn set_leaf(&self, chat_id: &str, leaf: &str) -> Result<(), String> {
        self.with(|c| c.execute("UPDATE chats SET leaf_id = ?2 WHERE id = ?1", params![chat_id, leaf]).map(|_| ()))
    }

    pub fn delete_chat(&self, id: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute("DELETE FROM messages_fts WHERE chat_id = ?1", [id])?;
            c.execute("DELETE FROM messages WHERE chat_id = ?1", [id])?;
            c.execute("DELETE FROM chats WHERE id = ?1", [id])?;
            Ok(())
        })
    }

    pub fn delete_all_chats(&self) -> Result<(), String> {
        self.with(|c| c.execute_batch("DELETE FROM messages_fts; DELETE FROM messages; DELETE FROM chats;"))
    }

    // ---- messages -------------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn add_message(
        &self,
        chat_id: &str,
        parent_id: Option<&str>,
        role: &str,
        content: &str,
        attachments: &[Attachment],
        provider_id: Option<&str>,
        model: Option<&str>,
        status: &str,
    ) -> Result<Message, String> {
        let id = new_id();
        let now = now_ms();
        let files = if attachments.is_empty() { None } else { Some(text_of(&attachments)) };
        // Search finds a message by its files' names too.
        let searchable = std::iter::once(content).chain(attachments.iter().map(|a| a.name.as_str())).collect::<Vec<_>>().join("\n");
        let searchable = searchable.trim();
        self.with(|c| {
            c.execute(
                "INSERT INTO messages (id, chat_id, parent_id, role, content, provider_id, model, status, created_at, attachments)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![id, chat_id, parent_id, role, content, provider_id, model, status, now, files],
            )?;
            if !searchable.is_empty() {
                c.execute("INSERT INTO messages_fts (content, message_id, chat_id) VALUES (?1, ?2, ?3)", params![searchable, id, chat_id])?;
            }
            c.execute("UPDATE chats SET leaf_id = ?2, updated_at = ?3 WHERE id = ?1", params![chat_id, id, now])?;
            Ok(())
        })?;
        let mut m = self.message(&id)?.ok_or("The new message disappeared")?;
        let mut one = [m.clone()];
        self.fill_siblings(chat_id, &mut one)?;
        m.siblings = one[0].siblings.clone();
        Ok(m)
    }

    pub fn message(&self, id: &str) -> Result<Option<Message>, String> {
        self.with(|c| c.query_row(&format!("SELECT {MESSAGE_COLS} FROM messages WHERE id = ?1"), [id], message_from).optional())
    }

    /// Stores a reply as it ends (finished, stopped or failed).
    #[allow(clippy::too_many_arguments)]
    pub fn finish_message(
        &self,
        id: &str,
        content: &str,
        reasoning: &str,
        parts: &Value,
        status: &str,
        error: Option<&str>,
        stats: &Value,
    ) -> Result<(), String> {
        self.with(|c| {
            let chat_id: String = c.query_row("SELECT chat_id FROM messages WHERE id = ?1", [id], |r| r.get(0))?;
            c.execute(
                "UPDATE messages SET content = ?2, reasoning = ?3, status = ?4, error = ?5, stats = ?6, parts = ?7 WHERE id = ?1",
                params![id, content, reasoning, status, error, stats.to_string(), parts.to_string()],
            )?;
            c.execute("DELETE FROM messages_fts WHERE message_id = ?1", [id])?;
            if !content.is_empty() {
                c.execute("INSERT INTO messages_fts (content, message_id, chat_id) VALUES (?1, ?2, ?3)", params![content, id, chat_id])?;
            }
            c.execute("UPDATE chats SET updated_at = ?2 WHERE id = ?1", params![chat_id, now_ms()])?;
            Ok(())
        })
    }

    /// A reply's blocks as an Anthropic server sent them, one list per round
    /// (with the model that wrote them), so the next request can send them
    /// back unchanged: thinking only counts with its signature.
    pub fn set_native(&self, id: &str, native: &Value) -> Result<(), String> {
        self.with(|c| c.execute("UPDATE messages SET native = ?2 WHERE id = ?1", params![id, native.to_string()]).map(|_| ()))
    }

    pub fn natives(&self, ids: &[String]) -> Result<HashMap<String, Value>, String> {
        self.with(|c| {
            let mut st = c.prepare("SELECT native FROM messages WHERE id = ?1 AND native IS NOT NULL")?;
            let mut out = HashMap::new();
            for id in ids {
                if let Some(text) = st.query_row([id], |r| r.get::<_, String>(0)).optional()? {
                    if let Ok(v) = serde_json::from_str(&text) {
                        out.insert(id.clone(), v);
                    }
                }
            }
            Ok(out)
        })
    }

    // ---- search -----------------------------------------------------------------------------

    pub fn search(&self, input: &str, limit: usize) -> Result<Vec<SearchHit>, String> {
        let needle = input.trim().to_lowercase();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let mut hits: Vec<SearchHit> = Vec::new();
        // Titles first: they're what people remember.
        for chat in self.chats()? {
            if chat.title.to_lowercase().contains(&needle) {
                hits.push(SearchHit {
                    chat_id: chat.id,
                    title: chat.title,
                    updated_at: chat.updated_at,
                    snippet: String::new(),
                    archived: chat.archived,
                });
            }
        }
        if let Some(q) = fts_query(input) {
            let found = self.with(|c| {
                let mut st = c.prepare(
                    "SELECT f.chat_id, c.title, c.updated_at, snippet(messages_fts, 0, char(1), char(2), '…', 14), c.archived
                     FROM messages_fts f JOIN chats c ON c.id = f.chat_id
                     WHERE messages_fts MATCH ?1 ORDER BY rank LIMIT 200",
                )?;
                let rows = st.query_map([q], |r| {
                    Ok(SearchHit {
                        chat_id: r.get(0)?,
                        title: r.get(1)?,
                        updated_at: r.get(2)?,
                        snippet: r.get(3)?,
                        archived: r.get::<_, i64>(4)? != 0,
                    })
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })?;
            for hit in found {
                match hits.iter_mut().find(|h| h.chat_id == hit.chat_id) {
                    Some(h) if h.snippet.is_empty() => h.snippet = hit.snippet,
                    Some(_) => {}
                    None => hits.push(hit),
                }
            }
        }
        hits.truncate(limit);
        Ok(hits)
    }

    /// The newest chats with their last message, for Holster's preview.
    pub fn recent(&self, n: usize) -> Result<Vec<(ChatSummary, String)>, String> {
        // Archived chats stay out of Holster's preview.
        let mut chats: Vec<ChatSummary> = self.chats()?.into_iter().filter(|c| !c.title.is_empty() && !c.archived).collect();
        chats.sort_by_key(|c| std::cmp::Reverse(c.updated_at));
        chats.truncate(n);
        let mut out = Vec::new();
        for chat in chats {
            let last = self.with(|c| {
                c.query_row(
                    "SELECT m.content FROM chats c JOIN messages m ON m.id = c.leaf_id WHERE c.id = ?1",
                    [&chat.id],
                    |r| r.get::<_, String>(0),
                )
                .optional()
            })?;
            out.push((chat, last.unwrap_or_default()));
        }
        Ok(out)
    }

    // ---- stats ------------------------------------------------------------------------------

    /// Totals, per model, and per day, over the last `days` days (local
    /// time), or over everything when `days` is None.
    pub fn usage(&self, days: Option<i64>) -> Result<Usage, String> {
        let since = days.map(|d| now_ms() - d * 86_400_000).unwrap_or(0);
        self.with(|c| {
            let chats: i64 = c.query_row("SELECT count(DISTINCT chat_id) FROM messages WHERE created_at >= ?1", [since], |r| r.get(0))?;
            let messages: i64 = c.query_row("SELECT count(*) FROM messages WHERE created_at >= ?1", [since], |r| r.get(0))?;
            let tool_calls_of = "COALESCE((SELECT count(*) FROM json_each(m.parts) WHERE json_extract(json_each.value, '$.type') = 'tool'), 0)";
            let (replies, prompt_tokens, completion_tokens, tool_calls): (i64, i64, i64, i64) = c.query_row(
                &format!(
                    "SELECT count(*),
                        COALESCE(sum(json_extract(m.stats, '$.promptTokens')), 0),
                        COALESCE(sum(json_extract(m.stats, '$.completionTokens')), 0),
                        COALESCE(sum({tool_calls_of}), 0)
                     FROM messages m WHERE m.role = 'assistant' AND m.created_at >= ?1"
                ),
                [since],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            let mut st = c.prepare(&format!(
                "SELECT m.provider_id, m.model, count(*),
                    COALESCE(sum(json_extract(m.stats, '$.promptTokens')), 0),
                    COALESCE(sum(json_extract(m.stats, '$.completionTokens')), 0),
                    avg(json_extract(m.stats, '$.tokensPerSec')),
                    avg(json_extract(m.stats, '$.ttftMs')),
                    COALESCE(sum({tool_calls_of}), 0),
                    sum(m.status = 'error'),
                    max(m.created_at)
                 FROM messages m WHERE m.role = 'assistant' AND m.model IS NOT NULL AND m.created_at >= ?1
                 GROUP BY m.provider_id, m.model ORDER BY max(m.created_at) DESC"
            ))?;
            let models = st
                .query_map([since], |r| {
                    Ok(ModelUsage {
                        provider_id: r.get(0)?,
                        model: r.get(1)?,
                        replies: r.get(2)?,
                        prompt_tokens: r.get(3)?,
                        completion_tokens: r.get(4)?,
                        avg_tokens_per_sec: r.get(5)?,
                        avg_ttft_ms: r.get(6)?,
                        tool_calls: r.get(7)?,
                        errors: r.get(8)?,
                        last_used: r.get(9)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut st = c.prepare(
                "SELECT date(m.created_at / 1000, 'unixepoch', 'localtime') AS d, count(*),
                    COALESCE(sum(json_extract(m.stats, '$.completionTokens')), 0)
                 FROM messages m WHERE m.role = 'assistant' AND m.created_at >= ?1
                 GROUP BY d ORDER BY d",
            )?;
            let days = st
                .query_map([since], |r| Ok(DayUsage { day: r.get(0)?, replies: r.get(1)?, tokens: r.get(2)? }))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Usage { chats, messages, replies, prompt_tokens, completion_tokens, tool_calls, models, days })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn path_follows_the_branch() {
        let db = Db::memory();
        let chat = db.create_chat(Some("p"), Some("m"), &ChatOptions::default()).unwrap();
        let a = db.add_message(&chat.id, None, "user", "hello there", &[], None, None, "done").unwrap();
        let b = db.add_message(&chat.id, Some(&a.id), "assistant", "hi", &[], Some("p"), Some("m"), "done").unwrap();
        let b2 = db.add_message(&chat.id, Some(&a.id), "assistant", "hey", &[], Some("p"), Some("m"), "done").unwrap();
        let full = db.chat(&chat.id).unwrap().unwrap();
        assert_eq!(full.leaf_id.as_deref(), Some(b2.id.as_str()));
        assert_eq!(full.messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(), ["hello there", "hey"]);
        assert_eq!(full.messages[1].siblings, [b.id.clone(), b2.id.clone()]);
        db.set_leaf(&chat.id, &b.id).unwrap();
        let full = db.chat(&chat.id).unwrap().unwrap();
        assert_eq!(full.messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(), ["hello there", "hi"]);
        assert_eq!(db.deepest_leaf(&a.id).unwrap(), b2.id);
    }

    #[test]
    fn search_finds_titles_and_messages() {
        let db = Db::memory();
        let chat = db.create_chat(None, None, &ChatOptions::default()).unwrap();
        db.set_title(&chat.id, "Rust lifetimes", true).unwrap();
        let m = db.add_message(&chat.id, None, "user", "", &[], None, None, "streaming").unwrap();
        db.finish_message(&m.id, "Borrowing explained with café examples", "", &json!([]), "done", None, &Value::Null).unwrap();
        assert_eq!(db.search("lifetimes", 10).unwrap().len(), 1);
        let hits = db.search("cafe borrow", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains('\u{1}'));
        db.delete_chat(&chat.id).unwrap();
        assert!(db.search("borrowing", 10).unwrap().is_empty());
    }

    #[test]
    fn interrupted_replies_stop() {
        let path = std::env::temp_dir().join(format!("relay-test-{}.db", new_id()));
        {
            let db = Db::open(&path).unwrap();
            let chat = db.create_chat(None, None, &ChatOptions::default()).unwrap();
            db.add_message(&chat.id, None, "assistant", "", &[], None, None, "streaming").unwrap();
        }
        let db = Db::open(&path).unwrap();
        let chat = &db.chats().unwrap()[0];
        assert_eq!(db.chat(&chat.id).unwrap().unwrap().messages[0].status, "stopped");
        drop(db);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn usage_counts_tokens_and_tools() {
        let db = Db::memory();
        let chat = db.create_chat(Some("p"), Some("m"), &ChatOptions::default()).unwrap();
        let u = db.add_message(&chat.id, None, "user", "q", &[], None, None, "done").unwrap();
        let a = db.add_message(&chat.id, Some(&u.id), "assistant", "", &[], Some("p"), Some("m"), "streaming").unwrap();
        let parts = json!([{ "type": "tool", "id": "1" }, { "type": "text", "text": "a" }]);
        let stats = json!({ "promptTokens": 10, "completionTokens": 5, "tokensPerSec": 20.0, "ttftMs": 300 });
        db.finish_message(&a.id, "a", "", &parts, "done", None, &stats).unwrap();
        let usage = db.usage(Some(30)).unwrap();
        assert_eq!(db.usage(None).unwrap().replies, 1);
        assert_eq!((usage.replies, usage.prompt_tokens, usage.completion_tokens, usage.tool_calls), (1, 10, 5, 1));
        assert_eq!(usage.models[0].model, "m");
        assert_eq!(usage.days.len(), 1);
    }

    #[test]
    fn presets_and_servers_round_trip() {
        let db = Db::memory();
        let p = Preset {
            id: "x".into(),
            name: "Coder".into(),
            system_prompt: "Be terse".into(),
            temperature: Some(0.2),
            top_p: None,
            max_tokens: Some(2000),
            effort: Some("high".into()),
            sort: 0,
        };
        db.save_preset(&p).unwrap();
        assert_eq!(db.preset("x").unwrap().unwrap().max_tokens, Some(2000));
        let s: McpServer = serde_json::from_value(json!({ "id": "s", "name": "fs", "kind": "stdio", "command": "npx" })).unwrap();
        assert!(s.enabled && s.default_policy == "ask");
        db.save_mcp_server(&s).unwrap();
        assert_eq!(db.mcp_servers().unwrap().len(), 1);
    }

    #[test]
    fn upgrades_a_v1_database() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!("{SCHEMA_V1} PRAGMA user_version = 1;")).unwrap();
        conn.execute("INSERT INTO chats (id, title, created_at, updated_at, leaf_id) VALUES ('c', 'Old', 1, 1, 'm')", []).unwrap();
        conn.execute("INSERT INTO messages (id, chat_id, role, content, created_at) VALUES ('m', 'c', 'user', 'hi', 1)", []).unwrap();
        conn.execute("INSERT INTO providers (id, name, base_url, created_at) VALUES ('p', 'Local', 'http://x/v1', 1)", []).unwrap();
        let db = Db::init(conn).unwrap();
        let chat = db.chat("c").unwrap().unwrap();
        assert_eq!(chat.summary.title, "Old");
        assert!(chat.summary.tools && chat.summary.skills && !chat.summary.web && chat.summary.effort.is_none());
        assert_eq!(chat.messages[0].content, "hi");
        assert!(db.provider("p").unwrap().unwrap().contexts.is_empty());
        assert!(db.presets().unwrap().is_empty());
    }

    #[test]
    fn queries() {
        assert_eq!(fts_query("  ").as_deref(), None);
        assert_eq!(fts_query("rust life").as_deref(), Some("\"rust\" \"life\"*"));
    }
}

//! User preferences, one JSON file in the app data folder. Every field has a
//! default, so files written by older versions keep loading.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// "system" | "light" | "dark"
    pub theme: String,
    /// Chat text size in px.
    pub text_size: f64,
    /// "narrow" | "normal" | "wide"
    pub chat_width: String,
    /// Ask the model for a short title after the first reply. Off: the first
    /// message, shortened, is the title.
    pub auto_titles: bool,
    /// "enter": Enter sends, Shift+Enter breaks the line. "ctrl-enter": the other way round.
    pub send_key: String,
    /// Reasoning shows open while the model thinks, then folds away.
    pub expand_thinking: bool,
    /// How a reply's text appears while it streams: "token" (as it comes),
    /// "sentence", "paragraph" or "whole" (when it's done). Display only.
    pub reveal: String,
    /// Speed and token counts under each reply.
    pub show_stats: bool,
    /// The model new chats start with.
    pub default_model: Option<ModelRef>,
    /// "off" | "low" | "medium" | "high" | "xhigh" | "max"
    pub default_effort: String,
    pub default_preset: Option<String>,
    /// The model that names chats. None: the chat's own model.
    pub title_model: Option<ModelRef>,
    /// Who you are, for the model.
    pub profile: Profile,
    /// Send the profile with every chat.
    pub share_profile: bool,
    /// Tell the model today's date and time.
    pub share_date: bool,
    /// A Holster notification when a reply finishes while Relay is in the background.
    pub notify_on_reply: bool,
    /// Holster's tab glows instead when notifications are off or Relay is muted there.
    pub holster_glow: bool,
    /// How many times in a row a model may call tools before it must answer.
    pub tool_rounds: u32,
    /// Send each earlier reply's reasoning back as `reasoning_content`.
    pub send_reasoning: bool,
    pub launch_at_startup: bool,
    /// "tray": the close button keeps Relay in the tray. "quit": it quits.
    pub close_action: String,
    pub window: Option<WindowBounds>,
    pub sidebar_width: f64,
    /// Open with chats; closed, the sidebar is a slim rail of icons.
    pub sidebar_open: bool,
    /// Skills (folder names) models don't get.
    pub disabled_skills: Vec<String>,
    /// Skills whose scripts run without asking.
    pub skill_scripts: Vec<String>,
    /// How long Anthropic keeps a conversation's prompt cache: "5m" or "1h".
    pub anthropic_cache: String,
    /// The switches a new chat starts with: the last ones used.
    pub chat_skills: bool,
    pub chat_mcp: bool,
    pub chat_web: bool,
    /// What the window shows: "chat" (chats) or "agent" (Agent sessions).
    pub view: String,
    /// The permission mode a new Agent session starts with: the last one used.
    pub agent_mode: String,
    /// Files bigger than this (MB) aren't protected by checkpoints.
    pub checkpoint_file_mb: u64,
    /// The checkpoint store's soft budget per folder (GB).
    pub checkpoint_budget_gb: u64,
    /// Agent sessions get a cmd.exe tool too (off: PowerShell, and Bash where installed).
    pub agent_cmd: bool,
    /// Agent folders' own settings, by the folder's full path in lowercase.
    pub folders: std::collections::BTreeMap<String, FolderPrefs>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct FolderPrefs {
    /// "Auto may push and publish": git push, gh pr create, npm publish…
    pub auto_push: bool,
    /// Secret environment variables Auto leaves in commands (their values are masked).
    pub allow_secrets: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Profile {
    pub name: String,
    /// What models should call you (when it differs from your name).
    pub nickname: String,
    pub pronouns: String,
    pub location: String,
    pub occupation: String,
    pub about: String,
    /// How you like answers: tone, length, format.
    pub style: String,
    /// Reply in this language unless asked otherwise. Empty: the user's.
    pub language: String,
}

impl Profile {
    /// The profile as a short block for the system prompt; None when empty.
    pub fn as_prompt(&self) -> Option<String> {
        let mut lines = Vec::new();
        let field = |label: &str, v: &str, lines: &mut Vec<String>| {
            if !v.trim().is_empty() {
                lines.push(format!("{label}: {}", v.trim()));
            }
        };
        field("Name", &self.name, &mut lines);
        field("Call them", &self.nickname, &mut lines);
        field("Pronouns", &self.pronouns, &mut lines);
        field("Location", &self.location, &mut lines);
        field("Occupation", &self.occupation, &mut lines);
        field("About them", &self.about, &mut lines);
        field("How they like answers", &self.style, &mut lines);
        field("Answer in", &self.language, &mut lines);
        (!lines.is_empty()).then(|| format!("About the user you're talking with:
{}", lines.join("
")))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    pub provider_id: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WindowBounds {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub maximized: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            text_size: 14.0,
            chat_width: "normal".into(),
            auto_titles: true,
            send_key: "enter".into(),
            expand_thinking: true,
            reveal: "token".into(),
            show_stats: true,
            default_model: None,
            default_effort: "medium".into(),
            default_preset: None,
            title_model: None,
            profile: Profile::default(),
            share_profile: true,
            share_date: true,
            notify_on_reply: true,
            holster_glow: true,
            tool_rounds: 12,
            send_reasoning: true,
            launch_at_startup: false,
            close_action: "tray".into(),
            window: None,
            sidebar_width: 272.0,
            sidebar_open: true,
            disabled_skills: Vec::new(),
            skill_scripts: Vec::new(),
            anthropic_cache: "5m".into(),
            chat_skills: true,
            chat_mcp: true,
            chat_web: false,
            view: "chat".into(),
            agent_mode: "ask".into(),
            checkpoint_file_mb: 50,
            checkpoint_budget_gb: 2,
            agent_cmd: false,
            folders: Default::default(),
        }
    }
}

pub struct Prefs {
    path: PathBuf,
    current: Mutex<Settings>,
}

impl Prefs {
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join("settings.json");
        let mut current = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Settings>(&t).ok())
            .unwrap_or_default();
        // 0.2.0 had "auto"; there's no such level any more.
        current.default_effort = crate::chat::normalize_effort(&current.default_effort).to_string();
        Self { path, current: Mutex::new(current) }
    }

    pub fn get(&self) -> Settings {
        self.current.lock().unwrap().clone()
    }

    pub fn update(&self, f: impl FnOnce(&mut Settings)) -> Settings {
        let mut cur = self.current.lock().unwrap();
        f(&mut cur);
        let snapshot = cur.clone();
        drop(cur);
        self.save(&snapshot);
        snapshot
    }

    /// Merges a partial JSON object from the UI.
    pub fn patch(&self, patch: Value) -> Result<Settings, String> {
        let mut cur = self.current.lock().unwrap();
        let mut value = serde_json::to_value(&*cur).map_err(|e| e.to_string())?;
        if let (Some(obj), Value::Object(p)) = (value.as_object_mut(), patch) {
            for (k, v) in p {
                obj.insert(k, v);
            }
        }
        let next: Settings = serde_json::from_value(value).map_err(|e| format!("Invalid settings: {e}"))?;
        *cur = next.clone();
        drop(cur);
        self.save(&next);
        Ok(next)
    }

    fn save(&self, s: &Settings) {
        if let Ok(text) = serde_json::to_string_pretty(s) {
            if let Err(e) = write_atomic(&self.path, &text) {
                eprintln!("settings: couldn't save: {e}");
            }
        }
    }
}

/// Writes through a temp file and a rename, so readers never see half a file.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// `write_atomic`, skipped when the file already holds exactly this text.
pub fn write_if_changed(path: &Path, text: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path).ok().as_deref() == Some(text) {
        return Ok(());
    }
    write_atomic(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_files_keep_loading() {
        let s: Settings = serde_json::from_str(r#"{"theme":"dark"}"#).unwrap();
        assert_eq!(s.theme, "dark");
        assert!(s.auto_titles);
        assert_eq!(s.send_key, "enter");
        assert_eq!(s.default_effort, "medium");
    }

    #[test]
    fn profile_prompt() {
        assert_eq!(Profile::default().as_prompt(), None);
        let p = Profile { name: "Sam".into(), style: " Short answers ".into(), ..Default::default() };
        assert_eq!(p.as_prompt().unwrap(), "About the user you're talking with:
Name: Sam
How they like answers: Short answers");
    }
}

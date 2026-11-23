//! Tauri commands: everything the UI asks of the backend.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

use crate::attachments::{self, Attachment};
use crate::db::{new_id, now_ms, Caps, Chat, ChatOptions, ChatSummary, McpServer, Message, Preset, Provider, SearchHit, Usage};
use crate::mcp::{self, ServerStatus};
use crate::settings::Settings;
use crate::{agent, chat, db, holster, llm, secrets, skills, Core};

type CoreState<'a> = State<'a, Arc<Core>>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    version: String,
    data_dir: String,
}

/// Asks GitHub for a newer Relay (Settings → About → Updates).
#[tauri::command]
pub async fn update_check(app: AppHandle, pending: tauri::State<'_, crate::update::Pending>) -> Result<Option<crate::update::Available>, String> {
    crate::update::check(&app, &pending).await
}

/// Downloads, verifies and runs the update `update_check` found.
#[tauri::command]
pub async fn update_install(app: AppHandle, pending: tauri::State<'_, crate::update::Pending>) -> Result<(), String> {
    crate::update::install(&app, &pending).await
}

#[tauri::command]
pub fn app_info(core: CoreState) -> AppInfo {
    AppInfo { version: env!("CARGO_PKG_VERSION").into(), data_dir: core.data_dir.display().to_string() }
}

#[tauri::command]
pub fn get_settings(core: CoreState) -> Settings {
    core.prefs.get()
}

#[tauri::command]
pub fn update_settings(app: AppHandle, core: CoreState, patch: Value) -> Result<Settings, String> {
    let before = core.prefs.get();
    let after = core.prefs.patch(patch)?;
    crate::apply_settings_change(&app, &before, &after);
    Ok(after)
}

/// What the model is told before the conversation, with the current settings.
#[tauri::command]
pub fn system_prompt_preview(core: CoreState, preset_id: Option<String>) -> String {
    let preset = preset_id.as_deref().and_then(|id| core.db.preset(id).ok().flatten());
    chat::system_prompt(&core, preset.as_ref(), "")
}

// ---- skills -----------------------------------------------------------------------

#[tauri::command]
pub fn skills_list(core: CoreState) -> Vec<skills::Skill> {
    skills::list(&core)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillsInfo {
    /// What models get in the system prompt (in chats that use tools).
    prompt: String,
    folder: String,
    workspace: String,
}

#[tauri::command]
pub fn skills_info(core: CoreState) -> SkillsInfo {
    SkillsInfo {
        prompt: skills::prompt_block(&skills::usable(&core)).unwrap_or_default(),
        folder: skills::root(&core).display().to_string(),
        workspace: skills::workspace(&core).display().to_string(),
    }
}

#[tauri::command]
pub fn skill_save(app: AppHandle, core: CoreState, id: Option<String>, name: String, description: String, body: String) -> Result<skills::Skill, String> {
    let s = skills::save(&core, id.as_deref().filter(|i| !i.is_empty()), &name, &description, &body)?;
    let _ = app.emit("skills-changed", ());
    Ok(s)
}

#[tauri::command]
pub fn skill_delete(app: AppHandle, core: CoreState, id: String) -> Result<(), String> {
    skills::delete(&core, &id)?;
    let _ = app.emit("skills-changed", ());
    Ok(())
}

#[tauri::command]
pub fn skill_set(app: AppHandle, core: CoreState, id: String, enabled: Option<bool>, scripts_allowed: Option<bool>) {
    skills::set_flags(&core, &id, enabled, scripts_allowed);
    let _ = app.emit("skills-changed", ());
}

#[tauri::command]
pub fn skill_import(app: AppHandle, core: CoreState, path: String) -> Result<skills::Imported, String> {
    let out = skills::import(&core, std::path::Path::new(&path))?;
    let _ = app.emit("skills-changed", ());
    Ok(out)
}

/// Opens a skill's folder, the skills folder ("") or the scripts' workspace ("workspace").
#[tauri::command]
pub fn skills_open_folder(app: AppHandle, core: CoreState, which: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let path = match which.as_str() {
        "" => skills::root(&core),
        "workspace" => skills::workspace(&core),
        id if skills::valid_id(id) => skills::root(&core).join(id),
        _ => return Err("That isn't a folder Relay knows".into()),
    };
    std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    app.opener().open_path(path.to_string_lossy(), None::<&str>).map_err(|e| e.to_string())
}

// ---- providers --------------------------------------------------------------------

#[tauri::command]
pub fn providers_list(core: CoreState) -> Result<Vec<Provider>, String> {
    core.db.providers()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInput {
    id: Option<String>,
    name: String,
    base_url: String,
    #[serde(default)]
    custom_models: Vec<String>,
    /// None leaves the stored key alone; an empty string removes it.
    key: Option<String>,
    /// The model list a successful test just fetched.
    models: Option<Vec<String>>,
    /// Context windows the user set by hand (0: detect again).
    contexts: Option<HashMap<String, u64>>,
    /// Show every model the server lists (else only `custom_models`).
    show_all: Option<bool>,
    /// None leaves the icon alone; "" goes back to the one chosen from the address.
    icon: Option<String>,
    /// "openai" or "anthropic". None: unchanged (new providers pick by address).
    api: Option<String>,
}

#[tauri::command]
pub fn provider_save(core: CoreState, input: ProviderInput) -> Result<Provider, String> {
    let name = input.name.trim();
    let typed = llm::normalize_base(&input.base_url);
    if name.is_empty() {
        return Err("Give the provider a name".into());
    }
    if !(typed.starts_with("http://") || typed.starts_with("https://")) {
        return Err("The address should start with http:// or https://".into());
    }
    let existing = match &input.id {
        Some(id) => core.db.provider(id)?,
        None => None,
    };
    let sort = existing.as_ref().map(|p| p.sort).unwrap_or_else(|| core.db.providers().map(|v| v.len() as i64).unwrap_or(0));
    let mut p = existing.unwrap_or(Provider {
        id: new_id(),
        name: String::new(),
        base_url: String::new(),
        models: Vec::new(),
        models_at: 0,
        custom_models: Vec::new(),
        show_all: true,
        icon: None,
        contexts: HashMap::new(),
        caps: HashMap::new(),
        caps_user: HashMap::new(),
        has_key: false,
        sort,
        api: if typed.contains("api.anthropic.com") { "anthropic".into() } else { "openai".into() },
    });
    if let Some(api) = input.api.as_deref() {
        if api != "openai" && api != "anthropic" {
            return Err(format!("Unknown API \"{api}\""));
        }
        p.api = api.to_string();
    }
    let base_url = llm::api_base(&p.api, &typed);
    if let Some(all) = input.show_all {
        p.show_all = all;
    }
    if let Some(icon) = input.icon.as_deref().map(str::trim) {
        p.icon = if icon.is_empty() { None } else { Some(valid_icon(icon)?.to_string()) };
    }
    if p.base_url != base_url {
        p.models.clear();
        p.models_at = 0;
        p.contexts.clear();
        p.caps.clear();
    }
    p.name = name.to_string();
    p.base_url = base_url;
    p.custom_models = input.custom_models.into_iter().map(|m| m.trim().to_string()).filter(|m| !m.is_empty()).collect();
    p.custom_models.dedup();
    if let Some(models) = input.models {
        p.models = models;
        p.models_at = now_ms();
    }
    if let Some(ctx) = input.contexts {
        for (model, n) in ctx {
            if n == 0 {
                p.contexts.remove(&model);
            } else {
                p.contexts.insert(model, n);
            }
        }
    }
    match input.key.as_deref().map(str::trim) {
        Some("") => {
            secrets::delete_key(&p.id)?;
            p.has_key = false;
        }
        Some(key) => {
            secrets::set_key(&p.id, key)?;
            p.has_key = true;
        }
        None => {}
    }
    core.db.save_provider(&p)?;
    Ok(p)
}

/// Largest image a provider icon may be (it's stored in the database).
const ICON_MAX: u64 = 256 * 1024;

fn valid_icon(icon: &str) -> Result<&str, String> {
    let named = |p: &str| icon.strip_prefix(p).is_some_and(|n| !n.is_empty() && n.len() <= 40 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    if named("brand:") || named("lucide:") {
        return Ok(icon);
    }
    if icon.starts_with("data:image/") && icon.contains(";base64,") && (icon.len() as u64) < ICON_MAX * 4 / 3 + 64 {
        return Ok(icon);
    }
    Err("That icon can't be used".into())
}

/// Reads an image for a provider's icon and returns it as a data URL.
#[tauri::command]
pub fn icon_from_file(path: String) -> Result<String, String> {
    use base64::Engine as _;
    let path = std::path::Path::new(&path);
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        _ => return Err("Pick a PNG, JPG, WebP, GIF, SVG or ICO image".into()),
    };
    let size = std::fs::metadata(path).map_err(|e| format!("Couldn't read that file ({e})"))?.len();
    if size > ICON_MAX {
        return Err(format!("That image is {} KB; icons can be up to {} KB", size / 1024, ICON_MAX / 1024));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("Couldn't read that file ({e})"))?;
    Ok(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
}

#[tauri::command]
pub fn provider_delete(core: CoreState, id: String) -> Result<(), String> {
    let _ = secrets::delete_key(&id);
    core.db.delete_provider(&id)
}

/// Asks an endpoint for its models. Used to test a provider before saving it
/// (`key` as typed, or the stored key of `id` when the field was left alone).
#[tauri::command]
pub async fn provider_test(core: CoreState<'_>, base_url: String, key: Option<String>, id: Option<String>, kind: Option<String>) -> Result<Vec<String>, String> {
    let api = kind.filter(|k| k == "anthropic").unwrap_or_else(|| "openai".into());
    let base_url = llm::api_base(&api, &base_url);
    let key = match (key, id) {
        (Some(k), _) => Some(k),
        (None, Some(id)) => {
            let stored = secrets::key(&id);
            // A stored key only goes to the server it was saved for.
            let saved = core.db.provider(&id)?.map(|p| p.base_url).unwrap_or_default();
            if stored.is_some() && !llm::same_host(&saved, &base_url) {
                return Err("The address changed: type the key again to test it there".into());
            }
            stored
        }
        (None, None) => None,
    };
    llm::list_models(&api, &base_url, key.as_deref()).await
}

#[tauri::command]
pub async fn provider_refresh(core: CoreState<'_>, id: String) -> Result<Provider, String> {
    let mut p = core.db.provider(&id)?.ok_or("That provider was removed")?;
    let key = secrets::key(&p.id);
    p.models = llm::list_models(&p.api, &p.base_url, key.as_deref()).await?;
    p.models_at = now_ms();
    core.db.save_provider(&p)?;
    Ok(p)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatus {
    id: String,
    online: bool,
    /// What it lists now; None when it doesn't list models.
    models: Option<Vec<String>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    statuses: Vec<ProviderStatus>,
    /// A server's list changed, and Relay saved the new one.
    changed: bool,
}

/// Asks every provider, at once, whether it's there and what it lists, for
/// the model picker's Offline section. A list that changed is saved, so new
/// models show up (and removed ones go) without pressing refresh.
#[tauri::command]
pub async fn providers_probe(core: CoreState<'_>) -> Result<ProbeResult, String> {
    let providers = core.db.providers()?;
    let checks = providers.iter().map(|p| {
        let key = secrets::key(&p.id);
        let base = p.base_url.clone();
        let api = p.api.clone();
        async move { llm::probe(&api, &base, key.as_deref()).await }
    });
    let answers = futures_util::future::join_all(checks).await;
    let mut changed = false;
    let mut statuses = Vec::new();
    for (mut p, (online, models)) in providers.into_iter().zip(answers) {
        if let Some(list) = models.as_ref().filter(|l| **l != p.models) {
            p.models = list.clone();
            p.models_at = now_ms();
            if core.db.save_provider(&p).is_ok() {
                changed = true;
            }
        }
        statuses.push(ProviderStatus { id: p.id, online, models });
    }
    Ok(ProbeResult { statuses, changed })
}

/// The model's context window: set by the user, remembered, or asked of the server.
#[tauri::command]
pub async fn model_context(core: CoreState<'_>, provider_id: String, model: String) -> Result<Option<u64>, String> {
    let Some(p) = core.db.provider(&provider_id)? else { return Ok(None) };
    if let Some(n) = p.contexts.get(&model) {
        return Ok(Some(*n));
    }
    let key = secrets::key(&p.id);
    let found = llm::detect_context(&p.base_url, key.as_deref(), &model).await;
    if let Some(n) = found {
        // Read again: the user may have edited the provider meanwhile.
        if let Some(mut fresh) = core.db.provider(&provider_id)? {
            fresh.contexts.insert(model, n);
            core.db.save_provider(&fresh)?;
        }
    }
    Ok(found)
}

/// What a model takes besides text: `caps` is what Relay goes by (detected or
/// guessed, with the user's word over it), `user` what the user said.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCaps {
    caps: Caps,
    detected: Caps,
    user: HashMap<String, bool>,
}

fn model_caps_of(p: &Provider, model: &str) -> ModelCaps {
    let detected = p.caps.get(model).copied().unwrap_or_else(|| llm::guess_caps(&p.base_url, model));
    let user = p.caps_user.get(model).cloned().unwrap_or_default();
    ModelCaps { caps: detected.with_user(Some(&user)), detected, user }
}

/// What a model takes: remembered, or asked of the server once.
#[tauri::command]
pub async fn model_caps(core: CoreState<'_>, provider_id: String, model: String) -> Result<Option<ModelCaps>, String> {
    let Some(p) = core.db.provider(&provider_id)? else { return Ok(None) };
    if p.caps.contains_key(&model) {
        return Ok(Some(model_caps_of(&p, &model)));
    }
    let key = secrets::key(&p.id);
    let found = llm::detect_caps(&p.base_url, key.as_deref(), &model).await;
    // Read again: the user may have edited the provider meanwhile.
    let Some(mut fresh) = core.db.provider(&provider_id)? else { return Ok(None) };
    if let Some(caps) = found {
        fresh.caps.insert(model.clone(), caps);
        core.db.save_provider(&fresh)?;
    }
    Ok(Some(model_caps_of(&fresh, &model)))
}

/// Asks a model whether it calls tools (one small request) and remembers
/// the answer as detected.
#[tauri::command]
pub async fn model_check_tools(core: CoreState<'_>, provider_id: String, model: String) -> Result<Option<ModelCaps>, String> {
    let Some(p) = core.db.provider(&provider_id)? else { return Ok(None) };
    let key = secrets::key(&p.id);
    let found = llm::check_tools(&p.api, &p.base_url, key.as_deref(), &model).await;
    let Some(mut fresh) = core.db.provider(&provider_id)? else { return Ok(None) };
    if let Some(yes) = found {
        let mut caps = fresh.caps.get(&model).copied().unwrap_or_else(|| llm::guess_caps(&fresh.base_url, &model));
        caps.tools = Some(yes);
        fresh.caps.insert(model.clone(), caps);
        core.db.save_provider(&fresh)?;
    } else {
        return Err("The model didn't answer: check that its server is running".into());
    }
    Ok(Some(model_caps_of(&fresh, &model)))
}

/// The user's word on one ability of a model (None: go back to what was detected).
#[tauri::command]
pub fn model_caps_set(core: CoreState, provider_id: String, model: String, ability: String, on: Option<bool>) -> Result<ModelCaps, String> {
    if !Caps::ABILITIES.contains(&ability.as_str()) {
        return Err(format!("Relay doesn't know \"{ability}\""));
    }
    let mut p = core.db.provider(&provider_id)?.ok_or("That provider was removed")?;
    let user = p.caps_user.entry(model.clone()).or_default();
    match on {
        Some(v) => {
            user.insert(ability, v);
        }
        None => {
            user.remove(&ability);
        }
    }
    if user.is_empty() {
        p.caps_user.remove(&model);
    }
    core.db.save_provider(&p)?;
    Ok(model_caps_of(&p, &model))
}

// ---- attachments ------------------------------------------------------------------------

fn files_root(core: &Core) -> std::path::PathBuf {
    attachments::root(&core.data_dir)
}

/// A file from disk (picked or dropped).
#[tauri::command]
pub async fn attach_path(core: CoreState<'_>, path: String) -> Result<Attachment, String> {
    let root = files_root(&core);
    tauri::async_runtime::spawn_blocking(move || attachments::add_path(&root, std::path::Path::new(&path))).await.map_err(|e| e.to_string())?
}

fn header(request: &tauri::ipc::Request<'_>, name: &str) -> Option<String> {
    request.headers().get(name).and_then(|v| v.to_str().ok()).map(attachments::decode_header)
}

fn raw_body(request: &tauri::ipc::Request<'_>) -> Result<Vec<u8>, String> {
    match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => Ok(bytes.clone()),
        _ => Err("The file didn't come through".into()),
    }
}

/// A file from bytes: pasted, or a voice message. Headers: `x-name`
/// (URI-encoded), and for recordings `x-voice: 1` and `x-duration` (seconds).
#[tauri::command]
pub async fn attach_bytes(core: CoreState<'_>, request: tauri::ipc::Request<'_>) -> Result<Attachment, String> {
    let bytes = raw_body(&request)?;
    let name = header(&request, "x-name").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "file".into());
    let voice = header(&request, "x-voice").as_deref() == Some("1");
    let duration = header(&request, "x-duration").and_then(|d| d.parse().ok());
    let root = files_root(&core);
    tauri::async_runtime::spawn_blocking(move || attachments::add_bytes(&root, &name, &bytes, voice, duration)).await.map_err(|e| e.to_string())?
}

/// A file Relay made for an attachment (headers `x-id`, `x-file`, `x-at`): a
/// video still or the sound of a video or audio file.
#[tauri::command]
pub fn attach_put(core: CoreState, request: tauri::ipc::Request<'_>) -> Result<Attachment, String> {
    let bytes = raw_body(&request)?;
    let id = header(&request, "x-id").unwrap_or_default();
    let file = header(&request, "x-file").unwrap_or_default();
    let at = header(&request, "x-at").and_then(|d| d.parse().ok());
    attachments::put(&files_root(&core), &id, &file, &bytes, at)
}

/// What the UI measured of a video, audio file or PDF.
#[tauri::command]
pub fn attach_info(
    core: CoreState,
    id: String,
    duration: Option<f64>,
    width: Option<u32>,
    height: Option<u32>,
    page_count: Option<u32>,
) -> Result<Attachment, String> {
    attachments::set_info(&files_root(&core), &id, duration, width, height, page_count)
}

/// Files taken out of the composer before sending.
#[tauri::command]
pub fn attach_discard(core: CoreState, ids: Vec<String>) -> Result<(), String> {
    let keep = core.db.attachment_ids(None)?;
    attachments::forget(&files_root(&core), ids, &keep);
    Ok(())
}

/// Opens an attachment in the app Windows uses for it.
#[tauri::command]
pub fn attach_open(app: AppHandle, core: CoreState, id: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let root = files_root(&core);
    let a = attachments::load(&root, &id)?;
    let path = attachments::path_of(&root, &a);
    // A script or program would run, not open: show it in its folder instead.
    if attachments::runs_when_opened(&a.file) {
        return app.opener().reveal_item_in_dir(&path).map_err(|e| e.to_string());
    }
    app.opener().open_path(path.to_string_lossy(), None::<&str>).map_err(|e| e.to_string())
}

// ---- presets --------------------------------------------------------------------------

#[tauri::command]
pub fn presets_list(core: CoreState) -> Result<Vec<Preset>, String> {
    core.db.presets()
}

#[tauri::command]
pub fn preset_save(core: CoreState, mut preset: Preset) -> Result<Preset, String> {
    preset.name = preset.name.trim().to_string();
    if preset.name.is_empty() {
        return Err("Give the preset a name".into());
    }
    if preset.id.is_empty() {
        preset.id = new_id();
        preset.sort = core.db.presets().map(|v| v.len() as i64).unwrap_or(0);
    }
    preset.temperature = preset.temperature.map(|t| t.clamp(0.0, 2.0));
    preset.top_p = preset.top_p.map(|t| t.clamp(0.0, 1.0));
    preset.effort = preset.effort.filter(|e| !e.is_empty());
    core.db.save_preset(&preset)?;
    Ok(preset)
}

#[tauri::command]
pub fn preset_delete(core: CoreState, id: String) -> Result<(), String> {
    core.db.delete_preset(&id)?;
    if core.prefs.get().default_preset.as_deref() == Some(id.as_str()) {
        core.prefs.update(|s| s.default_preset = None);
    }
    Ok(())
}

// ---- MCP servers ------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpView {
    #[serde(flatten)]
    server: McpServer,
    status: ServerStatus,
}

#[tauri::command]
pub fn mcp_list(core: CoreState) -> Result<Vec<McpView>, String> {
    Ok(core
        .db
        .mcp_servers()?
        .into_iter()
        .map(|s| {
            let status = core.mcp.status_of(&s.id).unwrap_or(ServerStatus { id: s.id.clone(), state: "off".into(), ..Default::default() });
            McpView { server: s, status }
        })
        .collect())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpInput {
    #[serde(flatten)]
    server: McpServer,
    /// The environment, whole (None leaves it as it is).
    env: Option<HashMap<String, String>>,
    /// None leaves the stored token alone; an empty string removes it.
    token: Option<String>,
}

#[tauri::command]
pub fn mcp_save(app: AppHandle, core: CoreState, input: McpInput) -> Result<McpServer, String> {
    let mut s = input.server;
    s.name = s.name.trim().to_string();
    s.command = s.command.trim().to_string();
    s.url = s.url.trim().to_string();
    if s.name.is_empty() {
        return Err("Give the server a name".into());
    }
    if s.kind == "stdio" && s.command.is_empty() {
        return Err("Add the command that starts the server".into());
    }
    if s.kind == "http" && !(s.url.starts_with("http://") || s.url.starts_with("https://")) {
        return Err("The address should start with http:// or https://".into());
    }
    let existing = if s.id.is_empty() { None } else { core.db.mcp_server(&s.id)? };
    if s.id.is_empty() {
        s.id = new_id();
        s.sort = core.db.mcp_servers().map(|v| v.len() as i64).unwrap_or(0);
    }
    if let Some(e) = &existing {
        // Policies change one tool at a time, elsewhere; keep them.
        s.tool_policy = e.tool_policy.clone();
        s.default_policy = e.default_policy.clone();
    }
    let mut stored = mcp::load_secrets(&s.id);
    if let Some(env) = input.env {
        stored.env = env.into_iter().filter(|(k, _)| !k.trim().is_empty()).map(|(k, v)| (k.trim().to_string(), v)).collect();
    }
    if let Some(t) = input.token {
        stored.token = t.trim().to_string();
    }
    mcp::save_secrets(&s.id, &stored)?;
    let mut keys: Vec<String> = stored.env.keys().cloned().collect();
    keys.sort();
    s.env_keys = keys;
    s.has_token = !stored.token.is_empty();
    core.db.save_mcp_server(&s)?;
    core.mcp.connect(s.clone());
    let _ = app.emit("mcp-servers-changed", ());
    Ok(s)
}

/// The server's environment values, for editing.
#[tauri::command]
pub fn mcp_env(id: String) -> HashMap<String, String> {
    mcp::load_secrets(&id).env
}

#[tauri::command]
pub fn mcp_delete(core: CoreState, id: String) -> Result<(), String> {
    core.mcp.forget(&id);
    let _ = mcp::save_secrets(&id, &mcp::Secrets::default());
    core.db.delete_mcp_server(&id)
}

#[tauri::command]
pub fn mcp_restart(core: CoreState, id: String) -> Result<(), String> {
    let s = core.db.mcp_server(&id)?.ok_or("That server was removed")?;
    core.mcp.connect(s);
    Ok(())
}

#[tauri::command]
pub fn mcp_set_enabled(core: CoreState, id: String, enabled: bool) -> Result<(), String> {
    let mut s = core.db.mcp_server(&id)?.ok_or("That server was removed")?;
    s.enabled = enabled;
    core.db.save_mcp_server(&s)?;
    core.mcp.connect(s);
    Ok(())
}

/// One tool's policy ("ask" | "allow" | "off"), or the server's default when
/// `tool` is None (which resets the per-tool ones).
#[tauri::command]
pub fn mcp_set_policy(core: CoreState, id: String, tool: Option<String>, policy: String) -> Result<(), String> {
    if !matches!(policy.as_str(), "ask" | "allow" | "off") {
        return Err("Unknown policy".into());
    }
    let mut s = core.db.mcp_server(&id)?.ok_or("That server was removed")?;
    match tool {
        Some(t) => {
            if policy == s.default_policy {
                s.tool_policy.remove(&t);
            } else {
                s.tool_policy.insert(t, policy);
            }
        }
        None => {
            s.default_policy = policy;
            s.tool_policy.clear();
        }
    }
    core.db.save_mcp_server(&s)
}

/// Adds the servers from a Claude Desktop style JSON config. Returns how many.
#[tauri::command]
pub fn mcp_import(app: AppHandle, core: CoreState, text: String) -> Result<usize, String> {
    let found = mcp::parse_import(&text)?;
    let mut sort = core.db.mcp_servers().map(|v| v.len() as i64).unwrap_or(0);
    let n = found.len();
    for (mut s, stored) in found {
        s.sort = sort;
        sort += 1;
        mcp::save_secrets(&s.id, &stored)?;
        core.db.save_mcp_server(&s)?;
        core.mcp.connect(s);
    }
    let _ = app.emit("mcp-servers-changed", ());
    Ok(n)
}

#[tauri::command]
pub fn tool_decide(core: CoreState, message_id: String, call_id: String, decision: String) -> Result<(), String> {
    if !matches!(decision.as_str(), "once" | "always" | "deny" | "edits") {
        return Err("Unknown decision".into());
    }
    if core.streams.decide(&message_id, &call_id, &decision) {
        Ok(())
    } else {
        Err("That tool call isn't waiting anymore".into())
    }
}

// ---- chats --------------------------------------------------------------------------

#[tauri::command]
pub fn chats_list(core: CoreState) -> Result<Vec<ChatSummary>, String> {
    core.db.chats()
}

#[tauri::command]
pub fn chat_get(core: CoreState, id: String) -> Result<Option<Chat>, String> {
    let mut chat = core.db.chat(&id)?;
    if let Some(c) = chat.as_mut() {
        core.streams.overlay(&mut c.messages);
    }
    Ok(chat)
}

#[tauri::command]
pub fn chat_rename(app: AppHandle, core: CoreState, id: String, title: String) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("A chat needs a name".into());
    }
    core.db.set_title(&id, &title.chars().take(120).collect::<String>(), false)?;
    holster::publish(&app);
    Ok(())
}

#[tauri::command]
pub fn chat_pin(core: CoreState, id: String, pinned: bool) -> Result<(), String> {
    core.db.set_pinned(&id, pinned)
}

#[tauri::command]
pub fn chat_archive(app: AppHandle, core: CoreState, id: String, archived: bool) -> Result<(), String> {
    core.db.set_archived(&id, archived)?;
    holster::publish(&app);
    Ok(())
}

#[tauri::command]
pub fn chat_delete(
    app: AppHandle,
    core: CoreState,
    id: String,
    delete_folder: Option<bool>,
    delete_checkpoints: Option<bool>,
) -> Result<(), String> {
    core.streams.stop_chat(&id);
    agent::shell::Processes::global().stop_all(&id);
    let files = core.db.attachment_ids(Some(&id))?;
    let summary = core.db.chat_summary(&id)?;
    core.db.delete_chat(&id)?;
    // A session's checkpoints go when the user said so (with its workspace,
    // always); kept, they stay as the folder's file history.
    if delete_checkpoints == Some(true) || delete_folder == Some(true) {
        let _ = agent::checkpoints::forget_session(&agent::checkpoints::Store::of(&core), &id);
    }
    // A workspace Relay made goes too when the user said so; a folder they
    // picked never does.
    if let (Some(chat), Some(true)) = (summary, delete_folder) {
        agent::remove_workspace(&core, &chat)?;
    }
    // Its files go too, unless another chat's message still holds them.
    attachments::forget(&files_root(&core), files, &core.db.attachment_ids(None)?);
    // Its words leave the search index and the log in the background.
    let bg = core.inner().clone();
    std::thread::spawn(move || {
        let _ = bg.db.scrub();
    });
    holster::seen(&id);
    holster::publish(&app);
    Ok(())
}

#[tauri::command]
pub fn chats_delete_all(app: AppHandle, core: CoreState) -> Result<(), String> {
    for id in core.streams.active_chats() {
        core.streams.stop_chat(&id);
    }
    let files = core.db.attachment_ids(None)?;
    core.db.delete_all_chats()?;
    attachments::forget(&files_root(&core), files, &Default::default());
    holster::publish(&app);
    Ok(())
}

#[tauri::command]
pub fn chat_set_model(core: CoreState, id: String, provider_id: String, model: String) -> Result<(), String> {
    core.db.set_chat_model(&id, &provider_id, &model)
}

#[tauri::command]
pub fn chat_set_options(core: CoreState, id: String, options: ChatOptions) -> Result<Option<ChatSummary>, String> {
    core.db.set_chat_options(&id, &options)?;
    core.db.chat_summary(&id)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendResult {
    chat: ChatSummary,
    user: Option<Message>,
    assistant: Message,
}

/// Sends a message; without a `chat_id` it starts a new chat first, with the
/// options the composer had picked.
#[tauri::command]
pub fn chat_send(
    app: AppHandle,
    core: CoreState,
    chat_id: Option<String>,
    text: String,
    attachments: Option<Vec<String>>,
    provider_id: String,
    model: String,
    options: Option<ChatOptions>,
) -> Result<SendResult, String> {
    let files = attachments.unwrap_or_default();
    if text.trim().is_empty() && files.is_empty() {
        return Err("Type a message first".into());
    }
    let chat_id = match chat_id {
        Some(id) => id,
        None => {
            let options = options.unwrap_or_default();
            if let Some(folder) = options.folder.as_deref().filter(|f| !f.is_empty()) {
                agent::check_folder(folder)?;
            }
            core.db.create_chat(Some(&provider_id), Some(&model), &options)?.id
        }
    };
    if let Some(summary) = core.db.chat_summary(&chat_id)?.filter(|c| c.kind == "agent") {
        agent::ensure_folder(&core, &summary)?;
    }
    let sent = chat::send(&app, &chat_id, &text, &files, &provider_id, &model)?;
    let chat = core.db.chat_summary(&chat_id)?.ok_or("That chat no longer exists")?;
    Ok(SendResult { chat, user: sent.user, assistant: sent.assistant })
}

/// The chat as it is now (the branch on screen changed).
fn reloaded(core: &Core, id: &str) -> Result<Chat, String> {
    let mut chat = core.db.chat(id)?.ok_or("That chat no longer exists")?;
    core.streams.overlay(&mut chat.messages);
    Ok(chat)
}

#[tauri::command]
pub fn chat_edit(
    app: AppHandle,
    core: CoreState,
    chat_id: String,
    message_id: String,
    text: String,
    attachments: Option<Vec<String>>,
    provider_id: String,
    model: String,
) -> Result<Chat, String> {
    let files = attachments.unwrap_or_default();
    if text.trim().is_empty() && files.is_empty() {
        return Err("The message can't be empty".into());
    }
    chat::edit(&app, &chat_id, &message_id, &text, &files, &provider_id, &model)?;
    reloaded(&core, &chat_id)
}

#[tauri::command]
pub fn chat_regenerate(
    app: AppHandle,
    core: CoreState,
    chat_id: String,
    message_id: String,
    provider_id: String,
    model: String,
) -> Result<Chat, String> {
    chat::regenerate(&app, &chat_id, &message_id, &provider_id, &model)?;
    reloaded(&core, &chat_id)
}

/// Continue: a reply that didn't finish goes on in the same message.
#[tauri::command]
pub fn chat_continue(app: AppHandle, core: CoreState, chat_id: String, message_id: String) -> Result<Chat, String> {
    chat::continue_reply(&app, &chat_id, &message_id)?;
    reloaded(&core, &chat_id)
}

/// Shows another branch: the newest conversation that runs through `message_id`.
#[tauri::command]
pub fn chat_branch(core: CoreState, chat_id: String, message_id: String) -> Result<Chat, String> {
    let leaf = core.db.deepest_leaf(&message_id)?;
    core.db.set_leaf(&chat_id, &leaf)?;
    reloaded(&core, &chat_id)
}

#[tauri::command]
pub fn chat_stop(core: CoreState, chat_id: String) {
    core.streams.stop_chat(&chat_id);
}

#[tauri::command]
pub fn streaming_chats(core: CoreState) -> Vec<String> {
    core.streams.active_chats()
}

#[tauri::command]
pub fn chat_search(core: CoreState, query: String, kind: Option<String>) -> Result<Vec<SearchHit>, String> {
    core.db.search(&query, 50, db::kind_of(kind.as_deref()))
}

// ---- agent sessions -------------------------------------------------------------------

/// A folder's name, whether it's there, and its git branch.
#[tauri::command]
pub fn agent_folder_info(path: String) -> agent::FolderInfo {
    agent::folder_info(std::path::Path::new(&path))
}

/// A session's folder, as the checkpoint store knows it.
fn session_folder(core: &Core, chat_id: &str) -> Result<std::path::PathBuf, String> {
    let chat = core.db.chat_summary(chat_id)?.ok_or("That session no longer exists")?;
    let folder = chat.folder.ok_or("This session has no folder yet")?;
    Ok(agent::paths::real(std::path::Path::new(&folder)))
}

/// Runs checkpoint work off the main thread.
async fn blocking<T: Send + 'static>(core: &Arc<Core>, f: impl FnOnce(&Core) -> Result<T, String> + Send + 'static) -> Result<T, String> {
    let core = core.clone();
    tokio::task::spawn_blocking(move || f(&core)).await.map_err(|e| e.to_string())?
}

fn not_while_working(core: &Core, chat_id: &str) -> Result<(), String> {
    if core.streams.active_chats().iter().any(|c| c == chat_id) {
        return Err("Wait for the reply to finish (or stop it) first".into());
    }
    Ok(())
}

/// Makes the session's folder match a checkpoint (undoable).
#[tauri::command]
pub async fn agent_restore(core: CoreState<'_>, chat_id: String, checkpoint: i64) -> Result<agent::checkpoints::Restored, String> {
    not_while_working(&core, &chat_id)?;
    blocking(core.inner(), move |core| {
        let folder = session_folder(core, &chat_id)?;
        agent::checkpoints::restore(&agent::checkpoints::Store::of(core), &chat_id, &folder, checkpoint)
    })
    .await
}

/// The user's answers to the model's questions (agent__ask_user): a JSON
/// list, one entry per question (`{picked: [labels], other: text}`), or null
/// when they closed the questions.
#[tauri::command]
pub fn tool_answer(core: CoreState, message_id: String, call_id: String, answers: Value) -> Result<(), String> {
    let text = if answers.is_array() { answers.to_string() } else { "null".into() };
    if core.streams.decide(&message_id, &call_id, &text) {
        Ok(())
    } else {
        Err("Those questions aren't waiting anymore".into())
    }
}

/// A session's background processes.
#[tauri::command]
pub fn agent_processes(chat_id: String) -> Vec<agent::shell::ProcessView> {
    agent::shell::Processes::global().list(&chat_id)
}

#[tauri::command]
pub fn agent_process_stop(chat_id: String, id: u32) -> bool {
    agent::shell::Processes::global().stop(&chat_id, id)
}

/// A session's checkpoints (which steps can still be restored).
#[tauri::command]
pub fn agent_checkpoints(core: CoreState, chat_id: String) -> Result<Vec<db::Checkpoint>, String> {
    core.db.checkpoints_of(&chat_id)
}

/// What the session changed in its folder (the files panel).
#[tauri::command]
pub async fn agent_changes(core: CoreState<'_>, chat_id: String) -> Result<agent::checkpoints::Changes, String> {
    blocking(core.inner(), move |core| {
        let folder = session_folder(core, &chat_id)?;
        agent::checkpoints::changes(&agent::checkpoints::Store::of(core), &chat_id, &folder)
    })
    .await
}

/// A file's versions in the checkpoint store.
#[tauri::command]
pub fn agent_file_versions(core: CoreState, chat_id: String, path: String) -> Result<Vec<db::Version>, String> {
    let folder = session_folder(&core, &chat_id)?;
    core.db.file_versions(&agent::checkpoints::folder_id(&folder), &path)
}

/// One version of a file against the file now.
#[tauri::command]
pub async fn agent_version_diff(core: CoreState<'_>, chat_id: String, path: String, hash: String) -> Result<Value, String> {
    blocking(core.inner(), move |core| {
        let folder = session_folder(core, &chat_id)?;
        let st = agent::checkpoints::Store::of(core);
        let old = agent::checkpoints::read_version(&st, &folder, &hash)?;
        let old = agent::files::decode(&old).ok_or("That version isn't text")?;
        let now = std::fs::read(folder.join(&path)).ok().and_then(|b| agent::files::decode(&b)).unwrap_or_default();
        Ok(agent::diff::lines(&old.replace("\r\n", "\n"), &now.replace("\r\n", "\n")))
    })
    .await
}

/// Writes one version of a file back (undoable: returns the checkpoint taken first).
#[tauri::command]
pub async fn agent_restore_file(core: CoreState<'_>, chat_id: String, path: String, hash: String) -> Result<i64, String> {
    not_while_working(&core, &chat_id)?;
    blocking(core.inner(), move |core| {
        let folder = session_folder(core, &chat_id)?;
        agent::checkpoints::restore_file(&agent::checkpoints::Store::of(core), &chat_id, &folder, &path, &hash)
    })
    .await
}

/// Opens a file of the session's folder in its app (a script or program is
/// shown in its folder instead).
#[tauri::command]
pub fn agent_open_file(app: AppHandle, core: CoreState, chat_id: String, path: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let folder = session_folder(&core, &chat_id)?;
    let full = agent::paths::normalize(&folder.join(&path));
    if !agent::paths::within(&full, &folder) {
        return Err("That file isn't in the session's folder".into());
    }
    if !full.exists() {
        return Err("That file no longer exists".into());
    }
    if attachments::runs_when_opened(&path) {
        return app.opener().reveal_item_in_dir(&full).map_err(|e| e.to_string());
    }
    app.opener().open_path(full.to_string_lossy(), None::<&str>).map_err(|e| e.to_string())
}

/// Reads a session's AGENTS.md / CLAUDE.md again (they're read once when it
/// starts); returns how long the text is (0: no file).
#[tauri::command]
pub fn agent_reload_instructions(core: CoreState, chat_id: String) -> Result<usize, String> {
    let chat = core.db.chat_summary(&chat_id)?.ok_or("That session no longer exists")?;
    let folder = chat.folder.ok_or("This session has no folder yet")?;
    let text = agent::read_instructions(std::path::Path::new(&folder));
    core.db.set_instructions(&chat_id, &text)?;
    Ok(text.len())
}

/// Opens a session's folder in Explorer.
#[tauri::command]
pub fn agent_open_folder(app: AppHandle, core: CoreState, chat_id: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let chat = core.db.chat_summary(&chat_id)?.ok_or("That session no longer exists")?;
    let folder = chat.folder.ok_or("This session has no folder yet")?;
    if !std::path::Path::new(&folder).is_dir() {
        return Err("That folder no longer exists".into());
    }
    app.opener().open_path(folder, None::<&str>).map_err(|e| e.to_string())
}

/// The conversation on screen as Markdown.
#[tauri::command]
pub fn chat_markdown(core: CoreState, id: String) -> Result<String, String> {
    let chat = core.db.chat(&id)?.ok_or("That chat no longer exists")?;
    let title = if chat.summary.title.is_empty() { "Chat" } else { chat.summary.title.as_str() };
    let mut out = format!("# {title}\n\n");
    for m in &chat.messages {
        let who = if m.role == "user" { "You".to_string() } else { m.model.clone().unwrap_or_else(|| "Assistant".into()) };
        out.push_str(&format!("**{who}**\n\n"));
        if !m.attachments.is_empty() {
            out.push_str(&format!("*Attached: {}*\n\n", attachments::describe(&m.attachments)));
        }
        if !m.content.trim().is_empty() {
            out.push_str(&format!("{}\n\n", m.content.trim()));
        }
    }
    Ok(out)
}

#[tauri::command]
pub fn usage_stats(core: CoreState, days: Option<i64>) -> Result<Usage, String> {
    core.db.usage(days.map(|d| d.clamp(1, 36_500)))
}

// ---- holster & window ------------------------------------------------------------------

#[tauri::command]
pub fn holster_status() -> holster::HolsterStatus {
    holster::status()
}

#[tauri::command]
pub fn holster_seen(chat_id: String) {
    holster::seen(&chat_id);
}

/// The close button: to the tray, or quit (Settings, General).
#[tauri::command]
pub fn window_close(app: AppHandle, core: CoreState) {
    if core.prefs.get().close_action == "quit" {
        crate::quit(&app);
    } else {
        crate::window::hide(&app);
    }
}

#[tauri::command]
pub fn app_quit(app: AppHandle) {
    crate::quit(&app);
}

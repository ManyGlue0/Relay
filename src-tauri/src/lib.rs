mod anthropic;
mod attachments;
mod chat;
mod commands;
mod db;
mod holster;
mod llm;
mod mcp;
mod secrets;
mod settings;
mod skills;
mod taskbar;
mod update;
mod web;
mod window;

use std::path::PathBuf;
use std::sync::Arc;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};
use tauri_plugin_autostart::ManagerExt as _;

use crate::settings::{Prefs, Settings};

pub struct Core {
    pub prefs: Arc<Prefs>,
    pub db: db::Db,
    pub streams: chat::Streams,
    pub mcp: Arc<mcp::Mcp>,
    pub data_dir: PathBuf,
}

/// Side effects of a settings change.
pub fn apply_settings_change(app: &AppHandle, before: &Settings, after: &Settings) {
    if before.theme != after.theme {
        window::apply_theme(app, &after.theme);
    }
    if before.launch_at_startup != after.launch_at_startup {
        sync_autostart(app, after.launch_at_startup);
    }
}

fn sync_autostart(app: &AppHandle, enabled: bool) {
    // Dev builds would register target/debug; only touch autostart in release.
    if cfg!(debug_assertions) {
        return;
    }
    let al = app.autolaunch();
    let _ = if enabled { al.enable() } else { al.disable() };
}

/// Quits for real (closing the window only hides it, unless the user chose otherwise).
pub fn quit(app: &AppHandle) {
    window::save_bounds(app);
    if let Some(core) = app.try_state::<Arc<Core>>() {
        core.mcp.shutdown();
    }
    app.exit(0);
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Relay", true, None::<&str>)?;
    let new_chat = MenuItem::with_id(app, "new", "New chat", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit Relay", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &new_chat, &settings, &sep, &quit_item])?;
    TrayIconBuilder::with_id("relay")
        .icon(app.default_window_icon().cloned().expect("app icon"))
        .tooltip("Relay")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => window::show(app),
            "new" => {
                window::show(app);
                let _ = app.emit("navigate", "new");
            }
            "settings" => {
                window::show(app);
                let _ = app.emit("navigate", "settings");
            }
            "quit" => quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                window::toggle(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// `relay.exe --pdf-text <pdf> <out>`: reads a PDF's text in its own process (see attachments.rs).
pub fn pdf_text_main(pdf: &str, out: &str) {
    attachments::pdf_text_main(pdf, out);
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| window::show(app)))
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec!["--autostart"])))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(update::Pending::default())
        // The microphone is for voice messages: Relay's own page asks only when
        // the mic button is pressed (Windows' privacy setting still applies).
        .on_permission_request(|_, kind| match kind {
            tauri::webview::PermissionKind::Microphone => tauri::webview::PermissionResponse::Allow,
            _ => tauri::webview::PermissionResponse::Default,
        })
        // Attachments, for the UI: http://relay.localhost/<id>/<file>.
        .register_asynchronous_uri_scheme_protocol("relay", |ctx, request, responder| {
            let root = ctx.app_handle().path().app_data_dir().map(|d| attachments::root(&d));
            std::thread::spawn(move || {
                let range = request.headers().get("range").and_then(|v| v.to_str().ok()).map(str::to_string);
                let response = match root {
                    Ok(root) => attachments::serve(&root, request.uri().path(), range.as_deref()),
                    Err(_) => tauri::http::Response::builder().status(404).body(Vec::new()).unwrap(),
                };
                responder.respond(response);
            });
        })
        .setup(|app| {
            let handle = app.handle().clone();
            // Release builds carry the updater's public key; local builds don't update.
            if update::enabled(&handle) {
                handle.plugin(tauri_plugin_updater::Builder::new().build())?;
            }
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;

            let prefs = Arc::new(Prefs::load(&data_dir));
            let settings = prefs.get();
            let db = db::Db::open(&data_dir.join("relay.db")).map_err(|e| format!("Couldn't open the chat database: {e}"))?;
            let core = Arc::new(Core {
                prefs: prefs.clone(),
                db,
                streams: chat::Streams::default(),
                mcp: mcp::Mcp::new(handle.clone()),
                data_dir: data_dir.clone(),
            });
            app.manage(core.clone());
            // Files no message holds any more (and not just attached in the composer).
            let gc = core.clone();
            std::thread::spawn(move || {
                if let Ok(keep) = gc.db.attachment_ids(None) {
                    attachments::collect(&attachments::root(&gc.data_dir), &keep, std::time::Duration::from_secs(24 * 3600));
                }
            });
            for server in core.db.mcp_servers().unwrap_or_default() {
                core.mcp.connect(server);
            }

            window::install(&handle, &prefs);
            let main = app.get_webview_window("main").expect("main window");
            let events_app = handle.clone();
            let events_window = main.clone();
            main.on_window_event(move |event| match event {
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let quit_on_close = events_app.try_state::<Arc<Core>>().is_some_and(|c| c.prefs.get().close_action == "quit");
                    if quit_on_close {
                        quit(&events_app);
                    } else {
                        window::hide(&events_app);
                    }
                }
                WindowEvent::Focused(on) => window::set_focused(*on),
                WindowEvent::ThemeChanged(_) => window::apply_material(&events_window),
                _ => {}
            });

            build_tray(&handle)?;
            // Relay Dev (tauri.dev.json) runs beside the installed Relay and
            // never takes over its Holster tile.
            let side_by_side = app.config().identifier.ends_with(".dev");
            holster::start(handle.clone(), &data_dir, !side_by_side);
            sync_autostart(&handle, settings.launch_at_startup);
            // At sign-in Relay waits in the tray; otherwise it opens.
            if !std::env::args().any(|a| a == "--autostart") {
                window::show(&handle);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::update_check,
            commands::update_install,
            commands::providers_probe,
            commands::icon_from_file,
            commands::get_settings,
            commands::update_settings,
            commands::system_prompt_preview,
            commands::providers_list,
            commands::provider_save,
            commands::provider_delete,
            commands::provider_test,
            commands::provider_refresh,
            commands::model_context,
            commands::model_caps,
            commands::model_caps_set,
            commands::attach_path,
            commands::attach_bytes,
            commands::attach_put,
            commands::attach_info,
            commands::attach_discard,
            commands::attach_open,
            commands::presets_list,
            commands::preset_save,
            commands::preset_delete,
            commands::mcp_list,
            commands::mcp_save,
            commands::mcp_env,
            commands::mcp_delete,
            commands::mcp_restart,
            commands::mcp_set_enabled,
            commands::mcp_set_policy,
            commands::mcp_import,
            commands::tool_decide,
            commands::skills_list,
            commands::skills_info,
            commands::skill_save,
            commands::skill_delete,
            commands::skill_set,
            commands::skill_import,
            commands::skills_open_folder,
            commands::chats_list,
            commands::chat_get,
            commands::chat_rename,
            commands::chat_pin,
            commands::chat_archive,
            commands::chat_delete,
            commands::chats_delete_all,
            commands::chat_set_model,
            commands::chat_set_options,
            commands::chat_send,
            commands::chat_edit,
            commands::chat_regenerate,
            commands::chat_branch,
            commands::chat_stop,
            commands::streaming_chats,
            commands::chat_search,
            commands::chat_markdown,
            commands::chat_export,
            commands::usage_stats,
            commands::holster_status,
            commands::holster_seen,
            commands::window_close,
            commands::app_quit,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Relay")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                if let Some(core) = app.try_state::<Arc<Core>>() {
                    core.mcp.shutdown();
                }
            }
        });
}

//! Personal Analyst desktop backend (Tauri 2). The commands in [`commands`] are thin wrappers over
//! `analyst_core::service::Workbench`; session events reach the renderer through
//! `tauri::ipc::Channel<SessionEvent>` subscriptions opened with `session_subscribe`.

pub mod commands;

use analyst_core::service::{Workbench, WorkbenchOptions};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::Manager;

/// One `session_subscribe` channel: the forwarding task and the window that opened it.
pub struct Subscription {
    pub window: String,
    pub session: String,
    pub handle: tokio::task::JoinHandle<()>,
}

/// Managed Tauri state.
pub struct AppState {
    pub workbench: Arc<Workbench>,
    pub subscriptions: Mutex<HashMap<String, Subscription>>,
    /// Folders already added to the asset protocol scope (deck caches, assets, references).
    pub asset_dirs: Mutex<std::collections::HashSet<std::path::PathBuf>>,
}

impl AppState {
    pub fn new(workbench: Arc<Workbench>) -> Self {
        AppState {
            workbench,
            subscriptions: Mutex::new(HashMap::new()),
            asset_dirs: Mutex::new(Default::default()),
        }
    }

    /// Drop every subscription a window opened (window closed).
    pub fn drop_window_subscriptions(&self, window: &str) {
        if let Ok(mut subs) = self.subscriptions.lock() {
            let ids: Vec<String> = subs
                .iter()
                .filter(|(_, s)| s.window == window)
                .map(|(id, _)| id.clone())
                .collect();
            for id in ids {
                if let Some(s) = subs.remove(&id) {
                    s.handle.abort();
                }
            }
        }
    }
}

/// Build the workbench from `$ANALYST_HOME` (default `~/.analyst`) and the environment:
/// `ANALYST_SANDBOX` overrides the kernel sandbox kind.
pub fn workbench_from_env() -> anyhow::Result<Arc<Workbench>> {
    let home = analyst_core::config::analyst_home();
    // First start: bring the deck workbench's old home (`~/.present`) over once.
    match analyst_core::migrate::migrate_legacy_home(&home) {
        Ok(Some(r)) => tracing::info!(
            "migrated {} into {}: {} provider(s), {} file(s), {} key(s)",
            r.from.display(),
            home.display(),
            r.providers.len(),
            r.files.len(),
            r.keys.len()
        ),
        Ok(None) => {}
        Err(e) => tracing::warn!("migrating ~/.present failed: {e:#}"),
    }
    let mut opts = WorkbenchOptions::new(home);
    if let Ok(s) = std::env::var("ANALYST_SANDBOX") {
        if !s.is_empty() {
            opts.sandbox = Some(s);
        }
    }
    let wb = Workbench::new(opts).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(Arc::new(wb))
}

/// The command names registered in `generate_handler!`, kept in one place so a test can check
/// them against docs/IPC.md sections 4, 7, and 9.
pub const COMMANDS: &[&str] = &[
    "app_info",
    "sessions_list",
    "session_create",
    "session_open",
    "session_close",
    "session_info",
    "session_rename",
    "session_subscribe",
    "session_unsubscribe",
    "session_set_target",
    "session_set_policy",
    "layout_get",
    "layout_save",
    "fs_tree",
    "fs_read",
    "fs_read_binary",
    "fs_write",
    "fs_create",
    "fs_delete",
    "fs_rename",
    "fs_search",
    "index_get",
    "check_run",
    "run_start",
    "run_cancel",
    "runs_list",
    "run_get",
    "out_list",
    "out_read",
    "figures_list",
    "figure_data",
    "eq_extract",
    "eq_check",
    "env_packages",
    "env_add",
    "env_remove",
    "env_sync",
    "conversation_new",
    "conversation_select",
    "conversation_transcript",
    "chat_send",
    "chat_cancel",
    "chat_set_mode",
    "chat_set_model",
    "proposal_respond",
    "models_list",
    "model_probe",
    "provider_keys_status",
    "provider_key_set",
    "targets_list",
    "target_bootstrap",
    "terminal_open",
    "terminal_write",
    "terminal_resize",
    "terminal_close",
    "dialog_pick_directory",
    "dialog_pick_files",
    "decks_list",
    "deck_create",
    "deck_open",
    "template_thumbnails",
    "template_replace",
    "deck_get",
    "slide_update",
    "slide_create",
    "slide_delete",
    "slide_duplicate",
    "slide_move",
    "slide_lock",
    "slide_place_figure",
    "slide_place_equation",
    "slide_preview_png",
    "chart_compute",
    "deck_code_read",
    "deck_code_write",
    "deck_build",
    "deck_switch_mode",
    "render_fidelity",
    "deck_check_run",
    "refs_list",
    "refs_add",
    "refs_remove",
    "refs_remove_unused",
    "refs_search",
    "source_passage",
    "assets_list",
    "asset_add",
    "exports_list",
    "export_run",
    "export_reveal",
    "export_open",
    "history_list",
    "history_restore",
    "history_revert_turn",
    "artifacts_list",
    "verification_list",
    "verification_set",
    "deck_verification_gaps",
];

/// Where the kernel source bundled with the app lives: `<resource dir>/kernel` (see
/// `bundle.resources` in `tauri.conf.json`), when it is there.
fn bundled_kernel_source(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
    let dir = app.path().resource_dir().ok()?.join("kernel");
    dir.join("pyproject.toml").is_file().then_some(dir)
}

/// The macOS application menu. The default menu binds Cmd+W to "Close Window", which would
/// close the only window instead of the active editor tab; this one has no such item, so the
/// renderer's close-tab shortcut receives the key. The standard Edit items stay, because on
/// macOS copy, paste, undo, and select-all reach the webview through them.
pub fn build_app_menu<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> tauri::Result<tauri::menu::Menu<R>> {
    use tauri::menu::{Menu, PredefinedMenuItem, Submenu};
    let app_menu = Submenu::with_items(
        app,
        "Personal Analyst",
        true,
        &[
            &PredefinedMenuItem::about(app, None, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, None)?,
        ],
    )?;
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    // No `close_window` here on purpose.
    let window = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::fullscreen(app, None)?,
        ],
    )?;
    Menu::with_items(app, &[&app_menu, &edit, &window])
}

/// Configure and run the app.
pub fn run(workbench: Arc<Workbench>) -> anyhow::Result<()> {
    use commands::*;
    let state = AppState::new(workbench.clone());
    let shutdown_wb = workbench.clone();
    let setup_wb = workbench.clone();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .setup(move |app| {
            if cfg!(target_os = "macos") {
                let menu = build_app_menu(app.handle())?;
                app.set_menu(menu)?;
            }
            // Packaging (B12): outside a development checkout the kernel is installed from
            // the bundled source into `$ANALYST_HOME/tooling`, in the background. Kernels
            // wait for it; a missing `uv` becomes a clear kernel error state.
            match bundled_kernel_source(app.handle()) {
                Some(dir) => {
                    tracing::info!("bundled kernel source: {}", dir.display());
                    setup_wb.set_kernel_source(dir);
                }
                None => tracing::info!("no bundled kernel source next to the app"),
            }
            if let Some(task) = setup_wb.prepare_tooling() {
                tracing::info!("preparing the tooling environment ({task})");
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                let label = window.label().to_string();
                if let Some(state) = window.app_handle().try_state::<AppState>() {
                    state.drop_window_subscriptions(&label);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            sessions_list,
            session_create,
            session_open,
            session_close,
            session_info,
            session_rename,
            session_subscribe,
            session_unsubscribe,
            session_set_target,
            session_set_policy,
            layout_get,
            layout_save,
            fs_tree,
            fs_read,
            fs_read_binary,
            fs_write,
            fs_create,
            fs_delete,
            fs_rename,
            fs_search,
            index_get,
            check_run,
            run_start,
            run_cancel,
            runs_list,
            run_get,
            out_list,
            out_read,
            figures_list,
            figure_data,
            eq_extract,
            eq_check,
            env_packages,
            env_add,
            env_remove,
            env_sync,
            conversation_new,
            conversation_select,
            conversation_transcript,
            chat_send,
            chat_cancel,
            chat_set_mode,
            chat_set_model,
            proposal_respond,
            models_list,
            model_probe,
            provider_keys_status,
            provider_key_set,
            targets_list,
            target_bootstrap,
            terminal_open,
            terminal_write,
            terminal_resize,
            terminal_close,
            dialog_pick_directory,
            dialog_pick_files,
            decks_list,
            deck_create,
            deck_open,
            template_thumbnails,
            template_replace,
            deck_get,
            slide_update,
            slide_create,
            slide_delete,
            slide_duplicate,
            slide_move,
            slide_lock,
            slide_place_figure,
            slide_place_equation,
            slide_preview_png,
            chart_compute,
            deck_code_read,
            deck_code_write,
            deck_build,
            deck_switch_mode,
            render_fidelity,
            deck_check_run,
            refs_list,
            refs_add,
            refs_remove,
            refs_remove_unused,
            refs_search,
            source_passage,
            assets_list,
            asset_add,
            exports_list,
            export_run,
            export_reveal,
            export_open,
            history_list,
            history_restore,
            history_revert_turn,
            artifacts_list,
            verification_list,
            verification_set,
            deck_verification_gaps,
        ])
        .build(tauri::generate_context!())
        .map_err(|e| anyhow::anyhow!("building the Tauri app: {e}"))?
        .run(move |_app, event| {
            if let tauri::RunEvent::Exit = event {
                let wb = shutdown_wb.clone();
                tauri::async_runtime::block_on(async move { wb.shutdown().await });
            }
        });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::COMMANDS;

    /// Copied from docs/IPC.md sections 4, 7, and 9 (the `command` columns).
    const IPC_MD_COMMANDS: &[&str] = &[
        "app_info",
        "sessions_list",
        "session_create",
        "session_open",
        "session_close",
        "session_info",
        "session_rename",
        "session_subscribe",
        "session_unsubscribe",
        "session_set_target",
        "session_set_policy",
        "layout_get",
        "layout_save",
        "fs_tree",
        "fs_read",
        "fs_read_binary",
        "fs_write",
        "fs_create",
        "fs_delete",
        "fs_rename",
        "fs_search",
        "index_get",
        "check_run",
        "run_start",
        "run_cancel",
        "runs_list",
        "run_get",
        "out_list",
        "out_read",
        "figures_list",
        "figure_data",
        "eq_extract",
        "eq_check",
        "env_packages",
        "env_add",
        "env_remove",
        "env_sync",
        "conversation_new",
        "conversation_select",
        "conversation_transcript",
        "chat_send",
        "chat_cancel",
        "chat_set_mode",
        "chat_set_model",
        "proposal_respond",
        "models_list",
        "model_probe",
        "provider_keys_status",
        "provider_key_set",
        "targets_list",
        "target_bootstrap",
        "terminal_open",
        "terminal_write",
        "terminal_resize",
        "terminal_close",
        "dialog_pick_directory",
        "dialog_pick_files",
        "decks_list",
        "deck_create",
        "deck_open",
        "template_thumbnails",
        "template_replace",
        "deck_get",
        "slide_update",
        "slide_create",
        "slide_delete",
        "slide_duplicate",
        "slide_move",
        "slide_lock",
        "slide_place_figure",
        "slide_place_equation",
        "slide_preview_png",
        "chart_compute",
        "deck_code_read",
        "deck_code_write",
        "deck_build",
        "deck_switch_mode",
        "render_fidelity",
        "deck_check_run",
        "refs_list",
        "refs_add",
        "refs_remove",
        "refs_remove_unused",
        "refs_search",
        "source_passage",
        "assets_list",
        "asset_add",
        "exports_list",
        "export_run",
        "export_reveal",
        "export_open",
        "history_list",
        "history_restore",
        "history_revert_turn",
        "artifacts_list",
        "verification_list",
        "verification_set",
        "deck_verification_gaps",
    ];

    #[test]
    fn every_ipc_md_command_is_registered() {
        for c in IPC_MD_COMMANDS {
            assert!(
                COMMANDS.contains(c),
                "command `{c}` from IPC.md is not registered"
            );
        }
        for c in COMMANDS {
            assert!(
                IPC_MD_COMMANDS.contains(c),
                "command `{c}` is not in IPC.md"
            );
        }
        assert_eq!(COMMANDS.len(), IPC_MD_COMMANDS.len());
    }

    /// The registered list must match the source of `generate_handler!` in this file.
    #[test]
    fn generate_handler_matches_command_list() {
        let src = include_str!("lib.rs");
        let start = src.find("generate_handler![").expect("handler list");
        let end = src[start..].find("])").expect("handler list end") + start;
        let body = &src[start + "generate_handler![".len()..end];
        let names: Vec<&str> = body
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(names, COMMANDS);
    }
}

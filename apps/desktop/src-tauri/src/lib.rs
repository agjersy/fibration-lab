//! Personal Present desktop backend (Tauri 2). The commands in [`commands`] are thin wrappers
//! over `present_core::service`; project events reach the renderer through a
//! `tauri::ipc::Channel<ProjectEvent>` opened with `project_subscribe`.

pub mod commands;

use present_core::service::Workbench;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::Manager;

/// One `project_subscribe` channel: the forwarding task and the window that opened it.
pub struct Subscription {
    pub window: String,
    pub project: String,
    pub handle: tokio::task::JoinHandle<()>,
}

/// Managed Tauri state.
pub struct AppState {
    pub workbench: Arc<Workbench>,
    pub subscriptions: Mutex<HashMap<u64, Subscription>>,
    pub next_subscription: std::sync::atomic::AtomicU64,
}

impl AppState {
    pub fn new(workbench: Arc<Workbench>) -> Self {
        AppState {
            workbench,
            subscriptions: Mutex::new(HashMap::new()),
            next_subscription: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// End the subscriptions matching `pick` (a closed window or a closed project).
    pub fn drop_subscriptions(&self, pick: impl Fn(&Subscription) -> bool) {
        if let Ok(mut subs) = self.subscriptions.lock() {
            let ids: Vec<u64> = subs
                .iter()
                .filter(|(_, s)| pick(s))
                .map(|(id, _)| *id)
                .collect();
            for id in ids {
                if let Some(s) = subs.remove(&id) {
                    s.handle.abort();
                }
            }
        }
    }
}

/// Build the workbench from `PRESENT_HOME` (default `~/.present`, `%APPDATA%\Present` on
/// Windows). `PRESENT_ENGINE`, `PRESENT_SANDBOX`, and `PRESENT_PYTHON` override the rendering
/// engine, the sandbox kind, and the kernel's interpreter.
pub fn workbench_from_env() -> Arc<Workbench> {
    let home = present_core::config::default_home();
    let workbench = Workbench::new(&home);
    {
        let mut options = workbench.options.lock().unwrap_or_else(|e| e.into_inner());
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        if let Some(engine) = var("PRESENT_ENGINE") {
            options.engine = engine;
        }
        if let Some(sandbox) = var("PRESENT_SANDBOX") {
            options.sandbox = sandbox;
        }
        if let Some(python) = var("PRESENT_PYTHON") {
            options.runtime_python = Some(python);
        }
    }
    Arc::new(workbench)
}

/// The command names registered in `generate_handler!`, kept in one place so a test can check
/// them against docs/IPC.md section 4.
pub const COMMANDS: &[&str] = &[
    "app_info",
    "projects_list",
    "project_create",
    "project_open",
    "project_close",
    "project_subscribe",
    "project_set_policy",
    "project_layout_get",
    "project_layout_set",
    "template_thumbnails",
    "template_replace",
    "deck_get",
    "slide_update",
    "slide_create",
    "slide_delete",
    "slide_duplicate",
    "slide_move",
    "slide_lock",
    "slide_preview_png",
    "chart_compute",
    "code_read",
    "code_write",
    "deck_build",
    "deck_switch_mode",
    "render_fidelity",
    "check_run",
    "refs_list",
    "refs_add",
    "refs_remove",
    "refs_remove_unused",
    "refs_search",
    "source_passage",
    "analyst_scan",
    "analyst_import",
    "assets_list",
    "asset_add",
    "history_list",
    "history_restore",
    "history_revert_turn",
    "exports_list",
    "export_run",
    "export_reveal",
    "export_open",
    "conversation_new",
    "conversation_select",
    "conversation_transcript",
    "chat_send",
    "chat_cancel",
    "chat_set_mode",
    "chat_set_model",
    "proposal_respond",
    "skills_list",
    "skill_set_enabled",
    "models_list",
    "model_probe",
    "provider_set_key",
    "audit_export",
    "dialog_pick_file",
    "dialog_pick_files",
    "dialog_pick_directory",
    "dialog_save_file",
];

/// Configure and run the app.
pub fn run(workbench: Arc<Workbench>) -> anyhow::Result<()> {
    use commands::*;
    let state = AppState::new(workbench.clone());
    let shutdown_wb = workbench.clone();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                let label = window.label().to_string();
                if let Some(state) = window.app_handle().try_state::<AppState>() {
                    state.drop_subscriptions(|s| s.window == label);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            projects_list,
            project_create,
            project_open,
            project_close,
            project_subscribe,
            project_set_policy,
            project_layout_get,
            project_layout_set,
            template_thumbnails,
            template_replace,
            deck_get,
            slide_update,
            slide_create,
            slide_delete,
            slide_duplicate,
            slide_move,
            slide_lock,
            slide_preview_png,
            chart_compute,
            code_read,
            code_write,
            deck_build,
            deck_switch_mode,
            render_fidelity,
            check_run,
            refs_list,
            refs_add,
            refs_remove,
            refs_remove_unused,
            refs_search,
            source_passage,
            analyst_scan,
            analyst_import,
            assets_list,
            asset_add,
            history_list,
            history_restore,
            history_revert_turn,
            exports_list,
            export_run,
            export_reveal,
            export_open,
            conversation_new,
            conversation_select,
            conversation_transcript,
            chat_send,
            chat_cancel,
            chat_set_mode,
            chat_set_model,
            proposal_respond,
            skills_list,
            skill_set_enabled,
            models_list,
            model_probe,
            provider_set_key,
            audit_export,
            dialog_pick_file,
            dialog_pick_files,
            dialog_pick_directory,
            dialog_save_file,
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
    use std::collections::BTreeSet;

    /// The `command` column of docs/IPC.md section 4, read from the document itself.
    fn documented() -> BTreeSet<String> {
        let doc = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../docs/IPC.md"),
        )
        .expect("docs/IPC.md");
        let section = doc
            .split("\n## ")
            .find(|s| s.starts_with("4."))
            .expect("IPC.md section 4");
        let mut names = BTreeSet::new();
        for line in section.lines().filter(|l| l.starts_with("| `")) {
            let cell = line.split('|').nth(1).unwrap_or_default();
            let mut previous = String::new();
            for part in cell.split('`').skip(1).step_by(2) {
                // `project_layout_get` / `_set` is shorthand for a shared prefix.
                let name = match part.strip_prefix('_') {
                    Some(suffix) => {
                        let stem = previous.rsplit_once('_').map_or("", |(a, _)| a);
                        format!("{stem}_{suffix}")
                    }
                    None => part.to_string(),
                };
                previous = name.clone();
                names.insert(name);
            }
        }
        names
    }

    #[test]
    fn every_documented_command_is_registered_and_nothing_else() {
        let registered: BTreeSet<String> = COMMANDS.iter().map(|c| c.to_string()).collect();
        assert_eq!(registered.len(), COMMANDS.len(), "duplicate command names");
        assert_eq!(registered, documented());
    }

    #[test]
    fn the_handler_list_matches_the_names() {
        let source = include_str!("lib.rs");
        let handler = source
            .split("generate_handler![")
            .nth(1)
            .and_then(|s| s.split(']').next())
            .expect("handler list");
        let listed: Vec<&str> = handler
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(listed, COMMANDS);
    }

    #[test]
    fn every_command_has_a_function() {
        let source = include_str!("commands.rs");
        for name in COMMANDS {
            assert!(
                source.contains(&format!("pub async fn {name}(")),
                "commands.rs has no `{name}`"
            );
        }
    }
}

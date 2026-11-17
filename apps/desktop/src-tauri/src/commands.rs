//! One `#[tauri::command]` per row of docs/IPC.md section 4, with exactly those names and
//! argument names (`rename_all = "snake_case"` keeps the JS-side keys as written in the doc).
//! Every error is serialized as `IpcError`.

use crate::{AppState, Subscription};
use present_core::service::ProjectRuntime;
use present_protocol::deck::{DeckMode, Slide};
use present_protocol::kernel::LayoutThumbnail;
use present_protocol::ui::*;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::ipc::Channel;
use tauri::{Manager, State};
use tauri_plugin_dialog::{DialogExt, FilePath};

type R<T> = Result<T, IpcError>;

fn rt_of(state: &State<'_, AppState>, id: &str) -> R<Arc<ProjectRuntime>> {
    state.workbench.project(id)
}

/// Let the webview load files of an open project (thumbnails, page images, assets).
fn allow_project_files(app: &tauri::AppHandle, root: &str) {
    if let Err(e) = app.asset_protocol_scope().allow_directory(root, true) {
        tracing::warn!("cannot expose {root} to the window: {e}");
    }
}

// ---- app and projects ----

#[tauri::command(rename_all = "snake_case")]
pub async fn app_info(state: State<'_, AppState>) -> R<AppInfo> {
    Ok(state.workbench.app_info())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn projects_list(state: State<'_, AppState>) -> R<Vec<ProjectSummary>> {
    Ok(state.workbench.projects_list())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn project_create(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    path_abs: String,
    name: String,
    template_abs: String,
    deck_mode: Option<DeckMode>,
) -> R<ProjectInfo> {
    let info = state
        .workbench
        .project_create(&path_abs, &name, &template_abs, deck_mode)
        .await?;
    allow_project_files(&app, &info.summary.path_abs);
    Ok(info)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn project_open(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    path_abs: String,
) -> R<ProjectInfo> {
    let info = state.workbench.project_open(&path_abs).await?;
    allow_project_files(&app, &info.summary.path_abs);
    Ok(info)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn project_close(state: State<'_, AppState>, project: String) -> R<()> {
    state.drop_subscriptions(|s| s.project == project);
    state.workbench.project_close(&project).await
}

/// Opens the project's event channel for this window; it lives until the project or the
/// window closes. Subscribing again from the same window replaces the earlier channel.
#[tauri::command(rename_all = "snake_case")]
pub async fn project_subscribe(
    state: State<'_, AppState>,
    window: tauri::Window,
    project: String,
    channel: Channel<ProjectEvent>,
) -> R<()> {
    let rt = rt_of(&state, &project)?;
    let label = window.label().to_string();
    state.drop_subscriptions(|s| s.project == project && s.window == label);
    let mut rx = rt.subscribe();
    let name = project.clone();
    let handle = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if channel.send(event).is_err() {
                        tracing::debug!("events of {name}: channel closed");
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("events of {name}: dropped {n}");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    let id = state
        .next_subscription
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if let Ok(mut subs) = state.subscriptions.lock() {
        subs.insert(
            id,
            Subscription {
                window: label,
                project,
                handle,
            },
        );
    }
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn project_set_policy(
    state: State<'_, AppState>,
    project: String,
    patch: PolicyPatch,
    persist: Option<bool>,
) -> R<PolicyView> {
    let rt = rt_of(&state, &project)?;
    if persist == Some(true) {
        rt.persist_policy(&patch)?;
    }
    Ok(rt.set_policy(patch))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn project_layout_get(state: State<'_, AppState>, project: String) -> R<Option<Value>> {
    Ok(rt_of(&state, &project)?.layout_get())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn project_layout_set(
    state: State<'_, AppState>,
    project: String,
    layout: Value,
) -> R<()> {
    rt_of(&state, &project)?.layout_set(layout);
    Ok(())
}

// ---- template ----

#[tauri::command(rename_all = "snake_case")]
pub async fn template_thumbnails(
    state: State<'_, AppState>,
    project: String,
) -> R<Vec<LayoutThumbnail>> {
    rt_of(&state, &project)?.template_thumbnails().await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn template_replace(
    state: State<'_, AppState>,
    project: String,
    template_abs: String,
) -> R<String> {
    Ok(rt_of(&state, &project)?.template_replace(template_abs))
}

// ---- deck and slides ----

#[tauri::command(rename_all = "snake_case")]
pub async fn deck_get(state: State<'_, AppState>, project: String) -> R<DeckView> {
    rt_of(&state, &project)?.deck_get()
}

#[tauri::command(rename_all = "snake_case")]
pub async fn slide_update(
    state: State<'_, AppState>,
    project: String,
    slide: Slide,
    expected_hash: Option<String>,
) -> R<SlideView> {
    rt_of(&state, &project)?.slide_update(slide, expected_hash.as_deref())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn slide_create(
    state: State<'_, AppState>,
    project: String,
    after: Option<String>,
    layout: String,
) -> R<SlideView> {
    rt_of(&state, &project)?
        .slide_create(after.as_deref(), &layout)
        .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn slide_delete(state: State<'_, AppState>, project: String, slide_id: String) -> R<()> {
    rt_of(&state, &project)?.slide_delete(&slide_id)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn slide_duplicate(
    state: State<'_, AppState>,
    project: String,
    slide_id: String,
) -> R<SlideView> {
    rt_of(&state, &project)?.slide_duplicate(&slide_id)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn slide_move(
    state: State<'_, AppState>,
    project: String,
    slide_id: String,
    after: Option<String>,
    section: Option<String>,
) -> R<()> {
    rt_of(&state, &project)?.slide_move(&slide_id, after.as_deref(), section.as_deref())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn slide_lock(
    state: State<'_, AppState>,
    project: String,
    slide_id: String,
    element_id: Option<String>,
    locked: bool,
) -> R<()> {
    rt_of(&state, &project)?.slide_lock(&slide_id, element_id.as_deref(), locked)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn slide_preview_png(
    state: State<'_, AppState>,
    project: String,
    slide_id: String,
    png_base64: String,
) -> R<()> {
    rt_of(&state, &project)?.slide_preview_png(&slide_id, &png_base64)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn chart_compute(
    state: State<'_, AppState>,
    project: String,
    source: Value,
    steps: Option<Value>,
) -> R<Value> {
    rt_of(&state, &project)?
        .chart_compute(source, steps.unwrap_or(Value::Null))
        .await
}

// ---- code mode ----

#[tauri::command(rename_all = "snake_case")]
pub async fn code_read(
    state: State<'_, AppState>,
    project: String,
    path: String,
) -> R<FileContent> {
    rt_of(&state, &project)?.code_read(&path)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn code_write(
    state: State<'_, AppState>,
    project: String,
    path: String,
    content: String,
    expected_hash: Option<String>,
) -> R<WriteResult> {
    rt_of(&state, &project)?.code_write(&path, &content, expected_hash.as_deref())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn deck_build(state: State<'_, AppState>, project: String) -> R<String> {
    Ok(rt_of(&state, &project)?.deck_build())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn deck_switch_mode(
    state: State<'_, AppState>,
    project: String,
    to: DeckMode,
    via: Option<String>,
) -> R<String> {
    Ok(rt_of(&state, &project)?.deck_switch_mode(to, via))
}

// ---- rendering and checks ----

#[tauri::command(rename_all = "snake_case")]
pub async fn render_fidelity(
    state: State<'_, AppState>,
    project: String,
    slides: Option<Vec<String>>,
) -> R<String> {
    // The engine renders the whole deck in one pass; `slides` only says which thumbnails the
    // window is waiting for.
    let _ = slides;
    let rt = rt_of(&state, &project)?;
    Ok(rt.spawn_task("Rendering slides", |rt| async move {
        let n = rt.render_thumbnails().await?;
        Ok(Some(format!("{n} slides rendered")))
    }))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn check_run(
    state: State<'_, AppState>,
    project: String,
    slides: Option<Vec<String>>,
) -> R<Vec<Value>> {
    rt_of(&state, &project)?.run_checks(slides).await
}

// ---- references ----

#[tauri::command(rename_all = "snake_case")]
pub async fn refs_list(state: State<'_, AppState>, project: String) -> R<Vec<RefView>> {
    rt_of(&state, &project)?.refs_list()
}

#[tauri::command(rename_all = "snake_case")]
pub async fn refs_add(
    state: State<'_, AppState>,
    project: String,
    paths_abs: Vec<String>,
) -> R<String> {
    Ok(rt_of(&state, &project)?.refs_add(paths_abs))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn refs_remove(state: State<'_, AppState>, project: String, ref_id: String) -> R<()> {
    rt_of(&state, &project)?.refs_remove(&ref_id)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn refs_remove_unused(state: State<'_, AppState>, project: String) -> R<Vec<String>> {
    rt_of(&state, &project)?.refs_remove_unused()
}

#[tauri::command(rename_all = "snake_case")]
pub async fn refs_search(
    state: State<'_, AppState>,
    project: String,
    query: String,
    ref_ids: Option<Vec<String>>,
) -> R<Vec<SearchHit>> {
    rt_of(&state, &project)?.refs_search(&query, ref_ids).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn source_passage(
    state: State<'_, AppState>,
    project: String,
    cite: String,
) -> R<SourcePassage> {
    rt_of(&state, &project)?.source_passage(&cite).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn analyst_scan(
    state: State<'_, AppState>,
    project: String,
    repo_abs: String,
) -> R<Vec<AnalystItem>> {
    rt_of(&state, &project)?.analyst_scan(&repo_abs).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn analyst_import(
    state: State<'_, AppState>,
    project: String,
    repo_abs: String,
    item: String,
) -> R<RefView> {
    rt_of(&state, &project)?
        .analyst_import(&repo_abs, &item)
        .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn assets_list(state: State<'_, AppState>, project: String) -> R<Vec<AssetView>> {
    rt_of(&state, &project)?.assets_list()
}

#[tauri::command(rename_all = "snake_case")]
pub async fn asset_add(
    state: State<'_, AppState>,
    project: String,
    path_abs: String,
) -> R<AssetView> {
    rt_of(&state, &project)?.asset_add(&path_abs)
}

// ---- history ----

#[tauri::command(rename_all = "snake_case")]
pub async fn history_list(
    state: State<'_, AppState>,
    project: String,
    limit: Option<usize>,
) -> R<Vec<SnapshotView>> {
    rt_of(&state, &project)?.history_list(limit)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn history_restore(
    state: State<'_, AppState>,
    project: String,
    snapshot_id: String,
) -> R<SnapshotView> {
    rt_of(&state, &project)?.history_restore(&snapshot_id)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn history_revert_turn(
    state: State<'_, AppState>,
    project: String,
    turn: String,
) -> R<SnapshotView> {
    rt_of(&state, &project)?.history_revert_turn(&turn)
}

// ---- exports ----

#[tauri::command(rename_all = "snake_case")]
pub async fn exports_list(state: State<'_, AppState>, project: String) -> R<Vec<ExportView>> {
    Ok(rt_of(&state, &project)?.exports_list())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn export_run(
    state: State<'_, AppState>,
    project: String,
    format: String,
    preset: Option<String>,
    options: Option<Value>,
) -> R<String> {
    // Presets and per-format options arrive with the export dialog (BUILD-SPEC D8).
    let _ = (preset, options);
    Ok(rt_of(&state, &project)?.export_run(format))
}

/// Run a desktop helper program and report a failure to start as an `IpcError`.
fn launch(program: &str, args: &[&std::ffi::OsStr]) -> R<()> {
    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| IpcError::new(IpcErrorCode::Io, format!("cannot start {program}: {e}")))
}

fn reveal_in_file_manager(path: &Path) -> R<()> {
    #[cfg(target_os = "windows")]
    {
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(path);
        launch("explorer.exe", &[arg.as_os_str()])
    }
    #[cfg(target_os = "macos")]
    {
        launch("open", &[std::ffi::OsStr::new("-R"), path.as_os_str()])
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let dir = path.parent().unwrap_or(path);
        launch("xdg-open", &[dir.as_os_str()])
    }
}

fn open_with_default_app(path: &Path) -> R<()> {
    #[cfg(target_os = "windows")]
    {
        // `start` is a shell builtin; the empty string is the window title it expects first.
        launch(
            "cmd.exe",
            &[
                std::ffi::OsStr::new("/C"),
                std::ffi::OsStr::new("start"),
                std::ffi::OsStr::new(""),
                path.as_os_str(),
            ],
        )
    }
    #[cfg(target_os = "macos")]
    {
        launch("open", &[path.as_os_str()])
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        launch("xdg-open", &[path.as_os_str()])
    }
}

#[tauri::command(rename_all = "snake_case")]
pub async fn export_reveal(
    state: State<'_, AppState>,
    project: String,
    export_id: String,
) -> R<()> {
    reveal_in_file_manager(&rt_of(&state, &project)?.export_path(&export_id)?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn export_open(state: State<'_, AppState>, project: String, export_id: String) -> R<()> {
    open_with_default_app(&rt_of(&state, &project)?.export_path(&export_id)?)
}

// ---- conversations ----

#[tauri::command(rename_all = "snake_case")]
pub async fn conversation_new(
    state: State<'_, AppState>,
    project: String,
) -> R<ConversationSummary> {
    Ok(rt_of(&state, &project)?.conversation_new())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn conversation_select(
    state: State<'_, AppState>,
    project: String,
    conversation: String,
) -> R<()> {
    rt_of(&state, &project)?.conversation_select(&conversation)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn conversation_transcript(
    state: State<'_, AppState>,
    project: String,
    conversation: String,
) -> R<Vec<Value>> {
    rt_of(&state, &project)?.conversation_transcript(&conversation)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn chat_send(
    state: State<'_, AppState>,
    project: String,
    conversation: String,
    text: String,
    attachments: Option<ChatAttachments>,
) -> R<String> {
    rt_of(&state, &project)?
        .chat_send(&conversation, &text, attachments)
        .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn chat_cancel(
    state: State<'_, AppState>,
    project: String,
    conversation: String,
) -> R<()> {
    rt_of(&state, &project)?.chat_cancel(&conversation);
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn chat_set_mode(
    state: State<'_, AppState>,
    project: String,
    conversation: String,
    mode: Mode,
) -> R<()> {
    rt_of(&state, &project)?.chat_set_mode(&conversation, mode);
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn chat_set_model(
    state: State<'_, AppState>,
    project: String,
    conversation: String,
    model: String,
) -> R<()> {
    rt_of(&state, &project)?.chat_set_model(&conversation, &model);
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn proposal_respond(
    state: State<'_, AppState>,
    project: String,
    proposal_id: String,
    decision: ProposalDecision,
) -> R<()> {
    rt_of(&state, &project)?.proposal_respond(&proposal_id, decision)
}

// ---- skills, models, audit ----

#[tauri::command(rename_all = "snake_case")]
pub async fn skills_list(state: State<'_, AppState>, project: String) -> R<Vec<SkillView>> {
    Ok(rt_of(&state, &project)?.skills_list())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn skill_set_enabled(
    state: State<'_, AppState>,
    project: String,
    name: String,
    enabled: bool,
) -> R<()> {
    rt_of(&state, &project)?.skill_set_enabled(&name, enabled);
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn models_list(state: State<'_, AppState>) -> R<Vec<ModelEntry>> {
    Ok(state.workbench.models_list())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn model_probe(state: State<'_, AppState>, model: String) -> R<ModelEntry> {
    state.workbench.model_probe(&model).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn provider_set_key(state: State<'_, AppState>, provider: String, key: String) -> R<()> {
    state.workbench.provider_set_key(&provider, &key)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn audit_export(
    state: State<'_, AppState>,
    from: String,
    to: String,
    path_abs: String,
) -> R<()> {
    state
        .workbench
        .audit_export(&from, &to, &path_abs)
        .map(|_| ())
}

// ---- native dialogs ----

/// A file-type filter of a native dialog.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DialogFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

fn dialog(
    app: &tauri::AppHandle,
    title: Option<String>,
    filters: Option<Vec<DialogFilter>>,
) -> tauri_plugin_dialog::FileDialogBuilder<tauri::Wry> {
    let mut dialog = app.dialog().file();
    if let Some(t) = title {
        dialog = dialog.set_title(t);
    }
    for f in filters.unwrap_or_default() {
        let extensions: Vec<&str> = f.extensions.iter().map(String::as_str).collect();
        dialog = dialog.add_filter(f.name, &extensions);
    }
    dialog
}

fn to_path(picked: FilePath) -> R<String> {
    let path: PathBuf = picked
        .into_path()
        .map_err(|e| IpcError::invalid(format!("not a filesystem path: {e}")))?;
    Ok(path.to_string_lossy().into_owned())
}

async fn answer<T>(rx: tokio::sync::oneshot::Receiver<T>) -> R<T> {
    rx.await
        .map_err(|_| IpcError::internal("the dialog was closed without an answer"))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn dialog_pick_file(
    app: tauri::AppHandle,
    title: Option<String>,
    filters: Option<Vec<DialogFilter>>,
) -> R<Option<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    dialog(&app, title, filters).pick_file(move |p| {
        let _ = tx.send(p);
    });
    answer(rx).await?.map(to_path).transpose()
}

#[tauri::command(rename_all = "snake_case")]
pub async fn dialog_pick_files(
    app: tauri::AppHandle,
    title: Option<String>,
    filters: Option<Vec<DialogFilter>>,
) -> R<Option<Vec<String>>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    dialog(&app, title, filters).pick_files(move |p| {
        let _ = tx.send(p);
    });
    answer(rx)
        .await?
        .map(|paths| paths.into_iter().map(to_path).collect())
        .transpose()
}

#[tauri::command(rename_all = "snake_case")]
pub async fn dialog_pick_directory(
    app: tauri::AppHandle,
    title: Option<String>,
    filters: Option<Vec<DialogFilter>>,
) -> R<Option<String>> {
    let _ = filters; // folders have no type filter
    let (tx, rx) = tokio::sync::oneshot::channel();
    dialog(&app, title, None).pick_folder(move |p| {
        let _ = tx.send(p);
    });
    answer(rx).await?.map(to_path).transpose()
}

#[tauri::command(rename_all = "snake_case")]
pub async fn dialog_save_file(
    app: tauri::AppHandle,
    title: Option<String>,
    filters: Option<Vec<DialogFilter>>,
) -> R<Option<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    dialog(&app, title, filters).save_file(move |p| {
        let _ = tx.send(p);
    });
    answer(rx).await?.map(to_path).transpose()
}

//! One `#[tauri::command]` per row of docs/IPC.md sections 4, 7, and 9, with exactly those names and
//! argument names (`rename_all = "snake_case"` keeps the JS-side keys as written in the doc).
//! Every error is serialized as `IpcError`.

use crate::{AppState, Subscription};
use analyst_core::service::{agent_runner, SessionRuntime};
use analyst_deck_protocol::deck::{DeckMode, Slide};
use analyst_deck_protocol::kernel::LayoutThumbnail;
use analyst_deck_protocol::ui::{
    AssetView, DeckInfo, DeckView, ExportView, FileContent as DeckFileContent,
    IpcError as DeckIpcError, RefView, SearchHit as RefSearchHit, SlideView, SourcePassage,
    WriteResult as DeckWriteResult,
};
use analyst_protocol::evals::Mode;
use analyst_protocol::kernel::{
    ArtifactKind, Diagnostic, EqCheckResult, Equation, OutListResult, OutReadResult, PackageInfo,
};
use analyst_protocol::transcript::Event as TranscriptEvent;
use analyst_protocol::ui::*;
use serde_json::Value;
use std::sync::Arc;
use tauri::ipc::Channel;
use tauri::State;
use tauri_plugin_dialog::DialogExt;

type R<T> = Result<T, IpcError>;

fn rt_of(state: &State<'_, AppState>, id: &str) -> R<Arc<SessionRuntime>> {
    state.workbench.session(id).map_err(IpcError::from)
}

fn parse_mode(mode: &str) -> R<Mode> {
    mode.parse::<Mode>().map_err(IpcError::invalid)
}

// ---- app and sessions ----

#[tauri::command(rename_all = "snake_case")]
pub async fn app_info(state: State<'_, AppState>) -> R<AppInfo> {
    Ok(state.workbench.app_info())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn sessions_list(state: State<'_, AppState>) -> R<Vec<SessionSummary>> {
    state.workbench.sessions_list().map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn session_create(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    repo_abs: String,
    target: Option<String>,
    model: Option<String>,
    init: Option<bool>,
) -> R<SessionInfo> {
    let info = state
        .workbench
        .create_session(
            &repo_abs,
            target.as_deref(),
            model.as_deref(),
            init.unwrap_or(false),
        )
        .await
        .map_err(IpcError::from)?;
    allow_session_assets(&app, &state, &info.summary.id);
    Ok(info)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn session_open(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    session: String,
) -> R<SessionInfo> {
    let info = state
        .workbench
        .open_session(&session)
        .await
        .map_err(IpcError::from)?;
    allow_session_assets(&app, &state, &session);
    Ok(info)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn session_close(state: State<'_, AppState>, session: String) -> R<()> {
    // Subscriptions of a closed session end too.
    if let Ok(mut subs) = state.subscriptions.lock() {
        let ids: Vec<String> = subs
            .iter()
            .filter(|(_, s)| s.session == session)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(s) = subs.remove(&id) {
                s.handle.abort();
            }
        }
    }
    state
        .workbench
        .close_session(&session)
        .await
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn session_info(state: State<'_, AppState>, session: String) -> R<SessionInfo> {
    state
        .workbench
        .session_info(&session)
        .await
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn session_rename(
    state: State<'_, AppState>,
    session: String,
    name: String,
) -> R<SessionSummary> {
    state
        .workbench
        .rename_session(&session, &name)
        .map_err(Into::into)
}

/// Opens a per-window channel; events flow until `session_unsubscribe` or the window closes.
/// `not_found` when the session is not open (the renderer opens first, IPC.md section 7).
/// A subscriber that falls behind receives `{"type":"resync"}`; terminal output arrives merged
/// on its own bounded path and cannot displace control events.
#[tauri::command(rename_all = "snake_case")]
pub async fn session_subscribe(
    state: State<'_, AppState>,
    window: tauri::Window,
    session: String,
    channel: Channel<SessionEvent>,
) -> R<String> {
    let rt = rt_of(&state, &session)?;
    let id = format!("sub-{}", analyst_core::util::stamped_id());
    let mut rx = rt.subscribe();
    drop(rt);
    let sub_id = id.clone();
    let handle = tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            if matches!(event, SessionEvent::Resync) {
                tracing::warn!("subscription {sub_id}: fell behind, asking the renderer to resync");
            }
            if channel.send(event).is_err() {
                tracing::debug!("subscription {sub_id}: channel closed");
                break;
            }
        }
    });
    if let Ok(mut subs) = state.subscriptions.lock() {
        subs.insert(
            id.clone(),
            Subscription {
                window: window.label().to_string(),
                session,
                handle,
            },
        );
    }
    Ok(id)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn session_unsubscribe(state: State<'_, AppState>, subscription: String) -> R<()> {
    let sub = state
        .subscriptions
        .lock()
        .ok()
        .and_then(|mut s| s.remove(&subscription));
    match sub {
        Some(s) => {
            s.handle.abort();
            Ok(())
        }
        None => Err(IpcError::not_found(format!(
            "subscription `{subscription}` not found"
        ))),
    }
}

#[tauri::command(rename_all = "snake_case")]
pub async fn session_set_target(
    state: State<'_, AppState>,
    session: String,
    target: String,
) -> R<TaskId> {
    rt_of(&state, &session)?
        .set_target_task(&target)
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn session_set_policy(
    state: State<'_, AppState>,
    session: String,
    policy: PolicyPatch,
) -> R<PolicyView> {
    let rt = rt_of(&state, &session)?;
    let view = rt.set_policy(&policy)?;
    rt.emit_session_changed().await;
    Ok(view)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn layout_get(state: State<'_, AppState>, session: String) -> R<Option<String>> {
    state.workbench.layout_get(&session).map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn layout_save(state: State<'_, AppState>, session: String, layout: String) -> R<()> {
    state
        .workbench
        .layout_save(&session, &layout)
        .map_err(Into::into)
}

// ---- filesystem ----

#[tauri::command(rename_all = "snake_case")]
pub async fn fs_tree(
    state: State<'_, AppState>,
    session: String,
    path: Option<String>,
    depth: Option<u32>,
) -> R<Vec<FileEntry>> {
    rt_of(&state, &session)?
        .fs_tree(path.as_deref(), depth)
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn fs_read(state: State<'_, AppState>, session: String, path: String) -> R<FileContent> {
    rt_of(&state, &session)?.fs_read(&path).map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn fs_read_binary(
    state: State<'_, AppState>,
    session: String,
    path: String,
) -> R<FigureData> {
    rt_of(&state, &session)?
        .fs_read_binary(&path)
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn fs_write(
    state: State<'_, AppState>,
    session: String,
    path: String,
    content: String,
    expected_hash: Option<String>,
) -> R<WriteResult> {
    rt_of(&state, &session)?
        .fs_write(&path, &content, expected_hash.as_deref())
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn fs_create(
    state: State<'_, AppState>,
    session: String,
    path: String,
    is_dir: bool,
) -> R<()> {
    rt_of(&state, &session)?
        .fs_create(&path, is_dir)
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn fs_delete(state: State<'_, AppState>, session: String, path: String) -> R<()> {
    rt_of(&state, &session)?
        .fs_delete(&path)
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn fs_rename(
    state: State<'_, AppState>,
    session: String,
    from: String,
    to: String,
) -> R<()> {
    rt_of(&state, &session)?
        .fs_rename(&from, &to)
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn fs_search(
    state: State<'_, AppState>,
    session: String,
    pattern: String,
    glob: Option<String>,
    max_results: Option<u32>,
) -> R<Vec<SearchHit>> {
    rt_of(&state, &session)?
        .fs_search(&pattern, glob.as_deref(), max_results)
        .await
        .map_err(Into::into)
}

// ---- index, checks, runs ----

#[tauri::command(rename_all = "snake_case")]
pub async fn index_get(state: State<'_, AppState>, session: String) -> R<IndexResult> {
    rt_of(&state, &session)?
        .index_get()
        .await
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn check_run(
    state: State<'_, AppState>,
    session: String,
    paths: Option<Vec<String>>,
) -> R<Vec<Diagnostic>> {
    rt_of(&state, &session)?
        .check_run(paths)
        .await
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn run_start(
    state: State<'_, AppState>,
    session: String,
    script: String,
    args: Vec<String>,
) -> R<RunView> {
    rt_of(&state, &session)?
        .run_start(&script, args)
        .await
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn run_cancel(state: State<'_, AppState>, session: String, run_id: String) -> R<()> {
    rt_of(&state, &session)?
        .run_cancel(&run_id)
        .await
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn runs_list(
    state: State<'_, AppState>,
    session: String,
    limit: Option<u32>,
) -> R<Vec<RunView>> {
    Ok(rt_of(&state, &session)?.runs_list(limit))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn run_get(state: State<'_, AppState>, session: String, run_id: String) -> R<RunDetail> {
    let rt = rt_of(&state, &session)?;
    // Reads up to 2 MiB per stream from the run's log files.
    tokio::task::spawn_blocking(move || rt.run_get(&run_id))
        .await
        .map_err(|e| IpcError::internal(e.to_string()))?
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn out_list(
    state: State<'_, AppState>,
    session: String,
    path: Option<String>,
    run_id: Option<String>,
) -> R<OutListResult> {
    rt_of(&state, &session)?
        .out_list(path, run_id)
        .await
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn out_read(
    state: State<'_, AppState>,
    session: String,
    path: String,
    max_bytes: Option<u64>,
) -> R<OutReadResult> {
    rt_of(&state, &session)?
        .out_read(path, max_bytes)
        .await
        .map_err(Into::into)
}

// ---- figures and equations ----

#[tauri::command(rename_all = "snake_case")]
pub async fn figures_list(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    session: String,
    limit: Option<u32>,
) -> R<Vec<FigureGroup>> {
    let groups = rt_of(&state, &session)?.figures_list(limit).await;
    // The figures carry `file_abs`; their folders must be in the asset scope.
    allow_session_assets(&app, &state, &session);
    Ok(groups)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn figure_data(
    state: State<'_, AppState>,
    session: String,
    path: String,
) -> R<FigureData> {
    rt_of(&state, &session)?
        .figure_data(&path)
        .await
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn eq_extract(
    state: State<'_, AppState>,
    session: String,
    path: String,
) -> R<Vec<Equation>> {
    rt_of(&state, &session)?
        .eq_extract(&path)
        .await
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn eq_check(
    state: State<'_, AppState>,
    session: String,
    path: String,
    function: String,
) -> R<EqCheckResult> {
    rt_of(&state, &session)?
        .eq_check(&path, &function)
        .await
        .map_err(Into::into)
}

// ---- environment ----

#[tauri::command(rename_all = "snake_case")]
pub async fn env_packages(state: State<'_, AppState>, session: String) -> R<Vec<PackageInfo>> {
    rt_of(&state, &session)?
        .env_packages()
        .await
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn env_add(
    state: State<'_, AppState>,
    session: String,
    packages: Vec<String>,
) -> R<TaskId> {
    rt_of(&state, &session)?
        .env_task("add", packages)
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn env_remove(
    state: State<'_, AppState>,
    session: String,
    packages: Vec<String>,
) -> R<TaskId> {
    rt_of(&state, &session)?
        .env_task("remove", packages)
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn env_sync(state: State<'_, AppState>, session: String) -> R<TaskId> {
    rt_of(&state, &session)?
        .env_task("sync", vec![])
        .map_err(Into::into)
}

// ---- conversations and chat ----

#[tauri::command(rename_all = "snake_case")]
pub async fn conversation_new(
    state: State<'_, AppState>,
    session: String,
    title: Option<String>,
) -> R<ConversationSummary> {
    let rt = rt_of(&state, &session)?;
    let c = rt.conversation_new(title.as_deref())?;
    rt.emit_session_changed().await;
    Ok(c)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn conversation_select(
    state: State<'_, AppState>,
    session: String,
    conversation: String,
) -> R<()> {
    let rt = rt_of(&state, &session)?;
    rt.conversation_select(&conversation)?;
    rt.emit_session_changed().await;
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn conversation_transcript(
    state: State<'_, AppState>,
    session: String,
    conversation: String,
) -> R<Vec<TranscriptEvent>> {
    rt_of(&state, &session)?
        .conversation_transcript(&conversation)
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn chat_send(
    state: State<'_, AppState>,
    session: String,
    conversation: String,
    text: String,
    attachments: Option<Vec<Attachment>>,
) -> R<TurnId> {
    let rt = rt_of(&state, &session)?;
    let conv = rt.conversation_runtime(&conversation)?;
    agent_runner::start_turn(rt, conv, &text, attachments.unwrap_or_default())
        .await
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn chat_cancel(
    state: State<'_, AppState>,
    session: String,
    conversation: String,
) -> R<()> {
    let rt = rt_of(&state, &session)?;
    let conv = rt.conversation_runtime(&conversation)?;
    agent_runner::cancel_turn(&rt, &conv).await;
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn chat_set_mode(
    state: State<'_, AppState>,
    session: String,
    conversation: String,
    mode: String,
) -> R<()> {
    let mode = parse_mode(&mode)?;
    rt_of(&state, &session)?
        .chat_set_mode(&conversation, mode)
        .await
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn chat_set_model(
    state: State<'_, AppState>,
    session: String,
    conversation: String,
    model: String,
) -> R<()> {
    rt_of(&state, &session)?
        .chat_set_model(&conversation, &model)
        .await
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn proposal_respond(
    state: State<'_, AppState>,
    session: String,
    proposal_id: String,
    decision: ProposalDecision,
) -> R<()> {
    rt_of(&state, &session)?
        .proposal_respond(&proposal_id, decision)
        .map_err(Into::into)
}

// ---- models and targets ----

#[tauri::command(rename_all = "snake_case")]
pub async fn models_list(state: State<'_, AppState>) -> R<Vec<ModelEntry>> {
    let _ = state.workbench.reload_global();
    Ok(state.workbench.models_list())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn model_probe(state: State<'_, AppState>, model: String) -> R<ModelEntry> {
    state
        .workbench
        .model_probe(&model)
        .await
        .map_err(Into::into)
}

/// One row per configured provider; key values never cross the boundary.
#[tauri::command(rename_all = "snake_case")]
pub async fn provider_keys_status(state: State<'_, AppState>) -> R<Vec<ProviderKeyStatus>> {
    Ok(state.workbench.provider_keys_status())
}

/// Store (or, with an empty key, remove) a provider's key in `$ANALYST_HOME/.env`.
#[tauri::command(rename_all = "snake_case")]
pub async fn provider_key_set(state: State<'_, AppState>, provider: String, key: String) -> R<()> {
    state
        .workbench
        .provider_key_set(&provider, &key)
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn targets_list(state: State<'_, AppState>) -> R<Vec<TargetInfo>> {
    let _ = state.workbench.reload_global();
    Ok(state.workbench.targets_list())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn target_bootstrap(state: State<'_, AppState>, target: String) -> R<TaskId> {
    state
        .workbench
        .target_bootstrap(&target)
        .map_err(Into::into)
}

// ---- terminals ----

#[tauri::command(rename_all = "snake_case")]
pub async fn terminal_open(
    state: State<'_, AppState>,
    session: String,
    cols: u16,
    rows: u16,
) -> R<TerminalOpened> {
    state
        .workbench
        .terminal_open(&session, cols, rows)
        .await
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn terminal_write(
    state: State<'_, AppState>,
    terminal_id: String,
    data: String,
) -> R<()> {
    state
        .workbench
        .terminal_write(&terminal_id, &data)
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn terminal_resize(
    state: State<'_, AppState>,
    terminal_id: String,
    cols: u16,
    rows: u16,
) -> R<()> {
    state
        .workbench
        .terminal_resize(&terminal_id, cols, rows)
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn terminal_close(state: State<'_, AppState>, terminal_id: String) -> R<()> {
    state
        .workbench
        .terminal_close(&terminal_id)
        .map_err(Into::into)
}

// ---- dialogs ----

/// Native folder picker; `null` when cancelled.
#[tauri::command(rename_all = "snake_case")]
pub async fn dialog_pick_directory(
    app: tauri::AppHandle,
    title: Option<String>,
) -> R<Option<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app.dialog().file();
    if let Some(t) = title {
        dialog = dialog.set_title(t);
    }
    dialog.pick_folder(move |p| {
        let _ = tx.send(p);
    });
    let picked = rx
        .await
        .map_err(|_| IpcError::internal("the folder dialog was dropped"))?;
    match picked {
        None => Ok(None),
        Some(p) => {
            let path = p
                .into_path()
                .map_err(|e| IpcError::invalid(format!("not a filesystem path: {e}")))?;
            Ok(Some(path.to_string_lossy().to_string()))
        }
    }
}

/// Native open dialog for files (a deck template, references); `null` when cancelled.
#[tauri::command(rename_all = "snake_case")]
pub async fn dialog_pick_files(
    app: tauri::AppHandle,
    title: Option<String>,
    filters: Option<Vec<DialogFilter>>,
    multiple: Option<bool>,
) -> R<Option<Vec<String>>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app.dialog().file();
    if let Some(t) = title {
        dialog = dialog.set_title(t);
    }
    for f in filters.unwrap_or_default() {
        let exts: Vec<&str> = f.extensions.iter().map(String::as_str).collect();
        dialog = dialog.add_filter(f.name, &exts);
    }
    if multiple.unwrap_or(false) {
        dialog.pick_files(move |p| {
            let _ = tx.send(p);
        });
    } else {
        dialog.pick_file(move |p| {
            let _ = tx.send(p.map(|one| vec![one]));
        });
    }
    let picked = rx
        .await
        .map_err(|_| IpcError::internal("the file dialog was dropped"))?;
    match picked {
        None => Ok(None),
        Some(files) => files
            .into_iter()
            .map(|p| {
                p.into_path()
                    .map(|path| path.to_string_lossy().to_string())
                    .map_err(|e| IpcError::invalid(format!("not a filesystem path: {e}")))
            })
            .collect::<R<Vec<String>>>()
            .map(Some),
    }
}

/// One filter of [`dialog_pick_files`]: a label and the file extensions it shows.
#[derive(serde::Deserialize)]
pub struct DialogFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

// ---- asset scope ----

/// The folders of a session's decks the webview may load through the asset protocol: each
/// deck's render cache (slide thumbnails, fidelity renders, layout thumbnails, PDF page images
/// under `.present/cache/`), its image `assets/`, and its `references/` (image references are
/// shown as their source page). Nothing else of the repository is opened to the webview here.
pub fn deck_asset_dirs(repo: &std::path::Path, slugs: &[String]) -> Vec<std::path::PathBuf> {
    slugs
        .iter()
        .filter(|s| analyst_core::service::decks::valid_slug(s).is_ok())
        .flat_map(|slug| {
            let root = repo.join("decks").join(slug);
            [
                root.join(".present").join("cache"),
                root.join("assets"),
                root.join("references"),
            ]
        })
        .collect()
}

/// The folders of a session's figures the webview may load through the asset protocol: the
/// project's run output directory (`out/` by default, where local runs write their figures) and
/// the session's `artifacts/` (figures pulled back from a remote target). An output directory
/// that is absolute, empty, `.`, or leaves the repository is not opened.
pub fn figure_asset_dirs(
    repo: &std::path::Path,
    out_dir: &str,
    artifacts_dir: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    let rel = out_dir.trim_matches('/');
    let plain = !rel.is_empty()
        && !std::path::Path::new(out_dir).is_absolute()
        && rel
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && !c.contains('\\'));
    if plain {
        dirs.push(repo.join(rel));
    }
    dirs.push(artifacts_dir.to_path_buf());
    dirs
}

/// Adds the deck folders and figure folders of an open session to the asset protocol scope
/// (once per folder). Called when a session is opened or created, when its decks change, and
/// when its figures are listed (the output directory follows the target).
pub(crate) fn allow_session_assets(
    app: &tauri::AppHandle,
    state: &State<'_, AppState>,
    session: &str,
) {
    use tauri::Manager;
    let Ok(rt) = rt_of(state, session) else {
        return;
    };
    let slugs: Vec<String> = rt.decks_list().into_iter().map(|d| d.slug).collect();
    let mut dirs = deck_asset_dirs(&rt.repo, &slugs);
    dirs.extend(figure_asset_dirs(
        &rt.repo,
        &rt.out_dir(),
        &rt.session_snapshot().artifacts_dir(),
    ));
    let Ok(mut allowed) = state.asset_dirs.lock() else {
        return;
    };
    let scope = app.asset_protocol_scope();
    for dir in dirs {
        if allowed.insert(dir.clone()) {
            if let Err(e) = scope.allow_directory(&dir, true) {
                tracing::warn!("asset scope: {}: {e}", dir.display());
            }
        }
    }
}

// ---- decks (docs/IPC.md section 9) ----
//
// Thin glue over the per-deck service of a session: `session` plus `deck` (the slug under
// `decks/`) where the deck workbench had a project id. A deck's kernel starts on first use.
// Deck events reach the renderer on the session's channel as `{type: "deck", deck, event}`;
// commands that return a `TaskId` finish with a `task_done` deck event.

type DeckService = analyst_core::service::decks::DeckService;

async fn deck_of(state: &State<'_, AppState>, session: &str, deck: &str) -> R<Arc<DeckService>> {
    rt_of(state, session)?
        .deck(deck)
        .await
        .map_err(IpcError::from)
}

/// A deck service error as an IPC error (same code, message, and detail).
fn de(e: DeckIpcError) -> IpcError {
    analyst_core::service::ServiceError::from(e).into()
}

#[tauri::command(rename_all = "snake_case")]
pub async fn decks_list(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    session: String,
) -> R<Vec<DeckSummary>> {
    let list = rt_of(&state, &session)?.decks_list();
    allow_session_assets(&app, &state, &session);
    Ok(list)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn deck_create(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    session: String,
    title: String,
    template_abs: String,
    slug: Option<String>,
    code: Option<bool>,
) -> R<DeckSummary> {
    let summary = rt_of(&state, &session)?
        .deck_create(
            slug.as_deref(),
            &title,
            &template_abs,
            code.unwrap_or(false),
        )
        .map_err(IpcError::from)?;
    allow_session_assets(&app, &state, &session);
    Ok(summary)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn deck_open(state: State<'_, AppState>, session: String, deck: String) -> R<DeckInfo> {
    deck_of(&state, &session, &deck)
        .await?
        .info()
        .await
        .map_err(de)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn template_thumbnails(
    state: State<'_, AppState>,
    session: String,
    deck: String,
) -> R<Vec<LayoutThumbnail>> {
    deck_of(&state, &session, &deck)
        .await?
        .template_thumbnails()
        .await
        .map_err(de)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn template_replace(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    template_abs: String,
) -> R<TaskId> {
    Ok(deck_of(&state, &session, &deck)
        .await?
        .template_replace(template_abs))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn deck_get(state: State<'_, AppState>, session: String, deck: String) -> R<DeckView> {
    deck_of(&state, &session, &deck)
        .await?
        .deck_get()
        .map_err(de)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn slide_update(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    slide: Slide,
    expected_hash: Option<String>,
) -> R<SlideView> {
    deck_of(&state, &session, &deck)
        .await?
        .slide_update(slide, expected_hash.as_deref())
        .map_err(de)
}

/// Places a figure of a run on a slide as a user edit, cited by its run: the core function of
/// the agent's `slide.place_figure` (`SessionRuntime::slide_place_figure`).
#[tauri::command(rename_all = "snake_case")]
pub async fn slide_place_figure(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    slide_id: String,
    run_id: String,
    artifact: String,
    placeholder: Option<String>,
) -> R<SlideView> {
    rt_of(&state, &session)?
        .slide_place_figure(&deck, &slide_id, &run_id, &artifact, placeholder.as_deref())
        .await
        .map_err(Into::into)
}

/// Places the equation of a function on a slide as a user edit, with its equation record (LaTeX,
/// tier, numeric check, function, file, content hash): the core function of the agent's
/// `slide.place_equation` (`SessionRuntime::slide_place_equation`).
#[tauri::command(rename_all = "snake_case")]
pub async fn slide_place_equation(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    slide_id: String,
    path: String,
    function: String,
    placeholder: Option<String>,
) -> R<SlideView> {
    rt_of(&state, &session)?
        .slide_place_equation(&deck, &slide_id, &path, &function, placeholder.as_deref())
        .await
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn slide_create(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    after: Option<String>,
    layout: String,
) -> R<SlideView> {
    deck_of(&state, &session, &deck)
        .await?
        .slide_create(after.as_deref(), &layout)
        .await
        .map_err(de)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn slide_delete(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    slide_id: String,
) -> R<()> {
    deck_of(&state, &session, &deck)
        .await?
        .slide_delete(&slide_id)
        .map_err(de)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn slide_duplicate(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    slide_id: String,
) -> R<SlideView> {
    deck_of(&state, &session, &deck)
        .await?
        .slide_duplicate(&slide_id)
        .map_err(de)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn slide_move(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    slide_id: String,
    after: Option<String>,
    section: Option<String>,
) -> R<()> {
    deck_of(&state, &session, &deck)
        .await?
        .slide_move(&slide_id, after.as_deref(), section.as_deref())
        .map_err(de)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn slide_lock(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    slide_id: String,
    element_id: Option<String>,
    locked: bool,
) -> R<()> {
    deck_of(&state, &session, &deck)
        .await?
        .slide_lock(&slide_id, element_id.as_deref(), locked)
        .map_err(de)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn slide_preview_png(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    slide_id: String,
    png_base64: String,
) -> R<()> {
    deck_of(&state, &session, &deck)
        .await?
        .slide_preview_png(&slide_id, &png_base64)
        .map_err(de)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn chart_compute(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    source: Value,
    steps: Option<Value>,
) -> R<Value> {
    deck_of(&state, &session, &deck)
        .await?
        .chart_compute(source, steps.unwrap_or(Value::Null))
        .await
        .map_err(de)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn deck_code_read(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    path: String,
) -> R<DeckFileContent> {
    deck_of(&state, &session, &deck)
        .await?
        .code_read(&path)
        .map_err(de)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn deck_code_write(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    path: String,
    content: String,
    expected_hash: Option<String>,
) -> R<DeckWriteResult> {
    deck_of(&state, &session, &deck)
        .await?
        .code_write(&path, &content, expected_hash.as_deref())
        .map_err(de)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn deck_build(state: State<'_, AppState>, session: String, deck: String) -> R<TaskId> {
    Ok(deck_of(&state, &session, &deck).await?.deck_build())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn deck_switch_mode(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    to: DeckMode,
    via: Option<String>,
) -> R<TaskId> {
    Ok(deck_of(&state, &session, &deck)
        .await?
        .deck_switch_mode(to, via))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn render_fidelity(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    slides: Option<Vec<String>>,
) -> R<TaskId> {
    // The engine renders the whole deck in one pass; `slides` only says which thumbnails the
    // window is waiting for.
    let _ = slides;
    let svc = deck_of(&state, &session, &deck).await?;
    Ok(svc.spawn_task("Rendering slides", |rt| async move {
        let n = rt.render_thumbnails().await?;
        Ok(Some(format!("{n} slides rendered")))
    }))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn deck_check_run(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    slides: Option<Vec<String>>,
) -> R<Vec<Value>> {
    deck_of(&state, &session, &deck)
        .await?
        .run_checks(slides)
        .await
        .map_err(de)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn refs_list(
    state: State<'_, AppState>,
    session: String,
    deck: String,
) -> R<Vec<RefView>> {
    deck_of(&state, &session, &deck)
        .await?
        .refs_list()
        .map_err(de)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn refs_add(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    paths_abs: Vec<String>,
) -> R<TaskId> {
    Ok(deck_of(&state, &session, &deck).await?.refs_add(paths_abs))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn refs_remove(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    ref_id: String,
) -> R<()> {
    deck_of(&state, &session, &deck)
        .await?
        .refs_remove(&ref_id)
        .map_err(de)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn refs_remove_unused(
    state: State<'_, AppState>,
    session: String,
    deck: String,
) -> R<Vec<String>> {
    deck_of(&state, &session, &deck)
        .await?
        .refs_remove_unused()
        .map_err(de)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn refs_search(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    query: String,
    ref_ids: Option<Vec<String>>,
) -> R<Vec<RefSearchHit>> {
    deck_of(&state, &session, &deck)
        .await?
        .refs_search(&query, ref_ids)
        .await
        .map_err(de)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn source_passage(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    cite: String,
) -> R<SourcePassage> {
    deck_of(&state, &session, &deck)
        .await?
        .source_passage(&cite)
        .await
        .map_err(de)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn assets_list(
    state: State<'_, AppState>,
    session: String,
    deck: String,
) -> R<Vec<AssetView>> {
    deck_of(&state, &session, &deck)
        .await?
        .assets_list()
        .map_err(de)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn asset_add(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    path_abs: String,
) -> R<AssetView> {
    deck_of(&state, &session, &deck)
        .await?
        .asset_add(&path_abs)
        .map_err(de)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn exports_list(
    state: State<'_, AppState>,
    session: String,
    deck: String,
) -> R<Vec<ExportView>> {
    Ok(deck_of(&state, &session, &deck).await?.exports_list())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn export_run(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    format: String,
) -> R<TaskId> {
    Ok(deck_of(&state, &session, &deck).await?.export_run(format))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn export_reveal(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    export_id: String,
) -> R<()> {
    let path = deck_of(&state, &session, &deck)
        .await?
        .export_path(&export_id)
        .map_err(de)?;
    reveal_in_file_manager(&path)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn export_open(
    state: State<'_, AppState>,
    session: String,
    deck: String,
    export_id: String,
) -> R<()> {
    let path = deck_of(&state, &session, &deck)
        .await?
        .export_path(&export_id)
        .map_err(de)?;
    open_with_default_app(&path)
}

/// Run a desktop helper program; a failure to start is an `io` error.
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

fn reveal_in_file_manager(path: &std::path::Path) -> R<()> {
    #[cfg(target_os = "macos")]
    {
        launch("open", &[std::ffi::OsStr::new("-R"), path.as_os_str()])
    }
    #[cfg(not(target_os = "macos"))]
    {
        let dir = path.parent().unwrap_or(path);
        launch("xdg-open", &[dir.as_os_str()])
    }
}

fn open_with_default_app(path: &std::path::Path) -> R<()> {
    #[cfg(target_os = "macos")]
    {
        launch("open", &[path.as_os_str()])
    }
    #[cfg(not(target_os = "macos"))]
    {
        launch("xdg-open", &[path.as_os_str()])
    }
}

// ---- history: one undo timeline (git checkpoints) for code and decks ----

#[tauri::command(rename_all = "snake_case")]
pub async fn history_list(
    state: State<'_, AppState>,
    session: String,
    limit: Option<u32>,
) -> R<Vec<CheckpointView>> {
    rt_of(&state, &session)?
        .history_list(limit)
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn history_restore(
    state: State<'_, AppState>,
    session: String,
    checkpoint: String,
) -> R<()> {
    rt_of(&state, &session)?
        .history_restore(&checkpoint)
        .await
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn history_revert_turn(
    state: State<'_, AppState>,
    session: String,
    turn: String,
) -> R<CheckpointView> {
    rt_of(&state, &session)?
        .history_revert_turn(&turn)
        .await
        .map_err(Into::into)
}

// ---- artifact index (what runs produced; slides cite runs from it) ----

#[tauri::command(rename_all = "snake_case")]
pub async fn artifacts_list(
    state: State<'_, AppState>,
    session: String,
    run_id: Option<String>,
    kind: Option<ArtifactKind>,
    limit: Option<u32>,
) -> R<Vec<ArtifactEntry>> {
    Ok(rt_of(&state, &session)?.artifacts_list(run_id.as_deref(), kind, limit))
}

#[cfg(test)]
mod tests {
    use super::{deck_asset_dirs, figure_asset_dirs};
    use std::path::{Path, PathBuf};

    #[test]
    fn the_asset_scope_covers_the_output_directory_and_pulled_figures() {
        let repo = Path::new("/home/u/analysis");
        let artifacts = Path::new("/home/u/.analyst/sessions/s1/artifacts");
        assert_eq!(
            figure_asset_dirs(repo, "out", artifacts),
            vec![repo.join("out"), artifacts.to_path_buf()]
        );
        assert_eq!(
            figure_asset_dirs(repo, "results/out/", artifacts)[0],
            repo.join("results/out")
        );
        // An output directory that would open the repository itself, or leave it, is skipped;
        // the session's pulled figures stay allowed.
        for bad in ["", ".", "/", "..", "../x", "a/../..", "/etc", "a/./b"] {
            assert_eq!(
                figure_asset_dirs(repo, bad, artifacts),
                vec![artifacts.to_path_buf()],
                "out_dir {bad:?} must be refused"
            );
        }
    }

    #[test]
    fn the_asset_scope_covers_each_decks_cache_assets_and_references_only() {
        let repo = Path::new("/home/u/analysis");
        let dirs = deck_asset_dirs(repo, &["q3-review".into(), "board".into()]);
        let want: Vec<PathBuf> = [
            "decks/q3-review/.present/cache",
            "decks/q3-review/assets",
            "decks/q3-review/references",
            "decks/board/.present/cache",
            "decks/board/assets",
            "decks/board/references",
        ]
        .iter()
        .map(|p| repo.join(p))
        .collect();
        assert_eq!(dirs, want);
        // Every folder is inside the repository's `decks/`; the repository itself, its code,
        // data and outputs, and the deck documents are not opened to the webview.
        for d in &dirs {
            assert!(d.starts_with(repo.join("decks")), "{}", d.display());
            assert!(!d.ends_with("deck"), "{}", d.display());
        }
        assert!(!dirs.contains(&repo.to_path_buf()));
    }

    #[test]
    fn a_slug_that_could_leave_decks_is_not_added_to_the_asset_scope() {
        let repo = Path::new("/home/u/analysis");
        for bad in ["..", "../..", "a/b", "", ".hidden", "UPPER"] {
            assert!(
                deck_asset_dirs(repo, &[bad.to_string()]).is_empty(),
                "slug {bad:?} must be refused"
            );
        }
    }
}

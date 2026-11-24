//! The shell's side of the command table: the two subscription commands (a Tauri channel per
//! window) and the [`Host`] of `analyst_core::commands` (native dialogs, the asset protocol
//! scope, the file manager). Every other command is a row of `analyst_core::commands`, which
//! the invoke handler in `lib.rs` dispatches to.

use crate::{AppState, Subscription};
use analyst_core::commands::{DialogFilter, Host};
use analyst_core::service::Workbench;
use analyst_protocol::ui::*;
use std::collections::HashSet;
use std::sync::Mutex;
use tauri::ipc::Channel;
use tauri::{Manager, State};
use tauri_plugin_dialog::DialogExt;

type R<T> = Result<T, IpcError>;

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
    let rt = state.workbench.session(&session).map_err(IpcError::from)?;
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

/// The desktop shell as the command table's host.
pub struct DesktopHost {
    pub app: tauri::AppHandle,
}

#[async_trait::async_trait]
impl Host for DesktopHost {
    fn session_assets(&self, wb: &Workbench, session: &str) {
        allow_session_assets(&self.app, wb, session);
        // From now on, decks created by any path (the agent included) widen the scope at once.
        let state = self.app.state::<AppState>();
        let Ok(mut watchers) = state.asset_watchers.lock() else {
            return;
        };
        if watchers.get(session).is_some_and(|h| !h.is_finished()) {
            return;
        }
        let Ok(rt) = wb.session(session) else {
            return;
        };
        let (app, workbench, id) = (
            self.app.clone(),
            state.workbench.clone(),
            session.to_string(),
        );
        let handle = watch_asset_events(&rt, move || allow_session_assets(&app, &workbench, &id));
        watchers.insert(session.to_string(), handle);
    }

    fn session_closed(&self, session: &str) {
        let state = self.app.state::<AppState>();
        if let Some(h) = state
            .asset_watchers
            .lock()
            .ok()
            .and_then(|mut w| w.remove(session))
        {
            h.abort();
        }
        // Subscriptions of a closed session end too.
        let Ok(mut subs) = state.subscriptions.lock() else {
            return;
        };
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

    async fn pick_directory(&self, title: Option<String>) -> R<Option<String>> {
        dialog_pick_directory(&self.app, title).await
    }

    async fn pick_files(
        &self,
        title: Option<String>,
        filters: Vec<DialogFilter>,
        multiple: bool,
    ) -> R<Option<Vec<String>>> {
        dialog_pick_files(&self.app, title, filters, multiple).await
    }

    fn reveal(&self, path: &std::path::Path) -> R<()> {
        reveal_in_file_manager(path)
    }

    fn open(&self, path: &std::path::Path) -> R<()> {
        open_with_default_app(path)
    }
}

/// Native folder picker; `null` when cancelled.
async fn dialog_pick_directory(app: &tauri::AppHandle, title: Option<String>) -> R<Option<String>> {
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
async fn dialog_pick_files(
    app: &tauri::AppHandle,
    title: Option<String>,
    filters: Vec<DialogFilter>,
    multiple: bool,
) -> R<Option<Vec<String>>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app.dialog().file();
    if let Some(t) = title {
        dialog = dialog.set_title(t);
    }
    for f in filters {
        let exts: Vec<&str> = f.extensions.iter().map(String::as_str).collect();
        dialog = dialog.add_filter(f.name, &exts);
    }
    if multiple {
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

/// The deck folders and figure folders of an open session (empty when it is not open).
pub(crate) fn session_asset_dirs(wb: &Workbench, session: &str) -> Vec<std::path::PathBuf> {
    let Ok(rt) = wb.session(session) else {
        return vec![];
    };
    let slugs: Vec<String> = rt.decks_list().into_iter().map(|d| d.slug).collect();
    let mut dirs = deck_asset_dirs(&rt.repo, &slugs);
    dirs.extend(figure_asset_dirs(
        &rt.repo,
        &rt.out_dir(),
        &rt.session_snapshot().artifacts_dir(),
    ));
    dirs
}

/// The folders of `dirs` not yet in `allowed`, which are added to it.
pub(crate) fn not_yet_allowed(
    allowed: &Mutex<HashSet<std::path::PathBuf>>,
    dirs: Vec<std::path::PathBuf>,
) -> Vec<std::path::PathBuf> {
    let Ok(mut allowed) = allowed.lock() else {
        return vec![];
    };
    dirs.into_iter()
        .filter(|d| allowed.insert(d.clone()))
        .collect()
}

/// Adds the deck folders and figure folders of an open session to the asset protocol scope
/// (once per folder). Called when a session is opened or created, when its decks change (by
/// any path: the Decks panel, the agent, a checkpoint restore; see [`watch_asset_events`]), and
/// when its figures are listed (the output directory follows the target).
pub(crate) fn allow_session_assets(app: &tauri::AppHandle, wb: &Workbench, session: &str) {
    let state = app.state::<AppState>();
    let scope = app.asset_protocol_scope();
    for dir in not_yet_allowed(&state.asset_dirs, session_asset_dirs(wb, session)) {
        if let Err(e) = scope.allow_directory(&dir, true) {
            tracing::warn!("asset scope: {}: {e}", dir.display());
        }
    }
}

/// Calls `widen` whenever an event of the session may have added asset folders: the core
/// announced a deck created or removed (`decks_changed`, whoever created it), or this watcher
/// fell behind (`resync`). Ends when the session's event hub does.
pub(crate) fn watch_asset_events(
    rt: &analyst_core::service::SessionRuntime,
    widen: impl Fn() + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    let mut rx = rt.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            if matches!(
                event,
                SessionEvent::DecksChanged { .. } | SessionEvent::Resync
            ) {
                widen();
            }
        }
    })
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

#[cfg(test)]
mod tests {
    use super::{deck_asset_dirs, figure_asset_dirs};
    use std::path::{Path, PathBuf};

    /// A deck created outside the Decks panel (here through the core, as the agent's tool does)
    /// widens the asset scope at once through the `decks_changed` watcher, without waiting for
    /// the next `decks_list`.
    #[tokio::test]
    async fn a_deck_created_by_the_core_is_added_to_the_asset_scope() {
        use analyst_core::service::{Workbench, WorkbenchOptions};
        use std::collections::HashSet;
        use std::sync::{Arc, Mutex};

        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let mut opts = WorkbenchOptions::new(home.path().to_path_buf());
        opts.autostart_kernel = false;
        opts.init = analyst_core::init::InitOptions {
            run_uv: false,
            git: true,
        };
        let wb = Arc::new(Workbench::new(opts).unwrap());
        let info = wb
            .create_session(&repo.path().to_string_lossy(), None, None, true)
            .await
            .unwrap();
        let session = info.summary.id.clone();
        let rt = wb.session(&session).unwrap();

        // What the shell does on session_create: allow what exists, then watch.
        let allowed: Arc<Mutex<HashSet<PathBuf>>> = Arc::default();
        let first = super::not_yet_allowed(&allowed, super::session_asset_dirs(&wb, &session));
        assert!(!first.is_empty());
        let deck_assets = rt.repo.join("decks").join("q3").join("assets");
        assert!(!allowed.lock().unwrap().contains(&deck_assets));
        let watcher = {
            let (allowed, wb, session) = (allowed.clone(), wb.clone(), session.clone());
            super::watch_asset_events(&rt, move || {
                super::not_yet_allowed(&allowed, super::session_asset_dirs(&wb, &session));
            })
        };

        let template = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../present/evals/templates/plain/plain.pptx");
        rt.deck_create(Some("q3"), "Q3", &template.to_string_lossy(), false)
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !allowed.lock().unwrap().contains(&deck_assets) {
            assert!(
                std::time::Instant::now() < deadline,
                "the new deck's folders never reached the scope"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let allowed_now = allowed.lock().unwrap().clone();
        for dir in deck_asset_dirs(&rt.repo, &["q3".into()]) {
            assert!(allowed_now.contains(&dir), "{}", dir.display());
        }
        watcher.abort();
        wb.shutdown().await;
    }

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

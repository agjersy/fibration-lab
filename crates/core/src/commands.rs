//! The one command table of the desktop IPC contract (docs/IPC.md sections 4, 7, 9 and 10).
//!
//! Every transport calls [`dispatch`] with a command name and its JSON arguments: the Tauri
//! shell's invoke handler (`analyst-desktop`) and the development bridge (`analyst-serve`). Each
//! row deserializes its arguments with the exact snake_case names of the contract and returns the
//! result as JSON; every error is an [`IpcError`]. What only a transport can do (native dialogs,
//! the asset protocol scope, opening a file manager) goes through [`Host`]. The two
//! subscription commands are listed here but carried by the transport ([`TRANSPORT_COMMANDS`]),
//! because their event stream is the transport's own (a Tauri channel, a WebSocket).
//!
//! The renderer's command table (`apps/desktop/src/ipc/commands.ts`) is checked against
//! [`COMMANDS`] and [`command_args`] by a test in this module.

use crate::service::{agent_runner, SessionRuntime, Workbench};
use analyst_deck_protocol::deck::{DeckMode, Slide};
use analyst_deck_protocol::ui::IpcError as DeckIpcError;
use analyst_protocol::evals::Mode;
use analyst_protocol::kernel::ArtifactKind;
use analyst_protocol::ui::*;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

type R<T> = Result<T, IpcError>;

/// One filter of `dialog_pick_files`: a label and the file extensions it shows.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct DialogFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

/// What a transport provides beyond the workbench.
#[async_trait::async_trait]
pub trait Host: Send + Sync {
    /// The folders a session's figures and decks live in may have changed (session opened or
    /// created, decks listed or created, figures listed): the shell widens its asset scope.
    fn session_assets(&self, _wb: &Workbench, _session: &str) {}
    /// A session was closed: the transport ends its event subscriptions.
    fn session_closed(&self, _session: &str) {}
    /// Native folder picker; `None` when cancelled.
    async fn pick_directory(&self, title: Option<String>) -> R<Option<String>>;
    /// Native open dialog for files; `None` when cancelled.
    async fn pick_files(
        &self,
        title: Option<String>,
        filters: Vec<DialogFilter>,
        multiple: bool,
    ) -> R<Option<Vec<String>>>;
    /// Show a file in the file manager.
    fn reveal(&self, path: &Path) -> R<()>;
    /// Open a file with its default application.
    fn open(&self, path: &Path) -> R<()>;
}

/// Commands whose result is an event stream owned by the transport.
pub const TRANSPORT_COMMANDS: &[&str] = &["session_subscribe", "session_unsubscribe"];

fn rt_of(wb: &Workbench, id: &str) -> R<Arc<SessionRuntime>> {
    wb.session(id).map_err(IpcError::from)
}

async fn deck_of(
    wb: &Workbench,
    session: &str,
    deck: &str,
) -> R<Arc<crate::service::decks::DeckService>> {
    rt_of(wb, session)?.deck(deck).await.map_err(IpcError::from)
}

/// A deck service error as an IPC error (same code, message, and detail).
fn de(e: DeckIpcError) -> IpcError {
    crate::service::ServiceError::from(e).into()
}

fn parse_mode(mode: &str) -> R<Mode> {
    mode.parse::<Mode>().map_err(IpcError::invalid)
}

macro_rules! table {
    (
        context: $wb:ident, $host:ident;
        transport: [$($t:literal),*];
        $( $name:ident ( $( $arg:ident : $ty:ty ),* $(,)? ) $body:block )*
    ) => {
        /// Every command name of the contract, in table order.
        pub const COMMANDS: &[&str] = &[ $($t,)* $( stringify!($name) ),* ];

        /// The argument names of each command (transport commands excluded), as the renderer
        /// must send them.
        pub fn command_args() -> Vec<(&'static str, Vec<&'static str>)> {
            vec![ $( (stringify!($name), vec![ $( stringify!($arg) ),* ]) ),* ]
        }

        /// Run one command. Unknown names are `not_found`; arguments that do not match the
        /// row are `invalid`. `null` arguments count as `{}`.
        pub async fn dispatch(
            $wb: &Arc<Workbench>,
            $host: &dyn Host,
            cmd: &str,
            args: Value,
        ) -> R<Value> {
            let args = if args.is_null() { Value::Object(Default::default()) } else { args };
            match cmd {
                $( stringify!($name) => {
                    #[derive(serde::Deserialize)]
                    struct Args { $( $arg: $ty ),* }
                    let Args { $( $arg ),* } = serde_json::from_value(args).map_err(|e| {
                        IpcError::invalid(format!("{}: {e}", stringify!($name)))
                    })?;
                    let out = $body;
                    serde_json::to_value(out).map_err(|e| IpcError::internal(e.to_string()))
                } )*
                $( $t )|* => Err(IpcError::invalid(format!(
                    "`{cmd}` is carried by the transport, not the command table"
                ))),
                _ => Err(IpcError::not_found(format!("unknown command `{cmd}`"))),
            }
        }
    };
}

table! {
    context: wb, host;
    transport: ["session_subscribe", "session_unsubscribe"];

    // ---- app and sessions ----
    app_info() { wb.app_info() }
    sessions_list() { wb.sessions_list()? }
    session_create(repo_abs: String, target: Option<String>, model: Option<String>, init: Option<bool>) {
        let info = wb
            .create_session(&repo_abs, target.as_deref(), model.as_deref(), init.unwrap_or(false))
            .await?;
        host.session_assets(wb, &info.summary.id);
        info
    }
    session_open(session: String) {
        let info = wb.open_session(&session).await?;
        host.session_assets(wb, &session);
        info
    }
    session_close(session: String) {
        host.session_closed(&session);
        wb.close_session(&session).await?
    }
    session_info(session: String) { wb.session_info(&session).await? }
    session_rename(session: String, name: String) { wb.rename_session(&session, &name)? }
    session_set_target(session: String, target: String) { rt_of(wb, &session)?.set_target_task(&target)? }
    session_set_policy(session: String, policy: PolicyPatch) {
        let rt = rt_of(wb, &session)?;
        let view = rt.set_policy(&policy)?;
        rt.emit_session_changed().await;
        view
    }
    layout_get(session: String) { wb.layout_get(&session)? }
    layout_save(session: String, layout: String) { wb.layout_save(&session, &layout)? }

    // ---- filesystem ----
    fs_tree(session: String, path: Option<String>, depth: Option<u32>) {
        rt_of(wb, &session)?.fs_tree(path.as_deref(), depth)?
    }
    fs_read(session: String, path: String) { rt_of(wb, &session)?.fs_read(&path)? }
    fs_read_binary(session: String, path: String) { rt_of(wb, &session)?.fs_read_binary(&path)? }
    fs_write(session: String, path: String, content: String, expected_hash: Option<String>) {
        rt_of(wb, &session)?.fs_write(&path, &content, expected_hash.as_deref())?
    }
    fs_create(session: String, path: String, is_dir: bool) { rt_of(wb, &session)?.fs_create(&path, is_dir)? }
    fs_delete(session: String, path: String) { rt_of(wb, &session)?.fs_delete(&path)? }
    fs_rename(session: String, from: String, to: String) { rt_of(wb, &session)?.fs_rename(&from, &to)? }
    fs_search(session: String, pattern: String, glob: Option<String>, max_results: Option<u32>) {
        rt_of(wb, &session)?.fs_search(&pattern, glob.as_deref(), max_results).await?
    }

    // ---- index, checks, runs ----
    index_get(session: String) { rt_of(wb, &session)?.index_get().await? }
    check_run(session: String, paths: Option<Vec<String>>) { rt_of(wb, &session)?.check_run(paths).await? }
    run_start(session: String, script: String, args: Vec<String>) {
        rt_of(wb, &session)?.run_start(&script, args).await?
    }
    run_cancel(session: String, run_id: String) { rt_of(wb, &session)?.run_cancel(&run_id).await? }
    runs_list(session: String, limit: Option<u32>) { rt_of(wb, &session)?.runs_list(limit) }
    run_get(session: String, run_id: String) {
        let rt = rt_of(wb, &session)?;
        // Reads up to 2 MiB per stream from the run's log files.
        tokio::task::spawn_blocking(move || rt.run_get(&run_id))
            .await
            .map_err(|e| IpcError::internal(e.to_string()))??
    }
    out_list(session: String, path: Option<String>, run_id: Option<String>) {
        rt_of(wb, &session)?.out_list(path, run_id).await?
    }
    out_read(session: String, path: String, max_bytes: Option<u64>) {
        rt_of(wb, &session)?.out_read(path, max_bytes).await?
    }

    // ---- figures and equations ----
    figures_list(session: String, limit: Option<u32>) {
        let groups = rt_of(wb, &session)?.figures_list(limit).await;
        // The figures carry `file_abs`; their folders must be in the asset scope.
        host.session_assets(wb, &session);
        groups
    }
    figure_data(session: String, path: String) { rt_of(wb, &session)?.figure_data(&path).await? }
    eq_extract(session: String, path: String) { rt_of(wb, &session)?.eq_extract(&path).await? }
    eq_check(session: String, path: String, function: String) {
        rt_of(wb, &session)?.eq_check(&path, &function).await?
    }

    // ---- environment ----
    env_packages(session: String) { rt_of(wb, &session)?.env_packages().await? }
    env_add(session: String, packages: Vec<String>) { rt_of(wb, &session)?.env_task("add", packages)? }
    env_remove(session: String, packages: Vec<String>) { rt_of(wb, &session)?.env_task("remove", packages)? }
    env_sync(session: String) { rt_of(wb, &session)?.env_task("sync", vec![])? }

    // ---- conversations and chat ----
    conversation_new(session: String, title: Option<String>) {
        let rt = rt_of(wb, &session)?;
        let c = rt.conversation_new(title.as_deref())?;
        rt.emit_session_changed().await;
        c
    }
    conversation_select(session: String, conversation: String) {
        let rt = rt_of(wb, &session)?;
        rt.conversation_select(&conversation)?;
        rt.emit_session_changed().await;
    }
    conversation_transcript(session: String, conversation: String) {
        rt_of(wb, &session)?.conversation_transcript(&conversation)?
    }
    chat_send(session: String, conversation: String, text: String, attachments: Option<Vec<Attachment>>) {
        let rt = rt_of(wb, &session)?;
        let conv = rt.conversation_runtime(&conversation)?;
        agent_runner::start_turn(rt, conv, &text, attachments.unwrap_or_default()).await?
    }
    chat_cancel(session: String, conversation: String) {
        let rt = rt_of(wb, &session)?;
        let conv = rt.conversation_runtime(&conversation)?;
        agent_runner::cancel_turn(&rt, &conv).await;
    }
    chat_set_mode(session: String, conversation: String, mode: String) {
        let mode = parse_mode(&mode)?;
        rt_of(wb, &session)?.chat_set_mode(&conversation, mode).await?
    }
    chat_set_model(session: String, conversation: String, model: String) {
        rt_of(wb, &session)?.chat_set_model(&conversation, &model).await?
    }
    proposal_respond(session: String, proposal_id: String, decision: ProposalDecision) {
        rt_of(wb, &session)?.proposal_respond(&proposal_id, decision)?
    }

    // ---- models and targets ----
    models_list() {
        let _ = wb.reload_global();
        wb.models_list()
    }
    model_probe(model: String) { wb.model_probe(&model).await? }
    // One row per configured provider; key values never cross the boundary.
    provider_keys_status() { wb.provider_keys_status() }
    // Store (or, with an empty key, remove) a provider's key in `$ANALYST_HOME/.env`.
    provider_key_set(provider: String, key: String) { wb.provider_key_set(&provider, &key)? }
    targets_list() {
        let _ = wb.reload_global();
        wb.targets_list()
    }
    target_bootstrap(target: String) { wb.target_bootstrap(&target)? }

    // ---- terminals ----
    terminal_open(session: String, cols: u16, rows: u16) { wb.terminal_open(&session, cols, rows).await? }
    terminal_write(terminal_id: String, data: String) { wb.terminal_write(&terminal_id, &data)? }
    terminal_resize(terminal_id: String, cols: u16, rows: u16) {
        wb.terminal_resize(&terminal_id, cols, rows)?
    }
    terminal_close(terminal_id: String) { wb.terminal_close(&terminal_id)? }

    // ---- dialogs (the transport's) ----
    dialog_pick_directory(title: Option<String>) { host.pick_directory(title).await? }
    dialog_pick_files(title: Option<String>, filters: Option<Vec<DialogFilter>>, multiple: Option<bool>) {
        host.pick_files(title, filters.unwrap_or_default(), multiple.unwrap_or(false)).await?
    }

    // ---- decks (section 9): thin glue over the per-deck service of a session ----
    decks_list(session: String) {
        let list = rt_of(wb, &session)?.decks_list();
        host.session_assets(wb, &session);
        list
    }
    deck_create(session: String, title: String, template_abs: String, slug: Option<String>, code: Option<bool>) {
        let summary = rt_of(wb, &session)?
            .deck_create(slug.as_deref(), &title, &template_abs, code.unwrap_or(false))?;
        host.session_assets(wb, &session);
        summary
    }
    deck_open(session: String, deck: String) { deck_of(wb, &session, &deck).await?.info().await.map_err(de)? }
    template_thumbnails(session: String, deck: String) {
        deck_of(wb, &session, &deck).await?.template_thumbnails().await.map_err(de)?
    }
    template_replace(session: String, deck: String, template_abs: String) {
        deck_of(wb, &session, &deck).await?.template_replace(template_abs)
    }
    deck_get(session: String, deck: String) { deck_of(wb, &session, &deck).await?.deck_get().map_err(de)? }
    slide_update(session: String, deck: String, slide: Slide, expected_hash: Option<String>) {
        deck_of(wb, &session, &deck).await?.slide_update(slide, expected_hash.as_deref()).map_err(de)?
    }
    slide_create(session: String, deck: String, after: Option<String>, layout: String) {
        deck_of(wb, &session, &deck).await?.slide_create(after.as_deref(), &layout).await.map_err(de)?
    }
    slide_delete(session: String, deck: String, slide_id: String) {
        deck_of(wb, &session, &deck).await?.slide_delete(&slide_id).map_err(de)?
    }
    slide_duplicate(session: String, deck: String, slide_id: String) {
        deck_of(wb, &session, &deck).await?.slide_duplicate(&slide_id).map_err(de)?
    }
    slide_move(session: String, deck: String, slide_id: String, after: Option<String>, section: Option<String>) {
        deck_of(wb, &session, &deck)
            .await?
            .slide_move(&slide_id, after.as_deref(), section.as_deref())
            .map_err(de)?
    }
    slide_lock(session: String, deck: String, slide_id: String, element_id: Option<String>, locked: bool) {
        deck_of(wb, &session, &deck)
            .await?
            .slide_lock(&slide_id, element_id.as_deref(), locked)
            .map_err(de)?
    }
    // The user places a run's figure on a slide: the core function of `slide.place_figure`.
    slide_place_figure(session: String, deck: String, slide_id: String, run_id: String, artifact: String, placeholder: Option<String>) {
        rt_of(wb, &session)?
            .slide_place_figure(&deck, &slide_id, &run_id, &artifact, placeholder.as_deref())
            .await?
    }
    // The user places a function's equation on a slide: the core function of `slide.place_equation`.
    slide_place_equation(session: String, deck: String, slide_id: String, path: String, function: String, placeholder: Option<String>) {
        rt_of(wb, &session)?
            .slide_place_equation(&deck, &slide_id, &path, &function, placeholder.as_deref())
            .await?
    }
    slide_preview_png(session: String, deck: String, slide_id: String, png_base64: String) {
        deck_of(wb, &session, &deck).await?.slide_preview_png(&slide_id, &png_base64).map_err(de)?
    }
    chart_compute(session: String, deck: String, source: Value, steps: Option<Value>) {
        deck_of(wb, &session, &deck)
            .await?
            .chart_compute(source, steps.unwrap_or(Value::Null))
            .await
            .map_err(de)?
    }
    deck_code_read(session: String, deck: String, path: String) {
        deck_of(wb, &session, &deck).await?.code_read(&path).map_err(de)?
    }
    deck_code_write(session: String, deck: String, path: String, content: String, expected_hash: Option<String>) {
        deck_of(wb, &session, &deck)
            .await?
            .code_write(&path, &content, expected_hash.as_deref())
            .map_err(de)?
    }
    deck_build(session: String, deck: String) { deck_of(wb, &session, &deck).await?.deck_build() }
    deck_switch_mode(session: String, deck: String, to: DeckMode, via: Option<String>) {
        deck_of(wb, &session, &deck).await?.deck_switch_mode(to, via)
    }
    render_fidelity(session: String, deck: String, slides: Option<Vec<String>>) {
        // The engine renders the whole deck in one pass; `slides` only says which thumbnails
        // the window is waiting for.
        let _ = slides;
        let svc = deck_of(wb, &session, &deck).await?;
        svc.spawn_task("Rendering slides", |rt| async move {
            let n = rt.render_thumbnails().await?;
            Ok(Some(format!("{n} slides rendered")))
        })
    }
    deck_check_run(session: String, deck: String, slides: Option<Vec<String>>) {
        deck_of(wb, &session, &deck).await?.run_checks(slides).await.map_err(de)?
    }
    refs_list(session: String, deck: String) { deck_of(wb, &session, &deck).await?.refs_list().map_err(de)? }
    refs_add(session: String, deck: String, paths_abs: Vec<String>) {
        deck_of(wb, &session, &deck).await?.refs_add(paths_abs)
    }
    refs_remove(session: String, deck: String, ref_id: String) {
        deck_of(wb, &session, &deck).await?.refs_remove(&ref_id).map_err(de)?
    }
    refs_remove_unused(session: String, deck: String) {
        deck_of(wb, &session, &deck).await?.refs_remove_unused().map_err(de)?
    }
    refs_search(session: String, deck: String, query: String, ref_ids: Option<Vec<String>>) {
        deck_of(wb, &session, &deck).await?.refs_search(&query, ref_ids).await.map_err(de)?
    }
    source_passage(session: String, deck: String, cite: String) {
        deck_of(wb, &session, &deck).await?.source_passage(&cite).await.map_err(de)?
    }
    assets_list(session: String, deck: String) { deck_of(wb, &session, &deck).await?.assets_list().map_err(de)? }
    asset_add(session: String, deck: String, path_abs: String) {
        deck_of(wb, &session, &deck).await?.asset_add(&path_abs).map_err(de)?
    }
    exports_list(session: String, deck: String) { deck_of(wb, &session, &deck).await?.exports_list() }
    export_run(session: String, deck: String, format: String) {
        deck_of(wb, &session, &deck).await?.export_run(format)
    }
    export_reveal(session: String, deck: String, export_id: String) {
        let path: PathBuf = deck_of(wb, &session, &deck).await?.export_path(&export_id).map_err(de)?;
        host.reveal(&path)?
    }
    export_open(session: String, deck: String, export_id: String) {
        let path: PathBuf = deck_of(wb, &session, &deck).await?.export_path(&export_id).map_err(de)?;
        host.open(&path)?
    }

    // ---- history: one undo timeline (git checkpoints) for code and decks ----
    history_list(session: String, limit: Option<u32>) { rt_of(wb, &session)?.history_list(limit)? }
    history_restore(session: String, checkpoint: String) {
        rt_of(wb, &session)?.history_restore(&checkpoint).await?
    }
    history_revert_turn(session: String, turn: String) {
        rt_of(wb, &session)?.history_revert_turn(&turn).await?
    }

    // ---- artifact index: what runs produced; slides cite runs from it ----
    artifacts_list(session: String, run_id: Option<String>, kind: Option<ArtifactKind>, limit: Option<u32>) {
        rt_of(wb, &session)?.artifacts_list(run_id.as_deref(), kind, limit)
    }

    // ---- verification ledger (section 10) ----
    verification_list(session: String) { rt_of(wb, &session)?.verification_list() }
    verification_set(session: String, item: VerifyItem, state: VerifyState, note: Option<String>) {
        rt_of(wb, &session)?.verification_set(&item, state, note.as_deref())?
    }
    deck_verification_gaps(session: String, deck: String) {
        rt_of(wb, &session)?.deck_verification_gaps(&deck)?
    }

    // ---- refresh from analysis (section 10) ----
    slide_refresh(session: String, deck: String, slide_id: String, element_id: Option<String>) {
        let rt = rt_of(wb, &session)?;
        crate::service::refresh::refresh_slide(&rt, &deck, &slide_id, element_id.as_deref()).await?
    }
    deck_refresh_stale(session: String, deck: String) {
        let rt = rt_of(wb, &session)?;
        crate::service::refresh::refresh_stale(&rt, &deck).await?
    }
}

#[cfg(test)]
mod tests {
    use super::{command_args, COMMANDS, TRANSPORT_COMMANDS};
    use std::collections::BTreeMap;

    /// The renderer's command table, `name -> argument names`, read from the TypeScript source.
    /// `S` stands for `{ session }` and `D` for `{ session, deck }`.
    fn renderer_table() -> BTreeMap<String, Vec<String>> {
        let src = include_str!("../../../apps/desktop/src/ipc/commands.ts");
        let start = src
            .find("export interface Commands {")
            .expect("Commands interface");
        let body = &src[start..];
        let mut out = BTreeMap::new();
        for line in body.lines().skip(1) {
            let line = line.trim();
            if line == "}" {
                break;
            }
            let Some((name, rest)) = line.split_once(": { args: ") else {
                continue;
            };
            let args_src = rest
                .rsplit_once("; result:")
                .map(|(a, _)| a)
                .expect("result type");
            let mut args = Vec::new();
            for part in args_src.split(" & ") {
                let part = part.trim();
                match part {
                    "S" => args.push("session".to_string()),
                    "D" => args.extend(["session".to_string(), "deck".to_string()]),
                    "Record<string, never>" => {}
                    p if p.starts_with('{') => args.extend(top_level_keys(p)),
                    p => panic!("{name}: unexpected argument type `{p}`"),
                }
            }
            out.insert(name.to_string(), args);
        }
        out
    }

    /// The keys of a TypeScript object type literal, ignoring nested object types.
    fn top_level_keys(src: &str) -> Vec<String> {
        let inner = &src[1..src.len() - 1];
        let mut keys = Vec::new();
        let mut depth = 0;
        let mut field = String::new();
        let mut fields = Vec::new();
        for c in inner.chars() {
            match c {
                '{' | '[' | '(' | '<' => depth += 1,
                '}' | ']' | ')' | '>' => depth -= 1,
                ';' if depth == 0 => {
                    fields.push(std::mem::take(&mut field));
                    continue;
                }
                _ => {}
            }
            field.push(c);
        }
        fields.push(field);
        for f in fields {
            if let Some((k, _)) = f.split_once(':') {
                let k = k.trim().trim_end_matches('?');
                if !k.is_empty() {
                    keys.push(k.to_string());
                }
            }
        }
        keys
    }

    #[test]
    fn the_renderer_command_table_matches_this_one() {
        let ts = renderer_table();
        let mut rust: BTreeMap<String, Vec<String>> = command_args()
            .into_iter()
            .map(|(n, a)| (n.to_string(), a.into_iter().map(String::from).collect()))
            .collect();
        for t in TRANSPORT_COMMANDS {
            assert!(ts.contains_key(*t), "commands.ts lacks `{t}`");
        }
        rust.extend(
            TRANSPORT_COMMANDS
                .iter()
                .map(|t| (t.to_string(), ts[*t].clone())),
        );
        let ts_names: Vec<&String> = ts.keys().collect();
        let rust_names: Vec<&String> = rust.keys().collect();
        assert_eq!(
            ts_names, rust_names,
            "commands.ts and the Rust table name different commands"
        );
        for (name, args) in &rust {
            let mut a = args.clone();
            let mut b = ts[name].clone();
            a.sort();
            b.sort();
            assert_eq!(
                a, b,
                "`{name}`: argument names differ between Rust and commands.ts"
            );
        }
        assert_eq!(COMMANDS.len(), rust.len(), "duplicate command names");
    }

    #[test]
    fn top_level_keys_skip_nested_objects() {
        assert_eq!(
            top_level_keys("{ title?: string; filters?: { name: string; extensions: string[] }[]; multiple?: boolean }"),
            vec!["title", "filters", "multiple"]
        );
    }
}

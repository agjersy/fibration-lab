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
//! The renderer's command table (`apps/desktop/src/ipc/commands.ts`) is generated from this one
//! ([`commands_ts`]): argument names and types and result types come from the rows, the types'
//! names from their ts-rs bindings. A test in this module writes it and fails when the
//! committed file was stale.

use crate::service::{agent_runner, SessionRuntime, Workbench};
use analyst_deck_protocol::deck::{DeckMode, Slide};
use analyst_deck_protocol::kernel::LayoutThumbnail;
use analyst_deck_protocol::ui::IpcError as DeckIpcError;
use analyst_deck_protocol::ui::{
    AssetView, DeckInfo, DeckView, ExportView, RefView, SlideView, SourcePassage,
};
use analyst_protocol::evals::Mode;
use analyst_protocol::kernel::{
    ArtifactKind, Diagnostic, Equation, OutListResult, OutReadResult, PackageInfo,
};
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

/// A JSON value the core passes through unchanged, typed for the renderer as `T`.
pub struct Json<T> {
    value: Value,
    _t: std::marker::PhantomData<T>,
}

impl<T> Json<T> {
    fn new(v: impl serde::Serialize) -> Json<T> {
        Json {
            value: serde_json::to_value(v).unwrap_or(Value::Null),
            _t: std::marker::PhantomData,
        }
    }
}

impl<T> serde::Serialize for Json<T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.value.serialize(s)
    }
}

/// TypeScript names of the types in the table, for the generated `commands.ts`: the ts-rs name
/// for the contract's types (deck types in the renderer's `Deck` namespace), and the obvious
/// spelling for the rest. A row whose type has no name here does not compile.
pub mod ts {
    use ts_rs::TS;

    pub trait TsType {
        fn ts() -> String;
        /// As an argument: the type and whether it may be left out (`Option<T>` is an
        /// optional `T`).
        fn ts_arg() -> (String, bool) {
            (Self::ts(), false)
        }
    }

    impl<T: TsType> TsType for Option<T> {
        fn ts() -> String {
            format!("{} | null", T::ts())
        }
        fn ts_arg() -> (String, bool) {
            (T::ts(), true)
        }
    }

    impl<T: TsType> TsType for Vec<T> {
        fn ts() -> String {
            let inner = T::ts();
            if inner.contains(' ') && !inner.starts_with('{') {
                format!("({inner})[]")
            } else {
                format!("{inner}[]")
            }
        }
    }

    impl<T: TsType> TsType for super::Json<T> {
        fn ts() -> String {
            T::ts()
        }
    }

    macro_rules! plain {
        ($($t:ty => $s:expr),* $(,)?) => { $( impl TsType for $t { fn ts() -> String { $s.to_string() } } )* };
    }
    plain! {
        () => "null",
        String => "string",
        bool => "boolean",
        u16 => "number",
        u32 => "number",
        u64 => "number",
        serde_json::Value => "unknown",
        super::DialogFilter => "{ name: string; extensions: string[] }",
    }

    fn cfg() -> ts_rs::Config {
        ts_rs::Config::new().with_large_int("number")
    }

    macro_rules! analysis {
        ($($t:ty),* $(,)?) => { $( impl TsType for $t { fn ts() -> String { <$t as TS>::name(&cfg()) } } )* };
    }
    macro_rules! deck {
        ($($t:ty),* $(,)?) => { $( impl TsType for $t { fn ts() -> String { format!("Deck.{}", <$t as TS>::name(&cfg())) } } )* };
    }
    use analyst_protocol::kernel as k;
    use analyst_protocol::ui as u;
    analysis! {
        u::AppInfo, u::SessionSummary, u::SessionInfo, u::PolicyView, u::PolicyPatch, u::FileEntry,
        u::FileContent, u::FigureData, u::WriteResult, u::SearchHit, u::IndexResult, k::Diagnostic,
        u::RunView, u::RunDetail, k::OutListResult, k::OutReadResult, u::FigureGroup, k::Equation,
        k::EqCheckResult, k::PackageInfo, u::ConversationSummary, analyst_protocol::transcript::Event,
        u::Attachment, u::ProposalDecision, u::ModelEntry, u::TargetInfo, u::TerminalOpened,
        u::ProviderKeyStatus, u::DeckSummary, u::CheckpointView, u::ArtifactEntry, k::ArtifactKind,
        u::VerificationView, u::VerifyItem, u::VerifyState, u::ProposalView,
    }
    use analyst_deck_protocol as d;
    deck! {
        d::ui::AssetView, d::ui::DeckInfo, d::ui::DeckView, d::ui::SlideView, d::ui::ExportView,
        d::ui::RefView, d::ui::SourcePassage, d::kernel::LayoutThumbnail, d::ui::FileContent,
        d::ui::WriteResult, d::ui::SearchHit, d::kernel::Diagnostic, d::kernel::ChartComputeResult,
        d::deck::Slide, d::deck::DeckMode,
    }
}

type DeckDiagnostic = analyst_deck_protocol::kernel::Diagnostic;
type DeckFileContent = analyst_deck_protocol::ui::FileContent;
type DeckWriteResult = analyst_deck_protocol::ui::WriteResult;
type DeckSearchHit = analyst_deck_protocol::ui::SearchHit;
use analyst_deck_protocol::kernel::ChartComputeResult;

/// One row of the renderer's command table: name, arguments (name, TypeScript type, optional),
/// and the result's TypeScript type.
pub struct TsRow {
    pub name: &'static str,
    pub args: Vec<(&'static str, String, bool)>,
    pub result: String,
}

/// Rows of the commands the transports carry ([`TRANSPORT_COMMANDS`]).
fn transport_rows() -> Vec<TsRow> {
    vec![
        TsRow {
            name: "session_subscribe",
            args: vec![
                ("session", "string".into(), false),
                ("channel", "unknown".into(), false),
            ],
            result: "string".into(),
        },
        TsRow {
            name: "session_unsubscribe",
            args: vec![("subscription", "string".into(), false)],
            result: "null".into(),
        },
    ]
}

/// The renderer's `apps/desktop/src/ipc/commands.ts`, generated from the table. Arguments are
/// written as `S` (`{ session }`) and `D` (`{ session, deck }`) where they start that way.
pub fn commands_ts() -> String {
    let mut rows = transport_rows();
    rows.extend(typescript_table());
    let mut order: Vec<&'static str> = Vec::new();
    for name in COMMANDS {
        order.push(name);
    }
    rows.sort_by_key(|r| {
        order
            .iter()
            .position(|n| *n == r.name)
            .unwrap_or(usize::MAX)
    });
    let mut names = std::collections::BTreeSet::new();
    let mut lines = Vec::new();
    for r in &rows {
        let mut parts: Vec<String> = Vec::new();
        let mut rest: &[(&str, String, bool)] = &r.args;
        let is_session = |a: &(&str, String, bool)| a.0 == "session" && a.1 == "string" && !a.2;
        if rest.first().is_some_and(is_session) {
            if rest
                .get(1)
                .is_some_and(|a| a.0 == "deck" && a.1 == "string" && !a.2)
            {
                parts.push("D".into());
                rest = &rest[2..];
            } else {
                parts.push("S".into());
                rest = &rest[1..];
            }
        }
        if !rest.is_empty() {
            let fields: Vec<String> = rest
                .iter()
                .map(|(n, t, opt)| format!("{n}{}: {t}", if *opt { "?" } else { "" }))
                .collect();
            parts.push(format!("{{ {} }}", fields.join("; ")));
        }
        let args = if parts.is_empty() {
            "Record<string, never>".to_string()
        } else {
            parts.join(" & ")
        };
        for t in r
            .args
            .iter()
            .map(|a| a.1.as_str())
            .chain([r.result.as_str()])
        {
            collect_names(t, &mut names);
        }
        lines.push(format!(
            "  {}: {{ args: {args}; result: {} }};",
            r.name, r.result
        ));
    }
    let mut out = String::new();
    out.push_str("// Generated from the command table in crates/core/src/commands.rs by\n");
    out.push_str("// `cargo test -p analyst-core --lib commands` (do not edit; a test fails when it is stale).\n");
    out.push_str(
        "// Argument names are exact (snake_case); see docs/IPC.md sections 4, 7, 9 and 10.\n",
    );
    let imports: Vec<String> = names.into_iter().collect();
    out.push_str("import type {\n");
    for n in &imports {
        out.push_str(&format!("  {n},\n"));
    }
    out.push_str("} from \"./types\";\n\n");
    out.push_str("type S = { session: string };\n");
    out.push_str("/** A deck command: the session and the deck's slug under `decks/`. */\n");
    out.push_str("type D = S & { deck: string };\n\n");
    out.push_str("export interface Commands {\n");
    for l in lines {
        out.push_str(&l);
        out.push('\n');
    }
    out.push_str("}\n\nexport type CommandName = keyof Commands;\n");
    out
}

/// The imported type names of a TypeScript type expression (`Deck.X` imports `Deck`).
fn collect_names(t: &str, out: &mut std::collections::BTreeSet<String>) {
    let mut word = String::new();
    let flush = |w: &mut String, out: &mut std::collections::BTreeSet<String>| {
        let first = w.split('.').next().unwrap_or("");
        if first.starts_with(|c: char| c.is_ascii_uppercase())
            && !["Record", "Array", "Partial"].contains(&first)
        {
            out.insert(first.to_string());
        }
        w.clear();
    };
    for c in t.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
            word.push(c);
        } else {
            flush(&mut word, out);
        }
    }
    flush(&mut word, out);
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
        $( $name:ident ( $( $arg:ident : $ty:ty ),* $(,)? ) -> $ret:ty $body:block )*
    ) => {
        /// Every command name of the contract, in table order.
        pub const COMMANDS: &[&str] = &[ $($t,)* $( stringify!($name) ),* ];

        /// The argument names of each command (transport commands excluded), as the renderer
        /// must send them.
        pub fn command_args() -> Vec<(&'static str, Vec<&'static str>)> {
            vec![ $( (stringify!($name), vec![ $( stringify!($arg) ),* ]) ),* ]
        }

        /// The rows as TypeScript, for the generated `commands.ts`.
        pub fn typescript_table() -> Vec<TsRow> {
            vec![ $( TsRow {
                name: stringify!($name),
                args: vec![ $( {
                    let (t, optional) = <$ty as ts::TsType>::ts_arg();
                    (stringify!($arg), t, optional)
                } ),* ],
                result: <$ret as ts::TsType>::ts(),
            } ),* ]
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
                    let out: $ret = $body;
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
    app_info() -> AppInfo { wb.app_info() }
    sessions_list() -> Vec<SessionSummary> { wb.sessions_list()? }
    session_create(repo_abs: String, target: Option<String>, model: Option<String>, init: Option<bool>) -> SessionInfo {
        let info = wb
            .create_session(&repo_abs, target.as_deref(), model.as_deref(), init.unwrap_or(false))
            .await?;
        host.session_assets(wb, &info.summary.id);
        info
    }
    session_open(session: String) -> SessionInfo {
        let info = wb.open_session(&session).await?;
        host.session_assets(wb, &session);
        info
    }
    session_close(session: String) -> () {
        host.session_closed(&session);
        wb.close_session(&session).await?
    }
    session_info(session: String) -> SessionInfo { wb.session_info(&session).await? }
    session_rename(session: String, name: String) -> SessionSummary { wb.rename_session(&session, &name)? }
    session_set_target(session: String, target: String) -> String { rt_of(wb, &session)?.set_target_task(&target)? }
    session_set_policy(session: String, policy: PolicyPatch) -> PolicyView {
        let rt = rt_of(wb, &session)?;
        let view = rt.set_policy(&policy)?;
        rt.emit_session_changed().await;
        view
    }
    layout_get(session: String) -> Option<String> { wb.layout_get(&session)? }
    layout_save(session: String, layout: String) -> () { wb.layout_save(&session, &layout)? }

    // ---- filesystem ----
    fs_tree(session: String, path: Option<String>, depth: Option<u32>) -> Vec<FileEntry> {
        rt_of(wb, &session)?.fs_tree(path.as_deref(), depth)?
    }
    fs_read(session: String, path: String) -> FileContent { rt_of(wb, &session)?.fs_read(&path)? }
    fs_read_binary(session: String, path: String) -> FigureData { rt_of(wb, &session)?.fs_read_binary(&path)? }
    fs_write(session: String, path: String, content: String, expected_hash: Option<String>) -> WriteResult {
        rt_of(wb, &session)?.fs_write(&path, &content, expected_hash.as_deref())?
    }
    fs_create(session: String, path: String, is_dir: bool) -> () { rt_of(wb, &session)?.fs_create(&path, is_dir)? }
    fs_delete(session: String, path: String) -> () { rt_of(wb, &session)?.fs_delete(&path)? }
    fs_rename(session: String, from: String, to: String) -> () { rt_of(wb, &session)?.fs_rename(&from, &to)? }
    fs_search(session: String, pattern: String, glob: Option<String>, max_results: Option<u32>) -> Vec<SearchHit> {
        rt_of(wb, &session)?.fs_search(&pattern, glob.as_deref(), max_results).await?
    }

    // ---- index, checks, runs ----
    index_get(session: String) -> IndexResult { rt_of(wb, &session)?.index_get().await? }
    check_run(session: String, paths: Option<Vec<String>>) -> Vec<Diagnostic> { rt_of(wb, &session)?.check_run(paths).await? }
    run_start(session: String, script: String, args: Vec<String>) -> RunView {
        rt_of(wb, &session)?.run_start(&script, args).await?
    }
    run_cancel(session: String, run_id: String) -> () { rt_of(wb, &session)?.run_cancel(&run_id).await? }
    runs_list(session: String, limit: Option<u32>) -> Vec<RunView> { rt_of(wb, &session)?.runs_list(limit) }
    run_get(session: String, run_id: String) -> RunDetail {
        let rt = rt_of(wb, &session)?;
        // Reads up to 2 MiB per stream from the run's log files.
        tokio::task::spawn_blocking(move || rt.run_get(&run_id))
            .await
            .map_err(|e| IpcError::internal(e.to_string()))??
    }
    out_list(session: String, path: Option<String>, run_id: Option<String>) -> OutListResult {
        rt_of(wb, &session)?.out_list(path, run_id).await?
    }
    out_read(session: String, path: String, max_bytes: Option<u64>) -> OutReadResult {
        rt_of(wb, &session)?.out_read(path, max_bytes).await?
    }

    // ---- figures and equations ----
    figures_list(session: String, limit: Option<u32>) -> Vec<FigureGroup> {
        let groups = rt_of(wb, &session)?.figures_list(limit).await;
        // The figures carry `file_abs`; their folders must be in the asset scope.
        host.session_assets(wb, &session);
        groups
    }
    figure_data(session: String, path: String) -> FigureData { rt_of(wb, &session)?.figure_data(&path).await? }
    eq_extract(session: String, path: String) -> Vec<Equation> { rt_of(wb, &session)?.eq_extract(&path).await? }
    eq_check(session: String, path: String, function: String) -> analyst_protocol::kernel::EqCheckResult {
        rt_of(wb, &session)?.eq_check(&path, &function).await?
    }

    // ---- environment ----
    env_packages(session: String) -> Vec<PackageInfo> { rt_of(wb, &session)?.env_packages().await? }
    env_add(session: String, packages: Vec<String>) -> String { rt_of(wb, &session)?.env_task("add", packages)? }
    env_remove(session: String, packages: Vec<String>) -> String { rt_of(wb, &session)?.env_task("remove", packages)? }
    env_sync(session: String) -> String { rt_of(wb, &session)?.env_task("sync", vec![])? }

    // ---- conversations and chat ----
    conversation_new(session: String, title: Option<String>) -> ConversationSummary {
        let rt = rt_of(wb, &session)?;
        let c = rt.conversation_new(title.as_deref())?;
        rt.emit_session_changed().await;
        c
    }
    conversation_select(session: String, conversation: String) -> () {
        let rt = rt_of(wb, &session)?;
        rt.conversation_select(&conversation)?;
        rt.emit_session_changed().await;
    }
    conversation_transcript(session: String, conversation: String) -> Vec<analyst_protocol::transcript::Event> {
        rt_of(wb, &session)?.conversation_transcript(&conversation)?
    }
    chat_send(session: String, conversation: String, text: String, attachments: Option<Vec<Attachment>>) -> String {
        let rt = rt_of(wb, &session)?;
        let conv = rt.conversation_runtime(&conversation)?;
        agent_runner::start_turn(rt, conv, &text, attachments.unwrap_or_default()).await?
    }
    chat_cancel(session: String, conversation: String) -> () {
        let rt = rt_of(wb, &session)?;
        let conv = rt.conversation_runtime(&conversation)?;
        agent_runner::cancel_turn(&rt, &conv).await;
    }
    chat_set_mode(session: String, conversation: String, mode: String) -> () {
        let mode = parse_mode(&mode)?;
        rt_of(wb, &session)?.chat_set_mode(&conversation, mode).await?
    }
    chat_set_model(session: String, conversation: String, model: String) -> () {
        rt_of(wb, &session)?.chat_set_model(&conversation, &model).await?
    }
    proposal_respond(session: String, proposal_id: String, decision: ProposalDecision) -> () {
        rt_of(wb, &session)?.proposal_respond(&proposal_id, decision)?
    }

    // ---- models and targets ----
    models_list() -> Vec<ModelEntry> {
        let _ = wb.reload_global();
        wb.models_list()
    }
    model_probe(model: String) -> ModelEntry { wb.model_probe(&model).await? }
    // One row per configured provider; key values never cross the boundary.
    provider_keys_status() -> Vec<ProviderKeyStatus> { wb.provider_keys_status() }
    // Store (or, with an empty key, remove) a provider's key in `$ANALYST_HOME/.env`.
    provider_key_set(provider: String, key: String) -> () { wb.provider_key_set(&provider, &key)? }
    targets_list() -> Vec<TargetInfo> {
        let _ = wb.reload_global();
        wb.targets_list()
    }
    target_bootstrap(target: String) -> String { wb.target_bootstrap(&target)? }

    // ---- terminals ----
    terminal_open(session: String, cols: u16, rows: u16) -> TerminalOpened { wb.terminal_open(&session, cols, rows).await? }
    terminal_write(terminal_id: String, data: String) -> () { wb.terminal_write(&terminal_id, &data)? }
    terminal_resize(terminal_id: String, cols: u16, rows: u16) -> () {
        wb.terminal_resize(&terminal_id, cols, rows)?
    }
    terminal_close(terminal_id: String) -> () { wb.terminal_close(&terminal_id)? }

    // ---- dialogs (the transport's) ----
    dialog_pick_directory(title: Option<String>) -> Option<String> { host.pick_directory(title).await? }
    dialog_pick_files(title: Option<String>, filters: Option<Vec<DialogFilter>>, multiple: Option<bool>) -> Option<Vec<String>> {
        host.pick_files(title, filters.unwrap_or_default(), multiple.unwrap_or(false)).await?
    }

    // ---- decks (section 9): thin glue over the per-deck service of a session ----
    decks_list(session: String) -> Vec<DeckSummary> {
        let list = rt_of(wb, &session)?.decks_list();
        host.session_assets(wb, &session);
        list
    }
    deck_create(session: String, title: String, template_abs: String, slug: Option<String>, code: Option<bool>) -> DeckSummary {
        let summary = rt_of(wb, &session)?
            .deck_create(slug.as_deref(), &title, &template_abs, code.unwrap_or(false))?;
        host.session_assets(wb, &session);
        summary
    }
    deck_open(session: String, deck: String) -> DeckInfo {
        let rt = rt_of(wb, &session)?;
        let info = rt.deck(&deck).await.map_err(IpcError::from)?.info().await.map_err(de)?;
        rt.set_active_deck(Some(&deck))?;
        info
    }
    template_thumbnails(session: String, deck: String) -> Vec<LayoutThumbnail> {
        deck_of(wb, &session, &deck).await?.template_thumbnails().await.map_err(de)?
    }
    template_replace(session: String, deck: String, template_abs: String) -> String {
        deck_of(wb, &session, &deck).await?.template_replace(template_abs)
    }
    deck_get(session: String, deck: String) -> DeckView { deck_of(wb, &session, &deck).await?.deck_get().map_err(de)? }
    slide_update(session: String, deck: String, slide: Slide, expected_hash: Option<String>) -> SlideView {
        deck_of(wb, &session, &deck).await?.slide_update(slide, expected_hash.as_deref()).map_err(de)?
    }
    slide_create(session: String, deck: String, after: Option<String>, layout: String) -> SlideView {
        deck_of(wb, &session, &deck).await?.slide_create(after.as_deref(), &layout).await.map_err(de)?
    }
    slide_delete(session: String, deck: String, slide_id: String) -> () {
        deck_of(wb, &session, &deck).await?.slide_delete(&slide_id).map_err(de)?
    }
    slide_duplicate(session: String, deck: String, slide_id: String) -> SlideView {
        deck_of(wb, &session, &deck).await?.slide_duplicate(&slide_id).map_err(de)?
    }
    slide_move(session: String, deck: String, slide_id: String, after: Option<String>, section: Option<String>) -> () {
        deck_of(wb, &session, &deck)
            .await?
            .slide_move(&slide_id, after.as_deref(), section.as_deref())
            .map_err(de)?
    }
    slide_lock(session: String, deck: String, slide_id: String, element_id: Option<String>, locked: bool) -> () {
        deck_of(wb, &session, &deck)
            .await?
            .slide_lock(&slide_id, element_id.as_deref(), locked)
            .map_err(de)?
    }
    // The user places a run's figure on a slide: the core function of `slide.place_figure`.
    slide_place_figure(session: String, deck: String, slide_id: String, run_id: String, artifact: String, placeholder: Option<String>) -> SlideView {
        rt_of(wb, &session)?
            .slide_place_figure(&deck, &slide_id, &run_id, &artifact, placeholder.as_deref())
            .await?
    }
    // The user places a function's equation on a slide: the core function of `slide.place_equation`.
    slide_place_equation(session: String, deck: String, slide_id: String, path: String, function: String, placeholder: Option<String>) -> SlideView {
        rt_of(wb, &session)?
            .slide_place_equation(&deck, &slide_id, &path, &function, placeholder.as_deref())
            .await?
    }
    slide_preview_png(session: String, deck: String, slide_id: String, png_base64: String) -> () {
        deck_of(wb, &session, &deck).await?.slide_preview_png(&slide_id, &png_base64).map_err(de)?
    }
    chart_compute(session: String, deck: String, source: Value, steps: Option<Value>) -> Json<ChartComputeResult> {
        Json::new(
            deck_of(wb, &session, &deck)
                .await?
                .chart_compute(source, steps.unwrap_or(Value::Null))
                .await
                .map_err(de)?,
        )
    }
    deck_code_read(session: String, deck: String, path: String) -> DeckFileContent {
        deck_of(wb, &session, &deck).await?.code_read(&path).map_err(de)?
    }
    deck_code_write(session: String, deck: String, path: String, content: String, expected_hash: Option<String>) -> DeckWriteResult {
        deck_of(wb, &session, &deck)
            .await?
            .code_write(&path, &content, expected_hash.as_deref())
            .map_err(de)?
    }
    deck_build(session: String, deck: String) -> String { deck_of(wb, &session, &deck).await?.deck_build() }
    deck_switch_mode(session: String, deck: String, to: DeckMode, via: Option<String>) -> String {
        deck_of(wb, &session, &deck).await?.deck_switch_mode(to, via)
    }
    render_fidelity(session: String, deck: String, slides: Option<Vec<String>>) -> String {
        // The engine renders the whole deck in one pass; `slides` only says which thumbnails
        // the window is waiting for.
        let _ = slides;
        let svc = deck_of(wb, &session, &deck).await?;
        svc.spawn_task("Rendering slides", |rt| async move {
            let n = rt.render_thumbnails().await?;
            Ok(Some(format!("{n} slides rendered")))
        })
    }
    deck_check_run(session: String, deck: String, slides: Option<Vec<String>>) -> Json<Vec<DeckDiagnostic>> {
        Json::new(deck_of(wb, &session, &deck).await?.run_checks(slides).await.map_err(de)?)
    }
    refs_list(session: String, deck: String) -> Vec<RefView> { deck_of(wb, &session, &deck).await?.refs_list().map_err(de)? }
    refs_add(session: String, deck: String, paths_abs: Vec<String>) -> String {
        deck_of(wb, &session, &deck).await?.refs_add(paths_abs)
    }
    refs_remove(session: String, deck: String, ref_id: String) -> () {
        deck_of(wb, &session, &deck).await?.refs_remove(&ref_id).map_err(de)?
    }
    refs_remove_unused(session: String, deck: String) -> Vec<String> {
        deck_of(wb, &session, &deck).await?.refs_remove_unused().map_err(de)?
    }
    refs_search(session: String, deck: String, query: String, ref_ids: Option<Vec<String>>) -> Vec<DeckSearchHit> {
        deck_of(wb, &session, &deck).await?.refs_search(&query, ref_ids).await.map_err(de)?
    }
    source_passage(session: String, deck: String, cite: String) -> SourcePassage {
        deck_of(wb, &session, &deck).await?.source_passage(&cite).await.map_err(de)?
    }
    assets_list(session: String, deck: String) -> Vec<AssetView> { deck_of(wb, &session, &deck).await?.assets_list().map_err(de)? }
    asset_add(session: String, deck: String, path_abs: String) -> AssetView {
        deck_of(wb, &session, &deck).await?.asset_add(&path_abs).map_err(de)?
    }
    exports_list(session: String, deck: String) -> Vec<ExportView> { deck_of(wb, &session, &deck).await?.exports_list() }
    export_run(session: String, deck: String, format: String) -> String {
        deck_of(wb, &session, &deck).await?.export_run(format)
    }
    export_reveal(session: String, deck: String, export_id: String) -> () {
        let path: PathBuf = deck_of(wb, &session, &deck).await?.export_path(&export_id).map_err(de)?;
        host.reveal(&path)?
    }
    export_open(session: String, deck: String, export_id: String) -> () {
        let path: PathBuf = deck_of(wb, &session, &deck).await?.export_path(&export_id).map_err(de)?;
        host.open(&path)?
    }

    // ---- history: one undo timeline (git checkpoints) for code and decks ----
    history_list(session: String, limit: Option<u32>) -> Vec<CheckpointView> { rt_of(wb, &session)?.history_list(limit)? }
    history_restore(session: String, checkpoint: String) -> () {
        rt_of(wb, &session)?.history_restore(&checkpoint).await?
    }
    history_revert_turn(session: String, turn: String) -> CheckpointView {
        rt_of(wb, &session)?.history_revert_turn(&turn).await?
    }

    // ---- artifact index: what runs produced; slides cite runs from it ----
    artifacts_list(session: String, run_id: Option<String>, kind: Option<ArtifactKind>, limit: Option<u32>) -> Vec<ArtifactEntry> {
        rt_of(wb, &session)?.artifacts_list(run_id.as_deref(), kind, limit)
    }

    // ---- verification ledger (section 10) ----
    verification_list(session: String) -> Vec<VerificationView> { rt_of(wb, &session)?.verification_list() }
    verification_set(session: String, item: VerifyItem, state: VerifyState, note: Option<String>) -> VerificationView {
        rt_of(wb, &session)?.verification_set(&item, state, note.as_deref())?
    }
    deck_verification_gaps(session: String, deck: String) -> Vec<VerificationView> {
        rt_of(wb, &session)?.deck_verification_gaps(&deck)?
    }

    // ---- refresh from analysis (section 10) ----
    slide_refresh(session: String, deck: String, slide_id: String, element_id: Option<String>) -> Option<ProposalView> {
        let rt = rt_of(wb, &session)?;
        crate::service::refresh::refresh_slide(&rt, &deck, &slide_id, element_id.as_deref()).await?
    }
    deck_refresh_stale(session: String, deck: String) -> Vec<ProposalView> {
        let rt = rt_of(wb, &session)?;
        crate::service::refresh::refresh_stale(&rt, &deck).await?
    }
}

#[cfg(test)]
mod tests {
    use super::{commands_ts, COMMANDS, TRANSPORT_COMMANDS};

    /// The renderer's command table is generated from this one. The test writes the file and
    /// fails when what was committed differed, so a stale `commands.ts` fails CI; run the test
    /// locally and commit the result.
    #[test]
    fn the_renderer_command_table_is_generated_from_this_one() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps/desktop/src/ipc/commands.ts");
        let want = commands_ts();
        let had = std::fs::read_to_string(&path).unwrap_or_default();
        if had != want {
            std::fs::write(&path, &want).unwrap();
        }
        assert!(
            had == want,
            "apps/desktop/src/ipc/commands.ts was stale and has been regenerated; commit it"
        );
    }

    #[test]
    fn every_command_has_one_row() {
        let mut names: Vec<&str> = COMMANDS.to_vec();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), COMMANDS.len(), "duplicate command names");
        let ts = commands_ts();
        for name in COMMANDS {
            assert!(ts.contains(&format!("\n  {name}: {{ args: ")), "{name}");
        }
        assert!(TRANSPORT_COMMANDS.iter().all(|t| COMMANDS.contains(t)));
    }
}

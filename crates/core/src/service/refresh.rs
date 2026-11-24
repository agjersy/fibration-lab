//! Refresh from analysis: a slide element whose source changed (a figure or bound number whose
//! run's code changed, an equation whose source file changed) is brought up to date without the
//! agent. The cited script is run again with the cited arguments through the session's run
//! service (policy applies as for any run the user starts), the equation is extracted and
//! checked again, and the result is a normal slide proposal (before/after card, with the
//! changed values and the verifications it invalidates) that the user accepts or rejects like
//! any other; accepting takes a checkpoint first, so undo is the usual history.

use super::error::{ServiceError, ServiceResult};
use super::runtime::SessionRuntime;
use fibration_protocol::kernel::{ArtifactKind, RunStatus};
use fibration_protocol::transcript::{DecidedBy, Decision, ProposalKind};
use fibration_protocol::ui::{
    ProposalDecision, ProposalReason, ProposalView, SessionEvent, VerificationState, VerifyItem,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

/// How long a refresh waits for a re-run.
pub const RERUN_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// What one refresh changed, for the proposal card.
#[derive(Default)]
struct Changes {
    lines: Vec<String>,
    /// Items whose verification the change resets.
    touched: Vec<VerifyItem>,
}

/// Runs made during one refresh, by (script, arguments), so two elements citing the same run
/// re-run it once.
type Reruns = HashMap<(String, Vec<String>), String>;

async fn rerun(
    rt: &SessionRuntime,
    reruns: &mut Reruns,
    script: &str,
    args: &[String],
) -> ServiceResult<String> {
    let key = (script.to_string(), args.to_vec());
    if let Some(id) = reruns.get(&key) {
        return Ok(id.clone());
    }
    let view = rt.run_start(script, args.to_vec()).await?;
    let id = view.meta.run_id.clone();
    let rec = rt
        .runs
        .wait(&id, RERUN_TIMEOUT)
        .await
        .ok_or_else(|| ServiceError::internal(format!("run `{id}` disappeared")))?;
    if !rec.completed {
        return Err(ServiceError::internal(format!(
            "run `{id}` of {script} did not finish in time"
        )));
    }
    if rec.meta.status != RunStatus::Done || rec.meta.exit_code.is_some_and(|c| c != 0) {
        return Err(ServiceError::invalid(format!(
            "the re-run of {script} {} failed (run `{id}`, {:?}); fix the script, then refresh again",
            args.join(" "),
            rec.meta.status
        )));
    }
    reruns.insert(key, id.clone());
    Ok(id)
}

/// The decimal places a number is shown with (`1.823 s` -> 3), if the text shows one.
fn shown_decimals(text: &str) -> Option<usize> {
    let start = text.find(|c: char| c.is_ascii_digit())?;
    let num: String = text[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
        .collect();
    Some(num.split_once('.').map(|(_, d)| d.len()).unwrap_or(0))
}

fn format_result(r: &fibration_protocol::kernel::NamedResult, decimals: Option<usize>) -> String {
    let n = super::provenance::format_value(r.value, decimals);
    match r.unit.as_deref().filter(|u| !u.is_empty()) {
        Some("%") => format!("{n}%"),
        Some(u) => format!("{n} {u}"),
        None => n,
    }
}

#[allow(clippy::too_many_arguments)]
/// Bring one element up to date in `el` (a placeholder content or a free element's content).
async fn refresh_element(
    rt: &SessionRuntime,
    svc: &fibration_deck::service::DeckService,
    reruns: &mut Reruns,
    tree: Option<&str>,
    deck: &str,
    slide_id: &str,
    el: &mut Value,
    changes: &mut Changes,
) -> ServiceResult<()> {
    let session = rt.session_snapshot();
    let element_id = el["id"].as_str().unwrap_or("").to_string();
    let claim = VerifyItem::SlideClaim {
        deck: deck.to_string(),
        slide_id: slide_id.to_string(),
        element_id: element_id.clone(),
    };
    let changed_tree = |cite: &Value| match (cite["tree_hash"].as_str(), tree) {
        (Some(was), Some(now)) => was != now,
        _ => false,
    };

    // A figure of a run.
    let rc = el["run_cite"].clone();
    if rc.is_object() && changed_tree(&rc) {
        let script = rc["script"].as_str().unwrap_or("").to_string();
        let args: Vec<String> = serde_json::from_value(rc["args"].clone()).unwrap_or_default();
        let old_run = rc["run_id"].as_str().unwrap_or("").to_string();
        let old_path = rc["artifact"].as_str().unwrap_or("").to_string();
        let new_run = rerun(rt, reruns, &script, &args).await?;
        // The same figure of the new run: the path below the run's directory is kept.
        let suffix = old_path
            .split_once(&format!("/{old_run}/"))
            .map(|(_, s)| s.to_string())
            .unwrap_or_else(|| old_path.rsplit('/').next().unwrap_or("").to_string());
        let figures = rt.artifacts_list(Some(&new_run), Some(ArtifactKind::Figure), None);
        let entry = figures
            .iter()
            .find(|a| a.path.ends_with(&format!("/{new_run}/{suffix}")))
            .or_else(|| {
                figures
                    .iter()
                    .find(|a| a.path.ends_with(&format!("/{suffix}")))
            })
            .ok_or_else(|| {
                ServiceError::not_found(format!(
                    "the re-run of {script} (run `{new_run}`) did not produce {suffix}"
                ))
            })?
            .clone();
        let asset = super::artifacts::copy_figure_asset(&session, &rt.repo, svc, &entry)?;
        el["asset"] = json!(asset);
        el["run_cite"] = json!({
            "run_id": entry.run_id,
            "artifact": entry.path,
            "script": entry.script,
            "args": entry.args,
            "tree_hash": entry.tree_hash,
        });
        changes.lines.push(format!(
            "figure {suffix}: run {old_run} → run {}",
            entry.run_id
        ));
        changes.touched.push(claim.clone());
        changes.touched.push(VerifyItem::Figure {
            run_id: old_run,
            artifact: old_path,
        });
    }

    // An equation of a function.
    let eq = el["eq_cite"].clone();
    if let (Some(path), Some(qualname), Some(hash)) = (
        eq["path"].as_str(),
        eq["qualname"].as_str(),
        eq["source_hash"].as_str(),
    ) {
        if super::provenance::file_hash(&rt.repo, path).as_deref() != Some(hash) {
            let k = rt.kernel().await?;
            let fresh = super::provenance::equation_record(&k, &rt.repo, path, qualname).await?;
            let mut line = format!("equation of {qualname}");
            if eq["latex"].as_str() != Some(fresh.latex.as_str()) {
                line.push_str(&format!(
                    ": {} → {}",
                    eq["latex"].as_str().unwrap_or("?"),
                    fresh.latex
                ));
            }
            let old_check = eq["check"]["status"].as_str().unwrap_or("?").to_string();
            let new_check = serde_json::to_value(fresh.check.status)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            if old_check != new_check {
                line.push_str(&format!(" (check {old_check} → {new_check})"));
            }
            if !line.contains(':') && !line.contains('(') {
                line.push_str(": source changed, equation unchanged");
            }
            el["eq_cite"] = serde_json::to_value(&fresh).unwrap_or(Value::Null);
            // The acknowledgement was for the old equation.
            if let Some(o) = el.as_object_mut() {
                o.remove("acknowledged");
            }
            changes.lines.push(line);
            changes.touched.push(claim.clone());
            changes.touched.push(VerifyItem::Equation {
                path: path.to_string(),
                qualname: qualname.to_string(),
            });
        }
    }

    // Numbers bound to named results, in text and in table cells.
    let bound: Vec<(String, Value, String)> = fibration_deck::workspace::text_runs(el)
        .into_iter()
        .filter(|(_, r)| r["run_cite"].is_object() && changed_tree(&r["run_cite"]))
        .map(|(ptr, r)| {
            (
                ptr,
                r["run_cite"].clone(),
                r["t"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    for (ptr, rc, text) in bound {
        let script = rc["script"].as_str().unwrap_or("").to_string();
        let args: Vec<String> = serde_json::from_value(rc["args"].clone()).unwrap_or_default();
        let name = rc["result_name"].as_str().unwrap_or("").to_string();
        let old_run = rc["run_id"].as_str().unwrap_or("").to_string();
        let new_run = rerun(rt, reruns, &script, &args).await?;
        let listed = rt.artifacts_list(Some(&new_run), Some(ArtifactKind::Results), Some(1));
        let entry = listed.first().cloned();
        let result = entry
            .as_ref()
            .and_then(|e| e.results.as_ref())
            .and_then(|rs| rs.iter().find(|r| r.name == name))
            .cloned()
            .ok_or_else(|| {
                ServiceError::not_found(format!(
                    "the re-run of {script} (run `{new_run}`) recorded no result `{name}`"
                ))
            })?;
        let entry = entry.expect("checked above");
        let shown = format_result(&result, shown_decimals(&text));
        let Some(run) = el.pointer_mut(&ptr) else {
            continue;
        };
        run["t"] = json!(shown);
        run["run_cite"] = json!({
            "run_id": entry.run_id,
            "result_name": name,
            "script": entry.script,
            "args": entry.args,
            "tree_hash": entry.tree_hash,
        });
        changes.lines.push(if text == shown {
            format!("{name}: {text} (unchanged value, run {old_run} → run {new_run})")
        } else {
            format!("{name}: {text} → {shown}")
        });
        changes.touched.push(claim.clone());
        changes.touched.push(VerifyItem::Result {
            run_id: old_run,
            name,
        });
    }
    Ok(())
}

/// Refresh the stale elements of one slide (or only `element_id`) and propose the result. Returns
/// `None` when nothing on it is stale.
pub async fn refresh_slide(
    rt: &SessionRuntime,
    deck: &str,
    slide_id: &str,
    element_id: Option<&str>,
) -> ServiceResult<Option<ProposalView>> {
    let svc = rt.deck(deck).await?;
    let id = fibration_deck_protocol::ids::SlideId::new(slide_id.to_string())
        .map_err(|_| ServiceError::invalid("`slide_id` is not a slide id"))?;
    let (slide, hash) = svc
        .ws
        .deck()
        .read_slide(&id)
        .map_err(|e| ServiceError::not_found(e.to_string()))?;
    let before = serde_json::to_value(&slide).map_err(|e| ServiceError::internal(e.to_string()))?;
    let mut after = before.clone();
    let tree = super::artifacts::current_tree(&rt.repo);
    let mut reruns = Reruns::new();
    let mut changes = Changes::default();
    let mut found = element_id.is_none();
    let keys: Vec<String> = after["placeholders"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    for key in keys {
        let el = &mut after["placeholders"][key.as_str()];
        if element_id.is_some_and(|w| el["id"] != w) {
            continue;
        }
        found |= element_id.is_some();
        refresh_element(
            rt,
            &svc,
            &mut reruns,
            tree.as_deref(),
            deck,
            slide_id,
            el,
            &mut changes,
        )
        .await?;
    }
    let n = after["elements"].as_array().map(Vec::len).unwrap_or(0);
    for i in 0..n {
        let id_matches = element_id.is_none_or(|w| after["elements"][i]["id"] == w);
        if !id_matches {
            continue;
        }
        found |= element_id.is_some();
        let el = &mut after["elements"][i]["content"];
        refresh_element(
            rt,
            &svc,
            &mut reruns,
            tree.as_deref(),
            deck,
            slide_id,
            el,
            &mut changes,
        )
        .await?;
    }
    if !found {
        return Err(ServiceError::not_found(format!(
            "slide {slide_id} has no element {}",
            element_id.unwrap_or("")
        )));
    }
    if changes.lines.is_empty() {
        return Ok(None);
    }
    // What the change resets: verification recorded for what is replaced.
    let verified: Vec<String> = rt
        .verification_list()
        .into_iter()
        .filter(|v| {
            changes.touched.contains(&v.item)
                && (v.state != VerificationState::Unverified || v.stale)
        })
        .map(|v| {
            format!(
                "{} ({})",
                v.label,
                match v.state {
                    VerificationState::HumanVerified => "verified",
                    VerificationState::MachineChecked => "machine-checked",
                    VerificationState::Flagged => "flagged",
                    VerificationState::Unverified => "verified for earlier content",
                }
            )
        })
        .collect();
    let mut message = format!("Changed:\n{}", changes.lines.join("\n"));
    message.push_str("\nInvalidates: ");
    if verified.is_empty() {
        message.push_str("no recorded verification");
    } else {
        message.push_str(&format!("\n{}", verified.join("\n")));
    }
    let conversation = rt.meta().active_conversation.unwrap_or_default();
    let view = ProposalView {
        id: format!("refresh-{slide_id}"),
        conversation: conversation.clone(),
        kind: ProposalKind::SlideUpdate,
        path: Some(super::decks::slide_rel(deck, slide_id)),
        summary: format!(
            "Refresh {} from the analysis ({} change{})",
            slide_id,
            changes.lines.len(),
            if changes.lines.len() == 1 { "" } else { "s" }
        ),
        original: None,
        proposed: None,
        diff: None,
        command: None,
        packages: None,
        message: Some(message),
        in_scope: true,
        reason: ProposalReason::Review,
        deck: Some(deck.to_string()),
        slide_ids: Some(vec![slide_id.to_string()]),
        before: Some(before),
        after: Some(after.clone()),
    };
    let (view, rx) = rt.add_pending(&conversation, view);
    rt.emit(SessionEvent::Proposal {
        proposal: view.clone(),
    });
    let Some(this) = rt.arc() else {
        return Ok(Some(view));
    };
    let proposal_id = view.id.clone();
    let deck = deck.to_string();
    let slide_id = slide_id.to_string();
    tokio::spawn(async move {
        let decision = rx.await.unwrap_or(ProposalDecision::Reject {
            reason: Some("cancelled".into()),
        });
        let accepted = match decision {
            ProposalDecision::Accept { content } => {
                let doc = match content {
                    Some(c) => serde_json::from_str::<Value>(&c).unwrap_or(after),
                    None => after,
                };
                match apply(&this, &deck, &slide_id, doc, &hash).await {
                    Ok(()) => true,
                    Err(e) => {
                        // The slide changed since the proposal was made (or the document is
                        // invalid): the proposal resolves as rejected, the slide is untouched,
                        // and the user is told so, with the offer to refresh again.
                        tracing::warn!("refreshing {deck}/{slide_id}: {e}");
                        let message = if e.code == fibration_protocol::ui::IpcErrorCode::Conflict {
                            format!(
                                "Slide {slide_id} of {deck} changed after the refresh was proposed, so the refresh was not applied. Refresh again to propose it against the current slide."
                            )
                        } else {
                            format!(
                                "The refresh of slide {slide_id} of {deck} could not be applied: {}. Refresh again to propose it anew.",
                                e.message
                            )
                        };
                        this.emit(SessionEvent::RefreshFailed {
                            conversation: conversation.clone(),
                            proposal_id: proposal_id.clone(),
                            deck: deck.clone(),
                            slide_id: slide_id.clone(),
                            message,
                        });
                        false
                    }
                }
            }
            ProposalDecision::Reject { .. } => false,
        };
        this.emit(SessionEvent::ProposalResolved {
            proposal_id,
            decision: if accepted {
                Decision::Accept
            } else {
                Decision::Reject
            },
            by: DecidedBy::User,
        });
    });
    Ok(Some(view))
}

async fn apply(
    rt: &SessionRuntime,
    deck: &str,
    slide_id: &str,
    doc: Value,
    hash: &str,
) -> ServiceResult<()> {
    let slide: fibration_deck_protocol::deck::Slide = serde_json::from_value(doc)
        .map_err(|e| ServiceError::invalid(format!("the refreshed slide: {e}")))?;
    // Undo is the checkpoint timeline, as for any accepted change.
    let id = crate::checkpoint::new_id(&format!("deck-{deck}"));
    crate::checkpoint::create_labeled(
        &rt.repo,
        &id,
        &format!("before refreshing {deck}/{slide_id}"),
    )
    .map_err(|e| ServiceError::io(format!("checkpoint: {e:#}")))?;
    let svc = rt.deck(deck).await?;
    svc.slide_update(slide, Some(hash))
        .map(|_| ())
        .map_err(ServiceError::from)
}

/// Refresh every slide of a deck that has a stale element; one proposal per slide.
pub async fn refresh_stale(rt: &SessionRuntime, deck: &str) -> ServiceResult<Vec<ProposalView>> {
    let svc = rt.deck(deck).await?;
    let (_, slides) = svc
        .ws
        .deck()
        .read_all()
        .map_err(|e| ServiceError::not_found(e.to_string()))?;
    let mut out = Vec::new();
    for s in slides {
        if let Some(p) = refresh_slide(rt, deck, s.id.as_str(), None).await? {
            out.push(p);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displayed_precision_is_kept() {
        assert_eq!(shown_decimals("1.823 s"), Some(3));
        assert_eq!(shown_decimals("about 18%"), Some(0));
        assert_eq!(shown_decimals("none"), None);
        let r = fibration_protocol::kernel::NamedResult {
            name: "tau".into(),
            value: 1.9,
            unit: Some("s".into()),
            uncertainty: None,
        };
        assert_eq!(format_result(&r, Some(3)), "1.900 s");
    }
}

//! Provenance of slide elements placed from the analysis: what the deck checker looks at in the
//! project (tree identity, source file hashes, named results of runs), and equations placed on
//! slides with their equation record.

use super::error::{ServiceError, ServiceResult};
use crate::kernel::KernelClient;
use analyst_deck::workspace::Provenance;
use analyst_deck_protocol::deck::{EqCheckRecord, EqCheckState, EqCite, EqCounterexample, EqTier};
use analyst_protocol::kernel::{
    EqCheckParams, EqCheckStatus, EqExtractParams, EquationStatus, EquationTier,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Trials and relative tolerance of the numeric check behind an equation placed on a slide
/// (the kernel's defaults, stated so the record carries them).
pub const EQ_CHECK_TRIALS: u64 = 32;
pub const EQ_CHECK_TOL: f64 = 1e-6;

/// SHA-256 (hex) of a repo-relative file's content, `None` when it is missing or outside the
/// repository.
pub fn file_hash(repo: &Path, rel: &str) -> Option<String> {
    let path = crate::util::resolve_in_repo(repo, rel, true).ok()?;
    let bytes = std::fs::read(path).ok()?;
    Some(crate::util::sha256_hex(&bytes))
}

/// Looks up a named result of a run: `(run id, name) -> {name, value, unit?, uncertainty?}`.
type ResultLookup = Box<dyn Fn(&str, &str) -> Option<Value> + Send + Sync>;

/// The project as the deck checker sees it: the repository's tree identity and files, and (with
/// a session) the named results its runs recorded.
pub struct RepoProvenance {
    repo: PathBuf,
    results: Option<ResultLookup>,
}

impl RepoProvenance {
    pub fn new(repo: &Path) -> RepoProvenance {
        RepoProvenance {
            repo: repo.to_path_buf(),
            results: None,
        }
    }

    /// With a lookup of the named results of the session's runs.
    pub fn with_results(
        mut self,
        f: impl Fn(&str, &str) -> Option<Value> + Send + Sync + 'static,
    ) -> RepoProvenance {
        self.results = Some(Box::new(f));
        self
    }
}

impl Provenance for RepoProvenance {
    fn tree_identity(&self) -> Option<String> {
        super::artifacts::current_tree(&self.repo)
    }

    fn file_hash(&self, path: &str) -> Option<String> {
        file_hash(&self.repo, path)
    }

    fn named_result(&self, run_id: &str, name: &str) -> Option<Value> {
        self.results.as_ref().and_then(|f| f(run_id, name))
    }
}

/// The equation record of a function: extracted by the analysis kernel (`eq.extract`), checked
/// numerically when it is declared (`eq.check`), with the source file's content hash.
pub async fn equation_record(
    kernel: &KernelClient,
    repo: &Path,
    path: &str,
    function: &str,
) -> ServiceResult<EqCite> {
    let path = crate::util::normalize_rel_path(path).map_err(ServiceError::invalid)?;
    let source_hash = file_hash(repo, &path)
        .ok_or_else(|| ServiceError::not_found(format!("`{path}` not found in the project")))?;
    let extracted = kernel
        .eq_extract(EqExtractParams {
            path: path.clone(),
            function: Some(function.to_string()),
        })
        .await?
        .equations;
    let eq = extracted
        .iter()
        .find(|e| e.qualname == function)
        .or_else(|| extracted.iter().find(|e| e.function == function))
        .ok_or_else(|| {
            ServiceError::not_found(format!(
                "`{path}` has no function `{function}` with an equation (functions: {})",
                extracted
                    .iter()
                    .map(|e| e.qualname.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
    let latex = match (&eq.latex, eq.status) {
        (Some(l), EquationStatus::Ok) if !l.trim().is_empty() => l.clone(),
        _ => {
            return Err(ServiceError::invalid(format!(
            "`{}` has no equation to show: {}",
            eq.qualname,
            eq.reason.as_deref().unwrap_or(
                "it is neither derivable from the code nor declared in a Math: or LaTeX: section"
            )
        )))
        }
    };
    let tier = match eq.tier {
        EquationTier::Derived => EqTier::Derived,
        EquationTier::Declared => EqTier::Declared,
        EquationTier::Latex => EqTier::Latex,
        EquationTier::None => {
            return Err(ServiceError::invalid(format!(
                "`{}` has no equation",
                eq.qualname
            )))
        }
    };
    let check = match tier {
        EqTier::Declared => {
            let r = kernel
                .eq_check(EqCheckParams {
                    path: path.clone(),
                    function: eq.qualname.clone(),
                    trials: Some(EQ_CHECK_TRIALS),
                    tol: Some(EQ_CHECK_TOL),
                    ranges: None,
                })
                .await?;
            EqCheckRecord {
                status: match r.status {
                    EqCheckStatus::Agree => EqCheckState::Agree,
                    EqCheckStatus::Mismatch => EqCheckState::Mismatch,
                    EqCheckStatus::Unavailable => EqCheckState::Unavailable,
                },
                trials: r.trials,
                tol: Some(EQ_CHECK_TOL),
                counterexample: r.counterexample.map(|c| EqCounterexample {
                    inputs: c.inputs,
                    expected: c.expected,
                    actual: c.actual,
                }),
                reason: r.reason,
            }
        }
        EqTier::Derived => EqCheckRecord {
            status: EqCheckState::Unavailable,
            trials: 0,
            tol: None,
            counterexample: None,
            reason: Some(
                "derived from the code itself, so there is nothing separate to check".into(),
            ),
        },
        EqTier::Latex => EqCheckRecord {
            status: EqCheckState::Unavailable,
            trials: 0,
            tol: None,
            counterexample: None,
            reason: Some("display-only LaTeX; nothing checks it against the code".into()),
        },
    };
    Ok(EqCite {
        latex,
        tier,
        check,
        qualname: eq.qualname.clone(),
        path,
        source_hash,
    })
}

/// Place the equation of a function on a slide: the equation record from the analysis kernel
/// ([`equation_record`]) in an equation element, in `placeholder` or the first free
/// non-title placeholder of the slide's layout that takes text or an image.
///
/// One function for both callers, as for figures: the agent's `slide.place_equation` turns
/// the result into a slide proposal; the UI's `slide_place_equation` writes it as a user edit.
#[allow(clippy::too_many_arguments)]
pub async fn place_equation(
    kernel: &KernelClient,
    repo: &Path,
    svc: &analyst_deck::service::DeckService,
    slide_id: &str,
    path: &str,
    function: &str,
    placeholder: Option<&str>,
) -> ServiceResult<super::artifacts::PlacedElement> {
    use analyst_deck_protocol::template::ContentKind;
    let id = analyst_deck_protocol::ids::SlideId::new(slide_id.to_string())
        .map_err(|_| ServiceError::invalid("`slide_id` is not a slide id"))?;
    let (slide, hash) = svc
        .ws
        .deck()
        .read_slide(&id)
        .map_err(|e| ServiceError::not_found(e.to_string()))?;
    let cite = equation_record(kernel, repo, path, function).await?;
    let mut doc =
        serde_json::to_value(&slide).map_err(|e| ServiceError::internal(e.to_string()))?;
    let key = super::artifacts::free_placeholder(
        svc,
        &slide.layout,
        &doc,
        placeholder,
        &[ContentKind::Text, ContentKind::Image],
        false,
    )
    .await?;
    let element_id = super::artifacts::element_id_for(&doc, &key, "e-eq");
    doc["placeholders"][key.as_str()] = json!({
        "type": "equation",
        "id": element_id,
        "eq_cite": cite,
    });
    Ok(super::artifacts::PlacedElement { slide: doc, hash })
}

/// A number as a slide shows it: `decimals` places when given, else four significant digits,
/// without trailing zeros.
pub fn format_value(value: f64, decimals: Option<usize>) -> String {
    if let Some(d) = decimals {
        return format!("{value:.*}", d.min(12));
    }
    if value == 0.0 || !value.is_finite() {
        return format!("{value}");
    }
    let magnitude = value.abs().log10().floor() as i64;
    let places = (3 - magnitude).clamp(0, 12) as usize;
    let s = format!("{value:.places$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// Insert a number bound to a named result of a run into a text placeholder of a slide: a new
/// paragraph `before` + the formatted value (and unit) + `after`, the number's text run carrying
/// a result citation (run id, result name, script, arguments, tree identity) the checker
/// verifies against the recorded value.
#[allow(clippy::too_many_arguments)]
pub fn bind_result(
    runs: &super::runs::RunService,
    session: &crate::session::Session,
    repo: &Path,
    svc: &analyst_deck::service::DeckService,
    slide_id: &str,
    placeholder: &str,
    run_id: &str,
    result_name: &str,
    decimals: Option<usize>,
    before: Option<&str>,
    after: Option<&str>,
) -> ServiceResult<super::artifacts::PlacedElement> {
    use analyst_protocol::kernel::ArtifactKind;
    let entry = super::artifacts::list(runs, session, repo, Some(run_id), Some(ArtifactKind::Results), Some(1))
        .into_iter()
        .next()
        .ok_or_else(|| {
            ServiceError::not_found(format!(
                "run `{run_id}` recorded no named results; the script records them with analyst_results.record(name, value, unit)"
            ))
        })?;
    let result = entry
        .results
        .as_ref()
        .and_then(|rs| rs.iter().find(|r| r.name == result_name))
        .cloned()
        .ok_or_else(|| {
            ServiceError::not_found(format!(
                "run `{run_id}` has no result `{result_name}` (results: {})",
                entry
                    .results
                    .iter()
                    .flatten()
                    .map(|r| r.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
    let id = analyst_deck_protocol::ids::SlideId::new(slide_id.to_string())
        .map_err(|_| ServiceError::invalid("`slide_id` is not a slide id"))?;
    let (slide, hash) = svc
        .ws
        .deck()
        .read_slide(&id)
        .map_err(|e| ServiceError::not_found(e.to_string()))?;
    let mut doc =
        serde_json::to_value(&slide).map_err(|e| ServiceError::internal(e.to_string()))?;
    let mut number = format_value(result.value, decimals);
    if let Some(u) = result.unit.as_deref().filter(|u| !u.is_empty()) {
        number = if u == "%" {
            format!("{number}%")
        } else {
            format!("{number} {u}")
        };
    }
    let mut runs_json = Vec::new();
    if let Some(b) = before.filter(|b| !b.is_empty()) {
        runs_json.push(json!({"t": b}));
    }
    runs_json.push(json!({
        "t": number,
        "run_cite": {
            "run_id": entry.run_id,
            "result_name": result.name,
            "script": entry.script,
            "args": entry.args,
            "tree_hash": entry.tree_hash,
        },
    }));
    if let Some(a) = after.filter(|a| !a.is_empty()) {
        runs_json.push(json!({"t": a}));
    }
    let paragraph = json!({"runs": runs_json});
    let slot = &mut doc["placeholders"][placeholder];
    match slot["type"].as_str() {
        Some("text") => {
            let paras = slot["text"]["paragraphs"]
                .as_array_mut()
                .ok_or_else(|| ServiceError::internal("a text element without paragraphs"))?;
            // An empty placeholder text is replaced, not appended to.
            let empty = paras.iter().all(|p| {
                p["runs"]
                    .as_array()
                    .is_none_or(|rs| rs.iter().all(|r| r["t"].as_str().unwrap_or("").is_empty()))
            });
            if empty {
                paras.clear();
            }
            paras.push(paragraph);
        }
        None | Some("empty") => {
            let element_id = super::artifacts::element_id_for(&doc, placeholder, "e-num");
            doc["placeholders"][placeholder] = json!({
                "type": "text",
                "id": element_id,
                "text": {"paragraphs": [paragraph]},
            });
        }
        Some(other) => {
            return Err(ServiceError::invalid(format!(
                "placeholder `{placeholder}` holds {other}, not text; a number goes into a text placeholder"
            )))
        }
    }
    Ok(super::artifacts::PlacedElement { slide: doc, hash })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_shown_at_a_sensible_precision() {
        assert_eq!(format_value(1.8234, None), "1.823");
        assert_eq!(format_value(0.18312, None), "0.1831");
        assert_eq!(format_value(18234.7, None), "18235");
        assert_eq!(format_value(2.5, None), "2.5");
        assert_eq!(format_value(-0.000123456, None), "-0.0001235");
        assert_eq!(format_value(0.0, None), "0");
        assert_eq!(format_value(1.8234, Some(1)), "1.8");
    }

    #[test]
    fn file_hashes_are_of_content_and_stay_inside_the_repo() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("lib")).unwrap();
        std::fs::write(d.path().join("lib/m.py"), b"x = 1\n").unwrap();
        let h = file_hash(d.path(), "lib/m.py").unwrap();
        assert_eq!(h, crate::util::sha256_hex(b"x = 1\n"));
        assert_eq!(file_hash(d.path(), "lib/missing.py"), None);
        assert_eq!(file_hash(d.path(), "../etc/passwd"), None);
        let p = RepoProvenance::new(d.path());
        assert_eq!(p.file_hash("lib/m.py"), Some(h));
        assert_eq!(p.named_result("r", "n"), None);
        let p = RepoProvenance::new(d.path())
            .with_results(|run, name| Some(json!({"run": run, "name": name})));
        assert_eq!(p.named_result("r1", "tau").unwrap()["name"], "tau");
    }
}

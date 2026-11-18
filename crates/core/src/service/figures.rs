//! Figure discovery: `out/<script>/<run>/**` (figures and their sidecars), including terminal
//! runs found only through their `run.json`, plus artifacts pulled from remote targets into the
//! session's `artifacts/<run_id>/` directory.

use analyst_protocol::kernel::{ArtifactKind, FigureSidecar};
use analyst_protocol::ui::{FigureFormat, FigureView};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// `run.json` as written by the kernel (PROTOCOL 1.4 step 3) or by the terminal watcher.
#[derive(Debug, Clone, Deserialize)]
pub struct RunJson {
    pub run_id: String,
    pub script: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub tree_hash: Option<String>,
}

/// One run directory under `out/`.
#[derive(Debug, Clone)]
pub struct ScannedRun {
    pub run_id: String,
    pub script: String,
    pub args: Vec<String>,
    pub started_at: String,
    pub host: Option<String>,
    /// Repo-relative run output directory, e.g. `out/fit_decay/<run>`.
    pub dir_rel: String,
    pub has_run_json: bool,
    pub figures: Vec<FigureView>,
}

pub fn read_run_json(path: &Path) -> Option<RunJson> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn mtime_rfc3339(p: &Path) -> String {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .map(|t| {
            chrono::DateTime::<chrono::Utc>::from(t)
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        })
        .unwrap_or_default()
}

fn to_rel(repo: &Path, p: &Path) -> String {
    p.strip_prefix(repo)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

/// A run id that is safe to use as a single path component (ids read from `run.json` files
/// on disk are untrusted).
pub fn safe_run_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The sidecar next to a figure: `<file name>.meta.json` (PROTOCOL 9).
pub fn sidecar_file(figure: &Path) -> PathBuf {
    let mut name = figure
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".meta.json");
    figure.with_file_name(name)
}

/// Whether a path is a figure sidecar (`*.meta.json`), which is never listed as a figure.
fn is_sidecar(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().ends_with(".meta.json"))
        .unwrap_or(false)
}

/// Build a `FigureView` for a figure file (sidecar read from `<file name>.meta.json` next to
/// it).
pub fn figure_view(repo: &Path, file: &Path, run_id: &str, script: &str) -> Option<FigureView> {
    let rel = to_rel(repo, file);
    let format = FigureFormat::from_path(&rel)?;
    let sidecar: Option<FigureSidecar> = std::fs::read_to_string(sidecar_file(file))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let meta = std::fs::metadata(file).ok()?;
    let name = file
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let created_at = sidecar
        .as_ref()
        .map(|s| s.created_at.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| mtime_rfc3339(file));
    Some(FigureView {
        // The file is on this machine (a local run's output, or a remote figure already
        // pulled back), so the renderer loads it through the asset protocol.
        file_abs: Some(file.to_string_lossy().to_string()),
        run_id: run_id.to_string(),
        script: script.to_string(),
        path: rel,
        name,
        format,
        sidecar,
        created_at,
        size: meta.len(),
    })
}

/// A `FigureView` built from a kernel artifact alone, for figures that are not on this
/// machine yet (remote targets).
pub fn figure_view_from_artifact(
    artifact: &analyst_protocol::kernel::Artifact,
    run_id: &str,
    script: &str,
) -> Option<FigureView> {
    let format = FigureFormat::from_path(&artifact.path)?;
    Some(FigureView {
        file_abs: None,
        run_id: run_id.to_string(),
        script: script.to_string(),
        path: artifact.path.clone(),
        name: artifact
            .path
            .rsplit('/')
            .next()
            .unwrap_or(&artifact.path)
            .to_string(),
        format,
        sidecar: artifact.sidecar.clone(),
        created_at: artifact.created_at.clone(),
        size: artifact.size,
    })
}

/// Figure files under a directory, sorted by path (skips `_`-prefixed directories).
pub fn figure_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_entry(|e| {
            !(e.file_type().is_dir()
                && e.depth() > 0
                && e.file_name().to_string_lossy().starts_with('_'))
        })
        .flatten()
    {
        if e.file_type().is_file()
            && !is_sidecar(e.path())
            && ArtifactKind::from_path(&e.path().to_string_lossy()) == ArtifactKind::Figure
        {
            out.push(e.path().to_path_buf());
        }
    }
    out.sort();
    out
}

/// Scan a single run directory `out/<script>/<run>/`.
pub fn scan_run_dir(repo: &Path, dir: &Path) -> Option<ScannedRun> {
    if !dir.is_dir() {
        return None;
    }
    let dir_rel = to_rel(repo, dir);
    let run_json = read_run_json(&dir.join("run.json"));
    let dir_name = dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let parent_name = dir
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    // A `run.json` with an unusable id is treated like a missing one.
    let run_json = run_json.filter(|r| safe_run_id(&r.run_id));
    let (run_id, script, args, started_at, host) = match &run_json {
        Some(r) => (
            r.run_id.clone(),
            r.script.clone(),
            r.args.clone(),
            r.started_at.clone().unwrap_or_else(|| mtime_rfc3339(dir)),
            r.host.clone(),
        ),
        None => (
            format!("{parent_name}-{dir_name}"),
            parent_name.clone(),
            vec![],
            mtime_rfc3339(dir),
            None,
        ),
    };
    let figures = figure_files(dir)
        .iter()
        .filter_map(|f| figure_view(repo, f, &run_id, &script))
        .collect();
    Some(ScannedRun {
        run_id,
        script,
        args,
        started_at,
        host,
        dir_rel,
        has_run_json: run_json.is_some(),
        figures,
    })
}

/// Run directories under `out/` (two levels: `<script>/<run>`), newest first by directory
/// name. Run directory names are time stamps, so this order needs no file reads.
pub fn run_dirs(repo: &Path, out_dir: &str) -> Vec<PathBuf> {
    let out = repo.join(out_dir);
    let mut dirs: Vec<(String, PathBuf)> = Vec::new();
    let Ok(scripts) = std::fs::read_dir(&out) else {
        return vec![];
    };
    for s in scripts.flatten() {
        let sname = s.file_name().to_string_lossy().to_string();
        if sname.starts_with('_') || sname.starts_with('.') {
            continue;
        }
        if !s.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(s.path()) else {
            continue;
        };
        for r in rd.flatten() {
            let rname = r.file_name().to_string_lossy().to_string();
            if rname.starts_with('_') || rname.starts_with('.') {
                continue;
            }
            // `file_type` does not follow symlinks: a link out of the repo is never scanned.
            if !r.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            dirs.push((rname, r.path()));
        }
    }
    dirs.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    dirs.into_iter().map(|(_, p)| p).collect()
}

/// Run directories under `out/`, newest first by `started_at`, including terminal runs and
/// directories that only hold figures. Scanning stops once `limit` runs are found (U3).
pub fn scan_out_limited(repo: &Path, out_dir: &str, limit: usize) -> Vec<ScannedRun> {
    let mut runs = Vec::new();
    for dir in run_dirs(repo, out_dir) {
        if runs.len() >= limit {
            break;
        }
        if let Some(scanned) = scan_run_dir(repo, &dir) {
            if scanned.has_run_json || !scanned.figures.is_empty() {
                runs.push(scanned);
            }
        }
    }
    runs.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then(b.run_id.cmp(&a.run_id))
    });
    runs
}

/// Every run directory under `out/`.
pub fn scan_out(repo: &Path, out_dir: &str) -> Vec<ScannedRun> {
    scan_out_limited(repo, out_dir, usize::MAX)
}

/// Figures pulled from a remote target into `artifacts/<run_id>/<path>`.
pub fn scan_pulled(artifacts_dir: &Path, run_id: &str, script: &str) -> Vec<FigureView> {
    if !safe_run_id(run_id) {
        return vec![];
    }
    let base = artifacts_dir.join(run_id);
    if !base.is_dir() {
        return vec![];
    }
    figure_files(&base)
        .iter()
        .filter_map(|f| {
            let mut v = figure_view(&base, f, run_id, script)?;
            // Path stays repo-relative (the layout under artifacts/<run_id>/ mirrors the repo).
            v.path = to_rel(&base, f);
            Some(v)
        })
        .collect()
}

/// Which run directory (`out/<script>/<run>`) a repo-relative path belongs to, if any.
pub fn run_dir_of(out_dir: &str, rel: &str) -> Option<String> {
    let prefix = format!("{}/", out_dir.trim_matches('/'));
    let rest = rel.strip_prefix(&prefix)?;
    let mut parts = rest.split('/');
    let script = parts.next()?;
    let run = parts.next()?;
    if script.starts_with('_') || run.starts_with('_') {
        return None;
    }
    // Need at least one more component (a file inside the run dir) or the run dir itself.
    Some(format!("{}{script}/{run}", prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_runs_with_and_without_run_json() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path();
        let r1 = repo.join("out/fit/20260101-000000-aaaaaa/figures");
        std::fs::create_dir_all(&r1).unwrap();
        std::fs::write(r1.join("residuals.png"), b"png").unwrap();
        std::fs::write(
            r1.join("residuals.png.meta.json"),
            serde_json::json!({
                "run_id": "20260101-000000-aaaaaa", "script": "scripts/fit.py", "args": [],
                "title": "Residuals", "axes": [], "created_at": "2026-01-01T00:00:01Z",
                "width_in": 6.4, "height_in": 4.8, "dpi": 100.0
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            repo.join("out/fit/20260101-000000-aaaaaa/run.json"),
            serde_json::json!({"run_id":"20260101-000000-aaaaaa","script":"scripts/fit.py","args":["--n","3"],"started_at":"2026-01-01T00:00:00Z","host":"h","tree_hash":null}).to_string(),
        )
        .unwrap();
        let t = repo.join("out/terminal/20260102-000000/figures");
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(t.join("fig-1.svg"), b"<svg/>").unwrap();
        std::fs::create_dir_all(repo.join("out/_repl/x")).unwrap();
        std::fs::write(repo.join("out/_repl/x/f.png"), b"x").unwrap();
        std::fs::create_dir_all(repo.join("out/empty/r")).unwrap();

        let runs = scan_out(repo, "out");
        assert_eq!(runs.len(), 2, "{runs:?}");
        let fit = runs.iter().find(|r| r.script == "scripts/fit.py").unwrap();
        assert_eq!(fit.args, vec!["--n", "3"]);
        assert!(fit.has_run_json);
        assert_eq!(fit.figures.len(), 1);
        let f = &fit.figures[0];
        assert_eq!(
            f.path,
            "out/fit/20260101-000000-aaaaaa/figures/residuals.png"
        );
        assert_eq!(f.format, FigureFormat::Png);
        assert_eq!(
            f.sidecar.as_ref().unwrap().title.as_deref(),
            Some("Residuals")
        );
        assert_eq!(f.created_at, "2026-01-01T00:00:01Z");
        let term = runs.iter().find(|r| r.script == "terminal").unwrap();
        assert!(!term.has_run_json);
        assert_eq!(term.run_id, "terminal-20260102-000000");
        assert_eq!(term.figures[0].format, FigureFormat::Svg);
        assert_eq!(
            run_dir_of("out", "out/fit/r1/figures/a.png").as_deref(),
            Some("out/fit/r1")
        );
        assert_eq!(run_dir_of("out", "out/_repl/r1/a.png"), None);
        assert_eq!(run_dir_of("out", "scripts/a.py"), None);
        // A data file that happens to share the figure's stem is not a sidecar (K3), and a
        // run.json with a path-like id is ignored.
        assert_eq!(fit.figures.len(), 1);
        std::fs::write(
            repo.join("out/terminal/20260102-000000/run.json"),
            r#"{"run_id":"../../x","script":"terminal"}"#,
        )
        .unwrap();
        let runs = scan_out_limited(repo, "out", 1);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, "terminal-20260102-000000");
        assert!(!safe_run_id("../x") && !safe_run_id("") && safe_run_id("20260101-000000-ab12cd"));
    }
}

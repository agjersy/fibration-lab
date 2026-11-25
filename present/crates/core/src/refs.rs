//! Reference files in the project: always copied, content-addressed, versioned (PROTOCOL 4.3).

use crate::util::{now_iso, sha256_hex, write_atomic};
use anyhow::{anyhow, Context};
use fibration_deck_protocol::cite::RefKind;
use fibration_deck_protocol::config::{RefEntry, RefSource, RefsManifest};
use fibration_deck_protocol::ids::RefId;
use std::path::{Path, PathBuf};

pub fn manifest_path(project: &Path) -> PathBuf {
    project.join("references").join("manifest.toml")
}

pub fn load_manifest(project: &Path) -> anyhow::Result<RefsManifest> {
    let path = manifest_path(project);
    if !path.is_file() {
        return Ok(RefsManifest::default());
    }
    toml::from_str(&std::fs::read_to_string(&path)?)
        .with_context(|| format!("parsing {}", path.display()))
}

pub fn save_manifest(project: &Path, manifest: &RefsManifest) -> anyhow::Result<()> {
    write_atomic(
        &manifest_path(project),
        toml::to_string_pretty(manifest)?.as_bytes(),
    )?;
    Ok(())
}

pub fn kind_of(path: &Path) -> Option<RefKind> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    Some(match ext.as_str() {
        "pdf" => RefKind::Pdf,
        "docx" => RefKind::Docx,
        "pptx" | "potx" => RefKind::Pptx,
        "xlsx" | "xlsm" => RefKind::Xlsx,
        "csv" | "tsv" => RefKind::Csv,
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff" => RefKind::Image,
        "md" | "markdown" => RefKind::Markdown,
        "txt" | "text" | "log" => RefKind::Text,
        "eml" => RefKind::Eml,
        "msg" => RefKind::Msg,
        "html" | "htm" | "mhtml" | "mht" => RefKind::Html,
        _ => return None,
    })
}

/// What adding a file did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Added {
    /// A file not seen before.
    New(RefEntry),
    /// The same original path with different content: a new version of an existing reference.
    NewVersion(RefEntry),
    /// Identical content is already in the project.
    Unchanged(RefEntry),
}

impl Added {
    pub fn entry(&self) -> &RefEntry {
        match self {
            Added::New(e) | Added::NewVersion(e) | Added::Unchanged(e) => e,
        }
    }
}

fn new_ref_id(manifest: &RefsManifest, sha: &str) -> anyhow::Result<RefId> {
    for len in 8..=24 {
        let id = RefId::new(format!("r-{}", &sha[..len])).map_err(|e| anyhow!("{e:?}"))?;
        if manifest.get(&id).is_none() {
            return Ok(id);
        }
    }
    Err(anyhow!("could not derive a free reference id"))
}

/// Copy a file into `references/` and record it. Content addressing means the same bytes are
/// stored once; a changed file at the same original path becomes a new version, and earlier
/// versions stay on disk so existing citations still resolve.
pub fn add_file(project: &Path, source: &Path, parent: Option<&RefId>) -> anyhow::Result<Added> {
    let kind = kind_of(source)
        .ok_or_else(|| anyhow!("unsupported reference format: {}", source.display()))?;
    let bytes = std::fs::read(source).with_context(|| format!("reading {}", source.display()))?;
    let sha = sha256_hex(&bytes);
    let version = sha[..12].to_string();
    let ext = source
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let rel = format!("references/{version}.{ext}");
    let mut manifest = load_manifest(project)?;
    if let Some(existing) = manifest.refs.iter().find(|r| r.version == version) {
        return Ok(Added::Unchanged(existing.clone()));
    }
    let target = project.join(&rel);
    if !target.is_file() {
        write_atomic(&target, &bytes)?;
    }
    let original = source.to_string_lossy().replace('\\', "/");
    let title = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| version.clone());
    if let Some(existing) = manifest
        .refs
        .iter_mut()
        .find(|r| r.original_path == original && parent.is_none())
    {
        existing.previous_versions.push(existing.version.clone());
        existing.version = version;
        existing.file = rel;
        existing.imported = now_iso();
        let entry = existing.clone();
        save_manifest(project, &manifest)?;
        return Ok(Added::NewVersion(entry));
    }
    let entry = RefEntry {
        id: new_ref_id(&manifest, &sha)?,
        file: rel,
        kind,
        title,
        original_path: original,
        imported: now_iso(),
        version,
        previous_versions: Vec::new(),
        parent: parent.map(|p| p.to_string()).unwrap_or_default(),
        source: RefSource::File,
        analyst: Default::default(),
    };
    manifest.refs.push(entry.clone());
    save_manifest(project, &manifest)?;
    Ok(Added::New(entry))
}

/// Remove a reference and every stored version of it.
pub fn remove(project: &Path, id: &RefId) -> anyhow::Result<()> {
    let mut manifest = load_manifest(project)?;
    let Some(pos) = manifest.refs.iter().position(|r| &r.id == id) else {
        return Err(anyhow!("reference {id} not found"));
    };
    let entry = manifest.refs.remove(pos);
    let ext = Path::new(&entry.file)
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    for version in entry
        .previous_versions
        .iter()
        .chain(std::iter::once(&entry.version))
    {
        let file = project.join("references").join(format!("{version}.{ext}"));
        if !manifest.refs.iter().any(|r| &r.version == version) {
            let _ = std::fs::remove_file(file);
        }
    }
    save_manifest(project, &manifest)
}

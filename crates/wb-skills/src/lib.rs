// Written for Personal Present PROTOCOL 11; the legacy fields (`triggers`, `paths`, `always`)
// and their matching follow Fibration Lab `docs/ARCHITECTURE.md` section 5.16 at commit
// f229e4d (Analyst has no skills code at that commit, so there is no source file to port).
//! Shared-to-be crate `wb-skills` (BUILD-SPEC section 4). Must not depend on Present-specific crates.
//!
//! Agent Skills loader: a skill is a folder with a `SKILL.md` (YAML front matter `name`,
//! `description`; markdown body), optional resources, and optional scripts under `scripts/`.
//!
//! Progressive disclosure: the system prompt carries names and descriptions only
//! ([`SkillSet::system_prompt_block`]); the body is read on demand ([`SkillSet::load`]), and
//! so are resources ([`SkillSet::read_resource`]). Scripts are never run by this crate; it
//! only resolves their paths ([`SkillSet::script_path`]).

mod front_matter;

pub use front_matter::{parse_skill_md, FrontMatter, FrontMatterError};

use globset::{Glob, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// File that makes a folder a skill.
pub const SKILL_FILE: &str = "SKILL.md";
/// Folder holding a skill's runnable scripts.
pub const SCRIPTS_DIR: &str = "scripts";
/// Longest allowed `name`.
pub const MAX_NAME_CHARS: usize = 64;
/// Longest allowed `description` (characters).
pub const MAX_DESCRIPTION_CHARS: usize = 1024;
/// Largest `SKILL.md` that is loaded.
pub const MAX_SKILL_MD_BYTES: u64 = 1024 * 1024;
/// Most resource files listed per skill.
pub const MAX_LISTED_RESOURCES: usize = 500;

/// Where a skill comes from. Discovery order is the order of the roots given; a later root
/// overrides an earlier one on a name clash, except that `Managed` skills cannot be
/// overridden.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum SkillSource {
    Builtin,
    User,
    Project,
    Managed,
}

impl SkillSource {
    pub fn as_str(self) -> &'static str {
        match self {
            SkillSource::Builtin => "builtin",
            SkillSource::User => "user",
            SkillSource::Project => "project",
            SkillSource::Managed => "managed",
        }
    }
}

/// A directory whose sub-folders are skills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRoot {
    pub source: SkillSource,
    pub path: PathBuf,
}

impl SkillRoot {
    pub fn new(source: SkillSource, path: impl Into<PathBuf>) -> Self {
        SkillRoot {
            source,
            path: path.into(),
        }
    }
}

/// What the UI and the system prompt need to know about a skill.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub source: SkillSource,
    pub has_scripts: bool,
    pub dir: PathBuf,
}

/// A discovered skill. The body is not kept in memory; `SkillSet::load` reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    /// Empty only for legacy skills (Analyst) that have `triggers`, `paths`, or `always`.
    pub description: String,
    pub source: SkillSource,
    /// The skill folder, canonicalized.
    pub dir: PathBuf,
    pub license: Option<String>,
    /// Parsed for completeness; not enforced.
    pub allowed_tools: Vec<String>,
    /// Legacy (Analyst): words in the user message that activate the skill.
    pub triggers: Vec<String>,
    /// Legacy (Analyst): globs of files in the context that activate the skill.
    pub paths: Vec<String>,
    /// Legacy (Analyst): always active.
    pub always: bool,
    /// Runnable scripts, relative to the skill folder (`scripts/<file>.py`), sorted.
    pub scripts: Vec<String>,
    /// Every other file in the folder, relative, with `/` separators, sorted. `SKILL.md`,
    /// hidden files, and files reached through symbolic links are not listed.
    pub resources: Vec<String>,
}

impl Skill {
    pub fn info(&self) -> SkillInfo {
        SkillInfo {
            name: self.name.clone(),
            description: self.description.clone(),
            source: self.source,
            has_scripts: !self.scripts.is_empty(),
            dir: self.dir.clone(),
        }
    }

    /// Legacy trigger matching (Analyst ARCHITECTURE 5.16): the skill is active when it is
    /// `always`, when one of its `triggers` occurs in the user message (case-insensitive, on
    /// word boundaries), or when one of its `paths` globs matches a file in the context.
    /// Skills without legacy fields never match; they are loaded on demand instead.
    pub fn matches<S: AsRef<str>>(&self, user_text: &str, paths: &[S]) -> bool {
        if self.always {
            return true;
        }
        let text = user_text.to_lowercase();
        if self
            .triggers
            .iter()
            .any(|t| contains_phrase(&text, &t.to_lowercase()))
        {
            return true;
        }
        if self.paths.is_empty() || paths.is_empty() {
            return false;
        }
        let mut b = GlobSetBuilder::new();
        for g in &self.paths {
            if let Ok(glob) = Glob::new(g) {
                b.add(glob);
            }
        }
        match b.build() {
            Ok(set) => paths
                .iter()
                .any(|p| set.is_match(p.as_ref().replace('\\', "/"))),
            Err(_) => false,
        }
    }
}

/// `needle` occurs in `haystack` and is not glued to a letter or digit on either side.
fn contains_phrase(haystack: &str, needle: &str) -> bool {
    let needle = needle.trim();
    if needle.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(pos) = haystack[from..].find(needle) {
        let start = from + pos;
        let end = start + needle.len();
        let before_ok = haystack[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        let after_ok = haystack[end..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        from = start + haystack[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticKind {
    /// A root exists but could not be listed.
    RootUnreadable,
    /// `SKILL.md` could not be read or is too large.
    Unreadable,
    /// No front matter, or the YAML does not parse.
    BadFrontMatter,
    /// `name` is missing or does not match `^[a-z0-9-]{1,64}$`.
    InvalidName,
    /// `description` is missing or longer than 1024 characters.
    InvalidDescription,
    /// Two folders of one root declare the same name; the first (by folder name) is kept.
    DuplicateInRoot,
    /// A skill tried to replace a managed skill and was ignored.
    ManagedOverride,
    /// Not skipped: the folder name differs from `name`.
    NameMismatch,
    /// Not skipped: this skill replaced one with the same name from an earlier root.
    Overrides,
}

/// Something discovery noticed. Never fatal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillDiagnostic {
    pub kind: DiagnosticKind,
    pub source: SkillSource,
    /// The skill folder (or the root).
    pub path: PathBuf,
    /// The declared name, when it could be read.
    pub name: Option<String>,
    /// Whether the skill was left out of the set.
    pub skipped: bool,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("unknown skill `{0}`")]
    UnknownSkill(String),
    #[error("invalid path `{path}`: {reason}")]
    InvalidPath { path: String, reason: &'static str },
    #[error("`{path}` not found in skill `{skill}`")]
    NotFound { skill: String, path: String },
    #[error("`{path}` in skill `{skill}` is neither text nor a supported image")]
    NotText { skill: String, path: String },
    #[error("`{script}` is not a script of skill `{skill}` (scripts are `scripts/*.py`)")]
    NotAScript { skill: String, script: String },
    #[error("skill `{skill}`: {message}")]
    Invalid { skill: String, message: String },
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Content of a resource file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resource {
    /// UTF-8 text.
    Text(String),
    /// An image to hand to the model as an image block.
    Image { media_type: String, bytes: Vec<u8> },
    /// The file is over the cap for its kind; nothing was read.
    TooLarge { size: u64, limit: u64 },
}

/// Size caps for `read_resource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceLimits {
    pub max_text_bytes: u64,
    pub max_image_bytes: u64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        ResourceLimits {
            max_text_bytes: 256 * 1024,
            max_image_bytes: 5 * 1024 * 1024,
        }
    }
}

/// The skills available to a conversation.
#[derive(Debug, Clone, Default)]
pub struct SkillSet {
    skills: BTreeMap<String, Skill>,
    limits: ResourceLimits,
}

/// `^[a-z0-9-]{1,64}$`
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_CHARS
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn image_media_type(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Validate a path given by the model: relative, no `..`, no drive or root. Returns the
/// components.
fn safe_components(rel: &str) -> Result<Vec<String>, SkillError> {
    let invalid = |reason| SkillError::InvalidPath {
        path: rel.to_string(),
        reason,
    };
    if rel.trim().is_empty() {
        return Err(invalid("empty path"));
    }
    if rel.contains('\0') {
        return Err(invalid("contains a NUL character"));
    }
    let norm = rel.replace('\\', "/");
    if norm.starts_with('/') || norm.starts_with('~') || Path::new(rel).is_absolute() {
        return Err(invalid("absolute paths are not allowed"));
    }
    let mut parts = Vec::new();
    for part in norm.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(invalid("`..` is not allowed")),
            p if p.contains(':') => return Err(invalid("`:` is not allowed")),
            p => parts.push(p.to_string()),
        }
    }
    if parts.is_empty() {
        return Err(invalid("empty path"));
    }
    // Belt and braces: whatever the platform makes of it must be plain components.
    if Path::new(&parts.join("/"))
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(invalid("not a plain relative path"));
    }
    Ok(parts)
}

fn io_err(path: &Path, source: std::io::Error) -> SkillError {
    SkillError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn list_files(dir: &Path) -> (Vec<String>, Vec<String>) {
    let mut scripts = Vec::new();
    let mut resources = Vec::new();
    let walker = walkdir::WalkDir::new(dir)
        .follow_links(false)
        .max_depth(8)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'));
    for entry in walker.flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(dir) else {
            continue;
        };
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect();
        let rel = parts.join("/");
        if rel == SKILL_FILE {
            continue;
        }
        let is_script = parts.len() == 2
            && parts[0] == SCRIPTS_DIR
            && Path::new(&parts[1]).extension().is_some_and(|e| e == "py");
        if is_script {
            scripts.push(rel);
        } else if resources.len() < MAX_LISTED_RESOURCES {
            resources.push(rel);
        }
    }
    scripts.sort();
    resources.sort();
    (scripts, resources)
}

impl SkillSet {
    /// An empty set.
    pub fn new() -> Self {
        SkillSet::default()
    }

    /// Discover skills in the roots, in order. A root that does not exist is skipped
    /// silently; an invalid skill is skipped with a diagnostic.
    pub fn discover(roots: &[SkillRoot]) -> (SkillSet, Vec<SkillDiagnostic>) {
        let mut set = SkillSet::default();
        let mut diags = Vec::new();
        for root in roots {
            set.add_root(root, &mut diags);
        }
        (set, diags)
    }

    pub fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self
    }

    fn add_root(&mut self, root: &SkillRoot, diags: &mut Vec<SkillDiagnostic>) {
        let rd = match std::fs::read_dir(&root.path) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                diags.push(SkillDiagnostic {
                    kind: DiagnosticKind::RootUnreadable,
                    source: root.source,
                    path: root.path.clone(),
                    name: None,
                    skipped: true,
                    message: format!("cannot list skills folder: {e}"),
                });
                return;
            }
        };
        let mut dirs: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && p.join(SKILL_FILE).is_file())
            .collect();
        dirs.sort();
        let mut seen_in_root: Vec<String> = Vec::new();
        for dir in dirs {
            let skill = match read_skill(&dir, root.source) {
                Ok(s) => s,
                Err((kind, name, message)) => {
                    diags.push(SkillDiagnostic {
                        kind,
                        source: root.source,
                        path: dir,
                        name,
                        skipped: true,
                        message,
                    });
                    continue;
                }
            };
            let mut diag = |kind, skipped, message: String| {
                diags.push(SkillDiagnostic {
                    kind,
                    source: root.source,
                    path: dir.clone(),
                    name: Some(skill.name.clone()),
                    skipped,
                    message,
                })
            };
            if seen_in_root.contains(&skill.name) {
                diag(
                    DiagnosticKind::DuplicateInRoot,
                    true,
                    format!(
                        "another folder of this root already declares `{}`",
                        skill.name
                    ),
                );
                continue;
            }
            seen_in_root.push(skill.name.clone());
            if let Some(existing) = self.skills.get(&skill.name) {
                if existing.source == SkillSource::Managed && skill.source != SkillSource::Managed {
                    diag(
                        DiagnosticKind::ManagedOverride,
                        true,
                        format!(
                            "`{}` is a managed skill and cannot be overridden",
                            skill.name
                        ),
                    );
                    continue;
                }
                diag(
                    DiagnosticKind::Overrides,
                    false,
                    format!(
                        "replaces the {} skill `{}` at {}",
                        existing.source.as_str(),
                        skill.name,
                        existing.dir.display()
                    ),
                );
            }
            if dir.file_name().is_some_and(|f| f != skill.name.as_str()) {
                diag(
                    DiagnosticKind::NameMismatch,
                    false,
                    format!("folder name differs from the skill name `{}`", skill.name),
                );
            }
            self.skills.insert(skill.name.clone(), skill);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    /// All skills, sorted by name.
    pub fn list(&self) -> Vec<SkillInfo> {
        self.skills.values().map(Skill::info).collect()
    }

    /// Names of all skills, sorted.
    pub fn names(&self) -> Vec<String> {
        self.skills.keys().cloned().collect()
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Skill> {
        self.skills.values()
    }

    fn require(&self, name: &str) -> Result<&Skill, SkillError> {
        self.skills
            .get(name)
            .ok_or_else(|| SkillError::UnknownSkill(name.to_string()))
    }

    /// The markdown body of a skill's `SKILL.md` (front matter removed), read from disk now.
    pub fn load(&self, name: &str) -> Result<String, SkillError> {
        let skill = self.require(name)?;
        let path = skill.dir.join(SKILL_FILE);
        let text = read_skill_md(&path).map_err(|message| SkillError::Invalid {
            skill: name.to_string(),
            message,
        })?;
        let (_, body) = parse_skill_md(&text).map_err(|e| SkillError::Invalid {
            skill: name.to_string(),
            message: e.to_string(),
        })?;
        Ok(body.to_string())
    }

    /// Resolve a path inside a skill folder to an existing file. Rejects absolute paths,
    /// `..`, and anything that leaves the folder once symbolic links are resolved.
    pub fn resolve(&self, name: &str, rel_path: &str) -> Result<PathBuf, SkillError> {
        let skill = self.require(name)?;
        let parts = safe_components(rel_path)?;
        let not_found = || SkillError::NotFound {
            skill: name.to_string(),
            path: rel_path.to_string(),
        };
        let joined = parts.iter().fold(skill.dir.clone(), |p, c| p.join(c));
        let canonical = match std::fs::canonicalize(&joined) {
            Ok(p) => p,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(not_found()),
            Err(e) => return Err(io_err(&joined, e)),
        };
        // `skill.dir` was canonicalized at discovery; do it again in case it moved.
        let root = std::fs::canonicalize(&skill.dir).map_err(|e| io_err(&skill.dir, e))?;
        if !canonical.starts_with(&root) || canonical == root {
            return Err(SkillError::InvalidPath {
                path: rel_path.to_string(),
                reason: "resolves outside the skill folder",
            });
        }
        if !canonical.is_file() {
            return Err(not_found());
        }
        Ok(canonical)
    }

    /// Read a file of a skill folder: text up to the text cap, PNG, JPEG, GIF, and WebP
    /// images up to the image cap. Over the cap nothing is read and `TooLarge` is returned.
    pub fn read_resource(&self, name: &str, rel_path: &str) -> Result<Resource, SkillError> {
        let path = self.resolve(name, rel_path)?;
        let size = std::fs::metadata(&path)
            .map_err(|e| io_err(&path, e))?
            .len();
        let image = image_media_type(&path);
        let limit = if image.is_some() {
            self.limits.max_image_bytes
        } else {
            self.limits.max_text_bytes
        };
        if size > limit {
            return Ok(Resource::TooLarge { size, limit });
        }
        let bytes = std::fs::read(&path).map_err(|e| io_err(&path, e))?;
        match image {
            Some(media_type) => Ok(Resource::Image {
                media_type: media_type.to_string(),
                bytes,
            }),
            None => String::from_utf8(bytes)
                .map(Resource::Text)
                .map_err(|_| SkillError::NotText {
                    skill: name.to_string(),
                    path: rel_path.to_string(),
                }),
        }
    }

    /// Absolute path of a runnable script (`scripts/<file>.py`; the `scripts/` prefix may be
    /// omitted). Only files directly under `scripts/` with extension `.py` qualify.
    pub fn script_path(&self, name: &str, script: &str) -> Result<PathBuf, SkillError> {
        let not_script = || SkillError::NotAScript {
            skill: name.to_string(),
            script: script.to_string(),
        };
        let mut parts = safe_components(script)?;
        if parts.len() == 1 {
            parts.insert(0, SCRIPTS_DIR.to_string());
        }
        if parts.len() != 2 || parts[0] != SCRIPTS_DIR || !parts[1].ends_with(".py") {
            return Err(not_script());
        }
        let path = self.resolve(name, &parts.join("/"))?;
        // A link in `scripts/` must not run a file that lives elsewhere in the folder.
        let in_scripts = path
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|d| d == SCRIPTS_DIR)
            && path.extension().is_some_and(|e| e == "py");
        if !in_scripts {
            return Err(not_script());
        }
        Ok(path)
    }

    /// The system prompt block: names and descriptions of the enabled skills, nothing else.
    /// Names that are not in the set are ignored; the result is empty when no enabled skill
    /// exists.
    pub fn system_prompt_block<S: AsRef<str>>(&self, enabled: &[S]) -> String {
        let mut lines = Vec::new();
        for skill in self.skills.values() {
            if !enabled.iter().any(|e| e.as_ref() == skill.name) {
                continue;
            }
            let description = skill.description.split_whitespace().collect::<Vec<_>>();
            if description.is_empty() {
                lines.push(format!("- {}", skill.name));
            } else {
                lines.push(format!("- {}: {}", skill.name, description.join(" ")));
            }
        }
        if lines.is_empty() {
            return String::new();
        }
        format!(
            "SKILLS (name: description). Only these summaries are in context; load a skill's full instructions on demand before relying on it.\n{}\n",
            lines.join("\n")
        )
    }

    /// Legacy skills active for a turn (see `Skill::matches`), sorted by name.
    pub fn matching<S: AsRef<str>>(&self, user_text: &str, paths: &[S]) -> Vec<&Skill> {
        self.skills
            .values()
            .filter(|s| s.matches(user_text, paths))
            .collect()
    }
}

fn read_skill_md(path: &Path) -> Result<String, String> {
    let size = std::fs::metadata(path)
        .map_err(|e| format!("cannot read {SKILL_FILE}: {e}"))?
        .len();
    if size > MAX_SKILL_MD_BYTES {
        return Err(format!(
            "{SKILL_FILE} is {size} bytes; the limit is {MAX_SKILL_MD_BYTES}"
        ));
    }
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {SKILL_FILE}: {e}"))
}

type SkillReadError = (DiagnosticKind, Option<String>, String);

fn read_skill(dir: &Path, source: SkillSource) -> Result<Skill, SkillReadError> {
    let text =
        read_skill_md(&dir.join(SKILL_FILE)).map_err(|m| (DiagnosticKind::Unreadable, None, m))?;
    let (fm, _body) =
        parse_skill_md(&text).map_err(|e| (DiagnosticKind::BadFrontMatter, None, e.to_string()))?;
    let name = fm.name.clone().unwrap_or_default();
    if !is_valid_name(&name) {
        return Err((
            DiagnosticKind::InvalidName,
            fm.name.clone(),
            if name.is_empty() {
                "front matter has no `name`".to_string()
            } else {
                format!("name `{name}` must match ^[a-z0-9-]{{1,64}}$")
            },
        ));
    }
    let description = fm
        .description
        .clone()
        .unwrap_or_default()
        .trim()
        .to_string();
    let legacy = fm.is_legacy();
    if description.is_empty() && !legacy {
        return Err((
            DiagnosticKind::InvalidDescription,
            Some(name),
            "front matter has no `description`".to_string(),
        ));
    }
    let chars = description.chars().count();
    if chars > MAX_DESCRIPTION_CHARS {
        return Err((
            DiagnosticKind::InvalidDescription,
            Some(name),
            format!("description has {chars} characters; the limit is {MAX_DESCRIPTION_CHARS}"),
        ));
    }
    let dir = std::fs::canonicalize(dir).map_err(|e| {
        (
            DiagnosticKind::Unreadable,
            Some(name.clone()),
            format!("cannot resolve the skill folder: {e}"),
        )
    })?;
    let (scripts, resources) = list_files(&dir);
    Ok(Skill {
        name,
        description,
        source,
        dir,
        license: fm.license.clone(),
        allowed_tools: fm.allowed_tools(),
        triggers: fm.triggers.clone().unwrap_or_default(),
        paths: fm.paths.clone().unwrap_or_default(),
        always: fm.always.unwrap_or(false),
        scripts,
        resources,
    })
}

#[cfg(test)]
mod tests;

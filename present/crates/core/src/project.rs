//! Creating and opening a deck folder (`<repo>/decks/<slug>/`): `deck/`, `template/`,
//! `references/`, `assets/`, `exports/`, and the deck config under `.present/`.

use crate::deck_store::DeckStore;
use crate::util::{now_iso, write_atomic};
use analyst_deck_protocol::config::ProjectConfig;
use analyst_deck_protocol::deck::{Deck, DeckMode, Section};
use analyst_deck_protocol::SCHEMA_VERSION;
use anyhow::{anyhow, Context};
use std::path::{Path, PathBuf};

pub const DIRS: &[&str] = &[
    ".present",
    ".present/skills",
    ".present/cache",
    "deck/slides",
    "template",
    "references",
    "assets",
    "exports",
];

#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
}

impl Project {
    /// Create the project skeleton and copy the template (and its sidecar) in.
    pub fn create(
        root: &Path,
        name: &str,
        template: &Path,
        mode: DeckMode,
    ) -> anyhow::Result<Project> {
        if root.join("deck").join("deck.json").exists() {
            return Err(anyhow!("{} already contains a deck", root.display()));
        }
        let ext = template
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if ext != "pptx" && ext != "potx" {
            return Err(anyhow!("the template must be a .pptx or .potx file"));
        }
        for dir in DIRS {
            std::fs::create_dir_all(root.join(dir))?;
        }
        let file_name = template
            .file_name()
            .ok_or_else(|| anyhow!("template has no file name"))?;
        let target = root.join("template").join(file_name);
        std::fs::copy(template, &target)
            .with_context(|| format!("copying {}", template.display()))?;
        if let Some(dir) = template.parent() {
            let sidecar = dir.join("template.toml");
            if sidecar.is_file() {
                std::fs::copy(&sidecar, root.join("template").join("template.toml"))?;
            }
        }
        let deck = Deck {
            schema_version: SCHEMA_VERSION,
            mode,
            title: name.to_string(),
            author: None,
            template: format!("template/{}", file_name.to_string_lossy()),
            slide_size: None,
            sections: vec![Section {
                title: String::new(),
                slides: Vec::new(),
            }],
            footer: None,
            classification: None,
            brief: None,
            properties: None,
        };
        DeckStore::new(root).write_deck(&deck)?;
        let mut config = ProjectConfig::default();
        config.project.name = name.to_string();
        config.project.created = now_iso();
        crate::deck_config::save(root, &config)?;
        write_atomic(&root.join(".gitignore"), b".present/cache/\nexports/\n")?;
        Ok(Project {
            root: root.to_path_buf(),
        })
    }

    pub fn open(root: &Path) -> anyhow::Result<Project> {
        if !root.join("deck").join("deck.json").is_file() {
            return Err(anyhow!(
                "{} is not a deck (deck/deck.json is missing)",
                root.display()
            ));
        }
        for dir in DIRS {
            std::fs::create_dir_all(root.join(dir))?;
        }
        Ok(Project {
            root: root.to_path_buf(),
        })
    }

    pub fn deck(&self) -> DeckStore {
        DeckStore::new(&self.root)
    }

    pub fn config(&self) -> anyhow::Result<ProjectConfig> {
        crate::deck_config::load(&self.root)
    }
}

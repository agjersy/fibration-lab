//! The deck document on disk: `deck/deck.json` and `deck/slides/<id>.json` (PROTOCOL 2.1).
//!
//! Every write is atomic and every slide carries a content hash so that agent proposals and
//! user edits can detect that the slide changed underneath them (ARCHITECTURE 13).

use crate::util::{new_id, sha256_hex, to_json_pretty, write_atomic};
use present_protocol::deck::{Deck, DeckMode, Section, Slide};
use present_protocol::ids::SlideId;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum DeckError {
    #[error("slide {0} not found")]
    NotFound(String),
    #[error("slide {0} changed since it was read")]
    Conflict(String),
    #[error("slide {0} is locked")]
    Locked(String),
    #[error("{0}")]
    Invalid(String),
    #[error("deck I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("deck JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Where a slide goes: after another slide, or at the end of a section (the last by default).
#[derive(Debug, Clone, Default)]
pub struct Position {
    pub after: Option<SlideId>,
    pub section: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DeckStore {
    root: PathBuf,
}

impl DeckStore {
    pub fn new(project: &Path) -> DeckStore {
        DeckStore {
            root: project.to_path_buf(),
        }
    }

    pub fn deck_path(&self) -> PathBuf {
        self.root.join("deck").join("deck.json")
    }

    pub fn slide_path(&self, id: &SlideId) -> PathBuf {
        self.root
            .join("deck")
            .join("slides")
            .join(format!("{id}.json"))
    }

    /// Whether a slide file exists for an id given as text (false for anything that is not an id).
    pub fn slide_path_exists(&self, id: &str) -> bool {
        id.parse::<SlideId>()
            .map(|sid| self.slide_path(&sid).is_file())
            .unwrap_or(false)
    }

    pub fn exists(&self) -> bool {
        self.deck_path().is_file()
    }

    pub fn read_deck(&self) -> Result<Deck, DeckError> {
        Ok(serde_json::from_slice(&std::fs::read(self.deck_path())?)?)
    }

    pub fn write_deck(&self, deck: &Deck) -> Result<(), DeckError> {
        write_atomic(&self.deck_path(), to_json_pretty(deck)?.as_bytes())?;
        Ok(())
    }

    /// A slide with the hash of its file content.
    pub fn read_slide(&self, id: &SlideId) -> Result<(Slide, String), DeckError> {
        let path = self.slide_path(id);
        if !path.is_file() {
            return Err(DeckError::NotFound(id.to_string()));
        }
        let bytes = std::fs::read(path)?;
        Ok((serde_json::from_slice(&bytes)?, sha256_hex(&bytes)))
    }

    pub fn slide_hash(&self, id: &SlideId) -> Result<String, DeckError> {
        let path = self.slide_path(id);
        if !path.is_file() {
            return Err(DeckError::NotFound(id.to_string()));
        }
        Ok(sha256_hex(&std::fs::read(path)?))
    }

    /// All slides in deck order.
    pub fn read_all(&self) -> Result<(Deck, Vec<Slide>), DeckError> {
        let deck = self.read_deck()?;
        let slides = deck
            .slide_ids()
            .map(|id| self.read_slide(id).map(|(s, _)| s))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((deck, slides))
    }

    fn write_slide_file(&self, slide: &Slide) -> Result<String, DeckError> {
        let text = to_json_pretty(slide)?;
        write_atomic(&self.slide_path(&slide.id), text.as_bytes())?;
        Ok(sha256_hex(text.as_bytes()))
    }

    pub fn new_slide_id(&self) -> SlideId {
        loop {
            let candidate = new_id("s", 6);
            if let Ok(id) = SlideId::new(candidate) {
                if !self.slide_path(&id).exists() {
                    return id;
                }
            }
        }
    }

    fn place(deck: &mut Deck, id: &SlideId, pos: &Position) -> Result<(), DeckError> {
        if let Some(after) = &pos.after {
            for section in deck.sections.iter_mut() {
                if let Some(i) = section.slides.iter().position(|s| s == after) {
                    section.slides.insert(i + 1, id.clone());
                    return Ok(());
                }
            }
            return Err(DeckError::NotFound(after.to_string()));
        }
        if let Some(title) = &pos.section {
            if let Some(section) = deck.sections.iter_mut().find(|s| &s.title == title) {
                section.slides.push(id.clone());
            } else {
                deck.sections.push(Section {
                    title: title.clone(),
                    slides: vec![id.clone()],
                });
            }
            return Ok(());
        }
        if deck.sections.is_empty() {
            deck.sections.push(Section {
                title: String::new(),
                slides: Vec::new(),
            });
        }
        if let Some(last) = deck.sections.last_mut() {
            last.slides.push(id.clone());
        }
        Ok(())
    }

    /// Add a slide; returns its hash.
    pub fn create_slide(&self, slide: &Slide, pos: &Position) -> Result<String, DeckError> {
        let mut deck = self.read_deck()?;
        if deck.slide_ids().any(|s| s == &slide.id) {
            return Err(DeckError::Invalid(format!(
                "slide {} already exists",
                slide.id
            )));
        }
        Self::place(&mut deck, &slide.id, pos)?;
        let hash = self.write_slide_file(slide)?;
        self.write_deck(&deck)?;
        Ok(hash)
    }

    /// Replace a slide. `expected_hash` guards against a concurrent change.
    pub fn update_slide(
        &self,
        slide: &Slide,
        expected_hash: Option<&str>,
    ) -> Result<String, DeckError> {
        let current = self.slide_hash(&slide.id)?;
        if let Some(expected) = expected_hash {
            if expected != current {
                return Err(DeckError::Conflict(slide.id.to_string()));
            }
        }
        self.write_slide_file(slide)
    }

    pub fn delete_slide(&self, id: &SlideId) -> Result<(), DeckError> {
        let mut deck = self.read_deck()?;
        let before = deck.slide_ids().count();
        for section in deck.sections.iter_mut() {
            section.slides.retain(|s| s != id);
        }
        if deck.slide_ids().count() == before {
            return Err(DeckError::NotFound(id.to_string()));
        }
        self.write_deck(&deck)?;
        let _ = std::fs::remove_file(self.slide_path(id));
        Ok(())
    }

    pub fn move_slide(&self, id: &SlideId, pos: &Position) -> Result<(), DeckError> {
        let mut deck = self.read_deck()?;
        if !deck.slide_ids().any(|s| s == id) {
            return Err(DeckError::NotFound(id.to_string()));
        }
        if pos.after.as_ref() == Some(id) {
            return Ok(());
        }
        for section in deck.sections.iter_mut() {
            section.slides.retain(|s| s != id);
        }
        if pos.after.is_none() && pos.section.is_none() {
            // Moving with no anchor means "to the front".
            if deck.sections.is_empty() {
                deck.sections.push(Section {
                    title: String::new(),
                    slides: Vec::new(),
                });
            }
            deck.sections[0].slides.insert(0, id.clone());
        } else {
            Self::place(&mut deck, id, pos)?;
        }
        self.write_deck(&deck)?;
        Ok(())
    }

    pub fn set_mode(&self, mode: DeckMode) -> Result<(), DeckError> {
        let mut deck = self.read_deck()?;
        deck.mode = mode;
        self.write_deck(&deck)
    }

    /// Project-relative paths of every file that makes up the deck in its current mode.
    pub fn deck_files(&self) -> Result<Vec<String>, DeckError> {
        let deck = self.read_deck()?;
        let mut files = vec!["deck/deck.json".to_string()];
        files.extend(deck.slide_ids().map(|id| format!("deck/slides/{id}.json")));
        let deck_dir = self.root.join("deck");
        if let Ok(entries) = std::fs::read_dir(&deck_dir) {
            let mut scripts: Vec<String> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|x| x == "py"))
                .map(|e| format!("deck/{}", e.file_name().to_string_lossy()))
                .collect();
            scripts.sort();
            files.extend(scripts);
        }
        Ok(files)
    }
}

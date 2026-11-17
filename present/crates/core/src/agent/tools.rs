//! The agent's tool registry (PROTOCOL 6.3): names, policy classes, modes, and JSON schemas.

use serde_json::{json, Value};
use wb_providers::model::ToolDef;
use wb_providers::names::to_model_name;

/// Conversation modes (ARCHITECTURE 7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Outline,
    Build,
    Polish,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Outline => "outline",
            Mode::Build => "build",
            Mode::Polish => "polish",
        }
    }
}

impl std::str::FromStr for Mode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "outline" => Ok(Mode::Outline),
            "build" => Ok(Mode::Build),
            "polish" => Ok(Mode::Polish),
            other => Err(format!("unknown mode {other:?} (outline, build, polish)")),
        }
    }
}

pub const CLASS_READ: &str = "read";
pub const CLASS_EDIT: &str = "edit";
pub const CLASS_CODE: &str = "code";
pub const CLASS_SKILLS: &str = "skills_scripts";
pub const CLASS_EXPORT: &str = "export";
/// Agent bookkeeping: always allowed, never gated.
pub const CLASS_META: &str = "meta";

const ALL: &[Mode] = &[Mode::Outline, Mode::Build, Mode::Polish];
const BUILD: &[Mode] = &[Mode::Build];
const EDITING: &[Mode] = &[Mode::Build, Mode::Polish];

#[derive(Debug, Clone)]
pub struct ToolSpec {
    /// Internal dotted name (`slide.propose_update`).
    pub name: &'static str,
    pub class: &'static str,
    pub modes: &'static [Mode],
    pub description: &'static str,
    pub parameters: Value,
}

impl ToolSpec {
    pub fn def(&self) -> ToolDef {
        ToolDef {
            name: to_model_name(self.name),
            description: self.description.to_string(),
            parameters: self.parameters.clone(),
        }
    }
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

fn cite_prop() -> Value {
    json!({"type": "string", "description": "A citation: ref:<ref_id>@<version>#<loc>, exactly as returned by refs_search or refs_list."})
}

fn slide_prop() -> Value {
    json!({"type": "object", "description": "A slide document: {id?, layout, placeholders: {<key>: content}, elements?: [], notes?: {paragraphs}}. Content is one of {type:'text', text:{paragraphs:[{level?, runs:[{t, b?, i?, cite?:[...]}]}]}}, {type:'chart', chart:{type, source:{kind:'ref', cite} | {kind:'inline', table, cites}, steps?, series_from:'columns'|'rows', category?, series?, title?, labels?}}, {type:'table', table:{columns, header_row, banded_rows, rows:[{cells:[{text}]}]}}, {type:'image', asset, alt}, {type:'diagram', diagram:{family, variant, ...}}. Element ids are assigned for you when omitted."})
}

pub fn registry() -> Vec<ToolSpec> {
    let slide_id = json!({"type": "string", "description": "A slide id such as s-7f3a2k."});
    vec![
        ToolSpec { name: "deck.outline", class: CLASS_READ, modes: ALL, description: "The deck in one line per slide: id, section, layout, title, lock state, and checker status. Start here.", parameters: obj(json!({}), &[]) },
        ToolSpec { name: "deck.read_slide", class: CLASS_READ, modes: ALL, description: "One slide's full document (placeholders, elements, notes) and its content hash.", parameters: obj(json!({"slide_id": slide_id}), &["slide_id"]) },
        ToolSpec { name: "deck.search", class: CLASS_READ, modes: ALL, description: "Find slides whose text or notes contain the query (case-insensitive).", parameters: obj(json!({"query": {"type": "string"}}), &["query"]) },
        ToolSpec { name: "template.layouts", class: CLASS_READ, modes: ALL, description: "The template's layout catalog: each usable layout with what it is for and its placeholder keys, what each accepts, and roughly how much text it holds.", parameters: obj(json!({}), &[]) },
        ToolSpec { name: "template.layout_detail", class: CLASS_READ, modes: ALL, description: "Full placeholder geometry and text styles of one layout.", parameters: obj(json!({"layout": {"type": "string"}}), &["layout"]) },
        ToolSpec { name: "refs.list", class: CLASS_READ, modes: ALL, description: "The project's references with their ids, versions, kinds, and outlines.", parameters: obj(json!({}), &[]) },
        ToolSpec { name: "refs.search", class: CLASS_READ, modes: ALL, description: "Search the references. Returns passages with citations you can pass to refs_read and put on runs.", parameters: obj(json!({"query": {"type": "string"}, "ref_ids": {"type": "array", "items": {"type": "string"}}, "k": {"type": "integer", "minimum": 1, "maximum": 20}}), &["query"]) },
        ToolSpec { name: "refs.read", class: CLASS_READ, modes: ALL, description: "Read the passage a citation points at (a page, a character range, a slide, a sheet range).", parameters: obj(json!({"cite": cite_prop(), "max_chars": {"type": "integer", "minimum": 100, "maximum": 12000}}), &["cite"]) },
        ToolSpec { name: "refs.table", class: CLASS_READ, modes: ALL, description: "Read a table from a reference: a sheet range, a named range, a CSV row range, or an extracted table.", parameters: obj(json!({"cite": cite_prop()}), &["cite"]) },
        ToolSpec { name: "refs.image", class: CLASS_READ, modes: ALL, description: "Look at a page of a PDF reference as an image (for scanned pages, figures, and layout).", parameters: obj(json!({"ref_id": {"type": "string"}, "page": {"type": "integer", "minimum": 1}}), &["ref_id", "page"]) },
        ToolSpec { name: "slide.render", class: CLASS_READ, modes: ALL, description: "Render one slide and look at it. Do this for every slide you create or change.", parameters: obj(json!({"slide_id": slide_id}), &["slide_id"]) },
        ToolSpec { name: "check.run", class: CLASS_READ, modes: ALL, description: "Run the deck checker on the whole deck or on some slides.", parameters: obj(json!({"slide_ids": {"type": "array", "items": {"type": "string"}}}), &[]) },
        ToolSpec { name: "chart.preview", class: CLASS_READ, modes: ALL, description: "Compute a chart's data: the source table after the transform steps, without changing the deck.", parameters: obj(json!({"source": {"type": "object"}, "steps": {"type": "array", "items": {"type": "object"}}}), &["source"]) },
        ToolSpec { name: "skill.load", class: CLASS_READ, modes: ALL, description: "Load the full instructions of a skill listed under SKILLS.", parameters: obj(json!({"name": {"type": "string"}}), &["name"]) },
        ToolSpec { name: "skill.read_resource", class: CLASS_READ, modes: ALL, description: "Read a file that belongs to a skill.", parameters: obj(json!({"name": {"type": "string"}, "path": {"type": "string"}}), &["name", "path"]) },
        ToolSpec { name: "scope.declare", class: CLASS_META, modes: EDITING, description: "Declare which existing slides you will change this turn and how many new slides you will add, with a one-paragraph plan. Required before your first change; changes outside the declared scope need the user's approval.", parameters: obj(json!({"slide_ids": {"type": "array", "items": {"type": "string"}}, "new_slides": {"type": "integer", "minimum": 0}, "plan": {"type": "string"}}), &["slide_ids", "plan"]) },
        ToolSpec { name: "facts.pin", class: CLASS_META, modes: ALL, description: "Record a fact to keep across the conversation (a decision, a figure, a name) so it survives history compaction.", parameters: obj(json!({"fact": {"type": "string"}}), &["fact"]) },
        ToolSpec { name: "outline.propose", class: CLASS_EDIT, modes: &[Mode::Outline, Mode::Build], description: "Propose the storyline: one row per slide with its action title, the message it proves, the intended layout, and the citations that support it. Nothing is rendered; the user edits and approves the outline.", parameters: obj(json!({"rows": {"type": "array", "items": obj(json!({"slide_id": {"type": "string"}, "title": {"type": "string"}, "message": {"type": "string"}, "layout": {"type": "string"}, "sources": {"type": "array", "items": {"type": "string"}}}), &["title", "message"])}}), &["rows"]) },
        ToolSpec { name: "slide.propose_create", class: CLASS_EDIT, modes: BUILD, description: "Propose a new slide, placed after a slide or at the end of a section.", parameters: obj(json!({"slide": slide_prop(), "after": slide_id, "section": {"type": "string"}}), &["slide"]) },
        ToolSpec { name: "slide.propose_update", class: CLASS_EDIT, modes: EDITING, description: "Propose a change to an existing slide: either the whole new slide document, or an RFC 6902 JSON Patch against the document you read.", parameters: obj(json!({"slide_id": slide_id, "slide": slide_prop(), "patch": {"type": "array", "items": {"type": "object"}}}), &["slide_id"]) },
        ToolSpec { name: "slide.propose_delete", class: CLASS_EDIT, modes: BUILD, description: "Propose deleting a slide.", parameters: obj(json!({"slide_id": slide_id}), &["slide_id"]) },
        ToolSpec { name: "slide.propose_move", class: CLASS_EDIT, modes: EDITING, description: "Propose moving a slide after another slide or to the end of a section.", parameters: obj(json!({"slide_id": slide_id, "after": slide_id, "section": {"type": "string"}}), &["slide_id"]) },
        ToolSpec { name: "asset.import", class: CLASS_EDIT, modes: BUILD, description: "Copy an image from a reference (a PDF page figure, an image reference, a Personal Analyst figure) into assets/ so a slide can use it. Returns the asset path and its citation.", parameters: obj(json!({"cite": cite_prop()}), &["cite"]) },
        ToolSpec { name: "code.read", class: CLASS_READ, modes: ALL, description: "Read the deck build script (code mode) or another file under deck/.", parameters: obj(json!({"path": {"type": "string"}}), &[]) },
        ToolSpec { name: "code.propose_edit", class: CLASS_EDIT, modes: EDITING, description: "Propose an edit to the build script: either exact replacements (each `old` must match once) or the whole new content.", parameters: obj(json!({"path": {"type": "string"}, "edits": {"type": "array", "items": obj(json!({"old": {"type": "string"}, "new": {"type": "string"}}), &["old", "new"])}, "content": {"type": "string"}}), &[]) },
        ToolSpec { name: "deck.build", class: CLASS_CODE, modes: EDITING, description: "Run the build script in the sandbox and report its output and the checker's findings (code mode).", parameters: obj(json!({}), &[]) },
        ToolSpec { name: "deck.switch_mode", class: CLASS_EDIT, modes: BUILD, description: "Switch the project to code generation mode: the deck becomes an equivalent build script. Only when the structured deck cannot express what is needed. Always asks the user.", parameters: obj(json!({"to": {"type": "string", "enum": ["code"]}, "reason": {"type": "string"}}), &["to", "reason"]) },
        ToolSpec { name: "skill.run_script", class: CLASS_SKILLS, modes: EDITING, description: "Run a script that belongs to a skill, in the sandbox. Files it writes are returned as project paths.", parameters: obj(json!({"name": {"type": "string"}, "script": {"type": "string"}, "args": {"type": "array", "items": {"type": "string"}}}), &["name", "script"]) },
        ToolSpec { name: "export.run", class: CLASS_EXPORT, modes: ALL, description: "Export the deck: pptx, pdf, png, md (outline), or notes-docx (speaker notes).", parameters: obj(json!({"format": {"type": "string", "enum": ["pptx", "pdf", "png", "md", "notes-docx"]}}), &["format"]) },
    ]
}

pub fn find(name: &str) -> Option<ToolSpec> {
    registry().into_iter().find(|t| t.name == name)
}

/// The internal name for a model-facing name (`slide_propose_update` -> `slide.propose_update`).
pub fn internal_name(model_name: &str) -> Option<&'static str> {
    registry()
        .into_iter()
        .map(|t| t.name)
        .find(|n| to_model_name(n) == model_name || *n == model_name)
}

/// Tool definitions offered to the model in a mode. Code tools appear only in code mode and
/// slide-document tools only in structured mode, so the model is never offered a tool that
/// cannot work on the deck in front of it.
pub fn defs_for(mode: Mode, code_mode: bool) -> Vec<ToolDef> {
    registry()
        .iter()
        .filter(|t| t.modes.contains(&mode))
        .filter(|t| match t.name {
            "code.read" | "code.propose_edit" | "deck.build" => code_mode,
            "slide.propose_create"
            | "slide.propose_update"
            | "slide.propose_delete"
            | "slide.propose_move"
            | "outline.propose"
            | "deck.switch_mode" => !code_mode,
            _ => true,
        })
        .map(ToolSpec::def)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wb_providers::names::is_valid_model_name;

    #[test]
    fn registry_is_well_formed() {
        let reg = registry();
        let mut names = std::collections::BTreeSet::new();
        for t in &reg {
            assert!(names.insert(t.name), "duplicate {}", t.name);
            assert!(is_valid_model_name(&to_model_name(t.name)), "{}", t.name);
            assert_eq!(t.parameters["type"], "object");
            assert!(!t.description.is_empty() && !t.modes.is_empty());
            assert_eq!(internal_name(&to_model_name(t.name)), Some(t.name));
        }
        for required in [
            "deck.outline",
            "refs.search",
            "slide.propose_update",
            "slide.render",
            "check.run",
            "export.run",
        ] {
            assert!(names.contains(required));
        }
    }

    #[test]
    fn modes_limit_the_tools_offered() {
        let names = |mode, code| {
            defs_for(mode, code)
                .into_iter()
                .map(|d| d.name)
                .collect::<Vec<_>>()
        };
        let outline = names(Mode::Outline, false);
        assert!(
            outline.contains(&"outline_propose".to_string())
                && !outline.contains(&"slide_propose_create".to_string())
        );
        let polish = names(Mode::Polish, false);
        assert!(polish.contains(&"slide_propose_update".to_string()));
        assert!(
            !polish.contains(&"slide_propose_create".to_string())
                && !polish.contains(&"slide_propose_delete".to_string())
        );
        let code = names(Mode::Build, true);
        assert!(
            code.contains(&"code_propose_edit".to_string())
                && !code.contains(&"slide_propose_update".to_string())
        );
        assert!(!names(Mode::Build, false).contains(&"deck_build".to_string()));
    }
}

//! The one tool registry of the agent loop: the analysis tools and the deck tools, with
//! policy classes and model-facing JSON schemas. Which mode offers which tool is decided by
//! the policy's mode rules ([`crate::tools::gate::mode_rules`]), not here.

use analyst_protocol::evals::Mode;
use serde_json::{json, Value};
use wb_providers::model::ToolDef;
use wb_providers::names::to_model_name;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolClass {
    Read,
    Edit,
    Run,
    Package,
    Shell,
    Git,
    /// Agent bookkeeping (`declare_scope`, `pin_fact`): always allowed, never gated.
    Meta,
}

#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub class: ToolClass,
    pub description: &'static str,
    pub parameters: Value,
    /// A deck tool (implemented by the deck services); offered when the project has a deck.
    pub deck: bool,
}

/// `slide.place_figure`: place a figure of an analysis run on a slide, cited by its run.
pub const PLACE_FIGURE: &str = "slide.place_figure";

/// `slide.place_equation`: place the equation of a function on a slide, with its equation record.
pub const PLACE_EQUATION: &str = "slide.place_equation";

/// The tools that place analysis results on slides (structured decks only).
pub const PLACE_TOOLS: [&str; 2] = [PLACE_FIGURE, PLACE_EQUATION];

/// Which kinds of decks the project has; the deck tools offered follow from it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeckKinds {
    pub structured: bool,
    pub code: bool,
}

impl DeckKinds {
    pub fn any(self) -> bool {
        self.structured || self.code
    }
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

/// The policy class of a deck tool: slide and script changes are edits; deck builds, skill
/// scripts, and exports are runs of what exists.
fn deck_tool_class(class: &str) -> ToolClass {
    use analyst_deck::tools as deck;
    match class {
        deck::CLASS_READ => ToolClass::Read,
        deck::CLASS_EDIT => ToolClass::Edit,
        deck::CLASS_CODE | deck::CLASS_SKILLS | deck::CLASS_EXPORT => ToolClass::Run,
        _ => ToolClass::Read,
    }
}

fn all() -> &'static [ToolSpec] {
    static ALL: std::sync::OnceLock<Vec<ToolSpec>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| {
        let mut tools = analysis_tools();
        tools.extend(
            analyst_deck::tools::registry()
                .into_iter()
                .map(|t| ToolSpec {
                    name: t.name,
                    class: deck_tool_class(t.class),
                    description: t.description,
                    parameters: t.parameters,
                    deck: true,
                }),
        );
        // The tool that joins the two workbenches: a slide cites an analysis run directly.
        tools.push(ToolSpec {
            deck: true,
            name: PLACE_FIGURE,
            class: ToolClass::Edit,
            description: "Propose placing a figure of an analysis run on a slide: name the run and the artifact path as artifacts_list shows them. The image is copied into the deck's assets and the element cites the run (run id, artifact, script, arguments, tree identity), so the checker can tell when the code has changed since.",
            parameters: obj(
                json!({"deck": {"type": "string", "description": "The deck's slug under decks/. May be omitted when the project has exactly one deck."}, "slide_id": {"type": "string"}, "run_id": {"type": "string"}, "artifact": {"type": "string", "description": "The figure's repo-relative path, as artifacts_list returns it."}, "placeholder": {"type": "string", "description": "The placeholder key to fill; by default the first empty placeholder of the slide's layout that accepts an image."}, "alt": {"type": "string", "description": "Alt text; by default the figure's title."}}),
                &["slide_id", "run_id", "artifact"],
            ),
        });
        tools.push(ToolSpec {
            deck: true,
            name: PLACE_EQUATION,
            class: ToolClass::Edit,
            description: "Propose placing the equation of a function on a slide as a native, editable equation: name the source file and the function as eq_extract shows them. The analysis kernel extracts the equation and checks declared ones numerically; the element carries the equation record (LaTeX, tier, check result, function, file, the file's content hash), so the checker flags mismatches and LaTeX-only equations as errors and warns when the source changes.",
            parameters: obj(
                json!({"deck": {"type": "string", "description": "The deck's slug under decks/. May be omitted when the project has exactly one deck."}, "slide_id": {"type": "string"}, "path": {"type": "string", "description": "The repo-relative source file."}, "function": {"type": "string", "description": "The function's name or qualified name."}, "placeholder": {"type": "string", "description": "The placeholder key to fill; by default the first empty non-title placeholder that takes text or an image."}}),
                &["slide_id", "path", "function"],
            ),
        });
        tools
    })
}

/// Every tool of the loop: analysis tools, then deck tools.
pub fn registry() -> Vec<ToolSpec> {
    all().to_vec()
}

fn analysis_tools() -> Vec<ToolSpec> {
    use ToolClass::*;
    let tools: Vec<ToolSpec> = vec![
        ToolSpec { deck: false, name: "declare_scope", class: Meta, description: "Declare what you intend to create or change this turn: repo-relative files, and slides of a deck. Required before the first change; changes outside the declared set need user approval.", parameters: obj(json!({"files": {"type":"array","items":{"type":"string"}}, "slides": {"type":"array","items":{"type":"string"},"description":"Ids of existing slides you will change."}, "new_slides": {"type":"integer","minimum":0,"description":"How many slides you will add."}, "deck": {"type":"string","description":"The deck the slides belong to; may be omitted when the project has exactly one deck."}, "plan": {"type":"string","description":"One short paragraph describing the intended change."}}), &[]) },
        ToolSpec { deck: false, name: "pin_fact", class: Meta, description: "Pin a short fact (decision, file name, parameter value) that must survive context compaction.", parameters: obj(json!({"fact": {"type":"string"}}), &["fact"]) },
        ToolSpec { deck: false, name: "fs.read", class: Read, description: "Read a repo file (optionally a line range). Also reads session artifact files returned by truncated tool results.", parameters: obj(json!({"path":{"type":"string"},"start_line":{"type":"integer","minimum":1},"end_line":{"type":"integer","minimum":1}}), &["path"]) },
        ToolSpec { deck: false, name: "skill.load", class: Read, description: "Load the full instructions of a skill listed under SKILLS in the system prompt. Do this before relying on the skill.", parameters: obj(json!({"name":{"type":"string"}}), &["name"]) },
        ToolSpec { deck: false, name: "fs.list", class: Read, description: "List files and directories under a repo path (depth-limited; ignores .git, .venv, out, __pycache__).", parameters: obj(json!({"path":{"type":"string"},"depth":{"type":"integer","minimum":1,"maximum":6}}), &[]) },
        ToolSpec { deck: false, name: "fs.search", class: Read, description: "Search file contents with a regex (ripgrep). Returns path, line, and text per match.", parameters: obj(json!({"pattern":{"type":"string"},"glob":{"type":"string"},"max_results":{"type":"integer","minimum":1}}), &["pattern"]) },
        ToolSpec { deck: false, name: "scripts.index", class: Read, description: "Index of command-line scripts: name, description, argparse options with defaults.", parameters: obj(json!({}), &[]) },
        ToolSpec { deck: false, name: "lib.index", class: Read, description: "Index of public library functions with signatures, doc summaries, and call sites. Consult before writing a new function.", parameters: obj(json!({}), &[]) },
        ToolSpec { deck: false, name: "runs.list", class: Read, description: "Recent runs on the target, newest first.", parameters: obj(json!({"limit":{"type":"integer","minimum":1}}), &[]) },
        ToolSpec { deck: false, name: "runs.get", class: Read, description: "Metadata of one run.", parameters: obj(json!({"run_id":{"type":"string"}}), &["run_id"]) },
        ToolSpec { deck: false, name: "out.list", class: Read, description: "List output files on the target for a run or path under out/.", parameters: obj(json!({"path":{"type":"string"},"run_id":{"type":"string"}}), &[]) },
        ToolSpec { deck: false, name: "out.read", class: Read, description: "Bounded preview of an output file on the target (head of text/CSV, array summary).", parameters: obj(json!({"path":{"type":"string"},"max_bytes":{"type":"integer","minimum":1}}), &["path"]) },
        ToolSpec { deck: false, name: "eq.extract", class: Read, description: "Extract equations (derived or declared) from a file or function.", parameters: obj(json!({"path":{"type":"string"},"function":{"type":"string"}}), &["path"]) },
        ToolSpec { deck: false, name: "eq.check", class: Read, description: "Numerically check a function against its declared Math: equation.", parameters: obj(json!({"path":{"type":"string"},"function":{"type":"string"},"trials":{"type":"integer"},"tol":{"type":"number"}}), &["path","function"]) },
        ToolSpec { deck: false, name: "contract.check", class: Read, description: "Run the script contract checker (default: all scripts and the library).", parameters: obj(json!({"paths":{"type":"array","items":{"type":"string"}}}), &[]) },
        ToolSpec { deck: false, name: "contract.similar", class: Read, description: "Find duplicate and near-duplicate functions across scripts and the library.", parameters: obj(json!({"threshold":{"type":"number"}}), &[]) },
        ToolSpec { deck: false, name: "lint.run", class: Read, description: "Run ruff on the given paths (default: all).", parameters: obj(json!({"paths":{"type":"array","items":{"type":"string"}}}), &[]) },
        ToolSpec { deck: false, name: "fig.list", class: Read, description: "List figures produced by runs (all, or one run).", parameters: obj(json!({"run_id":{"type":"string"}}), &[]) },
        ToolSpec { deck: false, name: "artifacts.list", class: Read, description: "The artifact index: what the session's runs produced (figures, tables, files), by run id and path, with the script and arguments of the run and whether the code has changed since (stale). Use it to pick a figure for slide_place_figure.", parameters: obj(json!({"run_id":{"type":"string"},"kind":{"type":"string","enum":["figure","table","array","json","text","file"]},"limit":{"type":"integer","minimum":1}}), &[]) },
        ToolSpec { deck: false, name: "target.info", class: Read, description: "Name, kind, sandbox level, and data directories of the execution target.", parameters: obj(json!({}), &[]) },
        ToolSpec { deck: false, name: "fs.propose_edit", class: Edit, description: "Edit a file: either `edits` (each `old` must match exactly once and is replaced by `new`) or full `content`.", parameters: obj(json!({"path":{"type":"string"},"edits":{"type":"array","items":{"type":"object","properties":{"old":{"type":"string"},"new":{"type":"string"}},"required":["old","new"]}},"content":{"type":"string"}}), &["path"]) },
        ToolSpec { deck: false, name: "fs.propose_create", class: Edit, description: "Create a new file with the given content.", parameters: obj(json!({"path":{"type":"string"},"content":{"type":"string"}}), &["path","content"]) },
        ToolSpec { deck: false, name: "fs.propose_delete", class: Edit, description: "Delete a file.", parameters: obj(json!({"path":{"type":"string"}}), &["path"]) },
        ToolSpec { deck: false, name: "run.script", class: Run, description: "Run a script on the target with arguments; returns status, stdout/stderr tails, and artifacts. Runs have no network access.", parameters: obj(json!({"script":{"type":"string"},"args":{"type":"array","items":{"type":"string"}},"timeout_s":{"type":"number"}}), &["script"]) },
        ToolSpec { deck: false, name: "exec.snippet", class: Run, description: "Run a short Python snippet in the session environment (persistent REPL, sandboxed).", parameters: obj(json!({"code":{"type":"string"}}), &["code"]) },
        ToolSpec { deck: false, name: "pkg.add", class: Package, description: "Add packages to the project environment with uv.", parameters: obj(json!({"packages":{"type":"array","items":{"type":"string"}}}), &["packages"]) },
        ToolSpec { deck: false, name: "pkg.remove", class: Package, description: "Remove packages from the project environment with uv.", parameters: obj(json!({"packages":{"type":"array","items":{"type":"string"}}}), &["packages"]) },
        ToolSpec { deck: false, name: "pkg.sync", class: Package, description: "Sync the environment to the lockfile.", parameters: obj(json!({}), &[]) },
        ToolSpec { deck: false, name: "shell.run", class: Shell, description: "Run a shell command inside the target sandbox.", parameters: obj(json!({"command":{"type":"string"},"timeout_s":{"type":"number"}}), &["command"]) },
        ToolSpec { deck: false, name: "git.status", class: Read, description: "git status of the local repo.", parameters: obj(json!({}), &[]) },
        ToolSpec { deck: false, name: "git.diff", class: Read, description: "git diff of the working tree (optionally one path).", parameters: obj(json!({"path":{"type":"string"}}), &[]) },
        ToolSpec { deck: false, name: "git.commit", class: Git, description: "Commit all working tree changes with a message.", parameters: obj(json!({"message":{"type":"string"}}), &["message"]) },
    ];
    tools
}

pub fn find(name: &str) -> Option<ToolSpec> {
    all().iter().find(|t| t.name == name).cloned()
}

pub fn tool_class(name: &str) -> Option<ToolClass> {
    all().iter().find(|t| t.name == name).map(|t| t.class)
}

/// Whether a tool is implemented by the deck services.
pub fn is_deck_tool(name: &str) -> bool {
    all().iter().any(|t| t.name == name && t.deck)
}

/// Internal ids of every tool.
pub fn names() -> Vec<&'static str> {
    all().iter().map(|t| t.name).collect()
}

/// Internal id for a model-facing name.
pub fn internal_name(model_name: &str) -> Option<&'static str> {
    all()
        .iter()
        .map(|t| t.name)
        .find(|n| to_model_name(n) == model_name)
}

/// Tool definitions a mode offers in a project without decks.
pub fn defs_for_mode(mode: Mode) -> Vec<ToolDef> {
    defs_for(mode, DeckKinds::default())
}

/// Tool definitions offered to the model: what the mode's policy rules make available
/// (ARCHITECTURE 5.7), and of the deck tools those that can work on the decks the project
/// has (none without a deck; slide tools for structured decks, script tools for decks in
/// code generation mode).
pub fn defs_for(mode: Mode, decks: DeckKinds) -> Vec<ToolDef> {
    let rules = super::gate::mode_rules(mode);
    all()
        .iter()
        .filter(|t| rules.tool_available(t.name, super::gate::class_name(t.class)))
        .filter(|t| {
            if !t.deck {
                return true;
            }
            if analyst_deck::tools::CODE_MODE_TOOLS.contains(&t.name) {
                decks.code
            } else if analyst_deck::tools::STRUCTURED_TOOLS.contains(&t.name)
                || PLACE_TOOLS.contains(&t.name)
            {
                decks.structured
            } else {
                decks.any()
            }
        })
        .map(|t| t.def())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wb_providers::names::is_valid_model_name;

    #[test]
    fn registry_is_well_formed() {
        let r = registry();
        for t in &r {
            assert!(is_valid_model_name(&t.def().name), "{}", t.name);
            assert_eq!(t.parameters["type"], "object");
        }
        assert_eq!(internal_name("fs_propose_edit"), Some("fs.propose_edit"));
        assert_eq!(tool_class("run.script"), Some(ToolClass::Run));
        let plan: Vec<String> = defs_for_mode(Mode::Plan)
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert!(plan.contains(&"fs_read".to_string()));
        assert!(!plan.contains(&"run_script".to_string()));
        let run: Vec<String> = defs_for_mode(Mode::Run)
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert!(run.contains(&"run_script".to_string()));
        assert!(!run.contains(&"fs_propose_edit".to_string()));
        assert_eq!(
            defs_for_mode(Mode::Build).len(),
            r.iter().filter(|t| !t.deck).count()
        );
        let mut names = std::collections::BTreeSet::new();
        for t in &r {
            assert!(names.insert(t.name), "duplicate tool {}", t.name);
        }
    }

    /// The deck tools are offered when the project has a deck, by what the deck can do.
    #[test]
    fn deck_tools_follow_the_mode_and_the_decks() {
        let names = |mode, decks| -> Vec<String> {
            defs_for(mode, decks).into_iter().map(|d| d.name).collect()
        };
        let structured = DeckKinds {
            structured: true,
            code: false,
        };
        let code = DeckKinds {
            structured: false,
            code: true,
        };
        let has = |v: &[String], n: &str| v.iter().any(|x| x == n);
        // No deck: no deck tools, whatever the mode.
        for mode in Mode::ALL {
            assert!(!has(&names(mode, DeckKinds::default()), "deck_outline"));
        }
        // Build offers both tool sets: one turn can edit a script, run it, and propose a slide.
        let build = names(Mode::Build, structured);
        for n in [
            "fs_propose_edit",
            "run_script",
            "slide_propose_create",
            "deck_outline",
            "export_run",
        ] {
            assert!(has(&build, n), "{n}");
        }
        assert!(!has(&build, "code_propose_edit") && !has(&build, "deck_build"));
        let build_code = names(Mode::Build, code);
        assert!(has(&build_code, "code_propose_edit") && has(&build_code, "deck_build"));
        assert!(!has(&build_code, "slide_propose_update"));
        // Outline: the storyline and nothing else that changes the deck.
        let outline = names(Mode::Outline, structured);
        assert!(has(&outline, "outline_propose") && has(&outline, "refs_search"));
        assert!(!has(&outline, "slide_propose_create") && !has(&outline, "fs_propose_edit"));
        // Slides: every deck tool, no code or run tools.
        let slides = names(Mode::Slides, structured);
        assert!(has(&slides, "slide_propose_create") && has(&slides, "slide_propose_delete"));
        assert!(
            !has(&slides, "fs_propose_edit")
                && !has(&slides, "run_script")
                && !has(&slides, "shell_run")
        );
        // Polish: change and reorder, never add or delete.
        let polish = names(Mode::Polish, structured);
        assert!(has(&polish, "slide_propose_update") && has(&polish, "slide_propose_move"));
        assert!(!has(&polish, "slide_propose_create") && !has(&polish, "slide_propose_delete"));
        assert!(!has(&polish, "outline_propose") && !has(&polish, "fs_propose_edit"));
        // Plan and run stay read-only for decks.
        let plan = names(Mode::Plan, structured);
        assert!(has(&plan, "deck_outline") && !has(&plan, "slide_propose_update"));
    }
}

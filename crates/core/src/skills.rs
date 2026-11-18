//! Skills for the analysis agent, loaded with the shared `wb-skills` crate (ARCHITECTURE
//! 5.16): Agent Skills folders (`<name>/SKILL.md` with `name` and `description`) under
//! `~/.analyst/skills/` (user) and `<repo>/.analyst/skills/` (project; it wins on a name
//! clash). The older front matter keys `triggers`, `paths` and `always` keep working: a
//! skill that carries them is put into the prompt in full when it matches the turn; every
//! other skill is listed by description and loaded on demand with the `skill.load` tool.

use std::path::Path;
use wb_skills::{SkillRoot, SkillSet, SkillSource};

/// Discover the skills visible to a session. Folders that cannot be read are reported in
/// the log and skipped; a missing folder is not an error.
pub fn discover(home: &Path, repo: &Path) -> SkillSet {
    let roots = [
        SkillRoot::new(SkillSource::User, home.join("skills")),
        SkillRoot::new(SkillSource::Project, repo.join(".analyst").join("skills")),
    ];
    let (set, diagnostics) = SkillSet::discover(&roots);
    for d in diagnostics.iter().filter(|d| d.skipped) {
        tracing::warn!("skill at {} skipped: {}", d.path.display(), d.message);
    }
    set
}

/// The skills part of the system prompt for one turn: the list of skills by description,
/// then the full instructions of the skills whose `triggers` occur in the user message,
/// whose `paths` match a file in the context, or that are `always` on.
pub fn prompt_block(set: &SkillSet, user_text: &str, paths: &[String]) -> String {
    if set.is_empty() {
        return String::new();
    }
    let active = set.matching(user_text, paths);
    let listed: Vec<String> = set
        .iter()
        .filter(|s| !active.iter().any(|a| a.name == s.name))
        .map(|s| s.name.clone())
        .collect();
    let mut out = set.system_prompt_block(&listed);
    if !out.is_empty() {
        out.push_str("Load one with the skill_load tool before relying on it.\n");
    }
    for skill in active {
        match set.load(&skill.name) {
            Ok(body) => out.push_str(&format!(
                "\nACTIVE SKILL `{}`:\n{}\n",
                skill.name,
                crate::util::truncate_bytes(body.trim(), 16 * 1024)
            )),
            Err(e) => tracing::warn!("skill `{}` could not be loaded: {e}", skill.name),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, text: &str) {
        let d = dir.join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("SKILL.md"), text).unwrap();
    }

    #[test]
    fn agent_skills_and_legacy_front_matter_both_load() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        // An Agent Skill: name and description, loaded on demand.
        write(
            &home.path().join("skills"),
            "report",
            "---\nname: report\ndescription: How to assemble the PDF report.\n---\nUse sections.\n",
        );
        // The analysis legacy front matter: triggers, paths, always (no description).
        write(
            &repo.path().join(".analyst/skills"),
            "curve-fitting",
            "---\nname: curve-fitting\ntriggers: [\"fit\", \"least squares\"]\npaths: [\"analysis/fitting*.py\"]\nalways: false\n---\nReport parameter uncertainties from the covariance.\n",
        );
        write(
            &repo.path().join(".analyst/skills"),
            "house-style",
            "---\nname: house-style\nalways: true\n---\nSI units on every axis.\n",
        );
        // A project skill replaces a user skill of the same name.
        write(
            &home.path().join("skills"),
            "house-style",
            "---\nname: house-style\nalways: true\n---\nImperial units.\n",
        );
        let set = discover(home.path(), repo.path());
        assert_eq!(set.names(), vec!["curve-fitting", "house-style", "report"]);
        assert_eq!(set.load("report").unwrap().trim(), "Use sections.");

        // A turn that mentions nothing: the always-on skill in full, the rest by name.
        let block = prompt_block(&set, "plot the data", &[]);
        assert!(block.contains("- report: How to assemble the PDF report."));
        assert!(block.contains("- curve-fitting"));
        assert!(block.contains("ACTIVE SKILL `house-style`:\nSI units on every axis."));
        assert!(!block.contains("Imperial") && !block.contains("covariance"));
        // A trigger word, or a matching file in the context, activates the legacy skill.
        let block = prompt_block(&set, "Fit the decay curve", &[]);
        assert!(block.contains("ACTIVE SKILL `curve-fitting`:\nReport parameter uncertainties"));
        assert!(!block.contains("- curve-fitting"));
        let block = prompt_block(&set, "tidy up", &["analysis/fitting_tools.py".to_string()]);
        assert!(block.contains("ACTIVE SKILL `curve-fitting`"));
        // No skills, no block.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            prompt_block(&discover(empty.path(), empty.path()), "fit", &[]),
            ""
        );
    }
}

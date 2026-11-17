//! `SKILL.md` front matter (Present PROTOCOL 11).

use serde::Deserialize;

/// The YAML front matter of a `SKILL.md`. Unknown keys are ignored.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
pub struct FrontMatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub license: Option<String>,
    /// A list, or one string of space- or comma-separated tool names.
    #[serde(rename = "allowed-tools", alias = "allowed_tools")]
    pub allowed_tools: Option<StringOrList>,
    /// Analyst legacy.
    pub triggers: Option<Vec<String>>,
    /// Analyst legacy.
    pub paths: Option<Vec<String>>,
    /// Analyst legacy.
    pub always: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum StringOrList {
    One(String),
    Many(Vec<String>),
}

impl FrontMatter {
    /// Whether any Analyst legacy field is present.
    pub fn is_legacy(&self) -> bool {
        self.triggers.is_some() || self.paths.is_some() || self.always.is_some()
    }

    pub fn allowed_tools(&self) -> Vec<String> {
        match &self.allowed_tools {
            None => vec![],
            Some(StringOrList::Many(v)) => v.clone(),
            Some(StringOrList::One(s)) => s
                .split([' ', ','])
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect(),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrontMatterError {
    #[error("SKILL.md does not start with a `---` front matter block")]
    Missing,
    #[error("the front matter block is not closed with `---`")]
    Unterminated,
    #[error("front matter is not valid YAML: {0}")]
    Yaml(String),
}

/// Split a `SKILL.md` into its parsed front matter and the markdown body.
pub fn parse_skill_md(text: &str) -> Result<(FrontMatter, &str), FrontMatterError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split_inclusive('\n');
    let first = lines.next().ok_or(FrontMatterError::Missing)?;
    if first.trim_end() != "---" {
        return Err(FrontMatterError::Missing);
    }
    let yaml_start = first.len();
    let mut offset = yaml_start;
    for line in lines {
        if matches!(line.trim_end(), "---" | "...") {
            let yaml = &text[yaml_start..offset];
            let body = &text[offset + line.len()..];
            let fm = if yaml.trim().is_empty() {
                FrontMatter::default()
            } else {
                serde_yaml::from_str(yaml).map_err(|e| FrontMatterError::Yaml(e.to_string()))?
            };
            return Ok((fm, body.trim_start_matches(['\r', '\n'])));
        }
        offset += line.len();
    }
    Err(FrontMatterError::Unterminated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_front_matter_and_body() {
        let text = "---\nname: storyline\ndescription: >\n  Write a storyline\n  with action titles.\nlicense: MIT\nallowed-tools: Read Grep, Bash\nmetadata:\n  version: 2\n---\n\n# Storyline\n\nBody with --- inside.\n---\nmore\n";
        let (fm, body) = parse_skill_md(text).unwrap();
        assert_eq!(fm.name.as_deref(), Some("storyline"));
        assert_eq!(
            fm.description.as_deref().map(str::trim),
            Some("Write a storyline with action titles.")
        );
        assert_eq!(fm.license.as_deref(), Some("MIT"));
        assert_eq!(fm.allowed_tools(), vec!["Read", "Grep", "Bash"]);
        assert!(!fm.is_legacy());
        assert_eq!(body, "# Storyline\n\nBody with --- inside.\n---\nmore\n");
    }

    #[test]
    fn legacy_fields_crlf_and_bom() {
        let text = "\u{feff}---\r\nname: curve-fitting\r\ntriggers: [\"fit\", \"regression\", \"least squares\"]\r\npaths: [\"analysis/fitting*.py\"]\r\nalways: false\r\nallowed-tools:\r\n  - a\r\n  - b\r\n---\r\nWhen fitting models: report uncertainties.\r\n";
        let (fm, body) = parse_skill_md(text).unwrap();
        assert_eq!(fm.name.as_deref(), Some("curve-fitting"));
        assert_eq!(
            fm.triggers.clone().unwrap(),
            vec!["fit", "regression", "least squares"]
        );
        assert_eq!(fm.paths.clone().unwrap(), vec!["analysis/fitting*.py"]);
        assert_eq!(fm.always, Some(false));
        assert_eq!(fm.allowed_tools(), vec!["a", "b"]);
        assert!(fm.is_legacy());
        assert_eq!(body, "When fitting models: report uncertainties.\r\n");
    }

    #[test]
    fn errors() {
        assert_eq!(parse_skill_md(""), Err(FrontMatterError::Missing));
        assert_eq!(
            parse_skill_md("# Just markdown\n"),
            Err(FrontMatterError::Missing)
        );
        assert_eq!(
            parse_skill_md("---\nname: x\n"),
            Err(FrontMatterError::Unterminated)
        );
        assert!(matches!(
            parse_skill_md("---\nname: [unclosed\n---\nbody"),
            Err(FrontMatterError::Yaml(_))
        ));
        assert!(matches!(
            parse_skill_md("---\n- a\n- b\n---\nbody"),
            Err(FrontMatterError::Yaml(_))
        ));
        // An empty block parses to nothing; a closed block with no body has an empty body.
        let (fm, body) = parse_skill_md("---\n---").unwrap();
        assert_eq!(fm, FrontMatter::default());
        assert_eq!(body, "");
    }
}

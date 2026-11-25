//! The scene notebook format (ARCHITECTURE 8).
//!
//! A notebook is a valid Python file in the "percent" cell format:
//!
//! ```text
//! # fibration-math: scene api=1
//! # %% [c1] Define the family and its parameters
//! from fibration_math import sym, plot, param
//! x = sym.symbols("x")
//!
//! # %% [c2] Slider commit
//! param.set(a=2.4)
//! ```
//!
//! * The optional first line `# fibration-math: ...` is the header.
//! * Text between the header and the first marker is the preamble, kept verbatim and
//!   executed before the cells (ids never refer to it).
//! * Each cell starts with `# %% [<id>] <caption>`. Ids are stable across edits; new ids are
//!   `c<n>` with `n` one more than the largest existing `c<n>`.
//!
//! Printing is normalized: no blank lines at the start or end of a cell's code, exactly one
//! blank line between cells, and one trailing newline. `print(parse(s)) == s` for any
//! normalized file.

use std::sync::LazyLock;

use math_protocol::Cell;
use regex::Regex;

pub const HEADER: &str = "# fibration-math: scene api=1";
const HEADER_PREFIX: &str = "# fibration-math:";

static MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^# %% \[([A-Za-z0-9_-]{1,64})\](?: (.*))?$").expect("marker regex")
});
static LOOSE_MARKER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^#\s*%%").expect("loose marker regex"));
static GENERATED_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^c(\d{1,9})$").expect("id regex"));

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NotebookError {
    #[error("line {line}: a cell marker must look like `# %% [id] caption`")]
    BadMarker { line: usize },
    #[error("cell id {0:?} appears more than once")]
    DuplicateId(String),
    #[error("there is no cell {0:?}")]
    UnknownCell(String),
    #[error("cell code may not contain a cell marker line (`# %%`)")]
    MarkerInCode,
    #[error("a caption must be a single line")]
    MultilineCaption,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notebook {
    pub header: Option<String>,
    /// Code before the first cell, without trailing blank lines (may be empty).
    pub preamble: String,
    pub cells: Vec<Cell>,
}

fn trim_blank_lines(lines: &[&str]) -> String {
    let start = lines.iter().position(|l| !l.trim().is_empty());
    let end = lines.iter().rposition(|l| !l.trim().is_empty());
    match (start, end) {
        (Some(s), Some(e)) => lines[s..=e]
            .iter()
            .map(|l| l.trim_end_matches('\r'))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn normalize_code(code: &str) -> Result<String, NotebookError> {
    let lines: Vec<&str> = code.lines().collect();
    if lines.iter().any(|l| LOOSE_MARKER.is_match(l)) {
        return Err(NotebookError::MarkerInCode);
    }
    Ok(trim_blank_lines(&lines))
}

fn check_caption(caption: &str) -> Result<String, NotebookError> {
    if caption.contains('\n') || caption.contains('\r') {
        return Err(NotebookError::MultilineCaption);
    }
    Ok(caption.trim().to_string())
}

impl Notebook {
    pub fn new() -> Self {
        Self {
            header: Some(HEADER.to_string()),
            preamble: String::new(),
            cells: Vec::new(),
        }
    }

    pub fn parse(text: &str) -> Result<Self, NotebookError> {
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;
        let mut header = None;
        if let Some(first) = lines.first() {
            if first.starts_with(HEADER_PREFIX) {
                header = Some(first.trim_end().to_string());
                i = 1;
            }
        }
        let mut preamble_lines = Vec::new();
        let mut cells: Vec<Cell> = Vec::new();
        let mut current: Option<(String, String, Vec<&str>)> = None;
        for (n, line) in lines.iter().enumerate().skip(i) {
            if LOOSE_MARKER.is_match(line) {
                let caps = MARKER
                    .captures(line.trim_end())
                    .ok_or(NotebookError::BadMarker { line: n + 1 })?;
                if let Some((id, caption, body)) = current.take() {
                    cells.push(Cell {
                        id,
                        caption,
                        code: trim_blank_lines(&body),
                    });
                }
                let id = caps[1].to_string();
                if cells.iter().any(|c| c.id == id) {
                    return Err(NotebookError::DuplicateId(id));
                }
                let caption = caps
                    .get(2)
                    .map(|m| m.as_str().trim().to_string())
                    .unwrap_or_default();
                current = Some((id, caption, Vec::new()));
            } else if let Some((_, _, body)) = current.as_mut() {
                body.push(line);
            } else {
                preamble_lines.push(*line);
            }
        }
        if let Some((id, caption, body)) = current.take() {
            if cells.iter().any(|c| c.id == id) {
                return Err(NotebookError::DuplicateId(id));
            }
            cells.push(Cell {
                id,
                caption,
                code: trim_blank_lines(&body),
            });
        }
        Ok(Self {
            header,
            preamble: trim_blank_lines(&preamble_lines),
            cells,
        })
    }

    pub fn print(&self) -> String {
        let mut blocks: Vec<String> = Vec::new();
        if !self.preamble.is_empty() {
            blocks.push(self.preamble.clone());
        }
        for cell in &self.cells {
            let marker = if cell.caption.is_empty() {
                format!("# %% [{}]", cell.id)
            } else {
                format!("# %% [{}] {}", cell.id, cell.caption)
            };
            if cell.code.is_empty() {
                blocks.push(marker);
            } else {
                blocks.push(format!("{marker}\n{}", cell.code));
            }
        }
        let mut out = String::new();
        if let Some(h) = &self.header {
            out.push_str(h);
            out.push('\n');
        }
        out.push_str(&blocks.join("\n\n"));
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out
    }

    pub fn cell(&self, id: &str) -> Option<&Cell> {
        self.cells.iter().find(|c| c.id == id)
    }

    fn index(&self, id: &str) -> Result<usize, NotebookError> {
        self.cells
            .iter()
            .position(|c| c.id == id)
            .ok_or_else(|| NotebookError::UnknownCell(id.to_string()))
    }

    /// The next generated id: `c<max+1>` over existing `c<n>` ids.
    pub fn next_id(&self) -> String {
        let max = self
            .cells
            .iter()
            .filter_map(|c| {
                GENERATED_ID
                    .captures(&c.id)
                    .and_then(|m| m[1].parse::<u64>().ok())
            })
            .max()
            .unwrap_or(0);
        format!("c{}", max + 1)
    }

    /// Insert a cell after `after` (`None` puts it first). Returns the new id.
    pub fn insert_after(
        &mut self,
        after: Option<&str>,
        code: &str,
        caption: &str,
    ) -> Result<String, NotebookError> {
        let at = match after {
            Some(id) => self.index(id)? + 1,
            None => 0,
        };
        let cell = Cell {
            id: self.next_id(),
            caption: check_caption(caption)?,
            code: normalize_code(code)?,
        };
        let id = cell.id.clone();
        self.cells.insert(at, cell);
        Ok(id)
    }

    /// Append a cell at the end. Returns the new id.
    pub fn append(&mut self, code: &str, caption: &str) -> Result<String, NotebookError> {
        let last = self.cells.last().map(|c| c.id.clone());
        self.insert_after(last.as_deref(), code, caption)
    }

    /// Replace a cell's code, and its caption when given. The id is kept.
    pub fn replace(
        &mut self,
        id: &str,
        code: &str,
        caption: Option<&str>,
    ) -> Result<(), NotebookError> {
        let i = self.index(id)?;
        let code = normalize_code(code)?;
        let caption = caption.map(check_caption).transpose()?;
        let cell = &mut self.cells[i];
        cell.code = code;
        if let Some(c) = caption {
            cell.caption = c;
        }
        Ok(())
    }

    pub fn delete(&mut self, id: &str) -> Result<Cell, NotebookError> {
        let i = self.index(id)?;
        Ok(self.cells.remove(i))
    }

    /// Cells up to and including `until` (all cells when `None`).
    pub fn cells_until(&self, until: Option<&str>) -> Result<&[Cell], NotebookError> {
        match until {
            None => Ok(&self.cells),
            Some(id) => Ok(&self.cells[..=self.index(id)?]),
        }
    }

    /// Whether the preamble has code to run (anything besides comments and blank lines).
    pub fn preamble_has_code(&self) -> bool {
        self.preamble
            .lines()
            .any(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# fibration-math: scene api=1\n\
# %% [c1] Define the family and its parameters\n\
from fibration_math import sym, plot, param\n\
x = sym.symbols(\"x\")\n\
\n\
for k in range(3):\n\
    pass\n\
\n\
# %% [c2] Slider commit\n\
param.set(a=2.4)\n";

    #[test]
    fn round_trip_is_exact() {
        let nb = Notebook::parse(SAMPLE).unwrap();
        assert_eq!(nb.cells.len(), 2);
        assert_eq!(nb.cells[0].caption, "Define the family and its parameters");
        assert!(
            nb.cells[0].code.contains("\n\nfor k"),
            "inner blank lines are kept"
        );
        assert_eq!(nb.print(), SAMPLE);
    }

    #[test]
    fn normalizes_blank_lines() {
        let messy = "# %% [c1]\n\n\nx = 1\n\n\n\n# %% [c2] two\ny = 2\n\n\n";
        let nb = Notebook::parse(messy).unwrap();
        let printed = nb.print();
        assert_eq!(printed, "# %% [c1]\nx = 1\n\n# %% [c2] two\ny = 2\n");
        assert_eq!(Notebook::parse(&printed).unwrap().print(), printed);
    }

    #[test]
    fn preamble_is_kept_verbatim() {
        let text =
            "# fibration-math: scene api=1\nimport math\n# a comment\n\n# %% [c1] one\nx = 1\n";
        let nb = Notebook::parse(text).unwrap();
        assert_eq!(nb.preamble, "import math\n# a comment");
        assert!(nb.preamble_has_code());
        assert_eq!(nb.print(), text);
    }

    #[test]
    fn rejects_bad_markers_and_duplicates() {
        assert_eq!(
            Notebook::parse("# %% c1\nx=1\n"),
            Err(NotebookError::BadMarker { line: 1 })
        );
        assert_eq!(
            Notebook::parse("# %% [c1]\n# %% [c1]\n"),
            Err(NotebookError::DuplicateId("c1".into()))
        );
    }

    #[test]
    fn edits_keep_ids_stable() {
        let mut nb = Notebook::parse(SAMPLE).unwrap();
        let id = nb
            .append("plot.explicit(f, x=(-1, 1))\n\n", "Plot")
            .unwrap();
        assert_eq!(id, "c3");
        let first = nb.insert_after(None, "import math", "Imports").unwrap();
        assert_eq!(first, "c4");
        assert_eq!(nb.cells[0].id, "c4");
        nb.replace("c2", "param.set(a=1.0)", Some("Reset a"))
            .unwrap();
        assert_eq!(nb.cell("c2").unwrap().caption, "Reset a");
        nb.delete("c1").unwrap();
        assert_eq!(
            nb.cells.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["c4", "c2", "c3"]
        );
        assert_eq!(nb.next_id(), "c5");
        let printed = nb.print();
        assert_eq!(Notebook::parse(&printed).unwrap(), nb);
    }

    #[test]
    fn edit_errors() {
        let mut nb = Notebook::parse(SAMPLE).unwrap();
        assert_eq!(
            nb.replace("zz", "x", None),
            Err(NotebookError::UnknownCell("zz".into()))
        );
        assert_eq!(
            nb.append("# %% [c9]\nx", ""),
            Err(NotebookError::MarkerInCode)
        );
        assert_eq!(nb.append("x", "a\nb"), Err(NotebookError::MultilineCaption));
    }

    #[test]
    fn cells_until() {
        let nb = Notebook::parse(SAMPLE).unwrap();
        assert_eq!(nb.cells_until(Some("c1")).unwrap().len(), 1);
        assert_eq!(nb.cells_until(None).unwrap().len(), 2);
        assert!(nb.cells_until(Some("nope")).is_err());
    }
}

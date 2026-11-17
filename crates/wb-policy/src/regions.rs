// Ported from Personal Analyst `crates/core/src/policy.rs` (`locked_ranges`, `locked_blocks`,
// `edit_touches_locked_region`) at commit f229e4d.
// Adapted: the markers are parameters and the change is described by `TextChange` instead of
// tool arguments, so the engine has no knowledge of tool names.
//! Protected regions inside a text file, delimited by comment markers
//! (`# analyst: lock` / `# analyst: unlock`). Proposals touching them are denied.

/// Marker texts looked for inside comment lines (`#`, `//`, `--`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockMarkers<'a> {
    pub lock: &'a str,
    pub unlock: &'a str,
}

/// Analyst's markers.
pub const ANALYST_MARKERS: LockMarkers<'static> = LockMarkers {
    lock: "analyst: lock",
    unlock: "analyst: unlock",
};

/// A proposed change to an existing text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextChange<'a> {
    /// The file is deleted.
    Delete,
    /// Exact-match replacements; each entry is the `old` text being replaced.
    Replacements(&'a [&'a str]),
    /// The whole content is replaced.
    FullContent(&'a str),
}

/// Locked line ranges (1-based, inclusive, markers included). An unterminated lock runs to EOF.
pub fn locked_ranges(text: &str, markers: LockMarkers<'_>) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start: Option<usize> = None;
    let mut last = 0;
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        last = n;
        let t = line.trim();
        let is_comment = t.starts_with('#') || t.starts_with("//") || t.starts_with("--");
        if !is_comment {
            continue;
        }
        if t.contains(markers.unlock) {
            if let Some(s) = start.take() {
                ranges.push((s, n));
            }
        } else if t.contains(markers.lock) && start.is_none() {
            start = Some(n);
        }
    }
    if let Some(s) = start {
        ranges.push((s, last.max(s)));
    }
    ranges
}

/// Text of each locked block, verbatim.
pub fn locked_blocks(text: &str, markers: LockMarkers<'_>) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    locked_ranges(text, markers)
        .into_iter()
        .map(|(s, e)| lines[s - 1..e.min(lines.len())].join("\n"))
        .collect()
}

fn line_of_offset(text: &str, offset: usize) -> usize {
    text[..offset].matches('\n').count() + 1
}

/// Returns a denial reason when a change would modify a locked region of `existing`.
pub fn change_touches_locked_region(
    existing: &str,
    change: TextChange<'_>,
    markers: LockMarkers<'_>,
) -> Option<String> {
    let ranges = locked_ranges(existing, markers);
    if ranges.is_empty() {
        return None;
    }
    let lock = markers.lock;
    match change {
        TextChange::Delete => Some(format!(
            "file contains a locked region (`{lock}`) and cannot be deleted"
        )),
        TextChange::Replacements(olds) => {
            for old in olds {
                if old.is_empty() {
                    continue;
                }
                if let Some(pos) = existing.find(old) {
                    let first = line_of_offset(existing, pos);
                    let last = line_of_offset(existing, pos + old.len().saturating_sub(1));
                    for (s, en) in &ranges {
                        if first <= *en && last >= *s {
                            return Some(format!(
                                "edit touches locked region lines {s}-{en} (`{lock}`)"
                            ));
                        }
                    }
                }
            }
            None
        }
        TextChange::FullContent(content) => locked_blocks(existing, markers)
            .iter()
            .any(|block| !content.contains(block.as_str()))
            .then(|| format!("replacement content changes a locked region (`{lock}`)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: LockMarkers<'static> = ANALYST_MARKERS;

    #[test]
    fn locked_regions_deny() {
        let file = "import x\n# analyst: lock\nSECRET = 1\n# analyst: unlock\ny = 2\n";
        assert_eq!(locked_ranges(file, M), vec![(2, 4)]);
        assert_eq!(
            locked_blocks(file, M),
            vec!["# analyst: lock\nSECRET = 1\n# analyst: unlock"]
        );
        let touch =
            change_touches_locked_region(file, TextChange::Replacements(&["SECRET = 1"]), M);
        assert!(touch.unwrap().contains("lines 2-4"));
        assert_eq!(
            change_touches_locked_region(file, TextChange::Replacements(&["y = 2"]), M),
            None
        );
        assert_eq!(
            change_touches_locked_region(file, TextChange::Replacements(&["", "absent"]), M),
            None
        );
        let keep = "import z\n# analyst: lock\nSECRET = 1\n# analyst: unlock\n";
        assert_eq!(
            change_touches_locked_region(file, TextChange::FullContent(keep), M),
            None
        );
        assert!(
            change_touches_locked_region(file, TextChange::FullContent("import z\n"), M).is_some()
        );
        assert!(change_touches_locked_region(file, TextChange::Delete, M).is_some());
        assert_eq!(locked_ranges("# analyst: lock\na\nb\n", M), vec![(1, 3)]);
    }

    #[test]
    fn no_markers_no_restriction_and_custom_markers() {
        assert_eq!(
            change_touches_locked_region("a = 1\n", TextChange::Delete, M),
            None
        );
        // A marker outside a comment line is not a marker.
        assert!(locked_ranges("x = 'analyst: lock'\n", M).is_empty());
        let custom = LockMarkers {
            lock: "present: lock",
            unlock: "present: unlock",
        };
        let text = "// present: lock\nA\n// present: unlock\nB\n";
        assert_eq!(locked_ranges(text, custom), vec![(1, 3)]);
        assert!(locked_ranges(text, M).is_empty());
    }
}

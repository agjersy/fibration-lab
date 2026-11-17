//! Run journal: what a sandbox run changed on disk, so that a later run can undo it if the
//! helper was killed before it could clean up.
//!
//! A journal is a small text file, one per run, written *before* any ACL is changed and removed
//! after the ACEs are revoked and the AppContainer profile is deleted. The Windows implementation
//! keeps the file open without sharing for the life of the run; a journal that can be opened
//! exclusively therefore belongs to a dead run. The format is parsed and written here so it can
//! be tested on every platform.

/// First line of every journal file.
pub const HEADER: &str = "wb-sandbox-journal 1";

/// File extension of journal files in the state directory.
pub const EXTENSION: &str = "journal";

/// The record of one run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Journal {
    /// AppContainer profile name (for `DeleteAppContainerProfile`).
    pub profile: String,
    /// The AppContainer SID in string form (`S-1-15-2-...`), for revoking ACEs.
    pub sid: String,
    /// Paths whose DACL received an ACE for `sid`.
    pub paths: Vec<String>,
}

impl Journal {
    /// Serialize. Paths containing line breaks cannot be represented and are rejected (they are
    /// not legal Windows paths).
    pub fn to_text(&self) -> Result<String, JournalError> {
        let fields = [&self.profile, &self.sid]
            .into_iter()
            .chain(self.paths.iter());
        for f in fields {
            if f.contains('\n') || f.contains('\r') {
                return Err(JournalError::LineBreak);
            }
        }
        let mut out = format!("{HEADER}\nprofile {}\nsid {}\n", self.profile, self.sid);
        for p in &self.paths {
            out.push_str("path ");
            out.push_str(p);
            out.push('\n');
        }
        out.push_str("end\n");
        Ok(out)
    }

    /// Parse. A file without the trailing `end` line was cut off mid-write; it is still returned
    /// (the profile and SID lines come first, and revoking a path that was never granted is a
    /// no-op) as long as the profile and SID are present.
    pub fn parse(text: &str) -> Result<Journal, JournalError> {
        let mut lines = text.lines();
        if lines.next() != Some(HEADER) {
            return Err(JournalError::BadHeader);
        }
        let mut journal = Journal::default();
        for line in lines {
            if let Some(v) = line.strip_prefix("profile ") {
                journal.profile = v.to_string();
            } else if let Some(v) = line.strip_prefix("sid ") {
                journal.sid = v.to_string();
            } else if let Some(v) = line.strip_prefix("path ") {
                journal.paths.push(v.to_string());
            } else if line == "end" {
                break;
            }
            // Unknown lines are ignored so that a newer helper's journal can still be cleaned.
        }
        if journal.profile.is_empty() || !is_app_container_sid(&journal.sid) {
            return Err(JournalError::Incomplete);
        }
        Ok(journal)
    }
}

/// Only AppContainer package SIDs (`S-1-15-2-` followed by numeric sub-authorities) are ever
/// revoked from a journal, so a corrupted or planted journal cannot be used to strip a user's or
/// a group's ACEs.
pub fn is_app_container_sid(sid: &str) -> bool {
    match sid.strip_prefix("S-1-15-2-") {
        Some(rest) => {
            let parts: Vec<&str> = rest.split('-').collect();
            // ALL APPLICATION PACKAGES is S-1-15-2-1; package SIDs have seven sub-authorities
            // after the "2".
            parts.len() >= 2
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        }
        None => false,
    }
}

/// Why a journal could not be read or written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalError {
    BadHeader,
    Incomplete,
    LineBreak,
}

impl std::fmt::Display for JournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            JournalError::BadHeader => "not a sandbox journal",
            JournalError::Incomplete => "the journal has no profile name or AppContainer SID",
            JournalError::LineBreak => "a journal field contains a line break",
        })
    }
}

impl std::error::Error for JournalError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Journal {
        Journal {
            profile: "present.sandbox.1234.0.1a2b".into(),
            sid: "S-1-15-2-1-2-3-4-5-6-7".into(),
            paths: vec![r"C:\Users\a b\runtime".into(), r"C:\scratch".into()],
        }
    }

    #[test]
    fn round_trip() {
        let j = sample();
        assert_eq!(Journal::parse(&j.to_text().unwrap()).unwrap(), j);
    }

    #[test]
    fn truncated_journal_keeps_what_was_written() {
        let text = sample().to_text().unwrap();
        let cut = text.rfind("path ").unwrap();
        let j = Journal::parse(&text[..cut]).unwrap();
        assert_eq!(j.paths, vec![r"C:\Users\a b\runtime".to_string()]);
    }

    #[test]
    fn rejects_garbage_and_foreign_sids() {
        assert_eq!(Journal::parse(""), Err(JournalError::BadHeader));
        assert_eq!(
            Journal::parse("something else\n"),
            Err(JournalError::BadHeader)
        );
        assert_eq!(
            Journal::parse(&format!("{HEADER}\nprofile x\n")),
            Err(JournalError::Incomplete)
        );
        let mut j = sample();
        j.sid = "S-1-5-32-544".into(); // Administrators
        assert_eq!(
            Journal::parse(&j.to_text().unwrap()),
            Err(JournalError::Incomplete)
        );
        j.sid = "S-1-1-0".into(); // Everyone
        assert_eq!(
            Journal::parse(&j.to_text().unwrap()),
            Err(JournalError::Incomplete)
        );
    }

    #[test]
    fn sid_shape() {
        assert!(is_app_container_sid("S-1-15-2-1-2-3-4-5-6-7"));
        assert!(!is_app_container_sid("S-1-15-2-1")); // ALL APPLICATION PACKAGES
        assert!(!is_app_container_sid("S-1-15-2-"));
        assert!(!is_app_container_sid("S-1-15-2-1--3"));
        assert!(!is_app_container_sid("S-1-15-3-1-2"));
        assert!(!is_app_container_sid("S-1-15-2-1-x"));
    }

    #[test]
    fn line_breaks_are_rejected() {
        let mut j = sample();
        j.paths.push("a\nsid S-1-1-0".into());
        assert_eq!(j.to_text(), Err(JournalError::LineBreak));
    }
}

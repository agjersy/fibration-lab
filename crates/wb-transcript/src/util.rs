// Ported from Personal Analyst `crates/core/src/util.rs` at commit f229e4d.
// Adapted: only the helpers the transcript code needs.
//! Small helpers: timestamps, ids, token estimate, truncation.

/// Current time as RFC 3339 UTC with second precision (`2026-10-16T18:15:02Z`).
pub fn now_rfc3339() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// A fresh id of the form `<UTC stamp>-<6 hex>` (`20261016-181502-a1b2c3`); sorts by time.
pub fn stamped_id() -> String {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let rand = uuid::Uuid::new_v4().simple().to_string();
    format!("{stamp}-{}", &rand[..6])
}

/// Rough token estimate (4 characters per token) used for context budgeting.
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Truncate a string at a byte limit on a char boundary.
pub fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_respects_char_boundaries() {
        let s = "héllo";
        assert_eq!(truncate_bytes(s, 2), "h");
        assert_eq!(truncate_bytes(s, 3), "hé");
        assert_eq!(truncate_bytes(s, 100), s);
    }

    #[test]
    fn stamps_and_estimates() {
        let ts = now_rfc3339();
        assert!(chrono::DateTime::parse_from_rfc3339(&ts).is_ok());
        assert!(ts.ends_with('Z'));
        let id = stamped_id();
        assert_eq!(id.len(), "20261016-181502-a1b2c3".len());
        assert_ne!(id, stamped_id());
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcde"), 2);
    }
}

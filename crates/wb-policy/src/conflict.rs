// Ported from Fibration Lab `crates/core/src/tools/proposals.rs` (the `base_hash`
// conflict check) and `crates/core/src/util.rs` (`sha256_hex`) at commit f229e4d.
// Adapted: works on bytes the app supplies instead of reading files.
//! Conflict detection by content hash: a proposal carries the hash of the content it was
//! based on; if the content changed meanwhile the proposal is rejected and the agent must
//! re-read.

use sha2::{Digest, Sha256};

/// Lowercase hex SHA-256 of the content.
pub fn content_hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Why a proposal can no longer be applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Conflict {
    #[error(
        "conflict: `{target}` changed since the proposal was made; re-read it and propose again"
    )]
    Changed {
        target: String,
        expected: String,
        actual: String,
    },
    #[error("conflict: `{target}` no longer exists; re-read and propose again")]
    Missing { target: String },
    #[error("conflict: `{target}` already exists; re-read it and propose again")]
    AlreadyExists { target: String },
}

/// Check a proposal's base against the current content.
///
/// - `base_hash = Some(h)`: the proposal changes an existing item whose content hashed to
///   `h`; `current` must exist and hash to `h`.
/// - `base_hash = None`: the proposal creates the item; `current` must be `None`.
pub fn check_conflict(
    target: &str,
    base_hash: Option<&str>,
    current: Option<&[u8]>,
) -> Result<(), Conflict> {
    match (base_hash, current) {
        (Some(h), Some(bytes)) => {
            let actual = content_hash(bytes);
            if actual.eq_ignore_ascii_case(h) {
                Ok(())
            } else {
                Err(Conflict::Changed {
                    target: target.to_string(),
                    expected: h.to_string(),
                    actual,
                })
            }
        }
        (Some(_), None) => Err(Conflict::Missing {
            target: target.to_string(),
        }),
        (None, Some(_)) => Err(Conflict::AlreadyExists {
            target: target.to_string(),
        }),
        (None, None) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_sha256_hex() {
        assert_eq!(
            content_hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn conflict_cases() {
        let h = content_hash(b"v1");
        assert_eq!(check_conflict("s1", Some(&h), Some(b"v1")), Ok(()));
        assert_eq!(
            check_conflict("s1", Some(&h.to_uppercase()), Some(b"v1")),
            Ok(())
        );
        let err = check_conflict("s1", Some(&h), Some(b"v2")).unwrap_err();
        assert!(matches!(err, Conflict::Changed { .. }));
        assert!(err.to_string().contains("conflict"));
        assert_eq!(
            check_conflict("s1", Some(&h), None),
            Err(Conflict::Missing {
                target: "s1".into()
            })
        );
        assert_eq!(
            check_conflict("s1", None, Some(b"x")),
            Err(Conflict::AlreadyExists {
                target: "s1".into()
            })
        );
        assert_eq!(check_conflict("s1", None, None), Ok(()));
    }
}

//! Validated string identifiers (PROTOCOL 2.1, 2.3, 4.4).
//!
//! Each id is a newtype over `String` that can only be constructed from text matching its
//! pattern; serde reads and writes it as a plain JSON string.

use thiserror::Error;

/// An identifier did not match its pattern.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid {kind} `{value}` (expected {pattern})")]
pub struct IdError {
    /// Type name of the id, e.g. `SlideId`.
    pub kind: &'static str,
    /// The rejected text.
    pub value: String,
    /// The regular expression the id must match.
    pub pattern: &'static str,
}

/// `<prefix>`, one alphanumeric character, then up to `max_rest` characters that are
/// alphanumeric or `-`. `upper` says whether uppercase letters count as alphanumeric.
fn matches_id(s: &str, prefix: &str, max_rest: usize, upper: bool) -> bool {
    let alnum =
        |b: u8| b.is_ascii_digit() || b.is_ascii_lowercase() || (upper && b.is_ascii_uppercase());
    match s.strip_prefix(prefix).map(str::as_bytes) {
        Some([first, rest @ ..]) => {
            alnum(*first) && rest.len() <= max_rest && rest.iter().all(|&b| alnum(b) || b == b'-')
        }
        _ => false,
    }
}

/// `h-` followed by 1 to 16 characters of `[0-9a-z]`.
fn matches_snapshot_id(s: &str) -> bool {
    match s.strip_prefix("h-") {
        Some(rest) => {
            (1..=16).contains(&rest.len())
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
        }
        None => false,
    }
}

macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident, $check:expr, $pattern:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, ts_rs::TS)]
        pub struct $name(String);

        impl $name {
            /// The regular expression every value matches.
            pub const PATTERN: &'static str = $pattern;

            /// Validates `value` against [`Self::PATTERN`].
            pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
                let value = value.into();
                let check: fn(&str) -> bool = $check;
                if check(&value) {
                    Ok($name(value))
                } else {
                    Err(IdError {
                        kind: stringify!($name),
                        value,
                        pattern: $pattern,
                    })
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl std::str::FromStr for $name {
            type Err = IdError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                $name::new(s)
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;
            fn try_from(s: String) -> Result<Self, Self::Error> {
                $name::new(s)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> String {
                id.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.0)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                $name::new(s).map_err(serde::de::Error::custom)
            }
        }

        impl schemars::JsonSchema for $name {
            fn schema_name() -> String {
                stringify!($name).to_string()
            }

            fn json_schema(_: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
                crate::schema::string_with_pattern($pattern)
            }
        }
    };
}

string_id!(
    /// Slide id, `^s-[0-9a-z][0-9a-z-]{0,23}$`, stable for the life of the slide
    /// (PROTOCOL 2.1).
    SlideId, |s| matches_id(s, "s-", 23, false), "^s-[0-9a-z][0-9a-z-]{0,23}$"
);
string_id!(
    /// Element id, `^e-[0-9A-Za-z][0-9A-Za-z-]{0,31}$` (PROTOCOL 2.1). The SDK emits ids
    /// such as `e-raw-1`, `e-ftr`, and `e-sldNum`.
    ElementId, |s| matches_id(s, "e-", 31, true), "^e-[0-9A-Za-z][0-9A-Za-z-]{0,31}$"
);
string_id!(
    /// Reference id, `^r-[0-9a-z][0-9a-z-]{0,23}$` (PROTOCOL 2.3).
    RefId, |s| matches_id(s, "r-", 23, false), "^r-[0-9a-z][0-9a-z-]{0,23}$"
);
string_id!(
    /// Snapshot id, `^h-[0-9a-z]{1,16}$` (PROTOCOL 4.4).
    SnapshotId, matches_snapshot_id, "^h-[0-9a-z]{1,16}$"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_accept_documented_examples_and_reject_others() {
        for ok in [
            "s-7f3a",
            "s-c001",
            "s-a",
            "s-0",
            "s-auto-0001",
            "s-0123456789abcdefghijklmn",
        ] {
            assert!(SlideId::new(ok).is_ok(), "{ok:?}");
        }
        for bad in [
            "",
            "s-",
            "s",
            "s--a",
            "s-0123456789abcdefghijklmno",
            "S-7f3a",
            "s-7F3a",
            "e-7f3a",
            "s_7f3a",
            " s-7f3a",
            "s-7f3a\n",
            "s-a b",
            "s-é",
        ] {
            assert!(SlideId::new(bad).is_err(), "{bad:?}");
        }
        for ok in [
            "e-a1b2",
            "e-raw-1",
            "e-ftr",
            "e-sldNum",
            "e-b2",
            "e-A",
            "e-0123456789abcdefghijklmnopqrstuv",
        ] {
            assert!(ElementId::new(ok).is_ok(), "{ok:?}");
        }
        for bad in [
            "e-",
            "e--raw",
            "e-a_b",
            "e-a.b",
            "s-a1b2",
            "e-0123456789abcdefghijklmnopqrstuvw",
        ] {
            assert!(ElementId::new(bad).is_err(), "{bad:?}");
        }
        for ok in ["r-q3rep", "r-q3-report", "r-1"] {
            assert!(RefId::new(ok).is_ok(), "{ok:?}");
        }
        for bad in ["r-", "r-Q3", "r--x", "q3rep", "r-a@b", "r-a#b"] {
            assert!(RefId::new(bad).is_err(), "{bad:?}");
        }
        for ok in ["h-0a1b2c3d4e", "h-0000000001", "h-1", "h-0123456789abcdef"] {
            assert!(SnapshotId::new(ok).is_ok(), "{ok:?}");
        }
        for bad in ["h-", "h-0123456789abcdefg", "h-a-b", "h-A", "HEAD"] {
            assert!(SnapshotId::new(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn ids_serialize_as_strings_and_validate_on_parse() {
        let id: SlideId = serde_json::from_str("\"s-7f3a\"").unwrap();
        assert_eq!(id.as_str(), "s-7f3a");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"s-7f3a\"");
        let err = serde_json::from_str::<SlideId>("\"slide-1\"").unwrap_err();
        assert!(err.to_string().contains("invalid SlideId"), "{err}");
    }
}

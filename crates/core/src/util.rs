//! Small shared helpers: hashing, ids, time, atomic writes.

use rand::Rng;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    Ok(sha256_hex(&std::fs::read(path)?))
}

/// RFC 3339 UTC with millisecond precision.
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// `<prefix>-<n lowercase base36 characters>`.
pub fn new_id(prefix: &str, len: usize) -> String {
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut rng = rand::thread_rng();
    let body: String = (0..len)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect();
    format!("{prefix}-{body}")
}

/// Write through a temporary file in the same directory, then rename over the target.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.flush()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Pretty JSON with a trailing newline, the on-disk form of deck documents.
pub fn to_json_pretty<T: serde::Serialize>(value: &T) -> serde_json::Result<String> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    Ok(text)
}

/// A project-relative path with forward slashes.
pub fn rel_slash(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

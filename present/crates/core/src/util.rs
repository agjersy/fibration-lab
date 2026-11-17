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
    // On Windows a file another process has open for reading cannot be replaced; readers
    // (the kernel, a virus scanner) hold files only briefly, so wait for them.
    let mut attempt = 0;
    loop {
        match tmp.persist(path) {
            Ok(_) => return Ok(()),
            Err(e) if attempt < BUSY_RETRIES && is_busy(&e.error) => {
                attempt += 1;
                std::thread::sleep(BUSY_WAIT);
                tmp = e.file;
            }
            Err(e) => return Err(e.error),
        }
    }
}

const BUSY_RETRIES: u32 = 20;
const BUSY_WAIT: std::time::Duration = std::time::Duration::from_millis(50);

/// A sharing violation or a transient denial while another process has the file open.
fn is_busy(e: &std::io::Error) -> bool {
    cfg!(windows)
        && (e.kind() == std::io::ErrorKind::PermissionDenied
            || matches!(e.raw_os_error(), Some(32 | 33)))
}

/// Remove a file, waiting briefly when another process has it open (Windows).
pub fn remove_file_patiently(path: &Path) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        match std::fs::remove_file(path) {
            Err(e) if attempt < BUSY_RETRIES && is_busy(&e) => {
                attempt += 1;
                std::thread::sleep(BUSY_WAIT);
            }
            other => return other,
        }
    }
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

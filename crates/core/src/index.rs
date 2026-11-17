//! The reference index: SQLite FTS5 plus embedding vectors, and hybrid search (PROTOCOL 8.1, 8.2).
//!
//! Vectors are stored as little-endian `f32` blobs and searched by exact cosine similarity.
//! That replaces the `sqlite-vec` virtual table named in the contract: exact search over the
//! chunk counts a deck project has (tens of thousands) takes milliseconds, needs no native
//! extension, and gives deterministic results on every platform.

use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct ChunkRow {
    pub loc: String,
    pub heading_path: Vec<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub ref_id: String,
    pub version: String,
    pub loc: String,
    pub title: String,
    pub heading_path: Vec<String>,
    pub snippet: String,
    pub score: f64,
}

impl Hit {
    pub fn cite(&self) -> String {
        format!("ref:{}@{}#{}", self.ref_id, self.version, self.loc)
    }
}

#[derive(Debug, Clone)]
pub struct RefRow {
    pub id: String,
    pub version: String,
    pub kind: String,
    pub title: String,
    pub file: String,
    pub outline_json: String,
    pub status: String,
}

pub struct Index {
    conn: Connection,
}

const RRF_K: f64 = 60.0;
const PER_LIST: usize = 50;
const PER_REF_CAP: usize = 3;

impl Index {
    pub fn open(path: &Path) -> rusqlite::Result<Index> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn in_memory() -> rusqlite::Result<Index> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Index> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS refs (id TEXT PRIMARY KEY, version TEXT NOT NULL, kind TEXT NOT NULL,
                 title TEXT NOT NULL, file TEXT NOT NULL, outline_json TEXT NOT NULL DEFAULT '[]', status TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS chunks (rowid INTEGER PRIMARY KEY, ref_id TEXT NOT NULL, version TEXT NOT NULL,
                 loc TEXT NOT NULL, heading_path TEXT NOT NULL, text TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS chunks_ref ON chunks(ref_id);
             CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(text, heading_path, content='chunks',
                 content_rowid='rowid', tokenize='unicode61 remove_diacritics 2');
             CREATE TABLE IF NOT EXISTS chunks_vec (rowid INTEGER PRIMARY KEY, dims INTEGER NOT NULL, embedding BLOB NOT NULL);
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        Ok(Index { conn })
    }

    pub fn set_meta(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.conn.execute("INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2", params![key, value])?;
        Ok(())
    }

    pub fn meta(&self, key: &str) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()
    }

    pub fn upsert_ref(&self, r: &RefRow) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO refs(id, version, kind, title, file, outline_json, status) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET version = ?2, kind = ?3, title = ?4, file = ?5, outline_json = ?6, status = ?7",
            params![r.id, r.version, r.kind, r.title, r.file, r.outline_json, r.status],
        )?;
        Ok(())
    }

    pub fn set_status(&self, ref_id: &str, status: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE refs SET status = ?2 WHERE id = ?1",
            params![ref_id, status],
        )?;
        Ok(())
    }

    pub fn refs(&self) -> rusqlite::Result<Vec<RefRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, version, kind, title, file, outline_json, status FROM refs ORDER BY rowid",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(RefRow {
                id: r.get(0)?,
                version: r.get(1)?,
                kind: r.get(2)?,
                title: r.get(3)?,
                file: r.get(4)?,
                outline_json: r.get(5)?,
                status: r.get(6)?,
            })
        })?;
        rows.collect()
    }

    fn delete_chunks(&self, ref_id: &str) -> rusqlite::Result<()> {
        let mut stmt = self
            .conn
            .prepare("SELECT rowid, text, heading_path FROM chunks WHERE ref_id = ?1")?;
        let rows: Vec<(i64, String, String)> = stmt
            .query_map([ref_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        for (rowid, text, heading) in rows {
            self.conn.execute("INSERT INTO chunks_fts(chunks_fts, rowid, text, heading_path) VALUES ('delete', ?1, ?2, ?3)", params![rowid, text, heading])?;
            self.conn
                .execute("DELETE FROM chunks_vec WHERE rowid = ?1", [rowid])?;
        }
        self.conn
            .execute("DELETE FROM chunks WHERE ref_id = ?1", [ref_id])?;
        Ok(())
    }

    /// Replace the searchable chunks of a reference with those of its current version.
    /// Returns the row ids, in order, for attaching embeddings.
    pub fn replace_chunks(
        &mut self,
        ref_id: &str,
        version: &str,
        chunks: &[ChunkRow],
    ) -> rusqlite::Result<Vec<i64>> {
        self.delete_chunks(ref_id)?;
        let tx = self.conn.transaction()?;
        let mut ids = Vec::with_capacity(chunks.len());
        for c in chunks {
            let heading = c.heading_path.join(" > ");
            tx.execute("INSERT INTO chunks(ref_id, version, loc, heading_path, text) VALUES (?1, ?2, ?3, ?4, ?5)", params![ref_id, version, c.loc, heading, c.text])?;
            let rowid = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO chunks_fts(rowid, text, heading_path) VALUES (?1, ?2, ?3)",
                params![rowid, c.text, heading],
            )?;
            ids.push(rowid);
        }
        tx.commit()?;
        Ok(ids)
    }

    pub fn remove_ref(&mut self, ref_id: &str) -> rusqlite::Result<()> {
        self.delete_chunks(ref_id)?;
        self.conn
            .execute("DELETE FROM refs WHERE id = ?1", [ref_id])?;
        Ok(())
    }

    pub fn set_embeddings(&mut self, rowids: &[i64], vectors: &[Vec<f32>]) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        for (rowid, v) in rowids.iter().zip(vectors) {
            let norm = v
                .iter()
                .map(|x| x * x)
                .sum::<f32>()
                .sqrt()
                .max(f32::MIN_POSITIVE);
            let blob: Vec<u8> = v.iter().flat_map(|x| (x / norm).to_le_bytes()).collect();
            tx.execute(
                "INSERT OR REPLACE INTO chunks_vec(rowid, dims, embedding) VALUES (?1, ?2, ?3)",
                params![rowid, v.len() as i64, blob],
            )?;
        }
        tx.commit()
    }

    /// Row ids and texts of chunks that have no embedding yet.
    pub fn unembedded(&self, limit: usize) -> rusqlite::Result<Vec<(i64, String)>> {
        let mut stmt = self.conn.prepare("SELECT c.rowid, c.text FROM chunks c LEFT JOIN chunks_vec v ON v.rowid = c.rowid WHERE v.rowid IS NULL ORDER BY c.rowid LIMIT ?1")?;
        let rows = stmt.query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }

    pub fn clear_embeddings(&self) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM chunks_vec", [])?;
        Ok(())
    }

    pub fn chunk_count(&self) -> rusqlite::Result<usize> {
        self.conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get::<_, i64>(0))
            .map(|n| n as usize)
    }

    fn fts_query(query: &str) -> String {
        // Quote every term so punctuation in the user's words cannot be read as FTS syntax.
        let terms: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.chars().count() > 1)
            .map(|t| format!("\"{}\"", t.replace('"', "")))
            .collect();
        terms.join(" OR ")
    }

    fn lexical(&self, query: &str, filter: Option<&[String]>) -> rusqlite::Result<Vec<i64>> {
        let fts = Self::fts_query(query);
        if fts.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare("SELECT c.rowid, c.ref_id FROM chunks_fts f JOIN chunks c ON c.rowid = f.rowid WHERE chunks_fts MATCH ?1 ORDER BY bm25(chunks_fts, 1.0, 0.5) LIMIT 400")?;
        let rows: Vec<(i64, String)> = stmt
            .query_map([fts], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(rows
            .into_iter()
            .filter(|(_, r)| filter.is_none_or(|f| f.contains(r)))
            .map(|(id, _)| id)
            .take(PER_LIST)
            .collect())
    }

    fn semantic(&self, query_vec: &[f32], filter: Option<&[String]>) -> rusqlite::Result<Vec<i64>> {
        let norm = query_vec
            .iter()
            .map(|x| x * x)
            .sum::<f32>()
            .sqrt()
            .max(f32::MIN_POSITIVE);
        let q: Vec<f32> = query_vec.iter().map(|x| x / norm).collect();
        let mut stmt = self.conn.prepare("SELECT v.rowid, v.embedding, c.ref_id FROM chunks_vec v JOIN chunks c ON c.rowid = v.rowid WHERE v.dims = ?1")?;
        let mut scored: Vec<(f32, i64)> = Vec::new();
        let rows = stmt.query_map([q.len() as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (rowid, blob, ref_id) = row?;
            if filter.is_some_and(|f| !f.contains(&ref_id)) {
                continue;
            }
            let dot: f32 = blob
                .chunks_exact(4)
                .zip(&q)
                .map(|(b, x)| f32::from_le_bytes([b[0], b[1], b[2], b[3]]) * x)
                .sum();
            scored.push((dot, rowid));
        }
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.1.cmp(&b.1))
        });
        Ok(scored
            .into_iter()
            .take(PER_LIST)
            .map(|(_, id)| id)
            .collect())
    }

    /// Hybrid search: BM25 and vector lists fused by reciprocal rank, capped per reference.
    pub fn search(
        &self,
        query: &str,
        query_vec: Option<&[f32]>,
        ref_ids: Option<&[String]>,
        k: usize,
    ) -> rusqlite::Result<Vec<Hit>> {
        let lexical = self.lexical(query, ref_ids)?;
        let semantic = match query_vec {
            Some(v) => self.semantic(v, ref_ids)?,
            None => Vec::new(),
        };
        let mut scores: HashMap<i64, f64> = HashMap::new();
        for list in [&lexical, &semantic] {
            for (rank, rowid) in list.iter().enumerate() {
                *scores.entry(*rowid).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
            }
        }
        let mut ranked: Vec<(i64, f64)> = scores.into_iter().collect();
        ranked.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        let cap = if ref_ids.is_some() {
            usize::MAX
        } else {
            PER_REF_CAP
        };
        let mut per_ref: HashMap<String, usize> = HashMap::new();
        let mut out = Vec::new();
        let terms: Vec<String> = query.split_whitespace().map(|t| t.to_lowercase()).collect();
        for (rowid, score) in ranked {
            if out.len() >= k {
                break;
            }
            let row = self.conn.query_row(
                "SELECT c.ref_id, c.version, c.loc, c.heading_path, c.text, r.title FROM chunks c JOIN refs r ON r.id = c.ref_id WHERE c.rowid = ?1",
                [rowid],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?, r.get::<_, String>(5)?)),
            ).optional()?;
            let Some((ref_id, version, loc, heading, text, title)) = row else {
                continue;
            };
            let n = per_ref.entry(ref_id.clone()).or_default();
            if *n >= cap {
                continue;
            }
            *n += 1;
            out.push(Hit {
                ref_id,
                version,
                loc,
                title,
                heading_path: if heading.is_empty() {
                    Vec::new()
                } else {
                    heading.split(" > ").map(str::to_string).collect()
                },
                snippet: snippet(&text, &terms, 240),
                score,
            });
        }
        Ok(out)
    }
}

/// A window of `text` around the first query term found, on character boundaries.
fn snippet(text: &str, terms: &[String], width: usize) -> String {
    let lower = text.to_lowercase();
    let at = terms
        .iter()
        .filter_map(|t| lower.find(t.as_str()))
        .min()
        .unwrap_or(0);
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let center = chars.iter().position(|(i, _)| *i >= at).unwrap_or(0);
    let start = center.saturating_sub(width / 4);
    let end = (start + width).min(chars.len());
    let mut out: String = chars[start..end].iter().map(|(_, c)| *c).collect();
    out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if start > 0 {
        out.insert(0, '…');
    }
    if end < chars.len() {
        out.push('…');
    }
    out
}

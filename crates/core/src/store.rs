//! SQLite data layer: files, chunks, wikilinks, FTS5 index and sqlite-vec vectors in one file.
//! See "Anamnesis Rust - Data Layer and ERD" in the vault for the schema diagram.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

pub const SCHEMA_VERSION: &str = "1";
/// Backlinks counted towards importance and listed in the first chunk's embed text (TS parity).
pub const MAX_BACKLINKS: usize = 5;

#[derive(Debug, Clone, PartialEq)]
pub struct FileState {
    pub mtime_ms: i64,
    pub content_hash: String,
}

#[derive(Debug, Clone)]
pub struct ChunkRow {
    pub chunk_index: usize,
    pub heading: String,
    pub context_path: String,
    pub text: String,
    pub embed_hash: String,
    pub vector: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct FileRecord {
    pub path: String,
    pub mtime_ms: i64,
    pub content_hash: String,
    pub tags: String,
    pub chunks: Vec<ChunkRow>,
}

/// A chunk as returned to search callers (MCP, UI).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Hit {
    pub id: i64,
    pub file_path: String,
    pub heading: String,
    pub context_path: String,
    pub chunk_index: i64,
    pub text: String,
    pub tags: String,
    pub importance_score: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct VectorNode {
    pub id: String,
    pub vector: Vec<f32>,
    pub text: String,
    pub tags: String,
    pub last_modified: i64,
}

// ponytail: one connection behind a mutex; embedding runs outside the lock so writes are short.
// Add a second read-only WAL connection if MCP searches ever queue behind big write batches.
pub struct Store {
    conn: Mutex<Connection>,
    dim: usize,
}

impl Store {
    /// Opens (or creates) the store. Returns `true` as the second value when the stored
    /// schema, model or dimension differ from the requested ones and the data was wiped,
    /// meaning the caller must run a full index.
    pub fn open(path: &Path, model: &str, dim: usize) -> Result<(Store, bool)> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        register_sqlite_vec();
        Self::init(Connection::open(path)?, model, dim)
    }

    pub fn open_in_memory(model: &str, dim: usize) -> Result<Store> {
        register_sqlite_vec();
        Ok(Self::init(Connection::open_in_memory()?, model, dim)?.0)
    }

    /// sqlite-vec is registered as an auto extension, so it must happen before the connection opens.
    fn init(conn: Connection, model: &str, dim: usize) -> Result<(Store, bool)> {
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL;")?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);")?;
        let want = [("schema_version", SCHEMA_VERSION.to_string()), ("model", model.to_string()), ("dim", dim.to_string())];
        let matches = want.iter().all(|(k, v)| {
            conn.query_row("SELECT value FROM meta WHERE key = ?1", [k], |r| r.get::<_, String>(0)).ok().as_deref() == Some(v.as_str())
        });
        if !matches {
            conn.execute_batch(
                "DROP TABLE IF EXISTS chunks_vec; DROP TABLE IF EXISTS chunks_fts; DROP TABLE IF EXISTS links;
                 DROP TABLE IF EXISTS chunks; DROP TABLE IF EXISTS files; DELETE FROM meta;",
            )?;
        }
        conn.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS files(
                id INTEGER PRIMARY KEY,
                path TEXT NOT NULL UNIQUE,
                stem TEXT NOT NULL COLLATE NOCASE,
                mtime_ms INTEGER NOT NULL DEFAULT 0,
                content_hash TEXT,
                tags TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS files_stem ON files(stem);
            CREATE TABLE IF NOT EXISTS chunks(
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                chunk_index INTEGER NOT NULL,
                heading TEXT NOT NULL,
                context_path TEXT NOT NULL,
                text TEXT NOT NULL,
                embed_hash TEXT NOT NULL,
                UNIQUE(file_id, chunk_index)
            );
            CREATE INDEX IF NOT EXISTS chunks_embed_hash ON chunks(embed_hash);
            CREATE TABLE IF NOT EXISTS links(
                source_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                target TEXT NOT NULL COLLATE NOCASE,
                PRIMARY KEY(source_id, target)
            );
            CREATE INDEX IF NOT EXISTS links_target ON links(target);
            CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
                text, content='chunks', content_rowid='id', tokenize='unicode61 remove_diacritics 2'
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS chunks_vec USING vec0(embedding float[{dim}] distance_metric=cosine);
            CREATE TRIGGER IF NOT EXISTS chunks_ai AFTER INSERT ON chunks BEGIN
                INSERT INTO chunks_fts(rowid, text) VALUES (new.id, new.text);
            END;
            CREATE TRIGGER IF NOT EXISTS chunks_ad AFTER DELETE ON chunks BEGIN
                INSERT INTO chunks_fts(chunks_fts, rowid, text) VALUES ('delete', old.id, old.text);
                DELETE FROM chunks_vec WHERE rowid = old.id;
            END;"
        ))?;
        for (k, v) in &want {
            conn.execute("INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)", [k, v.as_str()])?;
        }
        Ok((Store { conn: Mutex::new(conn), dim }, !matches))
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn file_state(&self, path: &str) -> Result<Option<FileState>> {
        Ok(self
            .conn()
            .query_row("SELECT mtime_ms, content_hash FROM files WHERE path = ?1 AND content_hash IS NOT NULL", [path], |r| {
                Ok(FileState { mtime_ms: r.get(0)?, content_hash: r.get(1)? })
            })
            .optional()?)
    }

    /// Records a new mtime for a file whose content hash did not change.
    pub fn touch_file(&self, path: &str, mtime_ms: i64) -> Result<()> {
        self.conn().execute("UPDATE files SET mtime_ms = ?2 WHERE path = ?1", params![path, mtime_ms])?;
        Ok(())
    }

    /// Replaces the outgoing wikilinks of `path` (creating a placeholder file row if needed).
    pub fn set_links(&self, path: &str, targets: &[String]) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let id = ensure_file(&tx, path)?;
        tx.execute("DELETE FROM links WHERE source_id = ?1", [id])?;
        for t in targets {
            tx.execute("INSERT OR IGNORE INTO links(source_id, target) VALUES (?1, ?2)", params![id, t])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Stems of the files that link to `stem`, excluding self links, at most `limit`.
    pub fn backlink_titles(&self, stem: &str, limit: usize) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT DISTINCT f.stem FROM links l JOIN files f ON f.id = l.source_id
             WHERE l.target = ?1 AND f.stem <> ?1 ORDER BY f.stem LIMIT ?2",
        )?;
        let rows = st.query_map(params![stem, limit as i64], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Existing vectors keyed by embed hash, so unchanged chunk text is never re-embedded.
    pub fn vectors_by_hash(&self, hashes: &[String]) -> Result<HashMap<String, Vec<f32>>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT v.embedding FROM chunks c JOIN chunks_vec v ON v.rowid = c.id WHERE c.embed_hash = ?1 LIMIT 1",
        )?;
        let mut out = HashMap::new();
        for h in hashes {
            if let Some(blob) = st.query_row([h], |r| r.get::<_, Vec<u8>>(0)).optional()? {
                out.insert(h.clone(), from_blob(&blob));
            }
        }
        Ok(out)
    }

    /// Atomically replaces every chunk of a file (FTS and vectors follow via triggers).
    pub fn replace_file(&self, rec: &FileRecord) -> Result<()> {
        if let Some(c) = rec.chunks.iter().find(|c| c.vector.len() != self.dim) {
            anyhow::bail!("chunk {} of {} has dim {}, store expects {}", c.chunk_index, rec.path, c.vector.len(), self.dim);
        }
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let id = ensure_file(&tx, &rec.path)?;
        tx.execute(
            "UPDATE files SET mtime_ms = ?2, content_hash = ?3, tags = ?4 WHERE id = ?1",
            params![id, rec.mtime_ms, rec.content_hash, rec.tags],
        )?;
        tx.execute("DELETE FROM chunks WHERE file_id = ?1", [id])?;
        {
            let mut ins = tx.prepare(
                "INSERT INTO chunks(file_id, chunk_index, heading, context_path, text, embed_hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            let mut vec = tx.prepare("INSERT INTO chunks_vec(rowid, embedding) VALUES (?1, ?2)")?;
            for c in &rec.chunks {
                ins.execute(params![id, c.chunk_index as i64, c.heading, c.context_path, c.text, c.embed_hash])?;
                vec.execute(params![tx.last_insert_rowid(), to_blob(&c.vector)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Deletes a file, or every file under a directory path. Returns files removed.
    pub fn delete_path(&self, path: &str) -> Result<usize> {
        let dir = path.trim_end_matches(['/', '\\']);
        let n = self.conn().execute(
            "DELETE FROM files WHERE path = ?1
               OR (substr(path, 1, length(?2)) = ?2 AND substr(path, length(?2) + 1, 1) IN ('/', '\\'))",
            params![path, dir],
        )?;
        Ok(n)
    }

    pub fn clear(&self) -> Result<()> {
        self.conn().execute_batch("DELETE FROM files;")?;
        Ok(())
    }

    pub fn indexed_paths(&self) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT path FROM files WHERE content_hash IS NOT NULL ORDER BY path")?;
        let rows = st.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn chunk_count(&self) -> Result<usize> {
        Ok(self.conn().query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get::<_, i64>(0))? as usize)
    }

    /// (path, chunk count) sorted by count descending, then path.
    pub fn file_chunk_counts(&self) -> Result<Vec<(String, usize)>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT f.path, COUNT(*) AS n FROM chunks c JOIN files f ON f.id = c.file_id GROUP BY f.id ORDER BY n DESC, f.path",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as usize)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Nearest chunks by cosine distance: (chunk id, distance) ascending.
    pub fn knn(&self, query: &[f32], k: usize) -> Result<Vec<(i64, f64)>> {
        if k == 0 {
            return Ok(vec![]);
        }
        let conn = self.conn();
        let mut st = conn.prepare("SELECT rowid, distance FROM chunks_vec WHERE embedding MATCH ?1 AND k = ?2 ORDER BY distance")?;
        let rows = st.query_map(params![to_blob(query), k as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// BM25-ranked chunk ids for an FTS5 MATCH expression (see `search::fts_query`).
    pub fn fts(&self, match_expr: &str, k: usize) -> Result<Vec<i64>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY bm25(chunks_fts) LIMIT ?2")?;
        let rows = st.query_map(params![match_expr, k as i64], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn hits(&self, ids: &[i64]) -> Result<HashMap<i64, Hit>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT c.id, f.path, c.heading, c.context_path, c.chunk_index, c.text, f.tags,
                    (SELECT COUNT(DISTINCT l.source_id) FROM links l WHERE l.target = f.stem AND l.source_id <> f.id)
             FROM chunks c JOIN files f ON f.id = c.file_id WHERE c.id = ?1",
        )?;
        let mut out = HashMap::new();
        for id in ids {
            let hit = st
                .query_row([id], |r| {
                    Ok(Hit {
                        id: r.get(0)?,
                        file_path: r.get(1)?,
                        heading: r.get(2)?,
                        context_path: r.get(3)?,
                        chunk_index: r.get(4)?,
                        text: r.get(5)?,
                        tags: r.get(6)?,
                        importance_score: r.get::<_, i64>(7)?.min(MAX_BACKLINKS as i64),
                    })
                })
                .optional()?;
            if let Some(h) = hit {
                out.insert(*id, h);
            }
        }
        Ok(out)
    }

    /// First-chunk vector of up to `max_files` files, for the UMAP graph.
    pub fn vector_sample(&self, max_files: usize) -> Result<Vec<VectorNode>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT f.path, v.embedding, c.text, f.tags, f.mtime_ms
             FROM chunks c JOIN files f ON f.id = c.file_id JOIN chunks_vec v ON v.rowid = c.id
             WHERE c.chunk_index = 0 ORDER BY f.path LIMIT ?1",
        )?;
        let rows = st.query_map([max_files as i64], |r| {
            Ok(VectorNode {
                id: r.get(0)?,
                vector: from_blob(&r.get::<_, Vec<u8>>(1)?),
                text: r.get::<_, String>(2)?.chars().take(120).collect(),
                tags: r.get(3)?,
                last_modified: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn ensure_file(tx: &rusqlite::Transaction, path: &str) -> rusqlite::Result<i64> {
    tx.execute("INSERT OR IGNORE INTO files(path, stem) VALUES (?1, ?2)", params![path, stem_of(path)])?;
    tx.query_row("SELECT id FROM files WHERE path = ?1", [path], |r| r.get(0))
}

fn register_sqlite_vec() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        #[allow(clippy::missing_transmute_annotations)]
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(sqlite_vec::sqlite3_vec_init as *const ())));
    });
}

fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn from_blob(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

pub fn stem_of(path: &str) -> String {
    Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIM: usize = 4;

    fn store() -> Store {
        Store::open_in_memory("test-model", DIM).unwrap()
    }

    fn chunk(i: usize, text: &str, v: [f32; DIM]) -> ChunkRow {
        ChunkRow {
            chunk_index: i,
            heading: format!("H{i}"),
            context_path: format!("Doc > H{i}"),
            text: text.into(),
            embed_hash: format!("hash-{text}"),
            vector: v.to_vec(),
        }
    }

    fn file(path: &str, chunks: Vec<ChunkRow>) -> FileRecord {
        FileRecord { path: path.into(), mtime_ms: 100, content_hash: "c1".into(), tags: "a, b".into(), chunks }
    }

    #[test]
    fn open_creates_schema_and_reports_fresh_db_as_needing_index() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.db");
        let (s, reset) = Store::open(&p, "m", DIM).unwrap();
        assert!(reset, "brand new store needs a full index");
        assert_eq!(s.chunk_count().unwrap(), 0);
        drop(s);
        let (_, reset) = Store::open(&p, "m", DIM).unwrap();
        assert!(!reset, "same model + dim reopens without wiping");
    }

    #[test]
    fn open_wipes_when_model_or_dim_changes() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.db");
        let (s, _) = Store::open(&p, "m1", DIM).unwrap();
        s.replace_file(&file("/v/a.md", vec![chunk(0, "x", [1.0, 0.0, 0.0, 0.0])])).unwrap();
        drop(s);
        let (s, reset) = Store::open(&p, "m2", DIM).unwrap();
        assert!(reset, "same dim, different model: vectors are incompatible");
        assert_eq!(s.chunk_count().unwrap(), 0);
        drop(s);
        let (s, reset) = Store::open(&p, "m2", 8).unwrap();
        assert!(reset);
        assert_eq!(s.dim(), 8);
    }

    #[test]
    fn replace_file_stores_chunks_and_state() {
        let s = store();
        s.replace_file(&file("/v/a.md", vec![chunk(0, "alpha", [1.0, 0.0, 0.0, 0.0]), chunk(1, "beta", [0.0, 1.0, 0.0, 0.0])])).unwrap();
        assert_eq!(s.chunk_count().unwrap(), 2);
        assert_eq!(s.file_state("/v/a.md").unwrap(), Some(FileState { mtime_ms: 100, content_hash: "c1".into() }));
        assert_eq!(s.file_state("/v/missing.md").unwrap(), None);
        assert_eq!(s.indexed_paths().unwrap(), vec!["/v/a.md"]);
    }

    #[test]
    fn replace_file_twice_leaves_only_the_new_chunks_everywhere() {
        let s = store();
        s.replace_file(&file("/v/a.md", vec![chunk(0, "alpha", [1.0, 0.0, 0.0, 0.0]), chunk(1, "beta", [0.0, 1.0, 0.0, 0.0])])).unwrap();
        s.replace_file(&file("/v/a.md", vec![chunk(0, "gamma", [0.0, 0.0, 1.0, 0.0])])).unwrap();
        assert_eq!(s.chunk_count().unwrap(), 1);
        assert!(s.fts("\"alpha\"", 10).unwrap().is_empty(), "FTS rows follow chunk deletes");
        assert_eq!(s.knn(&[1.0, 0.0, 0.0, 0.0], 10).unwrap().len(), 1, "vector rows follow chunk deletes");
    }

    #[test]
    fn replace_file_rejects_wrong_dimension_without_partial_write() {
        let s = store();
        let bad = file("/v/a.md", vec![chunk(0, "ok", [1.0, 0.0, 0.0, 0.0]), ChunkRow { vector: vec![1.0; 3], ..chunk(1, "bad", [0.0; DIM]) }]);
        assert!(s.replace_file(&bad).is_err());
        assert_eq!(s.chunk_count().unwrap(), 0, "transaction rolled back");
    }

    #[test]
    fn touch_file_updates_mtime_only() {
        let s = store();
        s.replace_file(&file("/v/a.md", vec![chunk(0, "x", [1.0, 0.0, 0.0, 0.0])])).unwrap();
        s.touch_file("/v/a.md", 999).unwrap();
        assert_eq!(s.file_state("/v/a.md").unwrap().unwrap(), FileState { mtime_ms: 999, content_hash: "c1".into() });
        assert_eq!(s.chunk_count().unwrap(), 1);
    }

    #[test]
    fn delete_path_removes_single_file_or_whole_directory() {
        let s = store();
        for p in ["/v/a.md", "/v/sub/b.md", "/v/sub/c.md", "/v/subway.md"] {
            s.replace_file(&file(p, vec![chunk(0, p, [1.0, 0.0, 0.0, 0.0])])).unwrap();
        }
        assert_eq!(s.delete_path("/v/a.md").unwrap(), 1);
        assert_eq!(s.delete_path("/v/sub").unwrap(), 2, "directory prefix, not string prefix");
        assert_eq!(s.indexed_paths().unwrap(), vec!["/v/subway.md"]);
        assert_eq!(s.chunk_count().unwrap(), 1);
        assert_eq!(s.knn(&[1.0, 0.0, 0.0, 0.0], 10).unwrap().len(), 1);
    }

    #[test]
    fn delete_path_handles_windows_separators() {
        let s = store();
        s.replace_file(&file(r"C:\v\sub\b.md", vec![chunk(0, "b", [1.0, 0.0, 0.0, 0.0])])).unwrap();
        assert_eq!(s.delete_path(r"C:\v\sub").unwrap(), 1);
    }

    #[test]
    fn paths_with_quotes_are_safe() {
        let s = store();
        let p = r#"/v/it's "quoted".md"#;
        s.replace_file(&file(p, vec![chunk(0, "q", [1.0, 0.0, 0.0, 0.0])])).unwrap();
        assert_eq!(s.delete_path(p).unwrap(), 1);
    }

    #[test]
    fn clear_empties_everything() {
        let s = store();
        s.replace_file(&file("/v/a.md", vec![chunk(0, "x", [1.0, 0.0, 0.0, 0.0])])).unwrap();
        s.set_links("/v/a.md", &["b".into()]).unwrap();
        s.clear().unwrap();
        assert_eq!(s.chunk_count().unwrap(), 0);
        assert!(s.indexed_paths().unwrap().is_empty());
        assert!(s.backlink_titles("b", 5).unwrap().is_empty());
    }

    #[test]
    fn knn_orders_by_cosine_distance() {
        let s = store();
        s.replace_file(&file("/v/a.md", vec![chunk(0, "east", [1.0, 0.0, 0.0, 0.0]), chunk(1, "north", [0.0, 1.0, 0.0, 0.0]), chunk(2, "northeast", [0.7, 0.7, 0.0, 0.0])])).unwrap();
        let hits = s.knn(&[0.0, 2.0, 0.0, 0.0], 3).unwrap();
        let texts: Vec<String> = hits.iter().map(|(id, _)| s.hits(&[*id]).unwrap()[id].text.clone()).collect();
        assert_eq!(texts, vec!["north", "northeast", "east"]);
        assert!(hits[0].1 < 1e-5, "identical direction ≈ zero distance regardless of magnitude");
        assert!(hits.windows(2).all(|w| w[0].1 <= w[1].1));
        assert_eq!(s.knn(&[1.0, 0.0, 0.0, 0.0], 1).unwrap().len(), 1, "k limits results");
    }

    #[test]
    fn fts_ranks_matching_chunks_with_bm25() {
        let s = store();
        s.replace_file(&file("/v/a.md", vec![
            chunk(0, "rust rust rust ownership", [1.0, 0.0, 0.0, 0.0]),
            chunk(1, "a note about rust", [0.0, 1.0, 0.0, 0.0]),
            chunk(2, "nothing relevant", [0.0, 0.0, 1.0, 0.0]),
        ])).unwrap();
        let ids = s.fts("\"rust\"", 10).unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(s.hits(&ids[..1]).unwrap()[&ids[0]].chunk_index, 0, "denser match ranks first");
        assert!(s.fts("\"absent\"", 10).unwrap().is_empty());
    }

    #[test]
    fn fts_is_accent_and_case_insensitive() {
        let s = store();
        s.replace_file(&file("/v/a.md", vec![chunk(0, "Reflexión sobre el Área", [1.0, 0.0, 0.0, 0.0])])).unwrap();
        assert_eq!(s.fts("\"reflexion\"", 10).unwrap().len(), 1);
        assert_eq!(s.fts("\"AREA\"", 10).unwrap().len(), 1);
    }

    #[test]
    fn hits_return_chunk_fields_and_importance() {
        let s = store();
        s.replace_file(&file("/v/Target.md", vec![chunk(0, "t", [1.0, 0.0, 0.0, 0.0])])).unwrap();
        s.set_links("/v/x.md", &["Target".into()]).unwrap();
        s.set_links("/v/y.md", &["target".into()]).unwrap();
        let id = s.knn(&[1.0, 0.0, 0.0, 0.0], 1).unwrap()[0].0;
        let h = &s.hits(&[id]).unwrap()[&id];
        assert_eq!((h.file_path.as_str(), h.heading.as_str(), h.context_path.as_str(), h.tags.as_str()), ("/v/Target.md", "H0", "Doc > H0", "a, b"));
        assert_eq!(h.importance_score, 2, "backlinks match case-insensitively");
    }

    #[test]
    fn importance_is_capped_and_ignores_self_links() {
        let s = store();
        s.replace_file(&file("/v/Hub.md", vec![chunk(0, "hub", [1.0, 0.0, 0.0, 0.0])])).unwrap();
        s.set_links("/v/Hub.md", &["Hub".into()]).unwrap();
        for i in 0..8 {
            s.set_links(&format!("/v/n{i}.md"), &["Hub".into()]).unwrap();
        }
        let id = s.knn(&[1.0, 0.0, 0.0, 0.0], 1).unwrap()[0].0;
        assert_eq!(s.hits(&[id]).unwrap()[&id].importance_score, MAX_BACKLINKS as i64);
        let titles = s.backlink_titles("Hub", 5).unwrap();
        assert_eq!(titles.len(), 5);
        assert!(!titles.contains(&"Hub".to_string()));
    }

    #[test]
    fn set_links_replaces_previous_links_incrementally() {
        let s = store();
        s.set_links("/v/a.md", &["B".into(), "C".into()]).unwrap();
        assert_eq!(s.backlink_titles("B", 5).unwrap(), vec!["a"]);
        s.set_links("/v/a.md", &["C".into()]).unwrap();
        assert!(s.backlink_titles("B", 5).unwrap().is_empty(), "removed link no longer counts");
        assert_eq!(s.backlink_titles("C", 5).unwrap(), vec!["a"]);
        s.delete_path("/v/a.md").unwrap();
        assert!(s.backlink_titles("C", 5).unwrap().is_empty(), "deleting the source drops its links");
    }

    #[test]
    fn link_placeholder_rows_are_not_indexed_files() {
        let s = store();
        s.set_links("/v/a.md", &["B".into()]).unwrap();
        assert!(s.indexed_paths().unwrap().is_empty());
        assert_eq!(s.file_state("/v/a.md").unwrap(), None, "no hash yet means not indexed");
    }

    #[test]
    fn vectors_by_hash_returns_stored_vectors_for_reuse() {
        let s = store();
        s.replace_file(&file("/v/a.md", vec![chunk(0, "x", [0.5, 0.5, 0.0, 0.0])])).unwrap();
        let got = s.vectors_by_hash(&["hash-x".into(), "hash-nope".into()]).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got["hash-x"], vec![0.5, 0.5, 0.0, 0.0]);
        assert!(s.vectors_by_hash(&[]).unwrap().is_empty());
    }

    #[test]
    fn file_chunk_counts_are_sorted_by_count_desc() {
        let s = store();
        s.replace_file(&file("/v/one.md", vec![chunk(0, "a", [1.0, 0.0, 0.0, 0.0])])).unwrap();
        s.replace_file(&file("/v/two.md", vec![chunk(0, "b", [1.0, 0.0, 0.0, 0.0]), chunk(1, "c", [1.0, 0.0, 0.0, 0.0])])).unwrap();
        assert_eq!(s.file_chunk_counts().unwrap(), vec![("/v/two.md".to_string(), 2), ("/v/one.md".to_string(), 1)]);
    }

    #[test]
    fn vector_sample_returns_one_node_per_file_with_truncated_text() {
        let s = store();
        let long = "z".repeat(300);
        s.replace_file(&file("/v/a.md", vec![chunk(0, &long, [1.0, 0.0, 0.0, 0.0]), chunk(1, "second", [0.0, 1.0, 0.0, 0.0])])).unwrap();
        s.replace_file(&file("/v/b.md", vec![chunk(0, "b", [0.0, 0.0, 1.0, 0.0])])).unwrap();
        let nodes = s.vector_sample(10).unwrap();
        assert_eq!(nodes.len(), 2);
        let a = nodes.iter().find(|n| n.id == "/v/a.md").unwrap();
        assert_eq!(a.vector, vec![1.0, 0.0, 0.0, 0.0], "first chunk's vector");
        assert_eq!(a.text.chars().count(), 120);
        assert_eq!(a.last_modified, 100);
        assert_eq!(s.vector_sample(1).unwrap().len(), 1);
    }

    #[test]
    fn stem_of_strips_directories_and_extension() {
        assert_eq!(stem_of("/v/My Note.md"), "My Note");
        assert_eq!(stem_of("/v/archive.tar.gz"), "archive.tar");
    }
}

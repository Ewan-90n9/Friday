use rusqlite::Connection;
use rusqlite::ffi::sqlite3_auto_extension;
use sqlite_vec::sqlite3_vec_init;
use std::sync::{Mutex, Once};

static VEC_INIT: Once = Once::new();

pub struct VecStore {
    conn: Mutex<Connection>,
}

impl VecStore {
    pub fn new(db_path: &str) -> Result<Self, String> {
        // Register sqlite-vec as an auto-extension (once, globally).
        // sqlite3_vec_init is declared with no args in the Rust FFI;
        // sqlite3_auto_extension will invoke it with the proper args.
        VEC_INIT.call_once(|| {
            unsafe {
                sqlite3_auto_extension(Some(std::mem::transmute(
                    sqlite3_vec_init as *const (),
                )));
            }
        });

        let conn = Connection::open(db_path)
            .map_err(|e| format!("failed to open vec db: {e}"))?;

        // Create virtual table if not exists
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS experiences_vec USING vec0(\
                id TEXT PRIMARY KEY,\
                embedding FLOAT[512]\
            );",
        )
        .map_err(|e| format!("failed to create vec table: {e}"))?;

        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn upsert_vector(&self, id: &str, embedding: &[f32]) -> Result<(), String> {
        let mut conn = self.conn.lock().map_err(|e| format!("vec store lock: {e}"))?;
        let embedding_bytes: Vec<u8> = embedding
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        // vec0 virtual tables don't support INSERT OR REPLACE —
        // delete first (no-op if absent), then insert within a transaction.
        let tx = conn
            .transaction()
            .map_err(|e| format!("failed to begin tx: {e}"))?;
        tx.execute(
            "DELETE FROM experiences_vec WHERE id = ?",
            rusqlite::params![id],
        )
        .map_err(|e| format!("failed to delete old vector: {e}"))?;
        tx.execute(
            "INSERT INTO experiences_vec (id, embedding) VALUES (?, ?)",
            rusqlite::params![id, embedding_bytes],
        )
        .map_err(|e| format!("failed to insert vector: {e}"))?;
        tx.commit()
            .map_err(|e| format!("failed to commit upsert: {e}"))?;
        Ok(())
    }

    pub fn delete_vector(&self, id: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| format!("vec store lock: {e}"))?;
        conn.execute(
            "DELETE FROM experiences_vec WHERE id = ?",
            rusqlite::params![id],
        )
        .map_err(|e| format!("failed to delete vector: {e}"))?;
        Ok(())
    }

    /// Query top-K nearest neighbors by embedding.
    /// Returns (experience_id, distance) pairs.
    pub fn query(
        &self,
        embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<(String, f32)>, String> {
        let conn = self.conn.lock().map_err(|e| format!("vec store lock: {e}"))?;
        let embedding_bytes: Vec<u8> = embedding
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();

        let mut stmt = conn
            .prepare("SELECT id, distance FROM experiences_vec WHERE embedding MATCH ? ORDER BY distance ASC LIMIT ?")
            .map_err(|e| format!("failed to prepare query: {e}"))?;

        let rows = stmt
            .query_map(rusqlite::params![embedding_bytes, limit as i64], |row| {
                let id: String = row.get(0)?;
                let distance: f32 = row.get(1)?;
                Ok((id, distance))
            })
            .map_err(|e| format!("failed to query vectors: {e}"))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| format!("row mapping error: {e}"))?);
        }
        Ok(result)
    }

    /// Query top-K nearest neighbors, filtered by a set of experience IDs.
    /// This is used for outcome-filtered retrieval (positive-only, negative-only).
    pub fn query_filtered(
        &self,
        embedding: &[f32],
        limit: usize,
        allowed_ids: &[String],
    ) -> Result<Vec<(String, f32)>, String> {
        if allowed_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().map_err(|e| format!("vec store lock: {e}"))?;
        let embedding_bytes: Vec<u8> = embedding
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();

        // Build placeholder list for IN clause.
        // Use vec0's hidden `k` column instead of LIMIT — when combined with
        // `id IN (...)`, SQLite's planner may not forward LIMIT to the virtual
        // table, causing "A LIMIT or 'k = ?' constraint is required" errors.
        let placeholders: Vec<String> = (0..allowed_ids.len())
            .map(|_| "?".to_string())
            .collect();
        let sql = format!(
            "SELECT id, distance FROM experiences_vec WHERE embedding MATCH ? AND id IN ({}) AND k = ? ORDER BY distance",
            placeholders.join(", ")
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("failed to prepare filtered query: {e}"))?;

        // Bind embedding bytes first, then each allowed_id, then k (limit)
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        params_vec.push(Box::new(embedding_bytes));
        for id in allowed_ids {
            params_vec.push(Box::new(id.clone()));
        }
        params_vec.push(Box::new(limit as i64));

        let param_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();

        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                let id: String = row.get(0)?;
                let distance: f32 = row.get(1)?;
                Ok((id, distance))
            })
            .map_err(|e| format!("failed to query filtered vectors: {e}"))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| format!("row mapping error: {e}"))?);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_dummy_vec(id: &str) -> (String, Vec<f32>) {
        let embedding: Vec<f32> = (0..512).map(|i| (i as f32) * 0.001).collect();
        (id.to_string(), embedding)
    }

    #[test]
    fn test_upsert_and_query_basic() {
        let tmp = tempfile::tempdir().unwrap();
        let store = VecStore::new(tmp.path().join("vec.db").to_str().unwrap()).unwrap();

        let (id, embedding) = make_dummy_vec("exp-1");
        store.upsert_vector(&id, &embedding).unwrap();

        let results = store.query(&embedding, 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "exp-1");
        assert!(results[0].1 < 0.01);
    }

    #[test]
    fn test_upsert_replaces_on_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let store = VecStore::new(tmp.path().join("vec.db").to_str().unwrap()).unwrap();

        let (_, embedding1) = make_dummy_vec("exp-1");
        store.upsert_vector("exp-1", &embedding1).unwrap();

        let embedding2: Vec<f32> = (0..512).map(|i| (i as f32) * 0.002).collect();
        store.upsert_vector("exp-1", &embedding2).unwrap();

        let results = store.query(&embedding2, 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "exp-1");
    }

    #[test]
    fn test_query_filtered() {
        let tmp = tempfile::tempdir().unwrap();
        let store = VecStore::new(tmp.path().join("vec.db").to_str().unwrap()).unwrap();

        let (id1, emb1) = make_dummy_vec("exp-1");
        let (id2, emb2) = make_dummy_vec("exp-2");
        store.upsert_vector(&id1, &emb1).unwrap();
        store.upsert_vector(&id2, &emb2).unwrap();

        let results = store.query_filtered(&emb1, 5, &["exp-2".to_string()]).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "exp-2");
    }

    #[test]
    fn test_query_filtered_empty_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let store = VecStore::new(tmp.path().join("vec.db").to_str().unwrap()).unwrap();

        let (_, emb) = make_dummy_vec("exp-1");
        let results = store.query_filtered(&emb, 5, &[]).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_delete_vector() {
        let tmp = tempfile::tempdir().unwrap();
        let store = VecStore::new(tmp.path().join("vec.db").to_str().unwrap()).unwrap();

        let (id, emb) = make_dummy_vec("exp-1");
        store.upsert_vector(&id, &emb).unwrap();
        store.delete_vector(&id).unwrap();

        let results = store.query(&emb, 1).unwrap();
        assert!(results.is_empty());
    }
}

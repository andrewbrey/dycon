use std::path::Path;
use std::path::PathBuf;

use rusqlite::Connection;

pub(crate) trait ContentProvider: Send + Sync {
    fn extra_content(&self, rel_path: &Path) -> anyhow::Result<Vec<u8>>;
}

pub(crate) struct SqliteProvider {
    db_path: PathBuf,
}

impl SqliteProvider {
    pub fn new(db_path: PathBuf) -> Self {
        Self { db_path }
    }

    pub fn ensure_schema(&self) -> anyhow::Result<()> {
        let conn = Connection::open(&self.db_path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS snippets (
                id INTEGER PRIMARY KEY,
                filename TEXT NOT NULL,
                content TEXT NOT NULL,
                sort_order INTEGER DEFAULT 0
            )",
        )?;
        Ok(())
    }
}

impl ContentProvider for SqliteProvider {
    fn extra_content(&self, rel_path: &Path) -> anyhow::Result<Vec<u8>> {
        let conn = Connection::open(&self.db_path)?;
        let filename = rel_path.to_string_lossy();
        let mut stmt =
            conn.prepare("SELECT content FROM snippets WHERE filename = ?1 ORDER BY sort_order")?;
        let rows: Vec<String> = stmt
            .query_map([filename.as_ref()], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let joined = rows.join("\n");
        Ok(joined.into_bytes())
    }
}

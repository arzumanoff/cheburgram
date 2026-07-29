use anyhow::Result;
use cheburgram_protocol::TextMessage;
use rusqlite::{params, Connection};
use std::fs;
use std::path::Path;
use tracing::info;

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let conn = Connection::open(path)?;
        let mut db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let mut db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&mut self) -> Result<()> {
        self.conn.execute_batch(
            "
            PRAGMA foreign_keys = ON;
            PRAGMA journal_mode = WAL;

            CREATE TABLE IF NOT EXISTS users (
                user_code TEXT PRIMARY KEY,
                client_id TEXT NOT NULL,
                name TEXT NOT NULL,
                token_hash TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS friends (
                user_code TEXT NOT NULL,
                friend_code TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (user_code, friend_code)
            );

            CREATE TABLE IF NOT EXISTS friend_requests (
                from_code TEXT NOT NULL,
                to_code TEXT NOT NULL,
                from_name TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (from_code, to_code)
            );

            CREATE TABLE IF NOT EXISTS offline_messages (
                id TEXT PRIMARY KEY,
                from_code TEXT NOT NULL,
                from_name TEXT NOT NULL,
                to_code TEXT NOT NULL,
                text TEXT NOT NULL,
                timestamp TEXT NOT NULL
            );
            ",
        )?;
        Ok(())
    }

    pub fn migrate_from_json_if_needed<P: AsRef<Path>>(&mut self, json_path: P) -> Result<()> {
        let path = json_path.as_ref();
        if !path.exists() {
            return Ok(());
        }

        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?;
        if count > 0 {
            return Ok(());
        }

        info!("📋 Обнаружен {}, выполняется миграция в SQLite...", path.display());
        let content = fs::read_to_string(path)?;
        let json_data: serde_json::Value = serde_json::from_str(&content)?;

        let tx = self.conn.transaction()?;
        let now = chrono::Utc::now().to_rfc3339();

        if let Some(clients) = json_data.get("clients").and_then(|v| v.as_object()) {
            for (code, entry) in clients {
                let client_id = entry.get("client_id").and_then(|v| v.as_str()).unwrap_or("");
                let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let token_hash = entry.get("token_hash").and_then(|v| v.as_str());

                tx.execute(
                    "INSERT OR REPLACE INTO users (user_code, client_id, name, token_hash, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                    params![code, client_id, name, token_hash, now],
                )?;

                if let Some(friends) = entry.get("friends").and_then(|v| v.as_array()) {
                    for f in friends {
                        if let Some(f_code) = f.as_str() {
                            tx.execute(
                                "INSERT OR IGNORE INTO friends (user_code, friend_code, created_at) VALUES (?1, ?2, ?3)",
                                params![code, f_code, now],
                            )?;
                        }
                    }
                }
            }
        }

        if let Some(pending) = json_data.get("pending_messages").and_then(|v| v.as_object()) {
            for (_to_code, msgs) in pending {
                if let Some(msg_list) = msgs.as_array() {
                    for m in msg_list {
                        let default_id = uuid::Uuid::new_v4().to_string();
                        let id = m.get("id").and_then(|v| v.as_str()).unwrap_or(&default_id);
                        let from_code = m.get("from_code").and_then(|v| v.as_str()).unwrap_or("");
                        let from_name = m.get("from_name").and_then(|v| v.as_str()).unwrap_or("");
                        let to_code = m.get("to_code").and_then(|v| v.as_str()).unwrap_or("");
                        let text = m.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        let ts = m.get("timestamp").and_then(|v| v.as_str()).unwrap_or(&now);

                        tx.execute(
                            "INSERT OR IGNORE INTO offline_messages (id, from_code, from_name, to_code, text, timestamp) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            params![id, from_code, from_name, to_code, text, ts],
                        )?;
                    }
                }
            }
        }

        tx.commit()?;

        let bak_path = path.with_extension("json.bak");
        let _ = fs::rename(path, &bak_path);
        info!("✅ Миграция завершена! Резервная копия сохранена в {}", bak_path.display());
        Ok(())
    }

    pub fn upsert_user(&mut self, user_code: &str, client_id: &str, name: &str, token_hash: Option<&str>) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO users (user_code, client_id, name, token_hash, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(user_code) DO UPDATE SET
                client_id = excluded.client_id,
                name = excluded.name,
                token_hash = COALESCE(excluded.token_hash, users.token_hash),
                updated_at = excluded.updated_at",
            params![user_code, client_id, name, token_hash, now],
        )?;
        Ok(())
    }

    pub fn add_friend(&mut self, user_code: &str, friend_code: &str) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO friends (user_code, friend_code, created_at) VALUES (?1, ?2, ?3)",
            params![user_code, friend_code, now],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO friends (user_code, friend_code, created_at) VALUES (?1, ?2, ?3)",
            params![friend_code, user_code, now],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn remove_friend(&mut self, user_code: &str, friend_code: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM friends WHERE user_code = ?1 AND friend_code = ?2",
            params![user_code, friend_code],
        )?;
        tx.execute(
            "DELETE FROM friends WHERE user_code = ?2 AND friend_code = ?1",
            params![user_code, friend_code],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn save_offline_message(&mut self, msg: &TextMessage) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO offline_messages (id, from_code, from_name, to_code, text, timestamp) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![msg.id, msg.from_code, msg.from_name, msg.to_code, msg.text, msg.timestamp],
        )?;
        Ok(())
    }

    pub fn get_offline_messages(&mut self, user_code: &str) -> Result<Vec<TextMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, from_code, from_name, to_code, text, timestamp FROM offline_messages WHERE to_code = ?1 ORDER BY timestamp ASC",
        )?;
        let rows = stmt.query_map(params![user_code], |row| {
            Ok(TextMessage {
                id: row.get(0)?,
                from_code: row.get(1)?,
                from_name: row.get(2)?,
                to_code: row.get(3)?,
                text: row.get(4)?,
                timestamp: row.get(5)?,
            })
        })?;

        let mut msgs = Vec::new();
        for r in rows {
            msgs.push(r?);
        }

        self.conn.execute("DELETE FROM offline_messages WHERE to_code = ?1", params![user_code])?;
        Ok(msgs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sqlite_db_basic_ops() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_user("100001", "client-1", "Алиса", Some("hash123")).unwrap();
        db.upsert_user("100002", "client-2", "Боб", Some("hash456")).unwrap();

        db.add_friend("100001", "100002").unwrap();

        let msg = TextMessage {
            id: "msg-1".into(),
            from_code: "100001".into(),
            from_name: "Алиса".into(),
            to_code: "100002".into(),
            text: "Привет Боб".into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };

        db.save_offline_message(&msg).unwrap();

        let msgs = db.get_offline_messages("100002").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].text, "Привет Боб");

        let empty_msgs = db.get_offline_messages("100002").unwrap();
        assert!(empty_msgs.is_empty());
    }

    #[test]
    fn test_json_migration() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let json_path = tmp_dir.path().join("clients.json");
        let db_path = tmp_dir.path().join("cheburgram.db");

        let sample_json = r#"{
            "clients": {
                "123456": {
                    "client_id": "c-1",
                    "name": "Тест",
                    "last_seen": "2026-07-29T12:00:00Z",
                    "token_hash": "abc"
                }
            },
            "pending_requests": {},
            "pending_messages": {}
        }"#;

        fs::write(&json_path, sample_json).unwrap();

        let mut db = Db::open(&db_path).unwrap();
        db.migrate_from_json_if_needed(&json_path).unwrap();

        let count: i64 = db.conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);

        assert!(!json_path.exists());
        assert!(tmp_dir.path().join("clients.json.bak").exists());
    }
}

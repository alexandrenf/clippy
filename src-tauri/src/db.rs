use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_ITEM_CHARS: usize = 100_000;
pub const MAX_SECTION_CHARS: usize = 80;
pub const MAX_BATCH_ITEMS: usize = 10_000;

pub struct Db(pub Mutex<Connection>);

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Section {
    pub id: i64,
    pub name: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: i64,
    pub section_id: Option<i64>,
    pub content: String,
    pub done: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AppState {
    pub sections: Vec<Section>,
    pub items: Vec<Item>,
    pub active_section_id: Option<i64>,
    pub theme: String,
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn init(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         PRAGMA synchronous = NORMAL;
         PRAGMA busy_timeout = 1000;
         CREATE TABLE IF NOT EXISTS sections(
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           name TEXT NOT NULL,
           created_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS items(
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           section_id INTEGER REFERENCES sections(id) ON DELETE SET NULL,
           content TEXT NOT NULL,
           done INTEGER NOT NULL DEFAULT 0,
           created_at INTEGER NOT NULL,
           updated_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS settings(
           key TEXT PRIMARY KEY,
           value TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_items_section_id ON items(section_id, id);",
    )?;
    Ok(conn)
}

pub fn get_setting(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        params![key],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO settings(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn get_active_section(conn: &Connection) -> Option<i64> {
    let id: i64 = get_setting(conn, "active_section")?.parse().ok()?;
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sections WHERE id = ?1)",
            params![id],
            |r| r.get(0),
        )
        .unwrap_or(false);
    exists.then_some(id)
}

pub fn set_active_section(conn: &Connection, id: Option<i64>) -> rusqlite::Result<()> {
    set_setting(
        conn,
        "active_section",
        &id.map(|v| v.to_string()).unwrap_or_default(),
    )
}

pub fn section_exists(conn: &Connection, id: i64) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sections WHERE id = ?1)",
        params![id],
        |row| row.get(0),
    )
}

pub fn section_name_exists(
    conn: &Connection,
    name: &str,
    except_id: Option<i64>,
) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM sections
           WHERE name = ?1 COLLATE NOCASE AND (?2 IS NULL OR id != ?2)
         )",
        params![name, except_id],
        |row| row.get(0),
    )
}

pub fn list_sections(conn: &Connection) -> rusqlite::Result<Vec<Section>> {
    let mut stmt = conn.prepare("SELECT id, name FROM sections ORDER BY id")?;
    let rows = stmt.query_map([], |r| {
        Ok(Section {
            id: r.get(0)?,
            name: r.get(1)?,
        })
    })?;
    rows.collect()
}

pub fn list_items(conn: &Connection) -> rusqlite::Result<Vec<Item>> {
    let mut stmt = conn.prepare(
        "SELECT id, section_id, content, done, created_at, updated_at FROM items ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Item {
            id: r.get(0)?,
            section_id: r.get(1)?,
            content: r.get(2)?,
            done: r.get::<_, i64>(3)? != 0,
            created_at: r.get(4)?,
            updated_at: r.get(5)?,
        })
    })?;
    rows.collect()
}

pub fn get_state(conn: &Connection) -> rusqlite::Result<AppState> {
    Ok(AppState {
        sections: list_sections(conn)?,
        items: list_items(conn)?,
        active_section_id: get_active_section(conn),
        theme: get_setting(conn, "theme").unwrap_or_else(|| "system".into()),
    })
}

pub fn insert_item(
    conn: &Connection,
    content: &str,
    section_id: Option<i64>,
) -> rusqlite::Result<i64> {
    let t = now_ms();
    conn.execute(
        "INSERT INTO items(section_id, content, done, created_at, updated_at) VALUES(?1, ?2, 0, ?3, ?3)",
        params![section_id, content, t],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn is_recent_duplicate(
    conn: &Connection,
    content: &str,
    section_id: Option<i64>,
    since: i64,
) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(
           SELECT 1
           FROM (SELECT section_id, content, created_at FROM items ORDER BY id DESC LIMIT 1)
           WHERE section_id IS ?1 AND content = ?2 AND created_at >= ?3
         )",
        params![section_id, content, since],
        |row| row.get(0),
    )
}

/// Find a section by name (case-insensitive) or create it. Returns its id.
pub fn find_or_create_section(conn: &Connection, name: &str) -> rusqlite::Result<i64> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM sections WHERE name = ?1 COLLATE NOCASE",
            params![name],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }
    conn.execute(
        "INSERT INTO sections(name, created_at) VALUES(?1, ?2)",
        params![name, now_ms()],
    )?;
    Ok(conn.last_insert_rowid())
}

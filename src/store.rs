use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::core::{Event, EventFilter, EventStore, EventType, Result, ShuttleError};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use uuid::Uuid;

#[derive(Clone)]
pub struct SqliteEventStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteEventStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(to_store_error)?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.init()?;
        Ok(store)
    }

    pub fn init(&self) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS events (
                id TEXT PRIMARY KEY NOT NULL,
                event_type TEXT NOT NULL,
                workspace_id TEXT NOT NULL,
                repo_id TEXT,
                repo_path TEXT,
                git_remote TEXT,
                bit_repo_id TEXT,
                branch TEXT,
                commit_hash TEXT,
                repo_dirty INTEGER,
                agent TEXT NOT NULL,
                session_id TEXT NOT NULL,
                title TEXT,
                content TEXT NOT NULL,
                tags TEXT NOT NULL,
                metadata_json TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS event_tags (
                event_id TEXT NOT NULL,
                tag TEXT NOT NULL,
                PRIMARY KEY (event_id, tag),
                FOREIGN KEY (event_id) REFERENCES events(id)
            );

            CREATE INDEX IF NOT EXISTS idx_events_type_created ON events(event_type, created_at);
            CREATE INDEX IF NOT EXISTS idx_events_workspace_created ON events(workspace_id, created_at);
            CREATE INDEX IF NOT EXISTS idx_events_agent_created ON events(agent, created_at);
            CREATE INDEX IF NOT EXISTS idx_event_tags_tag ON event_tags(tag);
            "#,
        )
        .map_err(to_store_error)?;
        ensure_column(&conn, "repo_path", "TEXT")?;
        ensure_column(&conn, "git_remote", "TEXT")?;
        ensure_column(&conn, "bit_repo_id", "TEXT")?;
        ensure_column(&conn, "repo_dirty", "INTEGER")?;
        ensure_column(&conn, "metadata_json", "TEXT NOT NULL DEFAULT '{}'")?;
        backfill_event_tags(&conn)?;
        Ok(())
    }

    pub fn append_if_absent(&self, event: Event) -> Result<bool> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
        insert_event(&mut conn, event, InsertMode::IgnoreDuplicates)
    }
}

#[async_trait]
impl EventStore for SqliteEventStore {
    async fn append(&self, event: Event) -> Result<Event> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
        insert_event(&mut conn, event.clone(), InsertMode::Strict)?;
        Ok(event)
    }

    async fn list(&self, filter: EventFilter) -> Result<Vec<Event>> {
        let conn = self
            .conn
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
        let limit = filter.limit.unwrap_or(50);
        let event_type = filter
            .event_type
            .map(|event_type| Value::Text(event_type.as_str().to_owned()));
        let query = filter
            .query
            .as_ref()
            .map(|query| Value::Text(format!("%{}%", query.to_lowercase())));
        let tags = filter_tags(&filter);
        let mut sql = r#"
                SELECT id, event_type, workspace_id, repo_id, repo_path, git_remote, bit_repo_id, branch, commit_hash, repo_dirty,
                       agent, session_id, title, content, tags, metadata_json, created_at
                FROM events
                WHERE (?1 IS NULL OR event_type = ?1)
                  AND (?2 IS NULL OR workspace_id = ?2)
                  AND (?3 IS NULL OR agent = ?3)
                  AND (
                    ?4 IS NULL
                    OR json_extract(metadata_json, '$.to') = ?4
                    OR EXISTS (
                      SELECT 1 FROM event_tags
                      WHERE event_tags.event_id = events.id AND event_tags.tag = ('to:' || ?4)
                    )
                  )
                  AND (
                    ?5 IS NULL
                    OR lower(coalesce(title, '')) LIKE ?5
                    OR lower(content) LIKE ?5
                    OR lower(tags) LIKE ?5
                    OR lower(metadata_json) LIKE ?5
                  )
        "#
        .to_owned();
        let mut values = vec![
            event_type.unwrap_or(Value::Null),
            filter.workspace_id.map(Value::Text).unwrap_or(Value::Null),
            filter.agent.map(Value::Text).unwrap_or(Value::Null),
            filter.recipient.map(Value::Text).unwrap_or(Value::Null),
            query.unwrap_or(Value::Null),
        ];
        for tag in tags {
            let index = values.len() + 1;
            sql.push_str(&format!(
                " AND EXISTS (SELECT 1 FROM event_tags WHERE event_tags.event_id = events.id AND event_tags.tag = ?{index})"
            ));
            values.push(Value::Text(tag));
        }
        let limit_index = values.len() + 1;
        sql.push_str(&format!(" ORDER BY created_at DESC LIMIT ?{limit_index}"));
        values.push(Value::Integer(i64::from(limit)));

        let mut stmt = conn.prepare(&sql).map_err(to_store_error)?;

        let rows = stmt
            .query_map(params_from_iter(values.iter()), row_to_event)
            .map_err(to_store_error)?;

        let mut events = Vec::new();
        for row in rows {
            let event = row.map_err(to_store_error)?;
            events.push(event);
        }

        Ok(events)
    }
}

#[derive(Debug, Clone, Copy)]
enum InsertMode {
    Strict,
    IgnoreDuplicates,
}

fn insert_event(conn: &mut Connection, event: Event, mode: InsertMode) -> Result<bool> {
    let tags = serde_json::to_string(&event.tags)
        .map_err(|err| ShuttleError::Serialization(err.to_string()))?;
    let metadata_json = serde_json::to_string(&event.metadata_json)
        .map_err(|err| ShuttleError::Serialization(err.to_string()))?;
    let insert = match mode {
        InsertMode::Strict => "INSERT INTO events",
        InsertMode::IgnoreDuplicates => "INSERT OR IGNORE INTO events",
    };

    let tx = conn.transaction().map_err(to_store_error)?;
    let inserted = tx
        .execute(
            &format!(
                r#"
            {insert} (
                id, event_type, workspace_id, repo_id, repo_path, git_remote, bit_repo_id, branch, commit_hash, repo_dirty,
                agent, session_id, title, content, tags, metadata_json, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
            "#
            ),
            params![
                event.id.to_string(),
                event.event_type.as_str(),
                &event.workspace_id,
                &event.repo_id,
                &event.repo_path,
                &event.git_remote,
                &event.bit_repo_id,
                &event.branch,
                &event.commit,
                event.repo_dirty,
                &event.agent,
                &event.session_id,
                &event.title,
                &event.content,
                tags,
                metadata_json,
                event.created_at.to_rfc3339(),
            ],
        )
        .map_err(to_store_error)?
        > 0;

    if inserted {
        for tag in &event.tags {
            tx.execute(
                "INSERT OR IGNORE INTO event_tags (event_id, tag) VALUES (?1, ?2)",
                params![event.id.to_string(), tag],
            )
            .map_err(to_store_error)?;
        }
    }
    tx.commit().map_err(to_store_error)?;

    Ok(inserted)
}

fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let event_type: String = row.get(1)?;
    let tags: String = row.get(14)?;
    let metadata_json: String = row.get(15)?;
    let created_at: String = row.get(16)?;

    let event_type = EventType::try_from(event_type.as_str()).map_err(to_sql_error)?;
    let tags = serde_json::from_str(&tags).map_err(to_sql_error)?;
    let metadata_json = serde_json::from_str(&metadata_json).map_err(to_sql_error)?;
    let created_at = DateTime::parse_from_rfc3339(&created_at)
        .map_err(to_sql_error)?
        .with_timezone(&Utc);

    Ok(Event {
        id: Uuid::parse_str(&row.get::<_, String>(0)?).map_err(to_sql_error)?,
        event_type,
        workspace_id: row.get(2)?,
        repo_id: row.get(3)?,
        repo_path: row.get(4)?,
        git_remote: row.get(5)?,
        bit_repo_id: row.get(6)?,
        branch: row.get(7)?,
        commit: row.get(8)?,
        repo_dirty: row.get(9)?,
        agent: row.get(10)?,
        session_id: row.get(11)?,
        title: row.get(12)?,
        content: row.get(13)?,
        tags,
        metadata_json,
        created_at,
    })
}

fn filter_tags(filter: &EventFilter) -> Vec<String> {
    let mut tags = filter.tags.clone();
    if let Some(tag) = &filter.tag {
        tags.push(tag.clone());
    }
    tags.sort();
    tags.dedup();
    tags
}

fn ensure_column(conn: &Connection, column: &str, column_type: &str) -> Result<()> {
    let exists = conn
        .prepare("PRAGMA table_info(events)")
        .map_err(to_store_error)?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(to_store_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(to_store_error)?
        .iter()
        .any(|name| name == column);

    if !exists {
        conn.execute(
            &format!("ALTER TABLE events ADD COLUMN {column} {column_type}"),
            [],
        )
        .map_err(to_store_error)?;
    }
    Ok(())
}

fn backfill_event_tags(conn: &Connection) -> Result<()> {
    let mut stmt = conn
        .prepare("SELECT id, tags FROM events")
        .map_err(to_store_error)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(to_store_error)?;

    for row in rows {
        let (event_id, tags_json) = row.map_err(to_store_error)?;
        let tags: Vec<String> = serde_json::from_str(&tags_json)
            .map_err(|err| ShuttleError::Serialization(err.to_string()))?;
        for tag in tags {
            conn.execute(
                "INSERT OR IGNORE INTO event_tags (event_id, tag) VALUES (?1, ?2)",
                params![event_id, tag],
            )
            .map_err(to_store_error)?;
        }
    }

    Ok(())
}

fn to_store_error(err: rusqlite::Error) -> ShuttleError {
    ShuttleError::Store(err.to_string())
}

fn to_sql_error<E>(err: E) -> rusqlite::Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    rusqlite::Error::ToSqlConversionFailure(Box::new(err))
}

pub fn database_exists(path: impl AsRef<Path>) -> Result<bool> {
    let conn = Connection::open(path).map_err(to_store_error)?;
    let exists = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'events'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(to_store_error)?
        .is_some();
    Ok(exists)
}

#[cfg(test)]
mod tests {
    use crate::core::{Event, NewEvent};
    use serde_json::json;

    use super::*;

    #[test]
    fn stores_and_filters_events() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteEventStore::open(dir.path().join("shuttle.db")).unwrap();
        let event = Event::new(NewEvent {
            event_type: EventType::Memory,
            workspace_id: "workspace".into(),
            repo_id: None,
            repo_path: None,
            git_remote: None,
            bit_repo_id: None,
            branch: None,
            commit: None,
            repo_dirty: None,
            agent: "codex".into(),
            session_id: "session".into(),
            title: None,
            content: "SQLite chosen for local-first storage".into(),
            tags: vec!["storage".into()],
            metadata_json: json!({}),
        });

        futures_executor::block_on(store.append(event)).unwrap();
        let events = futures_executor::block_on(store.list(EventFilter {
            event_type: Some(EventType::Memory),
            query: Some("sqlite".into()),
            ..EventFilter::default()
        }))
        .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].content, "SQLite chosen for local-first storage");
    }

    #[test]
    fn stores_metadata_and_filters_normalized_tags() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteEventStore::open(dir.path().join("shuttle.db")).unwrap();
        let event = Event::new(NewEvent {
            event_type: EventType::Message,
            workspace_id: "workspace".into(),
            repo_id: None,
            repo_path: None,
            git_remote: None,
            bit_repo_id: None,
            branch: None,
            commit: None,
            repo_dirty: None,
            agent: "codex".into(),
            session_id: "session".into(),
            title: None,
            content: "hello".into(),
            tags: vec!["important".into()],
            metadata_json: json!({ "to": "claude" }),
        });

        futures_executor::block_on(store.append(event)).unwrap();
        let tag_events = futures_executor::block_on(store.list(EventFilter {
            tag: Some("important".into()),
            ..EventFilter::default()
        }))
        .unwrap();
        let recipient_events = futures_executor::block_on(store.list(EventFilter {
            recipient: Some("claude".into()),
            ..EventFilter::default()
        }))
        .unwrap();

        assert_eq!(tag_events.len(), 1);
        assert_eq!(recipient_events.len(), 1);
        assert_eq!(recipient_events[0].metadata_json["to"], "claude");
    }

    #[test]
    fn filters_by_all_requested_tags() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteEventStore::open(dir.path().join("shuttle.db")).unwrap();
        let first = Event::new(NewEvent {
            event_type: EventType::Memory,
            workspace_id: "workspace".into(),
            repo_id: None,
            repo_path: None,
            git_remote: None,
            bit_repo_id: None,
            branch: None,
            commit: None,
            repo_dirty: None,
            agent: "codex".into(),
            session_id: "session".into(),
            title: None,
            content: "storage memory".into(),
            tags: vec!["storage".into(), "mvp".into()],
            metadata_json: json!({}),
        });
        let second = Event::new(NewEvent {
            event_type: EventType::Memory,
            workspace_id: "workspace".into(),
            repo_id: None,
            repo_path: None,
            git_remote: None,
            bit_repo_id: None,
            branch: None,
            commit: None,
            repo_dirty: None,
            agent: "codex".into(),
            session_id: "session".into(),
            title: None,
            content: "storage only".into(),
            tags: vec!["storage".into()],
            metadata_json: json!({}),
        });

        futures_executor::block_on(store.append(first)).unwrap();
        futures_executor::block_on(store.append(second)).unwrap();
        let events = futures_executor::block_on(store.list(EventFilter {
            tags: vec!["storage".into(), "mvp".into()],
            ..EventFilter::default()
        }))
        .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].content, "storage memory");
    }

    #[test]
    fn applies_query_filter_before_limit() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteEventStore::open(dir.path().join("shuttle.db")).unwrap();
        let old_match = Event::new(NewEvent {
            event_type: EventType::Memory,
            workspace_id: "workspace".into(),
            repo_id: None,
            repo_path: None,
            git_remote: None,
            bit_repo_id: None,
            branch: None,
            commit: None,
            repo_dirty: None,
            agent: "codex".into(),
            session_id: "session".into(),
            title: None,
            content: "needle memory".into(),
            tags: Vec::new(),
            metadata_json: json!({}),
        });
        futures_executor::block_on(store.append(old_match)).unwrap();
        for index in 0..75 {
            let event = Event::new(NewEvent {
                event_type: EventType::Memory,
                workspace_id: "workspace".into(),
                repo_id: None,
                repo_path: None,
                git_remote: None,
                bit_repo_id: None,
                branch: None,
                commit: None,
                repo_dirty: None,
                agent: "codex".into(),
                session_id: "session".into(),
                title: None,
                content: format!("recent memory {index}"),
                tags: Vec::new(),
                metadata_json: json!({}),
            });
            futures_executor::block_on(store.append(event)).unwrap();
        }

        let events = futures_executor::block_on(store.list(EventFilter {
            event_type: Some(EventType::Memory),
            query: Some("needle".into()),
            limit: Some(1),
            ..EventFilter::default()
        }))
        .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].content, "needle memory");
    }

    #[test]
    fn repo_dirty_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteEventStore::open(dir.path().join("shuttle.db")).unwrap();
        let event = Event::new(NewEvent {
            event_type: EventType::Decision,
            workspace_id: "workspace".into(),
            repo_id: Some("repo".into()),
            repo_path: Some("/repo".into()),
            git_remote: None,
            bit_repo_id: None,
            branch: Some("main".into()),
            commit: Some("abc".into()),
            repo_dirty: Some(true),
            agent: "codex".into(),
            session_id: "session".into(),
            title: None,
            content: "dirty decision".into(),
            tags: Vec::new(),
            metadata_json: json!({}),
        });

        futures_executor::block_on(store.append(event)).unwrap();
        let events = futures_executor::block_on(store.list(EventFilter {
            event_type: Some(EventType::Decision),
            ..EventFilter::default()
        }))
        .unwrap();

        assert_eq!(events[0].repo_dirty, Some(true));
    }

    #[test]
    fn append_if_absent_skips_duplicate_ids() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteEventStore::open(dir.path().join("shuttle.db")).unwrap();
        let event = Event::new(NewEvent {
            event_type: EventType::Memory,
            workspace_id: "workspace".into(),
            repo_id: None,
            repo_path: None,
            git_remote: None,
            bit_repo_id: None,
            branch: None,
            commit: None,
            repo_dirty: None,
            agent: "codex".into(),
            session_id: "session".into(),
            title: None,
            content: "replicated memory".into(),
            tags: vec!["sync".into()],
            metadata_json: json!({}),
        });

        assert!(store.append_if_absent(event.clone()).unwrap());
        assert!(!store.append_if_absent(event).unwrap());
        let events = futures_executor::block_on(store.list(EventFilter {
            event_type: Some(EventType::Memory),
            ..EventFilter::default()
        }))
        .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tags, vec!["sync"]);
    }

    #[test]
    fn backfills_legacy_tags_and_reads_legacy_recipients() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shuttle.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE events (
                id TEXT PRIMARY KEY NOT NULL,
                event_type TEXT NOT NULL,
                workspace_id TEXT NOT NULL,
                repo_id TEXT,
                repo_path TEXT,
                git_remote TEXT,
                bit_repo_id TEXT,
                branch TEXT,
                commit_hash TEXT,
                agent TEXT NOT NULL,
                session_id TEXT NOT NULL,
                title TEXT,
                content TEXT NOT NULL,
                tags TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            "#,
        )
        .unwrap();
        conn.execute(
            r#"
            INSERT INTO events (
                id, event_type, workspace_id, repo_id, repo_path, git_remote, bit_repo_id, branch, commit_hash,
                agent, session_id, title, content, tags, created_at
            ) VALUES (?1, 'message', 'workspace', NULL, NULL, NULL, NULL, NULL, NULL, 'codex', 'session', NULL, 'legacy', ?2, ?3)
            "#,
            params![
                Uuid::new_v4().to_string(),
                serde_json::to_string(&vec!["to:claude".to_owned(), "legacy".to_owned()]).unwrap(),
                Utc::now().to_rfc3339(),
            ],
        )
        .unwrap();
        drop(conn);

        let store = SqliteEventStore::open(&path).unwrap();
        let events = futures_executor::block_on(store.list(EventFilter {
            recipient: Some("claude".into()),
            ..EventFilter::default()
        }))
        .unwrap();
        let tag_events = futures_executor::block_on(store.list(EventFilter {
            tag: Some("legacy".into()),
            ..EventFilter::default()
        }))
        .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].content, "legacy");
        assert_eq!(tag_events.len(), 1);
    }
}

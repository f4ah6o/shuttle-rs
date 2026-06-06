use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use shuttle_core::{Event, EventFilter, EventStore, EventType, Result, ShuttleError};
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
                agent TEXT NOT NULL,
                session_id TEXT NOT NULL,
                title TEXT,
                content TEXT NOT NULL,
                tags TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_events_type_created ON events(event_type, created_at);
            CREATE INDEX IF NOT EXISTS idx_events_workspace_created ON events(workspace_id, created_at);
            CREATE INDEX IF NOT EXISTS idx_events_agent_created ON events(agent, created_at);
            "#,
        )
        .map_err(to_store_error)?;
        ensure_column(&conn, "repo_path", "TEXT")?;
        ensure_column(&conn, "git_remote", "TEXT")?;
        ensure_column(&conn, "bit_repo_id", "TEXT")?;
        Ok(())
    }
}

#[async_trait]
impl EventStore for SqliteEventStore {
    async fn append(&self, event: Event) -> Result<Event> {
        let conn = self
            .conn
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
        let tags = serde_json::to_string(&event.tags)
            .map_err(|err| ShuttleError::Serialization(err.to_string()))?;

        conn.execute(
            r#"
            INSERT INTO events (
                id, event_type, workspace_id, repo_id, repo_path, git_remote, bit_repo_id, branch, commit_hash,
                agent, session_id, title, content, tags, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            "#,
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
                &event.agent,
                &event.session_id,
                &event.title,
                &event.content,
                tags,
                event.created_at.to_rfc3339(),
            ],
        )
        .map_err(to_store_error)?;

        Ok(event)
    }

    async fn list(&self, filter: EventFilter) -> Result<Vec<Event>> {
        let conn = self
            .conn
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
        let limit = filter.limit.unwrap_or(50).min(500);
        let event_type = filter
            .event_type
            .map(|event_type| event_type.as_str().to_owned());
        let tag = filter.tag.as_ref().map(|tag| format!("%\"{tag}\"%"));
        let query = filter
            .query
            .as_ref()
            .map(|query| format!("%{}%", query.to_lowercase()));
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, event_type, workspace_id, repo_id, repo_path, git_remote, bit_repo_id, branch, commit_hash,
                       agent, session_id, title, content, tags, created_at
                FROM events
                WHERE (?1 IS NULL OR event_type = ?1)
                  AND (?2 IS NULL OR workspace_id = ?2)
                  AND (?3 IS NULL OR agent = ?3)
                  AND (?4 IS NULL OR tags LIKE ?4)
                  AND (
                    ?5 IS NULL
                    OR lower(coalesce(title, '')) LIKE ?5
                    OR lower(content) LIKE ?5
                    OR lower(tags) LIKE ?5
                  )
                ORDER BY created_at DESC
                LIMIT ?6
                "#,
            )
            .map_err(to_store_error)?;

        let rows = stmt
            .query_map(
                params![
                    event_type,
                    filter.workspace_id,
                    filter.agent,
                    tag,
                    query,
                    limit
                ],
                row_to_event,
            )
            .map_err(to_store_error)?;

        let mut events = Vec::new();
        for row in rows {
            let event = row.map_err(to_store_error)?;
            events.push(event);
        }

        Ok(events)
    }
}

fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let event_type: String = row.get(1)?;
    let tags: String = row.get(13)?;
    let created_at: String = row.get(14)?;

    let event_type = EventType::try_from(event_type.as_str()).map_err(to_sql_error)?;
    let tags = serde_json::from_str(&tags).map_err(to_sql_error)?;
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
        agent: row.get(9)?,
        session_id: row.get(10)?,
        title: row.get(11)?,
        content: row.get(12)?,
        tags,
        created_at,
    })
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
    use shuttle_core::{Event, NewEvent};

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
            agent: "codex".into(),
            session_id: "session".into(),
            title: None,
            content: "SQLite chosen for local-first storage".into(),
            tags: vec!["storage".into()],
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
}

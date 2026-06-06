use std::fs;
use std::path::Path;

use crate::core::{Event, EventFilter, EventStore, Result, ShuttleError};
use crate::store::SqliteEventStore;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshArchive {
    pub exported_at: DateTime<Utc>,
    pub event_count: usize,
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReport {
    pub imported: usize,
    pub skipped_duplicates: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReport {
    pub local_imported: usize,
    pub peer_imported: usize,
    pub skipped_duplicates: usize,
}

pub async fn export_archive(store: &SqliteEventStore) -> Result<MeshArchive> {
    let mut events = all_events(store).await?;
    events.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.id.cmp(&right.id))
    });
    Ok(MeshArchive {
        exported_at: Utc::now(),
        event_count: events.len(),
        events,
    })
}

pub fn write_archive(path: impl AsRef<Path>, archive: &MeshArchive) -> Result<()> {
    let contents = serde_json::to_string_pretty(archive)
        .map_err(|err| ShuttleError::Serialization(err.to_string()))?;
    fs::write(path, contents).map_err(|err| ShuttleError::Store(err.to_string()))
}

pub fn read_archive(path: impl AsRef<Path>) -> Result<MeshArchive> {
    let contents = fs::read_to_string(path).map_err(|err| ShuttleError::Store(err.to_string()))?;
    let archive = serde_json::from_str(&contents)
        .map_err(|err| ShuttleError::Serialization(err.to_string()))?;
    Ok(archive)
}

pub async fn import_archive(
    store: &SqliteEventStore,
    archive: MeshArchive,
) -> Result<ImportReport> {
    import_events(store, archive.events).await
}

pub async fn import_archive_into_workspace(
    store: &SqliteEventStore,
    archive: MeshArchive,
    target_workspace_id: &str,
) -> Result<ImportReport> {
    import_events_into_workspace(store, archive.events, Some(target_workspace_id)).await
}

pub async fn sync_bidirectional(
    local: &SqliteEventStore,
    peer: &SqliteEventStore,
) -> Result<SyncReport> {
    let local_events = all_events(local).await?;
    let peer_events = all_events(peer).await?;
    let local_report = import_events(local, peer_events).await?;
    let peer_report = import_events(peer, local_events).await?;

    Ok(SyncReport {
        local_imported: local_report.imported,
        peer_imported: peer_report.imported,
        skipped_duplicates: local_report.skipped_duplicates + peer_report.skipped_duplicates,
    })
}

pub async fn sync_bidirectional_into_workspaces(
    local: &SqliteEventStore,
    local_workspace_id: &str,
    peer: &SqliteEventStore,
    peer_workspace_id: Option<&str>,
) -> Result<SyncReport> {
    let local_events = all_events(local).await?;
    let peer_events = all_events(peer).await?;
    let local_report =
        import_events_into_workspace(local, peer_events, Some(local_workspace_id)).await?;
    let peer_report = import_events_into_workspace(peer, local_events, peer_workspace_id).await?;

    Ok(SyncReport {
        local_imported: local_report.imported,
        peer_imported: peer_report.imported,
        skipped_duplicates: local_report.skipped_duplicates + peer_report.skipped_duplicates,
    })
}

async fn all_events(store: &SqliteEventStore) -> Result<Vec<Event>> {
    store
        .list(EventFilter {
            limit: Some(u32::MAX),
            ..EventFilter::default()
        })
        .await
}

async fn import_events(store: &SqliteEventStore, events: Vec<Event>) -> Result<ImportReport> {
    import_events_into_workspace(store, events, None).await
}

async fn import_events_into_workspace(
    store: &SqliteEventStore,
    events: Vec<Event>,
    target_workspace_id: Option<&str>,
) -> Result<ImportReport> {
    let mut report = ImportReport::default();
    for event in events {
        let event = if let Some(workspace_id) = target_workspace_id {
            event_for_workspace(event, workspace_id)
        } else {
            event
        };
        if store.append_if_absent(event)? {
            report.imported += 1;
        } else {
            report.skipped_duplicates += 1;
        }
    }
    Ok(report)
}

fn event_for_workspace(mut event: Event, workspace_id: &str) -> Event {
    if event.workspace_id != workspace_id {
        if let Some(metadata) = event.metadata_json.as_object_mut() {
            metadata.insert(
                "mesh_source_workspace_id".to_owned(),
                serde_json::json!(event.workspace_id),
            );
        }
        event.workspace_id = workspace_id.to_owned();
    }
    event
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{EventStore, EventType};

    #[test]
    fn imports_are_idempotent_and_preserve_event_ids() {
        let source_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let source = SqliteEventStore::open(source_dir.path().join("source.db")).unwrap();
        let target = SqliteEventStore::open(target_dir.path().join("target.db")).unwrap();
        let event = crate::message::new_message(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "claude".into(),
            "review this branch".into(),
        );
        let event_id = event.id;
        futures_executor::block_on(source.append(event)).unwrap();

        let archive = futures_executor::block_on(export_archive(&source)).unwrap();
        let first = futures_executor::block_on(import_archive(&target, archive.clone())).unwrap();
        let second = futures_executor::block_on(import_archive(&target, archive)).unwrap();
        let events = futures_executor::block_on(target.list(EventFilter {
            event_type: Some(EventType::Message),
            ..EventFilter::default()
        }))
        .unwrap();

        assert_eq!(first.imported, 1);
        assert_eq!(first.skipped_duplicates, 0);
        assert_eq!(second.imported, 0);
        assert_eq!(second.skipped_duplicates, 1);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, event_id);
    }

    #[test]
    fn workspace_import_keeps_ids_but_makes_events_visible_locally() {
        let source_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let source = SqliteEventStore::open(source_dir.path().join("source.db")).unwrap();
        let target = SqliteEventStore::open(target_dir.path().join("target.db")).unwrap();
        let event = crate::memory::new_memory(
            "source-workspace".into(),
            "codex".into(),
            "session".into(),
            "shared memory".into(),
        );
        let event_id = event.id;
        futures_executor::block_on(source.append(event)).unwrap();

        let archive = futures_executor::block_on(export_archive(&source)).unwrap();
        let report = futures_executor::block_on(import_archive_into_workspace(
            &target,
            archive,
            "target-workspace",
        ))
        .unwrap();
        let events = futures_executor::block_on(target.list(EventFilter {
            workspace_id: Some("target-workspace".into()),
            ..EventFilter::default()
        }))
        .unwrap();

        assert_eq!(report.imported, 1);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, event_id);
        assert_eq!(events[0].workspace_id, "target-workspace");
        assert_eq!(
            events[0].metadata_json["mesh_source_workspace_id"],
            "source-workspace"
        );
    }

    #[test]
    fn bidirectional_sync_resumes_after_each_peer_records_events() {
        let local_dir = tempfile::tempdir().unwrap();
        let peer_dir = tempfile::tempdir().unwrap();
        let local = SqliteEventStore::open(local_dir.path().join("local.db")).unwrap();
        let peer = SqliteEventStore::open(peer_dir.path().join("peer.db")).unwrap();

        let memory = crate::memory::new_memory(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "SQLite is the local store".into(),
        );
        let task = crate::task::new_task(
            "workspace".into(),
            "claude".into(),
            "session".into(),
            "finish mesh sync".into(),
        );
        futures_executor::block_on(local.append(memory)).unwrap();
        futures_executor::block_on(peer.append(task)).unwrap();

        let first = futures_executor::block_on(sync_bidirectional(&local, &peer)).unwrap();
        assert_eq!(first.local_imported, 1);
        assert_eq!(first.peer_imported, 1);

        let handoff = crate::task::new_handoff(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "claude".into(),
            "continue after reconnect".into(),
        );
        let message = crate::message::new_message(
            "workspace".into(),
            "claude".into(),
            "session".into(),
            "codex".into(),
            "synced".into(),
        );
        futures_executor::block_on(local.append(handoff)).unwrap();
        futures_executor::block_on(peer.append(message)).unwrap();

        let second = futures_executor::block_on(sync_bidirectional(&local, &peer)).unwrap();
        assert_eq!(second.local_imported, 1);
        assert_eq!(second.peer_imported, 1);
        assert!(second.skipped_duplicates >= 4);

        for store in [&local, &peer] {
            let events = futures_executor::block_on(store.list(EventFilter {
                limit: Some(u32::MAX),
                ..EventFilter::default()
            }))
            .unwrap();
            assert_eq!(events.len(), 4);
            assert!(events
                .iter()
                .any(|event| event.event_type == EventType::Memory));
            assert!(events
                .iter()
                .any(|event| event.event_type == EventType::Task));
            assert!(events
                .iter()
                .any(|event| event.event_type == EventType::Handoff));
            assert!(events
                .iter()
                .any(|event| event.event_type == EventType::Message));
        }
    }

    #[test]
    fn archives_round_trip_through_json_files() {
        let source_dir = tempfile::tempdir().unwrap();
        let file_dir = tempfile::tempdir().unwrap();
        let source = SqliteEventStore::open(source_dir.path().join("source.db")).unwrap();
        let event = crate::memory::new_memory(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "portable archive".into(),
        );
        futures_executor::block_on(source.append(event)).unwrap();

        let archive = futures_executor::block_on(export_archive(&source)).unwrap();
        let path = file_dir.path().join("mesh.json");
        write_archive(&path, &archive).unwrap();
        let read_back = read_archive(&path).unwrap();

        assert_eq!(read_back.event_count, 1);
        assert_eq!(read_back.events[0].content, "portable archive");
    }
}

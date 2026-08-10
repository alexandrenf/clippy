use super::files::{self, FileManifest, DEFAULT_CHUNK_SIZE};
use super::model::{
    Dot, EntityKind, EntityState, Mutation, Operation, SyncPayload, VersionVector,
    SYNC_SCHEMA_VERSION,
};
use crate::db;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const MAX_REMOTE_OPS: usize = 2_000;
const MAX_CONTEXT_ACTORS: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Shadow {
    fields: BTreeMap<String, Value>,
    content: Option<String>,
}

struct SnapshotRecord {
    kind: EntityKind,
    sync_id: String,
    shadow: Shadow,
}

pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    migrate_operations_primary_key(conn)?;
    ensure_column(conn, "sections", "sync_id", "TEXT")?;
    ensure_column(conn, "sections", "sync_sort_index", "REAL")?;
    ensure_column(conn, "items", "sync_id", "TEXT")?;
    ensure_column(conn, "attachments", "sync_id", "TEXT")?;
    conn.execute_batch(
        "UPDATE sections SET sync_id = lower(hex(randomblob(16))) WHERE sync_id IS NULL;
         UPDATE items SET sync_id = lower(hex(randomblob(16))) WHERE sync_id IS NULL;
         UPDATE attachments SET sync_id = lower(hex(randomblob(16))) WHERE sync_id IS NULL;
         CREATE UNIQUE INDEX IF NOT EXISTS idx_sections_sync_id ON sections(sync_id);
         CREATE UNIQUE INDEX IF NOT EXISTS idx_items_sync_id ON items(sync_id);
         CREATE UNIQUE INDEX IF NOT EXISTS idx_attachments_sync_id ON attachments(sync_id);
         CREATE TRIGGER IF NOT EXISTS sync_sections_assign_id
           AFTER INSERT ON sections WHEN NEW.sync_id IS NULL
           BEGIN
             UPDATE sections SET sync_id=lower(hex(randomblob(16))) WHERE id=NEW.id;
           END;
         CREATE TRIGGER IF NOT EXISTS sync_items_assign_id
           AFTER INSERT ON items WHEN NEW.sync_id IS NULL
           BEGIN
             UPDATE items SET sync_id=lower(hex(randomblob(16))) WHERE id=NEW.id;
           END;
         CREATE TRIGGER IF NOT EXISTS sync_attachments_assign_id
           AFTER INSERT ON attachments WHEN NEW.sync_id IS NULL
           BEGIN
             UPDATE attachments SET sync_id=lower(hex(randomblob(16))) WHERE id=NEW.id;
           END;
         CREATE TABLE IF NOT EXISTS sync_local_clock(
           workspace_id TEXT NOT NULL,
           actor_id TEXT NOT NULL,
           counter INTEGER NOT NULL,
           PRIMARY KEY(workspace_id, actor_id)
         );
         CREATE TABLE IF NOT EXISTS sync_shadow(
           workspace_id TEXT NOT NULL,
           entity_kind TEXT NOT NULL,
           entity_id TEXT NOT NULL,
           snapshot_json TEXT NOT NULL,
           PRIMARY KEY(workspace_id, entity_kind, entity_id)
         );
         CREATE TABLE IF NOT EXISTS sync_entities(
           workspace_id TEXT NOT NULL,
           entity_kind TEXT NOT NULL,
           entity_id TEXT NOT NULL,
           state_json TEXT NOT NULL,
           PRIMARY KEY(workspace_id, entity_kind, entity_id)
         );
         CREATE TABLE IF NOT EXISTS sync_peers(
           workspace_id TEXT NOT NULL,
           actor_id TEXT NOT NULL,
           last_seen INTEGER NOT NULL,
           PRIMARY KEY(workspace_id, actor_id)
         );
         CREATE TABLE IF NOT EXISTS sync_peer_frontiers(
           workspace_id TEXT NOT NULL,
           peer_actor_id TEXT NOT NULL,
           operation_actor_id TEXT NOT NULL,
           counter INTEGER NOT NULL,
           PRIMARY KEY(workspace_id,peer_actor_id,operation_actor_id)
         );
         CREATE TABLE IF NOT EXISTS sync_attachment_manifests(
           attachment_id TEXT PRIMARY KEY,
           stored_path TEXT NOT NULL,
           size INTEGER NOT NULL,
           modified_at INTEGER NOT NULL,
           manifest_json TEXT NOT NULL
         );",
    )?;
    ensure_column(
        conn,
        "sync_file_chunks",
        "offset",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    Ok(())
}

fn migrate_operations_primary_key(conn: &Connection) -> rusqlite::Result<()> {
    let mut statement = conn.prepare("PRAGMA table_info(sync_operations)")?;
    let columns = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(5)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut primary: Vec<_> = columns
        .into_iter()
        .filter(|(_, order)| *order > 0)
        .collect();
    primary.sort_by_key(|(_, order)| *order);
    let names: Vec<_> = primary.into_iter().map(|(name, _)| name).collect();
    if names.is_empty() || names == ["workspace_id", "actor_id", "counter"] {
        return Ok(());
    }
    conn.execute_batch(
        "ALTER TABLE sync_operations RENAME TO sync_operations_legacy;
         CREATE TABLE sync_operations(
           actor_id TEXT NOT NULL,
           counter INTEGER NOT NULL CHECK(counter > 0),
           workspace_id TEXT NOT NULL,
           entity_kind TEXT NOT NULL,
           entity_id TEXT NOT NULL,
           operation_json TEXT NOT NULL,
           received_at INTEGER NOT NULL,
           PRIMARY KEY(workspace_id,actor_id,counter)
         );
         INSERT OR IGNORE INTO sync_operations
           SELECT actor_id,counter,workspace_id,entity_kind,entity_id,operation_json,received_at
           FROM sync_operations_legacy;
         DROP TABLE sync_operations_legacy;
         CREATE INDEX IF NOT EXISTS idx_sync_operations_workspace
           ON sync_operations(workspace_id,actor_id,counter);",
    )
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> rusqlite::Result<()> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !names.iter().any(|name| name == column) {
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))?;
    }
    Ok(())
}

pub fn ensure_identity(conn: &Connection) -> rusqlite::Result<String> {
    if let Some(actor) = db::get_setting(conn, "sync_device_actor") {
        if Uuid::parse_str(&actor).is_ok() {
            return Ok(actor);
        }
    }
    let actor = Uuid::new_v4().to_string();
    db::set_setting(conn, "sync_device_actor", &actor)?;
    Ok(actor)
}

pub fn scan_local(
    conn: &mut Connection,
    workspace_id: &str,
    actor_id: &str,
) -> Result<usize, StoreError> {
    let records = snapshot_records(conn)?;
    let tx = conn.transaction()?;
    let mut seen = HashSet::new();
    let mut emitted = 0;
    for record in records {
        let kind = kind_name(&record.kind);
        seen.insert((kind.to_string(), record.sync_id.clone()));
        let previous: Option<String> = tx
            .query_row(
                "SELECT snapshot_json FROM sync_shadow
                 WHERE workspace_id=?1 AND entity_kind=?2 AND entity_id=?3",
                params![workspace_id, kind, record.sync_id],
                |row| row.get(0),
            )
            .optional()?;
        let previous: Option<Shadow> = previous.as_deref().map(serde_json::from_str).transpose()?;
        emitted += emit_diff(
            &tx,
            workspace_id,
            actor_id,
            &record.kind,
            &record.sync_id,
            previous.as_ref(),
            &record.shadow,
        )?;
        tx.execute(
            "INSERT INTO sync_shadow(workspace_id,entity_kind,entity_id,snapshot_json)
             VALUES(?1,?2,?3,?4)
             ON CONFLICT(workspace_id,entity_kind,entity_id)
             DO UPDATE SET snapshot_json=excluded.snapshot_json",
            params![
                workspace_id,
                kind,
                record.sync_id,
                serde_json::to_string(&record.shadow)?
            ],
        )?;
    }

    let mut statement =
        tx.prepare("SELECT entity_kind,entity_id FROM sync_shadow WHERE workspace_id=?1")?;
    let previous = statement
        .query_map([workspace_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for (kind, entity_id) in previous {
        if seen.contains(&(kind.clone(), entity_id.clone())) {
            continue;
        }
        let entity_kind = parse_kind(&kind)?;
        let operation = next_operation(
            &tx,
            workspace_id,
            actor_id,
            entity_kind,
            entity_id.clone(),
            Mutation::Delete,
        )?;
        insert_operation(&tx, &operation)?;
        tx.execute(
            "DELETE FROM sync_shadow WHERE workspace_id=?1 AND entity_kind=?2 AND entity_id=?3",
            params![workspace_id, kind, entity_id],
        )?;
        emitted += 1;
    }
    tx.commit()?;
    Ok(emitted)
}

fn snapshot_records(conn: &Connection) -> Result<Vec<SnapshotRecord>, StoreError> {
    let mut records = Vec::new();
    let mut sections = conn.prepare(
        "SELECT sync_id,name,COALESCE(sync_sort_index,CAST(id AS REAL))
         FROM sections ORDER BY COALESCE(sync_sort_index,CAST(id AS REAL)),id",
    )?;
    for row in sections.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, f64>(2)?,
        ))
    })? {
        let (sync_id, name, sort_index) = row?;
        records.push(SnapshotRecord {
            kind: EntityKind::Section,
            sync_id,
            shadow: Shadow {
                fields: BTreeMap::from([
                    ("name".into(), Value::String(name)),
                    (
                        "sortIndex".into(),
                        Value::Number(
                            serde_json::Number::from_f64(sort_index)
                                .ok_or(StoreError::InvalidPayload)?,
                        ),
                    ),
                ]),
                content: None,
            },
        });
    }

    let mut items = conn.prepare(
        "SELECT items.sync_id,sections.sync_id,items.content,items.done,items.created_at
         FROM items LEFT JOIN sections ON sections.id=items.section_id ORDER BY items.id",
    )?;
    for row in items.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })? {
        let (sync_id, section_sync_id, content, done, created_at) = row?;
        records.push(SnapshotRecord {
            kind: EntityKind::Item,
            sync_id,
            shadow: Shadow {
                fields: BTreeMap::from([
                    (
                        "sectionId".into(),
                        section_sync_id.map(Value::String).unwrap_or(Value::Null),
                    ),
                    ("done".into(), Value::Bool(done != 0)),
                    ("createdAt".into(), Value::Number(created_at.into())),
                ]),
                content: Some(content),
            },
        });
    }

    let mut attachments = conn.prepare(
        "SELECT attachments.sync_id,items.sync_id,attachments.name,
                attachments.media_type,attachments.size,attachments.stored_path
         FROM attachments JOIN items ON items.id=attachments.item_id ORDER BY attachments.id",
    )?;
    for row in attachments.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, String>(5)?,
        ))
    })? {
        let (sync_id, item_sync_id, name, media_type, size, stored_path) = row?;
        let path = PathBuf::from(&stored_path);
        let file_manifest = cached_manifest(conn, &sync_id, &path, size)?;
        records.push(SnapshotRecord {
            kind: EntityKind::Attachment,
            sync_id,
            shadow: Shadow {
                fields: BTreeMap::from([
                    ("itemId".into(), Value::String(item_sync_id)),
                    ("name".into(), Value::String(name)),
                    ("mediaType".into(), Value::String(media_type)),
                    ("size".into(), Value::Number(size.into())),
                    ("manifest".into(), serde_json::to_value(file_manifest)?),
                ]),
                content: None,
            },
        });
    }
    Ok(records)
}

fn cached_manifest(
    conn: &Connection,
    attachment_id: &str,
    path: &Path,
    size: i64,
) -> Result<FileManifest, StoreError> {
    let modified_at = fs::metadata(path)?
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let cached: Option<String> = conn
        .query_row(
            "SELECT manifest_json FROM sync_attachment_manifests
             WHERE attachment_id=?1 AND stored_path=?2 AND size=?3 AND modified_at=?4",
            params![attachment_id, path.to_string_lossy(), size, modified_at],
            |row| row.get(0),
        )
        .optional()?;
    let manifest = match cached {
        Some(json) => serde_json::from_str(&json)?,
        None => {
            let manifest = files::manifest(path, DEFAULT_CHUNK_SIZE)?;
            conn.execute(
                "INSERT INTO sync_attachment_manifests(
                   attachment_id,stored_path,size,modified_at,manifest_json
                 ) VALUES(?1,?2,?3,?4,?5)
                 ON CONFLICT(attachment_id) DO UPDATE SET stored_path=excluded.stored_path,
                   size=excluded.size,modified_at=excluded.modified_at,
                   manifest_json=excluded.manifest_json",
                params![
                    attachment_id,
                    path.to_string_lossy(),
                    size,
                    modified_at,
                    serde_json::to_string(&manifest)?
                ],
            )?;
            manifest
        }
    };
    register_chunks(conn, path, &manifest)?;
    Ok(manifest)
}

fn register_chunks(
    conn: &Connection,
    path: &Path,
    manifest: &FileManifest,
) -> rusqlite::Result<()> {
    let mut offset = 0_i64;
    for chunk in &manifest.chunks {
        conn.execute(
            "INSERT INTO sync_file_chunks(sha256,size,stored_path,verified_at,offset)
             VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(sha256) DO UPDATE SET size=excluded.size,
               stored_path=excluded.stored_path,verified_at=excluded.verified_at,offset=excluded.offset",
            params![
                chunk.sha256,
                chunk.size as i64,
                path.to_string_lossy(),
                db::now_ms(),
                offset
            ],
        )?;
        offset += chunk.size as i64;
    }
    Ok(())
}

fn emit_diff(
    tx: &Transaction<'_>,
    workspace_id: &str,
    actor_id: &str,
    kind: &EntityKind,
    entity_id: &str,
    previous: Option<&Shadow>,
    current: &Shadow,
) -> Result<usize, StoreError> {
    let mut emitted = 0;
    for (field, value) in &current.fields {
        if previous.and_then(|shadow| shadow.fields.get(field)) == Some(value) {
            continue;
        }
        let operation = next_operation(
            tx,
            workspace_id,
            actor_id,
            kind.clone(),
            entity_id.to_string(),
            Mutation::SetMetadata {
                field: field.clone(),
                value: value.clone(),
            },
        )?;
        insert_operation(tx, &operation)?;
        emitted += 1;
    }
    if let Some(content) = &current.content {
        if previous.and_then(|shadow| shadow.content.as_ref()) != Some(content) {
            let operation = next_operation(
                tx,
                workspace_id,
                actor_id,
                kind.clone(),
                entity_id.to_string(),
                Mutation::SetContent {
                    context: load_frontier(tx, workspace_id)?,
                    value: content.clone(),
                },
            )?;
            insert_operation(tx, &operation)?;
            emitted += 1;
        }
    }
    Ok(emitted)
}

fn next_operation(
    tx: &Transaction<'_>,
    workspace_id: &str,
    actor_id: &str,
    entity_kind: EntityKind,
    entity_id: String,
    mutation: Mutation,
) -> Result<Operation, StoreError> {
    tx.execute(
        "INSERT INTO sync_local_clock(workspace_id,actor_id,counter) VALUES(?1,?2,1)
         ON CONFLICT(workspace_id,actor_id) DO UPDATE SET counter=counter+1",
        params![workspace_id, actor_id],
    )?;
    let counter = tx.query_row(
        "SELECT counter FROM sync_local_clock WHERE workspace_id=?1 AND actor_id=?2",
        params![workspace_id, actor_id],
        |row| row.get::<_, u64>(0),
    )?;
    Ok(Operation {
        schema_version: SYNC_SCHEMA_VERSION,
        workspace_id: workspace_id.into(),
        entity_kind,
        entity_id,
        dot: Dot {
            actor_id: actor_id.into(),
            counter,
        },
        mutation,
    })
}

fn insert_operation(tx: &Transaction<'_>, operation: &Operation) -> Result<bool, StoreError> {
    let encoded = serde_json::to_string(operation)?;
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO sync_operations(
           actor_id,counter,workspace_id,entity_kind,entity_id,operation_json,received_at
         ) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            operation.dot.actor_id,
            operation.dot.counter,
            operation.workspace_id,
            kind_name(&operation.entity_kind),
            operation.entity_id,
            encoded,
            db::now_ms()
        ],
    )? > 0;
    if !inserted {
        let existing: String = tx.query_row(
            "SELECT operation_json FROM sync_operations
             WHERE workspace_id=?1 AND actor_id=?2 AND counter=?3",
            params![
                operation.workspace_id,
                operation.dot.actor_id,
                operation.dot.counter
            ],
            |row| row.get(0),
        )?;
        if existing != encoded {
            return Err(StoreError::InvalidPayload);
        }
    }
    if inserted {
        tx.execute(
            "INSERT INTO sync_frontier(workspace_id,actor_id,counter) VALUES(?1,?2,?3)
             ON CONFLICT(workspace_id,actor_id) DO UPDATE SET counter=max(counter,excluded.counter)",
            params![
                operation.workspace_id,
                operation.dot.actor_id,
                operation.dot.counter
            ],
        )?;
        update_entity_state(tx, operation)?;
    }
    Ok(inserted)
}

fn update_entity_state(
    tx: &Transaction<'_>,
    operation: &Operation,
) -> Result<EntityState, StoreError> {
    let kind = kind_name(&operation.entity_kind);
    let previous: Option<String> = tx
        .query_row(
            "SELECT state_json FROM sync_entities
             WHERE workspace_id=?1 AND entity_kind=?2 AND entity_id=?3",
            params![operation.workspace_id, kind, operation.entity_id],
            |row| row.get(0),
        )
        .optional()?;
    let mut state: EntityState = previous
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_default();
    state.apply(operation);
    tx.execute(
        "INSERT INTO sync_entities(workspace_id,entity_kind,entity_id,state_json)
         VALUES(?1,?2,?3,?4)
         ON CONFLICT(workspace_id,entity_kind,entity_id)
         DO UPDATE SET state_json=excluded.state_json",
        params![
            operation.workspace_id,
            kind,
            operation.entity_id,
            serde_json::to_string(&state)?
        ],
    )?;
    Ok(state)
}

pub fn exchange(
    conn: &mut Connection,
    workspace_id: &str,
    peer_actor: &str,
    incoming: SyncPayload,
) -> Result<SyncPayload, StoreError> {
    if incoming.schema_version != SYNC_SCHEMA_VERSION || incoming.workspace_id != workspace_id {
        return Err(StoreError::InvalidPayload);
    }
    if incoming.operations.len() > MAX_REMOTE_OPS
        || Uuid::parse_str(peer_actor).is_err()
        || incoming.frontier.0.len() > MAX_CONTEXT_ACTORS
        || incoming
            .frontier
            .0
            .iter()
            .any(|(actor, counter)| Uuid::parse_str(actor).is_err() || *counter == 0)
        || incoming
            .operations
            .iter()
            .any(|operation| operation.dot.actor_id != peer_actor)
    {
        return Err(StoreError::InvalidPayload);
    }
    let tx = conn.transaction()?;
    for operation in &incoming.operations {
        validate_operation(operation, workspace_id)?;
    }
    validate_incoming_frontier_and_sequence(
        &tx,
        workspace_id,
        peer_actor,
        &incoming.frontier,
        &incoming.operations,
    )?;
    let mut changed = HashSet::new();
    let mut peer_frontier = incoming.frontier.clone();
    for operation in incoming.operations {
        peer_frontier.observe(&operation.dot);
        if insert_operation(&tx, &operation)? {
            changed.insert((operation.entity_kind.clone(), operation.entity_id.clone()));
        }
    }
    let mut changed: Vec<_> = changed.into_iter().collect();
    changed.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    for (kind, entity_id) in changed {
        project_entity(&tx, workspace_id, &kind, &entity_id)?;
    }
    tx.execute(
        "INSERT INTO sync_peers(workspace_id,actor_id,last_seen) VALUES(?1,?2,?3)
         ON CONFLICT(workspace_id,actor_id) DO UPDATE SET last_seen=excluded.last_seen",
        params![workspace_id, peer_actor, db::now_ms()],
    )?;
    tx.execute(
        "DELETE FROM sync_peer_frontiers WHERE workspace_id=?1 AND peer_actor_id=?2",
        params![workspace_id, peer_actor],
    )?;
    for (operation_actor, counter) in &peer_frontier.0 {
        tx.execute(
            "INSERT INTO sync_peer_frontiers(
               workspace_id,peer_actor_id,operation_actor_id,counter
             ) VALUES(?1,?2,?3,?4)",
            params![workspace_id, peer_actor, operation_actor, counter],
        )?;
    }
    let response_ops = load_delta(&tx, workspace_id, &peer_frontier)?;
    // The response frontier is a delivery acknowledgement, not the server's
    // complete frontier. It must only include dots the recipient already had
    // plus dots present in this bounded page. Otherwise a >2,000-op delta
    // would skip every operation after page one.
    let mut delivered_frontier = peer_frontier;
    for operation in &response_ops {
        delivered_frontier.observe(&operation.dot);
    }
    tx.commit()?;
    Ok(SyncPayload {
        schema_version: SYNC_SCHEMA_VERSION,
        workspace_id: workspace_id.into(),
        frontier: delivered_frontier,
        operations: response_ops,
    })
}

fn validate_incoming_frontier_and_sequence(
    tx: &Transaction<'_>,
    workspace_id: &str,
    peer_actor: &str,
    claimed: &VersionVector,
    operations: &[Operation],
) -> Result<(), StoreError> {
    let known = load_frontier(tx, workspace_id)?;
    let current = known.0.get(peer_actor).copied().unwrap_or(0);
    let mut counters = operations
        .iter()
        .map(|operation| operation.dot.counter)
        .collect::<Vec<_>>();
    counters.sort_unstable();
    counters.dedup();
    let mut expected = current.saturating_add(1);
    for counter in counters.into_iter().filter(|counter| *counter > current) {
        if counter != expected {
            return Err(StoreError::InvalidPayload);
        }
        expected = expected.checked_add(1).ok_or(StoreError::InvalidPayload)?;
    }

    let mut deliverable = known;
    for operation in operations {
        deliverable.observe(&operation.dot);
    }
    if claimed
        .0
        .iter()
        .any(|(actor, counter)| deliverable.0.get(actor).copied().unwrap_or(0) < *counter)
    {
        return Err(StoreError::InvalidPayload);
    }
    Ok(())
}

fn validate_operation(operation: &Operation, workspace_id: &str) -> Result<(), StoreError> {
    if operation.schema_version != SYNC_SCHEMA_VERSION
        || operation.workspace_id != workspace_id
        || operation.dot.counter == 0
        || Uuid::parse_str(&operation.dot.actor_id).is_err()
        || Uuid::parse_str(&operation.entity_id).is_err()
    {
        return Err(StoreError::InvalidPayload);
    }
    match (&operation.entity_kind, &operation.mutation) {
        (EntityKind::Section, Mutation::SetMetadata { field, value }) => match field.as_str() {
            "name"
                if value.as_str().is_some_and(|name| {
                    !name.is_empty() && name.chars().count() <= db::MAX_SECTION_CHARS
                }) => {}
            "sortIndex" if value.as_f64().is_some_and(f64::is_finite) => {}
            _ => return Err(StoreError::InvalidPayload),
        },
        (EntityKind::Item, Mutation::SetMetadata { field, value }) => match field.as_str() {
            "sectionId"
                if value.is_null()
                    || value.as_str().is_some_and(|id| Uuid::parse_str(id).is_ok()) => {}
            "done" if value.is_boolean() => {}
            "createdAt" if value.as_i64().is_some_and(|timestamp| timestamp >= 0) => {}
            _ => return Err(StoreError::InvalidPayload),
        },
        (EntityKind::Attachment, Mutation::SetMetadata { field, value }) => match field.as_str() {
            "itemId" if value.as_str().is_some_and(|id| Uuid::parse_str(id).is_ok()) => {}
            "name" | "mediaType"
                if value
                    .as_str()
                    .is_some_and(|text| !text.is_empty() && text.len() <= 1024) => {}
            "size"
                if value
                    .as_u64()
                    .is_some_and(|size| size <= db::MAX_ATTACHMENT_BYTES) => {}
            "manifest"
                if serde_json::from_value::<FileManifest>(value.clone())
                    .is_ok_and(|manifest| validate_manifest(&manifest)) => {}
            _ => return Err(StoreError::InvalidPayload),
        },
        (EntityKind::Item, Mutation::SetContent { context, value })
        | (EntityKind::Item, Mutation::ResolveContent { context, value }) => {
            if value.chars().count() > db::MAX_ITEM_CHARS
                || context.0.len() > MAX_CONTEXT_ACTORS
                || context
                    .0
                    .iter()
                    .any(|(actor, counter)| Uuid::parse_str(actor).is_err() || *counter == 0)
            {
                return Err(StoreError::InvalidPayload);
            }
        }
        (_, Mutation::Delete) => {}
        _ => return Err(StoreError::InvalidPayload),
    }
    let encoded = serde_json::to_vec(operation)?;
    if encoded.len() > db::MAX_ITEM_CHARS * 4 + 32_768 {
        return Err(StoreError::InvalidPayload);
    }
    Ok(())
}

fn load_delta(
    conn: &Connection,
    workspace_id: &str,
    frontier: &VersionVector,
) -> Result<Vec<Operation>, StoreError> {
    let mut statement = conn.prepare(
        "SELECT operation_json FROM sync_operations
         WHERE workspace_id=?1 ORDER BY received_at,actor_id,counter",
    )?;
    let operations = statement
        .query_map([workspace_id], |row| row.get::<_, String>(0))?
        .filter_map(|result| match result {
            Ok(json) => match serde_json::from_str::<Operation>(&json) {
                Ok(operation) if !frontier.observes(&operation.dot) => Some(Ok(operation)),
                Ok(_) => None,
                Err(error) => Some(Err(StoreError::Json(error))),
            },
            Err(error) => Some(Err(StoreError::Database(error))),
        })
        .take(MAX_REMOTE_OPS)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(operations)
}

fn load_frontier(conn: &Connection, workspace_id: &str) -> Result<VersionVector, StoreError> {
    let mut statement =
        conn.prepare("SELECT actor_id,counter FROM sync_frontier WHERE workspace_id=?1")?;
    let values = statement
        .query_map([workspace_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
        })?
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
    Ok(VersionVector(values))
}

fn project_entity(
    tx: &Transaction<'_>,
    workspace_id: &str,
    kind: &EntityKind,
    entity_id: &str,
) -> Result<(), StoreError> {
    let json: String = tx.query_row(
        "SELECT state_json FROM sync_entities
         WHERE workspace_id=?1 AND entity_kind=?2 AND entity_id=?3",
        params![workspace_id, kind_name(kind), entity_id],
        |row| row.get(0),
    )?;
    let state: EntityState = serde_json::from_str(&json)?;
    match kind {
        EntityKind::Section => project_section(tx, workspace_id, entity_id, &state)?,
        EntityKind::Item => project_item(tx, workspace_id, entity_id, &state)?,
        EntityKind::Attachment => project_attachment(tx, workspace_id, entity_id, &state)?,
    }
    Ok(())
}

fn project_section(
    tx: &Transaction<'_>,
    workspace_id: &str,
    entity_id: &str,
    state: &EntityState,
) -> Result<(), StoreError> {
    if state.is_deleted() {
        tx.execute("DELETE FROM sections WHERE sync_id=?1", [entity_id])?;
        tx.execute(
            "DELETE FROM sync_shadow WHERE workspace_id=?1 AND entity_kind='section' AND entity_id=?2",
            params![workspace_id, entity_id],
        )?;
        return Ok(());
    }
    let Some(name) = state.value("name").and_then(Value::as_str) else {
        return Ok(());
    };
    let sort_index = state
        .value("sortIndex")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| db::now_ms() as f64);
    tx.execute(
        "INSERT INTO sections(name,created_at,sync_id,sync_sort_index) VALUES(?1,?2,?3,?4)
         ON CONFLICT(sync_id) DO UPDATE SET
           name=excluded.name,sync_sort_index=excluded.sync_sort_index",
        params![name, db::now_ms(), entity_id, sort_index],
    )?;
    write_shadow(
        tx,
        workspace_id,
        kind_name(&EntityKind::Section),
        entity_id,
        Shadow {
            fields: BTreeMap::from([
                ("name".into(), Value::String(name.into())),
                (
                    "sortIndex".into(),
                    Value::Number(
                        serde_json::Number::from_f64(sort_index)
                            .ok_or(StoreError::InvalidPayload)?,
                    ),
                ),
            ]),
            content: None,
        },
    )?;

    // A referenced item may have arrived before its section. Reproject those
    // items whenever the dependency materializes, independent of wire order.
    let mut statement = tx.prepare(
        "SELECT entity_id,state_json FROM sync_entities
         WHERE workspace_id=?1 AND entity_kind='item'",
    )?;
    let dependents = statement
        .query_map([workspace_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for (item_id, json) in dependents {
        let item_state: EntityState = serde_json::from_str(&json)?;
        if item_state.value("sectionId").and_then(Value::as_str) == Some(entity_id) {
            project_item(tx, workspace_id, &item_id, &item_state)?;
        }
    }
    Ok(())
}

fn project_item(
    tx: &Transaction<'_>,
    workspace_id: &str,
    entity_id: &str,
    state: &EntityState,
) -> Result<(), StoreError> {
    if state.is_deleted() {
        tx.execute("DELETE FROM items WHERE sync_id=?1", [entity_id])?;
        tx.execute(
            "DELETE FROM sync_shadow WHERE workspace_id=?1 AND entity_kind='item' AND entity_id=?2",
            params![workspace_id, entity_id],
        )?;
        return Ok(());
    }
    let Some(content) = state.content.projected_value() else {
        return Ok(());
    };
    let section_sync_id = state.value("sectionId").and_then(Value::as_str);
    let section_id: Option<i64> = section_sync_id
        .map(|sync_id| {
            tx.query_row(
                "SELECT id FROM sections WHERE sync_id=?1",
                [sync_id],
                |row| row.get(0),
            )
            .optional()
        })
        .transpose()?
        .flatten();
    let done = state
        .value("done")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let created_at = state
        .value("createdAt")
        .and_then(Value::as_i64)
        .unwrap_or_else(db::now_ms);
    tx.execute(
        "INSERT INTO items(section_id,content,done,created_at,updated_at,sync_id)
         VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(sync_id) DO UPDATE SET section_id=excluded.section_id,
           content=excluded.content,done=excluded.done,updated_at=excluded.updated_at",
        params![
            section_id,
            content,
            done as i64,
            created_at,
            db::now_ms(),
            entity_id
        ],
    )?;
    if state.content.has_conflict() {
        tx.execute(
            "INSERT INTO sync_content_conflicts(workspace_id,entity_id,versions_json,updated_at)
             VALUES(?1,?2,?3,?4)
             ON CONFLICT(workspace_id,entity_id) DO UPDATE SET
               versions_json=excluded.versions_json,updated_at=excluded.updated_at",
            params![
                workspace_id,
                entity_id,
                serde_json::to_string(&state.content)?,
                db::now_ms()
            ],
        )?;
    } else {
        tx.execute(
            "DELETE FROM sync_content_conflicts WHERE workspace_id=?1 AND entity_id=?2",
            params![workspace_id, entity_id],
        )?;
    }
    write_shadow(
        tx,
        workspace_id,
        kind_name(&EntityKind::Item),
        entity_id,
        Shadow {
            fields: BTreeMap::from([
                (
                    "sectionId".into(),
                    section_sync_id
                        .map(|value| Value::String(value.into()))
                        .unwrap_or(Value::Null),
                ),
                ("done".into(), Value::Bool(done)),
                ("createdAt".into(), Value::Number(created_at.into())),
            ]),
            content: Some(content.into()),
        },
    )
}

fn project_attachment(
    _tx: &Transaction<'_>,
    _workspace_id: &str,
    _entity_id: &str,
    _state: &EntityState,
) -> Result<(), StoreError> {
    // The origin stores manifests immediately. Attachment rows are projected
    // only after the chunk assembler verifies and atomically installs the full
    // file, preventing a dangling path from reaching the current UI.
    Ok(())
}

/// Materializes attachment rows only when every addressed chunk is present and
/// the reconstructed file hash matches the authenticated manifest. This is
/// called after metadata exchange and after each chunk PUT; the all-present
/// preflight avoids repeatedly rebuilding a partial file.
pub fn project_ready_attachments(
    conn: &mut Connection,
    workspace_id: &str,
    attachments_dir: &Path,
) -> Result<usize, StoreError> {
    let states = {
        let mut statement = conn.prepare(
            "SELECT entity_id,state_json FROM sync_entities
             WHERE workspace_id=?1 AND entity_kind='attachment' ORDER BY entity_id",
        )?;
        let values = statement
            .query_map([workspace_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        values
    };
    fs::create_dir_all(attachments_dir)?;
    let canonical_dir = attachments_dir.canonicalize()?;
    let tx = conn.transaction()?;
    let mut projected = 0;
    let mut obsolete_files = Vec::new();

    for (entity_id, json) in states {
        let state: EntityState = serde_json::from_str(&json)?;
        if state.is_deleted() {
            if let Some(path) = tx
                .query_row(
                    "SELECT stored_path FROM attachments WHERE sync_id=?1",
                    [&entity_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            {
                obsolete_files.push(PathBuf::from(path));
            }
            tx.execute("DELETE FROM attachments WHERE sync_id=?1", [&entity_id])?;
            tx.execute(
                "DELETE FROM sync_shadow
                 WHERE workspace_id=?1 AND entity_kind='attachment' AND entity_id=?2",
                params![workspace_id, entity_id],
            )?;
            continue;
        }

        let Some(item_sync_id) = state.value("itemId").and_then(Value::as_str) else {
            continue;
        };
        let Some(name) = state.value("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(media_type) = state.value("mediaType").and_then(Value::as_str) else {
            continue;
        };
        let Some(size) = state.value("size").and_then(Value::as_u64) else {
            continue;
        };
        let Some(manifest_value) = state.value("manifest") else {
            continue;
        };
        let manifest: FileManifest = serde_json::from_value(manifest_value.clone())?;
        if !validate_manifest(&manifest) || manifest.size != size {
            return Err(StoreError::InvalidPayload);
        }
        let desired_shadow = Shadow {
            fields: BTreeMap::from([
                ("itemId".into(), Value::String(item_sync_id.into())),
                ("name".into(), Value::String(name.into())),
                ("mediaType".into(), Value::String(media_type.into())),
                ("size".into(), Value::Number((size as i64).into())),
                ("manifest".into(), serde_json::to_value(&manifest)?),
            ]),
            content: None,
        };
        let item_id: Option<i64> = tx
            .query_row(
                "SELECT id FROM items WHERE sync_id=?1",
                [item_sync_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(item_id) = item_id else { continue };

        let extension = Path::new(name)
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| {
                value
                    .chars()
                    .filter(char::is_ascii_alphanumeric)
                    .take(12)
                    .collect::<String>()
                    .to_ascii_lowercase()
            })
            .filter(|value| !value.is_empty());
        let target = match extension {
            Some(extension) => canonical_dir.join(format!("{entity_id}.{extension}")),
            None => canonical_dir.join(&entity_id),
        };
        let existing_shadow: Option<String> = tx
            .query_row(
                "SELECT snapshot_json FROM sync_shadow
                 WHERE workspace_id=?1 AND entity_kind='attachment' AND entity_id=?2",
                params![workspace_id, entity_id],
                |row| row.get(0),
            )
            .optional()?;
        let existing_row: Option<(String, u64)> = tx
            .query_row(
                "SELECT stored_path,size FROM attachments WHERE sync_id=?1",
                [&entity_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if existing_shadow
            .as_deref()
            .map(serde_json::from_str::<Shadow>)
            .transpose()?
            .as_ref()
            == Some(&desired_shadow)
            && existing_row.as_ref().is_some_and(|(path, stored_size)| {
                Path::new(path) == target
                    && *stored_size == size
                    && fs::metadata(path)
                        .is_ok_and(|metadata| metadata.is_file() && metadata.len() == size)
            })
        {
            continue;
        }

        let mut sources = Vec::with_capacity(manifest.chunks.len());
        let mut all_present = true;
        for descriptor in &manifest.chunks {
            match chunk_source(&tx, &descriptor.sha256)? {
                Some((path, offset, stored_size)) if stored_size as u64 == descriptor.size => {
                    sources.push((path, offset, stored_size));
                }
                _ => {
                    all_present = false;
                    break;
                }
            }
        }
        if !all_present {
            continue;
        }

        let mut temp = tempfile::NamedTempFile::new_in(&canonical_dir)?;
        let mut file_hasher = Sha256::new();
        let mut total = 0_u64;
        for (descriptor, (path, offset, stored_size)) in manifest.chunks.iter().zip(sources) {
            let mut source = File::open(path)?;
            source.seek(SeekFrom::Start(offset))?;
            let mut buffer = vec![0_u8; stored_size];
            source.read_exact(&mut buffer)?;
            if !files::verify_chunk(&descriptor.sha256, &buffer) {
                return Err(StoreError::InvalidPayload);
            }
            temp.write_all(&buffer)?;
            file_hasher.update(&buffer);
            total = total.saturating_add(buffer.len() as u64);
        }
        temp.flush()?;
        let file_hash = format!("{:x}", file_hasher.finalize());
        if total != manifest.size || file_hash != manifest.file_sha256 {
            return Err(StoreError::InvalidPayload);
        }
        temp.persist(&target)
            .map_err(|error| StoreError::Io(error.error))?;

        let previous_path: Option<String> = tx
            .query_row(
                "SELECT stored_path FROM attachments WHERE sync_id=?1",
                [&entity_id],
                |row| row.get(0),
            )
            .optional()?;
        tx.execute(
            "INSERT INTO attachments(
               item_id,name,stored_path,media_type,size,created_at,sync_id
             ) VALUES(?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(sync_id) DO UPDATE SET item_id=excluded.item_id,
               name=excluded.name,stored_path=excluded.stored_path,
               media_type=excluded.media_type,size=excluded.size",
            params![
                item_id,
                name,
                target.to_string_lossy(),
                media_type,
                size as i64,
                db::now_ms(),
                entity_id
            ],
        )?;
        if let Some(previous) = previous_path {
            let previous = PathBuf::from(previous);
            if previous != target {
                obsolete_files.push(previous);
            }
        }
        write_shadow(
            &tx,
            workspace_id,
            kind_name(&EntityKind::Attachment),
            &entity_id,
            desired_shadow,
        )?;
        projected += 1;
    }
    tx.commit()?;

    for path in obsolete_files {
        if path.starts_with(&canonical_dir) {
            let _ = fs::remove_file(path);
        }
    }
    Ok(projected)
}

fn validate_manifest(manifest: &FileManifest) -> bool {
    if manifest.schema_version != 1
        || manifest.size > db::MAX_ATTACHMENT_BYTES
        || manifest.chunk_size == 0
        || manifest.chunk_size as usize > DEFAULT_CHUNK_SIZE
        || !is_hash(&manifest.file_sha256)
    {
        return false;
    }
    if manifest.size == 0 {
        return manifest.chunks.is_empty();
    }
    let expected_max = manifest
        .size
        .div_ceil(manifest.chunk_size as u64)
        .min(1_024) as usize;
    if manifest.chunks.is_empty() || manifest.chunks.len() != expected_max {
        return false;
    }
    let mut total = 0_u64;
    for (index, chunk) in manifest.chunks.iter().enumerate() {
        let maximum = manifest.chunk_size as u64;
        if !is_hash(&chunk.sha256)
            || chunk.size == 0
            || chunk.size > maximum
            || (index + 1 < manifest.chunks.len() && chunk.size != maximum)
        {
            return false;
        }
        total = match total.checked_add(chunk.size) {
            Some(total) => total,
            None => return false,
        };
    }
    total == manifest.size
}

fn is_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn write_shadow(
    tx: &Transaction<'_>,
    workspace_id: &str,
    kind: &str,
    entity_id: &str,
    shadow: Shadow,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO sync_shadow(workspace_id,entity_kind,entity_id,snapshot_json)
         VALUES(?1,?2,?3,?4)
         ON CONFLICT(workspace_id,entity_kind,entity_id)
         DO UPDATE SET snapshot_json=excluded.snapshot_json",
        params![
            workspace_id,
            kind,
            entity_id,
            serde_json::to_string(&shadow)?
        ],
    )?;
    Ok(())
}

pub fn pending_count(conn: &Connection, workspace_id: &str) -> rusqlite::Result<u64> {
    let count = conn.query_row(
        "SELECT COUNT(*)
         FROM sync_operations AS operation
         WHERE operation.workspace_id=?1
           AND (
             NOT EXISTS(
               SELECT 1 FROM sync_peers AS peer WHERE peer.workspace_id=?1
             )
             OR EXISTS(
               SELECT 1
               FROM sync_peers AS peer
               WHERE peer.workspace_id=?1
                 AND COALESCE((
                   SELECT frontier.counter
                   FROM sync_peer_frontiers AS frontier
                   WHERE frontier.workspace_id=?1
                     AND frontier.peer_actor_id=peer.actor_id
                     AND frontier.operation_actor_id=operation.actor_id
                 ),0) < operation.counter
             )
           )",
        [workspace_id],
        |row| row.get(0),
    )?;
    Ok(count)
}

pub fn chunk_source(
    conn: &Connection,
    hash: &str,
) -> rusqlite::Result<Option<(String, u64, usize)>> {
    conn.query_row(
        "SELECT stored_path,offset,size FROM sync_file_chunks WHERE sha256=?1",
        [hash],
        |row| {
            Ok((
                row.get(0)?,
                row.get::<_, i64>(1)? as u64,
                row.get::<_, i64>(2)? as usize,
            ))
        },
    )
    .optional()
}

pub fn store_received_chunk(
    conn: &Connection,
    chunks_dir: &Path,
    hash: &str,
    bytes: &[u8],
) -> Result<(), StoreError> {
    if !files::verify_chunk(hash, bytes) {
        return Err(StoreError::InvalidPayload);
    }
    fs::create_dir_all(chunks_dir)?;
    let final_path = chunks_dir.join(hash);
    let mut temp = tempfile::NamedTempFile::new_in(chunks_dir)?;
    std::io::Write::write_all(&mut temp, bytes)?;
    temp.persist(&final_path)
        .map_err(|error| StoreError::Io(error.error))?;
    conn.execute(
        "INSERT INTO sync_file_chunks(sha256,size,stored_path,verified_at,offset)
         VALUES(?1,?2,?3,?4,0)
         ON CONFLICT(sha256) DO UPDATE SET size=excluded.size,
           stored_path=excluded.stored_path,verified_at=excluded.verified_at,offset=0",
        params![
            hash,
            bytes.len() as i64,
            final_path.to_string_lossy(),
            db::now_ms()
        ],
    )?;
    Ok(())
}

fn kind_name(kind: &EntityKind) -> &'static str {
    match kind {
        EntityKind::Section => "section",
        EntityKind::Item => "item",
        EntityKind::Attachment => "attachment",
    }
}

fn parse_kind(value: &str) -> Result<EntityKind, StoreError> {
    match value {
        "section" => Ok(EntityKind::Section),
        "item" => Ok(EntityKind::Item),
        "attachment" => Ok(EntityKind::Attachment),
        _ => Err(StoreError::InvalidPayload),
    }
}

#[derive(Debug)]
pub enum StoreError {
    Database(rusqlite::Error),
    Json(serde_json::Error),
    Io(std::io::Error),
    InvalidPayload,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "database error: {error}"),
            Self::Json(error) => write!(formatter, "JSON error: {error}"),
            Self::Io(error) => write!(formatter, "file error: {error}"),
            Self::InvalidPayload => formatter.write_str("invalid sync payload"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Database(value)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanner_emits_only_changes_and_remote_apply_is_idempotent() {
        let mut conn = db::init(Path::new(":memory:")).unwrap();
        crate::sync::migrate(&conn).unwrap();
        let actor = Uuid::new_v4().to_string();
        let workspace = Uuid::new_v4().to_string();
        db::insert_item(&conn, "first", None).unwrap();

        assert!(scan_local(&mut conn, &workspace, &actor).unwrap() > 0);
        assert_eq!(scan_local(&mut conn, &workspace, &actor).unwrap(), 0);
        let frontier = VersionVector::default();
        let payload = SyncPayload {
            schema_version: SYNC_SCHEMA_VERSION,
            workspace_id: workspace.clone(),
            frontier,
            operations: vec![],
        };
        let peer = Uuid::new_v4().to_string();
        let first = exchange(&mut conn, &workspace, &peer, payload).unwrap();
        assert!(!first.operations.is_empty());
        let acknowledgement = SyncPayload {
            schema_version: SYNC_SCHEMA_VERSION,
            workspace_id: workspace.clone(),
            frontier: first.frontier.clone(),
            operations: vec![],
        };
        let second = exchange(&mut conn, &workspace, &peer, acknowledgement).unwrap();
        assert!(second.operations.is_empty());
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn bounded_delta_frontier_requires_every_page() {
        let mut conn = db::init(Path::new(":memory:")).unwrap();
        crate::sync::migrate(&conn).unwrap();
        let workspace = Uuid::new_v4().to_string();
        let local_actor = Uuid::new_v4().to_string();
        let peer_actor = Uuid::new_v4().to_string();
        let tx = conn.transaction().unwrap();
        for _ in 0..(MAX_REMOTE_OPS + 1) {
            let operation = next_operation(
                &tx,
                &workspace,
                &local_actor,
                EntityKind::Item,
                Uuid::new_v4().to_string(),
                Mutation::SetContent {
                    context: VersionVector::default(),
                    value: "page me".into(),
                },
            )
            .unwrap();
            insert_operation(&tx, &operation).unwrap();
        }
        tx.commit().unwrap();

        let first = exchange(
            &mut conn,
            &workspace,
            &peer_actor,
            SyncPayload {
                schema_version: SYNC_SCHEMA_VERSION,
                workspace_id: workspace.clone(),
                frontier: VersionVector::default(),
                operations: vec![],
            },
        )
        .unwrap();
        assert_eq!(first.operations.len(), MAX_REMOTE_OPS);
        assert_eq!(
            first.frontier.0.get(&local_actor),
            Some(&(MAX_REMOTE_OPS as u64))
        );

        let second = exchange(
            &mut conn,
            &workspace,
            &peer_actor,
            SyncPayload {
                schema_version: SYNC_SCHEMA_VERSION,
                workspace_id: workspace.clone(),
                frontier: first.frontier,
                operations: vec![],
            },
        )
        .unwrap();
        assert_eq!(second.operations.len(), 1);
        assert_eq!(
            second.frontier.0.get(&local_actor),
            Some(&((MAX_REMOTE_OPS + 1) as u64))
        );
    }

    #[test]
    fn swift_and_rust_share_the_canonical_wire_fixture() {
        let payload: SyncPayload =
            serde_json::from_str(include_str!("../../../fixtures/sync-wire-v1.json")).unwrap();
        let workspace = payload.workspace_id.clone();
        let peer = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        for operation in &payload.operations {
            validate_operation(operation, &workspace).unwrap();
        }

        let mut conn = db::init(Path::new(":memory:")).unwrap();
        crate::sync::migrate(&conn).unwrap();
        exchange(&mut conn, &workspace, peer, payload).unwrap();
        let item: (String, i64, i64) = conn
            .query_row(
                "SELECT content,done,created_at FROM items
                 WHERE sync_id='33333333-3333-4333-8333-333333333333'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(item, ("From Rust".into(), 1, 1_786_276_800_000));
    }

    #[test]
    fn verified_chunks_project_a_real_attachment_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.txt");
        let plaintext = b"chunked attachment";
        fs::write(&source, plaintext).unwrap();
        let manifest = files::manifest(&source, 4).unwrap();
        let chunks_dir = temp.path().join("chunks");
        let attachments_dir = temp.path().join("attachments");
        let mut conn = db::init(Path::new(":memory:")).unwrap();
        crate::sync::migrate(&conn).unwrap();
        let item_id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO items(content,done,created_at,updated_at,sync_id)
             VALUES('parent',0,1,1,?1)",
            [&item_id],
        )
        .unwrap();

        let workspace = Uuid::new_v4().to_string();
        let actor = Uuid::new_v4().to_string();
        let attachment_id = Uuid::new_v4().to_string();
        let values = [
            ("itemId", Value::String(item_id)),
            ("name", Value::String("note.txt".into())),
            ("mediaType", Value::String("text/plain".into())),
            ("size", Value::Number((manifest.size as i64).into())),
            ("manifest", serde_json::to_value(&manifest).unwrap()),
        ];
        let operations = values
            .into_iter()
            .enumerate()
            .map(|(index, (field, value))| Operation {
                schema_version: SYNC_SCHEMA_VERSION,
                workspace_id: workspace.clone(),
                entity_kind: EntityKind::Attachment,
                entity_id: attachment_id.clone(),
                dot: Dot {
                    actor_id: actor.clone(),
                    counter: index as u64 + 1,
                },
                mutation: Mutation::SetMetadata {
                    field: field.into(),
                    value,
                },
            })
            .collect::<Vec<_>>();
        exchange(
            &mut conn,
            &workspace,
            &actor,
            SyncPayload {
                schema_version: SYNC_SCHEMA_VERSION,
                workspace_id: workspace.clone(),
                frontier: VersionVector(BTreeMap::from([(actor.clone(), 5)])),
                operations,
            },
        )
        .unwrap();

        let mut offset = 0;
        for descriptor in &manifest.chunks {
            let end = offset + descriptor.size as usize;
            store_received_chunk(
                &conn,
                &chunks_dir,
                &descriptor.sha256,
                &plaintext[offset..end],
            )
            .unwrap();
            offset = end;
        }
        assert_eq!(
            project_ready_attachments(&mut conn, &workspace, &attachments_dir).unwrap(),
            1
        );
        let stored: String = conn
            .query_row(
                "SELECT stored_path FROM attachments WHERE sync_id=?1",
                [&attachment_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fs::read(stored).unwrap(), plaintext);
        let stored_path: String = conn
            .query_row(
                "SELECT stored_path FROM attachments WHERE sync_id=?1",
                [&attachment_id],
                |row| row.get(0),
            )
            .unwrap();
        let modified = fs::metadata(&stored_path).unwrap().modified().unwrap();
        assert_eq!(
            project_ready_attachments(&mut conn, &workspace, &attachments_dir).unwrap(),
            0
        );
        assert_eq!(
            fs::metadata(stored_path).unwrap().modified().unwrap(),
            modified
        );
    }

    #[test]
    fn rejects_actor_gaps_and_conflicting_dot_replays() {
        let mut conn = db::init(Path::new(":memory:")).unwrap();
        crate::sync::migrate(&conn).unwrap();
        let workspace = Uuid::new_v4().to_string();
        let actor = Uuid::new_v4().to_string();
        let entity = Uuid::new_v4().to_string();
        let operation = |counter, value: &str| Operation {
            schema_version: SYNC_SCHEMA_VERSION,
            workspace_id: workspace.clone(),
            entity_kind: EntityKind::Item,
            entity_id: entity.clone(),
            dot: Dot {
                actor_id: actor.clone(),
                counter,
            },
            mutation: Mutation::SetContent {
                context: VersionVector::default(),
                value: value.into(),
            },
        };
        let payload = |operation: Operation| SyncPayload {
            schema_version: SYNC_SCHEMA_VERSION,
            workspace_id: workspace.clone(),
            frontier: VersionVector(BTreeMap::from([(actor.clone(), operation.dot.counter)])),
            operations: vec![operation],
        };
        assert!(matches!(
            exchange(&mut conn, &workspace, &actor, payload(operation(2, "gap"))),
            Err(StoreError::InvalidPayload)
        ));
        exchange(
            &mut conn,
            &workspace,
            &actor,
            payload(operation(1, "original")),
        )
        .unwrap();
        assert!(matches!(
            exchange(
                &mut conn,
                &workspace,
                &actor,
                payload(operation(1, "forged replay"))
            ),
            Err(StoreError::InvalidPayload)
        ));
    }
}

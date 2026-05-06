//! Engine `OpKind` ↔ WAL `WalOp` projection.
//!
//! The engine keeps an in-memory [`crate::OpKind`] log; each entry is also
//! projected to a [`mimisbrunnr_wal::WalOp`] at append time so the WAL ring
//! gets a durable record. Replay reverses the projection.
//!
//! ## Mapping table
//!
//! | Engine `OpKind`        | WAL `WalOpKind`        |
//! | ---------------------- | ---------------------- |
//! | `CreateObject`         | `CreateObject`         |
//! | `DeleteObject`         | `DeleteObject`         |
//! | `AddTag`               | `AddTag`               |
//! | `RemoveTag`            | `RemoveTag`            |
//! | `SetAttr`              | `SetAttr`              |
//! | `RemoveAttr`           | `RemoveAttr`           |
//! | `AddRelation`          | `AddRelation`          |
//! | `RemoveRelation`       | `RemoveRelation`       |
//! | `WriteBlob`            | `WriteBlob`            |
//!
//! The wider WAL universe (chunked-flow ops, snapshot lifecycle, bucket
//! lifecycle, backpointers, reconcile, format-promote) is **not** generated
//! by the current engine projection — those ops are written directly by their
//! respective subsystems when they land. Replay nevertheless tolerates them
//! by ignoring entries whose kind has no engine analogue.

use mimisbrunnr_storage::BlockRef;
use mimisbrunnr_types::{ObjectId, TagId, value_hash};
use mimisbrunnr_wal::{
    AddRelation as WalAddRelation, AddTag as WalAddTag, CreateObject as WalCreateObject,
    DeleteObject as WalDeleteObject, RemoveAttr as WalRemoveAttr,
    RemoveRelation as WalRemoveRelation, RemoveTag as WalRemoveTag, SetAttr as WalSetAttr,
    WalOp, WalOpKind, WriteBlob as WalWriteBlob,
};

use crate::{Engine, EngineError, OpKind};

/// Origin byte for the `Direct` provenance — matches IMPL §3.3 / DESIGN §6.4.
pub const TAG_ORIGIN_DIRECT: u8 = 0;
/// Origin byte for the `Materialized` provenance.
pub const TAG_ORIGIN_MATERIALIZED: u8 = 1;

/// Project an engine `OpKind` into the equivalent WAL payload (`WalOp` carries
/// both the kind discriminant and the CBOR payload).
pub fn project_op(op: &OpKind) -> WalOp {
    match op {
        OpKind::CreateObject { oid } => WalOp::CreateObject(WalCreateObject {
            oid: oid.to_u64(),
            generation: 0,
            created_ns: 0,
        }),
        OpKind::DeleteObject { oid } => WalOp::DeleteObject(WalDeleteObject {
            oid: oid.to_u64(),
            lsn: 0,
        }),
        OpKind::AddTag { oid, tag } => WalOp::AddTag(WalAddTag {
            oid: oid.to_u64(),
            tag: tag.raw(),
            origin: TAG_ORIGIN_DIRECT,
        }),
        OpKind::RemoveTag { oid, tag } => WalOp::RemoveTag(WalRemoveTag {
            oid: oid.to_u64(),
            tag: tag.raw(),
        }),
        OpKind::SetAttr { oid, key, value } => WalOp::SetAttr(WalSetAttr {
            oid: oid.to_u64(),
            key: key.raw(),
            value: value.clone(),
        }),
        OpKind::RemoveAttr {
            oid,
            key,
            value_hash: vh,
        } => WalOp::RemoveAttr(WalRemoveAttr {
            oid: oid.to_u64(),
            key: key.raw(),
            value_hash: *vh,
        }),
        OpKind::AddRelation {
            oid,
            predicate,
            target,
        } => WalOp::AddRelation(WalAddRelation {
            oid: oid.to_u64(),
            predicate: predicate.raw(),
            target: target.to_u64(),
        }),
        OpKind::RemoveRelation {
            oid,
            predicate,
            target,
        } => WalOp::RemoveRelation(WalRemoveRelation {
            oid: oid.to_u64(),
            predicate: predicate.raw(),
            target: target.to_u64(),
        }),
        OpKind::WriteBlob {
            oid,
            content_hash,
            size,
        } => WalOp::WriteBlob(WalWriteBlob {
            oid: oid.to_u64(),
            content_hash: *content_hash,
            extent: BlockRef::new(0, 0, 0).into(),
            size: *size,
        }),
    }
}

/// Apply a WAL entry to a fresh / partially-recovered [`Engine`].
///
/// `lsn` is the entry's LSN; the engine refuses re-application of an LSN it
/// has already seen (returns [`EngineError::LsnAlreadyApplied`]).
pub fn replay_wal_op(
    engine: &mut Engine,
    kind: WalOpKind,
    payload: &[u8],
    lsn: u64,
) -> Result<(), EngineError> {
    if lsn != 0 && lsn <= engine.last_applied_lsn {
        return Err(EngineError::LsnAlreadyApplied(lsn));
    }
    let op = WalOp::decode(kind, payload)?;
    match op {
        WalOp::CreateObject(p) => {
            let oid = ObjectId::from_u64(p.oid);
            engine.replay_create_object(oid, p.created_ns, lsn)?;
        }
        WalOp::DeleteObject(p) => {
            let oid = ObjectId::from_u64(p.oid);
            engine.replay_delete_object(oid, lsn)?;
        }
        WalOp::AddTag(p) => {
            let oid = ObjectId::from_u64(p.oid);
            engine.replay_add_tag(oid, TagId::new(p.tag), lsn)?;
        }
        WalOp::RemoveTag(p) => {
            let oid = ObjectId::from_u64(p.oid);
            engine.replay_remove_tag(oid, TagId::new(p.tag), lsn)?;
        }
        WalOp::SetAttr(p) => {
            let oid = ObjectId::from_u64(p.oid);
            engine.replay_set_attr(oid, TagId::new(p.key), p.value, lsn)?;
        }
        WalOp::RemoveAttr(p) => {
            let oid = ObjectId::from_u64(p.oid);
            engine.replay_remove_attr_by_hash(oid, TagId::new(p.key), p.value_hash, lsn)?;
        }
        WalOp::AddRelation(p) => {
            let oid = ObjectId::from_u64(p.oid);
            let target = ObjectId::from_u64(p.target);
            engine.replay_add_relation(oid, TagId::new(p.predicate), target, lsn)?;
        }
        WalOp::RemoveRelation(p) => {
            let oid = ObjectId::from_u64(p.oid);
            let target = ObjectId::from_u64(p.target);
            engine.replay_remove_relation(oid, TagId::new(p.predicate), target, lsn)?;
        }
        WalOp::WriteBlob(p) => {
            let oid = ObjectId::from_u64(p.oid);
            engine.replay_write_blob(oid, p.content_hash, p.size, lsn)?;
        }
        // Ops outside the engine's mutation surface — silently skipped during
        // replay. Each is owned by its own subsystem and recovered there.
        WalOp::ChunkInsertBatch(_)
        | WalOp::ChunkListAppend(_)
        | WalOp::ChunkListReplace(_)
        | WalOp::ChunkListShrink(_)
        | WalOp::ChunkObjectFinalize(_)
        | WalOp::BucketAlloc(_)
        | WalOp::BucketWrite(_)
        | WalOp::BucketGenBump(_)
        | WalOp::BucketDiscard(_)
        | WalOp::BackpointerInsert(_)
        | WalOp::BackpointerRemove(_)
        | WalOp::TagBitmapGrow(_)
        | WalOp::TagBitmapShrink(_)
        | WalOp::SnapshotCreate(_)
        | WalOp::SnapshotDelete(_)
        | WalOp::SnapshotUnlink(_)
        | WalOp::SnapshotDepthUpdate(_)
        | WalOp::ReconcileEnqueue(_)
        | WalOp::ReconcileDequeue(_)
        | WalOp::ReconcileMove(_)
        | WalOp::ReconcileScanStep(_)
        | WalOp::FormatPromote(_)
        | WalOp::Checkpoint(_) => {
            // Engine doesn't model these in the in-memory mirror; ignore.
        }
    }
    engine.last_applied_lsn = engine.last_applied_lsn.max(lsn);
    Ok(())
}

/// Helper: compute the `value_hash` the engine and the WAL agree on for
/// `Attr` payloads. Wraps [`mimisbrunnr_types::value_hash`] so callers don't
/// have to plumb the per-pool secret. Today we use the all-zero secret per
/// the IMPL §4.2 TODO; replace once the per-pool secret is wired.
// TODO(rewrite-phase-N): plumb the per-pool secret from `Superblock`.
pub fn engine_value_hash(value: &mimisbrunnr_types::Value) -> u64 {
    value_hash(value, &[0u8; 16])
}

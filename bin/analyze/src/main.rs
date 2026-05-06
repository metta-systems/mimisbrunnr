//! `analyze` — read-only visual inspector for a Mímisbrunnr pool.
//!
//! Phase 7d rewrite. Opens a pool via [`DiskEngine::open`] (read-write today —
//! see `open_pool_read_only`), builds a [`PoolSnapshot`] of the in-memory state
//! plus on-disk header metadata, and renders it via either:
//!
//! - an `egui` GUI with tabs (Pool / Disk / WAL / Objects / Tags / Ontology),
//! - a plain-text dump (`--dump`),
//! - a JSON-ish dump (`--json`).
//!
//! The GUI is read-only and does not poll for changes — it shows a static
//! snapshot taken at startup.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use clap::Parser;
use eframe::egui;

use mimisbrunnr::{
    engine::{DiskEngine, OpKind, OpLogEntry},
    meta::ObjectRecord,
    ontology::TagSemantics,
    pool::{PoolConfig, PoolStatus},
    storage::{BlockDevice, RootPointer, Superblock},
    types::{
        Assertion, CompressionState, DiskId, MediaType, ObjectId, ObjectState, StorageTier, TagId,
        Value,
    },
    wal::{Wal, WalEntry, WalOp, WalOpKind},
};

// -----------------------------------------------------------------------
// CLI
// -----------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(
    name = "analyze",
    about = "Read-only visual inspector for a Mímisbrunnr pool"
)]
struct Cli {
    /// Path to pool.toml.
    #[arg(long)]
    pool: PathBuf,

    /// Print a plain-text dump and exit (no GUI). Useful for CI.
    #[arg(long, conflicts_with = "json")]
    dump: bool,

    /// Print a JSON-ish dump and exit (no GUI).
    #[arg(long)]
    json: bool,
}

// -----------------------------------------------------------------------
// PoolSnapshot — pre-extracted, GUI-frame-stable view of the pool.
// -----------------------------------------------------------------------

/// Static snapshot of a pool's state as observed at startup.
///
/// Captured *once* by [`build_snapshot`]; the GUI never re-reads the disk.
#[derive(Debug, Clone)]
pub struct PoolSnapshot {
    /// `pool.toml` configuration as supplied to the engine.
    pub pool_config: PoolConfig,
    /// Live `PoolManager` capacity / health view at snapshot time.
    pub pool_status: PoolStatus,
    /// Engine-level node id.
    pub node_id: u16,
    /// Per-disk on-disk header summaries.
    pub disks: Vec<DiskSummary>,
    /// Tag-index snapshot, sorted by descending cardinality.
    pub tags: Vec<TagSnapshot>,
    /// Object-record snapshot.
    pub objects: Vec<ObjectSnapshot>,
    /// Total object count (active + tombstoned).
    pub total_objects: usize,
    /// Total tag count.
    pub total_tags: usize,
    /// Engine-level oplog tail (most-recent first).
    pub oplog: Vec<OpLogEntryView>,
    /// Recent WAL entries, newest first; lifted from the primary disk's WAL.
    pub wal_entries: Vec<WalEntryView>,
    /// Header summary for the primary disk's WAL ring.
    pub wal_header: WalHeaderView,
    /// Installed ontology modules.
    pub modules: Vec<ModuleSnapshot>,
    /// Edges in the implication DAG (`from_id`, `to_id`).
    pub implications: Vec<(u32, u32)>,
}

/// On-disk identity / layout summary for one disk in the pool.
#[derive(Debug, Clone)]
pub struct DiskSummary {
    pub disk_id: DiskId,
    pub path: PathBuf,
    pub media_type: MediaType,
    pub tier: StorageTier,
    pub capacity_bytes: u64,
    pub used_bytes: u64,
    pub state: String,
    /// Decoded superblock (only the primary disk for now — see TODO below).
    pub superblock: Option<SuperblockView>,
    pub root_pointer: Option<RootPointerView>,
}

/// Decoded view of the fields most useful to an operator.
#[derive(Debug, Clone)]
pub struct SuperblockView {
    pub fs_uuid: [u8; 16],
    pub node_id: u16,
    pub disk_id: u16,
    pub media_type: MediaType,
    pub tier: StorageTier,
    pub device_capacity: u64,
    pub creation_timestamp_ns: i64,
    pub last_mount_timestamp_ns: i64,
    pub mount_count: u64,
    pub bucket_size_log2: u8,
    pub btree_node_size_log2: u8,
    pub wal_offset: u64,
    pub wal_size: u64,
    pub index_zone_offset: u64,
    pub index_zone_length: u64,
    pub metadata_zone_offset: u64,
    pub metadata_zone_length: u64,
    pub blob_zone_offset: u64,
    pub blob_zone_length: u64,
    pub fs_format_version: u32,
}

/// Decoded `RootPointer` summary.
#[derive(Debug, Clone)]
pub struct RootPointerView {
    pub seq: u64,
    pub lsn: u64,
    pub flags: u32,
    /// `(field_name, "disk_id:block_no@gen")` for each B+ tree root slot.
    pub roots: Vec<(&'static str, String)>,
}

/// Snapshot of the WAL header.
#[derive(Debug, Clone, Default)]
pub struct WalHeaderView {
    pub next_lsn: u64,
    pub write_cursor: u64,
    pub read_cursor: u64,
    pub used_bytes: u64,
    pub data_capacity: u64,
    pub last_checkpoint_lsn: u64,
}

/// One WAL entry, decoded into a view.
#[derive(Debug, Clone)]
pub struct WalEntryView {
    pub lsn: u64,
    pub op_kind: u8,
    pub op_kind_name: String,
    pub payload_length: u32,
    pub timestamp_ns: i64,
    /// CBOR-pretty payload preview, lazily computed via [`WalOp::decode`].
    pub payload_preview: String,
}

/// One tag in the index, with a small member preview.
#[derive(Debug, Clone)]
pub struct TagSnapshot {
    pub id: TagId,
    pub name: String,
    pub semantics: String,
    pub object_count: u32,
    /// First few member oids (raw form).
    pub member_preview: Vec<u64>,
}

/// One object's metadata view.
#[derive(Debug, Clone)]
pub struct ObjectSnapshot {
    pub oid: ObjectId,
    pub state: ObjectState,
    pub compression: CompressionState,
    pub blob_length: u64,
    pub stored_size: u64,
    pub generation: u32,
    pub direct_tags: Vec<u32>,
    pub materialized_tags: Vec<u32>,
    pub attrs: Vec<(String, String)>,
}

/// One installed ontology module.
#[derive(Debug, Clone)]
pub struct ModuleSnapshot {
    pub id: String,
    pub version: String,
    pub name: String,
    pub installed_tags: usize,
    pub installed_implications: usize,
}

/// View of one `OpLogEntry`.
#[derive(Debug, Clone)]
pub struct OpLogEntryView {
    pub lsn: u64,
    pub timestamp_ns: i64,
    pub op_summary: String,
}

// -----------------------------------------------------------------------
// build_snapshot — pure logic, no GUI deps. Tested separately.
// -----------------------------------------------------------------------

/// Build a [`PoolSnapshot`] from a live `DiskEngine`.
///
/// Reads the primary device's superblock + WAL ring straight from disk (read
/// path only) and walks the in-memory engine mirrors for the rest.
pub fn build_snapshot(engine: &DiskEngine) -> PoolSnapshot {
    let pool_config = engine.config.clone();
    let pool_status = engine.pool.status();
    let node_id = engine.engine.node_id;

    let disks = collect_disks(engine, &pool_config);
    let tags = collect_tags(engine);
    let objects = collect_objects(engine);
    let oplog = collect_oplog(&engine.engine.oplog);
    let wal_entries = collect_wal_entries(&engine.wal, engine.primary_device.as_ref());
    let wal_header = wal_header_view(&engine.wal);
    let modules = collect_modules(engine);
    let implications: Vec<(u32, u32)> = engine
        .engine
        .ontology
        .dag
        .edges()
        .map(|(f, t)| (f.raw(), t.raw()))
        .collect();

    let total_objects = engine.engine.object_count();
    let total_tags = engine.engine.tag_index.tag_count();

    PoolSnapshot {
        pool_config,
        pool_status,
        node_id,
        disks,
        tags,
        objects,
        total_objects,
        total_tags,
        oplog,
        wal_entries,
        wal_header,
        modules,
        implications,
    }
}

fn collect_disks(engine: &DiskEngine, cfg: &PoolConfig) -> Vec<DiskSummary> {
    let primary_id = cfg.primary().map(|d| d.id);
    let mut out = Vec::with_capacity(cfg.disks.len());
    for entry in &cfg.disks {
        let runtime = engine.pool.runtime(entry.id);
        let used = runtime.map(|r| r.used_bytes).unwrap_or(0);
        let state = runtime
            .map(|r| format!("{:?}", r.state))
            .unwrap_or_else(|| "Unknown".to_string());
        let (sb, root) = if Some(entry.id) == primary_id {
            (
                Some(superblock_view(&engine.superblock)),
                Some(root_pointer_view(engine.superblock.active_root_pointer())),
            )
        } else if let Some(disk_sb) = engine.pool.disk_superblock(entry.id) {
            (
                Some(superblock_view(disk_sb)),
                Some(root_pointer_view(disk_sb.active_root_pointer())),
            )
        } else {
            (None, None)
        };
        out.push(DiskSummary {
            disk_id: entry.id,
            path: entry.path.clone(),
            media_type: entry.media_type,
            tier: entry.tier,
            capacity_bytes: entry.capacity_bytes,
            used_bytes: used,
            state,
            superblock: sb,
            root_pointer: root,
        });
    }
    out
}

fn superblock_view(sb: &Superblock) -> SuperblockView {
    SuperblockView {
        fs_uuid: { sb.fs_uuid },
        node_id: { sb.node_id },
        disk_id: { sb.disk_id },
        media_type: sb.media_type().unwrap_or(MediaType::Ssd),
        tier: sb.tier().unwrap_or(StorageTier::Hot),
        device_capacity: { sb.device_capacity },
        creation_timestamp_ns: { sb.creation_timestamp_ns },
        last_mount_timestamp_ns: { sb.last_mount_timestamp_ns },
        mount_count: { sb.mount_count },
        bucket_size_log2: { sb.bucket_size_log2 },
        btree_node_size_log2: { sb.btree_node_size_log2 },
        wal_offset: { sb.wal_offset },
        wal_size: { sb.wal_size },
        index_zone_offset: { sb.index_zone.offset },
        index_zone_length: { sb.index_zone.length },
        metadata_zone_offset: { sb.metadata_zone.offset },
        metadata_zone_length: { sb.metadata_zone.length },
        blob_zone_offset: { sb.blob_zone.offset },
        blob_zone_length: { sb.blob_zone.length },
        fs_format_version: { sb.fs_format_version },
    }
}

fn root_pointer_view(rp: &RootPointer) -> RootPointerView {
    let fmt = |b: &mimisbrunnr::storage::BlockRef| -> String {
        let disk = { b.disk_id };
        let block = { b.block_no };
        let gen_ = { b.generation };
        format!("{disk}:{block}@{gen_}")
    };
    let roots = vec![
        ("object_table_root", fmt(&{ rp.object_table_root })),
        ("object_history_root", fmt(&{ rp.object_history_root })),
        ("location_table_root", fmt(&{ rp.location_table_root })),
        ("location_history_root", fmt(&{ rp.location_history_root })),
        ("forward_index_root", fmt(&{ rp.forward_index_root })),
        ("tag_index_root", fmt(&{ rp.tag_index_root })),
        ("kv_index_root", fmt(&{ rp.kv_index_root })),
        ("range_index_root", fmt(&{ rp.range_index_root })),
        ("chunk_index_root", fmt(&{ rp.chunk_index_root })),
        ("value_spill_root", fmt(&{ rp.value_spill_root })),
        ("backpointer_root", fmt(&{ rp.backpointer_root })),
        ("ontology_root", fmt(&{ rp.ontology_root })),
        ("subscriptions_root", fmt(&{ rp.subscriptions_root })),
        ("pool_state_root", fmt(&{ rp.pool_state_root })),
        ("snapshot_chain_root", fmt(&{ rp.snapshot_chain_root })),
        ("placement_rules_root", fmt(&{ rp.placement_rules_root })),
        ("cluster_peers_root", fmt(&{ rp.cluster_peers_root })),
    ];
    RootPointerView {
        seq: { rp.seq },
        lsn: { rp.lsn },
        flags: { rp.flags },
        roots,
    }
}

fn collect_tags(engine: &DiskEngine) -> Vec<TagSnapshot> {
    let dag = &engine.engine.ontology.dag;
    let mut tags: Vec<TagSnapshot> = Vec::new();
    for (tag_id, store) in engine.engine.tag_index.iter() {
        let bitmap = store.members();
        let count = bitmap.len() as u32;
        let preview: Vec<u64> = bitmap.iter().take(8).map(u64::from).collect();
        let def = engine.engine.ontology.tags.get(tag_id);
        let (name, semantics) = match def {
            Some(d) => (d.name.clone(), semantics_label(&d.semantics)),
            None => (format!("tag_{}", tag_id.raw()), "unknown".to_string()),
        };
        tags.push(TagSnapshot {
            id: *tag_id,
            name,
            semantics,
            object_count: count,
            member_preview: preview,
        });
    }
    // Also include tags that exist in the ontology but have empty bitmaps,
    // so the Ontology tab matches the tag-index tab counts.
    for tag_id in dag.tags() {
        if engine.engine.tag_index.get(tag_id).is_none() {
            let def = engine.engine.ontology.tags.get(&tag_id);
            let (name, semantics) = match def {
                Some(d) => (d.name.clone(), semantics_label(&d.semantics)),
                None => (format!("tag_{}", tag_id.raw()), "unknown".to_string()),
            };
            tags.push(TagSnapshot {
                id: tag_id,
                name,
                semantics,
                object_count: 0,
                member_preview: vec![],
            });
        }
    }
    // Sort by descending cardinality, then by name for deterministic output.
    tags.sort_by(|a, b| {
        b.object_count
            .cmp(&a.object_count)
            .then_with(|| a.name.cmp(&b.name))
    });
    tags
}

fn semantics_label(s: &TagSemantics) -> String {
    match s {
        TagSemantics::Label => "label".into(),
        TagSemantics::Attribute { value_type } => format!("attr({value_type:?})"),
        TagSemantics::Grouping => "grouping".into(),
        TagSemantics::OrderedCollection { .. } => "ordered".into(),
        TagSemantics::Hierarchical => "hierarchical".into(),
    }
}

fn collect_objects(engine: &DiskEngine) -> Vec<ObjectSnapshot> {
    let mut out = Vec::with_capacity(engine.engine.object_count());
    for (raw, rec) in engine.engine.object_table.iter() {
        let oid = ObjectId::from_u64(*raw);
        let mut direct_tags = Vec::new();
        let mut materialized_tags = Vec::new();
        let mut attrs = Vec::new();
        for (assertion, origin) in engine.engine.forward_index.assertions_of(oid) {
            match assertion {
                Assertion::Tag(t) => match origin {
                    mimisbrunnr::types::TagOrigin::Direct => direct_tags.push(t.raw()),
                    mimisbrunnr::types::TagOrigin::Materialized => materialized_tags.push(t.raw()),
                },
                Assertion::Attr { key, value } => {
                    let key_name = engine
                        .engine
                        .ontology
                        .tags
                        .get(key)
                        .map(|d| d.name.clone())
                        .unwrap_or_else(|| format!("attr_{}", key.raw()));
                    attrs.push((key_name, format_value(value)));
                }
                Assertion::Relation { .. } => {}
            }
        }
        let state = rec.state().unwrap_or(ObjectState::Active);
        let compression = rec.compression().unwrap_or(CompressionState::None);
        out.push(ObjectSnapshot {
            oid,
            state,
            compression,
            blob_length: { rec.blob_length },
            stored_size: { rec.stored_size },
            generation: { rec.generation },
            direct_tags,
            materialized_tags,
            attrs,
        });
    }
    // Sort by node then local seq for stable presentation.
    out.sort_by_key(|o| (o.oid.node_id(), o.oid.local_seq()));
    out
}

fn format_value(v: &Value) -> String {
    match v {
        Value::Text(s) => format!("\"{s}\""),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => format!("{f}"),
        Value::Timestamp(t) => format!("ts:{t}"),
        Value::Blob(b) => format!("<{} bytes>", b.len()),
        Value::Scoped { context, inner } => {
            format!("scoped(ctx={}, {})", context.raw(), format_value(inner))
        }
    }
}

fn collect_oplog(log: &mimisbrunnr::engine::OpLog) -> Vec<OpLogEntryView> {
    let mut out: Vec<OpLogEntryView> = log
        .since(0)
        .map(|e: &OpLogEntry| OpLogEntryView {
            lsn: e.lsn,
            timestamp_ns: e.timestamp.physical_ns,
            op_summary: opkind_summary(&e.op),
        })
        .collect();
    out.reverse(); // most-recent first
    out
}

fn opkind_summary(op: &OpKind) -> String {
    match op {
        OpKind::CreateObject { oid } => format!("CreateObject(#{})", oid.local_seq()),
        OpKind::DeleteObject { oid } => format!("DeleteObject(#{})", oid.local_seq()),
        OpKind::AddTag { oid, tag } => {
            format!("AddTag(#{}, t={})", oid.local_seq(), tag.raw())
        }
        OpKind::RemoveTag { oid, tag } => {
            format!("RemoveTag(#{}, t={})", oid.local_seq(), tag.raw())
        }
        OpKind::SetAttr { oid, key, value } => format!(
            "SetAttr(#{}, k={}, v={})",
            oid.local_seq(),
            key.raw(),
            format_value(value)
        ),
        OpKind::RemoveAttr { oid, key, .. } => {
            format!("RemoveAttr(#{}, k={})", oid.local_seq(), key.raw())
        }
        OpKind::AddRelation {
            oid,
            predicate,
            target,
        } => format!(
            "AddRelation(#{}, p={}, →#{})",
            oid.local_seq(),
            predicate.raw(),
            target.local_seq()
        ),
        OpKind::RemoveRelation {
            oid,
            predicate,
            target,
        } => format!(
            "RemoveRelation(#{}, p={}, →#{})",
            oid.local_seq(),
            predicate.raw(),
            target.local_seq()
        ),
        OpKind::WriteBlob { oid, size, .. } => {
            format!("WriteBlob(#{}, {} B)", oid.local_seq(), size)
        }
    }
}

fn wal_header_view(wal: &Wal) -> WalHeaderView {
    WalHeaderView {
        next_lsn: wal.next_lsn(),
        write_cursor: wal.write_cursor(),
        read_cursor: wal.read_cursor(),
        used_bytes: wal.used_bytes(),
        data_capacity: wal.data_capacity(),
        last_checkpoint_lsn: wal.last_checkpoint_lsn(),
    }
}

/// Walk the WAL ring (read-only) and capture every entry currently live.
/// The newest entries land first in the result.
fn collect_wal_entries(wal: &Wal, device: &dyn BlockDevice) -> Vec<WalEntryView> {
    let mut out = Vec::new();
    for result in wal.iter_from(device, 0) {
        let entry = match result {
            Ok(e) => e,
            Err(_) => break, // surface as truncated history; not a hard error
        };
        out.push(decode_wal_entry(&entry));
    }
    out.reverse();
    // Cap to a sensible upper bound for the GUI table.
    out.truncate(512);
    out
}

fn decode_wal_entry(entry: &WalEntry) -> WalEntryView {
    let op_kind_raw = { entry.header.op_kind };
    let lsn = { entry.header.lsn };
    let payload_length = { entry.header.payload_length };
    let timestamp_ns = { entry.header.timestamp.physical_ns };
    let (kind_name, payload_preview) = match WalOpKind::from_u8(op_kind_raw) {
        Ok(kind) => match WalOp::decode(kind, &entry.payload) {
            Ok(op) => (format!("{kind:?}"), format!("{op:?}")),
            Err(e) => (format!("{kind:?}"), format!("<decode error: {e}>")),
        },
        Err(_) => (format!("unknown({op_kind_raw})"), String::new()),
    };
    WalEntryView {
        lsn,
        op_kind: op_kind_raw,
        op_kind_name: kind_name,
        payload_length,
        timestamp_ns,
        payload_preview,
    }
}

fn collect_modules(engine: &DiskEngine) -> Vec<ModuleSnapshot> {
    let mut out: Vec<ModuleSnapshot> = engine
        .engine
        .ontology
        .installed_modules
        .values()
        .map(|m| ModuleSnapshot {
            id: m.id.clone(),
            version: m.version.clone(),
            name: m.name.clone(),
            installed_tags: m.installed_tags.len(),
            installed_implications: m.installed_implications.len(),
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

// -----------------------------------------------------------------------
// Plain-text + JSON dumps.
// -----------------------------------------------------------------------

/// Render the Pool Overview as a plain-text dump.
pub fn render_dump(snap: &PoolSnapshot) -> String {
    let mut out = String::new();
    out.push_str("=== Mímisbrunnr Pool Overview ===\n");
    out.push_str(&format!("node_id:        {}\n", snap.node_id));
    out.push_str(&format!(
        "disks:          {} (capacity={}, used={})\n",
        snap.pool_status.disk_count,
        format_bytes(snap.pool_status.total_capacity),
        format_bytes(snap.pool_status.total_used)
    ));
    out.push_str(&format!("total_objects:  {}\n", snap.total_objects));
    out.push_str(&format!("total_tags:     {}\n", snap.total_tags));
    out.push_str(&format!("oplog_entries:  {}\n", snap.oplog.len()));
    out.push_str(&format!("wal_next_lsn:   {}\n", snap.wal_header.next_lsn));
    out.push_str(&format!(
        "wal_used:       {} / {} ({}%)\n",
        snap.wal_header.used_bytes,
        snap.wal_header.data_capacity,
        wal_used_percent(&snap.wal_header)
    ));

    out.push_str("\n--- Disks ---\n");
    for d in &snap.disks {
        out.push_str(&format!(
            "  disk{:<3} {:<24}  {:?} {} {} (used {}, state {})\n",
            d.disk_id,
            d.path.display(),
            d.media_type,
            d.tier.name(),
            format_bytes(d.capacity_bytes),
            format_bytes(d.used_bytes),
            d.state,
        ));
    }

    out.push_str("\n--- Tier breakdown ---\n");
    for (tier, b) in &snap.pool_status.by_tier {
        out.push_str(&format!(
            "  {}: {} disks, {} capacity, {} used\n",
            tier.name(),
            b.disk_count,
            format_bytes(b.capacity_bytes),
            format_bytes(b.used_bytes),
        ));
    }

    out.push_str("\n--- Recent oplog ---\n");
    for e in snap.oplog.iter().take(16) {
        out.push_str(&format!("  lsn={:>5}  {}\n", e.lsn, e.op_summary));
    }

    out.push_str("\n--- Top tags ---\n");
    for t in snap.tags.iter().take(16) {
        out.push_str(&format!(
            "  {:<24} ({:>4}) {}\n",
            t.name, t.object_count, t.semantics
        ));
    }

    out.push_str("\n--- Modules ---\n");
    for m in &snap.modules {
        out.push_str(&format!(
            "  {} v{} ({}, {} tags, {} implications)\n",
            m.id, m.version, m.name, m.installed_tags, m.installed_implications,
        ));
    }
    out
}

/// Render the Pool Overview as JSON-ish text. No `serde_json` dep — we hand-format.
pub fn render_json(snap: &PoolSnapshot) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!("  \"node_id\": {},\n", snap.node_id));
    out.push_str(&format!("  \"disk_count\": {},\n", snap.pool_status.disk_count));
    out.push_str(&format!(
        "  \"total_capacity_bytes\": {},\n",
        snap.pool_status.total_capacity
    ));
    out.push_str(&format!(
        "  \"total_used_bytes\": {},\n",
        snap.pool_status.total_used
    ));
    out.push_str(&format!("  \"total_objects\": {},\n", snap.total_objects));
    out.push_str(&format!("  \"total_tags\": {},\n", snap.total_tags));
    out.push_str(&format!(
        "  \"wal_next_lsn\": {},\n",
        snap.wal_header.next_lsn
    ));
    out.push_str(&format!(
        "  \"wal_used_bytes\": {},\n",
        snap.wal_header.used_bytes
    ));
    out.push_str("  \"disks\": [\n");
    for (i, d) in snap.disks.iter().enumerate() {
        let comma = if i + 1 == snap.disks.len() { "" } else { "," };
        out.push_str(&format!(
            "    {{ \"id\": {}, \"tier\": \"{}\", \"capacity\": {}, \"used\": {}, \"state\": \"{}\" }}{}\n",
            d.disk_id,
            d.tier.name(),
            d.capacity_bytes,
            d.used_bytes,
            d.state,
            comma
        ));
    }
    out.push_str("  ],\n");
    out.push_str("  \"top_tags\": [\n");
    for (i, t) in snap.tags.iter().take(16).enumerate() {
        let comma = if i + 1 == snap.tags.iter().take(16).count() {
            ""
        } else {
            ","
        };
        out.push_str(&format!(
            "    {{ \"id\": {}, \"name\": \"{}\", \"count\": {} }}{}\n",
            t.id.raw(),
            t.name,
            t.object_count,
            comma
        ));
    }
    out.push_str("  ]\n");
    out.push_str("}\n");
    out
}

fn wal_used_percent(h: &WalHeaderView) -> u64 {
    if h.data_capacity == 0 {
        0
    } else {
        h.used_bytes.saturating_mul(100) / h.data_capacity
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".into();
    }
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut val = bytes as f64;
    let mut idx = 0;
    while val >= 1024.0 && idx < UNITS.len() - 1 {
        val /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{bytes} B")
    } else {
        format!("{val:.1} {}", UNITS[idx])
    }
}

// -----------------------------------------------------------------------
// GUI — eframe::App with a simple radio-button tab layout.
// -----------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Pool,
    Disk,
    Wal,
    Objects,
    Tags,
    Ontology,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Tab::Pool => "Pool Overview",
            Tab::Disk => "Disk Detail",
            Tab::Wal => "WAL Browser",
            Tab::Objects => "Object Browser",
            Tab::Tags => "Tag Index",
            Tab::Ontology => "Ontology",
        }
    }
}

struct AnalyzerApp {
    snap: PoolSnapshot,
    pool_path: String,
    tab: Tab,
    /// Disk index currently selected on the Disk tab.
    selected_disk: usize,
    /// Selected WAL entry index (for payload pane).
    selected_wal: Option<usize>,
    /// Selected object index (for assertion pane).
    selected_object: Option<usize>,
}

impl AnalyzerApp {
    fn new(snap: PoolSnapshot, pool_path: String) -> Self {
        Self {
            snap,
            pool_path,
            tab: Tab::Pool,
            selected_disk: 0,
            selected_wal: None,
            selected_object: None,
        }
    }
}

impl eframe::App for AnalyzerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Mímisbrunnr Analyzer");
                ui.separator();
                ui.label(&self.pool_path);
                ui.separator();
                ui.label(format!(
                    "node:{} | {} disks | {} objects | {} tags",
                    self.snap.node_id,
                    self.snap.pool_status.disk_count,
                    self.snap.total_objects,
                    self.snap.total_tags
                ));
            });
            ui.horizontal(|ui| {
                for tab in [
                    Tab::Pool,
                    Tab::Disk,
                    Tab::Wal,
                    Tab::Objects,
                    Tab::Tags,
                    Tab::Ontology,
                ] {
                    ui.selectable_value(&mut self.tab, tab, tab.label());
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Pool => self.draw_pool_tab(ui),
            Tab::Disk => self.draw_disk_tab(ui),
            Tab::Wal => self.draw_wal_tab(ui),
            Tab::Objects => self.draw_objects_tab(ui),
            Tab::Tags => self.draw_tags_tab(ui),
            Tab::Ontology => self.draw_ontology_tab(ui),
        });
    }
}

impl AnalyzerApp {
    fn draw_pool_tab(&self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Pool Overview");
            ui.separator();

            ui.label(format!("node_id: {}", self.snap.node_id));
            ui.label(format!(
                "Capacity: {} used / {}",
                format_bytes(self.snap.pool_status.total_used),
                format_bytes(self.snap.pool_status.total_capacity)
            ));
            ui.label(format!(
                "Objects: {} | Tags: {} | Oplog: {} entries",
                self.snap.total_objects,
                self.snap.total_tags,
                self.snap.oplog.len(),
            ));

            ui.add_space(8.0);
            ui.strong("Disks");
            for d in &self.snap.disks {
                ui.label(format!(
                    "  disk{} {:?} ({}, {}) — {} of {} used [{}]",
                    d.disk_id,
                    d.media_type,
                    d.tier.name(),
                    d.path.display(),
                    format_bytes(d.used_bytes),
                    format_bytes(d.capacity_bytes),
                    d.state,
                ));
            }

            ui.add_space(8.0);
            ui.strong("Tier breakdown");
            for (tier, b) in &self.snap.pool_status.by_tier {
                ui.label(format!(
                    "  {}: {} disks, capacity {}, used {}",
                    tier.name(),
                    b.disk_count,
                    format_bytes(b.capacity_bytes),
                    format_bytes(b.used_bytes),
                ));
            }

            ui.add_space(8.0);
            ui.strong("Recent oplog (newest first)");
            for e in self.snap.oplog.iter().take(32) {
                ui.label(
                    egui::RichText::new(format!("lsn={:>5} {}", e.lsn, e.op_summary))
                        .monospace()
                        .small(),
                );
            }
        });
    }

    fn draw_disk_tab(&mut self, ui: &mut egui::Ui) {
        if self.snap.disks.is_empty() {
            ui.label("No disks configured.");
            return;
        }
        ui.horizontal(|ui| {
            ui.label("Disk:");
            for (idx, d) in self.snap.disks.iter().enumerate() {
                ui.selectable_value(&mut self.selected_disk, idx, format!("disk{}", d.disk_id));
            }
        });
        ui.separator();
        let idx = self.selected_disk.min(self.snap.disks.len() - 1);
        let d = &self.snap.disks[idx];
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading(format!("disk{} ({})", d.disk_id, d.path.display()));
            ui.label(format!(
                "media={:?} tier={} capacity={} used={} state={}",
                d.media_type,
                d.tier.name(),
                format_bytes(d.capacity_bytes),
                format_bytes(d.used_bytes),
                d.state,
            ));
            ui.add_space(8.0);

            if let Some(sb) = &d.superblock {
                ui.strong("Superblock");
                draw_kv(ui, "fs_uuid", hex16(&sb.fs_uuid));
                draw_kv(ui, "node_id", sb.node_id.to_string());
                draw_kv(ui, "disk_id", sb.disk_id.to_string());
                draw_kv(ui, "media_type", format!("{:?}", sb.media_type));
                draw_kv(ui, "tier", sb.tier.name());
                draw_kv(ui, "device_capacity", format_bytes(sb.device_capacity));
                draw_kv(ui, "creation_ns", sb.creation_timestamp_ns.to_string());
                draw_kv(ui, "last_mount_ns", sb.last_mount_timestamp_ns.to_string());
                draw_kv(ui, "mount_count", sb.mount_count.to_string());
                draw_kv(ui, "bucket_size_log2", sb.bucket_size_log2.to_string());
                draw_kv(
                    ui,
                    "btree_node_size_log2",
                    sb.btree_node_size_log2.to_string(),
                );
                draw_kv(
                    ui,
                    "wal_offset/size",
                    format!("{:#x} / {}", sb.wal_offset, format_bytes(sb.wal_size)),
                );
                draw_kv(
                    ui,
                    "index_zone",
                    format!(
                        "@{:#x} len {}",
                        sb.index_zone_offset,
                        format_bytes(sb.index_zone_length)
                    ),
                );
                draw_kv(
                    ui,
                    "metadata_zone",
                    format!(
                        "@{:#x} len {}",
                        sb.metadata_zone_offset,
                        format_bytes(sb.metadata_zone_length)
                    ),
                );
                draw_kv(
                    ui,
                    "blob_zone",
                    format!(
                        "@{:#x} len {}",
                        sb.blob_zone_offset,
                        format_bytes(sb.blob_zone_length)
                    ),
                );
                draw_kv(ui, "fs_format_version", sb.fs_format_version.to_string());
            } else {
                ui.weak("Superblock view not available for this disk.");
            }

            ui.add_space(8.0);
            if let Some(rp) = &d.root_pointer {
                ui.strong("Active RootPointer");
                draw_kv(ui, "seq", rp.seq.to_string());
                draw_kv(ui, "lsn", rp.lsn.to_string());
                draw_kv(ui, "flags", format!("{:#x}", rp.flags));
                ui.add_space(4.0);
                ui.label("B+ tree root slots (disk:block@gen):");
                for (name, value) in &rp.roots {
                    ui.label(
                        egui::RichText::new(format!("  {name:<24} {value}"))
                            .monospace()
                            .small(),
                    );
                }
            }

            ui.add_space(8.0);
            ui.strong("WAL ring (primary disk)");
            let h = &self.snap.wal_header;
            draw_kv(ui, "next_lsn", h.next_lsn.to_string());
            draw_kv(ui, "write_cursor", format!("{:#x}", h.write_cursor));
            draw_kv(ui, "read_cursor", format!("{:#x}", h.read_cursor));
            draw_kv(
                ui,
                "used_bytes",
                format!(
                    "{} / {} ({}%)",
                    format_bytes(h.used_bytes),
                    format_bytes(h.data_capacity),
                    wal_used_percent(h)
                ),
            );
            draw_kv(
                ui,
                "last_checkpoint_lsn",
                h.last_checkpoint_lsn.to_string(),
            );
        });
    }

    fn draw_wal_tab(&mut self, ui: &mut egui::Ui) {
        if self.snap.wal_entries.is_empty() {
            ui.label("WAL ring is empty.");
            return;
        }
        let available = ui.available_size();
        ui.horizontal(|ui| {
            ui.allocate_ui(egui::vec2(available.x * 0.45, available.y), |ui| {
                ui.strong("Recent WAL entries (newest first)");
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("wal_list")
                    .show(ui, |ui| {
                        for (idx, e) in self.snap.wal_entries.iter().enumerate() {
                            let label = format!(
                                "lsn={:>5}  {:<22} {} B  ts={}",
                                e.lsn, e.op_kind_name, e.payload_length, e.timestamp_ns
                            );
                            let selected = self.selected_wal == Some(idx);
                            if ui
                                .selectable_label(
                                    selected,
                                    egui::RichText::new(label).monospace().small(),
                                )
                                .clicked()
                            {
                                self.selected_wal = Some(idx);
                            }
                        }
                    });
            });
            ui.separator();
            ui.allocate_ui(egui::vec2(available.x * 0.55, available.y), |ui| {
                ui.strong("Decoded payload");
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("wal_payload")
                    .show(ui, |ui| match self.selected_wal {
                        Some(i) if i < self.snap.wal_entries.len() => {
                            let e = &self.snap.wal_entries[i];
                            ui.label(
                                egui::RichText::new(format!("lsn={} kind={}", e.lsn, e.op_kind_name))
                                    .strong(),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(&e.payload_preview)
                                    .monospace()
                                    .small(),
                            );
                        }
                        _ => {
                            ui.weak("Click an entry to see its CBOR-decoded payload.");
                        }
                    });
            });
        });
    }

    fn draw_objects_tab(&mut self, ui: &mut egui::Ui) {
        if self.snap.objects.is_empty() {
            ui.label("No objects in this pool.");
            return;
        }
        let available = ui.available_size();
        ui.horizontal(|ui| {
            ui.allocate_ui(egui::vec2(available.x * 0.5, available.y), |ui| {
                ui.strong("Objects (sorted by id)");
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("objs_list")
                    .show(ui, |ui| {
                        for (idx, o) in self.snap.objects.iter().enumerate() {
                            let mut text = egui::RichText::new(format!(
                                "#{:>5}  state={:?}  blob={}  tags={}",
                                o.oid.local_seq(),
                                o.state,
                                format_bytes(o.blob_length),
                                o.direct_tags.len()
                            ))
                            .monospace()
                            .small();
                            if o.state != ObjectState::Active {
                                text = text.strikethrough();
                            }
                            if ui
                                .selectable_label(self.selected_object == Some(idx), text)
                                .clicked()
                            {
                                self.selected_object = Some(idx);
                            }
                        }
                    });
            });
            ui.separator();
            ui.allocate_ui(egui::vec2(available.x * 0.5, available.y), |ui| {
                ui.strong("Detail");
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("obj_detail")
                    .show(ui, |ui| match self.selected_object {
                        Some(i) if i < self.snap.objects.len() => {
                            let o = &self.snap.objects[i];
                            draw_kv(ui, "oid", format!("{}", o.oid));
                            draw_kv(ui, "state", format!("{:?}", o.state));
                            draw_kv(ui, "compression", format!("{:?}", o.compression));
                            draw_kv(ui, "generation", o.generation.to_string());
                            draw_kv(ui, "blob_length", format_bytes(o.blob_length));
                            draw_kv(ui, "stored_size", format_bytes(o.stored_size));
                            ui.add_space(4.0);
                            ui.strong("Direct tags");
                            for t in &o.direct_tags {
                                ui.label(format!("  {} = {}", t, self.tag_name(*t)));
                            }
                            ui.strong("Materialized tags");
                            for t in &o.materialized_tags {
                                ui.label(format!("  {} = {}", t, self.tag_name(*t)));
                            }
                            ui.strong("Attributes");
                            for (k, v) in &o.attrs {
                                ui.label(format!("  {k} = {v}"));
                            }
                            ui.add_space(4.0);
                            ui.weak("(location info — not yet wired)");
                        }
                        _ => {
                            ui.weak("Click an object to inspect its assertions.");
                        }
                    });
            });
        });
    }

    fn tag_name(&self, raw: u32) -> &str {
        self.snap
            .tags
            .iter()
            .find(|t| t.id.raw() == raw)
            .map(|t| t.name.as_str())
            .unwrap_or("?")
    }

    fn draw_tags_tab(&self, ui: &mut egui::Ui) {
        ui.strong("Tags (sorted by cardinality)");
        ui.separator();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for t in &self.snap.tags {
                ui.collapsing(
                    format!("{} ({})", t.name, t.object_count),
                    |ui| {
                        ui.label(format!("id: {}", t.id.raw()));
                        ui.label(format!("semantics: {}", t.semantics));
                        if !t.member_preview.is_empty() {
                            ui.label("member preview:");
                            for m in &t.member_preview {
                                ui.label(
                                    egui::RichText::new(format!("  {m:#x}"))
                                        .monospace()
                                        .small(),
                                );
                            }
                        }
                    },
                );
            }
        });
    }

    fn draw_ontology_tab(&self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.strong("Installed modules");
            ui.separator();
            for m in &self.snap.modules {
                ui.label(format!(
                    "  {} v{} ({}): {} tags, {} implications",
                    m.id, m.version, m.name, m.installed_tags, m.installed_implications,
                ));
            }
            if self.snap.modules.is_empty() {
                ui.weak("No modules installed.");
            }
            ui.add_space(8.0);
            ui.strong("Implication DAG (adjacency)");
            ui.separator();
            // Build a sorted adjacency list by `from` tag.
            let mut adj: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
            for (from, to) in &self.snap.implications {
                adj.entry(*from).or_default().push(*to);
            }
            if adj.is_empty() {
                ui.weak("No implications declared.");
            }
            for (from, tos) in &adj {
                let from_name = self.tag_name(*from);
                let target: Vec<String> = tos
                    .iter()
                    .map(|t| format!("{} ({t})", self.tag_name(*t)))
                    .collect();
                ui.label(
                    egui::RichText::new(format!(
                        "  {from_name} ({from}) → {}",
                        target.join(", ")
                    ))
                    .monospace()
                    .small(),
                );
            }
        });
    }
}

fn draw_kv(ui: &mut egui::Ui, key: &str, value: impl AsRef<str>) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("{key:>22}: ")).weak().small());
        ui.label(
            egui::RichText::new(value.as_ref())
                .monospace()
                .small(),
        );
    });
}

fn hex16(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// -----------------------------------------------------------------------
// main()
// -----------------------------------------------------------------------

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let cli = Cli::parse();

    let engine = open_pool_read_only(&cli.pool)?;
    let snap = build_snapshot(&engine);
    let pool_path = cli.pool.display().to_string();

    if cli.dump {
        print!("{}", render_dump(&snap));
        return Ok(());
    }
    if cli.json {
        print!("{}", render_json(&snap));
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title("Mímisbrunnr Analyzer"),
        ..Default::default()
    };
    eframe::run_native(
        "Mímisbrunnr Analyzer",
        options,
        Box::new(|_cc| Ok(Box::new(AnalyzerApp::new(snap, pool_path)))),
    )
    .map_err(|e| -> Box<dyn std::error::Error> { format!("eframe: {e}").into() })?;
    Ok(())
}

/// Open a pool read-only via [`DiskEngine::open_read_only`]. The primary
/// device, the WAL ring, and every mutation method on `DiskEngine` all reject
/// writes — the analyze tool itself never tries.
fn open_pool_read_only(config_path: &Path) -> Result<DiskEngine, Box<dyn std::error::Error>> {
    Ok(DiskEngine::open_read_only(config_path)?)
}

// Silence unused-import warning when `OBJECT_RECORD_SIZE` constant is added
// to the imports later. Keeps `meta::ObjectRecord` referenced explicitly.
#[allow(dead_code)]
fn _record_size_proof() -> usize {
    std::mem::size_of::<ObjectRecord>()
}

// -----------------------------------------------------------------------
// Tests — snapshot builder + dump formatter only. GUI is compile-only.
// -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use mimisbrunnr::pool::DiskConfigEntry;
    use tempfile::TempDir;

    fn fresh_pool(tmp: &TempDir) -> (PathBuf, DiskEngine) {
        let toml_path = tmp.path().join("pool.toml");
        let cfg = PoolConfig {
            node_id: 1,
            disks: vec![DiskConfigEntry {
                id: 1,
                path: tmp.path().join("disk0.img"),
                media_type: MediaType::Ssd,
                tier: StorageTier::Hot,
                capacity_bytes: 64 * 1024 * 1024,
            }],
        };
        let engine = DiskEngine::create(cfg, toml_path.clone()).expect("create pool");
        (toml_path, engine)
    }

    #[test]
    fn snapshot_empty_pool_has_one_disk() {
        let tmp = TempDir::new().unwrap();
        let (_, engine) = fresh_pool(&tmp);
        let snap = build_snapshot(&engine);
        assert_eq!(snap.disks.len(), 1);
        assert_eq!(snap.total_objects, 0);
        assert_eq!(snap.total_tags, 0);
        assert_eq!(snap.node_id, 1);
    }

    #[test]
    fn snapshot_after_create_and_tag() {
        let tmp = TempDir::new().unwrap();
        let (_, mut engine) = fresh_pool(&tmp);
        let oid1 = engine.create_object().unwrap();
        let oid2 = engine.create_object().unwrap();
        let tag = engine.engine.register_tag("demo");
        engine.add_tag(oid1, tag).unwrap();
        engine.add_tag(oid2, tag).unwrap();

        let snap = build_snapshot(&engine);
        assert_eq!(snap.total_objects, 2);
        assert!(snap.total_tags >= 1);
        // The tag with most members (demo) sorts first.
        assert_eq!(snap.tags[0].name, "demo");
        assert_eq!(snap.tags[0].object_count, 2);
        assert_eq!(snap.objects.len(), 2);
        // Oplog should record both creations + both tag adds (some entries
        // may be evicted but for two ops this is fine).
        assert!(snap.oplog.len() >= 4);
        // WAL has at least one entry.
        assert!(snap.wal_header.next_lsn > 1);
        assert!(!snap.wal_entries.is_empty());
    }

    #[test]
    fn render_dump_contains_disk_count() {
        let tmp = TempDir::new().unwrap();
        let (_, engine) = fresh_pool(&tmp);
        let snap = build_snapshot(&engine);
        let text = render_dump(&snap);
        assert!(!text.is_empty());
        assert!(text.contains("disks:"));
        assert!(text.contains("1 ")); // disk_count = 1 in the formatted line
        assert!(text.contains("Mímisbrunnr Pool Overview"));
    }

    #[test]
    fn render_json_is_well_formed_ish() {
        let tmp = TempDir::new().unwrap();
        let (_, engine) = fresh_pool(&tmp);
        let snap = build_snapshot(&engine);
        let text = render_json(&snap);
        assert!(text.starts_with('{'));
        assert!(text.trim_end().ends_with('}'));
        assert!(text.contains("\"node_id\": 1"));
        assert!(text.contains("\"disk_count\": 1"));
    }

    #[test]
    fn wal_entry_view_decodes_create_object() {
        let tmp = TempDir::new().unwrap();
        let (_, mut engine) = fresh_pool(&tmp);
        let _oid = engine.create_object().unwrap();
        let snap = build_snapshot(&engine);
        let create_entry = snap
            .wal_entries
            .iter()
            .find(|e| e.op_kind_name == "CreateObject")
            .expect("expected at least one CreateObject in WAL");
        assert!(!create_entry.payload_preview.is_empty());
    }

    #[test]
    fn snapshot_includes_root_pointer_view() {
        let tmp = TempDir::new().unwrap();
        let (_, engine) = fresh_pool(&tmp);
        let snap = build_snapshot(&engine);
        let primary = &snap.disks[0];
        let rp = primary
            .root_pointer
            .as_ref()
            .expect("primary disk has a root pointer");
        // 17 named slots in our view (subset of the 24 in the spec).
        assert!(!rp.roots.is_empty());
    }
}

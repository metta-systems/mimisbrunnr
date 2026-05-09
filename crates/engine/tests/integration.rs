//! Phase 6 integration tests for `mimisbrunnr-engine`.
//!
//! Forward-index inspection in tests goes through `assertions_of(oid)` per
//! object or the public `ForwardIndex::iter()` (added in phase R0).

use std::path::PathBuf;

use mimisbrunnr_engine::{
    DiskEngine, Engine, EngineError, HybridClock, OpKind, OpLog, project_op, replay_wal_op,
};
use mimisbrunnr_ontology::{OntologyModule, TagDefinition, TagSemantics};
use mimisbrunnr_pool::{DiskConfigEntry, PoolConfig};
use mimisbrunnr_types::{
    Assertion, ChangeInterest, MediaType, ObjectId, Query, StorageTier, TagId, TagOrigin, Value,
    WatchEvent,
};
use mimisbrunnr_wal::{WalOp, WalOpKind};
use mimisbrunnr_watch::Retention;
use tempfile::TempDir;

// ----------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------

fn label(name: &str) -> TagDefinition {
    TagDefinition {
        id: TagId::new(0),
        name: name.into(),
        semantics: TagSemantics::Label,
        implies: vec![],
        storage: None,
    }
}

fn small_pool(tmp: &TempDir) -> (PoolConfig, PathBuf) {
    let p0 = tmp.path().join("disk0.img");
    let cfg = PoolConfig {
        node_id: 1,
        disks: vec![DiskConfigEntry {
            id: 0,
            path: p0,
            media_type: MediaType::NVMe,
            tier: StorageTier::Hot,
            capacity_bytes: 32 * 1024 * 1024,
        }],
    };
    let cfg_path = tmp.path().join("pool.toml");
    (cfg, cfg_path)
}

fn install_car_vehicle(engine: &mut Engine) -> (TagId, TagId) {
    let module = OntologyModule {
        id: "core".into(),
        version: "0.1".into(),
        name: "core".into(),
        tags: vec![label("vehicle"), label("car")],
        implications: vec![("car".into(), "vehicle".into())],
        relations: vec![],
    };
    engine.install_ontology_module(module).unwrap();
    let car = engine.resolve_tag_name("car").unwrap();
    let vehicle = engine.resolve_tag_name("vehicle").unwrap();
    (car, vehicle)
}

// ----------------------------------------------------------------------
// In-memory Engine
// ----------------------------------------------------------------------

#[test]
fn ensure_oid_exists_is_idempotent_and_advances_counter() {
    let mut e = Engine::new(1);
    let pinned = ObjectId::from_parts(1, 100);
    e.ensure_oid_exists(pinned).unwrap();
    assert!(e.object_table.get(pinned.to_u64()).is_some());

    // Repeat call: still ok, no duplicate insert.
    e.ensure_oid_exists(pinned).unwrap();
    assert_eq!(e.object_count(), 1);

    // The next freshly-created oid must skip past the pinned local seq.
    let fresh = e.create_object();
    assert!(fresh.local_seq() > pinned.local_seq());
}

#[test]
fn create_object_increments_local_seq_and_yields_unique_oids() {
    let mut e = Engine::new(7);
    let a = e.create_object();
    let b = e.create_object();
    let c = e.create_object();
    assert_ne!(a, b);
    assert_ne!(b, c);
    assert_eq!(a.node_id(), 7);
    assert_eq!(b.local_seq(), a.local_seq() + 1);
    assert_eq!(c.local_seq(), b.local_seq() + 1);
    assert_eq!(e.object_count(), 3);
}

#[test]
fn add_tag_materialises_ontology_implications() {
    let mut e = Engine::new(1);
    let (car, vehicle) = install_car_vehicle(&mut e);
    let oid = e.create_object();
    e.add_tag(oid, car).unwrap();
    let asserts = e.forward_index.assertions_of(oid).to_vec();

    // Both `car` (Direct) and `vehicle` (Materialized) appear.
    let direct: Vec<_> = asserts
        .iter()
        .filter(|(_, o)| *o == TagOrigin::Direct)
        .filter_map(|(a, _)| match a {
            Assertion::Tag(t) => Some(*t),
            _ => None,
        })
        .collect();
    let materialised: Vec<_> = asserts
        .iter()
        .filter(|(_, o)| *o == TagOrigin::Materialized)
        .filter_map(|(a, _)| match a {
            Assertion::Tag(t) => Some(*t),
            _ => None,
        })
        .collect();
    assert_eq!(direct, vec![car]);
    assert_eq!(materialised, vec![vehicle]);
    // Both tag bitmaps now contain the object.
    assert!(e.tag_index.contains(car, oid));
    assert!(e.tag_index.contains(vehicle, oid));
}

#[test]
fn remove_tag_reverses_materialisation_when_unique_source() {
    let mut e = Engine::new(1);
    let (car, vehicle) = install_car_vehicle(&mut e);
    let oid = e.create_object();
    e.add_tag(oid, car).unwrap();
    assert!(e.tag_index.contains(vehicle, oid));
    e.remove_tag(oid, car).unwrap();
    let asserts = e.forward_index.assertions_of(oid).to_vec();
    assert!(asserts.is_empty(), "expected empty assertions, got {asserts:?}");
    assert!(!e.tag_index.contains(car, oid));
    assert!(!e.tag_index.contains(vehicle, oid));
}

#[test]
fn remove_tag_keeps_materialised_when_another_direct_implies_it() {
    let mut e = Engine::new(1);
    // car → vehicle, truck → vehicle. tag both; remove car; vehicle stays.
    let module = OntologyModule {
        id: "vehicles".into(),
        version: "0.1".into(),
        name: "vehicles".into(),
        tags: vec![label("vehicle"), label("car"), label("truck")],
        implications: vec![("car".into(), "vehicle".into()), ("truck".into(), "vehicle".into())],
        relations: vec![],
    };
    e.install_ontology_module(module).unwrap();
    let car = e.resolve_tag_name("car").unwrap();
    let truck = e.resolve_tag_name("truck").unwrap();
    let vehicle = e.resolve_tag_name("vehicle").unwrap();

    let oid = e.create_object();
    e.add_tag(oid, car).unwrap();
    e.add_tag(oid, truck).unwrap();
    assert!(e.tag_index.contains(vehicle, oid));
    e.remove_tag(oid, car).unwrap();
    // truck still implies vehicle.
    assert!(e.tag_index.contains(vehicle, oid));
    assert!(e.tag_index.contains(truck, oid));
    assert!(!e.tag_index.contains(car, oid));
}

#[test]
fn set_attr_round_trips_through_kv_and_range() {
    let mut e = Engine::new(1);
    let oid = e.create_object();
    let key = e.register_tag("year");
    e.set_attr(oid, key, Value::Int(2024)).unwrap();
    let bm = e.kv_index.lookup(key, &Value::Int(2024));
    assert_eq!(bm.len(), 1);
    let scan = e
        .range_index
        .range_scan(key, &Value::Int(2020), &Value::Int(2025));
    assert_eq!(scan.len(), 1);
}

#[test]
fn remove_attr_by_value_hash_scrubs_indices() {
    let mut e = Engine::new(1);
    let oid = e.create_object();
    let key = e.register_tag("name");
    let v = Value::Text("Aphex".into());
    e.set_attr(oid, key, v.clone()).unwrap();
    let h = mimisbrunnr_engine::engine_value_hash(&v);
    e.remove_attr(oid, key, h).unwrap();
    assert_eq!(e.kv_index.lookup(key, &v).len(), 0);
}

#[test]
fn query_has_tag_returns_correct_bitmap() {
    let mut e = Engine::new(1);
    let tag = e.register_tag("blue");
    let oids: Vec<_> = (0..5).map(|_| e.create_object()).collect();
    for o in &oids {
        e.add_tag(*o, tag).unwrap();
    }
    let bm = e.query(&Query::HasTag(tag)).unwrap();
    assert_eq!(bm.len(), 5);
}

#[test]
fn query_isa_matches_descendant_tags() {
    let mut e = Engine::new(1);
    let (car, vehicle) = install_car_vehicle(&mut e);
    let o1 = e.create_object();
    let o2 = e.create_object();
    e.add_tag(o1, car).unwrap();
    // Direct `vehicle` tag.
    e.add_tag(o2, vehicle).unwrap();
    let bm = e.query(&Query::IsA(vehicle)).unwrap();
    assert_eq!(bm.len(), 2);
}

#[test]
fn delete_object_tombstones_and_strips_indices() {
    let mut e = Engine::new(1);
    let oid = e.create_object();
    let tag = e.register_tag("doomed");
    e.add_tag(oid, tag).unwrap();
    assert!(e.tag_index.contains(tag, oid));
    e.delete_object(oid).unwrap();
    assert!(!e.tag_index.contains(tag, oid));
    let bm = e.query(&Query::HasTag(tag)).unwrap();
    assert!(bm.is_empty());
    assert_eq!(e.object_count(), 1); // tombstone keeps the slot
}

#[test]
fn subscribe_then_add_tag_drains_entered_event() {
    let mut e = Engine::new(1);
    let tag = e.register_tag("watched");
    let id = e
        .subscribe(
            "ws".into(),
            Query::HasTag(tag),
            ChangeInterest::ALL,
            Retention::default(),
        )
        .unwrap();
    let oid = e.create_object();
    e.add_tag(oid, tag).unwrap();
    let events = e.subscriptions.drain(id);
    assert!(events.iter().any(|ev| matches!(ev, WatchEvent::Entered { .. })));
}

#[test]
fn add_relation_records_forward_assertion() {
    let mut e = Engine::new(1);
    let pred = e.register_tag("links");
    let src = e.create_object();
    let dst = e.create_object();
    e.add_relation(src, pred, dst).unwrap();
    let asserts = e.forward_index.assertions_of(src).to_vec();
    assert!(asserts.iter().any(|(a, _)| matches!(
        a,
        Assertion::Relation { predicate: p, target: t } if *p == pred && *t == dst
    )));
}

#[test]
fn remove_relation_drops_assertion() {
    let mut e = Engine::new(1);
    let pred = e.register_tag("links");
    let src = e.create_object();
    let dst = e.create_object();
    e.add_relation(src, pred, dst).unwrap();
    e.remove_relation(src, pred, dst).unwrap();
    let asserts = e.forward_index.assertions_of(src).to_vec();
    assert!(!asserts.iter().any(|(a, _)| matches!(a, Assertion::Relation { .. })));
}

#[test]
fn write_blob_updates_object_record_metadata() {
    let mut e = Engine::new(1);
    let oid = e.create_object();
    let res = e.write_blob(oid, b"hello world").unwrap();
    assert_eq!(res.original_size, 11);
    let rec = e.object_table.get(oid.to_u64()).unwrap();
    assert_eq!({ rec.blob_length }, 11);
    assert_eq!({ rec.stored_size }, 11);
    assert_eq!(rec.content_hash, res.content_hash);
}

#[test]
fn delete_unknown_object_errors() {
    let mut e = Engine::new(1);
    let ghost = ObjectId::from_parts(1, 999);
    assert!(matches!(
        e.delete_object(ghost),
        Err(EngineError::ObjectNotFound(_))
    ));
}

#[test]
fn add_tag_unknown_object_errors() {
    let mut e = Engine::new(1);
    let ghost = ObjectId::from_parts(1, 999);
    let tag = e.register_tag("x");
    assert!(matches!(
        e.add_tag(ghost, tag),
        Err(EngineError::ObjectNotFound(_))
    ));
}

#[test]
fn install_ontology_module_advances_tag_id() {
    let mut e = Engine::new(1);
    install_car_vehicle(&mut e);
    let extra = e.register_tag("brand_new");
    // car=1, vehicle=2 (or vice versa); register_tag picks whichever is next.
    assert!(extra.raw() >= 3);
}

// ----------------------------------------------------------------------
// OpLog / clock
// ----------------------------------------------------------------------

#[test]
fn oplog_since_returns_every_entry() {
    let mut log = OpLog::new(0);
    let oid = ObjectId::from_parts(0, 1);
    for i in 1..=5u64 {
        log.record(
            OpKind::CreateObject { oid },
            i,
            mimisbrunnr_types::HybridTimestamp::new(i as i64, 0, 0),
        );
    }
    let collected: Vec<_> = log.since(0).collect();
    assert_eq!(collected.len(), 5);
}

#[test]
fn oplog_evicts_oldest_when_full() {
    let mut log = OpLog::new(2);
    let oid = ObjectId::from_parts(0, 1);
    for i in 1..=5u64 {
        log.record(
            OpKind::CreateObject { oid },
            i,
            mimisbrunnr_types::HybridTimestamp::new(i as i64, 0, 0),
        );
    }
    assert_eq!(log.len(), 2);
    let lsns: Vec<_> = log.since(0).map(|e| e.lsn).collect();
    assert_eq!(lsns, vec![4, 5]);
}

#[test]
fn hybrid_clock_now_is_monotonic() {
    let mut clk = HybridClock::new(1);
    let mut prev = clk.now();
    for _ in 0..500 {
        let next = clk.now();
        assert!(next >= prev);
        prev = next;
    }
}

// ----------------------------------------------------------------------
// WAL projection round-trip
// ----------------------------------------------------------------------

#[test]
fn wal_projection_round_trip_add_tag() {
    let mut e = Engine::new(1);
    let (car, vehicle) = install_car_vehicle(&mut e);
    let oid = e.create_object();
    let op = OpKind::AddTag { oid, tag: car };
    let wal_op = project_op(&op);
    let (kind, payload) = wal_op.encode().unwrap();
    assert_eq!(kind, WalOpKind::AddTag);

    // Decode and verify variant.
    let decoded = WalOp::decode(kind, &payload).unwrap();
    assert!(matches!(decoded, WalOp::AddTag(_)));

    // Replay against a fresh engine. The replay needs the ontology to do
    // the same materialisation, so we install it on the fresh engine first.
    let mut fresh = Engine::new(1);
    install_car_vehicle(&mut fresh);
    // Replay a CreateObject so the oid exists.
    let create_op = project_op(&OpKind::CreateObject { oid });
    let (cknd, cpl) = create_op.encode().unwrap();
    replay_wal_op(&mut fresh, cknd, &cpl, 1).unwrap();
    replay_wal_op(&mut fresh, kind, &payload, 2).unwrap();

    assert!(fresh.tag_index.contains(car, oid));
    assert!(fresh.tag_index.contains(vehicle, oid));
}

#[test]
fn wal_replay_idempotent_under_lsn() {
    let mut e = Engine::new(1);
    let oid = ObjectId::from_parts(1, 42);
    let op = OpKind::CreateObject { oid };
    let wal_op = project_op(&op);
    let (kind, payload) = wal_op.encode().unwrap();
    replay_wal_op(&mut e, kind, &payload, 1).unwrap();
    let snapshot_count = e.object_count();

    // Re-applying the same LSN must error and not change state.
    let err = replay_wal_op(&mut e, kind, &payload, 1).unwrap_err();
    assert!(matches!(err, EngineError::LsnAlreadyApplied(1)));
    assert_eq!(e.object_count(), snapshot_count);
}

// ----------------------------------------------------------------------
// DiskEngine round-trip
// ----------------------------------------------------------------------

#[test]
fn disk_engine_create_open_round_trip_preserves_tag() {
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);

    let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
    let tag = de.engine.register_tag("persistent");
    let oid = de.create_object().unwrap();
    de.add_tag(oid, tag).unwrap();
    de.commit().unwrap();
    drop(de);

    let de2 = DiskEngine::open(&cfg_path).unwrap();
    let tag2 = de2.engine.resolve_tag_name("persistent").unwrap();
    assert_eq!(tag2, tag);
    let bm = de2.engine.query(&Query::HasTag(tag2)).unwrap();
    assert_eq!(bm.len(), 1);
}

#[test]
fn disk_engine_read_blob_round_trips_in_memory() {
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);
    let mut de = DiskEngine::create(cfg, cfg_path).unwrap();

    let oid = de.create_object().unwrap();
    let res = de.write_blob(oid, b"phase-r0 payload").unwrap();
    assert_eq!(res.original_size, 16);

    let got = de.read_blob(oid).unwrap();
    assert!(got.is_some(), "expected blob present after write_blob");
    assert_eq!(got.unwrap().len(), res.stored_size as usize);

    // Unknown oid → None.
    let ghost = ObjectId::from_parts(1, 9999);
    assert!(de.read_blob(ghost).unwrap().is_none());
}

#[test]
fn disk_engine_replays_wal_past_last_checkpoint() {
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);

    let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
    let tag = de.engine.register_tag("checkpoint-tag");
    let oid1 = de.create_object().unwrap();
    de.add_tag(oid1, tag).unwrap();
    de.commit().unwrap();

    // Add more state without committing — the WAL should carry these.
    let oid2 = de.create_object().unwrap();
    de.add_tag(oid2, tag).unwrap();
    drop(de);

    let de2 = DiskEngine::open(&cfg_path).unwrap();
    let tag2 = de2.engine.resolve_tag_name("checkpoint-tag").unwrap();
    let bm = de2.engine.query(&Query::HasTag(tag2)).unwrap();
    // Both objects survived: one via index blob, one via WAL replay.
    assert_eq!(bm.len(), 2, "expected both oids; got {bm:?}");
}

#[test]
fn disk_engine_add_remove_disk_proxies_to_pool() {
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);
    let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
    let initial = de.status().pool.disk_count;

    let new_disk = DiskConfigEntry {
        id: 1,
        path: tmp.path().join("disk1.img"),
        media_type: MediaType::Ssd,
        tier: StorageTier::Warm,
        capacity_bytes: 16 * 1024 * 1024,
    };
    de.add_disk(new_disk).unwrap();
    assert_eq!(de.status().pool.disk_count, initial + 1);

    de.remove_disk(1).unwrap();
    // remove_disk only marks Draining; disk count stays.
    assert_eq!(de.status().pool.disk_count, initial + 1);
}

#[test]
fn disk_engine_status_carries_index_sizes() {
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);
    let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
    let tag = de.engine.register_tag("s");
    let oid = de.create_object().unwrap();
    de.add_tag(oid, tag).unwrap();

    let st = de.status();
    assert_eq!(st.object_count, 1);
    assert!(st.tag_count >= 1);
    assert!(st.wal_next_lsn >= 2); // CreateObject + AddTag
}

#[test]
fn snapshot_create_returns_not_implemented() {
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);
    let mut de = DiskEngine::create(cfg, cfg_path).unwrap();
    let err = de.snapshot_create(Some("v1".into())).unwrap_err();
    assert!(matches!(err, EngineError::NotImplemented(_)));
}

#[test]
fn reconcile_step_returns_zero() {
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);
    let mut de = DiskEngine::create(cfg, cfg_path).unwrap();
    assert_eq!(de.reconcile_step(100).unwrap(), 0);
}

#[test]
fn disk_engine_persists_attr_through_round_trip() {
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);

    let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
    let key = de.engine.register_tag("year");
    let oid = de.create_object().unwrap();
    de.set_attr(oid, key, Value::Int(2024)).unwrap();
    de.commit().unwrap();
    drop(de);

    let de2 = DiskEngine::open(&cfg_path).unwrap();
    let key2 = de2.engine.resolve_tag_name("year").unwrap();
    let bm = de2.engine.kv_index.lookup(key2, &Value::Int(2024));
    assert_eq!(bm.len(), 1);
}

#[test]
fn engine_subscriptions_field_exposes_pending_events() {
    let mut e = Engine::new(1);
    let tag = e.register_tag("watch");
    let id = e
        .subscribe(
            "x".into(),
            Query::HasTag(tag),
            ChangeInterest::ALL,
            Retention::default(),
        )
        .unwrap();
    let oid = e.create_object();
    e.add_tag(oid, tag).unwrap();
    // Inverted index has a slot for the watched tag.
    assert!(e.subscriptions.tag_to_subs.contains_key(&tag));
    let drained = e.subscriptions.drain(id);
    assert!(!drained.is_empty());
}

#[test]
fn register_tag_is_idempotent_by_name() {
    let mut e = Engine::new(1);
    let a = e.register_tag("dup");
    let b = e.register_tag("dup");
    assert_eq!(a, b);
}

// ----------------------------------------------------------------------
// DiskEngine::open_read_only
// ----------------------------------------------------------------------

#[test]
fn open_read_only_round_trip_preserves_state() {
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);

    let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
    let tag = de.engine.register_tag("ro_tag");
    let oid = de.create_object().unwrap();
    de.add_tag(oid, tag).unwrap();
    de.commit().unwrap();
    drop(de);

    let de_ro = DiskEngine::open_read_only(&cfg_path).unwrap();
    assert!(de_ro.is_read_only());
    let tag2 = de_ro.engine.resolve_tag_name("ro_tag").unwrap();
    assert_eq!(tag2, tag);
    let bm = de_ro.engine.query(&Query::HasTag(tag2)).unwrap();
    assert_eq!(bm.len(), 1);
}

#[test]
fn open_read_only_rejects_add_tag() {
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);

    {
        let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
        de.commit().unwrap();
    }

    let mut de_ro = DiskEngine::open_read_only(&cfg_path).unwrap();
    let tag = de_ro.engine.register_tag("ghost");
    let ghost = ObjectId::from_parts(1, 0);
    let err = de_ro.add_tag(ghost, tag).unwrap_err();
    assert!(matches!(err, EngineError::ReadOnly));
}

#[test]
fn open_read_only_rejects_commit() {
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);

    {
        let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
        de.commit().unwrap();
    }

    let mut de_ro = DiskEngine::open_read_only(&cfg_path).unwrap();
    let err = de_ro.commit().unwrap_err();
    assert!(matches!(err, EngineError::ReadOnly));
}

#[test]
fn open_read_only_twice_in_succession_succeeds() {
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);

    {
        let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
        de.commit().unwrap();
    }

    let de1 = DiskEngine::open_read_only(&cfg_path).unwrap();
    drop(de1);
    let de2 = DiskEngine::open_read_only(&cfg_path).unwrap();
    assert!(de2.is_read_only());
}

#[test]
fn read_only_then_mutating_open_sees_subsequent_changes() {
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);

    {
        let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
        de.commit().unwrap();
    }

    // Read-only open observes the empty post-commit state.
    let de_ro1 = DiskEngine::open_read_only(&cfg_path).unwrap();
    assert!(de_ro1.engine.resolve_tag_name("fresh").is_none());
    drop(de_ro1);

    // Sequential RW open adds a tag and commits.
    {
        let mut de = DiskEngine::open(&cfg_path).unwrap();
        let tag = de.engine.register_tag("fresh");
        let oid = de.create_object().unwrap();
        de.add_tag(oid, tag).unwrap();
        de.commit().unwrap();
    }

    // Re-RO-open sees the new tag.
    let de_ro2 = DiskEngine::open_read_only(&cfg_path).unwrap();
    assert!(de_ro2.engine.resolve_tag_name("fresh").is_some());
}

// ----------------------------------------------------------------------
// R1b-1: per-region index persistence (ChunkIndex / KvIndex)
// ----------------------------------------------------------------------

#[test]
fn fresh_pool_open_before_first_commit_sees_empty_migrated_indices() {
    // Sanity: the migrated-index regions are zero-filled at format time, so
    // an `open` against a freshly-created pool that hasn't been committed
    // must succeed and see empty ChunkIndex / KvIndex states.
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);
    {
        let _de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
        // Drop without committing.
    }
    let de = DiskEngine::open(&cfg_path).unwrap();
    assert_eq!(de.engine.chunk_index.chunk_count(), 0);
    assert_eq!(de.engine.kv_index.entry_count(), 0);
}

// ----------------------------------------------------------------------
// R1b-2: per-region index persistence (Forward / Tag / Range)
// ----------------------------------------------------------------------

#[test]
fn fresh_pool_open_before_first_commit_sees_empty_r1b2_indices() {
    // All five migrated regions are zero-filled at format time, so an
    // `open` against a freshly-created pool must succeed and see empty
    // forward / tag / range states alongside the R1b-1 indices.
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);
    {
        let _de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
        // Drop without committing.
    }
    let de = DiskEngine::open(&cfg_path).unwrap();
    assert_eq!(de.engine.forward_index.object_count(), 0);
    assert_eq!(de.engine.tag_index.tag_count(), 0);
    assert_eq!(de.engine.range_index.entry_count(), 0);
}

#[test]
fn r1b2_forward_tag_range_persist_across_commit_drop_open() {
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);

    let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();

    // Tag mutations exercise tag_index + forward_index; set_attr exercises
    // kv_index + range_index + forward_index.
    let watched = de.engine.register_tag("watched");
    let year = de.engine.register_tag("year");
    let oid1 = de.create_object().unwrap();
    let oid2 = de.create_object().unwrap();
    de.add_tag(oid1, watched).unwrap();
    de.add_tag(oid2, watched).unwrap();
    de.set_attr(oid1, year, Value::Int(2024)).unwrap();
    de.set_attr(oid2, year, Value::Int(2025)).unwrap();

    let pre_forward = de.engine.forward_index.object_count();
    let pre_tags = de.engine.tag_index.tag_count();
    let pre_range = de.engine.range_index.entry_count();
    de.commit().unwrap();
    drop(de);

    let de2 = DiskEngine::open(&cfg_path).unwrap();
    // Forward index round-trip.
    assert_eq!(de2.engine.forward_index.object_count(), pre_forward);
    assert_eq!(de2.engine.forward_index.assertions_of(oid1).len(), 2);
    assert_eq!(de2.engine.forward_index.assertions_of(oid2).len(), 2);

    // Tag index round-trip.
    assert_eq!(de2.engine.tag_index.tag_count(), pre_tags);
    let watched2 = de2.engine.resolve_tag_name("watched").unwrap();
    assert!(de2.engine.tag_index.contains(watched2, oid1));
    assert!(de2.engine.tag_index.contains(watched2, oid2));

    // Range index round-trip — equality + half-open scan.
    assert_eq!(de2.engine.range_index.entry_count(), pre_range);
    let year2 = de2.engine.resolve_tag_name("year").unwrap();
    let scan = de2
        .engine
        .range_index
        .range_scan(year2, &Value::Int(2024), &Value::Int(2026));
    assert_eq!(scan.len(), 2);
}

#[test]
fn all_five_migrated_indices_round_trip_together() {
    // ChunkIndex + KvIndex + ForwardIndex + TagIndex + RangeIndex —
    // commit, drop, re-open; every one of them must survive.
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);
    let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();

    // ChunkIndex (private mutation surface).
    let blob_ref = mimisbrunnr_storage::BlobRef {
        disk_id: 0,
        _pad: 0,
        block_no: 4242,
        length: 4096,
    };
    de.engine.chunk_index.insert_or_bump([0xc1u8; 32], blob_ref, 4096);

    // ForwardIndex / TagIndex / KvIndex / RangeIndex via the public surface.
    let red = de.engine.register_tag("red");
    let year = de.engine.register_tag("year");
    let oid = de.create_object().unwrap();
    de.add_tag(oid, red).unwrap();
    de.set_attr(oid, year, Value::Int(2026)).unwrap();
    de.commit().unwrap();
    drop(de);

    let de2 = DiskEngine::open(&cfg_path).unwrap();
    // Chunk.
    assert_eq!(de2.engine.chunk_index.chunk_count(), 1);
    // Forward.
    let asserts = de2.engine.forward_index.assertions_of(oid);
    assert!(asserts.iter().any(|(a, _)| matches!(a, Assertion::Tag(_))));
    assert!(
        asserts
            .iter()
            .any(|(a, _)| matches!(a, Assertion::Attr { .. }))
    );
    // Tag.
    let red2 = de2.engine.resolve_tag_name("red").unwrap();
    assert!(de2.engine.tag_index.contains(red2, oid));
    // KV (via kv_index).
    let year2 = de2.engine.resolve_tag_name("year").unwrap();
    assert_eq!(de2.engine.kv_index.lookup(year2, &Value::Int(2026)).len(), 1);
    // Range.
    let bm = de2
        .engine
        .range_index
        .range_scan(year2, &Value::Int(2025), &Value::Int(2027));
    assert_eq!(bm.len(), 1);
}

#[test]
fn migrated_indices_persist_across_commit_drop_open() {
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);

    let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
    // ChunkIndex inserts go through the in-memory engine directly — no
    // public DiskEngine surface for them in R1b-1, but the persistence
    // round-trip must still cover them.
    let blob_ref = mimisbrunnr_storage::BlobRef {
        disk_id: 0,
        _pad: 0,
        block_no: 1234,
        length: 4096,
    };
    de.engine.chunk_index.insert_or_bump([0xa1u8; 32], blob_ref, 4096);
    de.engine.chunk_index.insert_or_bump([0xa1u8; 32], blob_ref, 4096); // ref_count = 2
    de.engine.chunk_index.insert_or_bump([0xb2u8; 32], blob_ref, 4096);

    // KvIndex inserts go through the public set_attr path.
    let key = de.engine.register_tag("yr");
    let oid = de.create_object().unwrap();
    de.set_attr(oid, key, Value::Int(2026)).unwrap();
    de.commit().unwrap();
    drop(de);

    let de2 = DiskEngine::open(&cfg_path).unwrap();
    // ChunkIndex round-trip.
    assert_eq!(de2.engine.chunk_index.chunk_count(), 2);
    assert_eq!(de2.engine.chunk_index.ref_count(&[0xa1u8; 32]), 2);
    assert_eq!(de2.engine.chunk_index.ref_count(&[0xb2u8; 32]), 1);

    // KvIndex round-trip via set_attr.
    let key2 = de2.engine.resolve_tag_name("yr").unwrap();
    let bm = de2.engine.kv_index.lookup(key2, &Value::Int(2026));
    assert_eq!(bm.len(), 1);
}

// ----------------------------------------------------------------------
// R1b-3: per-region object table / location table / backpointer table
// ----------------------------------------------------------------------

#[test]
fn fresh_pool_open_before_first_commit_sees_empty_r1b3_tables() {
    // Every region (including the three R1b-3 ones plus the two new
    // R1b-4 ones) is zero-filled at format time, so an `open` against
    // a freshly-created pool must succeed and see empty
    // object/location/backpointer/bucket-alloc/freespace-LRU states.
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);
    {
        let _de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
        // Drop without committing.
    }
    let de = DiskEngine::open(&cfg_path).unwrap();
    assert_eq!(de.engine.object_table.len(), 0);
    assert_eq!(de.engine.location_table.len(), 0);
    assert_eq!(de.engine.backpointer_table.len(), 0);
    assert_eq!(de.engine.bucket_alloc.len(), 0);
    assert_eq!(de.engine.freespace_lru.len(), 0);
}

#[test]
fn r1b3_object_table_persists_across_commit_drop_open() {
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);

    let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
    let watched = de.engine.register_tag("watched");
    let oid1 = de.create_object().unwrap();
    let oid2 = de.create_object().unwrap();
    de.add_tag(oid1, watched).unwrap();
    de.add_tag(oid2, watched).unwrap();

    let pre_objects = de.engine.object_table.len();
    assert_eq!(pre_objects, 2);
    de.commit().unwrap();
    drop(de);

    let de2 = DiskEngine::open(&cfg_path).unwrap();
    assert_eq!(de2.engine.object_table.len(), pre_objects);
    let r1 = de2.engine.object_table.get(oid1.to_u64()).expect("oid1 missing");
    assert_eq!({ r1.id }, oid1.to_u64());
    let r2 = de2.engine.object_table.get(oid2.to_u64()).expect("oid2 missing");
    assert_eq!({ r2.id }, oid2.to_u64());
}

#[test]
fn r1b3_location_table_persists_across_commit_drop_open() {
    use mimisbrunnr_meta::{ObjectLocation, ReplicaRef};

    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);
    let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
    let oid = de.create_object().unwrap();

    // Inject a location directly (engine doesn't drive blob-zone allocation
    // yet — that's R4). The persistence round-trip is what we care about.
    let r = ReplicaRef {
        disk_id: 0,
        sector_offset: 5,
        bucket_no: 42,
    };
    let loc = ObjectLocation::new(0x4000, &[r]).unwrap();
    de.engine.location_table.insert(oid.to_u64(), loc);
    de.commit().unwrap();
    drop(de);

    let de2 = DiskEngine::open(&cfg_path).unwrap();
    assert_eq!(de2.engine.location_table.len(), 1);
    let got = de2
        .engine
        .location_table
        .get(oid.to_u64())
        .expect("location missing after round-trip");
    assert_eq!(got.header.replica_count, 1);
    assert_eq!({ got.header.extent_length }, 0x4000);
    assert_eq!(got.active_replicas()[0].bucket_no, 42);
}

#[test]
fn r1b3_backpointer_table_persists_across_commit_drop_open() {
    use mimisbrunnr_meta::{BackpointerKey, BackpointerValue, OwnerKind};

    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);
    let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();

    // Engine doesn't drive backpointer mutations yet (R4); insert a few
    // manually so we can prove the round-trip works end-to-end.
    let entries: Vec<(BackpointerKey, BackpointerValue)> = (0u32..5)
        .map(|i| {
            let k = BackpointerKey::new(0, 100 + i, (i as u16) * 2);
            let v = BackpointerValue {
                owner_kind: OwnerKind::BlobExtent as u8,
                length_sectors: 4,
                bucket_gen: 100 + i,
                owner_key: [(i as u8); 16],
                ..BackpointerValue::default()
            };
            (k, v)
        })
        .collect();
    for (k, v) in &entries {
        de.engine.backpointer_table.insert(*k, *v);
    }
    assert_eq!(de.engine.backpointer_table.len(), 5);

    de.commit().unwrap();
    drop(de);

    let de2 = DiskEngine::open(&cfg_path).unwrap();
    assert_eq!(de2.engine.backpointer_table.len(), 5);
    for (k, v) in &entries {
        let got = de2
            .engine
            .backpointer_table
            .get(k)
            .expect("backpointer missing after round-trip");
        assert_eq!(got.owner_kind, v.owner_kind);
        assert_eq!({ got.bucket_gen }, { v.bucket_gen });
        assert_eq!(got.owner_key, v.owner_key);
    }
    // Bucket-prefix scan still works.
    let scan: Vec<_> = de2.engine.backpointer_table.range_in_bucket(0, 102).collect();
    assert_eq!(scan.len(), 1);
}

#[test]
fn all_ten_migrated_structures_round_trip_together() {
    use mimisbrunnr_meta::{BackpointerKey, BackpointerValue, OwnerKind};

    // ChunkIndex + KvIndex + ForwardIndex + TagIndex + RangeIndex +
    // ObjectTable + LocationTable + Ontology + Subscriptions +
    // BackpointerTable — commit, drop, re-open; each must survive.
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);
    let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();

    // ChunkIndex.
    let blob_ref = mimisbrunnr_storage::BlobRef {
        disk_id: 0,
        _pad: 0,
        block_no: 5050,
        length: 4096,
    };
    de.engine.chunk_index.insert_or_bump([0xc7u8; 32], blob_ref, 4096);

    // ObjectTable + ForwardIndex + TagIndex + KvIndex + RangeIndex via
    // the public surface (create_object inserts into object_table; add_tag
    // touches forward + tag; set_attr touches forward + kv + range).
    let red = de.engine.register_tag("red");
    let year = de.engine.register_tag("year");
    let oid = de.create_object().unwrap();
    de.add_tag(oid, red).unwrap();
    de.set_attr(oid, year, Value::Int(2026)).unwrap();

    // BackpointerTable (manual insert — engine doesn't drive yet).
    let bk = BackpointerKey::new(0, 7, 0);
    let bv = BackpointerValue {
        owner_kind: OwnerKind::BlobExtent as u8,
        length_sectors: 1,
        bucket_gen: 7,
        owner_key: [9u8; 16],
        ..BackpointerValue::default()
    };
    de.engine.backpointer_table.insert(bk, bv);

    // Subscriptions: register one so the subscriptions region is non-empty.
    let _sub = de
        .subscribe(
            "watch-red".into(),
            Query::HasTag(red),
            ChangeInterest::ALL,
            Retention::default(),
        )
        .unwrap();

    de.commit().unwrap();
    drop(de);

    let de2 = DiskEngine::open(&cfg_path).unwrap();
    // ChunkIndex.
    assert_eq!(de2.engine.chunk_index.chunk_count(), 1);
    // ObjectTable.
    assert!(de2.engine.object_table.get(oid.to_u64()).is_some());
    // ForwardIndex.
    let asserts = de2.engine.forward_index.assertions_of(oid);
    assert!(asserts.iter().any(|(a, _)| matches!(a, Assertion::Tag(_))));
    assert!(
        asserts
            .iter()
            .any(|(a, _)| matches!(a, Assertion::Attr { .. }))
    );
    // TagIndex.
    let red2 = de2.engine.resolve_tag_name("red").unwrap();
    assert!(de2.engine.tag_index.contains(red2, oid));
    // KvIndex.
    let year2 = de2.engine.resolve_tag_name("year").unwrap();
    assert_eq!(de2.engine.kv_index.lookup(year2, &Value::Int(2026)).len(), 1);
    // RangeIndex.
    let bm = de2
        .engine
        .range_index
        .range_scan(year2, &Value::Int(2025), &Value::Int(2027));
    assert_eq!(bm.len(), 1);
    // BackpointerTable.
    assert_eq!(de2.engine.backpointer_table.len(), 1);
    assert!(de2.engine.backpointer_table.get(&bk).is_some());
    // Ontology + Subscriptions: both seeded above; resolve_tag_name above
    // already confirmed ontology, and the subscription count must be 1.
    assert_eq!(de2.engine.subscriptions.subscriptions.len(), 1);
    // LocationTable: empty in this test (no manual insert) — confirms
    // that an empty dedicated region round-trips correctly.
    assert_eq!(de2.engine.location_table.len(), 0);
}

// ----------------------------------------------------------------------
// R1b-4: bucket alloc table + freespace LRU
// ----------------------------------------------------------------------

#[test]
fn r1b4_bucket_alloc_table_persists_across_commit_drop_open() {
    use mimisbrunnr_storage::{
        BUCKET_FLAG_NEEDS_DISCARD, BUCKET_FLAG_PINNED_BY_SNAPSHOT, BucketAllocEntry,
        BucketDataType,
    };

    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);
    let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();

    // Engine doesn't drive bucket allocation yet (Theme F / H);
    // inject directly so we can prove the round-trip works.
    let entries: Vec<(u32, BucketAllocEntry)> = (0u32..16)
        .map(|i| {
            (
                i,
                BucketAllocEntry {
                    generation: i + 1,
                    data_type: BucketDataType::Blob as u8,
                    flags: if i % 4 == 0 {
                        BUCKET_FLAG_NEEDS_DISCARD
                    } else {
                        0
                    },
                    dirty_sectors: (i & 0xff) as u16,
                    last_modify_lsn: i as u64 * 100,
                },
            )
        })
        .collect();
    for (k, v) in &entries {
        de.engine.bucket_alloc.insert(*k, *v);
    }
    de.engine.bucket_alloc.insert(
        9999,
        BucketAllocEntry {
            generation: 42,
            data_type: BucketDataType::Metadata as u8,
            flags: BUCKET_FLAG_PINNED_BY_SNAPSHOT,
            dirty_sectors: 17,
            last_modify_lsn: 999_999,
        },
    );
    let pre_len = de.engine.bucket_alloc.len();
    assert_eq!(pre_len, 17);

    de.commit().unwrap();
    drop(de);

    let de2 = DiskEngine::open(&cfg_path).unwrap();
    assert_eq!(de2.engine.bucket_alloc.len(), pre_len);
    for (k, v) in &entries {
        let got = de2
            .engine
            .bucket_alloc
            .get(*k)
            .expect("bucket missing after round-trip");
        assert_eq!({ got.generation }, { v.generation });
        assert_eq!(got.data_type, v.data_type);
        assert_eq!(got.flags, v.flags);
        assert_eq!({ got.last_modify_lsn }, { v.last_modify_lsn });
    }
    let pinned = de2.engine.bucket_alloc.get(9999).expect("pinned missing");
    assert_eq!(pinned.flags, BUCKET_FLAG_PINNED_BY_SNAPSHOT);
}

#[test]
fn r1b4_freespace_lru_persists_across_commit_drop_open() {
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);
    let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();

    // Inject entries spread across bands 0 (allocator fast path), 100
    // (mid-fragmented), and 255 (saturated).
    for bucket in 0u32..10 {
        de.engine.freespace_lru.insert(0, 0, bucket);
    }
    for bucket in 100u32..105 {
        de.engine.freespace_lru.insert(100, 0, bucket);
    }
    for bucket in 200u32..203 {
        de.engine.freespace_lru.insert(255, 0, bucket);
    }
    // Cross-disk entry — proves the disk_id field round-trips through
    // the wire even though the in-memory mirror is pool-scoped.
    de.engine.freespace_lru.insert(50, 7, 42);
    let pre_len = de.engine.freespace_lru.len();
    assert_eq!(pre_len, 19);

    de.commit().unwrap();
    drop(de);

    let de2 = DiskEngine::open(&cfg_path).unwrap();
    assert_eq!(de2.engine.freespace_lru.len(), pre_len);
    for bucket in 0u32..10 {
        assert!(de2.engine.freespace_lru.contains(0, 0, bucket));
    }
    for bucket in 100u32..105 {
        assert!(de2.engine.freespace_lru.contains(100, 0, bucket));
    }
    for bucket in 200u32..203 {
        assert!(de2.engine.freespace_lru.contains(255, 0, bucket));
    }
    assert!(de2.engine.freespace_lru.contains(50, 7, 42));

    // Banded scan still works after round-trip — allocator fast path.
    let band0_count = de2.engine.freespace_lru.iter_band(0).count();
    assert_eq!(band0_count, 10);
    let band255_count = de2.engine.freespace_lru.iter_band(255).count();
    assert_eq!(band255_count, 3);
}

#[test]
fn all_twelve_migrated_structures_round_trip_together() {
    use mimisbrunnr_meta::{BackpointerKey, BackpointerValue, OwnerKind};
    use mimisbrunnr_storage::{BucketAllocEntry, BucketDataType};

    // ChunkIndex + KvIndex + ForwardIndex + TagIndex + RangeIndex +
    // ObjectTable + LocationTable + Ontology + Subscriptions +
    // BackpointerTable + BucketAllocTable + FreespaceLru — commit,
    // drop, re-open; each must survive.
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);
    let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();

    // ChunkIndex.
    let blob_ref = mimisbrunnr_storage::BlobRef {
        disk_id: 0,
        _pad: 0,
        block_no: 5050,
        length: 4096,
    };
    de.engine.chunk_index.insert_or_bump([0xc7u8; 32], blob_ref, 4096);

    // ObjectTable + ForwardIndex + TagIndex + KvIndex + RangeIndex via
    // the public surface.
    let red = de.engine.register_tag("red");
    let year = de.engine.register_tag("year");
    let oid = de.create_object().unwrap();
    de.add_tag(oid, red).unwrap();
    de.set_attr(oid, year, Value::Int(2026)).unwrap();

    // BackpointerTable.
    let bk = BackpointerKey::new(0, 7, 0);
    let bv = BackpointerValue {
        owner_kind: OwnerKind::BlobExtent as u8,
        length_sectors: 1,
        bucket_gen: 7,
        owner_key: [9u8; 16],
        ..BackpointerValue::default()
    };
    de.engine.backpointer_table.insert(bk, bv);

    // BucketAllocTable (R1b-4).
    de.engine.bucket_alloc.insert(
        13,
        BucketAllocEntry {
            generation: 13,
            data_type: BucketDataType::Blob as u8,
            flags: 0,
            dirty_sectors: 1,
            last_modify_lsn: 13,
        },
    );

    // FreespaceLru (R1b-4).
    de.engine.freespace_lru.insert(0, 0, 13);
    de.engine.freespace_lru.insert(255, 0, 99);

    // Subscriptions.
    let _sub = de
        .subscribe(
            "watch-red".into(),
            Query::HasTag(red),
            ChangeInterest::ALL,
            Retention::default(),
        )
        .unwrap();

    de.commit().unwrap();
    drop(de);

    let de2 = DiskEngine::open(&cfg_path).unwrap();
    assert_eq!(de2.engine.chunk_index.chunk_count(), 1);
    assert!(de2.engine.object_table.get(oid.to_u64()).is_some());
    let asserts = de2.engine.forward_index.assertions_of(oid);
    assert!(asserts.iter().any(|(a, _)| matches!(a, Assertion::Tag(_))));
    let red2 = de2.engine.resolve_tag_name("red").unwrap();
    assert!(de2.engine.tag_index.contains(red2, oid));
    let year2 = de2.engine.resolve_tag_name("year").unwrap();
    assert_eq!(de2.engine.kv_index.lookup(year2, &Value::Int(2026)).len(), 1);
    let bm = de2
        .engine
        .range_index
        .range_scan(year2, &Value::Int(2025), &Value::Int(2027));
    assert_eq!(bm.len(), 1);
    assert_eq!(de2.engine.backpointer_table.len(), 1);
    assert!(de2.engine.backpointer_table.get(&bk).is_some());
    // R1b-4: bucket alloc + freespace LRU.
    assert_eq!(de2.engine.bucket_alloc.len(), 1);
    let ba = de2.engine.bucket_alloc.get(13).unwrap();
    assert_eq!({ ba.generation }, 13);
    assert_eq!(de2.engine.freespace_lru.len(), 2);
    assert!(de2.engine.freespace_lru.contains(0, 0, 13));
    assert!(de2.engine.freespace_lru.contains(255, 0, 99));
    assert_eq!(de2.engine.subscriptions.subscriptions.len(), 1);
}

// ----- R1c-D1: RootPointer.*_root slots populated -----

#[test]
fn rootpointer_slots_populated_post_create() {
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);
    let de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();

    // The 10 R1b-migrated trees must all have non-zero BlockRefs.
    let r = de.superblock.active_root_pointer();
    assert!({ r.chunk_index_root.block_no } != 0, "chunk_index_root");
    assert!({ r.kv_index_root.block_no } != 0, "kv_index_root");
    assert!({ r.forward_index_root.block_no } != 0, "forward_index_root");
    assert!({ r.tag_index_root.block_no } != 0, "tag_index_root");
    assert!({ r.range_index_root.block_no } != 0, "range_index_root");
    assert!({ r.object_table_root.block_no } != 0, "object_table_root");
    assert!({ r.location_table_root.block_no } != 0, "location_table_root");
    assert!({ r.ontology_root.block_no } != 0, "ontology_root");
    assert!({ r.subscriptions_root.block_no } != 0, "subscriptions_root");
    assert!({ r.backpointer_root.block_no } != 0, "backpointer_root");

    // Each slot's generation is set (1 under R1c-D1).
    assert_eq!({ r.chunk_index_root.generation }, 1);
    assert_eq!({ r.kv_index_root.generation }, 1);

    // Slots that don't yet have engine-side trees (cluster_peers,
    // disks_overflow, placement_rules, reconcile_*, snapshot_chain,
    // value_spill, *_history) stay zero — they're populated by later
    // phases (R6/R7/R12).
    assert_eq!({ r.cluster_peers_root.block_no }, 0);
    assert_eq!({ r.snapshot_chain_root.block_no }, 0);
    assert_eq!({ r.reconcile_work_root.block_no }, 0);
}

#[test]
fn rootpointer_slots_stable_across_commit() {
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);
    let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
    let pre_commit = *de.superblock.active_root_pointer();

    // Mutate, commit.
    let _ = de.engine.register_tag("year");
    de.commit().unwrap();
    let post_commit = *de.superblock.active_root_pointer();

    // R1c-D1 keeps slots stable (COW reallocation is Tier 3 D3).
    assert_eq!(
        { pre_commit.chunk_index_root.block_no },
        { post_commit.chunk_index_root.block_no },
        "chunk_index_root drifted across commit",
    );
    assert_eq!(
        { pre_commit.ontology_root.block_no },
        { post_commit.ontology_root.block_no },
        "ontology_root drifted across commit",
    );
    // seq + lsn advance.
    assert!({ post_commit.seq } > { pre_commit.seq });
    assert!({ post_commit.lsn } >= { pre_commit.lsn });
}

// ----- R1c-A2: object_table / location_table use native positional radix leaves -----

#[test]
fn object_table_on_disk_uses_native_leaf_layout() {
    // Insert one record, commit, reopen, then peek the on-disk leaf
    // bytes. The header at the active object_table_root must carry
    // BlockKind == ObjectTable and the magic "MIMB".
    use mimisbrunnr_meta::OBJECT_LEAF_BITMAP_OFFSET;
    use mimisbrunnr_storage::{BLOCK_PREAMBLE_MAGIC_BTREE, BlockDevice, BtreeKind};

    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);
    let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
    let oid = de.create_object().unwrap();
    de.commit().unwrap();

    let root = de.superblock.active_root_pointer();
    let block_no = { root.object_table_root.block_no };
    let offset = block_no as u64 * 4096;
    drop(de);

    // Peek the leaf header.
    let dev = mimisbrunnr_storage::FileBlockDevice::open_read_only(
        small_pool_disk_path(&tmp),
    )
    .unwrap();
    let mut header_bytes = [0u8; 8];
    dev.read_at(offset, &mut header_bytes).unwrap();
    let preamble: &mimisbrunnr_storage::BlockPreamble =
        bytemuck::from_bytes(&header_bytes);
    assert_eq!({ preamble.magic }, BLOCK_PREAMBLE_MAGIC_BTREE);
    assert_eq!({ preamble.kind }, BtreeKind::ObjectTable as u16);

    // Bitmap byte at offset 64 must have at least one bit set (the
    // single object we created lives in some leaf slot).
    let mut bitmap_byte = [0u8; 1];
    dev.read_at(offset + OBJECT_LEAF_BITMAP_OFFSET as u64, &mut bitmap_byte)
        .unwrap();
    let oid_local = oid.to_u64() & ((1u64 << 48) - 1);
    let slot = oid_local as usize % 2044;
    if slot < 8 {
        assert_ne!(bitmap_byte[0] & (1 << slot), 0);
    } else {
        // Read a different bitmap byte for the test-occupied slot.
        let byte_idx = slot / 8;
        let mut buf = [0u8; 1];
        dev.read_at(
            offset + OBJECT_LEAF_BITMAP_OFFSET as u64 + byte_idx as u64,
            &mut buf,
        )
        .unwrap();
        assert_ne!(buf[0] & (1 << (slot % 8)), 0);
    }
}

fn small_pool_disk_path(tmp: &TempDir) -> std::path::PathBuf {
    tmp.path().join("disk0.img")
}

#[test]
fn kv_index_on_disk_uses_native_extendible_hash_layout() {
    // KvIndex now writes a 4 KiB KvDirectory at the kv_index_root
    // offset; the directory's BlockHeader carries kind=KvHashDirectory
    // and magic="MIMR". One bucket and one bitmap page should follow.
    use mimisbrunnr_storage::{BLOCK_PREAMBLE_MAGIC_BLOCK, BlockDevice, BlockKind};
    use mimisbrunnr_types::Value;

    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);
    let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
    let key = de.engine.register_tag("year");
    let oid = de.create_object().unwrap();
    de.set_attr(oid, key, Value::Int(2025)).unwrap();
    de.commit().unwrap();
    let root_block_no = { de.superblock.active_root_pointer().kv_index_root.block_no };
    let kv_dir_offset = root_block_no as u64 * 4096;
    drop(de);

    let dev = mimisbrunnr_storage::FileBlockDevice::open_read_only(
        small_pool_disk_path(&tmp),
    )
    .unwrap();

    // Directory header.
    let mut header = [0u8; 8];
    dev.read_at(kv_dir_offset, &mut header).unwrap();
    let preamble: &mimisbrunnr_storage::BlockPreamble =
        bytemuck::from_bytes(&header);
    assert_eq!({ preamble.magic }, BLOCK_PREAMBLE_MAGIC_BLOCK);
    assert_eq!({ preamble.kind }, BlockKind::KvHashDirectory as u16);

    // Bucket 0 sits at kv_dir_offset + 4 KiB.
    let bucket_offset = kv_dir_offset + 4096;
    let mut bheader = [0u8; 8];
    dev.read_at(bucket_offset, &mut bheader).unwrap();
    let bpreamble: &mimisbrunnr_storage::BlockPreamble =
        bytemuck::from_bytes(&bheader);
    assert_eq!({ bpreamble.kind }, BlockKind::KvHashBucket as u16);

    // Bitmap page sits at kv_dir_offset + 2 × 4 KiB.
    let bitmap_offset = kv_dir_offset + 8192;
    let mut bmh = [0u8; 8];
    dev.read_at(bitmap_offset, &mut bmh).unwrap();
    let bm_preamble: &mimisbrunnr_storage::BlockPreamble =
        bytemuck::from_bytes(&bmh);
    assert_eq!({ bm_preamble.kind }, BlockKind::TagBitmapPage as u16);
}

#[test]
fn rootpointer_survives_open_cycle() {
    let tmp = TempDir::new().unwrap();
    let (cfg, cfg_path) = small_pool(&tmp);
    let mut de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
    let _ = de.engine.register_tag("color");
    de.commit().unwrap();
    let pre_drop = *de.superblock.active_root_pointer();
    drop(de);

    let de2 = DiskEngine::open(&cfg_path).unwrap();
    let post_open = *de2.superblock.active_root_pointer();

    // Block_no preserved across drop/open.
    assert_eq!(
        { pre_drop.tag_index_root.block_no },
        { post_open.tag_index_root.block_no },
    );
    // The tag we registered is still resolvable, proving the load
    // path went through the RootPointer slot.
    assert!(de2.engine.resolve_tag_name("color").is_some());
}

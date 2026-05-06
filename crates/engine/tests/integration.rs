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

//! Integration tests for `mimir`'s command handlers — driven directly
//! against an in-process `DiskEngine` rooted in a `tempfile::TempDir`.
//!
//! These tests bypass the clap layer (which is just argument routing) and
//! instead exercise the `pub fn run_*` handlers in `mimir::commands`. The
//! library + bin split in `Cargo.toml` makes that possible.

use std::io::Cursor;
use std::path::PathBuf;

use mimir::{
    commands::{self, CommandError},
    value_parse::ValueKind,
};
use mimisbrunnr::{
    engine::DiskEngine,
    pool::{DiskConfigEntry, PoolConfig},
    types::{MediaType, ObjectId, StorageTier},
};
use tempfile::TempDir;

const DISK_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB

struct Fixture {
    _tmp: TempDir,
    pool_toml: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let disk_path = tmp.path().join("disk0.bin");
        let pool_toml = tmp.path().join("pool.toml");

        let config = PoolConfig {
            node_id: 0,
            disks: vec![DiskConfigEntry {
                id: 1,
                path: disk_path.clone(),
                media_type: MediaType::NVMe,
                tier: StorageTier::Hot,
                capacity_bytes: DISK_BYTES,
            }],
        };

        // `DiskEngine::create` formats every disk in the config and writes
        // pool.toml.
        let mut de = DiskEngine::create(config, pool_toml.clone()).unwrap();
        de.commit().unwrap();

        Self {
            _tmp: tmp,
            pool_toml,
        }
    }

    fn open(&self) -> DiskEngine {
        DiskEngine::open(&self.pool_toml).unwrap()
    }
}

fn capture(s: impl FnOnce(&mut Cursor<Vec<u8>>)) -> String {
    let mut cur = Cursor::new(Vec::new());
    s(&mut cur);
    String::from_utf8(cur.into_inner()).unwrap()
}

fn create_one(engine: &mut DiskEngine) -> ObjectId {
    let mut buf = Cursor::new(Vec::new());
    commands::run_create(engine, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner()).unwrap();
    let line = s.trim();
    parse_oid_str(line)
}

fn parse_oid_str(s: &str) -> ObjectId {
    // engine prints `obj:<hex_node>:<dec_local>`
    let body = s.strip_prefix("obj:").unwrap();
    let (node, local) = body.split_once(':').unwrap();
    let node = u16::from_str_radix(node, 16).unwrap();
    let local: u64 = local.parse().unwrap();
    ObjectId::from_parts(node, local)
}

// =========================================================================
// Object operations.
// =========================================================================

#[test]
fn tag_round_trip_info_lists_tag() {
    let f = Fixture::new();
    let mut engine = f.open();

    let oid = create_one(&mut engine);
    let oid_str = format!("obj:{}:{}", oid.node_id(), oid.local_seq());
    let oid_arg = format!("obj:{}", oid.local_seq()); // node 0 → bare local form ok

    commands::run_tag(
        &mut engine,
        &mut Cursor::new(Vec::new()),
        &oid_arg,
        &["electronic".into(), "ambient".into()],
    )
    .unwrap();

    let info = capture(|w| commands::run_info(&mut engine, w, &oid_str).unwrap());
    assert!(info.contains("tag=electronic"), "{info}");
    assert!(info.contains("tag=ambient"), "{info}");
}

#[test]
fn untag_removes_tag() {
    let f = Fixture::new();
    let mut engine = f.open();
    let oid = create_one(&mut engine);
    let oid_arg = format!("obj:{}", oid.local_seq());

    commands::run_tag(
        &mut engine,
        &mut Cursor::new(Vec::new()),
        &oid_arg,
        &["electronic".into()],
    )
    .unwrap();
    commands::run_untag(
        &mut engine,
        &mut Cursor::new(Vec::new()),
        &oid_arg,
        "electronic",
    )
    .unwrap();

    let info = capture(|w| commands::run_info(&mut engine, w, &oid_arg).unwrap());
    assert!(!info.contains("tag=electronic"), "{info}");
}

#[test]
fn set_attr_int_float_text_each() {
    let f = Fixture::new();
    let mut engine = f.open();
    let oid = create_one(&mut engine);
    let oid_arg = format!("obj:{}", oid.local_seq());

    commands::run_set(
        &mut engine,
        &mut Cursor::new(Vec::new()),
        &oid_arg,
        "year",
        "2024",
        ValueKind::Int,
    )
    .unwrap();
    commands::run_set(
        &mut engine,
        &mut Cursor::new(Vec::new()),
        &oid_arg,
        "rating",
        "4.5",
        ValueKind::Float,
    )
    .unwrap();
    commands::run_set(
        &mut engine,
        &mut Cursor::new(Vec::new()),
        &oid_arg,
        "artist",
        "Aphex Twin",
        ValueKind::Text,
    )
    .unwrap();
    commands::run_set(
        &mut engine,
        &mut Cursor::new(Vec::new()),
        &oid_arg,
        "released_at",
        "ns:1700000000000000000",
        ValueKind::Timestamp,
    )
    .unwrap();

    let info = capture(|w| commands::run_info(&mut engine, w, &oid_arg).unwrap());
    assert!(info.contains("attr=year") && info.contains("value=2024"), "{info}");
    assert!(info.contains("attr=rating") && info.contains("4.5"), "{info}");
    assert!(info.contains("attr=artist") && info.contains("Aphex Twin"), "{info}");
    assert!(info.contains("attr=released_at"), "{info}");
}

#[test]
fn auto_value_kind_falls_back_to_text() {
    let f = Fixture::new();
    let mut engine = f.open();
    let oid = create_one(&mut engine);
    let oid_arg = format!("obj:{}", oid.local_seq());

    commands::run_set(
        &mut engine,
        &mut Cursor::new(Vec::new()),
        &oid_arg,
        "name",
        "Selected Ambient Works",
        ValueKind::Auto,
    )
    .unwrap();
    let info = capture(|w| commands::run_info(&mut engine, w, &oid_arg).unwrap());
    assert!(info.contains("attr=name"), "{info}");
    assert!(info.contains("Selected Ambient Works"), "{info}");
}

// =========================================================================
// Queries.
// =========================================================================

#[test]
fn query_sexpr_returns_matching_oids() {
    let f = Fixture::new();
    let mut engine = f.open();

    let mut oids = Vec::new();
    for _ in 0..3 {
        let oid = create_one(&mut engine);
        let oid_arg = format!("obj:{}", oid.local_seq());
        commands::run_tag(
            &mut engine,
            &mut Cursor::new(Vec::new()),
            &oid_arg,
            &["song".into()],
        )
        .unwrap();
        oids.push(oid);
    }
    // One extra without the tag.
    create_one(&mut engine);

    let out = capture(|w| {
        commands::run_query_sexpr(&mut engine, w, "(tag song)", false).unwrap();
    });
    assert!(out.contains("(3 results)"), "{out}");
    for oid in &oids {
        assert!(out.contains(&format!("{oid}")), "{out}");
    }
}

#[test]
fn query_sql_returns_same_set_as_sexpr() {
    let f = Fixture::new();
    let mut engine = f.open();

    for _ in 0..2 {
        let oid = create_one(&mut engine);
        let oid_arg = format!("obj:{}", oid.local_seq());
        commands::run_tag(
            &mut engine,
            &mut Cursor::new(Vec::new()),
            &oid_arg,
            &["electronic".into()],
        )
        .unwrap();
    }

    let out = capture(|w| {
        commands::run_query_sql(
            &mut engine,
            w,
            "SELECT * FROM objects WHERE tag = 'electronic'",
            false,
        )
        .unwrap();
    });
    assert!(out.contains("(2 rows)"), "{out}");
}

#[test]
fn query_explain_does_not_execute() {
    let f = Fixture::new();
    let mut engine = f.open();

    // Register a tag so the parser succeeds, but no objects carry it.
    let _ = engine.engine.register_tag("ghost");

    let out = capture(|w| {
        commands::run_query_sexpr(&mut engine, w, "(tag ghost)", true).unwrap();
    });
    assert!(out.contains("HasTag"), "{out}");
    // Plan output, not result count.
    assert!(!out.contains("results"), "{out}");
}

#[test]
fn query_sql_explain_renders_plan() {
    let f = Fixture::new();
    let mut engine = f.open();
    let _ = engine.engine.register_tag("song");

    let out = capture(|w| {
        commands::run_query_sql(
            &mut engine,
            w,
            "SELECT * FROM objects WHERE tag = 'song'",
            true,
        )
        .unwrap();
    });
    assert!(out.contains("SQL:"), "{out}");
    assert!(out.contains("HasTag"), "{out}");
}

#[test]
fn explore_returns_facet_counts() {
    let f = Fixture::new();
    let mut engine = f.open();

    for _ in 0..3 {
        let oid = create_one(&mut engine);
        let oid_arg = format!("obj:{}", oid.local_seq());
        commands::run_tag(
            &mut engine,
            &mut Cursor::new(Vec::new()),
            &oid_arg,
            &["song".into(), "electronic".into()],
        )
        .unwrap();
    }
    let out = capture(|w| {
        commands::run_explore(&mut engine, w, "song", 16).unwrap();
    });
    assert!(out.contains("song\t3"), "{out}");
    assert!(out.contains("electronic\t3"), "{out}");
}

// =========================================================================
// Ontology.
// =========================================================================

#[test]
fn ontology_install_and_remove_round_trip() {
    let f = Fixture::new();
    let mut engine = f.open();

    let module_toml = r#"
[module]
id = "test.music"
version = "0.1.0"
name = "Test Music"

[[tags]]
name = "song"
semantics = "label"

[[tags]]
name = "album"
semantics = "label"

[[implications]]
from = "song"
to = "album"
"#;
    let path = f._tmp.path().join("music.toml");
    std::fs::write(&path, module_toml).unwrap();

    let install = capture(|w| {
        commands::run_ontology_install(&mut engine, w, &path).unwrap();
    });
    assert!(install.contains("test.music"), "{install}");
    assert!(install.contains("tags_registered=2"), "{install}");

    let listing = capture(|w| {
        commands::run_ontology_list(&mut engine, w).unwrap();
    });
    assert!(listing.contains("test.music"), "{listing}");

    let remove = capture(|w| {
        commands::run_ontology_remove(&mut engine, w, "test.music").unwrap();
    });
    assert!(remove.contains("tags_dropped=2"), "{remove}");
}

#[test]
fn ontology_orphans_lists_ad_hoc_tags() {
    let f = Fixture::new();
    let mut engine = f.open();

    // Register an ad-hoc tag — it's not part of any installed module.
    let _ = engine.engine.register_tag("freeform");

    let out = capture(|w| {
        commands::run_ontology_orphans(&mut engine, w).unwrap();
    });
    assert!(out.contains("freeform"), "{out}");
}

#[test]
fn ontology_show_prints_definition_and_closure() {
    let f = Fixture::new();
    let mut engine = f.open();

    let module_toml = r#"
[module]
id = "test.x"
version = "0.1.0"
name = "x"

[[tags]]
name = "a"
semantics = "label"

[[tags]]
name = "b"
semantics = "label"

[[implications]]
from = "a"
to = "b"
"#;
    let path = f._tmp.path().join("x.toml");
    std::fs::write(&path, module_toml).unwrap();
    commands::run_ontology_install(&mut engine, &mut Cursor::new(Vec::new()), &path).unwrap();

    let out = capture(|w| {
        commands::run_ontology_show(&mut engine, w, "a").unwrap();
    });
    assert!(out.contains("name=a"), "{out}");
    assert!(out.contains("a\t"), "{out}");
    assert!(out.contains("b\t"), "{out}");
}

// =========================================================================
// Subscriptions.
// =========================================================================

#[test]
fn watch_register_drain_round_trip() {
    let f = Fixture::new();
    let mut engine = f.open();

    let _ = engine.engine.register_tag("song");

    commands::run_watch_register(
        &mut engine,
        &mut Cursor::new(Vec::new()),
        "build-watcher",
        "(tag song)",
    )
    .unwrap();

    // Cause a tag mutation that should fire a hook.
    let oid = create_one(&mut engine);
    let oid_arg = format!("obj:{}", oid.local_seq());
    commands::run_tag(
        &mut engine,
        &mut Cursor::new(Vec::new()),
        &oid_arg,
        &["song".into()],
    )
    .unwrap();

    let drained = capture(|w| {
        commands::run_watch_drain(&mut engine, w, "build-watcher").unwrap();
    });
    assert!(drained.contains("drained="), "{drained}");
    // We should have at least one `Entered` or `TagAdded` line.
    assert!(
        drained.contains("Entered") || drained.contains("TagAdded"),
        "{drained}"
    );
}

#[test]
fn watch_list_then_unsubscribe() {
    let f = Fixture::new();
    let mut engine = f.open();
    let _ = engine.engine.register_tag("song");

    commands::run_watch_register(
        &mut engine,
        &mut Cursor::new(Vec::new()),
        "x",
        "(tag song)",
    )
    .unwrap();

    let listing = capture(|w| commands::run_watch_list(&mut engine, w).unwrap());
    assert!(listing.contains("name=\"x\""), "{listing}");

    commands::run_watch_unsubscribe(&mut engine, &mut Cursor::new(Vec::new()), "x").unwrap();
    let listing2 = capture(|w| commands::run_watch_list(&mut engine, w).unwrap());
    assert!(listing2.contains("subscriptions=0"), "{listing2}");
}

#[test]
fn watch_stream_is_unimplemented() {
    let f = Fixture::new();
    let mut engine = f.open();
    let err =
        commands::run_watch_stream(&mut engine, &mut Cursor::new(Vec::new()), "x").unwrap_err();
    assert!(matches!(err, CommandError::Unimplemented(_)), "{err}");
}

// =========================================================================
// Path projections.
// =========================================================================

#[test]
fn project_create_set_path_and_tree() {
    let f = Fixture::new();
    let mut engine = f.open();

    commands::run_project_create_context(
        &mut engine,
        &mut Cursor::new(Vec::new()),
        "rpi4-sdcard",
    )
    .unwrap();

    let oid = create_one(&mut engine);
    let oid_arg = format!("obj:{}", oid.local_seq());
    commands::run_project_set_path(
        &mut engine,
        &mut Cursor::new(Vec::new()),
        &oid_arg,
        "rpi4-sdcard",
        "boot/kernel8.img",
    )
    .unwrap();

    let tree = capture(|w| {
        commands::run_project_tree(&mut engine, w, "rpi4-sdcard").unwrap();
    });
    assert!(tree.contains("boot/kernel8.img"), "{tree}");
    assert!(tree.contains("entries=1"), "{tree}");

    // The `unix-path` attribute was attached too.
    let info = capture(|w| commands::run_info(&mut engine, w, &oid_arg).unwrap());
    assert!(info.contains("attr=unix-path"), "{info}");
}

#[test]
fn project_list_contexts_after_create() {
    let f = Fixture::new();
    let mut engine = f.open();

    commands::run_project_create_context(&mut engine, &mut Cursor::new(Vec::new()), "alpha")
        .unwrap();
    commands::run_project_create_context(&mut engine, &mut Cursor::new(Vec::new()), "beta")
        .unwrap();

    let listing = capture(|w| {
        commands::run_project_list_contexts(&mut engine, w).unwrap();
    });
    assert!(listing.contains("contexts=2"), "{listing}");
    assert!(listing.contains("name=alpha"), "{listing}");
    assert!(listing.contains("name=beta"), "{listing}");
}

#[test]
fn project_export_directory_form_empty_projection() {
    // Set up a context with no objects registered — the export should
    // succeed and create an (empty) target directory.
    let f = Fixture::new();
    let mut engine = f.open();
    commands::run_project_create_context(
        &mut engine,
        &mut Cursor::new(Vec::new()),
        "exp_ctx",
    )
    .unwrap();
    let target = f._tmp.path().join("export_empty");
    let mut buf = Cursor::new(Vec::new());
    commands::run_project_export(
        &mut engine,
        &mut buf,
        "exp_ctx",
        &target,
        None,
    )
    .unwrap();
    assert!(target.is_dir(), "expected empty export dir to exist");
    let msg = String::from_utf8(buf.into_inner()).unwrap();
    assert!(msg.contains("0 objects"), "{msg}");
}

#[test]
fn project_export_directory_form_writes_files() {
    use mimisbrunnr::unix::PathProjection;
    use std::path::PathBuf;

    let f = Fixture::new();
    let mut engine = f.open();
    commands::run_project_create_context(
        &mut engine,
        &mut Cursor::new(Vec::new()),
        "tree_ctx",
    )
    .unwrap();

    // Resolve the context tag id.
    let canonical = commands::context_tag_name("tree_ctx");
    let context_id = engine
        .engine
        .resolve_tag_name(&canonical)
        .expect("context tag");

    // Replace the projection with a fresh one rooted at /unused (the export
    // target_root supersedes it).
    {
        let projections = &mut engine.engine.path_contexts.projections;
        projections.insert(context_id, PathProjection::new(context_id, PathBuf::from("/unused")));
    }

    // Mint three objects, register their relative paths in the projection,
    // and write blobs.
    let mut oids = Vec::new();
    let payloads: Vec<&[u8]> = vec![b"alpha", b"beta", b"gamma"];
    let rel_paths = ["a.txt", "sub/b.txt", "sub/c.txt"];
    for (i, payload) in payloads.iter().enumerate() {
        let oid = create_one(&mut engine);
        engine.write_blob(oid, payload).unwrap();
        let proj = engine
            .engine
            .path_contexts
            .get_mut(context_id)
            .expect("projection");
        proj.add(oid, rel_paths[i].into()).unwrap();
        oids.push(oid);
    }

    let target = f._tmp.path().join("export_tree");
    let mut buf = Cursor::new(Vec::new());
    commands::run_project_export(
        &mut engine,
        &mut buf,
        "tree_ctx",
        &target,
        None,
    )
    .unwrap();

    let msg = String::from_utf8(buf.into_inner()).unwrap();
    assert!(msg.contains("3 objects"), "{msg}");
    for (i, rel) in rel_paths.iter().enumerate() {
        let written = std::fs::read(target.join(rel)).unwrap();
        assert_eq!(written.as_slice(), payloads[i]);
    }
}

#[test]
fn project_export_directory_form_missing_blob_errors() {
    use mimisbrunnr::unix::PathProjection;
    use std::path::PathBuf;

    let f = Fixture::new();
    let mut engine = f.open();
    commands::run_project_create_context(
        &mut engine,
        &mut Cursor::new(Vec::new()),
        "miss_ctx",
    )
    .unwrap();

    let canonical = commands::context_tag_name("miss_ctx");
    let context_id = engine
        .engine
        .resolve_tag_name(&canonical)
        .expect("context tag");
    {
        let projections = &mut engine.engine.path_contexts.projections;
        projections.insert(context_id, PathProjection::new(context_id, PathBuf::from("/unused")));
    }

    // Mint an object, register a path, but skip the blob write.
    let oid = create_one(&mut engine);
    let proj = engine
        .engine
        .path_contexts
        .get_mut(context_id)
        .expect("projection");
    proj.add(oid, "lonely.txt".into()).unwrap();

    let target = f._tmp.path().join("export_missing");
    let err = commands::run_project_export(
        &mut engine,
        &mut Cursor::new(Vec::new()),
        "miss_ctx",
        &target,
        None,
    )
    .unwrap_err();
    // Surfaces as Unix(MissingContent(_)).
    assert!(matches!(err, CommandError::Unix(_)), "{err}");
}

#[test]
fn project_export_single_oid_writes_blob_file() {
    let f = Fixture::new();
    let mut engine = f.open();
    commands::run_project_create_context(
        &mut engine,
        &mut Cursor::new(Vec::new()),
        "ctx2",
    )
    .unwrap();

    // Mint an object and write a blob.
    let oid = create_one(&mut engine);
    let payload = b"phase R0 export bytes";
    engine.write_blob(oid, payload).unwrap();

    let out_path = f._tmp.path().join("exported.bin");
    let mut buf = Cursor::new(Vec::new());
    commands::run_project_export(
        &mut engine,
        &mut buf,
        "ctx2",
        &out_path,
        Some(&oid.to_string()),
    )
    .unwrap();

    let written = std::fs::read(&out_path).unwrap();
    assert_eq!(written.as_slice(), payload);
}

// =========================================================================
// Miscellaneous.
// =========================================================================

#[test]
fn info_on_unknown_object_errors() {
    let f = Fixture::new();
    let mut engine = f.open();
    let err = commands::run_info(&mut engine, &mut Cursor::new(Vec::new()), "obj:99999")
        .unwrap_err();
    assert!(matches!(err, CommandError::NotFound(_)), "{err}");
}

#[test]
fn tag_unknown_object_errors() {
    let f = Fixture::new();
    let mut engine = f.open();
    let err = commands::run_tag(
        &mut engine,
        &mut Cursor::new(Vec::new()),
        "obj:9999",
        &["x".into()],
    )
    .unwrap_err();
    assert!(matches!(err, CommandError::NotFound(_)), "{err}");
}

#[test]
fn round_trip_through_commit_and_reopen() {
    let f = Fixture::new();
    let oid;
    {
        let mut engine = f.open();
        let o = create_one(&mut engine);
        oid = o;
        let oid_arg = format!("obj:{}", o.local_seq());
        commands::run_tag(
            &mut engine,
            &mut Cursor::new(Vec::new()),
            &oid_arg,
            &["song".into()],
        )
        .unwrap();
        engine.commit().unwrap();
    }

    let mut reopened = f.open();
    let oid_str = format!("obj:{}:{}", oid.node_id(), oid.local_seq());
    let info = capture(|w| commands::run_info(&mut reopened, w, &oid_str).unwrap());
    assert!(info.contains("tag=song"), "{info}");
}

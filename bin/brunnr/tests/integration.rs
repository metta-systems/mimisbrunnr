use std::process::Command;

use tempfile::TempDir;

fn run_brunnr(args: &[&str]) -> std::process::Output {
    Command::new("cargo")
        .args(["run", "--quiet", "--bin", "brunnr", "--"])
        .args(args)
        .output()
        .expect("failed to run brunnr")
}

fn run_mimir(pool_dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    let pool_toml = pool_dir.join("pool.toml");
    Command::new("cargo")
        .args(["run", "--quiet", "-p", "mimir", "--", "--pool"])
        .arg(pool_toml.to_str().unwrap())
        .args(args)
        .output()
        .expect("failed to run mimir")
}

#[test]
fn create_single_disk_pool() {
    let tmp = TempDir::new().unwrap();
    let disk = tmp.path().join("disk0.mbrunnr");

    let output = run_brunnr(&[
        "create",
        disk.to_str().unwrap(),
        "--size-mib",
        "128",
    ]);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("Pool created successfully"));
    assert!(stdout.contains("128 MiB"));

    // Verify the file was created with correct size
    let meta = std::fs::metadata(&disk).unwrap();
    assert_eq!(meta.len(), 128 * 1024 * 1024);
}

#[test]
fn create_multi_disk_pool() {
    let tmp = TempDir::new().unwrap();
    let disk0 = tmp.path().join("nvme.mbrunnr");
    let disk1 = tmp.path().join("ssd.mbrunnr");
    let disk2 = tmp.path().join("hdd.mbrunnr");

    let output = run_brunnr(&[
        "create",
        disk0.to_str().unwrap(),
        disk1.to_str().unwrap(),
        disk2.to_str().unwrap(),
        "--size-mib",
        "128",
        "--tier",
        "hot",
        "--tier",
        "warm",
        "--tier",
        "cold",
    ]);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("3 disk(s)"));
    assert!(stdout.contains("384 MiB")); // 3 × 128
}

#[test]
fn create_multi_disk_different_sizes() {
    let tmp = TempDir::new().unwrap();
    let disk0 = tmp.path().join("fast.mbrunnr");
    let disk1 = tmp.path().join("bulk.mbrunnr");

    let output = run_brunnr(&[
        "create",
        disk0.to_str().unwrap(),
        disk1.to_str().unwrap(),
        "--size-mib", "128",
        "--size-mib", "512",
        "--tier", "hot",
        "--tier", "cold",
    ]);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("128 MiB"), "expected 128 MiB disk, stdout: {stdout}");
    assert!(stdout.contains("512 MiB"), "expected 512 MiB disk, stdout: {stdout}");
    assert!(stdout.contains("zones:"), "expected zone info, stdout: {stdout}");

    // Verify files have correct different sizes
    assert_eq!(std::fs::metadata(&disk0).unwrap().len(), 128 * 1024 * 1024);
    assert_eq!(std::fs::metadata(&disk1).unwrap().len(), 512 * 1024 * 1024);

    // Status should show different capacities
    let status = run_brunnr(&["status", disk0.to_str().unwrap(), disk1.to_str().unwrap()]);
    let status_out = String::from_utf8_lossy(&status.stdout);
    assert!(status.status.success());
    assert!(status_out.contains("128 MiB"), "status: {status_out}");
    assert!(status_out.contains("512 MiB"), "status: {status_out}");
    assert!(status_out.contains("640 MiB"), "total should be 640, status: {status_out}"); // 128 + 512
}

#[test]
fn zone_sizes_scale_with_disk_capacity() {
    use mimisbrunnr::storage::{FileBlockDevice, Superblock, ExtentLayout, ZoneType};

    let tmp = TempDir::new().unwrap();
    let small = tmp.path().join("small.mbrunnr");
    let large = tmp.path().join("large.mbrunnr");

    run_brunnr(&[
        "create",
        small.to_str().unwrap(),
        large.to_str().unwrap(),
        "--size-mib", "128",
        "--size-mib", "1024",
    ]);

    // Read superblocks and compare zone sizes
    let dev_small = FileBlockDevice::open(&small, 0).unwrap();
    let sb_small = Superblock::read_from(&dev_small).unwrap();
    let el_small = ExtentLayout::from(&sb_small.layout);

    let dev_large = FileBlockDevice::open(&large, 0).unwrap();
    let sb_large = Superblock::read_from(&dev_large).unwrap();
    let el_large = ExtentLayout::from(&sb_large.layout);

    // Larger disk should have proportionally larger zones
    assert!(
        el_large.zone_size(ZoneType::Index) > el_small.zone_size(ZoneType::Index),
        "large index {} should exceed small index {}",
        el_large.zone_size(ZoneType::Index),
        el_small.zone_size(ZoneType::Index),
    );
    assert!(
        el_large.zone_size(ZoneType::Blob) > el_small.zone_size(ZoneType::Blob),
        "large blob {} should exceed small blob {}",
        el_large.zone_size(ZoneType::Blob),
        el_small.zone_size(ZoneType::Blob),
    );

    // Each zone should initially have exactly 1 extent
    assert_eq!(el_small.extents(ZoneType::Index).len(), 1);
    assert_eq!(el_large.extents(ZoneType::Blob).len(), 1);
}

#[test]
fn status_shows_disk_info() {
    let tmp = TempDir::new().unwrap();
    let disk = tmp.path().join("disk.mbrunnr");

    // Create
    let create_output = run_brunnr(&[
        "create",
        disk.to_str().unwrap(),
        "--size-mib",
        "128",
    ]);
    assert!(create_output.status.success());

    // Status
    let output = run_brunnr(&["status", disk.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("Capacity:  128 MiB"), "stdout: {stdout}");
    assert!(stdout.contains("Index:"), "stdout: {stdout}");
    assert!(stdout.contains("Metadata:"), "stdout: {stdout}");
    assert!(stdout.contains("Blob:"), "stdout: {stdout}");
    assert!(stdout.contains("extent(s)"), "stdout: {stdout}");
}

#[test]
fn status_of_invalid_file() {
    let tmp = TempDir::new().unwrap();
    let fake = tmp.path().join("notadisk.bin");
    std::fs::write(&fake, b"not a mimisbrunnr disk").unwrap();

    let output = run_brunnr(&["status", fake.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    // Should report it's not valid (might be on stdout or stderr)
    assert!(
        combined.contains("not a valid") || combined.contains("error"),
        "expected error output, got stdout={stdout:?} stderr={stderr:?}"
    );
}

#[test]
fn add_disk_to_pool() {
    let tmp = TempDir::new().unwrap();
    let disk0 = tmp.path().join("disk0.mbrunnr");
    let disk1 = tmp.path().join("disk1.mbrunnr");

    // Create initial pool
    run_brunnr(&["create", disk0.to_str().unwrap(), "--size-mib", "128"]);

    // Add a second disk
    let output = run_brunnr(&[
        "add-disk",
        disk1.to_str().unwrap(),
        "--pool-disk",
        disk0.to_str().unwrap(),
        "--size-mib",
        "128",
        "--tier",
        "cold",
    ]);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("Added disk"));

    // Verify both disks are valid
    let status = run_brunnr(&["status", disk0.to_str().unwrap(), disk1.to_str().unwrap()]);
    let status_out = String::from_utf8_lossy(&status.stdout);
    assert!(status_out.contains("256 MiB")); // 128 + 128
}

#[test]
fn create_pool_superblock_survives_reopen() {
    let tmp = TempDir::new().unwrap();
    let disk = tmp.path().join("persist.mbrunnr");

    // Create
    run_brunnr(&["create", disk.to_str().unwrap(), "--size-mib", "128", "--node-id", "42"]);

    // Read superblock directly to verify
    use mimisbrunnr::storage::{FileBlockDevice, Superblock};
    let dev = FileBlockDevice::open(&disk, 0).unwrap();
    let sb = Superblock::read_from(&dev).unwrap();

    assert_eq!(sb.node_id, 42);
    assert_eq!(sb.disk_id, 0);
    assert_eq!(sb.layout.device_capacity, 128 * 1024 * 1024);
}

#[test]
fn create_pool_wal_is_valid() {
    let tmp = TempDir::new().unwrap();
    let disk = tmp.path().join("waltest.mbrunnr");

    run_brunnr(&["create", disk.to_str().unwrap(), "--size-mib", "128"]);

    // Open and verify WAL
    use mimisbrunnr::storage::{FileBlockDevice, Superblock, WAL_SIZE};
    use mimisbrunnr::wal::WriteAheadLog;

    let dev = FileBlockDevice::open(&disk, 0).unwrap();
    let sb = Superblock::read_from(&dev).unwrap();

    let wal = WriteAheadLog::open(&dev, sb.layout.wal_offset, WAL_SIZE).unwrap();
    assert_eq!(wal.next_lsn(), 1);
    assert_eq!(wal.used_bytes(), 0);
}

// ── Cross-tool integration: brunnr create → mimir operates on pool ─────

#[test]
fn brunnr_create_then_mimir_ontology_register() {
    let tmp = TempDir::new().unwrap();
    let disk = tmp.path().join("disk0.mbrunnr");

    // Create pool
    let output = run_brunnr(&["create", disk.to_str().unwrap(), "--size-mib", "128"]);
    assert!(output.status.success());

    // pool.toml should exist
    assert!(tmp.path().join("pool.toml").exists());

    // Register a tag via mimir
    let output = run_mimir(tmp.path(), &[
        "ontology", "register", "electronic",
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("registered tag 'electronic'"), "stdout: {stdout}");

    // List tags — should show our tag persisted
    let output = run_mimir(tmp.path(), &["ontology", "list"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(stdout.contains("electronic"), "stdout: {stdout}");
}

#[test]
fn brunnr_create_then_mimir_create_tag_query_round_trip() {
    let tmp = TempDir::new().unwrap();
    let disk = tmp.path().join("disk0.mbrunnr");

    // Create pool
    run_brunnr(&["create", disk.to_str().unwrap(), "--size-mib", "128"]);

    // Register tags
    run_mimir(tmp.path(), &["ontology", "register", "electronic"]);
    run_mimir(tmp.path(), &["ontology", "register", "ambient"]);

    // Create objects and tag them via DiskEngine API
    {
        use mimisbrunnr::engine::DiskEngine;

        let pool_toml = tmp.path().join("pool.toml");
        let mut de = DiskEngine::open(&pool_toml).unwrap();
        let e = de.engine_mut();

        // Tags were registered by mimir above, they should be persisted
        let electronic = e.dag.lookup("electronic").expect("electronic tag should exist");
        let ambient = e.dag.lookup("ambient").expect("ambient tag should exist");

        let oid = e.create_object(1000).unwrap();
        e.add_tag(oid, electronic, 1000).unwrap();
        e.add_tag(oid, ambient, 1000).unwrap();

        let oid2 = e.create_object(1000).unwrap();
        e.add_tag(oid2, electronic, 1000).unwrap();

        de.flush().unwrap();
    }

    // Query via mimir CLI
    let output = run_mimir(tmp.path(), &["query", "electronic"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("2 result(s)"), "expected 2 results, stdout: {stdout}");

    // Compound query
    let output = run_mimir(tmp.path(), &["query", "electronic AND ambient"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(stdout.contains("1 result(s)"), "expected 1 result, stdout: {stdout}");

    // Info on object 0
    let output = run_mimir(tmp.path(), &["info", "--object", "0"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(stdout.contains("electronic"), "stdout: {stdout}");
    assert!(stdout.contains("ambient"), "stdout: {stdout}");
}

#[test]
fn brunnr_create_then_mimir_set_attr_and_query() {
    let tmp = TempDir::new().unwrap();
    let disk = tmp.path().join("disk0.mbrunnr");

    run_brunnr(&["create", disk.to_str().unwrap(), "--size-mib", "128"]);

    // Register an attribute tag
    run_mimir(tmp.path(), &[
        "ontology", "register", "artist", "--semantics", "attribute", "--value-type", "text",
    ]);

    // Create object and set attribute via API
    {
        use mimisbrunnr::engine::DiskEngine;
        use mimisbrunnr::types::Value;

        let pool_toml = tmp.path().join("pool.toml");
        let mut de = DiskEngine::open(&pool_toml).unwrap();
        let e = de.engine_mut();

        let artist_tag = e.dag.lookup("artist").unwrap();

        let oid = e.create_object(1000).unwrap();
        e.set_attr(oid, artist_tag, Value::Text("Aphex Twin".into()), 1000).unwrap();

        let oid2 = e.create_object(1000).unwrap();
        e.set_attr(oid2, artist_tag, Value::Text("Boards of Canada".into()), 1000).unwrap();

        de.flush().unwrap();
    }

    // Query for artist=Aphex Twin
    let output = run_mimir(tmp.path(), &["query", "artist=\"Aphex Twin\""]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("1 result(s)"), "expected 1 result, stdout: {stdout}");
}

#[test]
fn pool_config_written_by_brunnr() {
    let tmp = TempDir::new().unwrap();
    let disk0 = tmp.path().join("disk0.mbrunnr");
    let disk1 = tmp.path().join("disk1.mbrunnr");

    run_brunnr(&[
        "create",
        disk0.to_str().unwrap(),
        disk1.to_str().unwrap(),
        "--size-mib", "128",
        "--tier", "hot",
        "--tier", "warm",
        "--node-id", "7",
    ]);

    // Verify pool.toml contents
    use mimisbrunnr::pool::PoolConfig;
    let config = PoolConfig::load(&tmp.path().join("pool.toml")).unwrap();

    assert_eq!(config.node_id, 7);
    assert_eq!(config.disks.len(), 2);
    assert_eq!(config.disks[0].tier, "hot");
    assert_eq!(config.disks[1].tier, "warm");
    assert_eq!(config.disks[0].capacity_bytes, 128 * 1024 * 1024);
}

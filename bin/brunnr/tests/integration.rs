//! Integration tests that drive `brunnr` as a subprocess.
//!
//! Unit tests covering the command implementations live next to the code in
//! `src/commands.rs`; this file only exercises the binary at the CLI level
//! to make sure clap wiring stays sane and the help output works.

use std::process::Command;

use tempfile::TempDir;

fn run_brunnr(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_brunnr"))
        .args(args)
        .output()
        .expect("failed to run brunnr")
}

#[test]
fn help_prints_subcommands() {
    let out = run_brunnr(&["--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("create"), "help missing 'create': {stdout}");
    assert!(stdout.contains("status"));
    assert!(stdout.contains("add-disk"));
    assert!(stdout.contains("remove-disk"));
    assert!(stdout.contains("mount"));
    assert!(stdout.contains("unmount"));
}

#[test]
fn create_then_status_via_cli() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("pool.toml");
    let disk = tmp.path().join("disk0.img");

    // Pre-allocate the disk file so capacity=auto can pick up its size.
    std::fs::File::create(&disk)
        .unwrap()
        .set_len(64 * 1024 * 1024)
        .unwrap();

    let spec = format!("{}:tier=hot:media=nvme:capacity=auto", disk.display());

    let out = run_brunnr(&[
        "--config",
        cfg.to_str().unwrap(),
        "create",
        &spec,
        "--node-id",
        "3",
    ]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(cfg.exists(), "pool.toml should be written");

    let out = run_brunnr(&["--config", cfg.to_str().unwrap(), "status"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(stdout.contains("node_id:           3"), "{stdout}");
    assert!(stdout.contains("disks:             1"), "{stdout}");
}

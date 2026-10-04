//! End-to-end tests for `cibox update` merge semantics.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_yaml::Value;
use std::fs;
use std::path::Path;

/// A publishable Rust project with a Dockerfile, in a git repo
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(dir.path().join("Dockerfile"), "FROM rust:latest\n").unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    dir
}

fn update(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("cibox").unwrap();
    cmd.current_dir(dir).args(["update", "--platform", "github"]);
    cmd
}

fn read_yaml(path: &Path) -> Value {
    serde_yaml::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn write_yaml(path: &Path, doc: &Value) {
    fs::write(path, serde_yaml::to_string(doc).unwrap()).unwrap();
}

#[test]
fn test_update_lifecycle() {
    let dir = project();
    let ci_path = dir.path().join(".github/workflows/ci.yml");
    let release_path = dir.path().join(".github/workflows/release.yml");

    // Fresh run writes both workflow files
    update(dir.path()).assert().success();
    assert!(ci_path.is_file());
    assert!(release_path.is_file());

    // Re-running is a no-op and leaves the files byte-identical
    let before = fs::read_to_string(&ci_path).unwrap();
    update(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("unchanged"));
    assert_eq!(before, fs::read_to_string(&ci_path).unwrap());

    // Customize: break a managed job, add a custom job, delete another
    let mut doc = read_yaml(&ci_path);
    let jobs = doc["jobs"].as_mapping_mut().unwrap();
    jobs["rust-test"] = serde_yaml::from_str(
        "runs-on: ubuntu-latest\nsteps:\n  - run: echo stale\n",
    )
    .unwrap();
    jobs.insert(
        "my-job".into(),
        serde_yaml::from_str("runs-on: ubuntu-latest\nsteps:\n  - run: echo mine\n").unwrap(),
    );
    jobs.shift_remove("rust-fmt").expect("rust-fmt generated");
    write_yaml(&ci_path, &doc);

    update(dir.path()).assert().success();
    let content = fs::read_to_string(&ci_path).unwrap();
    assert!(content.contains("cargo test"), "conformed: {content}");
    assert!(!content.contains("echo stale"), "{content}");
    assert!(content.contains("my-job"), "custom kept: {content}");
    assert!(!content.contains("rust-fmt"), "not re-added: {content}");

    // Disabling a rule removes its job from the existing file
    fs::write(
        dir.path().join("cibox.ron"),
        "(rust_clippy: (enabled: false))",
    )
    .unwrap();
    update(dir.path()).assert().success();
    let content = fs::read_to_string(&ci_path).unwrap();
    assert!(!content.contains("rust-clippy"), "{content}");

    // --force restores the canonical pipeline
    update(dir.path()).arg("--force").assert().success();
    let content = fs::read_to_string(&ci_path).unwrap();
    assert!(content.contains("rust-fmt"), "{content}");
    assert!(!content.contains("my-job"), "{content}");
    assert!(!content.contains("rust-clippy"), "still disabled: {content}");
}

#[test]
fn test_generate_subcommand_is_gone() {
    let dir = project();
    Command::cargo_bin("cibox")
        .unwrap()
        .current_dir(dir.path())
        .arg("generate")
        .assert()
        .failure();
}

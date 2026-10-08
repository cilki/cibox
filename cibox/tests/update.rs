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
    cmd.current_dir(dir)
        .args(["update", "--platform", "github"]);
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
    jobs["rust-test"] =
        serde_yaml::from_str("runs-on: ubuntu-latest\nsteps:\n  - run: echo stale\n").unwrap();
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
    assert!(
        !content.contains("rust-clippy"),
        "still disabled: {content}"
    );
}

#[test]
fn test_version_matrix_lifecycle() {
    let dir = project();
    let ci_path = dir.path().join(".github/workflows/ci.yml");

    update(dir.path()).assert().success();
    let job_count = read_yaml(&ci_path)["jobs"].as_mapping().unwrap().len();

    // Configuring versions turns rust-test into a matrix job in place
    fs::write(
        dir.path().join("cibox.ron"),
        r#"(rust_test: (versions: ["1.85", "nightly"]))"#,
    )
    .unwrap();
    update(dir.path()).assert().success();
    let doc = read_yaml(&ci_path);
    let jobs = doc["jobs"].as_mapping().unwrap();
    assert_eq!(jobs.len(), job_count, "job keys unchanged");
    let rust_test = &jobs["rust-test"];
    assert_eq!(rust_test["strategy"]["fail-fast"], Value::Bool(false));
    assert_eq!(
        rust_test["strategy"]["matrix"]["include"][1]["image"],
        Value::String("rustlang/rust:nightly".to_string())
    );

    // Idempotent on a second run
    update(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("unchanged"));

    // Reverting the config conforms the matrix away again
    fs::write(dir.path().join("cibox.ron"), "()").unwrap();
    update(dir.path()).assert().success();
    let content = fs::read_to_string(&ci_path).unwrap();
    assert!(!content.contains("strategy"), "{content}");
    assert!(content.contains("rust:latest"), "{content}");
}

/// Turning every rule off is the way to hand a pipeline back to its author, so
/// `update` has to prune cibox's jobs then rather than refusing to run.
#[test]
fn test_disabling_every_rule_prunes_instead_of_erroring() {
    let dir = project();
    let ci_path = dir.path().join(".github/workflows/ci.yml");

    update(dir.path()).assert().success();

    // The user keeps a job of their own alongside the generated ones
    let mut doc = read_yaml(&ci_path);
    doc["jobs"].as_mapping_mut().unwrap().insert(
        "my-job".into(),
        serde_yaml::from_str("runs-on: ubuntu-latest\nsteps:\n  - run: echo mine\n").unwrap(),
    );
    write_yaml(&ci_path, &doc);

    fs::write(
        dir.path().join("cibox.ron"),
        "(\n  rust_test: (enabled: false),\n  rust_fmt: (enabled: false),\n  \
         rust_clippy: (enabled: false),\n  rust_audit: (enabled: false),\n  \
         rust_doc: (enabled: false),\n  rust_minimal_versions: (enabled: false),\n  \
         rust_release: (enabled: false),\n  docker_build: (enabled: false),\n  \
         docker_release: (enabled: false),\n)\n",
    )
    .unwrap();

    update(dir.path()).assert().success();
    let doc = read_yaml(&ci_path);
    let jobs = doc["jobs"].as_mapping().unwrap();
    assert_eq!(jobs.len(), 1, "only the custom job survives: {jobs:?}");
    assert!(jobs.contains_key("my-job"), "{jobs:?}");
}

/// Removing what a rule detected on (here the Dockerfile) must take the rule's
/// jobs with it, not leave a pipeline that builds an image that is gone.
#[test]
fn test_removing_the_last_detected_fact_prunes_its_jobs() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("Dockerfile"), "FROM alpine\n").unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    let ci_path = dir.path().join(".github/workflows/ci.yml");

    update(dir.path()).assert().success();
    assert!(fs::read_to_string(&ci_path)
        .unwrap()
        .contains("docker-build"));

    // Keep a job of the user's own so the file stays valid once pruned
    let mut doc = read_yaml(&ci_path);
    doc["jobs"].as_mapping_mut().unwrap().insert(
        "my-job".into(),
        serde_yaml::from_str("runs-on: ubuntu-latest\nsteps:\n  - run: echo mine\n").unwrap(),
    );
    write_yaml(&ci_path, &doc);

    fs::remove_file(dir.path().join("Dockerfile")).unwrap();
    update(dir.path()).assert().success();
    let content = fs::read_to_string(&ci_path).unwrap();
    assert!(!content.contains("docker-build"), "{content}");
    assert!(content.contains("my-job"), "{content}");
}

/// With nothing enabled *and* nothing on disk to prune there is genuinely
/// nothing to do, and a silent success would just look like a broken tool.
#[test]
fn test_nothing_enabled_and_nothing_to_prune_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();

    update(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("No rules enabled"));

    // --force rewrites files wholesale, so an existing pipeline is no reason
    // to keep going either
    let project = project();
    update(project.path()).assert().success();
    fs::write(
        project.path().join("cibox.ron"),
        "(\n  rust_test: (enabled: false),\n  rust_fmt: (enabled: false),\n  \
         rust_clippy: (enabled: false),\n  rust_audit: (enabled: false),\n  \
         rust_doc: (enabled: false),\n  rust_minimal_versions: (enabled: false),\n  \
         rust_release: (enabled: false),\n  docker_build: (enabled: false),\n  \
         docker_release: (enabled: false),\n)\n",
    )
    .unwrap();
    update(project.path())
        .arg("--force")
        .assert()
        .failure()
        .stderr(predicate::str::contains("No rules enabled"));
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

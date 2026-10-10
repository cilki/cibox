//! End-to-end tests for `cibox detect`.

use assert_cmd::Command;
use std::fs;
use std::path::Path;

/// A project whose own files try to drive the terminal cibox prints to: the
/// package name erases the line it is printed on, writes a reassuring message
/// in its place and retitles the window, the module path recolors the rest of
/// the output, and the origin URL erases the line reporting the remote.
fn hostile_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    // TOML `\uXXXX` escapes, so the file itself stays printable
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\n\
         name = \"poc\\u001B[2K\\u001B[1;32mnothing to see here\\u001B]0;pwned\\u0007\"\n\
         version = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("go.mod"),
        "module example.com/\u{1b}[31mevil\u{1b}[0m\n\ngo 1.23\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    fs::write(
        dir.path().join(".git/config"),
        "[remote \"origin\"]\n\turl = https://host/owner/repo\u{1b}[2Knice\n",
    )
    .unwrap();
    dir
}

fn detect(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("cibox").unwrap();
    // Keep cibox's own coloring out of the comparison
    cmd.env("NO_COLOR", "1")
        .args(["detect", "--dir", dir.to_str().unwrap()]);
    cmd
}

fn stdout_of(mut cmd: Command) -> String {
    let output = cmd.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn test_detect_output_holds_nothing_a_terminal_acts_on() {
    let dir = hostile_project();
    let stdout = stdout_of(detect(dir.path()));

    // The line breaks are cibox's own; everything else a terminal would act
    // on has to have been escaped on the way out
    let acted_on: Vec<char> = stdout
        .chars()
        .filter(|c| c.is_control() && *c != '\n')
        .collect();
    assert!(acted_on.is_empty(), "{stdout:?}");

    // ...and the facts are still reported, with the escapes made visible
    assert!(stdout.contains("poc\\u{1b}[2K"), "{stdout}");
    assert!(stdout.contains("example.com/\\u{1b}[31mevil"), "{stdout}");
    assert!(
        stdout.contains("remote: owner/repo\\u{1b}[2Knice"),
        "{stdout}"
    );
}

#[test]
fn test_detect_reports_ordinary_facts_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"my-app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(dir.path().join("go.mod"), "module github.com/owner/repo\n").unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    fs::write(
        dir.path().join(".git/config"),
        "[remote \"origin\"]\n\turl = git@github.com:owner/repo.git\n",
    )
    .unwrap();

    let stdout = stdout_of(detect(dir.path()));
    assert!(stdout.contains("Rust (my-app)"), "{stdout}");
    assert!(stdout.contains("Go (github.com/owner/repo)"), "{stdout}");
    assert!(stdout.contains("remote: owner/repo"), "{stdout}");
    // Nothing was escaped that didn't need to be
    assert!(!stdout.contains('\\'), "{stdout}");
}

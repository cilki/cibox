//! End-to-end tests for how the cibox binary behaves as a process.

use std::fs;
use std::process::{Command, Stdio};

/// A Rust project in a git repo, enough for `detect` to have plenty to say
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    dir
}

/// Detection never fails, so a mistyped `--dir` used to look like an empty
/// project: `detect` said every rule was "not detected" and exited 0. A
/// directory that isn't there, or a file in its place, has to be refused.
#[test]
fn test_dir_that_is_not_a_directory_is_refused() {
    let dir = project();
    let missing = dir.path().join("typo");
    let file = dir.path().join("Cargo.toml");
    for command in ["detect", "editor"] {
        for (bad, reason) in [(&missing, "no such directory"), (&file, "not a directory")] {
            let output = Command::new(env!("CARGO_BIN_EXE_cibox"))
                .args([command, "--dir"])
                .arg(bad)
                .stdin(Stdio::null())
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.code(), Some(2), "{command} {bad:?}: {stderr}");
            assert!(stderr.contains(reason), "{command} {bad:?}: {stderr}");
            assert!(output.stdout.is_empty(), "{command} {bad:?}: {output:?}");
        }
    }

    // A real directory is still accepted
    let output = Command::new(env!("CARGO_BIN_EXE_cibox"))
        .args(["detect", "--dir"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("Rust"));
}

/// A bare `cibox` opens the editor, so `cibox | less` or a script calling it
/// used to draw the editor into the pipe and wait for keys on a terminal
/// showing nothing — or, with no terminal at all, fail with "No such device
/// or address". Both have to be told plainly what to run instead, and
/// neither may leave cibox.ron behind.
#[test]
fn test_editor_without_a_terminal_says_what_to_run_instead() {
    let dir = project();
    for args in [&[][..], &["editor"][..]] {
        let output = Command::new(env!("CARGO_BIN_EXE_cibox"))
            .current_dir(dir.path())
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap();

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "args {args:?}: {output:?}");
        assert!(
            stderr.contains(cibox::editor::NEEDS_TERMINAL),
            "args {args:?}: {stderr}"
        );
        assert!(output.stdout.is_empty(), "args {args:?}: {output:?}");
    }
    assert!(!dir.path().join("cibox.ron").exists());
}

/// `cibox detect | head` used to end in "failed printing to stdout: Broken
/// pipe" with a backtrace hint: Rust ignores SIGPIPE, so the write to the
/// closed pipe reported EPIPE and `println!` panicked on it. A reader that
/// hangs up has to end the process quietly, like it does any other tool.
#[cfg(unix)]
#[test]
fn test_closed_stdout_ends_the_process_without_a_panic() {
    use std::os::unix::process::ExitStatusExt;

    let dir = project();
    let mut child = Command::new(env!("CARGO_BIN_EXE_cibox"))
        .current_dir(dir.path())
        .arg("detect")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Hang up before reading anything, the way `head -1` does once it has
    // its line
    drop(child.stdout.take());

    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked") && !stderr.contains("Broken pipe"),
        "stderr: {stderr}"
    );
    // Either the write to the closed pipe ended the process with SIGPIPE, or
    // every line was already in the pipe buffer when the reader left and
    // detect finished normally. A non-zero exit code is neither.
    assert!(
        output.status.signal() == Some(libc::SIGPIPE) || output.status.success(),
        "status: {:?}",
        output.status
    );
}

/// No cibox.ron is needed until the user wants to override something, and
/// `update` already runs on the detected defaults without one. `validate`
/// used to fail there with "Failed to read config file: cibox.ron", which
/// made a project with nothing to configure look broken. It has to report
/// the defaults it resolved instead, while a file named explicitly on the
/// command line still has to exist.
#[test]
fn test_validate_without_cibox_ron_resolves_the_detected_defaults() {
    let dir = project();
    let output = Command::new(env!("CARGO_BIN_EXE_cibox"))
        .current_dir(dir.path())
        .arg("validate")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{output:?}");
    assert!(stdout.contains("using detected defaults"), "{stdout}");
    assert!(!stdout.contains("Configuration is valid"), "{stdout}");
    assert!(stdout.contains("rust-test"), "{stdout}");
    assert!(!dir.path().join("cibox.ron").exists());

    let output = Command::new(env!("CARGO_BIN_EXE_cibox"))
        .current_dir(dir.path())
        .args(["validate", "other.ron"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("other.ron"),
        "{output:?}"
    );
}

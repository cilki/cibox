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

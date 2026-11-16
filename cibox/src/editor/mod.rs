pub mod app;
pub mod events;
pub mod state;
pub mod ui;

use crate::error::Result;
use std::io::IsTerminal;
use std::path::PathBuf;

/// Why the editor refuses to start when its screen has nowhere to go
pub const NEEDS_TERMINAL: &str =
    "the editor needs an interactive terminal, but stdout is not one; \
     in scripts and pipes use `cibox detect`, `cibox validate` or `cibox update` instead";

/// Run the editor with specific arguments
pub fn run_with_args(dir: &str) -> Result<()> {
    // The editor is also what a bare `cibox` runs, so `cibox | less`, `cibox
    // > out` or a script calling it is an easy mistake to make. Without this
    // check those drew the alternate screen into the pipe or file and sat
    // waiting for keys on a terminal showing nothing, or — with no terminal
    // at all — failed with a bare "No such device or address (os error 6)".
    if !std::io::stdout().is_terminal() {
        anyhow::bail!(NEEDS_TERMINAL);
    }
    let working_dir = crate::detection::project_root(&PathBuf::from(dir));
    let facts = crate::detection::gather_facts(&working_dir);
    app::EditorApp::new(facts, working_dir)?.run()
}

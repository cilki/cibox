pub mod app;
pub mod events;
pub mod state;
pub mod ui;

use crate::error::Result;
use std::path::PathBuf;

/// Run the editor with specific arguments
pub fn run_with_args(dir: &str, platform: Option<String>) -> Result<()> {
    let working_dir = PathBuf::from(dir);
    let facts = crate::detection::gather_facts(&working_dir);
    app::EditorApp::new(facts, platform, working_dir)?.run()
}

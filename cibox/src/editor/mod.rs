pub mod app;
pub mod config;
pub mod events;
pub mod registry;
pub mod state;
pub mod ui;

use crate::detection::DetectorRegistry;
use crate::error::Result;
use std::path::PathBuf;

/// Run the editor with specific arguments
pub fn run_with_args(dir: &str, platform: Option<String>) -> Result<()> {
    // Auto-detect project
    let working_dir = PathBuf::from(dir);
    let registry = DetectorRegistry::new();
    let detection = registry.detect(&working_dir)?;

    // Launch editor
    app::EditorApp::new(detection, platform)?.run()
}

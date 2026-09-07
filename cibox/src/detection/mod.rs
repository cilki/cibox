mod docker;
mod go;
mod python;
mod registry;
mod rust;

use crate::error::Result;
use std::collections::HashMap;
use std::path::Path;

/// Result of project detection
#[derive(Debug, Clone, PartialEq)]
pub struct DetectionResult {
    pub project_type: ProjectType,
    pub language_version: Option<String>,
    pub metadata: HashMap<String, String>,
}

/// Types of projects that can be detected
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectType {
    RustLibrary,
    RustBinary,
    RustWorkspace,
    PythonApp,
    PythonLibrary,
    GoApp,
    GoLibrary,
    DockerImage,
}

impl std::fmt::Display for ProjectType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectType::RustLibrary => write!(f, "Rust Library"),
            ProjectType::RustBinary => write!(f, "Rust Binary"),
            ProjectType::RustWorkspace => write!(f, "Rust Workspace"),
            ProjectType::PythonApp => write!(f, "Python Application"),
            ProjectType::PythonLibrary => write!(f, "Python Library"),
            ProjectType::GoApp => write!(f, "Go Application"),
            ProjectType::GoLibrary => write!(f, "Go Library"),
            ProjectType::DockerImage => write!(f, "Docker Image"),
        }
    }
}

/// Trait for detecting project types
pub trait ProjectDetector: Send + Sync {
    /// Detect if this detector matches the current project
    fn detect(&self, path: &Path) -> Result<Option<DetectionResult>>;

    /// Get the name of this detector
    fn name(&self) -> &str;
}

pub use docker::DockerDetector;
pub use go::GoDetector;
pub use python::PythonDetector;
pub use registry::DetectorRegistry;
pub use rust::RustDetector;

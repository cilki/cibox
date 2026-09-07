pub mod docker;
pub mod gitleaks;
pub mod go;
pub mod python;
pub mod rust;

pub use docker::{Docker, DockerRegistry};
pub use gitleaks::Gitleaks;
pub use go::GoApp;
pub use python::{PythonApp, PythonFormatter, PythonLinter};
pub use rust::Rust;

/// Docker image with all dependencies needed by generated CI jobs
pub(crate) const CIBOX_IMAGE: &str = "fossable/cibox:latest";

/// The single list of presets. Callers pass a macro that receives
/// `(EnumVariant, Type, "Display Name")` tuples and expands the
/// registration code they need — adding a preset means adding one
/// line here (plus its `PresetChoice` variant, which roniker requires
/// to stay a literal enum).
macro_rules! with_presets {
    ($m:ident) => {
        $m! {
            (Rust, crate::presets::Rust, "Rust"),
            (PythonApp, crate::presets::PythonApp, "Python"),
            (GoApp, crate::presets::GoApp, "Go App"),
            (Docker, crate::presets::Docker, "Docker"),
            (Gitleaks, crate::presets::Gitleaks, "Gitleaks"),
        }
    };
}
pub(crate) use with_presets;

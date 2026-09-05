use cibox_macros::Preset;

mod circleci;
mod gitea;
mod github;
mod gitlab;
mod jenkins;

/// Linter tool options for Python
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    strum::Display,
    strum::EnumString,
    strum::VariantNames,
)]
#[serde(rename_all = "lowercase")]
pub enum PythonLinter {
    #[default]
    #[strum(serialize = "flake8")]
    Flake8,
    #[strum(serialize = "ruff")]
    Ruff,
}

impl PythonLinter {
    pub fn name(&self) -> &'static str {
        match self {
            PythonLinter::Flake8 => "flake8",
            PythonLinter::Ruff => "ruff",
        }
    }

    pub fn check_command(&self) -> &'static str {
        match self {
            PythonLinter::Flake8 => "flake8 .",
            PythonLinter::Ruff => "ruff check .",
        }
    }

    pub fn toggle(&self) -> Self {
        match self {
            PythonLinter::Flake8 => PythonLinter::Ruff,
            PythonLinter::Ruff => PythonLinter::Flake8,
        }
    }
}

/// Formatter tool options for Python
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    strum::Display,
    strum::EnumString,
    strum::VariantNames,
)]
#[serde(rename_all = "lowercase")]
pub enum PythonFormatter {
    #[default]
    #[strum(serialize = "black")]
    Black,
    #[strum(serialize = "ruff")]
    Ruff,
}

impl PythonFormatter {
    pub fn name(&self) -> &'static str {
        match self {
            PythonFormatter::Black => "black",
            PythonFormatter::Ruff => "ruff",
        }
    }

    pub fn check_command(&self) -> &'static str {
        match self {
            PythonFormatter::Black => "black --check .",
            PythonFormatter::Ruff => "ruff format --check .",
        }
    }

    pub fn toggle(&self) -> Self {
        match self {
            PythonFormatter::Black => PythonFormatter::Ruff,
            PythonFormatter::Ruff => PythonFormatter::Black,
        }
    }
}

/// CI pipeline for Python applications with pytest, linting, and type checking
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Preset)]
#[preset(category = "Languages")]
#[serde(default)]
pub struct PythonApp {
    #[preset_field(hidden = true)]
    pub(super) python_version: String,

    /// Choose linter tool (None, Flake8, or Ruff)
    #[preset_field(display = "Linter")]
    pub(super) linter: Option<PythonLinter>,

    /// Enable mypy static type checking
    #[preset_field(display = "Type Checking")]
    pub(super) enable_type_check: bool,

    /// Choose formatter tool (None, Black, or Ruff)
    #[preset_field(display = "Formatter")]
    pub(super) formatter: Option<PythonFormatter>,
}

impl Default for PythonApp {
    fn default() -> Self {
        Self {
            python_version: "3.11".to_string(),
            linter: None,
            enable_type_check: false,
            formatter: None,
        }
    }
}

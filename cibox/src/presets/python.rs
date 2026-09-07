use crate::ir::{Job, Stage, Step, ToJobs};
use cibox_macros::Preset;

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

impl PythonApp {
    fn image(&self) -> String {
        format!("python:{}", self.python_version)
    }
}

impl ToJobs for PythonApp {
    fn jobs(&self) -> Vec<Job> {
        let mut jobs = vec![Job::new("test", "Test", Stage::Test)
            .with_image(self.image())
            .with_timeout(30)
            .with_steps(vec![
                Step::checkout(),
                Step::run("Install dependencies", "pip install -r requirements.txt"),
                Step::run("Run tests", "pytest"),
            ])];

        if let Some(linter) = &self.linter {
            jobs.push(
                Job::new("lint", "Lint", Stage::Lint)
                    .with_image(self.image())
                    .with_timeout(10)
                    .with_steps(vec![
                        Step::checkout(),
                        Step::run("Install linter", format!("pip install {}", linter.name())),
                        Step::run("Run linter", linter.check_command()),
                    ]),
            );
        }

        if self.enable_type_check {
            jobs.push(
                Job::new("typecheck", "Type Check", Stage::Lint)
                    .with_image(self.image())
                    .with_timeout(10)
                    .with_steps(vec![
                        Step::checkout(),
                        Step::run("Install mypy", "pip install mypy"),
                        Step::run("Run mypy", "mypy ."),
                    ]),
            );
        }

        if let Some(formatter) = &self.formatter {
            jobs.push(
                Job::new("format", "Format Check", Stage::Lint)
                    .with_image(self.image())
                    .with_timeout(10)
                    .with_steps(vec![
                        Step::checkout(),
                        Step::run(
                            "Install formatter",
                            format!("pip install {}", formatter.name()),
                        ),
                        Step::run("Check formatting", formatter.check_command()),
                    ]),
            );
        }

        jobs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_produces_only_test_job() {
        let jobs = PythonApp::default().jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "test");
        assert_eq!(jobs[0].image.as_deref(), Some("python:3.11"));
    }

    #[test]
    fn test_optional_jobs() {
        let preset = PythonApp {
            linter: Some(PythonLinter::Ruff),
            enable_type_check: true,
            formatter: Some(PythonFormatter::Black),
            ..PythonApp::default()
        };
        let ids: Vec<String> = preset.jobs().into_iter().map(|j| j.id).collect();
        assert_eq!(ids, vec!["test", "lint", "typecheck", "format"]);
    }

    #[test]
    fn test_linter_command() {
        let preset = PythonApp {
            linter: Some(PythonLinter::Ruff),
            ..PythonApp::default()
        };
        let jobs = preset.jobs();
        assert!(jobs[1]
            .steps
            .iter()
            .any(|s| matches!(s, Step::Run { command, .. } if command == "ruff check .")));
    }
}

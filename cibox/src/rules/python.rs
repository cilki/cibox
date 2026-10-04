use super::Rule;
use crate::detection::ProjectFacts;
use crate::ir::{Job, Stage, Step};

const IMAGE: &str = "python:3.12";

fn is_python(facts: &ProjectFacts) -> bool {
    facts.python.is_some()
}

/// Install command appropriate for the project layout
fn install_command(facts: &ProjectFacts) -> &'static str {
    match &facts.python {
        Some(py) if py.has_pyproject => "pip install .",
        _ => "pip install -r requirements.txt",
    }
}

/// Run the test suite with pytest
pub struct PythonTest;

impl Rule for PythonTest {
    fn id(&self) -> &'static str {
        "python-test"
    }

    fn name(&self) -> &'static str {
        "Pytest"
    }

    fn description(&self) -> &'static str {
        "Install the package and run pytest on every push"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_python(facts)
    }

    fn jobs(&self, facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Test)
            .with_image(IMAGE)
            .with_timeout(30)
            .with_steps(vec![
                Step::checkout(),
                Step::run("Install dependencies", install_command(facts)),
                Step::run("Install pytest", "pip install pytest"),
                Step::run("Run tests", "pytest"),
            ])]
    }
}

/// Lint with ruff
pub struct PythonLint;

impl Rule for PythonLint {
    fn id(&self) -> &'static str {
        "python-lint"
    }

    fn name(&self) -> &'static str {
        "Ruff check"
    }

    fn description(&self) -> &'static str {
        "Lint with ruff"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_python(facts)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Lint)
            .with_image(IMAGE)
            .with_timeout(10)
            .with_steps(vec![
                Step::checkout(),
                Step::run("Install ruff", "pip install ruff"),
                Step::run("Run ruff", "ruff check ."),
            ])]
    }
}

/// Check formatting with ruff
pub struct PythonFmt;

impl Rule for PythonFmt {
    fn id(&self) -> &'static str {
        "python-fmt"
    }

    fn name(&self) -> &'static str {
        "Ruff format"
    }

    fn description(&self) -> &'static str {
        "Check code formatting with ruff"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_python(facts)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Lint)
            .with_image(IMAGE)
            .with_timeout(10)
            .with_steps(vec![
                Step::checkout(),
                Step::run("Install ruff", "pip install ruff"),
                Step::run("Check formatting", "ruff format --check ."),
            ])]
    }
}

/// Publish to PyPI when a version tag is pushed
pub struct PythonRelease;

impl Rule for PythonRelease {
    fn id(&self) -> &'static str {
        "python-release"
    }

    fn name(&self) -> &'static str {
        "PyPI publish"
    }

    fn description(&self) -> &'static str {
        "Build and upload to PyPI on version tags (requires TWINE_PASSWORD)"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        facts.python.as_ref().is_some_and(|p| p.publishable)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        let mut job = Job::new(self.id(), self.name(), Stage::Deploy)
            .with_image(IMAGE)
            .with_timeout(15)
            .tags_only()
            .with_secrets(vec!["TWINE_PASSWORD".to_string()])
            .with_steps(vec![
                Step::checkout(),
                Step::run("Install build tools", "pip install build twine"),
                Step::run("Build distribution", "python -m build"),
                Step::run("Upload to PyPI", "twine upload dist/*"),
            ]);
        job.env.push(("TWINE_USERNAME".to_string(), "__token__".to_string()));
        vec![job]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn facts_with(files: &[(&str, &str)]) -> ProjectFacts {
        let dir = tempdir().unwrap();
        for (name, content) in files {
            fs::write(dir.path().join(name), content).unwrap();
        }
        crate::detection::gather_facts(dir.path())
    }

    #[test]
    fn test_detect_from_requirements() {
        let facts = facts_with(&[("requirements.txt", "requests\n")]);
        assert!(PythonTest.detect(&facts));
        assert!(PythonLint.detect(&facts));
        assert!(!PythonRelease.detect(&facts));
    }

    #[test]
    fn test_install_command_prefers_pyproject() {
        let facts = facts_with(&[("pyproject.toml", "[project]\nname = \"p\"\n")]);
        assert_eq!(install_command(&facts), "pip install .");
        let facts = facts_with(&[("requirements.txt", "requests\n")]);
        assert_eq!(install_command(&facts), "pip install -r requirements.txt");
    }

    #[test]
    fn test_release_detected_for_publishable_pyproject() {
        let facts = facts_with(&[(
            "pyproject.toml",
            "[project]\nname = \"p\"\nversion = \"0.1.0\"\n",
        )]);
        assert!(PythonRelease.detect(&facts));
        let jobs = PythonRelease.jobs(&facts);
        assert!(jobs[0].tags_only);
        assert_eq!(jobs[0].secrets, vec!["TWINE_PASSWORD"]);
        assert!(jobs[0]
            .env
            .contains(&("TWINE_USERNAME".to_string(), "__token__".to_string())));
    }
}

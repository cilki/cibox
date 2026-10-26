use super::{with_versions, Rule};
use crate::detection::ProjectFacts;
use crate::ir::{Job, Stage, Step};

pub(crate) const IMAGE: &str = "python:3.12";

fn is_python(facts: &ProjectFacts) -> bool {
    facts.python.is_some()
}

/// Install steps appropriate for the project layout.
///
/// The two are not alternatives: a requirements file pins what the project
/// needs, and `pip install .` installs the project itself so the tests can
/// import it. Which ones are there decides which run — a project with only a
/// `setup.py` has no requirements file to read, and a pyproject.toml that
/// carries nothing but `[tool.*]` config is not something pip can build, so
/// naming either unconditionally puts a step in the pipeline that fails or
/// quietly installs nothing.
///
/// The requirements file goes first so its pins win: pip leaves a requirement
/// alone once it is satisfied, so the project's own looser constraints can't
/// pull a different version in over the top.
fn install_steps(facts: &ProjectFacts) -> Vec<Step> {
    let Some(py) = &facts.python else {
        return Vec::new();
    };
    let mut steps = Vec::new();
    if py.has_requirements_txt {
        steps.push(Step::run(
            "Install requirements",
            "pip install -r requirements.txt",
        ));
    }
    if py.installable {
        steps.push(Step::run("Install project", "pip install ."));
    }
    steps
}

/// Run the test suite with pytest
#[derive(Default)]
pub struct PythonTest {
    /// Toolchain versions to matrix over; empty = the default pinned image
    pub versions: Vec<String>,
}

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
        let mut steps = vec![Step::checkout()];
        steps.extend(install_steps(facts));
        steps.push(Step::run("Install pytest", "pip install pytest"));
        steps.push(Step::run("Run tests", "pytest"));
        let job = Job::new(self.id(), self.name(), Stage::Test)
            .with_image(IMAGE)
            .with_timeout(30)
            .with_steps(steps);
        vec![with_versions(job, &self.versions, |v| format!("python:{v}"))]
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
        vec![Job::new(self.id(), self.name(), Stage::Deploy)
            .with_image(IMAGE)
            .with_timeout(15)
            .tags_only()
            .with_env("TWINE_USERNAME", "__token__")
            .with_secrets(vec!["TWINE_PASSWORD".to_string()])
            .with_steps(vec![
                Step::checkout(),
                Step::run("Install build tools", "pip install build twine"),
                Step::run("Build distribution", "python -m build"),
                Step::run("Upload to PyPI", "twine upload dist/*"),
            ])]
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
        assert!(PythonTest::default().detect(&facts));
        assert!(PythonLint.detect(&facts));
        assert!(!PythonRelease.detect(&facts));
    }

    fn install_commands(facts: &ProjectFacts) -> Vec<String> {
        install_steps(facts)
            .into_iter()
            .map(|step| match step {
                Step::Run { command, .. } => command,
                other => panic!("unexpected install step {other:?}"),
            })
            .collect()
    }

    const REQUIREMENTS: &str = "pip install -r requirements.txt";
    const PROJECT: &str = "pip install .";

    #[test]
    fn test_install_steps_follow_the_project_layout() {
        // A package pip can build is installed from the checkout
        let facts = facts_with(&[("pyproject.toml", "[project]\nname = \"p\"\n")]);
        assert_eq!(install_commands(&facts), [PROJECT]);

        // A requirements file with no package to build is just read
        let facts = facts_with(&[("requirements.txt", "requests\n")]);
        assert_eq!(install_commands(&facts), [REQUIREMENTS]);

        // Both shapes present: the pins are installed, then the project, so
        // the tests can import it
        let facts = facts_with(&[
            ("pyproject.toml", "[project]\nname = \"p\"\n"),
            ("requirements.txt", "requests\n"),
        ]);
        assert_eq!(install_commands(&facts), [REQUIREMENTS, PROJECT]);
    }

    /// `pip install -r requirements.txt` against a project that has no
    /// requirements file fails the job on its first step
    #[test]
    fn test_setup_py_project_installs_itself() {
        let facts = facts_with(&[("setup.py", "from setuptools import setup\nsetup()\n")]);
        assert_eq!(install_commands(&facts), [PROJECT]);
    }

    /// A pyproject.toml holding only tool config is not a distribution:
    /// `pip install .` there builds an empty `UNKNOWN-0.0.0` wheel and
    /// installs none of the dependencies the job needs
    #[test]
    fn test_tool_only_pyproject_is_not_installed_as_a_package() {
        let facts = facts_with(&[
            ("pyproject.toml", "[tool.ruff]\nline-length = 100\n"),
            ("requirements.txt", "requests\n"),
        ]);
        assert_eq!(install_commands(&facts), [REQUIREMENTS]);

        // Nothing to install at all is a shorter job, not a failing one
        let facts = facts_with(&[("pyproject.toml", "[tool.ruff]\nline-length = 100\n")]);
        assert!(install_commands(&facts).is_empty());

        // A build backend without `[project]` (setuptools reading setup.cfg,
        // poetry, pdm) still builds
        for backend in [
            "[build-system]\nrequires = [\"setuptools\"]\n",
            "[build-system]\nrequires = [\"poetry-core\"]\n[tool.poetry]\nname = \"p\"\n",
        ] {
            let facts = facts_with(&[("pyproject.toml", backend)]);
            assert_eq!(install_commands(&facts), [PROJECT], "{backend}");
        }
    }

    /// Force-enabling the rule on a project with no Python at all leaves the
    /// install out rather than naming a file that isn't there
    #[test]
    fn test_no_python_facts_means_no_install_step() {
        assert!(install_commands(&ProjectFacts::default()).is_empty());

        let jobs = PythonTest::default().jobs(&ProjectFacts::default());
        let commands: Vec<&str> = jobs[0]
            .steps
            .iter()
            .filter_map(|step| match step {
                Step::Run { command, .. } => Some(command.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(commands, ["pip install pytest", "pytest"]);
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

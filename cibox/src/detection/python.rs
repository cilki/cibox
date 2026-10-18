use std::fs;
use std::path::Path;

/// Facts about a Python project
#[derive(Debug, Clone, Default)]
pub struct PythonFacts {
    /// A pyproject.toml is present, so the project installs with `pip install .`
    pub has_pyproject: bool,
    /// A requirements.txt is present, so the project's dependencies install
    /// with `pip install -r requirements.txt`
    pub has_requirements_txt: bool,
    /// The checkout is a distribution pip can build, so `pip install .`
    /// installs the project and its dependencies: a `setup.py`, or a
    /// pyproject.toml declaring `[project]` or a `[build-system]`. A
    /// pyproject.toml carrying nothing but tool config (`[tool.ruff]`, …) is
    /// not one — there `pip install .` builds an empty `UNKNOWN-0.0.0`
    /// distribution and installs none of the project's dependencies.
    pub installable: bool,
    /// pyproject.toml declares a `[project]` with a name, so the package can
    /// be built and uploaded to PyPI
    pub publishable: bool,
}

pub(super) fn gather(path: &Path) -> Option<PythonFacts> {
    let has_pyproject = path.join("pyproject.toml").is_file();
    let has_setup_py = path.join("setup.py").is_file();
    let has_requirements_txt = path.join("requirements.txt").is_file();

    if !has_pyproject && !has_setup_py && !has_requirements_txt {
        return None;
    }

    let pyproject = has_pyproject
        .then(|| fs::read_to_string(path.join("pyproject.toml")).ok())
        .flatten()
        .and_then(|contents| contents.parse::<toml::Table>().ok());

    let declares = |key: &str| pyproject.as_ref().is_some_and(|t| t.contains_key(key));
    // An unreadable or malformed pyproject.toml is treated as a build
    // definition we couldn't parse rather than as tool config: pip will make
    // its own attempt at it, and `pip install .` is the better guess for a
    // file that exists at all.
    let unparsed_pyproject = has_pyproject && pyproject.is_none();

    let publishable = pyproject
        .as_ref()
        .and_then(|table| {
            let project = table.get("project")?.as_table()?;
            Some(project.contains_key("name"))
        })
        .unwrap_or(false);

    Some(PythonFacts {
        has_pyproject,
        has_requirements_txt,
        installable: has_setup_py
            || unparsed_pyproject
            || declares("project")
            || declares("build-system"),
        publishable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_no_python_files() {
        let dir = tempdir().unwrap();
        assert!(gather(dir.path()).is_none());
    }

    #[test]
    fn test_requirements_only_is_not_publishable() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("requirements.txt"), "requests\n").unwrap();
        let facts = gather(dir.path()).unwrap();
        assert!(facts.has_requirements_txt);
        assert!(!facts.installable);
        assert!(!facts.publishable);
    }

    #[test]
    fn test_setup_py_is_installable() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("setup.py"),
            "from setuptools import setup\nsetup()\n",
        )
        .unwrap();
        let facts = gather(dir.path()).unwrap();
        assert!(facts.installable);
        assert!(!facts.publishable);
    }

    #[test]
    fn test_installable_needs_a_build_definition_not_just_a_pyproject() {
        // Tool config alone: pip has nothing to build here
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.ruff]\nline-length = 100\n",
        )
        .unwrap();
        assert!(!gather(dir.path()).unwrap().installable);

        // A build backend without `[project]` — poetry, pdm, or setuptools
        // reading setup.cfg — builds all the same
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[build-system]\nrequires = [\"poetry-core\"]\n[tool.poetry]\nname = \"p\"\n",
        )
        .unwrap();
        let facts = gather(dir.path()).unwrap();
        assert!(facts.installable);
        assert!(!facts.publishable, "no [project] to publish from");
    }

    #[test]
    fn test_unparsable_pyproject_is_left_to_pip() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "[project\nname =").unwrap();
        let facts = gather(dir.path()).unwrap();
        assert!(facts.installable);
        assert!(!facts.publishable);
    }

    #[test]
    fn test_pyproject_with_project_name_is_publishable() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"mypkg\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let facts = gather(dir.path()).unwrap();
        assert!(facts.has_pyproject);
        assert!(facts.installable);
        assert!(facts.publishable);
    }

    #[test]
    fn test_pyproject_without_project_section_not_publishable() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.ruff]\nline-length = 100\n",
        )
        .unwrap();
        let facts = gather(dir.path()).unwrap();
        assert!(!facts.publishable);
    }
}

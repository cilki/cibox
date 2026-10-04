use std::fs;
use std::path::Path;

/// Facts about a Python project
#[derive(Debug, Clone, Default)]
pub struct PythonFacts {
    pub has_pyproject: bool,
    pub has_setup_py: bool,
    pub has_requirements_txt: bool,
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

    let publishable = has_pyproject
        && fs::read_to_string(path.join("pyproject.toml"))
            .ok()
            .and_then(|contents| contents.parse::<toml::Table>().ok())
            .and_then(|table| {
                let project = table.get("project")?.as_table()?;
                Some(project.contains_key("name"))
            })
            .unwrap_or(false);

    Some(PythonFacts {
        has_pyproject,
        has_setup_py,
        has_requirements_txt,
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

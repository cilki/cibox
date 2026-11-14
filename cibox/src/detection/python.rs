use std::fs;
use std::path::Path;

/// Facts about a Python project
#[derive(Debug, Clone, Default)]
pub struct PythonFacts {
    pub has_pyproject: bool,
    pub has_setup_py: bool,
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
    /// `[project] requires-python` from pyproject.toml, verbatim: the
    /// interpreter versions pip will agree to install the project on
    pub requires_python: Option<String>,
    /// `[tool.poetry.dependencies] python` from pyproject.toml, verbatim, in
    /// Poetry's own constraint syntax (`^3.11`): the interpreter versions
    /// Poetry will agree to install the project on. Poetry checks the running
    /// interpreter against it before installing anything, and poetry-core
    /// writes it into the built distribution's `Requires-Python`, so pip
    /// enforces it too. Poetry 2 projects declare `[project] requires-python`
    /// instead, which Poetry reads first when both are present.
    pub poetry_python: Option<String>,
    /// The `[project.optional-dependencies]` extra holding the test
    /// requirements (pytest plugins and the like), see [`TEST_SETS`]
    pub test_extra: Option<String>,
    /// The PEP 735 `[dependency-groups]` group holding the test requirements,
    /// see [`TEST_SETS`]
    pub test_group: Option<String>,
    /// The Poetry group holding the test requirements, see [`TEST_SETS`]:
    /// a `[tool.poetry.group.<name>]`, or `dev` for the pre-1.2
    /// `[tool.poetry.dev-dependencies]` table Poetry still reads as one.
    /// Only Poetry can install these — pip knows nothing of them.
    pub poetry_test_group: Option<String>,
    /// pytest has something to run: the project configures pytest, or has a
    /// file pytest collects by default. Without either, `pytest` collects
    /// nothing and exits 5, failing the job on a project that has no tests.
    pub has_tests: bool,
}

/// Directories visited looking for a test file before giving up and
/// assuming there are tests. A project with tests usually has one within the
/// first few directories; this only bounds a tree with none.
const MAX_TEST_SEARCH_DIRS: usize = 2000;

/// Whether pytest would find tests to run in the project at `path`.
///
/// Configuring pytest counts by itself: a project that does may also have
/// changed `python_files` or `testpaths`, and either one moves what gets
/// collected out from under the default patterns.
fn has_tests(path: &Path, pyproject: Option<&toml::Table>) -> bool {
    let tool_pytest = pyproject
        .and_then(|t| t.get("tool")?.get("pytest"))
        .is_some();
    let ini_section = |file: &str, section: &str| {
        fs::read_to_string(path.join(file))
            .is_ok_and(|ini| ini.lines().any(|line| line.trim() == section))
    };
    tool_pytest
        || path.join("pytest.ini").is_file()
        || path.join(".pytest.ini").is_file()
        || ini_section("setup.cfg", "[tool:pytest]")
        || ini_section("tox.ini", "[pytest]")
        || has_test_file(path)
}

/// Whether a file matching pytest's default `python_files` (`test_*.py`,
/// `*_test.py`) is somewhere pytest would look for it. The directories
/// pytest's default `norecursedirs` skips are skipped here too, as are
/// virtualenvs, which it doesn't collect from either. A tree too large to
/// search within [`MAX_TEST_SEARCH_DIRS`] counts as having tests, which keeps
/// the rule on when this can't tell.
fn has_test_file(path: &Path) -> bool {
    let mut stack = vec![path.to_path_buf()];
    let mut budget = MAX_TEST_SEARCH_DIRS;
    while let Some(dir) = stack.pop() {
        if budget == 0 {
            return true;
        }
        budget -= 1;
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                let skipped = name.starts_with('.')
                    || name.ends_with(".egg")
                    || matches!(
                        name,
                        "build" | "dist" | "node_modules" | "venv" | "CVS" | "_darcs" | "{arch}"
                    );
                if !skipped && !entry.path().join("pyvenv.cfg").is_file() {
                    stack.push(entry.path());
                }
            } else if name
                .strip_suffix(".py")
                .is_some_and(|stem| stem.starts_with("test_") || stem.ends_with("_test"))
            {
                return true;
            }
        }
    }
    false
}

/// The names a project's test requirements go by, most specific first: the
/// first one a project declares is what the test job installs. `dev` comes
/// last since it is the catch-all that usually carries pytest when nothing
/// narrower exists.
const TEST_SETS: [&str; 4] = ["test", "tests", "testing", "dev"];

/// The first of [`TEST_SETS`] that `table` has a key for
fn test_set(table: Option<&toml::Value>) -> Option<String> {
    let table = table?.as_table()?;
    TEST_SETS
        .iter()
        .find(|name| table.contains_key(**name))
        .map(|name| name.to_string())
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

    let requires_python = pyproject
        .as_ref()
        .and_then(|table| table.get("project")?.get("requires-python")?.as_str())
        .map(str::to_string);

    let test_extra = test_set(
        pyproject
            .as_ref()
            .and_then(|table| table.get("project")?.get("optional-dependencies")),
    );
    let test_group = test_set(pyproject.as_ref().and_then(|table| table.get("dependency-groups")));
    let poetry = pyproject.as_ref().and_then(|table| table.get("tool")?.get("poetry"));
    let poetry_python = poetry
        .and_then(|poetry| poetry.get("dependencies")?.get("python")?.as_str())
        .map(str::to_string);
    let poetry_test_group = test_set(poetry.and_then(|poetry| poetry.get("group"))).or_else(|| {
        poetry
            .is_some_and(|poetry| poetry.get("dev-dependencies").is_some())
            .then(|| "dev".to_string())
    });
    let has_tests = has_tests(path, pyproject.as_ref());

    Some(PythonFacts {
        has_pyproject,
        has_setup_py,
        has_requirements_txt,
        installable: has_setup_py
            || unparsed_pyproject
            || declares("project")
            || declares("build-system"),
        publishable,
        requires_python,
        poetry_python,
        test_extra,
        test_group,
        poetry_test_group,
        has_tests,
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

    #[test]
    fn test_requires_python_is_read_from_project_table() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"p\"\nrequires-python = \">=3.13\"\n",
        )
        .unwrap();
        let facts = gather(dir.path()).unwrap();
        assert_eq!(facts.requires_python.as_deref(), Some(">=3.13"));

        // Only `[project]` declares it; a tool table's key of the same name
        // says nothing about what pip will accept
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.x]\nrequires-python = \">=3.13\"\n",
        )
        .unwrap();
        assert_eq!(gather(dir.path()).unwrap().requires_python, None);
    }

    fn pyproject_facts(contents: &str) -> PythonFacts {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("pyproject.toml"), contents).unwrap();
        gather(dir.path()).unwrap()
    }

    /// A Poetry project declares its interpreters as a dependency on
    /// `python`, which is kept apart from `requires-python`: the syntax is
    /// Poetry's, not PEP 440's
    #[test]
    fn test_poetry_python_constraint_is_read_from_its_dependencies() {
        let facts = pyproject_facts(
            "[tool.poetry]\nname = \"p\"\n\
             [tool.poetry.dependencies]\npython = \"^3.11\"\nrequests = \"*\"\n",
        );
        assert_eq!(facts.poetry_python.as_deref(), Some("^3.11"));
        assert_eq!(facts.requires_python, None);

        // Both stay available as written; which one wins is the rule's call
        let facts = pyproject_facts(
            "[project]\nname = \"p\"\nrequires-python = \">=3.13\"\n\
             [tool.poetry.dependencies]\npython = \"^3.11\"\n",
        );
        assert_eq!(facts.requires_python.as_deref(), Some(">=3.13"));
        assert_eq!(facts.poetry_python.as_deref(), Some("^3.11"));

        // A Poetry project without the python dependency, or any other
        // dependency named python, declares nothing
        let facts = pyproject_facts("[tool.poetry]\nname = \"p\"\n");
        assert_eq!(facts.poetry_python, None);
        let facts = pyproject_facts("[tool.poetry.group.dev.dependencies]\npython = \"^3.11\"\n");
        assert_eq!(facts.poetry_python, None);
    }

    #[test]
    fn test_test_requirements_are_found_by_their_usual_names() {
        let facts = pyproject_facts(
            "[project]\nname = \"p\"\n[project.optional-dependencies]\n\
             docs = [\"sphinx\"]\ndev = [\"black\"]\ntests = [\"pytest-asyncio\"]\n",
        );
        assert_eq!(facts.test_extra.as_deref(), Some("tests"), "narrowest name wins");
        assert_eq!(facts.test_group, None);

        let facts = pyproject_facts("[dependency-groups]\ndev = [\"pytest\"]\n");
        assert_eq!(facts.test_group.as_deref(), Some("dev"));
        assert_eq!(facts.test_extra, None);

        // Extras nobody would run the tests with are left alone
        let facts = pyproject_facts(
            "[project]\nname = \"p\"\n[project.optional-dependencies]\ndocs = [\"sphinx\"]\n",
        );
        assert_eq!(facts.test_extra, None);
    }

    #[test]
    fn test_poetry_test_group_is_found() {
        let facts = pyproject_facts(
            "[tool.poetry]\nname = \"p\"\n\
             [tool.poetry.group.docs.dependencies]\nsphinx = \"*\"\n\
             [tool.poetry.group.dev.dependencies]\nblack = \"*\"\n\
             [tool.poetry.group.test.dependencies]\npytest-asyncio = \"*\"\n",
        );
        assert_eq!(
            facts.poetry_test_group.as_deref(),
            Some("test"),
            "narrowest name wins"
        );
        assert_eq!(facts.test_group, None);

        // The pre-1.2 table is the `dev` group to Poetry
        let facts = pyproject_facts(
            "[tool.poetry]\nname = \"p\"\n[tool.poetry.dev-dependencies]\npytest = \"*\"\n",
        );
        assert_eq!(facts.poetry_test_group.as_deref(), Some("dev"));

        let facts = pyproject_facts(
            "[tool.poetry]\nname = \"p\"\n[tool.poetry.group.docs.dependencies]\nsphinx = \"*\"\n",
        );
        assert_eq!(facts.poetry_test_group, None);
    }

    fn has_tests_with(files: &[(&str, &str)]) -> bool {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"p\"\n",
        )
        .unwrap();
        for (name, contents) in files {
            let file = dir.path().join(name);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, contents).unwrap();
        }
        gather(dir.path()).unwrap().has_tests
    }

    /// pytest exits 5 when it collects nothing, so a project without tests
    /// would get a test job that fails on every push
    #[test]
    fn test_project_without_tests_has_none() {
        assert!(!has_tests_with(&[]));
        assert!(!has_tests_with(&[
            ("src/p/__init__.py", ""),
            ("src/p/testing.py", ""),
            ("src/p/contest.py", ""),
        ]));
    }

    #[test]
    fn test_default_test_file_patterns_are_found_at_any_depth() {
        assert!(has_tests_with(&[("test_root.py", "")]));
        assert!(has_tests_with(&[("tests/unit/test_x.py", "")]));
        assert!(has_tests_with(&[("src/p/x_test.py", "")]));
    }

    /// pytest's default norecursedirs, and virtualenvs, are never collected
    /// from, so a test file in one doesn't make the project's suite non-empty
    #[test]
    fn test_test_files_where_pytest_does_not_look_are_ignored() {
        assert!(!has_tests_with(&[
            (".tox/py312/test_vendored.py", ""),
            ("build/lib/test_copy.py", ""),
            ("dist/test_x.py", ""),
            ("node_modules/x/test_x.py", ""),
            ("venv/lib/test_x.py", ""),
            ("p.egg/test_x.py", ""),
            ("env/pyvenv.cfg", ""),
            ("env/lib/site-packages/x/test_x.py", ""),
        ]));
    }

    /// Configuring pytest may move collection off the default patterns
    /// (`python_files = check_*.py`, `--doctest-modules`), so it counts
    #[test]
    fn test_pytest_configuration_counts_as_tests() {
        assert!(has_tests_with(&[("pytest.ini", "[pytest]\n")]));
        assert!(has_tests_with(&[(".pytest.ini", "[pytest]\n")]));
        assert!(has_tests_with(&[(
            "setup.cfg",
            "[metadata]\n[tool:pytest]\n"
        )]));
        assert!(has_tests_with(&[(
            "tox.ini",
            "[tox]\n[pytest]\naddopts = -q\n"
        )]));
        // A tox.ini or setup.cfg without a pytest section says nothing
        assert!(!has_tests_with(&[("tox.ini", "[tox]\nenvlist = py312\n")]));
        assert!(!has_tests_with(&[("setup.cfg", "[metadata]\nname = p\n")]));

        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"p\"\n[tool.pytest.ini_options]\naddopts = \"--doctest-modules\"\n",
        )
        .unwrap();
        assert!(gather(dir.path()).unwrap().has_tests);
    }
}

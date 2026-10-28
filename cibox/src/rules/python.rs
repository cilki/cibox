use super::{with_versions, Rule};
use crate::detection::ProjectFacts;
use crate::ir::{Job, Stage, Step};

/// The `python:3.N` images to choose from, oldest first; `IMAGES[i]` is
/// Python 3.`(FIRST_MINOR + i)`
const IMAGES: [&str; 7] = [
    "python:3.8",
    "python:3.9",
    "python:3.10",
    "python:3.11",
    "python:3.12",
    "python:3.13",
    "python:3.14",
];
const FIRST_MINOR: u64 = 8;
/// The minor used whenever the project doesn't rule it out
const DEFAULT_MINOR: u64 = 12;

fn is_python(facts: &ProjectFacts) -> bool {
    facts.python.is_some()
}

/// The image the Python jobs run on when no versions are configured.
///
/// pip refuses to install a project whose `requires-python` excludes the
/// running interpreter, so a fixed default fails `pip install .` outright for
/// a project that needs a newer Python (`>=3.13`) or hasn't caught up to it
/// yet (`<3.12`). The default stands whenever the project allows it;
/// otherwise the nearest minor it does allow is used, the newer on a tie. A
/// specifier this can't read, or one no image satisfies, keeps the default —
/// pip will report it more clearly than a guess would.
pub(crate) fn image(facts: &ProjectFacts) -> &'static str {
    let spec = facts
        .python
        .as_ref()
        .and_then(|py| py.requires_python.as_deref());
    let minor = spec
        .and_then(|spec| {
            let mut nearest: Vec<u64> = (FIRST_MINOR..FIRST_MINOR + IMAGES.len() as u64).collect();
            nearest.sort_by_key(|&m| (m.abs_diff(DEFAULT_MINOR), std::cmp::Reverse(m)));
            nearest
                .into_iter()
                .map(|m| allows(spec, m).map(|ok| ok.then_some(m)))
                .collect::<Option<Vec<_>>>()?
                .into_iter()
                .flatten()
                .next()
        })
        .unwrap_or(DEFAULT_MINOR);
    IMAGES[(minor - FIRST_MINOR) as usize]
}

/// Whether the PEP 440 specifier `spec` admits the `python:3.<minor>` image,
/// or `None` when it can't be read (pre-releases, `===`, environment
/// markers). The image tag follows the minor's patch releases, so it stands
/// for the whole 3.<minor> series: a clause holds when some patch release
/// satisfies it — `<=3.11` and `==3.11` admit 3.11, `>=3.12.4` admits 3.12.
fn allows(spec: &str, minor: u64) -> Option<bool> {
    use std::cmp::Ordering::{Greater, Less};
    // The series' bounds: its first and (any) last patch release
    let (first, last) = ([3, minor, 0], [3, minor, u64::MAX]);
    let cmp = |series: &[u64; 3], v: &[u64]| {
        let at = |s: &[u64], i: usize| s.get(i).copied().unwrap_or(0);
        (0..v.len().max(3))
            .map(|i| at(series, i).cmp(&at(v, i)))
            .find(|o| o.is_ne())
            .unwrap_or(std::cmp::Ordering::Equal)
    };
    // Some release in the series starts with `v`'s segments
    let within = |v: &[u64]| v.len() <= 3 && first.iter().zip(v).take(2).all(|(a, b)| a == b);

    let mut admitted = true;
    for clause in spec.split(',').map(str::trim).filter(|c| !c.is_empty()) {
        let (op, version) = ["~=", "===", "==", "!=", ">=", "<=", ">", "<"]
            .into_iter()
            .find_map(|op| Some((op, clause.strip_prefix(op)?.trim())))?;
        let (version, wildcard) = match version.strip_suffix(".*") {
            Some(v) => (v, true),
            None => (version, false),
        };
        let v: Vec<u64> = version
            .split('.')
            .map(|s| s.parse().ok())
            .collect::<Option<_>>()?;
        if wildcard && !matches!(op, "==" | "!=") {
            return None;
        }
        admitted &= match op {
            ">=" => cmp(&last, &v) != Less,
            ">" => cmp(&last, &v) == Greater,
            "<=" => cmp(&first, &v) != Greater,
            "<" => cmp(&first, &v) == Less,
            "==" => within(&v),
            // Excluding a single release leaves the series' other releases;
            // only a wildcard can exclude all of it
            "!=" => !(wildcard && v.len() <= 2 && within(&v)),
            // `~=X.Y.Z` is `>=X.Y.Z, ==X.Y.*`
            "~=" if v.len() >= 2 => cmp(&last, &v) != Less && within(&v[..v.len() - 1]),
            _ => return None,
        };
    }
    Some(admitted)
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
            .with_image(image(facts))
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

    fn jobs(&self, facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Lint)
            .with_image(image(facts))
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

    fn jobs(&self, facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Lint)
            .with_image(image(facts))
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

    fn jobs(&self, facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Deploy)
            .with_image(image(facts))
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

    fn image_for(requires_python: &str) -> &'static str {
        let pyproject =
            format!("[project]\nname = \"p\"\nrequires-python = \"{requires_python}\"\n");
        image(&facts_with(&[("pyproject.toml", &pyproject)]))
    }

    /// pip refuses `pip install .` on an interpreter the project's
    /// `requires-python` excludes, so the image has to be one it admits
    #[test]
    fn test_image_follows_requires_python() {
        // The default stands whenever it's allowed
        for spec in [
            ">=3.8",
            ">=3.12",
            "<4",
            ">=3.9,<3.14",
            "~=3.10",
            "==3.12.*",
            ">=3.12.4",
        ] {
            assert_eq!(image_for(spec), "python:3.12", "{spec}");
        }
        // Too old for the project: the oldest one it allows
        assert_eq!(image_for(">=3.13"), "python:3.13");
        assert_eq!(image_for(">3.13"), "python:3.13");
        assert_eq!(image_for(">=3.14"), "python:3.14");
        assert_eq!(image_for("~=3.13.1"), "python:3.13");
        // Too new for it: the newest one it allows
        assert_eq!(image_for(">=3.8,<3.12"), "python:3.11");
        assert_eq!(image_for("<=3.10"), "python:3.10");
        assert_eq!(image_for("==3.9.*"), "python:3.9");
        assert_eq!(image_for(">=3.8, !=3.12.*"), "python:3.13");
        // Excluding one release leaves the rest of its series
        assert_eq!(image_for("!=3.12.0"), "python:3.12");
    }

    /// The jobs a project gets run on that image, not on the fixed default
    #[test]
    fn test_jobs_use_the_admitted_image() {
        let facts = facts_with(&[(
            "pyproject.toml",
            "[project]\nname = \"p\"\nrequires-python = \">=3.13\"\n",
        )]);
        let rules: [&dyn Rule; 4] = [
            &PythonTest::default(),
            &PythonLint,
            &PythonFmt,
            &PythonRelease,
        ];
        for rule in rules {
            for job in rule.jobs(&facts) {
                assert_eq!(job.image.as_deref(), Some("python:3.13"), "{}", job.id);
            }
        }
    }

    /// Anything unreadable or unsatisfiable keeps the default rather than
    /// guessing; pip then names the real problem
    #[test]
    fn test_image_falls_back_to_default() {
        for spec in [
            "",
            ">=3.13rc1",
            "===3.13",
            "3.13",
            ">=4",
            "<3.8",
            ">=3.13,<3.12",
        ] {
            assert_eq!(image_for(spec), "python:3.12", "{spec:?}");
        }
        assert_eq!(image(&ProjectFacts::default()), "python:3.12");
        assert_eq!(
            image(&facts_with(&[("requirements.txt", "requests\n")])),
            "python:3.12"
        );
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

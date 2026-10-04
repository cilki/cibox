//! Project fact gathering.
//!
//! Every gatherer runs on every invocation and failures are never fatal: a
//! missing or unparseable file simply means the fact is absent. Rules consult
//! the collected [`ProjectFacts`] to decide whether they apply.

mod docker;
mod git;
mod go;
mod python;
mod rust;

use crate::config::Platform;
use std::path::Path;

pub use docker::DockerFacts;
pub use git::RemoteHost;
pub use go::GoFacts;
pub use python::PythonFacts;
pub use rust::RustFacts;

/// Everything cibox knows about the project directory
#[derive(Debug, Clone, Default)]
pub struct ProjectFacts {
    pub rust: Option<RustFacts>,
    pub python: Option<PythonFacts>,
    pub go: Option<GoFacts>,
    pub docker: Option<DockerFacts>,
    pub is_git_repo: bool,
    /// "owner/repo" parsed from the origin remote URL
    pub repo_slug: Option<String>,
    pub remote_host: Option<RemoteHost>,
    /// Platforms with existing CI configuration in the repository
    pub existing_ci: Vec<Platform>,
    /// Directory basename, the last-resort default for image naming
    pub dir_name: String,
}

/// Gather all project facts. Never fails: anything unreadable is just absent.
pub fn gather_facts(path: &Path) -> ProjectFacts {
    let dir_name = path
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "app".to_string());

    let git = git::gather(path);

    ProjectFacts {
        rust: rust::gather(path),
        python: python::gather(path),
        go: go::gather(path),
        docker: docker::gather(path),
        is_git_repo: git.is_repo,
        repo_slug: git.repo_slug,
        remote_host: git.remote_host,
        existing_ci: existing_ci(path),
        dir_name,
    }
}

/// Platforms that already have CI configuration in the repository
fn existing_ci(path: &Path) -> Vec<Platform> {
    let present = |p: Platform| -> bool {
        match p {
            Platform::GitHub => path.join(".github/workflows").is_dir(),
            Platform::Gitea => path.join(".gitea/workflows").is_dir(),
            Platform::GitLab => path.join(".gitlab-ci.yml").is_file(),
            Platform::CircleCI => path.join(".circleci").is_dir(),
        }
    };
    Platform::all().into_iter().filter(|&p| present(p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_empty_directory_has_no_facts() {
        let dir = tempdir().unwrap();
        let facts = gather_facts(dir.path());
        assert!(facts.rust.is_none());
        assert!(facts.python.is_none());
        assert!(facts.go.is_none());
        assert!(facts.docker.is_none());
        assert!(!facts.is_git_repo);
        assert!(facts.existing_ci.is_empty());
    }

    #[test]
    fn test_multiple_environments_detected_together() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM rust:latest\n").unwrap();

        let facts = gather_facts(dir.path());
        assert!(facts.rust.is_some());
        assert!(facts.docker.is_some());
    }

    #[test]
    fn test_unparseable_cargo_toml_is_absent_not_fatal() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "not [ valid { toml").unwrap();
        let facts = gather_facts(dir.path());
        assert!(facts.rust.is_none());
    }

    #[test]
    fn test_existing_ci_detected() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".github/workflows")).unwrap();
        fs::write(dir.path().join(".gitlab-ci.yml"), "stages: []\n").unwrap();
        let facts = gather_facts(dir.path());
        assert_eq!(facts.existing_ci, vec![Platform::GitHub, Platform::GitLab]);
    }
}

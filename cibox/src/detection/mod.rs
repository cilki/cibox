//! Project fact gathering.
//!
//! Every gatherer runs on every invocation and failures are never fatal: a
//! missing or unparseable file simply means the fact is absent. Rules consult
//! the collected [`ProjectFacts`] to decide whether they apply.

mod android;
mod cmake;
mod docker;
mod git;
mod go;
mod node;
mod python;
mod rust;
mod zig;

use crate::config::Platform;
use std::path::{Path, PathBuf};

pub use android::AndroidFacts;
pub use cmake::CmakeFacts;
pub use docker::DockerFacts;
pub use git::RemoteHost;
pub use go::GoFacts;
pub use node::{NodeFacts, NodePackageManager};
pub use python::PythonFacts;
pub use rust::RustFacts;
pub use zig::ZigFacts;

/// Everything cibox knows about the project directory
#[derive(Debug, Clone, Default)]
pub struct ProjectFacts {
    pub rust: Option<RustFacts>,
    pub python: Option<PythonFacts>,
    pub go: Option<GoFacts>,
    pub node: Option<NodeFacts>,
    pub zig: Option<ZigFacts>,
    pub cmake: Option<CmakeFacts>,
    pub docker: Option<DockerFacts>,
    pub android: Option<AndroidFacts>,
    pub is_git_repo: bool,
    /// "owner/repo" parsed from the origin remote URL
    pub repo_slug: Option<String>,
    pub remote_host: Option<RemoteHost>,
    /// Platforms with existing CI configuration in the repository
    pub existing_ci: Vec<Platform>,
    /// Platforms with at least one file cibox manages on disk (see
    /// [`Platform::managed_paths`]) — what a flagless `cibox update` targets
    pub managed_ci: Vec<Platform>,
    /// Directory basename, the last-resort default for image naming
    pub dir_name: String,
}

/// The directory cibox works in when started in `dir`: the top of the git
/// repository `dir` sits in, or `dir` itself outside of one.
///
/// Every platform reads its pipeline from the repository root, so a pipeline
/// written anywhere else is never run — and run from a subdirectory, the
/// facts are wrong too: no `.git` there means no remote, so the platform,
/// image name and AUR package fall back to guesses. The nearest `.git` (a
/// directory, or the file a submodule or linked worktree has instead) marks
/// the root, the same one `git` itself would pick.
///
/// The result stays relative when `dir` is (`../..`), so the paths `update`
/// reports read the way the user would type them.
pub fn project_root(dir: &Path) -> PathBuf {
    let depth = dir.canonicalize().ok().and_then(|absolute| {
        absolute
            .ancestors()
            .position(|ancestor| ancestor.join(".git").exists())
    });
    match depth {
        Some(depth) if depth > 0 => {
            let up: PathBuf = std::iter::repeat_n("..", depth).collect();
            if dir == Path::new(".") {
                up
            } else {
                dir.join(up)
            }
        }
        _ => dir.to_path_buf(),
    }
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
        node: node::gather(path),
        zig: zig::gather(path),
        cmake: cmake::gather(path),
        docker: docker::gather(path),
        android: android::gather(path),
        is_git_repo: git.is_repo,
        repo_slug: git.repo_slug,
        remote_host: git.remote_host,
        existing_ci: existing_ci(path),
        managed_ci: managed_ci(path),
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

/// Platforms with a cibox-managed pipeline file in the repository. Unlike
/// [`existing_ci`], other CI files (a hand-written `deploy.yml`) don't count.
pub fn managed_ci(path: &Path) -> Vec<Platform> {
    Platform::all()
        .into_iter()
        .filter(|p| p.managed_paths().iter().any(|f| path.join(f).is_file()))
        .collect()
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
        assert!(facts.node.is_none());
        assert!(facts.zig.is_none());
        assert!(facts.cmake.is_none());
        assert!(facts.docker.is_none());
        assert!(facts.android.is_none());
        assert!(!facts.is_git_repo);
        assert!(facts.existing_ci.is_empty());
        assert!(facts.managed_ci.is_empty());
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

    #[test]
    fn test_managed_ci_counts_only_cibox_files() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".github/workflows")).unwrap();
        fs::write(
            dir.path().join(".github/workflows/deploy.yml"),
            "on: push\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join(".gitea/workflows")).unwrap();
        fs::write(
            dir.path().join(".gitea/workflows/release.yml"),
            "on: push\n",
        )
        .unwrap();
        fs::write(dir.path().join(".gitlab-ci.yml"), "stages: []\n").unwrap();
        let facts = gather_facts(dir.path());
        // GitHub only has a workflow cibox doesn't manage
        assert_eq!(facts.managed_ci, vec![Platform::Gitea, Platform::GitLab]);
        assert_eq!(
            facts.existing_ci,
            vec![Platform::GitHub, Platform::Gitea, Platform::GitLab]
        );
    }

    /// Run from inside a repository, cibox works at its top, where the
    /// platforms look for the pipeline and where the remote can be read
    #[test]
    fn test_project_root_is_the_repository_top() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        let member = repo.join("crates/member");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(&member).unwrap();

        assert_eq!(project_root(&repo), repo);
        let root = project_root(&member);
        assert_eq!(root, member.join("../.."));
        assert_eq!(root.canonicalize().unwrap(), repo.canonicalize().unwrap());
    }

    /// A submodule or linked worktree has a `.git` file instead, and is a
    /// repository of its own with its own pipeline
    #[test]
    fn test_project_root_stops_at_the_nearest_repository() {
        let dir = tempdir().unwrap();
        let outer = dir.path().join("outer");
        let sub = outer.join("sub");
        fs::create_dir_all(outer.join(".git")).unwrap();
        fs::create_dir_all(sub.join("src")).unwrap();
        fs::write(sub.join(".git"), "gitdir: ../.git/modules/sub\n").unwrap();

        let root = project_root(&sub.join("src"));
        assert_eq!(root.canonicalize().unwrap(), sub.canonicalize().unwrap());
    }

    /// Outside of a repository there is nothing to walk up to
    #[test]
    fn test_project_root_outside_a_repository_is_the_directory() {
        let dir = tempdir().unwrap();
        let project = dir.path().join("project");
        fs::create_dir_all(&project).unwrap();
        assert_eq!(project_root(&project), project);
    }
}

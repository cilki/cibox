//! Turn resolved rules into output files for a platform.

mod merge;

pub use merge::{merge_file, MergeOutcome};

use crate::config::Platform;
use crate::detection::ProjectFacts;
use crate::error::Result;
use crate::ir::Job;
use crate::platforms::circleci::lower::lower_circleci;
use crate::platforms::github::lower::lower_github;
pub use crate::platforms::github::lower::WorkflowKind;
use crate::platforms::gitlab::lower::lower_gitlab;
use crate::rules::{enabled_jobs, ResolvedRule};
use anyhow::bail;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Reported when a resolution leaves no rule enabled, so there is nothing to
/// write anywhere.
pub const NOTHING_ENABLED: &str =
    "No rules enabled — nothing to generate. Run `cibox detect` to see why.";

/// One output file cibox manages: where it goes and the jobs it should hold.
///
/// `jobs` may be empty (e.g. a GitHub `release.yml` when no release rules are
/// enabled, or every file when no rule is enabled at all) so the update path
/// can strip stale cibox jobs from such a file.
#[derive(Debug, Clone)]
pub struct PlannedFile {
    pub path: PathBuf,
    pub jobs: Vec<Job>,
    pub kind: WorkflowKind,
}

/// Plan the output files for one platform without rendering them.
///
/// GitHub/Gitea get a `ci.yml` for branch/PR jobs and a separate `release.yml`
/// for tags-only jobs; the other platforms are single-file and gate release
/// jobs within the document.
///
/// Planning succeeds with no enabled rules: the resulting job-less files are
/// what lets `update` prune cibox's jobs out of an existing pipeline. Callers
/// that need something to write (see [`generate`]) reject that themselves.
pub fn plan(
    facts: &ProjectFacts,
    resolved: &[ResolvedRule],
    platform: Platform,
) -> Result<Vec<PlannedFile>> {
    let jobs = enabled_jobs(facts, resolved);

    match platform {
        Platform::GitHub | Platform::Gitea => {
            let workflow_dir = match platform {
                Platform::GitHub => ".github/workflows",
                _ => ".gitea/workflows",
            };
            let (release, ci): (Vec<Job>, Vec<Job>) =
                jobs.into_iter().partition(|job| job.tags_only);

            Ok(vec![
                PlannedFile {
                    path: PathBuf::from(workflow_dir).join("ci.yml"),
                    jobs: ci,
                    kind: WorkflowKind::Ci,
                },
                PlannedFile {
                    path: PathBuf::from(workflow_dir).join("release.yml"),
                    jobs: release,
                    kind: WorkflowKind::Release,
                },
            ])
        }
        Platform::GitLab | Platform::CircleCI => {
            if let Some(job) = jobs.iter().find(|j| j.runs_on == crate::ir::RunnerOs::Windows) {
                bail!(
                    "job '{}' needs a Windows runner, which is only supported on GitHub/Gitea; \
                     remove WindowsAmd64 from docker_release.platforms or switch platform",
                    job.id
                );
            }
            Ok(vec![PlannedFile {
                path: platform.output_path(),
                jobs,
                kind: WorkflowKind::Ci,
            }])
        }
    }
}

/// Render the full canonical content of one planned file.
pub fn render_file(platform: Platform, file: &PlannedFile) -> Result<String> {
    // Gitea Actions uses the GitHub Actions workflow format
    let mut content = match platform {
        Platform::GitHub | Platform::Gitea => {
            serde_yaml::to_string(&lower_github(&file.jobs, file.kind))?
        }
        Platform::GitLab => serde_yaml::to_string(&lower_gitlab(&file.jobs))?,
        Platform::CircleCI => serde_yaml::to_string(&lower_circleci(&file.jobs))?,
    };
    prepend_required_variables(platform, &file.jobs, &mut content);
    Ok(content)
}

/// GitLab and CircleCI read secrets from ambient CI variables rather than
/// naming them in the pipeline, so list what the jobs expect at the top of
/// the file to make setup discoverable. A no-op on the other platforms,
/// whose lowered jobs reference their secrets inline.
pub(crate) fn prepend_required_variables(platform: Platform, jobs: &[Job], content: &mut String) {
    if !matches!(platform, Platform::GitLab | Platform::CircleCI) {
        return;
    }
    let secrets: BTreeSet<&str> = jobs
        .iter()
        .flat_map(|job| job.secrets.iter().map(String::as_str))
        .collect();
    if !secrets.is_empty() {
        let names = secrets.into_iter().collect::<Vec<_>>().join(", ");
        content.insert_str(0, &format!("# Required CI variables: {names}\n"));
    }
}

/// Generate all output files for one platform, skipping empty ones.
///
/// Unlike [`plan`], this is for callers that write whole files and so have
/// nothing to say when no rule is enabled.
pub fn generate(
    facts: &ProjectFacts,
    resolved: &[ResolvedRule],
    platform: Platform,
) -> Result<Vec<(PathBuf, String)>> {
    let planned = plan(facts, resolved, platform)?;
    if planned.iter().all(|file| file.jobs.is_empty()) {
        bail!("{NOTHING_ENABLED}");
    }
    planned
        .into_iter()
        .filter(|file| !file.jobs.is_empty())
        .map(|file| Ok((file.path.clone(), render_file(platform, &file)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CiboxConfig;
    use crate::rules::resolve;
    use std::fs;
    use tempfile::tempdir;

    /// Facts for a publishable Rust project with a Dockerfile, in a git repo
    fn full_facts() -> ProjectFacts {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM rust:latest\n").unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        crate::detection::gather_facts(dir.path())
    }

    #[test]
    fn test_plan_always_includes_release_file_for_github() {
        // Even with every release rule disabled, the release.yml entry is
        // planned (with no jobs) so `update` can prune stale jobs from it
        let facts = full_facts();
        let mut config = CiboxConfig::default();
        config.rust_release.enabled = Some(false);
        config.docker_release.enabled = Some(false);
        let resolved = resolve(&facts, &config);

        let planned = plan(&facts, &resolved, Platform::GitHub).unwrap();
        let release = planned
            .iter()
            .find(|f| f.path.ends_with("release.yml"))
            .expect("release.yml planned");
        assert!(release.jobs.is_empty());
        assert_eq!(release.kind, WorkflowKind::Release);

        // generate() skips the empty file, as before
        let outputs = generate(&facts, &resolved, Platform::GitHub).unwrap();
        assert!(outputs.iter().all(|(p, _)| !p.ends_with("release.yml")));
    }

    #[test]
    fn test_github_splits_ci_and_release() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let outputs = generate(&facts, &resolved, Platform::GitHub).unwrap();

        let paths: Vec<_> = outputs.iter().map(|(p, _)| p.clone()).collect();
        assert_eq!(
            paths,
            vec![
                PathBuf::from(".github/workflows/ci.yml"),
                PathBuf::from(".github/workflows/release.yml"),
            ]
        );

        let ci = &outputs[0].1;
        let release = &outputs[1].1;
        assert!(ci.contains("rust-test"), "{ci}");
        assert!(!ci.contains("rust-release"), "{ci}");
        assert!(release.contains("rust-release"), "{release}");
        assert!(release.contains("docker-release"), "{release}");
        assert!(release.contains("v*"), "{release}");
    }

    #[test]
    fn test_single_file_platforms_merge_everything() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());

        for platform in [Platform::GitLab, Platform::CircleCI] {
            let outputs = generate(&facts, &resolved, platform).unwrap();
            assert_eq!(outputs.len(), 1, "{platform:?}");
            assert_eq!(outputs[0].0, platform.output_path());
            assert!(
                outputs[0].1.contains("Required CI variables"),
                "{platform:?} should list required secrets"
            );
        }
    }

    #[test]
    fn test_gitlab_single_document_with_merged_stages() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let outputs = generate(&facts, &resolved, Platform::GitLab).unwrap();
        let yaml = &outputs[0].1;
        assert!(yaml.contains("rust-test"), "{yaml}");
        assert!(yaml.contains("docker-build"), "{yaml}");
        assert_eq!(yaml.matches("stages:").count(), 1, "{yaml}");
        // Strip the leading secrets comment and ensure valid YAML
        serde_yaml::from_str::<crate::platforms::gitlab::models::GitLabCI>(yaml).unwrap();
    }

    #[test]
    fn test_no_rules_enabled_is_an_error() {
        let facts = ProjectFacts::default();
        let resolved = resolve(&facts, &CiboxConfig::default());
        assert!(generate(&facts, &resolved, Platform::GitHub).is_err());
    }

    #[test]
    fn test_plan_with_nothing_enabled_still_names_the_managed_files() {
        // `update` prunes through the planned files, so an empty resolution has
        // to plan them anyway — otherwise disabling every rule (or deleting the
        // last detected fact) would strand cibox's jobs in the pipeline
        let empty = ProjectFacts::default();
        let empty_resolved = resolve(&empty, &CiboxConfig::default());
        assert!(empty_resolved.iter().all(|r| !r.enabled), "fixture rot");

        let full = full_facts();
        let full_resolved = resolve(&full, &CiboxConfig::default());

        for platform in Platform::all() {
            let planned = plan(&empty, &empty_resolved, platform).unwrap();
            assert!(
                planned.iter().all(|file| file.jobs.is_empty()),
                "{platform:?} planned jobs out of nothing"
            );
            // ...and they are the same files a populated project would manage
            let paths: Vec<_> = planned.iter().map(|f| f.path.clone()).collect();
            let full_paths: Vec<_> = plan(&full, &full_resolved, platform)
                .unwrap()
                .iter()
                .map(|f| f.path.clone())
                .collect();
            assert!(!full_paths.is_empty(), "{platform:?} manages no files");
            assert_eq!(paths, full_paths, "{platform:?}");
        }
    }

    #[test]
    fn test_all_platforms_render_valid_output() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());

        for platform in Platform::all() {
            let outputs = generate(&facts, &resolved, platform).unwrap();
            assert!(!outputs.is_empty(), "{platform:?} produced no output");
            for (path, content) in outputs {
                serde_yaml::from_str::<serde_yaml::Value>(&content)
                    .unwrap_or_else(|e| panic!("{path:?} is not valid YAML: {e}"));
            }
        }
    }

    #[test]
    fn test_versioned_config_renders_valid_output_everywhere() {
        let facts = full_facts();
        let mut config = CiboxConfig::default();
        config.rust_test.versions = Some(vec!["1.85".to_string(), "nightly".to_string()]);
        let resolved = resolve(&facts, &config);

        for platform in Platform::all() {
            let marker = match platform {
                Platform::GitHub | Platform::Gitea => "matrix:",
                Platform::GitLab => "parallel:",
                Platform::CircleCI => "<< parameters.image >>",
            };
            let outputs = generate(&facts, &resolved, platform).unwrap();
            let ci = outputs
                .iter()
                .find(|(_, content)| content.contains("rust-test"))
                .unwrap_or_else(|| panic!("{platform:?} has no rust-test"));
            serde_yaml::from_str::<serde_yaml::Value>(&ci.1)
                .unwrap_or_else(|e| panic!("{platform:?} invalid YAML: {e}"));
            assert!(ci.1.contains(marker), "{platform:?}: {}", ci.1);
            // The job key stays the bare rule id
            assert!(ci.1.contains("rust-test"), "{platform:?}");
        }
    }

    #[test]
    fn test_mixed_docker_platforms_on_github() {
        let facts = full_facts();
        let mut config = CiboxConfig::default();
        config.docker_release.platforms = Some(vec![
            crate::config::DockerPlatform::LinuxAmd64,
            crate::config::DockerPlatform::WindowsAmd64,
        ]);
        let resolved = resolve(&facts, &config);

        let outputs = generate(&facts, &resolved, Platform::GitHub).unwrap();
        let release = &outputs
            .iter()
            .find(|(p, _)| p.ends_with("release.yml"))
            .unwrap()
            .1;
        assert!(release.contains("docker-release-linux"), "{release}");
        assert!(release.contains("docker-release-windows"), "{release}");
        assert!(release.contains("windows-latest"), "{release}");
        assert!(release.contains("imagetools create"), "{release}");
        assert!(release.contains("needs:"), "{release}");
    }

    #[test]
    fn test_windows_platform_errors_outside_github() {
        let facts = full_facts();
        let mut config = CiboxConfig::default();
        config.docker_release.platforms =
            Some(vec![crate::config::DockerPlatform::WindowsAmd64]);
        let resolved = resolve(&facts, &config);

        for platform in [Platform::GitLab, Platform::CircleCI] {
            let err = generate(&facts, &resolved, platform).unwrap_err();
            assert!(
                err.to_string().contains("Windows runner"),
                "{platform:?}: {err}"
            );
        }
        // GitHub and Gitea are fine
        generate(&facts, &resolved, Platform::GitHub).unwrap();
        generate(&facts, &resolved, Platform::Gitea).unwrap();
    }

    #[test]
    fn test_repo_config_parses_and_generates() {
        // The repo's own cibox.ron should parse and generate everywhere
        let config =
            crate::config::ron_types::parse_config(include_str!("../../../cibox.ron")).unwrap();
        let facts = full_facts();
        let resolved = resolve(&facts, &config);
        for platform in Platform::all() {
            let outputs = generate(&facts, &resolved, platform).unwrap();
            assert!(!outputs.is_empty(), "{platform:?} produced no output");
        }
    }
}

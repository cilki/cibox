//! Turn resolved rules into output files for a platform.

use crate::config::Platform;
use crate::detection::ProjectFacts;
use crate::error::Result;
use crate::ir::Job;
use crate::platforms::github::lower::{lower_github, WorkflowKind};
use crate::platforms::helpers::PlatformConfig;
use crate::rules::{enabled_jobs, ResolvedRule};
use anyhow::bail;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Generate all output files for one platform.
///
/// GitHub/Gitea get a `ci.yml` for branch/PR jobs and a separate `release.yml`
/// for tags-only jobs; the other platforms are single-file and gate release
/// jobs within the document.
pub fn generate(
    facts: &ProjectFacts,
    resolved: &[ResolvedRule],
    platform: Platform,
) -> Result<Vec<(PathBuf, String)>> {
    let jobs = enabled_jobs(facts, resolved);
    if jobs.is_empty() {
        bail!("No rules enabled — nothing to generate. Run `cibox detect` to see why.");
    }

    match platform {
        Platform::GitHub | Platform::Gitea => {
            let workflow_dir = match platform {
                Platform::GitHub => ".github/workflows",
                _ => ".gitea/workflows",
            };
            let (release, ci): (Vec<Job>, Vec<Job>) =
                jobs.into_iter().partition(|job| job.tags_only);

            let mut outputs = Vec::new();
            if !ci.is_empty() {
                let workflow = lower_github(&ci, WorkflowKind::Ci);
                outputs.push((
                    PathBuf::from(workflow_dir).join("ci.yml"),
                    serde_yaml::to_string(&workflow)?,
                ));
            }
            if !release.is_empty() {
                let workflow = lower_github(&release, WorkflowKind::Release);
                outputs.push((
                    PathBuf::from(workflow_dir).join("release.yml"),
                    serde_yaml::to_string(&workflow)?,
                ));
            }
            Ok(outputs)
        }
        Platform::GitLab | Platform::CircleCI | Platform::Jenkins => {
            let lowered = crate::platforms::lower::lower(platform, &jobs)?;
            let mut content = lowered.render()?;

            // These platforms read secrets from ambient CI variables; list
            // what the jobs expect so setup is discoverable
            let secrets: BTreeSet<&str> = jobs
                .iter()
                .flat_map(|job| job.secrets.iter().map(String::as_str))
                .collect();
            if !secrets.is_empty() {
                let names = secrets.into_iter().collect::<Vec<_>>().join(", ");
                let comment = match lowered {
                    PlatformConfig::Jenkins(_) => format!("// Required CI variables: {names}\n"),
                    _ => format!("# Required CI variables: {names}\n"),
                };
                content.insert_str(0, &comment);
            }

            Ok(vec![(platform.output_path(), content)])
        }
    }
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

        for platform in [Platform::GitLab, Platform::CircleCI, Platform::Jenkins] {
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
    fn test_all_platforms_render_valid_output() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());

        for platform in Platform::all() {
            let outputs = generate(&facts, &resolved, platform).unwrap();
            assert!(!outputs.is_empty(), "{platform:?} produced no output");
            for (path, content) in outputs {
                match platform {
                    Platform::Jenkins => {
                        assert_eq!(content.matches("pipeline {").count(), 1, "{path:?}")
                    }
                    _ => {
                        serde_yaml::from_str::<serde_yaml::Value>(&content)
                            .unwrap_or_else(|e| panic!("{path:?} is not valid YAML: {e}"));
                    }
                }
            }
        }
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

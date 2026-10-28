//! Turn resolved rules into output files for a platform.

mod disabled;
mod merge;

pub use merge::{merge_file, Counts, MergeOutcome};

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
use std::path::{Path, PathBuf};

/// Which jobs `update` may touch: ownership (per [`crate::rules::Rule::owns_job_id`])
/// narrowed by an optional `--rule` selection.
pub struct JobFilter<'a> {
    pub resolved: &'a [ResolvedRule],
    /// Rule ids named with `--rule`; `None` means every rule is selected
    pub selection: Option<&'a BTreeSet<String>>,
}

impl JobFilter<'_> {
    /// Some cibox rule emits this job id (whether or not it is selected)
    pub(crate) fn owned(&self, id: &str) -> bool {
        self.resolved.iter().any(|r| r.rule.owns_job_id(id))
    }

    /// cibox may touch this job: owned, and its rule is selected
    pub(crate) fn managed(&self, id: &str) -> bool {
        match self.selection {
            None => self.owned(id),
            Some(sel) => self
                .resolved
                .iter()
                .any(|r| sel.contains(r.rule.id()) && r.rule.owns_job_id(id)),
        }
    }
}

/// The planned jobs the filter lets `update` touch, with `needs` pruned to
/// jobs that are themselves included (a `--rule` selection may exclude a
/// dependency).
pub fn managed_jobs(file: &PlannedFile, filter: &JobFilter) -> Vec<Job> {
    let mut jobs: Vec<Job> = file
        .jobs
        .iter()
        .filter(|j| filter.managed(&j.id))
        .cloned()
        .collect();
    let ids: Vec<String> = jobs.iter().map(|j| j.id.clone()).collect();
    for job in &mut jobs {
        job.needs.retain(|n| ids.contains(n));
    }
    jobs
}

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
/// what lets `update` prune cibox's jobs out of an existing pipeline. The
/// command rejects a resolution that leaves nothing to write or prune itself.
pub fn plan(
    facts: &ProjectFacts,
    resolved: &[ResolvedRule],
    platform: Platform,
) -> Result<Vec<PlannedFile>> {
    let jobs = enabled_jobs(facts, resolved);

    match platform {
        Platform::GitHub | Platform::Gitea => {
            let [ci_path, release_path]: [PathBuf; 2] = platform
                .managed_paths()
                .try_into()
                .expect("GitHub/Gitea manage ci.yml and release.yml");
            let (release, ci): (Vec<Job>, Vec<Job>) =
                jobs.into_iter().partition(|job| job.tags_only);

            Ok(vec![
                PlannedFile {
                    path: ci_path,
                    jobs: ci,
                    kind: WorkflowKind::Ci,
                },
                PlannedFile {
                    path: release_path,
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
    reconcile_required_variables(platform, &file.jobs, &mut content);
    Ok(content)
}

/// Prefix of the secrets header comment on ambient-variable platforms
pub(crate) const REQUIRED_VARIABLES_PREFIX: &str = "# Required CI variables: ";

/// The secrets header line the given jobs call for, newline included, or
/// `None` when the platform names secrets inline or no job wants any.
///
/// Credentials and plain configuration are one list here: both arrive as
/// project-level CI variables on these platforms, and the user has to define
/// either way. (Which of them to mark masked/protected is a GitLab setting,
/// not something the pipeline can state.)
pub(crate) fn required_variables_line(platform: Platform, jobs: &[Job]) -> Option<String> {
    if !matches!(platform, Platform::GitLab | Platform::CircleCI) {
        return None;
    }
    let secrets: BTreeSet<&str> = jobs
        .iter()
        .flat_map(|job| job.secrets.iter().chain(job.variables.iter()))
        .map(String::as_str)
        .collect();
    if secrets.is_empty() {
        return None;
    }
    let names = secrets.into_iter().collect::<Vec<_>>().join(", ");
    Some(format!("{REQUIRED_VARIABLES_PREFIX}{names}\n"))
}

/// Bring the leading secrets header of `content` in line with what the jobs
/// require: replace a stale one, drop an obsolete one, add a missing one.
///
/// GitLab and CircleCI read secrets from ambient CI variables rather than
/// naming them in the pipeline, so what the jobs expect is listed at the top
/// of the file to make setup discoverable. A no-op on the other platforms,
/// whose lowered jobs reference their secrets inline.
///
/// Freshly rendered output carries no header yet, so for [`render_file`] this
/// is a plain prepend; a merged document keeps its existing text, header
/// included, and is the case that needs reconciling.
pub(crate) fn reconcile_required_variables(platform: Platform, jobs: &[Job], content: &mut String) {
    let desired = required_variables_line(platform, jobs);
    if content.starts_with(REQUIRED_VARIABLES_PREFIX) {
        let end = content.find('\n').map_or(content.len(), |i| i + 1);
        if content[..end] == *desired.as_deref().unwrap_or("") {
            return;
        }
        content.replace_range(..end, "");
    }
    if let Some(line) = desired {
        content.insert_str(0, &line);
    }
}

/// What `cibox update` would do to one managed file
#[derive(Debug)]
pub struct FileUpdate {
    /// Relative to the working directory
    pub path: PathBuf,
    /// The file's current content; `None` when it doesn't exist yet
    pub existing: Option<String>,
    pub change: FileChange,
}

#[derive(Debug)]
pub enum FileChange {
    /// Already matches byte for byte; left alone
    Unchanged,
    /// Merging would strip every active job; left alone (see
    /// [`MergeOutcome::WouldEmpty`])
    WouldEmpty,
    /// Written with `content`. `counts` is `Some` for a merge into an
    /// existing file, `None` when the file gets the full canonical output
    /// (missing, empty, or `--force`).
    Write {
        content: String,
        counts: Option<Counts>,
    },
}

/// Decide what `cibox update` does to each planned file, without writing
/// anything — the one place that decision is made, shared by the command and
/// the editor's preview so the two can't drift apart.
///
/// A missing or empty file gets the full canonical content (as does
/// `force`), narrowed to the jobs `filter` manages; a file with content is
/// merged, so cibox-managed jobs are conformed, re-added, or pruned and
/// everything the user did to it is kept. Files there is nothing to create
/// for are left out of the result.
pub fn pending_update(
    working_dir: &Path,
    platform: Platform,
    planned: &[PlannedFile],
    filter: &JobFilter,
    force: bool,
) -> Result<Vec<FileUpdate>> {
    let mut updates = Vec::new();
    for file in planned {
        let existing = std::fs::read_to_string(working_dir.join(&file.path)).ok();
        let change = match existing.as_deref() {
            Some(text) if !force && !text.trim().is_empty() => {
                match merge_file(platform, file, filter, text)? {
                    MergeOutcome::Unchanged => FileChange::Unchanged,
                    MergeOutcome::WouldEmpty => FileChange::WouldEmpty,
                    MergeOutcome::Merged { content, counts } => FileChange::Write {
                        content,
                        counts: Some(counts),
                    },
                }
            }
            _ => {
                let fresh = PlannedFile {
                    jobs: managed_jobs(file, filter),
                    ..file.clone()
                };
                if fresh.jobs.is_empty() {
                    continue;
                }
                FileChange::Write {
                    content: render_file(platform, &fresh)?,
                    counts: None,
                }
            }
        };
        updates.push(FileUpdate {
            path: file.path.clone(),
            existing,
            change,
        });
    }
    Ok(updates)
}

/// The planned files of every target platform, in order.
pub type Plans = Vec<(Platform, Vec<PlannedFile>)>;

/// [`plan`] each of `platforms`. All of them are planned before anything is
/// decided or written, so one platform refusing a job (e.g. a Windows runner
/// on GitLab) stops the whole update rather than leaving it half applied.
pub fn plan_all(
    facts: &ProjectFacts,
    resolved: &[ResolvedRule],
    platforms: &[Platform],
) -> Result<Plans> {
    platforms
        .iter()
        .map(|&platform| {
            let files = plan(facts, resolved, platform).map_err(|e| {
                e.context(format!(
                    "Failed to generate CI configuration for {}",
                    platform.name()
                ))
            })?;
            Ok((platform, files))
        })
        .collect()
}

/// [`pending_update`] for every platform in `plans`, concatenated in order
pub fn pending_updates(
    working_dir: &Path,
    plans: &Plans,
    filter: &JobFilter,
    force: bool,
) -> Result<Vec<FileUpdate>> {
    let mut updates = Vec::new();
    for (platform, files) in plans {
        updates.extend(pending_update(
            working_dir,
            *platform,
            files,
            filter,
            force,
        )?);
    }
    Ok(updates)
}

/// What `cibox update` would write into `working_dir` for one platform with
/// every rule selected: the files it would change, and their new content.
///
/// A test fixture. It goes through [`pending_update`], so a pipeline a test
/// stands up is the one the command writes rather than one rendered a second
/// way — and a file update would leave alone is absent here too.
#[cfg(test)]
pub(crate) fn update_files(
    working_dir: &Path,
    facts: &ProjectFacts,
    resolved: &[ResolvedRule],
    platform: Platform,
) -> Result<Vec<(PathBuf, String)>> {
    let planned = plan(facts, resolved, platform)?;
    let filter = JobFilter {
        resolved,
        selection: None,
    };
    Ok(
        pending_update(working_dir, platform, &planned, &filter, false)?
            .into_iter()
            .filter_map(|update| match update.change {
                FileChange::Write { content, .. } => Some((update.path, content)),
                FileChange::Unchanged | FileChange::WouldEmpty => None,
            })
            .collect(),
    )
}

/// Put [`update_files`] on disk, where `cibox update` would have put them
#[cfg(test)]
pub(crate) fn write_update(
    working_dir: &Path,
    facts: &ProjectFacts,
    resolved: &[ResolvedRule],
    platform: Platform,
) -> Result<()> {
    for (path, content) in update_files(working_dir, facts, resolved, platform)? {
        let path = working_dir.join(path);
        std::fs::create_dir_all(path.parent().expect("a pipeline file has a directory"))?;
        std::fs::write(path, content)?;
    }
    Ok(())
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

    /// What `cibox update` would write for `platform` into a repository that
    /// has no pipeline on disk yet, so the only thing that can fail is
    /// planning
    fn outputs(
        facts: &ProjectFacts,
        resolved: &[ResolvedRule],
        platform: Platform,
    ) -> Result<Vec<(PathBuf, String)>> {
        let dir = tempdir().unwrap();
        update_files(dir.path(), facts, resolved, platform)
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

        // ...and update creates nothing for it
        let written = outputs(&facts, &resolved, Platform::GitHub).unwrap();
        assert!(written.iter().all(|(p, _)| !p.ends_with("release.yml")));
    }

    #[test]
    fn test_github_splits_ci_and_release() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let outputs = outputs(&facts, &resolved, Platform::GitHub).unwrap();

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
            let outputs = outputs(&facts, &resolved, platform).unwrap();
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
        let outputs = outputs(&facts, &resolved, Platform::GitLab).unwrap();
        let yaml = &outputs[0].1;
        assert!(yaml.contains("rust-test"), "{yaml}");
        assert!(yaml.contains("docker-build"), "{yaml}");
        assert_eq!(yaml.matches("stages:").count(), 1, "{yaml}");
        // Strip the leading secrets comment and ensure valid YAML
        serde_yaml::from_str::<crate::platforms::gitlab::models::GitLabCI>(yaml).unwrap();
    }

    /// Nothing enabled and nothing on disk leaves update with no file to
    /// write — which is the state the command reports as an error (covered
    /// end to end in `tests/update.rs`)
    #[test]
    fn test_no_rules_enabled_writes_nothing() {
        let facts = ProjectFacts::default();
        let resolved = resolve(&facts, &CiboxConfig::default());
        for platform in Platform::all() {
            assert!(
                outputs(&facts, &resolved, platform).unwrap().is_empty(),
                "{platform:?}"
            );
        }
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
            let outputs = outputs(&facts, &resolved, platform).unwrap();
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
            let outputs = outputs(&facts, &resolved, platform).unwrap();
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

        let outputs = outputs(&facts, &resolved, Platform::GitHub).unwrap();
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
            let err = outputs(&facts, &resolved, platform).unwrap_err();
            assert!(
                err.to_string().contains("Windows runner"),
                "{platform:?}: {err}"
            );
        }
        // GitHub and Gitea are fine
        outputs(&facts, &resolved, Platform::GitHub).unwrap();
        outputs(&facts, &resolved, Platform::Gitea).unwrap();
    }

    #[test]
    fn test_aur_release_renders_valid_output_everywhere() {
        let facts = full_facts();
        let mut config = CiboxConfig::default();
        config.aur_release.enabled = Some(true);
        let resolved = resolve(&facts, &config);

        for platform in Platform::all() {
            let outputs = outputs(&facts, &resolved, platform).unwrap();
            let (path, content) = outputs
                .iter()
                .find(|(_, content)| content.contains("aur-release"))
                .unwrap_or_else(|| panic!("{platform:?} has no aur-release"));
            serde_yaml::from_str::<serde_yaml::Value>(content)
                .unwrap_or_else(|e| panic!("{platform:?} invalid YAML: {e}"));
            // Tag-gated, so GitHub/Gitea route it to release.yml; the secret
            // is referenced on GitHub/Gitea and documented on the others
            match platform {
                Platform::GitHub | Platform::Gitea => {
                    assert!(path.ends_with("release.yml"), "{platform:?}: {path:?}");
                    assert!(
                        content.contains("secrets.AUR_SSH_PRIVATE_KEY"),
                        "{platform:?}: {content}"
                    );
                }
                Platform::GitLab | Platform::CircleCI => {
                    assert!(
                        content.contains("Required CI variables")
                            && content.contains("AUR_SSH_PRIVATE_KEY"),
                        "{platform:?}: {content}"
                    );
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
            let outputs = outputs(&facts, &resolved, platform).unwrap();
            assert!(!outputs.is_empty(), "{platform:?} produced no output");
        }
    }
}

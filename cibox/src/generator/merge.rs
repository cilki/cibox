//! Merge canonical cibox output into an existing CI file.
//!
//! `cibox update` never adds jobs to a file the user already has: jobs the
//! user deleted stay deleted. Jobs that are present and cibox-owned (per
//! [`crate::rules::Rule::owns_job_id`]) are conformed to the canonical
//! output; owned jobs that are no longer generated (rule disabled) are
//! removed; everything else — custom jobs, extra top-level keys — is
//! preserved. Editing happens on `serde_yaml::Value` so user documents never
//! have to fit cibox's typed models.

use crate::config::Platform;
use crate::error::Result;
use crate::generator::PlannedFile;
use crate::ir::Job;
use crate::rules::ResolvedRule;
use anyhow::{bail, Context};
use serde_yaml::{Mapping, Value};

/// Result of merging canonical output into an existing file
#[derive(Debug)]
pub enum MergeOutcome {
    /// The existing file already matches; don't rewrite it (keeps comments)
    Unchanged,
    /// Merging would strip every job from the file, which the platform
    /// rejects as invalid; leave the file untouched
    WouldEmpty,
    Merged {
        content: String,
        /// Owned jobs replaced with canonical content
        conformed: usize,
        /// Owned jobs removed because their rule no longer generates them
        removed: usize,
        /// Jobs cibox doesn't own, left untouched
        preserved: usize,
    },
}

/// Merge one planned file into the existing on-disk content.
pub fn merge_file(
    platform: Platform,
    file: &PlannedFile,
    resolved: &[ResolvedRule],
    existing: &str,
) -> Result<MergeOutcome> {
    let original: Value = serde_yaml::from_str(existing).with_context(|| {
        format!(
            "{} is not valid YAML; use --force to overwrite it",
            file.path.display()
        )
    })?;
    let Value::Mapping(doc) = original.clone() else {
        bail!(
            "{} is not a YAML mapping cibox can merge into; use --force to overwrite it",
            file.path.display()
        );
    };

    let merged = match platform {
        Platform::GitHub | Platform::Gitea => merge_github(file, resolved, doc)?,
        Platform::GitLab => merge_gitlab(file, resolved, doc)?,
        Platform::CircleCI => merge_circleci(file, resolved, doc)?,
    };
    let Merged {
        doc,
        kept,
        conformed,
        removed,
        preserved,
    } = merged;

    if Value::Mapping(doc.clone()) == original {
        return Ok(MergeOutcome::Unchanged);
    }

    if matches!(platform, Platform::GitHub | Platform::Gitea)
        && doc
            .get("jobs")
            .and_then(Value::as_mapping)
            .is_none_or(Mapping::is_empty)
    {
        return Ok(MergeOutcome::WouldEmpty);
    }

    let mut content = serde_yaml::to_string(&Value::Mapping(doc))?;
    crate::generator::prepend_required_variables(platform, &kept, &mut content);

    Ok(MergeOutcome::Merged {
        content,
        conformed,
        removed,
        preserved,
    })
}

/// A merged document plus the jobs that survived and the change counts
struct Merged {
    doc: Mapping,
    kept: Vec<Job>,
    conformed: usize,
    removed: usize,
    preserved: usize,
}

fn is_owned(resolved: &[ResolvedRule], id: &str) -> bool {
    resolved.iter().any(|r| r.rule.owns_job_id(id))
}

/// Planned jobs whose id appears among the existing keys, with `needs`
/// pruned to jobs that survive (the user may have deleted a dependency)
fn kept_jobs(planned: &[Job], existing_keys: &[String]) -> Vec<Job> {
    let mut kept: Vec<Job> = planned
        .iter()
        .filter(|job| existing_keys.iter().any(|k| k == &job.id))
        .cloned()
        .collect();
    let ids: Vec<String> = kept.iter().map(|j| j.id.clone()).collect();
    for job in &mut kept {
        job.needs.retain(|n| ids.contains(n));
    }
    kept
}

fn string_keys(map: &Mapping) -> Vec<String> {
    map.keys()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// Replace owned+kept entries with canonical values and remove owned+stale
/// ones, in place. Returns (conformed, removed, preserved).
fn conform_jobs_map(
    jobs: &mut Mapping,
    job_keys: &[String],
    canonical_jobs: &Mapping,
    resolved: &[ResolvedRule],
) -> (usize, usize, usize) {
    let (mut conformed, mut removed, mut preserved) = (0, 0, 0);
    for key in job_keys {
        if !is_owned(resolved, key) {
            preserved += 1;
            continue;
        }
        let key = Value::from(key.as_str());
        match canonical_jobs.get(&key) {
            Some(canonical) => {
                jobs.insert(key, canonical.clone());
                conformed += 1;
            }
            None => {
                jobs.shift_remove(&key);
                removed += 1;
            }
        }
    }
    (conformed, removed, preserved)
}

fn merge_github(file: &PlannedFile, resolved: &[ResolvedRule], mut doc: Mapping) -> Result<Merged> {
    let Some(jobs) = doc.get_mut("jobs").and_then(Value::as_mapping_mut) else {
        bail!(
            "{} has no `jobs:` mapping; use --force to overwrite it",
            file.path.display()
        );
    };

    let job_keys = string_keys(jobs);
    let kept = kept_jobs(&file.jobs, &job_keys);
    let canonical =
        serde_yaml::to_value(crate::platforms::github::lower::lower_github(&kept, file.kind))?;
    let empty = Mapping::new();
    let canonical_jobs = canonical.get("jobs").and_then(Value::as_mapping).unwrap_or(&empty);

    let (conformed, removed, preserved) =
        conform_jobs_map(jobs, &job_keys, canonical_jobs, resolved);

    // Only `jobs` is managed; user-tuned triggers/env/permissions are kept.
    // Scaffold `name`/`on`/`permissions` solely when the workflow lacks them.
    let scaffold: Vec<(Value, Value)> = ["name", "on", "permissions"]
        .iter()
        .filter(|key| !doc.contains_key(**key))
        .filter_map(|key| Some((Value::from(*key), canonical.get(key)?.clone())))
        .collect();
    if !scaffold.is_empty() {
        doc = insert_before_jobs(doc, scaffold);
    }

    Ok(Merged {
        doc,
        kept,
        conformed,
        removed,
        preserved,
    })
}

/// Add top-level entries ahead of `jobs`, where a workflow's preamble
/// belongs. `Mapping` only appends, so the document is rebuilt in order.
fn insert_before_jobs(doc: Mapping, entries: Vec<(Value, Value)>) -> Mapping {
    let mut rebuilt = Mapping::with_capacity(doc.len() + entries.len());
    let mut entries = Some(entries);
    for (key, value) in doc {
        if key.as_str() == Some("jobs") {
            rebuilt.extend(entries.take().into_iter().flatten());
        }
        rebuilt.insert(key, value);
    }
    // No `jobs` key to anchor against (the caller rejects that), so append
    rebuilt.extend(entries.into_iter().flatten());
    rebuilt
}

/// Top-level `.gitlab-ci.yml` keys that are configuration, not jobs.
/// `.`-prefixed hidden templates are also never treated as jobs.
const GITLAB_RESERVED: &[&str] = &[
    "stages",
    "variables",
    "cache",
    "include",
    "default",
    "workflow",
    "image",
    "services",
    "before_script",
    "after_script",
];

fn merge_gitlab(file: &PlannedFile, resolved: &[ResolvedRule], mut doc: Mapping) -> Result<Merged> {
    let job_keys: Vec<String> = string_keys(&doc)
        .into_iter()
        .filter(|k| !GITLAB_RESERVED.contains(&k.as_str()) && !k.starts_with('.'))
        .collect();
    let kept = kept_jobs(&file.jobs, &job_keys);
    let canonical = serde_yaml::to_value(crate::platforms::gitlab::lower::lower_gitlab(&kept))?;

    let (conformed, removed, preserved) = conform_jobs_map(
        &mut doc,
        &job_keys,
        canonical.as_mapping().expect("GitLabCI serializes to a mapping"),
        resolved,
    );

    // Conform `stages` to what the remaining jobs (cibox or custom) reference
    let referenced: Vec<String> = doc
        .iter()
        .filter(|(k, _)| {
            k.as_str()
                .is_some_and(|k| !GITLAB_RESERVED.contains(&k) && !k.starts_with('.'))
        })
        .filter_map(|(_, v)| Some(v.as_mapping()?.get("stage")?.as_str()?.to_string()))
        .collect();
    let mut stages: Vec<Value> = doc
        .get("stages")
        .and_then(Value::as_sequence)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.as_str().is_some_and(|s| referenced.iter().any(|r| r == s)))
        .collect();
    for stage in canonical
        .get("stages")
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
    {
        if !stages.contains(stage) {
            stages.push(stage.clone());
        }
    }
    if stages.is_empty() {
        doc.shift_remove("stages");
    } else {
        doc.insert(Value::from("stages"), Value::from(stages));
    }

    // Conform the variables cibox defines; user-only variables stay (they
    // may feed custom jobs)
    if let Some(canonical_vars) = canonical.get("variables").and_then(Value::as_mapping) {
        if !doc.contains_key("variables") {
            doc.insert(Value::from("variables"), Value::Mapping(Mapping::new()));
        }
        let vars = doc
            .get_mut("variables")
            .and_then(Value::as_mapping_mut)
            .with_context(|| "`variables:` is not a mapping")?;
        for (key, value) in canonical_vars {
            vars.insert(key.clone(), value.clone());
        }
    }

    Ok(Merged {
        doc,
        kept,
        conformed,
        removed,
        preserved,
    })
}

fn merge_circleci(
    file: &PlannedFile,
    resolved: &[ResolvedRule],
    mut doc: Mapping,
) -> Result<Merged> {
    let Some(jobs) = doc.get_mut("jobs").and_then(Value::as_mapping_mut) else {
        bail!(
            "{} has no `jobs:` mapping; use --force to overwrite it",
            file.path.display()
        );
    };

    let job_keys = string_keys(jobs);
    let kept = kept_jobs(&file.jobs, &job_keys);
    let canonical =
        serde_yaml::to_value(crate::platforms::circleci::lower::lower_circleci(&kept))?;
    let empty = Mapping::new();
    let canonical_jobs = canonical.get("jobs").and_then(Value::as_mapping).unwrap_or(&empty);

    let (conformed, removed, preserved) =
        conform_jobs_map(jobs, &job_keys, canonical_jobs, resolved);

    // A job also appears as an entry in a workflow's `jobs` list — conform
    // or drop those entries to match, leaving custom entries alone
    let canonical_entries: Vec<&Value> = canonical
        .get("workflows")
        .and_then(|w| w.get("main"))
        .and_then(|m| m.get("jobs"))
        .and_then(Value::as_sequence)
        .map(|seq| seq.iter().collect())
        .unwrap_or_default();
    let entry_id = |entry: &Value| -> Option<String> {
        match entry {
            Value::String(id) => Some(id.clone()),
            Value::Mapping(m) if m.len() == 1 => {
                m.keys().next().and_then(Value::as_str).map(str::to_string)
            }
            _ => None,
        }
    };

    if let Some(workflows) = doc.get_mut("workflows").and_then(Value::as_mapping_mut) {
        let workflow_names = string_keys(workflows);
        for name in workflow_names {
            let name = Value::from(name);
            let Some(entries) = workflows
                .get_mut(&name)
                .and_then(|w| w.as_mapping_mut())
                .and_then(|w| w.get_mut("jobs"))
                .and_then(Value::as_sequence_mut)
            else {
                continue;
            };
            let had_entries = !entries.is_empty();
            *entries = entries
                .iter()
                .filter_map(|entry| match entry_id(entry) {
                    Some(id) if is_owned(resolved, &id) => canonical_entries
                        .iter()
                        .find(|c| entry_id(c).as_deref() == Some(&id))
                        .map(|c| (*c).clone()),
                    _ => Some(entry.clone()),
                })
                .collect();
            // CircleCI rejects a workflow with no jobs
            if had_entries && entries.is_empty() {
                workflows.shift_remove(&name);
            }
        }
    }

    Ok(Merged {
        doc,
        kept,
        conformed,
        removed,
        preserved,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CiboxConfig;
    use crate::detection::ProjectFacts;
    use crate::generator::{plan, render_file};
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

    fn planned_file(platform: Platform, resolved: &[ResolvedRule], index: usize) -> PlannedFile {
        let facts = full_facts();
        plan(&facts, resolved, platform).unwrap().swap_remove(index)
    }

    fn merged(outcome: MergeOutcome) -> (String, usize, usize, usize) {
        match outcome {
            MergeOutcome::Merged {
                content,
                conformed,
                removed,
                preserved,
            } => (content, conformed, removed, preserved),
            MergeOutcome::Unchanged => panic!("expected Merged, got Unchanged"),
            MergeOutcome::WouldEmpty => panic!("expected Merged, got WouldEmpty"),
        }
    }

    #[test]
    fn test_github_conform_preserve_and_no_readd() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitHub, &resolved, 0);
        assert!(file.jobs.iter().any(|j| j.id == "rust-fmt"), "fixture rot");

        // rust-test has stale content, rust-fmt was deleted by the user, a
        // custom job and a custom trigger block were added
        let existing = r#"
name: My CI
on:
  push:
    branches: [develop]
jobs:
  rust-test:
    runs-on: ubuntu-latest
    steps:
      - run: echo stale
  my-custom:
    runs-on: ubuntu-latest
    steps:
      - run: echo mine
"#;

        let (content, conformed, removed, preserved) =
            merged(merge_file(Platform::GitHub, &file, &resolved, existing).unwrap());

        assert!(!content.contains("echo stale"), "{content}");
        assert!(content.contains("cargo test"), "{content}");
        assert!(content.contains("echo mine"), "{content}");
        assert!(!content.contains("rust-fmt"), "{content}");
        assert!(content.contains("My CI"), "{content}");
        assert!(content.contains("develop"), "{content}");
        assert_eq!((conformed, removed, preserved), (1, 0, 1));
    }

    #[test]
    fn test_github_disabled_rule_job_removed() {
        let facts = full_facts();
        let mut config = CiboxConfig::default();
        config.rust_fmt.enabled = Some(false);
        let resolved = resolve(&facts, &config);
        let file = planned_file(Platform::GitHub, &resolved, 0);

        let existing = r#"
name: CI
on: [push]
jobs:
  rust-test:
    runs-on: ubuntu-latest
    steps:
      - run: cargo test
  rust-fmt:
    runs-on: ubuntu-latest
    steps:
      - run: cargo fmt --check
"#;

        let (content, _, removed, _) =
            merged(merge_file(Platform::GitHub, &file, &resolved, existing).unwrap());
        assert!(!content.contains("rust-fmt"), "{content}");
        assert_eq!(removed, 1);
    }

    #[test]
    fn test_github_needs_pruned_when_dependency_deleted() {
        let facts = full_facts();
        let mut config = CiboxConfig::default();
        config.docker_release.platforms = Some(vec![
            crate::config::DockerPlatform::LinuxAmd64,
            crate::config::DockerPlatform::WindowsAmd64,
        ]);
        let resolved = resolve(&facts, &config);
        let file = planned_file(Platform::GitHub, &resolved, 1);
        assert!(file.jobs.iter().any(|j| j.id == "docker-release-linux"));

        // The user deleted docker-release-linux but kept the manifest job
        let existing = r#"
name: Release
on:
  push:
    tags: [v*]
jobs:
  docker-release-windows:
    runs-on: windows-latest
    steps:
      - run: echo stale
  docker-release:
    runs-on: ubuntu-latest
    needs: [docker-release-linux, docker-release-windows]
    steps:
      - run: echo stale
"#;

        let (content, conformed, _, _) =
            merged(merge_file(Platform::GitHub, &file, &resolved, existing).unwrap());
        assert_eq!(conformed, 2);
        assert!(!content.contains("docker-release-linux"), "{content}");
        assert!(content.contains("docker-release-windows"), "{content}");
    }

    #[test]
    fn test_github_strip_to_empty_leaves_file_alone() {
        let facts = full_facts();
        let mut config = CiboxConfig::default();
        config.rust_fmt.enabled = Some(false);
        let resolved = resolve(&facts, &config);
        let file = planned_file(Platform::GitHub, &resolved, 0);

        // Only job present is owned but no longer generated
        let existing = r#"
name: CI
on: [push]
jobs:
  rust-fmt:
    runs-on: ubuntu-latest
    steps:
      - run: cargo fmt --check
"#;

        assert!(matches!(
            merge_file(Platform::GitHub, &file, &resolved, existing).unwrap(),
            MergeOutcome::WouldEmpty
        ));
    }

    #[test]
    fn test_github_scaffolds_permissions_but_never_overrides_them() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitHub, &resolved, 0);

        // A workflow predating the permissions block gets the read-only
        // default, so the update actually fixes an over-privileged token
        let without = r#"
name: CI
on: [push]
jobs:
  rust-test:
    runs-on: ubuntu-latest
    steps:
      - run: echo stale
"#;
        let (content, _, _, _) =
            merged(merge_file(Platform::GitHub, &file, &resolved, without).unwrap());
        let doc: Value = serde_yaml::from_str(&content).unwrap();
        assert_eq!(doc["permissions"]["contents"].as_str(), Some("read"));
        // Scaffolded keys land in the preamble, not after the job list
        assert!(
            content.find("permissions:").unwrap() < content.find("jobs:").unwrap(),
            "{content}"
        );

        // A user who widened the token on purpose keeps their choice
        let with = r#"
name: CI
on: [push]
permissions:
  contents: write
  id-token: write
jobs:
  rust-test:
    runs-on: ubuntu-latest
    steps:
      - run: echo stale
"#;
        let (content, _, _, _) =
            merged(merge_file(Platform::GitHub, &file, &resolved, with).unwrap());
        let doc: Value = serde_yaml::from_str(&content).unwrap();
        assert_eq!(doc["permissions"]["contents"].as_str(), Some("write"));
        assert_eq!(doc["permissions"]["id-token"].as_str(), Some("write"));
    }

    #[test]
    fn test_merge_is_idempotent_on_canonical_output() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        for platform in Platform::all() {
            for file in plan(&facts, &resolved, platform).unwrap() {
                if file.jobs.is_empty() {
                    continue;
                }
                let canonical = render_file(platform, &file).unwrap();
                assert!(
                    matches!(
                        merge_file(platform, &file, &resolved, &canonical).unwrap(),
                        MergeOutcome::Unchanged
                    ),
                    "{platform:?} {} not idempotent",
                    file.path.display()
                );
            }
        }
    }

    #[test]
    fn test_invalid_yaml_suggests_force() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitHub, &resolved, 0);

        for bad in ["jobs: [unclosed", "- a scalar list, not a mapping"] {
            let err = merge_file(Platform::GitHub, &file, &resolved, bad).unwrap_err();
            assert!(format!("{err:#}").contains("--force"), "{err:#}");
        }
    }

    #[test]
    fn test_gitlab_preserves_custom_config_and_conforms_managed() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitLab, &resolved, 0);

        let existing = r#"
include:
  - local: extra.yml
stages: [test, custom]
variables:
  MINE: "1"
.hidden-template:
  script: [echo hidden]
rust-test:
  stage: test
  script: [echo stale]
my-job:
  stage: custom
  script: [echo mine]
"#;

        let (content, conformed, removed, preserved) =
            merged(merge_file(Platform::GitLab, &file, &resolved, existing).unwrap());

        assert!(content.contains("extra.yml"), "{content}");
        assert!(content.contains(".hidden-template"), "{content}");
        assert!(content.contains("MINE"), "{content}");
        assert!(content.contains("my-job"), "{content}");
        assert!(content.contains("cargo test"), "{content}");
        assert!(!content.contains("echo stale"), "{content}");
        // The custom stage survives because my-job references it
        assert!(content.contains("custom"), "{content}");
        assert_eq!((conformed, removed, preserved), (1, 0, 1));
    }

    #[test]
    fn test_merge_lists_required_variables_only_where_needed() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());

        // A kept job with secrets gets the header on the ambient-variable
        // platforms...
        let existing = "stages: [deploy]\nrust-release:\n  stage: deploy\n  script: [echo stale]\n";
        let file = planned_file(Platform::GitLab, &resolved, 0);
        let (content, _, _, _) =
            merged(merge_file(Platform::GitLab, &file, &resolved, existing).unwrap());
        assert!(
            content.starts_with("# Required CI variables: CARGO_REGISTRY_TOKEN\n"),
            "{content}"
        );

        // ...but not when the surviving jobs need no secrets
        let existing = "stages: [test]\nrust-test:\n  stage: test\n  script: [echo stale]\n";
        let (content, _, _, _) =
            merged(merge_file(Platform::GitLab, &file, &resolved, existing).unwrap());
        assert!(!content.contains("Required CI variables"), "{content}");

        // GitHub workflows name their secrets inline, so no header there
        let existing = "name: Release\non:\n  push:\n    tags: [v*]\njobs:\n  \
                        rust-release:\n    runs-on: ubuntu-latest\n    steps:\n      \
                        - run: echo stale\n";
        let file = planned_file(Platform::GitHub, &resolved, 1);
        let (content, _, _, _) =
            merged(merge_file(Platform::GitHub, &file, &resolved, existing).unwrap());
        assert!(!content.contains("Required CI variables"), "{content}");
    }

    #[test]
    fn test_circleci_conforms_both_jobs_and_workflow_entries() {
        let facts = full_facts();
        let mut config = CiboxConfig::default();
        config.rust_fmt.enabled = Some(false);
        let resolved = resolve(&facts, &config);
        let file = planned_file(Platform::CircleCI, &resolved, 0);

        let existing = r#"
version: "2.1"
jobs:
  rust-test:
    docker: [{image: old}]
    steps: [checkout, run: echo stale]
  rust-fmt:
    docker: [{image: old}]
    steps: [checkout]
  my-job:
    docker: [{image: mine}]
    steps: [checkout]
workflows:
  main:
    jobs:
      - rust-test
      - rust-fmt:
          requires: [rust-test]
      - my-job
  nightly:
    jobs:
      - my-job
  stale-only:
    jobs:
      - rust-fmt
"#;

        let (content, conformed, removed, preserved) =
            merged(merge_file(Platform::CircleCI, &file, &resolved, existing).unwrap());

        assert!(!content.contains("rust-fmt"), "{content}");
        assert!(content.contains("my-job"), "{content}");
        assert!(content.contains("nightly"), "{content}");
        // A workflow left with no jobs is dropped entirely
        assert!(!content.contains("stale-only"), "{content}");
        assert!(content.contains("cargo test"), "{content}");
        assert!(!content.contains("echo stale"), "{content}");
        assert_eq!((conformed, removed, preserved), (1, 1, 1));
    }
}

//! Merge canonical cibox output into an existing CI file.
//!
//! Jobs that are present and cibox-owned (per
//! [`crate::rules::Rule::owns_job_id`]) are conformed to the canonical
//! output; owned jobs that are no longer generated (rule disabled) are
//! removed; everything else — custom jobs, extra top-level keys — is
//! preserved. Editing happens on `serde_yaml::Value` so user documents never
//! have to fit cibox's typed models.
//!
//! A job that detection enables but the file doesn't have is *not* added
//! back: the user deleted it and said nothing to the contrary. The one
//! exception is a rule turned on explicitly in `cibox.ron`, which is that
//! something to the contrary — see [`forced_job_ids`].

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
        /// Jobs added because `cibox.ron` turns their rule on explicitly
        added: usize,
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
        added,
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
        added,
    })
}

/// A merged document plus the jobs that survived and the change counts
struct Merged {
    doc: Mapping,
    kept: Vec<Job>,
    conformed: usize,
    removed: usize,
    preserved: usize,
    added: usize,
}

fn is_owned(resolved: &[ResolvedRule], id: &str) -> bool {
    resolved.iter().any(|r| r.rule.owns_job_id(id))
}

/// Ids of planned jobs whose rule `cibox.ron` turns on explicitly.
///
/// An `enabled: true` override is the user asking for the job in so many
/// words, and the only signal that tells a job they deleted apart from one
/// they have yet to see. Rules left to detection stay subject to the
/// never-re-add rule: the file alone cannot distinguish the two.
fn forced_job_ids(planned: &[Job], resolved: &[ResolvedRule]) -> Vec<String> {
    planned
        .iter()
        .filter(|job| {
            resolved
                .iter()
                .any(|r| r.forced && r.rule.owns_job_id(&job.id))
        })
        .map(|job| job.id.clone())
        .collect()
}

/// Planned jobs the merged file should hold: those whose id appears among the
/// existing keys, plus the force-enabled ones that are still missing. `needs`
/// is pruned to jobs that survive (the user may have deleted a dependency).
fn kept_jobs(planned: &[Job], existing_keys: &[String], forced: &[String]) -> Vec<Job> {
    let mut kept: Vec<Job> = planned
        .iter()
        .filter(|job| {
            existing_keys.iter().any(|k| k == &job.id) || forced.iter().any(|f| f == &job.id)
        })
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

/// Append the canonical entries for force-enabled jobs the file is missing.
/// Returns how many were added.
fn add_forced_jobs(
    jobs: &mut Mapping,
    job_keys: &[String],
    canonical_jobs: &Mapping,
    forced: &[String],
) -> usize {
    let mut added = 0;
    for id in forced {
        if job_keys.iter().any(|k| k == id) {
            continue;
        }
        let key = Value::from(id.as_str());
        if let Some(canonical) = canonical_jobs.get(&key) {
            jobs.insert(key, canonical.clone());
            added += 1;
        }
    }
    added
}

fn merge_github(file: &PlannedFile, resolved: &[ResolvedRule], mut doc: Mapping) -> Result<Merged> {
    let Some(jobs) = doc.get_mut("jobs").and_then(Value::as_mapping_mut) else {
        bail!(
            "{} has no `jobs:` mapping; use --force to overwrite it",
            file.path.display()
        );
    };

    let job_keys = string_keys(jobs);
    let forced = forced_job_ids(&file.jobs, resolved);
    let kept = kept_jobs(&file.jobs, &job_keys, &forced);
    let canonical =
        serde_yaml::to_value(crate::platforms::github::lower::lower_github(&kept, file.kind))?;
    let empty = Mapping::new();
    let canonical_jobs = canonical.get("jobs").and_then(Value::as_mapping).unwrap_or(&empty);

    let (conformed, removed, preserved) =
        conform_jobs_map(jobs, &job_keys, canonical_jobs, resolved);
    let added = add_forced_jobs(jobs, &job_keys, canonical_jobs, &forced);

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
        added,
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

/// Names of the variables cibox puts on its GitLab jobs, which an older cibox
/// would have written into the top-level `variables:` instead
fn gitlab_job_variable_names(planned: &[Job]) -> Vec<Value> {
    crate::platforms::gitlab::lower::lower_gitlab(planned)
        .jobs
        .into_values()
        .filter_map(|job| job.variables)
        .flat_map(|vars| vars.into_keys().map(Value::from))
        .collect()
}

fn merge_gitlab(file: &PlannedFile, resolved: &[ResolvedRule], mut doc: Mapping) -> Result<Merged> {
    let job_keys: Vec<String> = string_keys(&doc)
        .into_iter()
        .filter(|k| !GITLAB_RESERVED.contains(&k.as_str()) && !k.starts_with('.'))
        .collect();
    let forced = forced_job_ids(&file.jobs, resolved);
    let kept = kept_jobs(&file.jobs, &job_keys, &forced);
    let canonical = serde_yaml::to_value(crate::platforms::gitlab::lower::lower_gitlab(&kept))?;
    let canonical_jobs = canonical.as_mapping().expect("GitLabCI serializes to a mapping");

    let (conformed, removed, preserved) =
        conform_jobs_map(&mut doc, &job_keys, canonical_jobs, resolved);
    // Added before `stages` is recomputed, so the new job's stage counts as
    // referenced
    let added = add_forced_jobs(&mut doc, &job_keys, canonical_jobs, &forced);

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

    // cibox used to hoist every job's environment into the top-level
    // `variables:`, from where it applied to all the other jobs too. Those
    // keys now live on the job that wants them, so drop the global copies —
    // left behind they would keep leaking. Variables cibox never sets are the
    // user's (they may feed custom jobs) and stay.
    let hoisted = gitlab_job_variable_names(&file.jobs);
    if let Some(vars) = doc.get_mut("variables").and_then(Value::as_mapping_mut) {
        for key in hoisted {
            vars.shift_remove(&key);
        }
        if vars.is_empty() {
            doc.shift_remove("variables");
        }
    }

    Ok(Merged {
        doc,
        kept,
        conformed,
        removed,
        preserved,
        added,
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
    let forced = forced_job_ids(&file.jobs, resolved);
    let kept = kept_jobs(&file.jobs, &job_keys, &forced);
    let canonical =
        serde_yaml::to_value(crate::platforms::circleci::lower::lower_circleci(&kept))?;
    let empty = Mapping::new();
    let canonical_jobs = canonical.get("jobs").and_then(Value::as_mapping).unwrap_or(&empty);

    let (conformed, removed, preserved) =
        conform_jobs_map(jobs, &job_keys, canonical_jobs, resolved);
    let added = add_forced_jobs(jobs, &job_keys, canonical_jobs, &forced);

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
            // A matrix job is invoked once per leg, so one owned id can map
            // to several canonical entries: the first existing entry expands
            // to all of them, later ones with the same id are dropped
            let mut expanded: Vec<String> = Vec::new();
            let mut conformed = Vec::new();
            for entry in entries.iter() {
                match entry_id(entry) {
                    Some(id) if is_owned(resolved, &id) => {
                        if !expanded.contains(&id) {
                            expanded.push(id.clone());
                            conformed.extend(
                                canonical_entries
                                    .iter()
                                    .filter(|c| entry_id(c).as_deref() == Some(&id))
                                    .map(|c| (*c).clone()),
                            );
                        }
                    }
                    _ => conformed.push(entry.clone()),
                }
            }
            *entries = conformed;
            // CircleCI rejects a workflow with no jobs
            if had_entries && entries.is_empty() {
                workflows.shift_remove(&name);
            }
        }
    }

    // A job definition only runs when a workflow invokes it, so an added job
    // needs its entry too: `main` is where cibox puts its own, falling back
    // to whatever the user renamed their single workflow to, and scaffolding
    // the block when the file has none.
    let forced_entries: Vec<Value> = canonical_entries
        .iter()
        .filter(|entry| {
            entry_id(entry).is_some_and(|id| {
                forced.iter().any(|f| f == &id) && !job_keys.iter().any(|k| k == &id)
            })
        })
        .map(|entry| (*entry).clone())
        .collect();
    if !forced_entries.is_empty() {
        let workflows = doc
            .entry(Value::from("workflows"))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        if let Some(workflows) = workflows.as_mapping_mut() {
            let name = Some(Value::from("main"))
                .filter(|main| workflows.contains_key(main))
                .or_else(|| workflows.keys().next().cloned())
                .unwrap_or_else(|| Value::from("main"));
            let entries = workflows
                .entry(name)
                .or_insert_with(|| Value::Mapping(Mapping::new()))
                .as_mapping_mut()
                .map(|workflow| {
                    workflow
                        .entry(Value::from("jobs"))
                        .or_insert_with(|| Value::Sequence(Vec::new()))
                })
                .and_then(Value::as_sequence_mut);
            if let Some(entries) = entries {
                entries.extend(forced_entries);
            }
        }
    }

    Ok(Merged {
        doc,
        kept,
        conformed,
        removed,
        preserved,
        added,
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

    fn merged_counts(outcome: MergeOutcome) -> (String, usize, usize, usize, usize) {
        match outcome {
            MergeOutcome::Merged {
                content,
                conformed,
                removed,
                preserved,
                added,
            } => (content, conformed, removed, preserved, added),
            MergeOutcome::Unchanged => panic!("expected Merged, got Unchanged"),
            MergeOutcome::WouldEmpty => panic!("expected Merged, got WouldEmpty"),
        }
    }

    fn merged(outcome: MergeOutcome) -> (String, usize, usize, usize) {
        let (content, conformed, removed, preserved, _) = merged_counts(outcome);
        (content, conformed, removed, preserved)
    }

    /// Config that turns a rule on that the fixture's facts don't detect
    fn force_go_lint() -> CiboxConfig {
        let mut config = CiboxConfig::default();
        config.go_lint.enabled = Some(true);
        config
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
    fn test_github_adds_a_rule_turned_on_in_the_config() {
        // Enabling a rule in cibox.ron is the only way to ask for a job that
        // isn't in the file yet; without it `update` would report the rule
        // enabled and write nothing, so --force (which discards the user's
        // edits) would be the only way to adopt a new rule.
        let facts = full_facts();
        let config = force_go_lint();
        let resolved = resolve(&facts, &config);
        let file = planned_file(Platform::GitHub, &resolved, 0);

        // rust-fmt was deleted by the user and a custom job added
        let existing = r#"
name: CI
on: [push]
permissions:
  contents: read
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

        let (content, conformed, removed, preserved, added) =
            merged_counts(merge_file(Platform::GitHub, &file, &resolved, existing).unwrap());

        assert_eq!((conformed, removed, preserved, added), (1, 0, 1, 1));
        let doc: Value = serde_yaml::from_str(&content).unwrap();
        assert!(doc["jobs"]["go-lint"]["steps"].is_sequence(), "{content}");
        // Detection-only rules the user deleted are still left out
        assert!(doc["jobs"]["rust-fmt"].is_null(), "{content}");
        assert!(doc["jobs"]["my-custom"]["steps"].is_sequence(), "{content}");

        // ...and the job is now present, so a second update is a no-op
        assert!(matches!(
            merge_file(Platform::GitHub, &file, &resolved, &content).unwrap(),
            MergeOutcome::Unchanged
        ));
    }

    #[test]
    fn test_gitlab_adds_a_forced_job_with_its_stage() {
        let facts = full_facts();
        let config = force_go_lint();
        let resolved = resolve(&facts, &config);
        let file = planned_file(Platform::GitLab, &resolved, 0);

        // A pipeline trimmed down to one test job: `lint` isn't a stage yet
        let existing = "stages: [test]\nrust-test:\n  stage: test\n  script: [echo stale]\n";
        let (content, _, _, _, added) =
            merged_counts(merge_file(Platform::GitLab, &file, &resolved, existing).unwrap());

        assert_eq!(added, 1);
        let doc: Value = serde_yaml::from_str(&content).unwrap();
        assert_eq!(doc["go-lint"]["stage"].as_str(), Some("lint"));
        // GitLab rejects a job whose stage isn't declared
        let stages: Vec<&str> = doc["stages"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(stages.contains(&"lint"), "{content}");

        assert!(matches!(
            merge_file(Platform::GitLab, &file, &resolved, &content).unwrap(),
            MergeOutcome::Unchanged
        ));
    }

    #[test]
    fn test_circleci_adds_a_forced_job_and_its_workflow_entry() {
        let facts = full_facts();
        let config = force_go_lint();
        let resolved = resolve(&facts, &config);
        let file = planned_file(Platform::CircleCI, &resolved, 0);

        // The user renamed the workflow, so there is no `main` to extend;
        // a job with no invocation anywhere would never run
        let existing = r#"
version: "2.1"
jobs:
  rust-test:
    docker: [{image: old}]
    steps: [checkout]
workflows:
  everything:
    jobs:
      - rust-test
"#;

        let (content, _, _, _, added) =
            merged_counts(merge_file(Platform::CircleCI, &file, &resolved, existing).unwrap());

        assert_eq!(added, 1);
        let doc: Value = serde_yaml::from_str(&content).unwrap();
        assert!(doc["jobs"]["go-lint"]["steps"].is_sequence(), "{content}");
        assert!(doc["workflows"]["main"].is_null(), "{content}");
        let entries: Vec<&str> = doc["workflows"]["everything"]["jobs"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(entries, vec!["rust-test", "go-lint"], "{content}");

        assert!(matches!(
            merge_file(Platform::CircleCI, &file, &resolved, &content).unwrap(),
            MergeOutcome::Unchanged
        ));
    }

    #[test]
    fn test_github_adds_back_a_deleted_job_the_config_asks_for() {
        // `enabled: true` on a rule detection already enables changes nothing
        // about the resolution, but it is still the user asking for the job —
        // so a deletion they have since changed their mind about is undone
        let facts = full_facts();
        let mut config = CiboxConfig::default();
        config.rust_fmt.enabled = Some(true);
        let resolved = resolve(&facts, &config);
        assert!(
            resolved
                .iter()
                .any(|r| r.rule.id() == "rust-fmt" && r.detected),
            "fixture rot"
        );
        let file = planned_file(Platform::GitHub, &resolved, 0);

        let existing = "name: CI\non: [push]\njobs:\n  rust-test:\n    \
                        runs-on: ubuntu-latest\n    steps:\n      - run: echo stale\n";
        let (content, _, _, _, added) =
            merged_counts(merge_file(Platform::GitHub, &file, &resolved, existing).unwrap());
        assert_eq!(added, 1);
        assert!(content.contains("rust-fmt"), "{content}");
    }

    #[test]
    fn test_disabling_a_forced_rule_again_removes_its_job() {
        // The override is what adds the job, so dropping the override has to
        // take it back out rather than leave it orphaned forever
        let facts = full_facts();
        let resolved = resolve(&facts, &force_go_lint());
        let file = planned_file(Platform::GitHub, &resolved, 0);
        let existing = "name: CI\non: [push]\njobs:\n  rust-test:\n    \
                        runs-on: ubuntu-latest\n    steps:\n      - run: echo stale\n";
        let (with_job, _, _, _, _) =
            merged_counts(merge_file(Platform::GitHub, &file, &resolved, existing).unwrap());

        let plain = resolve(&facts, &CiboxConfig::default());
        let plain_file = planned_file(Platform::GitHub, &plain, 0);
        let (content, _, removed, _, added) =
            merged_counts(merge_file(Platform::GitHub, &plain_file, &plain, &with_job).unwrap());
        assert_eq!((removed, added), (1, 0));
        assert!(!content.contains("go-lint"), "{content}");
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
    fn test_gitlab_update_narrows_any_tag_release_gating() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitLab, &resolved, 0);

        // A pipeline generated before release jobs were gated on `v*`
        let existing = "stages: [deploy]\n\
                        rust-release:\n  stage: deploy\n  script: [cargo publish]\n  \
                        only:\n    refs: [tags]\n\
                        my-release:\n  stage: deploy\n  script: [echo mine]\n  \
                        only:\n    refs: [tags]\n";
        let (content, _, _, _) =
            merged(merge_file(Platform::GitLab, &file, &resolved, existing).unwrap());
        let doc: Value = serde_yaml::from_str(&content).unwrap();

        assert_eq!(
            doc["rust-release"]["rules"][0]["if"].as_str(),
            Some("$CI_COMMIT_TAG =~ /^v/")
        );
        assert!(doc["rust-release"]["only"].is_null(), "{content}");
        // The user's own job is not cibox's to re-gate
        assert_eq!(doc["my-release"]["only"]["refs"][0].as_str(), Some("tags"));
    }

    #[test]
    fn test_gitlab_update_moves_hoisted_variables_onto_their_jobs() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitLab, &resolved, 0);

        // A pipeline generated before job environments stayed on their jobs:
        // the doc job's RUSTDOCFLAGS applied to rust-test's doctests too
        let existing = "stages: [test, lint]\n\
                        variables:\n  CARGO_HOME: .cargo\n  \
                        RUSTDOCFLAGS: --cfg docsrs\n  MINE: '1'\n\
                        rust-test:\n  stage: test\n  script: [echo stale]\n\
                        rust-doc:\n  stage: lint\n  script: [echo stale]\n";
        let (content, _, _, _) =
            merged(merge_file(Platform::GitLab, &file, &resolved, existing).unwrap());
        let doc: Value = serde_yaml::from_str(&content).unwrap();

        assert!(doc["variables"]["RUSTDOCFLAGS"].is_null(), "{content}");
        assert!(doc["variables"]["CARGO_HOME"].is_null(), "{content}");
        // A variable cibox doesn't set is the user's to keep
        assert_eq!(doc["variables"]["MINE"].as_str(), Some("1"));

        assert_eq!(
            doc["rust-doc"]["variables"]["RUSTDOCFLAGS"].as_str(),
            Some("--cfg docsrs")
        );
        assert_eq!(doc["rust-test"]["variables"]["CARGO_HOME"].as_str(), Some(".cargo"));
        assert!(doc["rust-test"]["variables"]["RUSTDOCFLAGS"].is_null(), "{content}");
    }

    #[test]
    fn test_gitlab_update_drops_a_variables_block_left_empty() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitLab, &resolved, 0);

        let existing = "stages: [test]\nvariables:\n  CARGO_HOME: .cargo\n\
                        rust-test:\n  stage: test\n  script: [echo stale]\n";
        let (content, _, _, _) =
            merged(merge_file(Platform::GitLab, &file, &resolved, existing).unwrap());
        assert!(!content.contains("variables:\n  CARGO_HOME"), "{content}");
        let doc: Value = serde_yaml::from_str(&content).unwrap();
        assert!(doc.get("variables").is_none(), "{content}");
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
    fn test_merge_is_idempotent_with_versions() {
        let facts = full_facts();
        let mut config = CiboxConfig::default();
        config.rust_test.versions = Some(vec!["1.85".to_string(), "nightly".to_string()]);
        let resolved = resolve(&facts, &config);
        for platform in Platform::all() {
            for file in plan(&facts, &resolved, platform).unwrap() {
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
    fn test_github_matrix_body_conforms() {
        let facts = full_facts();
        let mut config = CiboxConfig::default();
        config.rust_test.versions = Some(vec!["1.85".to_string(), "nightly".to_string()]);
        let resolved = resolve(&facts, &config);
        let file = planned_file(Platform::GitHub, &resolved, 0);

        let existing = "name: CI\non: [push]\njobs:\n  rust-test:\n    \
                        runs-on: ubuntu-latest\n    steps:\n      - run: echo stale\n";
        let (content, _, _, _) =
            merged(merge_file(Platform::GitHub, &file, &resolved, existing).unwrap());
        assert!(content.contains("strategy:"), "{content}");
        assert!(content.contains("fail-fast: false"), "{content}");
        assert!(content.contains("rustlang/rust:nightly"), "{content}");
        assert!(!content.contains("echo stale"), "{content}");
    }

    #[test]
    fn test_circleci_matrix_expands_workflow_invocations() {
        let facts = full_facts();
        let mut config = CiboxConfig::default();
        config.rust_test.versions = Some(vec!["1.85".to_string(), "nightly".to_string()]);
        let resolved = resolve(&facts, &config);
        let file = planned_file(Platform::CircleCI, &resolved, 0);

        let existing = r#"
version: "2.1"
jobs:
  rust-test:
    docker: [{image: old}]
    steps: [checkout]
  my-job:
    docker: [{image: mine}]
    steps: [checkout]
workflows:
  main:
    jobs:
      - rust-test
      - my-job
"#;

        let (content, _, _, _) =
            merged(merge_file(Platform::CircleCI, &file, &resolved, existing).unwrap());
        assert!(content.contains("rust-test-1.85"), "{content}");
        assert!(content.contains("rust-test-nightly"), "{content}");
        assert!(content.contains("my-job"), "{content}");
        assert!(content.contains("<< parameters.image >>"), "{content}");
        // Merging the merged output again is a no-op: the expanded
        // invocations map back onto the same canonical entries
        assert!(matches!(
            merge_file(Platform::CircleCI, &file, &resolved, &content).unwrap(),
            MergeOutcome::Unchanged
        ));
    }

    #[test]
    fn test_circleci_stale_matrix_invocations_collapse() {
        let facts = full_facts();
        // One version left: the job collapses back to a single invocation
        let mut config = CiboxConfig::default();
        config.rust_test.versions = Some(vec!["1.85".to_string()]);
        let resolved = resolve(&facts, &config);
        let file = planned_file(Platform::CircleCI, &resolved, 0);

        let existing = r#"
version: "2.1"
jobs:
  rust-test:
    parameters:
      image: {type: string}
      version: {type: string}
    docker: [{image: << parameters.image >>}]
    steps: [checkout]
workflows:
  main:
    jobs:
      - rust-test:
          name: rust-test-1.85
          version: "1.85"
          image: rust:1.85
      - rust-test:
          name: rust-test-nightly
          version: nightly
          image: rustlang/rust:nightly
"#;

        let (content, _, _, _) =
            merged(merge_file(Platform::CircleCI, &file, &resolved, existing).unwrap());
        assert!(!content.contains("rust-test-nightly"), "{content}");
        assert!(!content.contains("parameters"), "{content}");
        assert!(content.contains("rust:1.85"), "{content}");
        // Exactly one workflow invocation remains
        assert_eq!(content.matches("- rust-test").count(), 1, "{content}");
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

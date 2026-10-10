//! Merge canonical cibox output into an existing CI file.
//!
//! Editing happens on a lossless `yaml-edit` syntax tree, so user comments,
//! formatting, and key order survive. For every planned job the
//! [`JobFilter`] lets cibox manage: a job that is present with stale content
//! is conformed to canonical output; one that is absent is re-added; one
//! whose block is commented out stays commented but gets its content
//! regenerated ("disabled" — commenting a job out is how a job is kept
//! inactive without going stale); one whose rule no longer generates it is
//! removed, commented or not. Everything else — custom jobs, extra top-level
//! keys, and under `--rule` the jobs of unselected rules — is preserved
//! byte for byte.

use crate::config::Platform;
use crate::error::Result;
use crate::generator::disabled::{self, DisabledBlock, Scan};
use crate::generator::{JobFilter, PlannedFile};
use crate::ir::Job;
use anyhow::{anyhow, bail, Context};
use serde_yaml::Value;
use std::ops::Range;
use std::path::Path;
use std::str::FromStr;
use yaml_edit::{AsYaml, Document, Mapping, YamlFile, YamlNode};

/// Result of merging canonical output into an existing file
#[derive(Debug)]
pub enum MergeOutcome {
    /// The existing file already matches byte for byte; don't rewrite it
    Unchanged,
    /// Merging would strip every active job from the file, which the
    /// platform rejects as invalid; leave the file untouched
    WouldEmpty,
    Merged {
        content: String,
        /// Owned jobs replaced with canonical content
        conformed: usize,
        /// Owned jobs re-added because they were missing
        added: usize,
        /// Owned jobs removed because their rule no longer generates them
        removed: usize,
        /// Commented-out jobs kept commented, with regenerated content
        disabled: usize,
        /// Jobs cibox doesn't manage, left untouched
        preserved: usize,
    },
}

#[derive(Default)]
struct Counts {
    conformed: usize,
    added: usize,
    removed: usize,
    disabled: usize,
    preserved: usize,
}

impl Counts {
    fn outcome(self, content: String) -> MergeOutcome {
        MergeOutcome::Merged {
            content,
            conformed: self.conformed,
            added: self.added,
            removed: self.removed,
            disabled: self.disabled,
            preserved: self.preserved,
        }
    }
}

/// Merge one planned file into the existing on-disk content.
pub fn merge_file(
    platform: Platform,
    file: &PlannedFile,
    filter: &JobFilter,
    existing: &str,
) -> Result<MergeOutcome> {
    let original: Value = serde_yaml::from_str(existing).with_context(|| {
        format!(
            "{} is not valid YAML; use --force to overwrite it",
            file.path.display()
        )
    })?;
    let Value::Mapping(original) = original else {
        bail!(
            "{} is not a YAML mapping cibox can merge into; use --force to overwrite it",
            file.path.display()
        );
    };
    let parsed = parse_lossless(existing, &file.path)?;
    let scan = disabled::scan(platform, existing, &parsed, filter);

    match platform {
        Platform::GitHub | Platform::Gitea => merge_github(file, filter, existing, &original, scan),
        Platform::GitLab => merge_gitlab(file, filter, existing, &original, scan),
        Platform::CircleCI => merge_circleci(file, filter, existing, &original, &parsed, scan),
    }
}

fn parse_lossless(text: &str, path: &Path) -> Result<YamlFile> {
    YamlFile::from_str(text).map_err(|e| {
        anyhow!("{} is not valid YAML; use --force to overwrite it: {e}", path.display())
    })
}

/// The planned jobs split into what runs and what stays commented out, with
/// `needs` edges pruned to jobs that exist on the other side of the merge.
struct Partition {
    /// Managed jobs that will be active in the file, in plan order
    active: Vec<Job>,
    /// Managed jobs the user commented out
    disabled: Vec<Job>,
    /// Ids of active jobs missing from the file, to re-add
    adds: Vec<String>,
}

fn partition(
    planned: &[Job],
    existing_keys: &[String],
    disabled_ids: &[String],
    filter: &JobFilter,
) -> Partition {
    let managed: Vec<Job> = planned
        .iter()
        .filter(|j| filter.managed(&j.id))
        .cloned()
        .collect();
    let (mut disabled, mut active): (Vec<Job>, Vec<Job>) = managed
        .into_iter()
        .partition(|j| disabled_ids.contains(&j.id));
    let adds: Vec<String> = active
        .iter()
        .map(|j| j.id.clone())
        .filter(|id| !existing_keys.contains(id))
        .collect();

    // What a live job may `needs`: the managed jobs that will be active,
    // plus every job cibox leaves alone (stale managed keys get removed, so
    // they don't count)
    let mut final_active: Vec<String> = active.iter().map(|j| j.id.clone()).collect();
    final_active.extend(
        existing_keys
            .iter()
            .filter(|k| !filter.managed(k))
            .cloned(),
    );
    for job in &mut active {
        job.needs.retain(|n| final_active.contains(n));
    }
    // A disabled job keeps edges to other disabled jobs, so uncommenting a
    // cluster restores a working graph
    let inert: Vec<String> = disabled.iter().map(|j| j.id.clone()).collect();
    for job in &mut disabled {
        job.needs
            .retain(|n| final_active.contains(n) || inert.contains(n));
    }
    Partition {
        active,
        disabled,
        adds,
    }
}

/// An active key shadows a commented block of the same id: the block is then
/// an ordinary comment, not a disabled job.
fn unshadowed_blocks(blocks: Vec<DisabledBlock>, active_keys: &[String]) -> Vec<DisabledBlock> {
    blocks
        .into_iter()
        .filter(|b| !b.ids.iter().any(|id| active_keys.contains(id)))
        .collect()
}

fn get<'a>(map: &'a serde_yaml::Mapping, key: &str) -> Option<&'a Value> {
    map.get(Value::from(key))
}

/// Render `{key: value}` canonically and return the parsed value node, ready
/// to splice into the user's tree. The document is returned alongside only
/// to keep its tree alive while the node is used.
fn fragment(key: &str, value: &Value) -> Result<(Document, YamlNode)> {
    let mut map = serde_yaml::Mapping::new();
    map.insert(Value::from(key), value.clone());
    let text = serde_yaml::to_string(&Value::Mapping(map))?;
    let doc = Document::from_str(&text)
        .map_err(|e| anyhow!("canonical output for `{key}` did not reparse: {e}"))?;
    let node = doc
        .as_mapping()
        .and_then(|m| m.get(key))
        .with_context(|| format!("canonical output for `{key}` did not round-trip"))?;
    Ok((doc, node))
}

fn render_job(id: &str, value: &Value) -> Result<String> {
    let mut map = serde_yaml::Mapping::new();
    map.insert(Value::from(id), value.clone());
    Ok(serde_yaml::to_string(&Value::Mapping(map))?)
}

/// What `conform_jobs` measures each job against: the canonical output, the
/// ids to add, and the filter deciding what cibox may touch at all
struct JobsPlan<'a> {
    canonical: &'a serde_yaml::Mapping,
    adds: &'a [String],
    filter: &'a JobFilter<'a>,
    path: &'a Path,
}

/// Replace stale managed entries with canonical content, drop managed
/// entries that are no longer generated, and append the missing ones.
/// `job_keys` are the keys of `current` that are jobs (GitLab mixes jobs
/// with reserved configuration keys at the top level).
fn conform_jobs(
    jobs: &Mapping,
    job_keys: &[String],
    current: &serde_yaml::Mapping,
    plan: &JobsPlan,
    counts: &mut Counts,
) -> Result<()> {
    let guard = |jobs: &Mapping| -> Result<()> {
        if jobs.is_flow_style() {
            bail!(
                "{} keeps its jobs in flow-style YAML, which cibox cannot edit in place; \
                 use --force to overwrite it",
                plan.path.display()
            );
        }
        Ok(())
    };
    for key in job_keys {
        if !plan.filter.managed(key) {
            counts.preserved += 1;
            continue;
        }
        match plan.canonical.get(Value::from(key.as_str())) {
            Some(canonical) if get(current, key) == Some(canonical) => {}
            Some(canonical) => {
                guard(jobs)?;
                let (_doc, node) = fragment(key, canonical)?;
                jobs.set(key.as_str(), node);
                counts.conformed += 1;
            }
            None => {
                guard(jobs)?;
                jobs.remove(key.as_str());
                counts.removed += 1;
            }
        }
    }
    for id in plan.adds {
        // Already present: e.g. inserted textually when `jobs:` held only
        // comments, in which case it was counted there
        if get(current, id).is_some() {
            continue;
        }
        if let Some(canonical) = plan.canonical.get(Value::from(id.as_str())) {
            guard(jobs)?;
            let (_doc, node) = fragment(id, canonical)?;
            jobs.set(id.as_str(), node);
            counts.added += 1;
        }
    }
    Ok(())
}

/// Text replacements for the disabled blocks: regenerate blocks whose
/// content went stale (still commented), delete blocks whose jobs are no
/// longer generated, leave up-to-date blocks exactly as the user wrote them.
fn block_edits(
    blocks: &[DisabledBlock],
    canonical_jobs: &serde_yaml::Mapping,
    counts: &mut Counts,
) -> Result<Vec<(Range<usize>, String)>> {
    let mut edits = Vec::new();
    for block in blocks {
        let kept: Vec<&String> = block
            .ids
            .iter()
            .filter(|id| get(canonical_jobs, id).is_some())
            .collect();
        counts.removed += block.ids.len() - kept.len();
        counts.disabled += kept.len();
        let unchanged = kept.len() == block.ids.len()
            && kept
                .iter()
                .all(|id| get(&block.jobs, id) == get(canonical_jobs, id));
        if unchanged {
            continue;
        }
        let mut replacement = String::new();
        for id in kept {
            let canonical = get(canonical_jobs, id).expect("filtered above");
            replacement.push_str(&disabled::comment_out(
                &render_job(id, canonical)?,
                block.indent,
            ));
        }
        edits.push((block.span.clone(), replacement));
    }
    Ok(edits)
}

fn apply_edits(text: &str, mut edits: Vec<(Range<usize>, String)>) -> String {
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut out = text.to_string();
    for (range, replacement) in edits {
        out.replace_range(range, &replacement);
    }
    out
}

/// Byte offset just past the top-level `jobs:` line, where new jobs can be
/// inserted textually when there is no mapping node to splice into (every
/// job commented out, or `jobs:` left empty)
fn jobs_line_end(text: &str) -> Option<usize> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let rest = line.trim_end().strip_prefix("jobs:");
        if let Some(rest) = rest {
            if rest.trim_start().is_empty() || rest.trim_start().starts_with('#') {
                return Some(offset + line.len());
            }
        }
        offset += line.len();
    }
    None
}

fn indent_by(text: &str, indent: usize) -> String {
    let pad = " ".repeat(indent);
    text.lines()
        .map(|line| {
            if line.trim().is_empty() {
                "\n".to_string()
            } else {
                format!("{pad}{line}\n")
            }
        })
        .collect()
}

/// Render the jobs to add when the file has no live `jobs:` mapping to
/// splice into, and queue the insertion right below the `jobs:` line.
fn add_jobs_textually(
    existing: &str,
    adds: &[String],
    canonical_jobs: &serde_yaml::Mapping,
    counts: &mut Counts,
    edits: &mut Vec<(Range<usize>, String)>,
) -> Result<()> {
    let Some(offset) = jobs_line_end(existing) else {
        return Ok(());
    };
    let mut rendered = String::new();
    for id in adds {
        if let Some(canonical) = get(canonical_jobs, id) {
            rendered.push_str(&indent_by(&render_job(id, canonical)?, 2));
            counts.added += 1;
        }
    }
    if !rendered.is_empty() {
        edits.push((offset..offset, rendered));
    }
    Ok(())
}

/// The planned jobs whose secrets the file's live pipeline will need: the
/// managed jobs that end up active, plus unselected planned jobs the file
/// already runs.
fn header_jobs(
    planned: &[Job],
    filter: &JobFilter,
    disabled_ids: &[String],
    existing_keys: &[String],
) -> Vec<Job> {
    planned
        .iter()
        .filter(|j| {
            if filter.managed(&j.id) {
                !disabled_ids.contains(&j.id)
            } else {
                existing_keys.contains(&j.id)
            }
        })
        .cloned()
        .collect()
}

fn merge_github(
    file: &PlannedFile,
    filter: &JobFilter,
    existing: &str,
    original: &serde_yaml::Mapping,
    scan: Scan,
) -> Result<MergeOutcome> {
    let mut counts = Counts::default();

    // `jobs:` holding nothing happens when every job is commented out or
    // deleted; a file without the key at all is not a workflow
    let existing_jobs: serde_yaml::Mapping = match get(original, "jobs") {
        Some(Value::Mapping(m)) => m.clone(),
        Some(Value::Null) => serde_yaml::Mapping::new(),
        _ => bail!(
            "{} has no `jobs:` mapping; use --force to overwrite it",
            file.path.display()
        ),
    };
    let existing_keys: Vec<String> = existing_jobs
        .keys()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();

    let blocks = unshadowed_blocks(scan.blocks, &existing_keys);
    let disabled_ids: Vec<String> = blocks.iter().flat_map(|b| b.ids.clone()).collect();
    let parts = partition(&file.jobs, &existing_keys, &disabled_ids, filter);

    let kept_all: Vec<Job> = parts.active.iter().chain(&parts.disabled).cloned().collect();
    let canonical =
        serde_yaml::to_value(crate::platforms::github::lower::lower_github(&kept_all, file.kind))?;
    let empty = serde_yaml::Mapping::new();
    let canonical_jobs = canonical
        .get("jobs")
        .and_then(Value::as_mapping)
        .unwrap_or(&empty);

    // Text phase: the commented blocks live outside the YAML value tree
    let mut edits = block_edits(&blocks, canonical_jobs, &mut counts)?;
    if existing_jobs.is_empty() {
        add_jobs_textually(existing, &parts.adds, canonical_jobs, &mut counts, &mut edits)?;
    }
    let text = apply_edits(existing, edits);

    // Tree phase: conform the live jobs on the lossless syntax tree
    let parsed = parse_lossless(&text, &file.path)?;
    let current: Value =
        serde_yaml::from_str(&text).context("regenerated comment blocks reparse")?;
    let current_map = current.as_mapping().context("document is a mapping")?;
    let current_jobs = get(current_map, "jobs")
        .and_then(Value::as_mapping)
        .cloned()
        .unwrap_or_default();
    let current_keys: Vec<String> = current_jobs
        .keys()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    let doc = parsed.document().context("document is a mapping")?;
    let root = doc.as_mapping().context("document is a mapping")?;
    if let Some(jobs) = root.get_mapping("jobs") {
        conform_jobs(
            &jobs,
            &current_keys,
            &current_jobs,
            &JobsPlan {
                canonical: canonical_jobs,
                adds: &parts.adds,
                filter,
                path: &file.path,
            },
            &mut counts,
        )?;
    }

    // Only `jobs` is managed; user-tuned triggers/env/permissions are kept.
    // Scaffold `name`/`on`/`permissions` solely when the workflow lacks them.
    for key in ["name", "on", "permissions"] {
        if get(current_map, key).is_some() {
            continue;
        }
        if let Some(value) = canonical.get(key) {
            let (_doc, node) = fragment(key, value)?;
            root.insert_before("jobs", key, node);
        }
    }

    let content = parsed.to_string();
    if content == existing {
        return Ok(MergeOutcome::Unchanged);
    }
    let live_jobs = existing_keys
        .iter()
        .filter(|k| !filter.managed(k) || get(canonical_jobs, k).is_some())
        .count()
        + parts.adds.len();
    if live_jobs == 0 && counts.disabled == 0 {
        return Ok(MergeOutcome::WouldEmpty);
    }
    Ok(counts.outcome(content))
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

fn gitlab_job_keys(map: &serde_yaml::Mapping) -> Vec<String> {
    map.keys()
        .filter_map(Value::as_str)
        .filter(|k| !GITLAB_RESERVED.contains(k) && !k.starts_with('.'))
        .map(str::to_string)
        .collect()
}

fn merge_gitlab(
    file: &PlannedFile,
    filter: &JobFilter,
    existing: &str,
    original: &serde_yaml::Mapping,
    scan: Scan,
) -> Result<MergeOutcome> {
    let mut counts = Counts::default();
    let existing_keys = gitlab_job_keys(original);

    let blocks = unshadowed_blocks(scan.blocks, &existing_keys);
    let disabled_ids: Vec<String> = blocks.iter().flat_map(|b| b.ids.clone()).collect();
    let parts = partition(&file.jobs, &existing_keys, &disabled_ids, filter);

    let kept_all: Vec<Job> = parts.active.iter().chain(&parts.disabled).cloned().collect();
    let canonical = serde_yaml::to_value(crate::platforms::gitlab::lower::lower_gitlab(&kept_all))?;
    let canonical_map = canonical
        .as_mapping()
        .expect("GitLabCI serializes to a mapping");

    let edits = block_edits(&blocks, canonical_map, &mut counts)?;
    let text = apply_edits(existing, edits);

    let parsed = parse_lossless(&text, &file.path)?;
    let current: Value =
        serde_yaml::from_str(&text).context("regenerated comment blocks reparse")?;
    let current_map = current.as_mapping().context("document is a mapping")?;
    let doc = parsed.document().context("document is a mapping")?;
    let root = doc.as_mapping().context("document is a mapping")?;

    conform_jobs(
        &root,
        &existing_keys,
        current_map,
        &JobsPlan {
            canonical: canonical_map,
            adds: &parts.adds,
            filter,
            path: &file.path,
        },
        &mut counts,
    )?;

    // Conform `stages` to what the final jobs reference. Canonical stages
    // cover the disabled jobs too, so a commented job's stage stays and
    // uncommenting it works without another update.
    let mut final_jobs: Vec<&Value> = Vec::new();
    for key in &existing_keys {
        if filter.managed(key) {
            if let Some(canonical) = get(canonical_map, key) {
                final_jobs.push(canonical);
            }
        } else if let Some(value) = get(current_map, key) {
            final_jobs.push(value);
        }
    }
    for id in &parts.adds {
        if let Some(canonical) = get(canonical_map, id) {
            final_jobs.push(canonical);
        }
    }
    let referenced: Vec<&str> = final_jobs
        .iter()
        .filter_map(|v| v.get("stage").and_then(Value::as_str))
        .collect();
    let existing_stages: Vec<Value> = get(current_map, "stages")
        .and_then(Value::as_sequence)
        .cloned()
        .unwrap_or_default();
    let mut stages: Vec<Value> = existing_stages
        .iter()
        .filter(|s| s.as_str().is_some_and(|s| referenced.contains(&s)))
        .cloned()
        .collect();
    for stage in get(canonical_map, "stages")
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
    {
        if !stages.contains(stage) {
            stages.push(stage.clone());
        }
    }
    if stages.is_empty() {
        if get(current_map, "stages").is_some() {
            root.remove("stages");
        }
    } else if stages != existing_stages {
        let (_doc, node) = fragment("stages", &Value::Sequence(stages))?;
        root.set("stages", node);
    }

    // cibox used to hoist every job's environment into the top-level
    // `variables:`, from where it applied to all the other jobs too. Those
    // keys now live on the job that wants them, so drop the global copies —
    // left behind they would keep leaking. Variables cibox never sets are the
    // user's (they may feed custom jobs) and stay.
    let hoisted = gitlab_job_variable_names(&file.jobs);
    if let Some(Value::Mapping(vars)) = get(current_map, "variables") {
        if let Some(vars_node) = root.get_mapping("variables") {
            let mut remaining = 0;
            for key in vars.keys() {
                if hoisted.contains(key) {
                    if let Some(key) = key.as_str() {
                        vars_node.remove(key);
                    }
                } else {
                    remaining += 1;
                }
            }
            if remaining == 0 {
                root.remove("variables");
            }
        }
    }

    let mut content = parsed.to_string();
    let header = header_jobs(&file.jobs, filter, &disabled_ids, &existing_keys);
    crate::generator::reconcile_required_variables(Platform::GitLab, &header, &mut content);

    if content == existing {
        return Ok(MergeOutcome::Unchanged);
    }
    Ok(counts.outcome(content))
}

/// A CircleCI workflow invocation entry names its job either as a bare
/// string or as the single key of a parameterized mapping.
pub(crate) fn entry_id(entry: &Value) -> Option<String> {
    match entry {
        Value::String(id) => Some(id.clone()),
        Value::Mapping(m) if m.len() == 1 => {
            m.keys().next().and_then(Value::as_str).map(str::to_string)
        }
        _ => None,
    }
}

/// Parse a canonically rendered sequence so it can replace a `jobs:` value
/// wholesale. (yaml-edit's per-item sequence operations mis-indent block
/// mapping entries, so whole-value replacement through the parent mapping is
/// the only tree-level edit used on sequences.)
fn sequence_fragment(entries: &[Value]) -> Result<(Document, yaml_edit::Sequence)> {
    let text = serde_yaml::to_string(&Value::Sequence(entries.to_vec()))?;
    let doc = Document::from_str(&text)
        .map_err(|e| anyhow!("canonical workflow entries did not reparse: {e}"))?;
    let seq = doc
        .as_sequence()
        .context("canonical workflow entries did not round-trip")?;
    Ok((doc, seq))
}

enum EntryOp {
    Keep,
    Remove,
    Replace(Vec<Value>),
}

/// Decide the fate of each invocation entry in one workflow and the list it
/// adds up to. A matrix job is invoked once per leg, so one id can map to
/// several canonical entries: the first existing entry expands to all of
/// them, later ones with the same id are dropped. Returns the final list,
/// the per-entry ops, and the managed ids that stay invoked.
fn conform_entries(
    entries: &[Value],
    canonical_entries: &[Value],
    disabled_ids: &[String],
    filter: &JobFilter,
) -> (Vec<Value>, Vec<EntryOp>, Vec<String>) {
    let legs_of = |id: &str| -> Vec<Value> {
        canonical_entries
            .iter()
            .filter(|c| entry_id(c).as_deref() == Some(id))
            .cloned()
            .collect()
    };
    let mut seen: Vec<String> = Vec::new();
    let mut desired: Vec<Value> = Vec::new();
    let mut ops: Vec<EntryOp> = Vec::new();
    for entry in entries {
        match entry_id(entry) {
            Some(id) if filter.managed(&id) => {
                if disabled_ids.contains(&id) || seen.contains(&id) {
                    ops.push(EntryOp::Remove);
                    continue;
                }
                let legs = legs_of(&id);
                if legs.is_empty() {
                    ops.push(EntryOp::Remove);
                    continue;
                }
                seen.push(id);
                desired.extend(legs.iter().cloned());
                if legs.len() == 1 && legs[0] == *entry {
                    ops.push(EntryOp::Keep);
                } else {
                    ops.push(EntryOp::Replace(legs));
                }
            }
            _ => {
                desired.push(entry.clone());
                ops.push(EntryOp::Keep);
            }
        }
    }
    (desired, ops, seen)
}

/// Where one workflow's invocation entries live in the source text
struct WorkflowSpans {
    /// Whole-line byte span of each entry, in order (block style only)
    items: Vec<Range<usize>>,
    flow: bool,
    /// Offset just past the workflow's `jobs:` line, for appends when the
    /// list holds no entries
    jobs_line_end: Option<usize>,
}

fn workflow_spans(existing: &str, parsed: &YamlFile, name: &str) -> WorkflowSpans {
    let mut spans = WorkflowSpans {
        items: Vec::new(),
        flow: false,
        jobs_line_end: None,
    };
    let Some(doc) = parsed.document() else {
        return spans;
    };
    let Some(workflow) = doc
        .as_mapping()
        .and_then(|r| r.get_mapping("workflows"))
        .and_then(|w| w.get_mapping(name))
    else {
        return spans;
    };
    match workflow.get_sequence("jobs") {
        Some(seq) => {
            spans.flow = seq.is_flow_style();
            for value in seq.values() {
                if let Some(node) = value.as_node() {
                    let range = node.text_range();
                    spans.items.push(disabled::line_span(
                        existing,
                        &(usize::from(range.start())..usize::from(range.end())),
                    ));
                }
            }
        }
        None => {
            // `jobs:` may hold nothing (all entries commented out); find its
            // line within the workflow for appends
            if let Some(node) = workflow.as_node() {
                let range = node.text_range();
                let span = disabled::line_span(
                    existing,
                    &(usize::from(range.start())..usize::from(range.end())),
                );
                let mut offset = span.start;
                for line in existing[span].split_inclusive('\n') {
                    if line.trim_end().ends_with("jobs:") {
                        spans.jobs_line_end = Some(offset + line.len());
                        break;
                    }
                    offset += line.len();
                }
            }
        }
    }
    spans
}

fn line_indent(text: &str, offset: usize) -> usize {
    let line = &text[offset..];
    line.len() - line.trim_start().len()
}

fn merge_circleci(
    file: &PlannedFile,
    filter: &JobFilter,
    existing: &str,
    original: &serde_yaml::Mapping,
    original_parse: &YamlFile,
    scan: Scan,
) -> Result<MergeOutcome> {
    let mut counts = Counts::default();

    let existing_jobs: serde_yaml::Mapping = match get(original, "jobs") {
        Some(Value::Mapping(m)) => m.clone(),
        Some(Value::Null) => serde_yaml::Mapping::new(),
        _ => bail!(
            "{} has no `jobs:` mapping; use --force to overwrite it",
            file.path.display()
        ),
    };
    let existing_keys: Vec<String> = existing_jobs
        .keys()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();

    let blocks = unshadowed_blocks(scan.blocks, &existing_keys);
    let disabled_ids: Vec<String> = blocks.iter().flat_map(|b| b.ids.clone()).collect();
    let parts = partition(&file.jobs, &existing_keys, &disabled_ids, filter);

    let kept_all: Vec<Job> = parts.active.iter().chain(&parts.disabled).cloned().collect();
    let canonical =
        serde_yaml::to_value(crate::platforms::circleci::lower::lower_circleci(&kept_all))?;
    let empty = serde_yaml::Mapping::new();
    let canonical_jobs = canonical
        .get("jobs")
        .and_then(Value::as_mapping)
        .unwrap_or(&empty);
    let canonical_entries: Vec<Value> = canonical
        .get("workflows")
        .and_then(|w| w.get("main"))
        .and_then(|m| m.get("jobs"))
        .and_then(Value::as_sequence)
        .cloned()
        .unwrap_or_default();
    let legs_of = |id: &str| -> Vec<Value> {
        canonical_entries
            .iter()
            .filter(|c| entry_id(c).as_deref() == Some(id))
            .cloned()
            .collect()
    };

    // Text phase: commented job blocks, plus commented workflow entries —
    // kept in sync with the job's canonical invocations while it stays
    // disabled, dropped when the job is no longer generated at all
    let mut edits = block_edits(&blocks, canonical_jobs, &mut counts)?;
    for run in &scan.entries {
        // An id that isn't a disabled job makes this an ordinary comment
        if run.ids.iter().any(|id| !disabled_ids.contains(id)) {
            continue;
        }
        let expected: Vec<Value> = run.ids.iter().flat_map(|id| legs_of(id)).collect();
        if expected == run.entries {
            continue;
        }
        if expected.is_empty() {
            edits.push((run.span.clone(), String::new()));
            continue;
        }
        let rendered = serde_yaml::to_string(&Value::Sequence(expected))?;
        edits.push((run.span.clone(), disabled::comment_out(&rendered, run.indent)));
    }
    if existing_jobs.is_empty() {
        add_jobs_textually(existing, &parts.adds, canonical_jobs, &mut counts, &mut edits)?;
    }

    // A job also appears as an entry in a workflow's `jobs` list — conform
    // or drop those entries to match, leaving custom entries alone. The
    // lists are edited textually, item by item (spans from the syntax
    // tree), so comments between entries survive; flow-style lists are
    // replaced wholesale on the tree instead.
    let active_ids: Vec<String> = parts.active.iter().map(|j| j.id.clone()).collect();
    let mut invoked: Vec<String> = Vec::new();
    let mut drop_workflows: Vec<String> = Vec::new();
    let mut wholesale: Vec<(String, Vec<Value>)> = Vec::new();
    let mut main_append_at: Option<(usize, usize)> = None;
    if let Some(Value::Mapping(workflows)) = get(original, "workflows") {
        for (name, workflow) in workflows {
            let Some(name) = name.as_str() else { continue };
            let Some(entries) = workflow.get("jobs").and_then(Value::as_sequence) else {
                if name == "main" {
                    let spans = workflow_spans(existing, original_parse, name);
                    main_append_at = spans
                        .jobs_line_end
                        .map(|at| (at, line_indent(existing, at - 1)));
                }
                continue;
            };
            let (desired, ops, seen) =
                conform_entries(entries, &canonical_entries, &disabled_ids, filter);
            invoked.extend(seen);
            let spans = workflow_spans(existing, original_parse, name);
            if name == "main" {
                main_append_at = spans
                    .items
                    .last()
                    .map(|span| (span.end, line_indent(existing, span.start)));
            }
            if desired == *entries {
                continue;
            }
            // CircleCI rejects a workflow with no jobs — drop it, unless
            // commented entries still live inside it
            if desired.is_empty()
                && !entries.is_empty()
                && !scan.entries.iter().any(|r| r.workflow == name)
            {
                drop_workflows.push(name.to_string());
                continue;
            }
            if spans.flow || spans.items.len() != entries.len() {
                wholesale.push((name.to_string(), desired));
                continue;
            }
            for (op, span) in ops.iter().zip(&spans.items) {
                match op {
                    EntryOp::Keep => {}
                    EntryOp::Remove => edits.push((span.clone(), String::new())),
                    EntryOp::Replace(legs) => {
                        let rendered = serde_yaml::to_string(&Value::Sequence(legs.clone()))?;
                        edits.push((
                            span.clone(),
                            indent_by(&rendered, line_indent(existing, span.start)),
                        ));
                    }
                }
            }
        }
    }

    // Every active managed job needs an invocation somewhere; route the
    // missing ones to the `main` workflow (re-adds and user-deleted entries
    // alike)
    let missing_entries: Vec<Value> = active_ids
        .iter()
        .filter(|id| !invoked.contains(id))
        .flat_map(|id| legs_of(id))
        .collect();
    let mut build_main = false;
    if !missing_entries.is_empty() {
        if let Some((_, desired)) = wholesale.iter_mut().find(|(name, _)| name == "main") {
            desired.extend(missing_entries.iter().cloned());
        } else if let Some((at, indent)) = main_append_at {
            let rendered = serde_yaml::to_string(&Value::Sequence(missing_entries.clone()))?;
            edits.push((at..at, indent_by(&rendered, indent)));
        } else {
            build_main = true;
        }
    }

    let text = apply_edits(existing, edits);

    let parsed = parse_lossless(&text, &file.path)?;
    let current: Value =
        serde_yaml::from_str(&text).context("regenerated comment blocks reparse")?;
    let current_map = current.as_mapping().context("document is a mapping")?;
    let current_jobs = get(current_map, "jobs")
        .and_then(Value::as_mapping)
        .cloned()
        .unwrap_or_default();
    let current_keys: Vec<String> = current_jobs
        .keys()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    let doc = parsed.document().context("document is a mapping")?;
    let root = doc.as_mapping().context("document is a mapping")?;
    if let Some(jobs) = root.get_mapping("jobs") {
        conform_jobs(
            &jobs,
            &current_keys,
            &current_jobs,
            &JobsPlan {
                canonical: canonical_jobs,
                adds: &parts.adds,
                filter,
                path: &file.path,
            },
            &mut counts,
        )?;
    }

    if let Some(workflows_node) = root.get_mapping("workflows") {
        for name in &drop_workflows {
            workflows_node.remove(name.as_str());
        }
        for (name, desired) in &wholesale {
            if let Some(workflow) = workflows_node.get_mapping(name) {
                let (_doc, seq) = sequence_fragment(desired)?;
                workflow.set("jobs", seq);
            }
        }
    }
    if build_main {
        let mut main = serde_yaml::Mapping::new();
        main.insert(Value::from("jobs"), Value::Sequence(missing_entries));
        match root.get_mapping("workflows") {
            Some(workflows_node) => {
                let (_doc, node) = fragment("main", &Value::Mapping(main))?;
                workflows_node.set("main", node);
            }
            None => {
                let mut workflows = serde_yaml::Mapping::new();
                workflows.insert(Value::from("main"), Value::Mapping(main));
                let (_doc, node) = fragment("workflows", &Value::Mapping(workflows))?;
                root.set("workflows", node);
            }
        }
    }

    let mut content = parsed.to_string();
    let header = header_jobs(&file.jobs, filter, &disabled_ids, &existing_keys);
    crate::generator::reconcile_required_variables(Platform::CircleCI, &header, &mut content);

    if content == existing {
        return Ok(MergeOutcome::Unchanged);
    }
    Ok(counts.outcome(content))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CiboxConfig;
    use crate::detection::ProjectFacts;
    use crate::generator::{plan, render_file};
    use crate::rules::{resolve, ResolvedRule};
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

    /// Facts for a publishable Rust project without a Dockerfile
    fn cargo_facts() -> ProjectFacts {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        crate::detection::gather_facts(dir.path())
    }

    fn planned_file(platform: Platform, resolved: &[ResolvedRule], index: usize) -> PlannedFile {
        let facts = full_facts();
        plan(&facts, resolved, platform).unwrap().swap_remove(index)
    }

    fn no_selection(resolved: &[ResolvedRule]) -> JobFilter<'_> {
        JobFilter {
            resolved,
            selection: None,
        }
    }

    struct MergedParts {
        content: String,
        conformed: usize,
        added: usize,
        removed: usize,
        disabled: usize,
        preserved: usize,
    }

    fn merged(outcome: MergeOutcome) -> MergedParts {
        match outcome {
            MergeOutcome::Merged {
                content,
                conformed,
                added,
                removed,
                disabled,
                preserved,
            } => MergedParts {
                content,
                conformed,
                added,
                removed,
                disabled,
                preserved,
            },
            MergeOutcome::Unchanged => panic!("expected Merged, got Unchanged"),
            MergeOutcome::WouldEmpty => panic!("expected Merged, got WouldEmpty"),
        }
    }

    /// Comment one job's block out of canonical output, `# ` style at the
    /// block's own indentation
    fn comment_out_job(content: &str, id: &str) -> String {
        let mut out = String::new();
        let mut in_block = false;
        let mut indent = 0;
        for line in content.split_inclusive('\n') {
            let this_indent = line.len() - line.trim_start().len();
            if line.trim_end() == format!("{}{id}:", " ".repeat(this_indent))
                && line.trim_start().starts_with(id)
            {
                in_block = true;
                indent = this_indent;
                out.push_str(&format!("{}# {}", " ".repeat(indent), &line[indent..]));
                continue;
            }
            if in_block {
                if line.trim().is_empty() || this_indent > indent {
                    out.push_str(&format!("{}# {}", " ".repeat(indent), &line[indent..]));
                    continue;
                }
                in_block = false;
            }
            out.push_str(line);
        }
        out
    }

    #[test]
    fn test_github_conform_preserve_and_readd() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitHub, &resolved, 0);
        assert!(file.jobs.iter().any(|j| j.id == "rust-fmt"), "fixture rot");

        // rust-test has stale content, rust-fmt was deleted by the user, a
        // custom job and a custom trigger block were added
        let existing = r#"name: My CI
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

        let filter = no_selection(&resolved);
        let m = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());

        assert!(!m.content.contains("echo stale"), "{}", m.content);
        assert!(m.content.contains("cargo test"), "{}", m.content);
        assert!(m.content.contains("echo mine"), "{}", m.content);
        // Deleted jobs come back
        assert!(m.content.contains("rust-fmt"), "{}", m.content);
        assert!(m.content.contains("My CI"), "{}", m.content);
        assert!(m.content.contains("develop"), "{}", m.content);
        assert_eq!(m.conformed, 1);
        assert_eq!(m.added, file.jobs.len() - 1);
        assert_eq!((m.removed, m.disabled, m.preserved), (0, 0, 1));
    }

    #[test]
    fn test_github_disabled_rule_job_removed() {
        let facts = full_facts();
        let mut config = CiboxConfig::default();
        config.rust_fmt.enabled = Some(false);
        let resolved = resolve(&facts, &config);
        let file = planned_file(Platform::GitHub, &resolved, 0);

        let existing = r#"name: CI
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

        let filter = no_selection(&resolved);
        let m = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());
        assert!(!m.content.contains("rust-fmt"), "{}", m.content);
        assert_eq!(m.removed, 1);
    }

    #[test]
    fn test_github_deleted_dependency_comes_back_with_needs() {
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
        let existing = r#"name: Release
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

        let filter = no_selection(&resolved);
        let m = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());
        // The deleted leg is re-added, so the manifest keeps depending on it
        let doc: Value = serde_yaml::from_str(&m.content).unwrap();
        assert!(doc["jobs"]["docker-release-linux"].is_mapping(), "{}", m.content);
        let needs: Vec<&str> = doc["jobs"]["docker-release"]["needs"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(needs.contains(&"docker-release-linux"), "{}", m.content);
        assert!(needs.contains(&"docker-release-windows"), "{}", m.content);
    }

    #[test]
    fn test_github_needs_pruned_when_dependency_commented_out() {
        let facts = full_facts();
        let mut config = CiboxConfig::default();
        config.docker_release.platforms = Some(vec![
            crate::config::DockerPlatform::LinuxAmd64,
            crate::config::DockerPlatform::WindowsAmd64,
        ]);
        let resolved = resolve(&facts, &config);
        let file = planned_file(Platform::GitHub, &resolved, 1);
        let filter = no_selection(&resolved);

        let canonical = render_file(Platform::GitHub, &file).unwrap();
        let existing = comment_out_job(&canonical, "docker-release-linux");

        let m = merged(merge_file(Platform::GitHub, &file, &filter, &existing).unwrap());
        assert_eq!(m.disabled, 1, "{}", m.content);
        let doc: Value = serde_yaml::from_str(&m.content).unwrap();
        // Not an active job anymore...
        assert!(doc["jobs"]["docker-release-linux"].is_null(), "{}", m.content);
        // ...so the live manifest job must not depend on it
        let needs: Vec<&str> = doc["jobs"]["docker-release"]["needs"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(!needs.contains(&"docker-release-linux"), "{}", m.content);
        assert!(needs.contains(&"docker-release-windows"), "{}", m.content);
        // The commented block itself survives
        assert!(m.content.contains("# docker-release-linux:"), "{}", m.content);
    }

    #[test]
    fn test_github_strip_to_empty_leaves_file_alone() {
        // Nothing detected: every rule is off, so nothing gets re-added and
        // pruning the last job would leave an empty workflow
        let facts = ProjectFacts::default();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = plan(&facts, &resolved, Platform::GitHub).unwrap().swap_remove(0);
        let filter = no_selection(&resolved);

        let existing = r#"name: CI
on: [push]
jobs:
  rust-fmt:
    runs-on: ubuntu-latest
    steps:
      - run: cargo fmt --check
"#;

        assert!(matches!(
            merge_file(Platform::GitHub, &file, &filter, existing).unwrap(),
            MergeOutcome::WouldEmpty
        ));
    }

    #[test]
    fn test_github_scaffolds_permissions_but_never_overrides_them() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitHub, &resolved, 0);
        let filter = no_selection(&resolved);

        // A workflow predating the permissions block gets the read-only
        // default, so the update actually fixes an over-privileged token
        let without = r#"name: CI
on: [push]
jobs:
  rust-test:
    runs-on: ubuntu-latest
    steps:
      - run: echo stale
"#;
        let m = merged(merge_file(Platform::GitHub, &file, &filter, without).unwrap());
        let doc: Value = serde_yaml::from_str(&m.content).unwrap();
        assert_eq!(doc["permissions"]["contents"].as_str(), Some("read"));
        // Scaffolded keys land in the preamble, not after the job list
        assert!(
            m.content.find("permissions:").unwrap() < m.content.find("jobs:").unwrap(),
            "{}",
            m.content
        );

        // A user who widened the token on purpose keeps their choice
        let with = r#"name: CI
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
        let m = merged(merge_file(Platform::GitHub, &file, &filter, with).unwrap());
        let doc: Value = serde_yaml::from_str(&m.content).unwrap();
        assert_eq!(doc["permissions"]["contents"].as_str(), Some("write"));
        assert_eq!(doc["permissions"]["id-token"].as_str(), Some("write"));
    }

    #[test]
    fn test_merge_is_idempotent_on_canonical_output() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let filter = no_selection(&resolved);
        for platform in Platform::all() {
            for file in plan(&facts, &resolved, platform).unwrap() {
                if file.jobs.is_empty() {
                    continue;
                }
                let canonical = render_file(platform, &file).unwrap();
                assert!(
                    matches!(
                        merge_file(platform, &file, &filter, &canonical).unwrap(),
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
        let filter = no_selection(&resolved);

        for bad in ["jobs: [unclosed", "- a scalar list, not a mapping"] {
            let err = merge_file(Platform::GitHub, &file, &filter, bad).unwrap_err();
            assert!(format!("{err:#}").contains("--force"), "{err:#}");
        }
    }

    #[test]
    fn test_merge_preserves_user_comments_and_formatting() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitHub, &resolved, 0);
        let filter = no_selection(&resolved);

        let existing = r#"name: My CI   # named with care
# trigger notes live here
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
      # keep this hack until the flake is fixed
      - run: echo mine
"#;
        let m = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());
        assert!(m.content.contains("# named with care"), "{}", m.content);
        assert!(m.content.contains("# trigger notes live here"), "{}", m.content);
        assert!(m.content.contains("# keep this hack"), "{}", m.content);
        assert!(!m.content.contains("echo stale"), "{}", m.content);
    }

    #[test]
    fn test_github_commented_job_updates_but_stays_commented() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitHub, &resolved, 0);
        let filter = no_selection(&resolved);

        let canonical = render_file(Platform::GitHub, &file).unwrap();
        let mut existing = comment_out_job(&canonical, "rust-test");
        // Make the commented content stale
        existing = existing.replace("cargo test", "cargo test --old-flag");

        let m = merged(merge_file(Platform::GitHub, &file, &filter, &existing).unwrap());
        assert_eq!(m.disabled, 1, "{}", m.content);
        assert_eq!(m.conformed, 0, "{}", m.content);
        assert!(m.content.contains("# rust-test:"), "{}", m.content);
        assert!(m.content.contains("cargo test"), "{}", m.content);
        assert!(!m.content.contains("--old-flag"), "{}", m.content);
        let doc: Value = serde_yaml::from_str(&m.content).unwrap();
        assert!(doc["jobs"]["rust-test"].is_null(), "{}", m.content);

        // Merging the output again is a no-op
        assert!(matches!(
            merge_file(Platform::GitHub, &file, &filter, &m.content).unwrap(),
            MergeOutcome::Unchanged
        ));
    }

    #[test]
    fn test_github_canonical_commented_job_is_unchanged_in_any_style() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitHub, &resolved, 0);
        let filter = no_selection(&resolved);
        let canonical = render_file(Platform::GitHub, &file).unwrap();

        // `# ` style, and the user's own `#` style: both stay untouched when
        // the content is current
        let ours = comment_out_job(&canonical, "rust-fmt");
        let theirs = ours.replace("  # ", "  #");
        for existing in [ours, theirs] {
            assert!(
                matches!(
                    merge_file(Platform::GitHub, &file, &filter, &existing).unwrap(),
                    MergeOutcome::Unchanged
                ),
                "{existing}"
            );
        }
    }

    #[test]
    fn test_github_commented_job_pruned_when_rule_disabled() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitHub, &resolved, 0);
        let canonical = render_file(Platform::GitHub, &file).unwrap();
        let existing = comment_out_job(&canonical, "rust-fmt");

        let mut config = CiboxConfig::default();
        config.rust_fmt.enabled = Some(false);
        let resolved = resolve(&facts, &config);
        let file = planned_file(Platform::GitHub, &resolved, 0);
        let filter = no_selection(&resolved);

        let m = merged(merge_file(Platform::GitHub, &file, &filter, &existing).unwrap());
        assert_eq!(m.removed, 1, "{}", m.content);
        assert_eq!(m.disabled, 0, "{}", m.content);
        assert!(!m.content.contains("rust-fmt"), "{}", m.content);
    }

    #[test]
    fn test_github_all_jobs_commented_still_updates() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitHub, &resolved, 0);
        let filter = no_selection(&resolved);

        let mut existing = render_file(Platform::GitHub, &file).unwrap();
        for job in &file.jobs {
            existing = comment_out_job(&existing, &job.id);
        }
        let existing = existing.replace("cargo test", "cargo test --old-flag");

        let m = merged(merge_file(Platform::GitHub, &file, &filter, &existing).unwrap());
        assert_eq!(m.disabled, file.jobs.len(), "{}", m.content);
        assert!(!m.content.contains("--old-flag"), "{}", m.content);
        let doc: Value = serde_yaml::from_str(&m.content).unwrap();
        assert!(doc["jobs"].is_null(), "{}", m.content);
    }

    #[test]
    fn test_rule_selection_only_touches_selected_jobs() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitHub, &resolved, 0);
        let selection: std::collections::BTreeSet<String> =
            std::iter::once("rust-test".to_string()).collect();
        let filter = JobFilter {
            resolved: &resolved,
            selection: Some(&selection),
        };

        let existing = r#"name: CI
on: [push]
jobs:
  rust-test:
    runs-on: ubuntu-latest
    steps:
      - run: echo stale
  rust-fmt:
    runs-on: ubuntu-latest
    steps:
      - run: echo stale too
  my-custom:
    runs-on: ubuntu-latest
    steps:
      - run: echo mine
"#;

        let m = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());
        assert_eq!(m.conformed, 1, "{}", m.content);
        // Unselected cibox jobs are left exactly as found, and missing ones
        // are not added
        assert!(m.content.contains("echo stale too"), "{}", m.content);
        assert_eq!(m.added, 0, "{}", m.content);
        assert_eq!(m.preserved, 2, "{}", m.content);
        assert!(!m.content.contains("rust-clippy"), "{}", m.content);
    }

    #[test]
    fn test_rule_selection_shields_unselected_jobs_from_pruning() {
        let facts = full_facts();
        let mut config = CiboxConfig::default();
        config.rust_fmt.enabled = Some(false);
        let resolved = resolve(&facts, &config);
        let file = planned_file(Platform::GitHub, &resolved, 0);
        let selection: std::collections::BTreeSet<String> =
            std::iter::once("rust-test".to_string()).collect();
        let filter = JobFilter {
            resolved: &resolved,
            selection: Some(&selection),
        };

        let existing = r#"name: CI
on: [push]
jobs:
  rust-test:
    runs-on: ubuntu-latest
    steps:
      - run: echo stale
  rust-fmt:
    runs-on: ubuntu-latest
    steps:
      - run: cargo fmt --check
"#;

        let m = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());
        // rust-fmt's rule is disabled, but it isn't selected, so it stays
        assert!(m.content.contains("rust-fmt"), "{}", m.content);
        assert_eq!(m.removed, 0, "{}", m.content);
    }

    #[test]
    fn test_gitlab_preserves_custom_config_and_conforms_managed() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitLab, &resolved, 0);
        let filter = no_selection(&resolved);

        let existing = r#"include:
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

        let m = merged(merge_file(Platform::GitLab, &file, &filter, existing).unwrap());

        assert!(m.content.contains("extra.yml"), "{}", m.content);
        assert!(m.content.contains(".hidden-template"), "{}", m.content);
        assert!(m.content.contains("MINE"), "{}", m.content);
        assert!(m.content.contains("my-job"), "{}", m.content);
        assert!(m.content.contains("cargo test"), "{}", m.content);
        assert!(!m.content.contains("echo stale"), "{}", m.content);
        // The custom stage survives because my-job references it
        assert!(m.content.contains("custom"), "{}", m.content);
        assert_eq!(m.conformed, 1);
        assert_eq!(m.added, file.jobs.len() - 1);
        assert_eq!((m.removed, m.preserved), (0, 1));
    }

    #[test]
    fn test_gitlab_update_narrows_any_tag_release_gating() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitLab, &resolved, 0);
        let filter = no_selection(&resolved);

        // A pipeline generated before release jobs were gated on `v*`
        let existing = "stages: [deploy]\n\
                        rust-release:\n  stage: deploy\n  script: [cargo publish]\n  \
                        only:\n    refs: [tags]\n\
                        my-release:\n  stage: deploy\n  script: [echo mine]\n  \
                        only:\n    refs: [tags]\n";
        let m = merged(merge_file(Platform::GitLab, &file, &filter, existing).unwrap());
        let doc: Value = serde_yaml::from_str(&m.content).unwrap();

        assert_eq!(
            doc["rust-release"]["rules"][0]["if"].as_str(),
            Some("$CI_COMMIT_TAG =~ /^v/")
        );
        assert!(doc["rust-release"]["only"].is_null(), "{}", m.content);
        // The user's own job is not cibox's to re-gate
        assert_eq!(doc["my-release"]["only"]["refs"][0].as_str(), Some("tags"));
    }

    #[test]
    fn test_gitlab_update_moves_hoisted_variables_onto_their_jobs() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitLab, &resolved, 0);
        let filter = no_selection(&resolved);

        // A pipeline generated before job environments stayed on their jobs:
        // the doc job's RUSTDOCFLAGS applied to rust-test's doctests too
        let existing = "stages: [test, lint]\n\
                        variables:\n  CARGO_HOME: .cargo\n  \
                        RUSTDOCFLAGS: --cfg docsrs\n  MINE: '1'\n\
                        rust-test:\n  stage: test\n  script: [echo stale]\n\
                        rust-doc:\n  stage: lint\n  script: [echo stale]\n";
        let m = merged(merge_file(Platform::GitLab, &file, &filter, existing).unwrap());
        let doc: Value = serde_yaml::from_str(&m.content).unwrap();

        assert!(doc["variables"]["RUSTDOCFLAGS"].is_null(), "{}", m.content);
        assert!(doc["variables"]["CARGO_HOME"].is_null(), "{}", m.content);
        // A variable cibox doesn't set is the user's to keep
        assert_eq!(doc["variables"]["MINE"].as_str(), Some("1"));

        assert_eq!(
            doc["rust-doc"]["variables"]["RUSTDOCFLAGS"].as_str(),
            Some("--cfg docsrs")
        );
        assert_eq!(doc["rust-test"]["variables"]["CARGO_HOME"].as_str(), Some(".cargo"));
        assert!(doc["rust-test"]["variables"]["RUSTDOCFLAGS"].is_null(), "{}", m.content);
    }

    #[test]
    fn test_gitlab_update_drops_a_variables_block_left_empty() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitLab, &resolved, 0);
        let filter = no_selection(&resolved);

        let existing = "stages: [test]\nvariables:\n  CARGO_HOME: .cargo\n\
                        rust-test:\n  stage: test\n  script: [echo stale]\n";
        let m = merged(merge_file(Platform::GitLab, &file, &filter, existing).unwrap());
        assert!(!m.content.contains("variables:\n  CARGO_HOME"), "{}", m.content);
        let doc: Value = serde_yaml::from_str(&m.content).unwrap();
        assert!(doc.get("variables").is_none(), "{}", m.content);
    }

    #[test]
    fn test_merge_reconciles_required_variables_header() {
        // Without a Dockerfile the only secret-bearing job is rust-release
        let facts = cargo_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = plan(&facts, &resolved, Platform::GitLab).unwrap().swap_remove(0);
        let filter = no_selection(&resolved);

        // The secret-bearing job gets re-added, so the header appears
        let existing = "stages: [test]\nrust-test:\n  stage: test\n  script: [echo stale]\n";
        let m = merged(merge_file(Platform::GitLab, &file, &filter, existing).unwrap());
        assert!(
            m.content
                .starts_with("# Required CI variables: CARGO_REGISTRY_TOKEN\n"),
            "{}",
            m.content
        );
        // ...exactly once, even though the next merge starts from a file
        // that already carries it
        let twice = merged(
            merge_file(
                Platform::GitLab,
                &file,
                &filter,
                &m.content.replace("cargo publish", "echo stale"),
            )
            .unwrap(),
        );
        assert_eq!(twice.content.matches("Required CI variables").count(), 1);

        // No secret-bearing job left: the header goes away
        let mut config = CiboxConfig::default();
        config.rust_release.enabled = Some(false);
        let resolved = resolve(&facts, &config);
        let file = plan(&facts, &resolved, Platform::GitLab).unwrap().swap_remove(0);
        let filter = no_selection(&resolved);
        let m = merged(merge_file(Platform::GitLab, &file, &filter, &m.content).unwrap());
        assert!(!m.content.contains("Required CI variables"), "{}", m.content);

        // GitHub workflows name their secrets inline, so no header there
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitHub, &resolved, 1);
        let filter = no_selection(&resolved);
        let existing = "name: Release\non:\n  push:\n    tags: [v*]\njobs:\n  \
                        rust-release:\n    runs-on: ubuntu-latest\n    steps:\n      \
                        - run: echo stale\n";
        let m = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());
        assert!(!m.content.contains("Required CI variables"), "{}", m.content);
    }

    #[test]
    fn test_gitlab_commented_job_keeps_stage_and_sheds_secrets() {
        let facts = cargo_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = plan(&facts, &resolved, Platform::GitLab).unwrap().swap_remove(0);
        let filter = no_selection(&resolved);

        let canonical = render_file(Platform::GitLab, &file).unwrap();
        let existing = comment_out_job(&canonical, "rust-release");

        let m = merged(merge_file(Platform::GitLab, &file, &filter, &existing).unwrap());
        assert_eq!(m.disabled, 1, "{}", m.content);
        // A commented job doesn't run, so its secret isn't required...
        assert!(!m.content.contains("Required CI variables"), "{}", m.content);
        assert!(m.content.contains("# rust-release:"), "{}", m.content);
        // ...but its stage stays declared, so uncommenting just works
        let doc: Value = serde_yaml::from_str(&m.content).unwrap();
        let stages: Vec<&str> = doc["stages"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(stages.contains(&"deploy"), "{}", m.content);

        assert!(matches!(
            merge_file(Platform::GitLab, &file, &filter, &m.content).unwrap(),
            MergeOutcome::Unchanged
        ));
    }

    #[test]
    fn test_merge_is_idempotent_with_versions() {
        let facts = full_facts();
        let mut config = CiboxConfig::default();
        config.rust_test.versions = Some(vec!["1.85".to_string(), "nightly".to_string()]);
        let resolved = resolve(&facts, &config);
        let filter = no_selection(&resolved);
        for platform in Platform::all() {
            for file in plan(&facts, &resolved, platform).unwrap() {
                let canonical = render_file(platform, &file).unwrap();
                assert!(
                    matches!(
                        merge_file(platform, &file, &filter, &canonical).unwrap(),
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
        let filter = no_selection(&resolved);

        let existing = "name: CI\non: [push]\njobs:\n  rust-test:\n    \
                        runs-on: ubuntu-latest\n    steps:\n      - run: echo stale\n";
        let m = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());
        assert!(m.content.contains("strategy:"), "{}", m.content);
        assert!(m.content.contains("fail-fast: false"), "{}", m.content);
        assert!(m.content.contains("rustlang/rust:nightly"), "{}", m.content);
        assert!(!m.content.contains("echo stale"), "{}", m.content);
    }

    #[test]
    fn test_circleci_matrix_expands_workflow_invocations() {
        let facts = full_facts();
        let mut config = CiboxConfig::default();
        config.rust_test.versions = Some(vec!["1.85".to_string(), "nightly".to_string()]);
        let resolved = resolve(&facts, &config);
        let file = planned_file(Platform::CircleCI, &resolved, 0);
        let filter = no_selection(&resolved);

        let existing = r#"version: "2.1"
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

        let m = merged(merge_file(Platform::CircleCI, &file, &filter, existing).unwrap());
        assert!(m.content.contains("rust-test-1.85"), "{}", m.content);
        assert!(m.content.contains("rust-test-nightly"), "{}", m.content);
        assert!(m.content.contains("my-job"), "{}", m.content);
        assert!(m.content.contains("<< parameters.image >>"), "{}", m.content);
        // Merging the merged output again is a no-op: the expanded
        // invocations map back onto the same canonical entries
        assert!(matches!(
            merge_file(Platform::CircleCI, &file, &filter, &m.content).unwrap(),
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
        let filter = no_selection(&resolved);

        let existing = r#"version: "2.1"
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

        let m = merged(merge_file(Platform::CircleCI, &file, &filter, existing).unwrap());
        assert!(!m.content.contains("rust-test-nightly"), "{}", m.content);
        assert!(!m.content.contains("parameters"), "{}", m.content);
        assert!(m.content.contains("rust:1.85"), "{}", m.content);
        // Exactly one rust-test invocation remains
        assert_eq!(m.content.matches("- rust-test").count(), 1, "{}", m.content);
    }

    #[test]
    fn test_circleci_conforms_both_jobs_and_workflow_entries() {
        let facts = full_facts();
        let mut config = CiboxConfig::default();
        config.rust_fmt.enabled = Some(false);
        let resolved = resolve(&facts, &config);
        let file = planned_file(Platform::CircleCI, &resolved, 0);
        let filter = no_selection(&resolved);

        let existing = r#"version: "2.1"
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

        let m = merged(merge_file(Platform::CircleCI, &file, &filter, existing).unwrap());

        assert!(!m.content.contains("rust-fmt"), "{}", m.content);
        assert!(m.content.contains("my-job"), "{}", m.content);
        assert!(m.content.contains("nightly"), "{}", m.content);
        // A workflow left with no jobs is dropped entirely
        assert!(!m.content.contains("stale-only"), "{}", m.content);
        assert!(m.content.contains("cargo test"), "{}", m.content);
        assert!(!m.content.contains("echo stale"), "{}", m.content);
        assert_eq!((m.conformed, m.removed, m.preserved), (1, 1, 1));
    }

    #[test]
    fn test_circleci_commented_job_and_entries_stay_commented() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::CircleCI, &resolved, 0);
        let filter = no_selection(&resolved);

        let canonical = render_file(Platform::CircleCI, &file).unwrap();
        // Comment out the job definition and its workflow invocation
        let mut existing = comment_out_job(&canonical, "rust-test");
        assert!(existing.contains("    - rust-test\n"), "fixture rot: {existing}");
        existing = existing.replace("    - rust-test\n", "    # - rust-test\n");

        // Both commented pieces are already canonical: nothing to write
        assert!(matches!(
            merge_file(Platform::CircleCI, &file, &filter, &existing).unwrap(),
            MergeOutcome::Unchanged
        ));

        // Stale commented content gets regenerated, still commented
        let stale = existing.replace("cargo test", "cargo test --old-flag");
        let m = merged(merge_file(Platform::CircleCI, &file, &filter, &stale).unwrap());
        assert_eq!(m.disabled, 1, "{}", m.content);
        assert!(!m.content.contains("--old-flag"), "{}", m.content);
        assert!(m.content.contains("# rust-test:"), "{}", m.content);
        assert!(m.content.contains("# - rust-test"), "{}", m.content);
        assert!(matches!(
            merge_file(Platform::CircleCI, &file, &filter, &m.content).unwrap(),
            MergeOutcome::Unchanged
        ));
    }
}

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
    Merged { content: String, counts: Counts },
}

/// How many jobs the merge did each thing to, for the command's summary line
#[derive(Debug, Default)]
pub struct Counts {
    /// Owned jobs replaced with canonical content
    pub conformed: usize,
    /// Owned jobs re-added because they were missing
    pub added: usize,
    /// Owned jobs removed because their rule no longer generates them
    pub removed: usize,
    /// Commented-out jobs kept commented, with regenerated content
    pub disabled: usize,
    /// Jobs cibox doesn't manage, left untouched
    pub preserved: usize,
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

/// The plan measured against the file: which managed jobs stay active, which
/// the user commented out (and the comment blocks holding them), and which
/// are missing and have to be re-added. `needs` edges are pruned to jobs that
/// exist on the same side of the merge.
struct Split {
    /// Comment blocks that really do define disabled managed jobs
    blocks: Vec<DisabledBlock>,
    /// The ids `blocks` define, i.e. the jobs the user disabled
    disabled_ids: Vec<String>,
    /// Managed jobs that will be active in the file, in plan order
    active: Vec<Job>,
    /// Managed jobs the user commented out
    disabled: Vec<Job>,
    /// Ids of active jobs missing from the file, to re-add
    adds: Vec<String>,
}

impl Split {
    /// Every managed job that survives the merge, active or commented out —
    /// what the canonical output has to cover
    fn kept(&self) -> Vec<Job> {
        self.active.iter().chain(&self.disabled).cloned().collect()
    }

    fn active_ids(&self) -> Vec<String> {
        self.active.iter().map(|j| j.id.clone()).collect()
    }
}

fn split_plan(
    planned: &[Job],
    existing_keys: &[String],
    blocks: Vec<DisabledBlock>,
    filter: &JobFilter,
) -> Split {
    // An active key shadows a commented block of the same id: the block is
    // then an ordinary comment, not a disabled job.
    let blocks: Vec<DisabledBlock> = blocks
        .into_iter()
        .filter(|b| !b.ids.iter().any(|id| existing_keys.contains(id)))
        .collect();
    let disabled_ids: Vec<String> = blocks.iter().flat_map(|b| b.ids.clone()).collect();

    let (mut disabled, mut active): (Vec<Job>, Vec<Job>) = planned
        .iter()
        .filter(|j| filter.managed(&j.id))
        .cloned()
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
    Split {
        blocks,
        disabled_ids,
        active,
        disabled,
        adds,
    }
}

fn get<'a>(map: &'a serde_yaml::Mapping, key: &str) -> Option<&'a Value> {
    map.get(Value::from(key))
}

/// The mapping's keys, in order, skipping any that aren't plain strings
fn string_keys(map: &serde_yaml::Mapping) -> Vec<String> {
    map.keys()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// The existing top-level `jobs:` mapping on the platforms that nest their
/// jobs under one. Holding nothing is legitimate — that is what every job
/// being commented out or deleted looks like — but a file without the key at
/// all is not a pipeline cibox can merge into.
fn existing_job_mapping(
    original: &serde_yaml::Mapping,
    path: &Path,
) -> Result<serde_yaml::Mapping> {
    match get(original, "jobs") {
        Some(Value::Mapping(m)) => Ok(m.clone()),
        Some(Value::Null) => Ok(serde_yaml::Mapping::new()),
        _ => bail!(
            "{} has no `jobs:` mapping; use --force to overwrite it",
            path.display()
        ),
    }
}

/// The `jobs:` mapping of freshly lowered canonical output, empty when the
/// resolution left no job for this file
fn canonical_job_mapping(canonical: &Value) -> serde_yaml::Mapping {
    canonical
        .get("jobs")
        .and_then(Value::as_mapping)
        .cloned()
        .unwrap_or_default()
}

fn root_mapping(parsed: &YamlFile) -> Result<Mapping> {
    parsed
        .document()
        .and_then(|doc| doc.as_mapping())
        .context("document is a mapping")
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

/// Where a platform keeps its jobs in the document.
enum JobsAt {
    /// Under a top-level `jobs:` mapping (GitHub/Gitea, CircleCI)
    JobsKey,
    /// At the document root, mixed with configuration keys (GitLab)
    Root,
}

/// The tree phase every merge ends with: reparse the text-edited document and
/// conform the managed jobs on the lossless syntax tree. Returns the parse —
/// whose tree the caller may keep editing before rendering it — alongside a
/// plain-value view of the same text.
fn conform_document(
    text: &str,
    at: JobsAt,
    file: &PlannedFile,
    filter: &JobFilter,
    canonical_jobs: &serde_yaml::Mapping,
    adds: &[String],
    counts: &mut Counts,
) -> Result<(YamlFile, Value)> {
    let parsed = parse_lossless(text, &file.path)?;
    let current: Value = serde_yaml::from_str(text).context("regenerated comment blocks reparse")?;
    let plan = JobsPlan {
        canonical: canonical_jobs,
        adds,
        filter,
        path: &file.path,
    };
    let current_map = current.as_mapping().context("document is a mapping")?;
    let root = root_mapping(&parsed)?;
    match at {
        JobsAt::Root => {
            conform_jobs(&root, &gitlab_job_keys(current_map), current_map, &plan, counts)?;
        }
        JobsAt::JobsKey => {
            let jobs = get(current_map, "jobs")
                .and_then(Value::as_mapping)
                .cloned()
                .unwrap_or_default();
            if let Some(node) = root.get_mapping("jobs") {
                conform_jobs(&node, &string_keys(&jobs), &jobs, &plan, counts)?;
            }
        }
    }
    Ok((parsed, current))
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

    let existing_jobs = existing_job_mapping(original, &file.path)?;
    let existing_keys = string_keys(&existing_jobs);
    let split = split_plan(&file.jobs, &existing_keys, scan.blocks, filter);

    let canonical = serde_yaml::to_value(crate::platforms::github::lower::lower_github(
        &split.kept(),
        file.kind,
    ))?;
    let canonical_jobs = &canonical_job_mapping(&canonical);

    // Text phase: the commented blocks live outside the YAML value tree
    let mut edits = block_edits(&split.blocks, canonical_jobs, &mut counts)?;
    if existing_jobs.is_empty() {
        add_jobs_textually(existing, &split.adds, canonical_jobs, &mut counts, &mut edits)?;
    }
    let text = apply_edits(existing, edits);

    // Tree phase: conform the live jobs on the lossless syntax tree
    let (parsed, current) = conform_document(
        &text,
        JobsAt::JobsKey,
        file,
        filter,
        canonical_jobs,
        &split.adds,
        &mut counts,
    )?;
    let current_map = current.as_mapping().context("document is a mapping")?;
    let root = root_mapping(&parsed)?;

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
        + split.adds.len();
    if live_jobs == 0 && counts.disabled == 0 {
        return Ok(MergeOutcome::WouldEmpty);
    }
    Ok(MergeOutcome::Merged { content, counts })
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
    let split = split_plan(&file.jobs, &existing_keys, scan.blocks, filter);

    let canonical =
        serde_yaml::to_value(crate::platforms::gitlab::lower::lower_gitlab(&split.kept()))?;
    let canonical_map = canonical
        .as_mapping()
        .expect("GitLabCI serializes to a mapping");

    let edits = block_edits(&split.blocks, canonical_map, &mut counts)?;
    let text = apply_edits(existing, edits);

    let (parsed, current) = conform_document(
        &text,
        JobsAt::Root,
        file,
        filter,
        canonical_map,
        &split.adds,
        &mut counts,
    )?;
    let current_map = current.as_mapping().context("document is a mapping")?;
    let root = root_mapping(&parsed)?;

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
    for id in &split.adds {
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
    let header = header_jobs(&file.jobs, filter, &split.disabled_ids, &existing_keys);
    crate::generator::reconcile_required_variables(Platform::GitLab, &header, &mut content);

    if content == existing {
        return Ok(MergeOutcome::Unchanged);
    }
    Ok(MergeOutcome::Merged { content, counts })
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

/// The canonical invocation entries of one job. A matrix job is invoked once
/// per leg, so an id can map to several entries; a job that is no longer
/// generated maps to none.
fn legs_of(canonical_entries: &[Value], id: &str) -> Vec<Value> {
    canonical_entries
        .iter()
        .filter(|c| entry_id(c).as_deref() == Some(id))
        .cloned()
        .collect()
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
                let legs = legs_of(canonical_entries, &id);
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

    let existing_jobs = existing_job_mapping(original, &file.path)?;
    let existing_keys = string_keys(&existing_jobs);
    let split = split_plan(&file.jobs, &existing_keys, scan.blocks, filter);

    let canonical = serde_yaml::to_value(crate::platforms::circleci::lower::lower_circleci(
        &split.kept(),
    ))?;
    let canonical_jobs = &canonical_job_mapping(&canonical);
    let canonical_entries: Vec<Value> = canonical
        .get("workflows")
        .and_then(|w| w.get("main"))
        .and_then(|m| m.get("jobs"))
        .and_then(Value::as_sequence)
        .cloned()
        .unwrap_or_default();

    // Text phase: commented job blocks, plus commented workflow entries —
    // kept in sync with the job's canonical invocations while it stays
    // disabled, dropped when the job is no longer generated at all
    let mut edits = block_edits(&split.blocks, canonical_jobs, &mut counts)?;
    for run in &scan.entries {
        // An id that isn't a disabled job makes this an ordinary comment
        if run.ids.iter().any(|id| !split.disabled_ids.contains(id)) {
            continue;
        }
        let expected: Vec<Value> = run
            .ids
            .iter()
            .flat_map(|id| legs_of(&canonical_entries, id))
            .collect();
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
        add_jobs_textually(existing, &split.adds, canonical_jobs, &mut counts, &mut edits)?;
    }

    // A job also appears as an entry in a workflow's `jobs` list — conform
    // or drop those entries to match, leaving custom entries alone. The
    // lists are edited textually, item by item (spans from the syntax
    // tree), so comments between entries survive; flow-style lists are
    // replaced wholesale on the tree instead.
    let active_ids = split.active_ids();
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
                conform_entries(entries, &canonical_entries, &split.disabled_ids, filter);
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
        .flat_map(|id| legs_of(&canonical_entries, id))
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

    let (parsed, _) = conform_document(
        &text,
        JobsAt::JobsKey,
        file,
        filter,
        canonical_jobs,
        &split.adds,
        &mut counts,
    )?;
    let root = root_mapping(&parsed)?;

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
    let header = header_jobs(&file.jobs, filter, &split.disabled_ids, &existing_keys);
    crate::generator::reconcile_required_variables(Platform::CircleCI, &header, &mut content);

    if content == existing {
        return Ok(MergeOutcome::Unchanged);
    }
    Ok(MergeOutcome::Merged { content, counts })
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

    /// The content and counts of a merge that is expected to have changed
    /// something
    fn merged(outcome: MergeOutcome) -> (String, Counts) {
        match outcome {
            MergeOutcome::Merged { content, counts } => (content, counts),
            other => panic!("expected Merged, got {other:?}"),
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
        let (content, counts) = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());

        assert!(!content.contains("echo stale"), "{}", content);
        assert!(content.contains("cargo test"), "{}", content);
        assert!(content.contains("echo mine"), "{}", content);
        // Deleted jobs come back
        assert!(content.contains("rust-fmt"), "{}", content);
        assert!(content.contains("My CI"), "{}", content);
        assert!(content.contains("develop"), "{}", content);
        assert_eq!(counts.conformed, 1);
        assert_eq!(counts.added, file.jobs.len() - 1);
        assert_eq!((counts.removed, counts.disabled, counts.preserved), (0, 0, 1));
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
        let (content, counts) = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());
        assert!(!content.contains("rust-fmt"), "{}", content);
        assert_eq!(counts.removed, 1);
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
        let (content, _) = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());
        // The deleted leg is re-added, so the manifest keeps depending on it
        let doc: Value = serde_yaml::from_str(&content).unwrap();
        assert!(doc["jobs"]["docker-release-linux"].is_mapping(), "{}", content);
        let needs: Vec<&str> = doc["jobs"]["docker-release"]["needs"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(needs.contains(&"docker-release-linux"), "{}", content);
        assert!(needs.contains(&"docker-release-windows"), "{}", content);
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

        let (content, counts) = merged(merge_file(Platform::GitHub, &file, &filter, &existing).unwrap());
        assert_eq!(counts.disabled, 1, "{}", content);
        let doc: Value = serde_yaml::from_str(&content).unwrap();
        // Not an active job anymore...
        assert!(doc["jobs"]["docker-release-linux"].is_null(), "{}", content);
        // ...so the live manifest job must not depend on it
        let needs: Vec<&str> = doc["jobs"]["docker-release"]["needs"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(!needs.contains(&"docker-release-linux"), "{}", content);
        assert!(needs.contains(&"docker-release-windows"), "{}", content);
        // The commented block itself survives
        assert!(content.contains("# docker-release-linux:"), "{}", content);
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
        let (content, _) = merged(merge_file(Platform::GitHub, &file, &filter, without).unwrap());
        let doc: Value = serde_yaml::from_str(&content).unwrap();
        assert_eq!(doc["permissions"]["contents"].as_str(), Some("read"));
        // Scaffolded keys land in the preamble, not after the job list
        assert!(
            content.find("permissions:").unwrap() < content.find("jobs:").unwrap(),
            "{}",
            content
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
        let (content, _) = merged(merge_file(Platform::GitHub, &file, &filter, with).unwrap());
        let doc: Value = serde_yaml::from_str(&content).unwrap();
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
        let (content, _) = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());
        assert!(content.contains("# named with care"), "{}", content);
        assert!(content.contains("# trigger notes live here"), "{}", content);
        assert!(content.contains("# keep this hack"), "{}", content);
        assert!(!content.contains("echo stale"), "{}", content);
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

        let (content, counts) = merged(merge_file(Platform::GitHub, &file, &filter, &existing).unwrap());
        assert_eq!(counts.disabled, 1, "{}", content);
        assert_eq!(counts.conformed, 0, "{}", content);
        assert!(content.contains("# rust-test:"), "{}", content);
        assert!(content.contains("cargo test"), "{}", content);
        assert!(!content.contains("--old-flag"), "{}", content);
        let doc: Value = serde_yaml::from_str(&content).unwrap();
        assert!(doc["jobs"]["rust-test"].is_null(), "{}", content);

        // Merging the output again is a no-op
        assert!(matches!(
            merge_file(Platform::GitHub, &file, &filter, &content).unwrap(),
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

        let (content, counts) = merged(merge_file(Platform::GitHub, &file, &filter, &existing).unwrap());
        assert_eq!(counts.removed, 1, "{}", content);
        assert_eq!(counts.disabled, 0, "{}", content);
        assert!(!content.contains("rust-fmt"), "{}", content);
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

        let (content, counts) = merged(merge_file(Platform::GitHub, &file, &filter, &existing).unwrap());
        assert_eq!(counts.disabled, file.jobs.len(), "{}", content);
        assert!(!content.contains("--old-flag"), "{}", content);
        let doc: Value = serde_yaml::from_str(&content).unwrap();
        assert!(doc["jobs"].is_null(), "{}", content);
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

        let (content, counts) = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());
        assert_eq!(counts.conformed, 1, "{}", content);
        // Unselected cibox jobs are left exactly as found, and missing ones
        // are not added
        assert!(content.contains("echo stale too"), "{}", content);
        assert_eq!(counts.added, 0, "{}", content);
        assert_eq!(counts.preserved, 2, "{}", content);
        assert!(!content.contains("rust-clippy"), "{}", content);
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

        let (content, counts) = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());
        // rust-fmt's rule is disabled, but it isn't selected, so it stays
        assert!(content.contains("rust-fmt"), "{}", content);
        assert_eq!(counts.removed, 0, "{}", content);
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

        let (content, counts) = merged(merge_file(Platform::GitLab, &file, &filter, existing).unwrap());

        assert!(content.contains("extra.yml"), "{}", content);
        assert!(content.contains(".hidden-template"), "{}", content);
        assert!(content.contains("MINE"), "{}", content);
        assert!(content.contains("my-job"), "{}", content);
        assert!(content.contains("cargo test"), "{}", content);
        assert!(!content.contains("echo stale"), "{}", content);
        // The custom stage survives because my-job references it
        assert!(content.contains("custom"), "{}", content);
        assert_eq!(counts.conformed, 1);
        assert_eq!(counts.added, file.jobs.len() - 1);
        assert_eq!((counts.removed, counts.preserved), (0, 1));
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
        let (content, _) = merged(merge_file(Platform::GitLab, &file, &filter, existing).unwrap());
        let doc: Value = serde_yaml::from_str(&content).unwrap();

        assert_eq!(
            doc["rust-release"]["rules"][0]["if"].as_str(),
            Some("$CI_COMMIT_TAG =~ /^v/")
        );
        assert!(doc["rust-release"]["only"].is_null(), "{}", content);
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
        let (content, _) = merged(merge_file(Platform::GitLab, &file, &filter, existing).unwrap());
        let doc: Value = serde_yaml::from_str(&content).unwrap();

        assert!(doc["variables"]["RUSTDOCFLAGS"].is_null(), "{}", content);
        assert!(doc["variables"]["CARGO_HOME"].is_null(), "{}", content);
        // A variable cibox doesn't set is the user's to keep
        assert_eq!(doc["variables"]["MINE"].as_str(), Some("1"));

        assert_eq!(
            doc["rust-doc"]["variables"]["RUSTDOCFLAGS"].as_str(),
            Some("--cfg docsrs")
        );
        assert_eq!(doc["rust-test"]["variables"]["CARGO_HOME"].as_str(), Some(".cargo"));
        assert!(doc["rust-test"]["variables"]["RUSTDOCFLAGS"].is_null(), "{}", content);
    }

    #[test]
    fn test_gitlab_update_drops_a_variables_block_left_empty() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitLab, &resolved, 0);
        let filter = no_selection(&resolved);

        let existing = "stages: [test]\nvariables:\n  CARGO_HOME: .cargo\n\
                        rust-test:\n  stage: test\n  script: [echo stale]\n";
        let (content, _) = merged(merge_file(Platform::GitLab, &file, &filter, existing).unwrap());
        assert!(!content.contains("variables:\n  CARGO_HOME"), "{}", content);
        let doc: Value = serde_yaml::from_str(&content).unwrap();
        assert!(doc.get("variables").is_none(), "{}", content);
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
        let (content, _) = merged(merge_file(Platform::GitLab, &file, &filter, existing).unwrap());
        assert!(
            content.starts_with("# Required CI variables: CARGO_REGISTRY_TOKEN\n"),
            "{}",
            content
        );
        // ...exactly once, even though the next merge starts from a file
        // that already carries it
        let (twice, _) = merged(
            merge_file(
                Platform::GitLab,
                &file,
                &filter,
                &content.replace("cargo publish", "echo stale"),
            )
            .unwrap(),
        );
        assert_eq!(twice.matches("Required CI variables").count(), 1);

        // No secret-bearing job left: the header goes away
        let mut config = CiboxConfig::default();
        config.rust_release.enabled = Some(false);
        let resolved = resolve(&facts, &config);
        let file = plan(&facts, &resolved, Platform::GitLab).unwrap().swap_remove(0);
        let filter = no_selection(&resolved);
        let (content, _) = merged(merge_file(Platform::GitLab, &file, &filter, &content).unwrap());
        assert!(!content.contains("Required CI variables"), "{}", content);

        // GitHub workflows name their secrets inline, so no header there
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitHub, &resolved, 1);
        let filter = no_selection(&resolved);
        let existing = "name: Release\non:\n  push:\n    tags: [v*]\njobs:\n  \
                        rust-release:\n    runs-on: ubuntu-latest\n    steps:\n      \
                        - run: echo stale\n";
        let (content, _) = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());
        assert!(!content.contains("Required CI variables"), "{}", content);
    }

    #[test]
    fn test_gitlab_commented_job_keeps_stage_and_sheds_secrets() {
        let facts = cargo_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = plan(&facts, &resolved, Platform::GitLab).unwrap().swap_remove(0);
        let filter = no_selection(&resolved);

        let canonical = render_file(Platform::GitLab, &file).unwrap();
        let existing = comment_out_job(&canonical, "rust-release");

        let (content, counts) = merged(merge_file(Platform::GitLab, &file, &filter, &existing).unwrap());
        assert_eq!(counts.disabled, 1, "{}", content);
        // A commented job doesn't run, so its secret isn't required...
        assert!(!content.contains("Required CI variables"), "{}", content);
        assert!(content.contains("# rust-release:"), "{}", content);
        // ...but its stage stays declared, so uncommenting just works
        let doc: Value = serde_yaml::from_str(&content).unwrap();
        let stages: Vec<&str> = doc["stages"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(stages.contains(&"deploy"), "{}", content);

        assert!(matches!(
            merge_file(Platform::GitLab, &file, &filter, &content).unwrap(),
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
        let (content, _) = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());
        assert!(content.contains("strategy:"), "{}", content);
        assert!(content.contains("fail-fast: false"), "{}", content);
        assert!(content.contains("rustlang/rust:nightly"), "{}", content);
        assert!(!content.contains("echo stale"), "{}", content);
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

        let (content, _) = merged(merge_file(Platform::CircleCI, &file, &filter, existing).unwrap());
        assert!(content.contains("rust-test-1.85"), "{}", content);
        assert!(content.contains("rust-test-nightly"), "{}", content);
        assert!(content.contains("my-job"), "{}", content);
        assert!(content.contains("<< parameters.image >>"), "{}", content);
        // Merging the merged output again is a no-op: the expanded
        // invocations map back onto the same canonical entries
        assert!(matches!(
            merge_file(Platform::CircleCI, &file, &filter, &content).unwrap(),
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

        let (content, _) = merged(merge_file(Platform::CircleCI, &file, &filter, existing).unwrap());
        assert!(!content.contains("rust-test-nightly"), "{}", content);
        assert!(!content.contains("parameters"), "{}", content);
        assert!(content.contains("rust:1.85"), "{}", content);
        // Exactly one rust-test invocation remains
        assert_eq!(content.matches("- rust-test").count(), 1, "{}", content);
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

        let (content, counts) = merged(merge_file(Platform::CircleCI, &file, &filter, existing).unwrap());

        assert!(!content.contains("rust-fmt"), "{}", content);
        assert!(content.contains("my-job"), "{}", content);
        assert!(content.contains("nightly"), "{}", content);
        // A workflow left with no jobs is dropped entirely
        assert!(!content.contains("stale-only"), "{}", content);
        assert!(content.contains("cargo test"), "{}", content);
        assert!(!content.contains("echo stale"), "{}", content);
        assert_eq!((counts.conformed, counts.removed, counts.preserved), (1, 1, 1));
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
        let (content, counts) = merged(merge_file(Platform::CircleCI, &file, &filter, &stale).unwrap());
        assert_eq!(counts.disabled, 1, "{}", content);
        assert!(!content.contains("--old-flag"), "{}", content);
        assert!(content.contains("# rust-test:"), "{}", content);
        assert!(content.contains("# - rust-test"), "{}", content);
        assert!(matches!(
            merge_file(Platform::CircleCI, &file, &filter, &content).unwrap(),
            MergeOutcome::Unchanged
        ));
    }
}

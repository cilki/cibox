//! Merge canonical cibox output into an existing CI file.
//!
//! Editing is done so that user comments, formatting, and key order survive:
//! a job's own lines are rewritten in the text, where nothing around them can
//! be disturbed, and everything else goes through a lossless `yaml-edit`
//! syntax tree. For every planned job the
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
use crate::platforms::github::lower::{tag_gate as gate_of_triggers, TagGate};
use anyhow::{anyhow, bail, Context};
use serde_yaml::Value;
use std::borrow::Cow;
use std::ops::Range;
use std::path::Path;
use std::str::FromStr;
use yaml_edit::{AsYaml, Document, Mapping, YamlFile, YamlNode};

/// One replacement the text phase makes: the byte span it covers in the
/// pipeline's text, and what goes there instead
type Edit = (Range<usize>, String);

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

/// Whether a merge has edited the syntax tree, and the only way it edits it.
///
/// Rendering the syntax tree back to text costs about as much as parsing it
/// did, and the tree is only ever a lossless parse of some text the merge
/// already holds — the original, or the text phase's output. So a merge that
/// never edited the tree finishes from that text instead of rendering, which
/// is the common case twice over: an up-to-date pipeline has nothing done to
/// it at all, and a stale one usually has every stale job rewritten in the
/// text, where the blank lines and comments around it survive.
///
/// Every tree edit goes through here rather than straight at the node, so
/// that "nothing was edited" cannot be a stale answer: a merge that grows a
/// new edit has no way to make it without saying so.
#[derive(Debug, Default, Clone, Copy)]
struct Changes(bool);

impl Changes {
    fn set(&mut self, map: &Mapping, key: impl AsYaml, value: impl AsYaml) {
        map.set(key, value);
        self.0 = true;
    }

    fn insert_before(
        &mut self,
        map: &Mapping,
        before_key: impl AsYaml,
        key: impl AsYaml,
        value: impl AsYaml,
    ) {
        map.insert_before(before_key, key, value);
        self.0 = true;
    }

    /// Removing a key the mapping doesn't have changes nothing, and must not
    /// count as an edit
    fn remove(&mut self, map: &Mapping, key: impl AsYaml) {
        if map.remove(key).is_some() {
            self.0 = true;
        }
    }

    /// The text to finish the merge with: the rendered tree when the tree
    /// was edited, otherwise `text` — the text that tree is the parse of.
    fn render(self, tree: &YamlFile, text: &str) -> String {
        if self.0 {
            tree.to_string()
        } else {
            text.to_string()
        }
    }
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
    // The scan reads the syntax tree and the text, and nothing of the value
    // tree, so it rides along with the value parse rather than waiting for it
    let (parse, scan) = Parse::of_with(existing, &file.path, |tree| {
        disabled::scan(platform, existing, tree, filter)
    })?;
    if !parse.value.is_mapping() {
        bail!(
            "{} is not a YAML mapping cibox can merge into; use --force to overwrite it",
            file.path.display()
        );
    }

    match platform {
        Platform::GitHub | Platform::Gitea => merge_github(file, filter, existing, &parse, scan),
        Platform::GitLab => merge_gitlab(file, filter, existing, &parse, scan),
        Platform::CircleCI => merge_circleci(file, filter, existing, &parse, scan),
    }
}

/// The two views of one pipeline's text a merge needs: the plain value tree
/// it measures jobs against, and the lossless syntax tree it edits.
///
/// Parsing YAML twice over is the bulk of a merge's work, so a parse is made
/// once per text and shared by every phase that reads it (see
/// [`Parse::after_edits`]).
struct Parse {
    tree: YamlFile,
    value: Value,
}

/// How big a pipeline has to be before a merge parses it on two threads.
///
/// The saving only grows past a thread's own cost once the file runs to a few
/// kilobytes — at 7.5 KB it is 0.4 ms of a 3.9 ms `update`, at 30 KB it is
/// 2.5 ms of 9.8 ms. Below that a whole merge is a fraction of a millisecond,
/// so the small pipelines that are the common case don't spawn anything.
const PARALLEL_PARSE_FROM: usize = 8 * 1024;

impl Parse {
    fn of(text: &str, path: &Path) -> Result<Self> {
        Ok(Self::of_with(text, path, |_| ())?.0)
    }

    /// Parse `text` into both views, running `alongside` on the syntax tree
    /// as soon as there is one — while the value tree is still being built.
    fn of_with<T>(
        text: &str,
        path: &Path,
        alongside: impl FnOnce(&YamlFile) -> T,
    ) -> Result<(Self, T)> {
        // The two parses read the same text and share nothing, and together
        // they are nearly the whole cost of a merge: of the 56 ms a 230 KB
        // GitLab pipeline took, 26 ms was serde_yaml and 29 ms was yaml-edit,
        // against 7 ms for everything the merge then did with them. So for a
        // pipeline big enough to pay for a thread they run side by side, and
        // the work that needs only the syntax tree — scanning for the
        // commented-out jobs — goes on this side of the pair too.
        //
        // The value tree is the one that travels, because a `YamlFile` holds
        // rowan's mutable red tree and is not `Send`.
        if text.len() < PARALLEL_PARSE_FROM {
            let value = serde_yaml::from_str(text);
            return Self::finish(value, YamlFile::from_str(text), path, alongside);
        }
        std::thread::scope(|scope| {
            let value = scope.spawn(|| serde_yaml::from_str(text));
            let tree = YamlFile::from_str(text);
            let extra = tree.as_ref().ok().map(alongside);
            // A parser panic is cibox's bug to report, not a YAML error, so
            // it keeps its own backtrace instead of becoming a message
            let value = value
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
            let parse = Self::check(value, tree, path)?;
            Ok((
                parse,
                extra.expect("the tree parsed, or `check` would have failed"),
            ))
        })
    }

    /// [`Self::check`] with `alongside` run on the tree it accepted
    fn finish<T>(
        value: std::result::Result<Value, serde_yaml::Error>,
        tree: std::result::Result<YamlFile, yaml_edit::YamlError>,
        path: &Path,
        alongside: impl FnOnce(&YamlFile) -> T,
    ) -> Result<(Self, T)> {
        let parse = Self::check(value, tree, path)?;
        let extra = alongside(&parse.tree);
        Ok((parse, extra))
    }

    /// Report whichever parse rejected the text, preferring serde_yaml's
    /// error: it reads the stricter of the two grammars, so when both fail
    /// its message is the one about the real mistake.
    fn check(
        value: std::result::Result<Value, serde_yaml::Error>,
        tree: std::result::Result<YamlFile, yaml_edit::YamlError>,
        path: &Path,
    ) -> Result<Self> {
        let value = value.with_context(|| {
            format!(
                "{} is not valid YAML; use --force to overwrite it",
                path.display()
            )
        })?;
        let tree = tree.map_err(|e| {
            anyhow!(
                "{} is not valid YAML; use --force to overwrite it: {e}",
                path.display()
            )
        })?;
        Ok(Self { tree, value })
    }

    /// The parse the tree phase has to work on once the text phase has run:
    /// `None` to keep using the parse of the original text, which came
    /// through untouched, or a fresh parse of the rewritten text.
    ///
    /// Only asked for when the tree phase has work (see [`JobOps`]); a
    /// reparse costs as much as the original parse did, and rendering its
    /// tree back costs about as much again.
    fn after_edits(edited: Option<&str>, path: &Path) -> Result<Option<Self>> {
        edited
            .map(|text| {
                // Failing here is cibox's fault, not the file's: everything
                // the text phase wrote is canonical output it rendered itself
                Parse::of(text, path).context("the text phase's own output reparses")
            })
            .transpose()
    }

    /// The document's top-level mapping, as plain values
    fn values(&self) -> Result<&serde_yaml::Mapping> {
        self.value.as_mapping().context("document is a mapping")
    }

    /// The document's top-level mapping, as an editable syntax node
    fn root(&self) -> Result<Mapping> {
        self.tree
            .document()
            .and_then(|doc| doc.as_mapping())
            .context("document is a mapping")
    }
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

/// Where a platform keeps its jobs in the document.
#[derive(Debug, Clone, Copy)]
enum JobsAt {
    /// Under a top-level `jobs:` mapping (GitHub/Gitea, CircleCI)
    JobsKey,
    /// At the document root, mixed with configuration keys (GitLab)
    Root,
}

/// A platform's job region resolved against one parse: the three views of it
/// every phase of a merge needs. The region is resolved twice per merge —
/// once on the original text for the text phase, once on the result for the
/// tree phase — which is the whole of what a platform has to say about where
/// its jobs live.
struct JobsRegion<'a> {
    /// The region's keys that are jobs, in document order
    keys: Vec<String>,
    /// The mapping holding them, as plain values to measure against
    values: Cow<'a, serde_yaml::Mapping>,
    /// The editable node, absent when the region has no live mapping (every
    /// job commented out, or `jobs:` left empty)
    node: Option<Mapping>,
}

impl JobsAt {
    /// Read the job region out of `parse`. An empty region is legitimate —
    /// that is what every job being commented out or deleted looks like —
    /// but a `jobs:`-key file without the key at all is not a pipeline cibox
    /// can merge into.
    fn resolve<'a>(self, parse: &'a Parse, path: &Path) -> Result<JobsRegion<'a>> {
        let root_values = parse.values()?;
        let root = parse.root()?;
        Ok(match self {
            JobsAt::Root => JobsRegion {
                keys: gitlab_job_keys(root_values),
                values: Cow::Borrowed(root_values),
                node: Some(root),
            },
            JobsAt::JobsKey => {
                let values = match get(root_values, "jobs") {
                    Some(Value::Mapping(m)) => Cow::Borrowed(m),
                    Some(Value::Null) => Cow::Owned(serde_yaml::Mapping::new()),
                    _ => bail!(
                        "{} has no `jobs:` mapping; use --force to overwrite it",
                        path.display()
                    ),
                };
                JobsRegion {
                    keys: string_keys(&values),
                    values,
                    node: root.get_mapping("jobs"),
                }
            }
        })
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

/// Canonical YAML text for one `key: value` mapping entry at column 0 — a
/// job under its id, or a top-level key under its own.
fn render_entry(key: &str, value: &Value) -> Result<String> {
    let mut map = serde_yaml::Mapping::new();
    map.insert(Value::from(key), value.clone());
    Ok(serde_yaml::to_string(&Value::Mapping(map))?)
}

/// [`render_entry`] parsed back as a value node, ready to splice into the
/// user's tree. The document is returned alongside only to keep its tree
/// alive while the node is used.
fn fragment(key: &str, value: &Value) -> Result<(Document, YamlNode)> {
    let doc = Document::from_str(&render_entry(key, value)?)
        .map_err(|e| anyhow!("canonical output for `{key}` did not reparse: {e}"))?;
    let node = doc
        .as_mapping()
        .and_then(|m| m.get(key))
        .with_context(|| format!("canonical output for `{key}` did not round-trip"))?;
    Ok((doc, node))
}

/// What the phases measure each job against: the canonical output, the
/// ids to add, and the filter deciding what cibox may touch at all
struct JobsPlan<'a> {
    canonical: &'a serde_yaml::Mapping,
    adds: &'a [String],
    filter: &'a JobFilter<'a>,
    path: &'a Path,
}

/// The work on the job region the text phase left for the tree: the stale
/// managed jobs whose body the text can't address, the managed jobs that are
/// no longer generated, and the missing ones the text had nowhere to append.
///
/// It is decided by the text phase, against the parse of the original text,
/// rather than read off a parse of that phase's output — the two agree,
/// because the text phase only ever rewrites a job's own body lines or a
/// comment block, and every job it rewrites it brings to canonical output.
/// Deciding it up front is what lets a merge the tree has no work in skip
/// both reparsing the new text and rendering a tree back to it.
#[derive(Debug, Default)]
struct JobOps {
    /// Stale managed jobs to replace with canonical output
    conform: Vec<String>,
    /// Managed jobs no rule generates any more
    remove: Vec<String>,
    /// Managed jobs to splice in
    add: Vec<String>,
}

impl JobOps {
    fn is_empty(&self) -> bool {
        self.conform.is_empty() && self.remove.is_empty() && self.add.is_empty()
    }
}

/// Carry out [`JobOps`] on the lossless syntax tree of the text phase's
/// output. The caller may keep editing the tree before rendering it.
///
/// Its tree edits go through `changes`, so a merge that only ever edited the
/// text can skip rendering the tree (see [`Changes`]).
fn apply_job_ops(
    region: &JobsRegion,
    ops: &JobOps,
    plan: &JobsPlan,
    changes: &mut Changes,
) -> Result<()> {
    let Some(jobs) = &region.node else {
        return Ok(());
    };
    if jobs.is_flow_style() {
        bail!(
            "{} keeps its jobs in flow-style YAML, which cibox cannot edit in place; \
             use --force to overwrite it",
            plan.path.display()
        );
    }
    for id in ops.conform.iter().chain(&ops.add) {
        let canonical = get(plan.canonical, id).context("canonical output for a planned job")?;
        let (_doc, node) = fragment(id, canonical)?;
        changes.set(jobs, id.as_str(), node);
    }
    for id in &ops.remove {
        changes.remove(jobs, id.as_str());
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
) -> Result<Vec<Edit>> {
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
                &render_entry(id, canonical)?,
                block.indent,
            ));
        }
        edits.push((block.span.clone(), replacement));
    }
    Ok(edits)
}

/// Whole-line byte span of one job's body — the lines below its key — plus
/// the column that body sits at. `None` unless the value is a node that
/// starts a line of its own, which is every block-style job and nothing
/// else: a `job: {…}` one-liner, a scalar, or an empty value has no body
/// lines to replace and goes back to the tree phase.
fn body_span(jobs: &Mapping, text: &str, key: &str) -> Option<(Range<usize>, usize)> {
    let node = jobs.get(key)?;
    let range = node.as_node()?.text_range();
    let (start, end) = (usize::from(range.start()), usize::from(range.end()));
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    if !text[line_start..start].chars().all(char::is_whitespace) {
        return None;
    }
    // Blank lines after the body, and the indentation of a comment line
    // below it, are inside the value node — the whole point of going through
    // the text is to leave them alone, so the span stops at the last line
    // that actually holds content.
    let content_end = text[..end].trim_end().len();
    if content_end <= start {
        return None;
    }
    let body_end = text[content_end..]
        .find('\n')
        .map_or(text.len(), |i| content_end + i + 1);
    Some((line_start..body_end, start - line_start))
}

/// Decide the fate of every job in the region: a text replacement where the
/// job's body is a block the text can address, an entry in [`JobOps`] for
/// the tree phase otherwise. Also counts the jobs cibox leaves alone.
///
/// Conforming a job by replacing its value on the syntax tree loses the
/// trivia the parser folded into that value: a job followed by a blank line
/// and a comment comes back with the blank line gone and the comment
/// un-indented (yaml-edit 0.3). Rewriting the body's own lines instead
/// leaves everything around it — the blank lines, the comments, and the key
/// line with whatever the user put on it — byte for byte. Removing a job,
/// and conforming one whose body is not a block of its own (a `job: {…}`
/// one-liner, say), is the tree's work; so is the whole region when the user
/// wrote it in flow style, which the tree phase then refuses outright.
fn conform_jobs(
    region: &JobsRegion,
    text: &str,
    plan: &JobsPlan,
    counts: &mut Counts,
    edits: &mut Vec<Edit>,
    ops: &mut JobOps,
) -> Result<()> {
    // A flow-style region has no body lines to rewrite, so all of it falls
    // to the tree phase
    let addressable = region.node.as_ref().filter(|jobs| !jobs.is_flow_style());
    for key in &region.keys {
        if !plan.filter.managed(key) {
            counts.preserved += 1;
            continue;
        }
        let Some(canonical) = get(plan.canonical, key) else {
            ops.remove.push(key.clone());
            counts.removed += 1;
            continue;
        };
        if get(&region.values, key) == Some(canonical) {
            continue;
        }
        match addressable.and_then(|jobs| body_span(jobs, text, key)) {
            Some((span, indent)) => {
                edits.push((span, indent_by(&serde_yaml::to_string(canonical)?, indent)))
            }
            None => ops.conform.push(key.clone()),
        }
        counts.conformed += 1;
    }
    Ok(())
}

/// Apply the text phase's replacements. `None` when there were none, which
/// is what tells the tree phase its parse of the original text is still good
/// (see [`Parse::after_edits`]).
fn apply_edits(text: &str, mut edits: Vec<Edit>) -> Option<String> {
    if edits.is_empty() {
        return None;
    }
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut out = text.to_string();
    for (range, replacement) in edits {
        out.replace_range(range, &replacement);
    }
    Some(out)
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

/// Where a new job's lines go, and the column its key sits at: just past the
/// last line the job region holds — the last job's body, or the comments
/// below it — at the column the region's keys already use. Without a live
/// mapping to measure (every job commented out, or `jobs:` left empty) it is
/// the line below `jobs:`, at the canonical column. `None` leaves the adding
/// to the tree phase, which rejects flow-style job mappings outright.
fn jobs_append_at(text: &str, jobs: Option<&Mapping>) -> Option<(usize, usize)> {
    let Some(jobs) = jobs else {
        return jobs_line_end(text).map(|at| (at, 2));
    };
    if jobs.is_flow_style() {
        return None;
    }
    let range = jobs.as_node()?.text_range();
    let (start, end) = (usize::from(range.start()), usize::from(range.end()));
    let span = disabled::line_span(text, &(start..end));
    // Trailing blank lines belong to nothing in particular; a job appended
    // after them would look detached from the region it joins.
    let content_end = text[..span.end].trim_end().len();
    let at = text[content_end..]
        .find('\n')
        .map_or(text.len(), |i| content_end + i + 1);
    Some((at, start - text[..start].rfind('\n').map_or(0, |i| i + 1)))
}

/// Queue the lines of the jobs that have to be re-added, or leave them to
/// the tree phase when the region has nowhere to append to.
///
/// Splicing them into the syntax tree instead would, when the region ends in
/// a blank line and a comment, pull the comment's indentation out from under
/// it (yaml-edit 0.3) — the same reason [`conform_jobs`] rewrites a body's
/// own lines.
fn add_jobs(
    existing: &str,
    jobs: Option<&Mapping>,
    plan: &JobsPlan,
    counts: &mut Counts,
    edits: &mut Vec<Edit>,
    ops: &mut JobOps,
) -> Result<()> {
    let append_at = jobs_append_at(existing, jobs);
    let mut rendered = String::new();
    for id in plan.adds {
        let Some(canonical) = get(plan.canonical, id) else {
            continue;
        };
        match append_at {
            Some((_, indent)) => {
                let entry = render_entry(id, canonical)?;
                rendered.push_str(&indent_by(&entry, indent));
            }
            None => ops.add.push(id.clone()),
        }
        counts.added += 1;
    }
    if let (Some((offset, _)), false) = (append_at, rendered.is_empty()) {
        edits.push((offset..offset, rendered));
    }
    Ok(())
}

/// The replacements the text phase queued (a platform may add its own), and
/// the work the text can't do for [`apply_job_ops`] to do on the tree
struct TextPhase {
    edits: Vec<Edit>,
    ops: JobOps,
}

/// The text phase of a merge, which is the same on every platform: the
/// commented blocks live outside the YAML value tree, and conforming or
/// adding a live job through the text is what keeps the blank lines and
/// comments around it intact.
fn text_phase(
    region: &JobsRegion,
    existing: &str,
    blocks: &[DisabledBlock],
    plan: &JobsPlan,
    counts: &mut Counts,
) -> Result<TextPhase> {
    let mut edits = block_edits(blocks, plan.canonical, counts)?;
    let mut ops = JobOps::default();
    conform_jobs(region, existing, plan, counts, &mut edits, &mut ops)?;
    add_jobs(
        existing,
        region.node.as_ref(),
        plan,
        counts,
        &mut edits,
        &mut ops,
    )?;
    Ok(TextPhase { edits, ops })
}

impl TextPhase {
    /// Apply the replacements, then run the tree phase: [`JobOps`] on the
    /// job region `at` names, then the platform's own `edit_root`, which
    /// `root_work` says has anything to do. With no tree work at all, the
    /// text the text phase produced is the answer already.
    fn finish(
        self,
        existing: &str,
        parse: &Parse,
        at: JobsAt,
        plan: &JobsPlan,
        root_work: bool,
        edit_root: impl FnOnce(&Mapping, &mut Changes) -> Result<()>,
    ) -> Result<String> {
        let edited = apply_edits(existing, self.edits);
        let text = edited.as_deref().unwrap_or(existing);
        if self.ops.is_empty() && !root_work {
            return Ok(text.to_string());
        }
        let reparsed = Parse::after_edits(edited.as_deref(), plan.path)?;
        let parse = reparsed.as_ref().unwrap_or(parse);
        let mut changes = Changes::default();
        apply_job_ops(
            &at.resolve(parse, plan.path)?,
            &self.ops,
            plan,
            &mut changes,
        )?;
        edit_root(&parse.root()?, &mut changes)?;
        Ok(changes.render(&parse.tree, text))
    }
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

/// What gates the tags-only jobs of the workflow being merged into.
///
/// Rendering a release workflow whole, `on: push: tags: [v*]` is the gate and
/// the jobs carry no `if:`. A merge inherits the triggers of the file on disk
/// instead — they are the user's, and `update` keeps them — so a release
/// workflow that was hand-written, adopted from another tool, or edited since
/// may well run on branch pushes or pull requests. The jobs in it hold the
/// publish credentials (`cargo publish`, `docker push`, the AUR key), so when
/// the file's own triggers don't gate them, each one gets the same `if:` it
/// would get inside `ci.yml`.
///
/// A file with no triggers at all is about to be given cibox's own, below.
fn tag_gate(file: &PlannedFile, current: &serde_yaml::Mapping) -> TagGate {
    match (TagGate::of(file.kind), get(current, "on")) {
        (TagGate::Workflow, Some(on)) => gate_of_triggers(on),
        (gate, _) => gate,
    }
}

fn merge_github(
    file: &PlannedFile,
    filter: &JobFilter,
    existing: &str,
    parse: &Parse,
    scan: Scan,
) -> Result<MergeOutcome> {
    let mut counts = Counts::default();

    let region = JobsAt::JobsKey.resolve(parse, &file.path)?;
    let split = split_plan(&file.jobs, &region.keys, scan.blocks, filter);

    let canonical = serde_yaml::to_value(crate::platforms::github::lower::lower_github_gated(
        &split.kept(),
        file.kind,
        tag_gate(file, parse.values()?),
    ))?;
    let canonical_jobs = canonical_job_mapping(&canonical);
    let plan = JobsPlan {
        canonical: &canonical_jobs,
        adds: &split.adds,
        filter,
        path: &file.path,
    };

    let phase = text_phase(&region, existing, &split.blocks, &plan, &mut counts)?;

    // Only `jobs` is managed; user-tuned triggers/env/permissions are kept.
    // Scaffold `name`/`on`/`permissions` solely when the workflow lacks them.
    // The text phase never writes a document key, so what is missing from
    // this parse is missing from its output too.
    let current_map = parse.values()?;
    let scaffold: Vec<(&str, &Value)> = ["name", "on", "permissions"]
        .into_iter()
        .filter(|key| get(current_map, key).is_none())
        .filter_map(|key| canonical.get(key).map(|value| (key, value)))
        .collect();

    let content = phase.finish(
        existing,
        parse,
        JobsAt::JobsKey,
        &plan,
        !scaffold.is_empty(),
        |root, changes| {
            for (key, value) in scaffold {
                let (_doc, node) = fragment(key, value)?;
                changes.insert_before(root, "jobs", key, node);
            }
            Ok(())
        },
    )?;
    if content == existing {
        return Ok(MergeOutcome::Unchanged);
    }
    let live_jobs = region
        .keys
        .iter()
        .filter(|k| !filter.managed(k) || get(&canonical_jobs, k).is_some())
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

/// What a GitLab merge does to the pipeline's `stages:` list
enum StagesEdit {
    /// Leave it as the user wrote it
    Leave,
    Remove,
    Set(Vec<Value>),
}

fn merge_gitlab(
    file: &PlannedFile,
    filter: &JobFilter,
    existing: &str,
    parse: &Parse,
    scan: Scan,
) -> Result<MergeOutcome> {
    let mut counts = Counts::default();
    let region = JobsAt::Root.resolve(parse, &file.path)?;
    let split = split_plan(&file.jobs, &region.keys, scan.blocks, filter);

    let canonical =
        serde_yaml::to_value(crate::platforms::gitlab::lower::lower_gitlab(&split.kept()))?;
    let canonical_map = canonical
        .as_mapping()
        .expect("GitLabCI serializes to a mapping");

    let plan = JobsPlan {
        canonical: canonical_map,
        adds: &split.adds,
        filter,
        path: &file.path,
    };
    let phase = text_phase(&region, existing, &split.blocks, &plan, &mut counts)?;

    // The text phase rewrites job bodies and comment blocks and nothing
    // else, so the document keys below read the same off this parse as off a
    // parse of its output — and deciding them here is what lets a merge with
    // no tree edits to make skip parsing that output at all.
    let current_map = parse.values()?;

    // Conform `stages` to what the final jobs reference. Canonical stages
    // cover the disabled jobs too, so a commented job's stage stays and
    // uncommenting it works without another update.
    //
    // Every root entry that isn't configuration counts, hidden
    // `.`-prefixed templates included: a job reaches a template's `stage`
    // through `extends` or a merge key without naming it itself, and GitLab
    // rejects the whole pipeline ("chosen stage does not exist") when a job
    // lands in a stage the final `stages:` doesn't declare.
    let mut final_jobs: Vec<&Value> = Vec::new();
    for (key, value) in current_map {
        let Some(key) = key.as_str() else { continue };
        if GITLAB_RESERVED.contains(&key) {
            continue;
        }
        match filter.managed(key) {
            true => final_jobs.extend(get(canonical_map, key)),
            false => final_jobs.push(value),
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
    // An `include:`d file contributes jobs this document never shows, so
    // there is no telling which of the declared stages they land in. Keep
    // the list as it is and only append what cibox's own jobs need.
    let prune = get(current_map, "include").is_none();
    let mut stages: Vec<Value> = existing_stages
        .iter()
        .filter(|s| !prune || s.as_str().is_some_and(|s| referenced.contains(&s)))
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
    let stages = match &stages[..] {
        // An empty list is no list: GitLab rejects `stages: []`
        [] => match get(current_map, "stages").is_some() {
            true => StagesEdit::Remove,
            false => StagesEdit::Leave,
        },
        _ if stages == existing_stages => StagesEdit::Leave,
        _ => StagesEdit::Set(stages),
    };

    // cibox used to hoist every job's environment into the top-level
    // `variables:`, from where it applied to all the other jobs too. Those
    // keys now live on the job that wants them, so drop the global copies —
    // left behind they would keep leaking. Variables cibox never sets are the
    // user's (they may feed custom jobs) and stay.
    let hoisted = gitlab_job_variable_names(&file.jobs);
    let global_vars: Option<Vec<&Value>> = match get(current_map, "variables") {
        Some(Value::Mapping(vars)) => Some(vars.keys().collect()),
        _ => None,
    };
    let drop_vars: Vec<&str> = global_vars
        .iter()
        .flatten()
        .filter(|key| hoisted.contains(key))
        .filter_map(|key| key.as_str())
        .collect();
    // Nothing of the user's is left in there, so the mapping goes too
    let drop_vars_key = global_vars
        .as_ref()
        .is_some_and(|keys| keys.iter().all(|key| hoisted.contains(key)));

    let root_work = !matches!(stages, StagesEdit::Leave) || !drop_vars.is_empty() || drop_vars_key;
    let mut content = phase.finish(
        existing,
        parse,
        JobsAt::Root,
        &plan,
        root_work,
        |root, changes| {
            match stages {
                StagesEdit::Leave => {}
                StagesEdit::Remove => changes.remove(root, "stages"),
                StagesEdit::Set(stages) => {
                    let (_doc, node) = fragment("stages", &Value::Sequence(stages))?;
                    changes.set(root, "stages", node);
                }
            }
            if let Some(vars_node) = root.get_mapping("variables") {
                for key in drop_vars {
                    changes.remove(&vars_node, key);
                }
                if drop_vars_key {
                    changes.remove(root, "variables");
                }
            }
            Ok(())
        },
    )?;
    let header = header_jobs(&file.jobs, filter, &split.disabled_ids, &region.keys);
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
    parse: &Parse,
    scan: Scan,
) -> Result<MergeOutcome> {
    let mut counts = Counts::default();

    let original = parse.values()?;
    let region = JobsAt::JobsKey.resolve(parse, &file.path)?;
    let split = split_plan(&file.jobs, &region.keys, scan.blocks, filter);

    let canonical = serde_yaml::to_value(crate::platforms::circleci::lower::lower_circleci(
        &split.kept(),
    ))?;
    let canonical_jobs = canonical_job_mapping(&canonical);
    let canonical_entries: Vec<Value> = canonical
        .get("workflows")
        .and_then(|w| w.get("main"))
        .and_then(|m| m.get("jobs"))
        .and_then(Value::as_sequence)
        .cloned()
        .unwrap_or_default();

    let plan = JobsPlan {
        canonical: &canonical_jobs,
        adds: &split.adds,
        filter,
        path: &file.path,
    };
    let mut phase = text_phase(&region, existing, &split.blocks, &plan, &mut counts)?;
    let edits = &mut phase.edits;

    // Commented workflow entries are text-only too: kept in sync with the
    // job's canonical invocations while it stays disabled, dropped when the
    // job is no longer generated at all
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
    // What `main` should end up invoking, when it has a list at all
    let mut main_desired: Option<Vec<Value>> = None;
    if let Some(Value::Mapping(workflows)) = get(original, "workflows") {
        for (name, workflow) in workflows {
            let Some(name) = name.as_str() else { continue };
            let Some(entries) = workflow.get("jobs").and_then(Value::as_sequence) else {
                if name == "main" {
                    let spans = workflow_spans(existing, &parse.tree, name);
                    main_append_at = spans
                        .jobs_line_end
                        .map(|at| (at, line_indent(existing, at - 1)));
                }
                continue;
            };
            let (desired, ops, seen) =
                conform_entries(entries, &canonical_entries, &split.disabled_ids, filter);
            invoked.extend(seen);
            let spans = workflow_spans(existing, &parse.tree, name);
            // Editing the list item by item needs the parse's items to line
            // up with the entries, and a flow-style list has no lines of its
            // own to edit
            let addressable = !spans.flow && spans.items.len() == entries.len();
            if name == "main" {
                // A block entry appended below a flow-style list would not be
                // YAML at all, so a list the text can't address item by item
                // can't be appended to either — the whole list is rewritten
                // on the tree instead, below
                main_append_at = addressable
                    .then(|| {
                        spans
                            .items
                            .last()
                            .map(|span| (span.end, line_indent(existing, span.start)))
                    })
                    .flatten();
                main_desired = Some(desired.clone());
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
            if !addressable {
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
        } else if let Some(mut desired) = main_desired {
            // `main` has a list, but not one the text can append to: rewrite
            // it whole, with the missing invocations in it
            desired.extend(missing_entries.iter().cloned());
            wholesale.push(("main".to_string(), desired));
        } else {
            build_main = true;
        }
    }

    let root_work = !drop_workflows.is_empty() || !wholesale.is_empty() || build_main;
    let mut content = phase.finish(
        existing,
        parse,
        JobsAt::JobsKey,
        &plan,
        root_work,
        |root, changes| {
            if let Some(workflows_node) = root.get_mapping("workflows") {
                for name in &drop_workflows {
                    changes.remove(&workflows_node, name.as_str());
                }
                for (name, desired) in &wholesale {
                    if let Some(workflow) = workflows_node.get_mapping(name) {
                        let (_doc, seq) = sequence_fragment(desired)?;
                        changes.set(&workflow, "jobs", seq);
                    }
                }
            }
            if build_main {
                let mut main = serde_yaml::Mapping::new();
                main.insert(Value::from("jobs"), Value::Sequence(missing_entries));
                match root.get_mapping("workflows") {
                    Some(workflows_node) => {
                        let (_doc, node) = fragment("main", &Value::Mapping(main))?;
                        changes.set(&workflows_node, "main", node);
                    }
                    None => {
                        let mut workflows = serde_yaml::Mapping::new();
                        workflows.insert(Value::from("main"), Value::Mapping(main));
                        let (_doc, node) = fragment("workflows", &Value::Mapping(workflows))?;
                        changes.set(root, "workflows", node);
                    }
                }
            }
            Ok(())
        },
    )?;
    let header = header_jobs(&file.jobs, filter, &split.disabled_ids, &region.keys);
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

    /// Whether a merge's output gates `id` on a version tag, either on the
    /// job or through the workflow's own triggers
    fn gated_on_tags(content: &str, id: &str) -> (bool, bool) {
        let doc: Value = serde_yaml::from_str(content).unwrap();
        let job = doc["jobs"][id]
            .as_mapping()
            .unwrap_or_else(|| panic!("{id} missing from {content}"))
            .clone();
        let by_job = get(&job, "if").and_then(Value::as_str)
            == Some("startsWith(github.ref, 'refs/tags/v')");
        (by_job, gate_of_triggers(&doc["on"]) == TagGate::Workflow)
    }

    /// A release workflow's triggers are what keeps `cargo publish` and
    /// `docker push` off branch pushes, and a merge keeps the triggers the
    /// file came with — so a file whose triggers don't gate them has to have
    /// the jobs gated instead.
    #[test]
    fn test_github_release_merge_gates_jobs_the_triggers_do_not() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitHub, &resolved, 1);
        assert!(file.jobs.iter().all(|j| j.tags_only), "fixture rot");
        let filter = no_selection(&resolved);

        // A release.yml that was not cibox's: it builds on pull requests and
        // pushes to main, and holds a stale copy of a managed job
        let existing = r#"name: Release
on:
  pull_request:
  push:
    branches: [main]
jobs:
  rust-release:
    runs-on: ubuntu-latest
    steps:
      - run: echo stale
"#;

        let (content, _) = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());
        // The user's triggers survive, so every managed job carries the gate
        assert!(content.contains("pull_request"), "{content}");
        for job in &file.jobs {
            assert_eq!(
                gated_on_tags(&content, &job.id),
                (true, false),
                "{} is not gated in {content}",
                job.id
            );
        }
    }

    #[test]
    fn test_github_release_merge_leaves_a_tag_gated_file_alone() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitHub, &resolved, 1);
        let filter = no_selection(&resolved);

        // cibox's own output: the workflow is the gate, so adding an `if:`
        // to every job would be noise — and would rewrite the file on an
        // `update` that has nothing to do
        let canonical = render_file(Platform::GitHub, &file).unwrap();
        assert!(matches!(
            merge_file(Platform::GitHub, &file, &filter, &canonical).unwrap(),
            MergeOutcome::Unchanged
        ));
        for job in &file.jobs {
            assert_eq!(
                gated_on_tags(&canonical, &job.id),
                (false, true),
                "{}",
                job.id
            );
        }

        // A hand-written file that gates just as well keeps the same treatment
        let existing = r#"name: Publish
on:
  push:
    tags: [v*]
jobs:
  rust-release:
    runs-on: ubuntu-latest
    steps:
      - run: echo stale
"#;
        let (content, _) = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());
        assert_eq!(
            gated_on_tags(&content, "rust-release"),
            (false, true),
            "{content}"
        );
    }

    /// A release workflow with no triggers at all is given cibox's own, which
    /// gate it — the jobs need nothing
    #[test]
    fn test_github_release_merge_without_triggers_scaffolds_the_tag_trigger() {
        let facts = cargo_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = plan(&facts, &resolved, Platform::GitHub)
            .unwrap()
            .swap_remove(1);
        let filter = no_selection(&resolved);

        let existing = r#"jobs:
  rust-release:
    runs-on: ubuntu-latest
    steps:
      - run: echo stale
"#;
        let (content, _) = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());
        assert_eq!(
            gated_on_tags(&content, "rust-release"),
            (false, true),
            "{content}"
        );
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

    /// Conforming a job used to be a tree edit, which dropped the blank
    /// lines and comment indentation the parser folds into the value it
    /// replaced: everything between the job and the next one came back
    /// crushed against it and flush with the left margin.
    #[test]
    fn test_conform_leaves_what_follows_the_job_alone() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitHub, &resolved, 0);
        let filter = no_selection(&resolved);

        let existing = r#"name: CI
on:
  push:
    branches: [main]
jobs:
  rust-test:   # the one I care about
    runs-on: ubuntu-latest
    steps:
      - run: echo stale

  # a job I wrote myself
  my-custom:
    runs-on: ubuntu-latest
    steps:
      - run: echo mine
"#;
        let (content, counts) = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());
        assert_eq!(counts.conformed, 1, "{}", content);
        assert!(!content.contains("echo stale"), "{}", content);
        assert!(
            content.contains("  rust-test:   # the one I care about\n"),
            "{}",
            content
        );
        assert!(
            content.contains("\n\n  # a job I wrote myself\n  my-custom:\n"),
            "{}",
            content
        );
    }

    /// As above, where the commented block following the job is a disabled
    /// managed job: it has to stay indented under `jobs:`, or uncommenting
    /// it puts the job at the top level
    #[test]
    fn test_conform_leaves_a_trailing_disabled_block_indented() {
        let facts = full_facts();
        let mut config = CiboxConfig::default();
        config.rust_clippy.enabled = Some(false);
        let resolved = resolve(&facts, &config);
        let file = planned_file(Platform::GitHub, &resolved, 0);
        let filter = no_selection(&resolved);

        let existing = r#"name: CI
on:
  push:
    branches: [main]
jobs:
  rust-test:
    runs-on: ubuntu-latest
    steps:
      - run: echo stale

  # rust-fmt:
  #   runs-on: ubuntu-latest
  #   steps:
  #   - run: cargo fmt -- --check
"#;
        let (content, counts) = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());
        assert_eq!((counts.conformed, counts.disabled), (1, 1), "{}", content);
        assert!(content.contains("\n  # rust-fmt:\n"), "{}", content);
        assert!(!content.contains("\n# rust-fmt:\n"), "{}", content);
    }

    #[test]
    fn test_gitlab_conform_leaves_what_follows_the_job_alone() {
        let facts = cargo_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = plan(&facts, &resolved, Platform::GitLab)
            .unwrap()
            .swap_remove(0);
        let filter = no_selection(&resolved);

        let existing = r#"stages:
- test
- lint

rust-test:
  stage: test
  script:
  - echo stale

# my own job, hands off
my-job:
  stage: lint
  script:
  - echo mine
"#;
        let (content, counts) = merged(merge_file(Platform::GitLab, &file, &filter, existing).unwrap());
        assert_eq!(counts.conformed, 1, "{}", content);
        assert!(!content.contains("echo stale"), "{}", content);
        assert!(
            content.contains("\n\n# my own job, hands off\nmy-job:\n"),
            "{}",
            content
        );
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

    /// A pipeline of `jobs` user jobs, each with a comment above it, plus one
    /// stale `rust-test`. Big enough to cross [`PARALLEL_PARSE_FROM`] at a
    /// few hundred jobs, which is what puts a merge on the two-thread path.
    fn padded_gitlab_pipeline(jobs: usize) -> String {
        let mut text = String::from("stages: [test]\nrust-test:\n  stage: test\n  script: [echo stale]\n");
        for i in 0..jobs {
            text.push_str(&format!(
                "# what mine-{i} is for\nmine-{i}:\n  stage: test\n  script:\n    - echo mine {i}\n"
            ));
        }
        text
    }

    /// A pipeline parsed on two threads has to come out of a merge exactly as
    /// one parsed on a single thread would: same conforming, same preserving,
    /// every user job and comment still there.
    #[test]
    fn test_gitlab_merge_of_a_pipeline_big_enough_to_parse_in_parallel() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitLab, &resolved, 0);
        let filter = no_selection(&resolved);

        let small = padded_gitlab_pipeline(4);
        let big = padded_gitlab_pipeline(300);
        assert!(small.len() < PARALLEL_PARSE_FROM, "fixture rot: {}", small.len());
        assert!(big.len() >= PARALLEL_PARSE_FROM, "fixture rot: {}", big.len());

        for (label, existing, users) in [("small", &small, 4), ("big", &big, 300)] {
            let (content, counts) =
                merged(merge_file(Platform::GitLab, &file, &filter, existing).unwrap());
            assert!(content.contains("cargo test"), "{label}");
            assert!(!content.contains("echo stale"), "{label}");
            for i in 0..users {
                assert!(content.contains(&format!("echo mine {i}")), "{label} job {i}");
                assert!(content.contains(&format!("what mine-{i} is for")), "{label} comment {i}");
            }
            assert_eq!(counts.conformed, 1, "{label}");
            assert_eq!(counts.added, file.jobs.len() - 1, "{label}");
            assert_eq!((counts.removed, counts.disabled, counts.preserved), (0, 0, users), "{label}");
        }
    }

    /// The parse errors a merge reports must not depend on how many threads
    /// carried the parses
    #[test]
    fn test_invalid_yaml_is_reported_at_every_size() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitLab, &resolved, 0);
        let filter = no_selection(&resolved);

        for pad in [0, 400] {
            let mut text = padded_gitlab_pipeline(pad);
            text.push_str("broken:\n  script: [unclosed\n");
            let big = text.len() >= PARALLEL_PARSE_FROM;
            assert_eq!(big, pad > 0, "fixture rot: {}", text.len());
            let err = merge_file(Platform::GitLab, &file, &filter, &text).unwrap_err();
            assert!(
                format!("{err:#}").contains("is not valid YAML; use --force to overwrite it"),
                "pad {pad}: {err:#}"
            );
        }
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

    /// The merges below each leave a file where the only thing out of date
    /// is something no job's own lines can express — the workflow preamble,
    /// GitLab's stage list or global variables, a CircleCI workflow — so
    /// they are the merge's only edit, with no job rewrite alongside to
    /// carry them. A merge that renders its output only once it has
    /// something to render has to count every one of them.
    #[test]
    fn test_github_scaffolding_is_the_only_change_needed() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitHub, &resolved, 0);
        let filter = no_selection(&resolved);

        // A workflow from before cibox pinned the token down: current in
        // every other respect
        let canonical = render_file(Platform::GitHub, &file).unwrap();
        let existing = canonical.replace("permissions:\n  contents: read\n", "");
        assert!(!existing.contains("permissions:"), "fixture rot: {existing}");

        let (content, counts) = merged(merge_file(Platform::GitHub, &file, &filter, &existing).unwrap());
        assert_eq!(
            (counts.conformed, counts.added, counts.removed),
            (0, 0, 0),
            "{content}"
        );
        assert!(content.contains("permissions:\n  contents: read\n"), "{content}");
        assert!(matches!(
            merge_file(Platform::GitHub, &file, &filter, &content).unwrap(),
            MergeOutcome::Unchanged
        ));
    }

    /// A job body written inline has no lines of its own to rewrite, so the
    /// text phase cannot address it and the tree phase is what conforms it
    #[test]
    fn test_github_inline_job_body_is_the_only_change_needed() {
        let facts = cargo_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let selection: std::collections::BTreeSet<String> =
            std::iter::once("rust-fmt".to_string()).collect();
        let filter = JobFilter {
            resolved: &resolved,
            selection: Some(&selection),
        };
        let file = plan(&facts, &resolved, Platform::GitHub)
            .unwrap()
            .swap_remove(0);

        let existing = "name: CI\non: [push]\npermissions:\n  contents: read\njobs:\n  \
                        rust-fmt: {runs-on: ubuntu-latest, steps: [{run: echo stale}]}\n";
        let (content, counts) = merged(merge_file(Platform::GitHub, &file, &filter, existing).unwrap());
        assert_eq!(counts.conformed, 1, "{content}");
        assert!(!content.contains("echo stale"), "{content}");
        assert!(content.contains("cargo fmt"), "{content}");
    }

    #[test]
    fn test_gitlab_stage_list_is_the_only_change_needed() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitLab, &resolved, 0);
        let filter = no_selection(&resolved);

        // A stage left over from a job that is gone: no job references it,
        // so declaring it only confuses the pipeline view
        let canonical = render_file(Platform::GitLab, &file).unwrap();
        let existing = canonical.replacen("stages:\n", "stages:\n- gone\n", 1);
        assert!(existing.contains("- gone"), "fixture rot: {existing}");

        let (content, _) = merged(merge_file(Platform::GitLab, &file, &filter, &existing).unwrap());
        assert!(!content.contains("- gone"), "{content}");
        assert!(matches!(
            merge_file(Platform::GitLab, &file, &filter, &content).unwrap(),
            MergeOutcome::Unchanged
        ));
    }

    /// A `stage` a job inherits instead of naming: GitLab's own idiom for
    /// sharing job configuration is a hidden `.`-prefixed template plus
    /// `extends`, and pruning the stage it declares leaves `deploy-prod`
    /// in an undeclared stage — a pipeline GitLab refuses to run at all
    /// ("chosen stage does not exist").
    #[test]
    fn test_gitlab_stage_of_an_extended_template_is_kept() {
        let facts = cargo_facts();
        let mut config = CiboxConfig::default();
        config.rust_release.enabled = Some(false);
        let resolved = resolve(&facts, &config);
        let file = plan(&facts, &resolved, Platform::GitLab)
            .unwrap()
            .swap_remove(0);
        let filter = no_selection(&resolved);

        let existing = "stages:\n- test\n- deploy\n\
                        .deploy_template:\n  stage: deploy\n  image: alpine:latest\n\
                        deploy-prod:\n  extends: .deploy_template\n  script: [echo deploying]\n";
        let (content, _) = merged(merge_file(Platform::GitLab, &file, &filter, existing).unwrap());

        let doc: Value = serde_yaml::from_str(&content).unwrap();
        let stages: Vec<&str> = doc["stages"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(stages.contains(&"deploy"), "{content}");
        assert!(matches!(
            merge_file(Platform::GitLab, &file, &filter, &content).unwrap(),
            MergeOutcome::Unchanged
        ));
    }

    /// The jobs an `include:` brings in are invisible here, so which stages
    /// are still referenced is unknowable and nothing may be pruned
    #[test]
    fn test_gitlab_stages_are_not_pruned_beside_an_include() {
        let facts = cargo_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = plan(&facts, &resolved, Platform::GitLab)
            .unwrap()
            .swap_remove(0);
        let filter = no_selection(&resolved);

        let existing = "include:\n- local: /ci/common.yml\nstages:\n- test\n- integration\n";
        let (content, _) = merged(merge_file(Platform::GitLab, &file, &filter, existing).unwrap());

        let doc: Value = serde_yaml::from_str(&content).unwrap();
        let stages: Vec<&str> = doc["stages"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(stages.contains(&"integration"), "{content}");
        // cibox's own jobs still get the stages they need appended
        assert!(stages.contains(&"lint"), "{content}");
        assert!(matches!(
            merge_file(Platform::GitLab, &file, &filter, &content).unwrap(),
            MergeOutcome::Unchanged
        ));
    }

    #[test]
    fn test_gitlab_hoisted_variables_are_the_only_change_needed() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::GitLab, &resolved, 0);
        let filter = no_selection(&resolved);

        // The jobs already carry their own CARGO_HOME; the global copy is
        // what an older cibox left behind, and it leaks onto every other job
        let canonical = render_file(Platform::GitLab, &file).unwrap();
        let existing = canonical.replacen("stages:\n", "variables:\n  CARGO_HOME: .cargo\nstages:\n", 1);

        let (content, _) = merged(merge_file(Platform::GitLab, &file, &filter, &existing).unwrap());
        let doc: Value = serde_yaml::from_str(&content).unwrap();
        assert!(doc.get("variables").is_none(), "{content}");
        assert_eq!(
            doc["rust-test"]["variables"]["CARGO_HOME"].as_str(),
            Some(".cargo"),
            "{content}"
        );
    }

    #[test]
    fn test_circleci_missing_workflow_is_the_only_change_needed() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::CircleCI, &resolved, 0);
        let filter = no_selection(&resolved);

        // Every job is current but nothing invokes any of them: without a
        // workflow CircleCI runs none of this
        let canonical = render_file(Platform::CircleCI, &file).unwrap();
        let existing = canonical[..canonical.find("workflows:").unwrap()].to_string();

        let (content, _) = merged(merge_file(Platform::CircleCI, &file, &filter, &existing).unwrap());
        assert!(content.contains("workflows:"), "{content}");
        for job in &file.jobs {
            assert!(content.contains(&format!("- {}", job.id)), "{}: {content}", job.id);
        }
        assert!(matches!(
            merge_file(Platform::CircleCI, &file, &filter, &content).unwrap(),
            MergeOutcome::Unchanged
        ));
    }

    #[test]
    fn test_circleci_emptied_workflow_is_the_only_change_needed() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::CircleCI, &resolved, 0);
        let filter = no_selection(&resolved);

        // A workflow invoking only a job cibox no longer generates: left
        // alone CircleCI rejects the config for the dangling reference
        let canonical = render_file(Platform::CircleCI, &file).unwrap();
        assert!(!canonical.contains("rust-msrv"), "fixture rot: {canonical}");
        let existing = format!("{canonical}  stale-only:\n    jobs:\n    - rust-msrv\n");

        let (content, _) = merged(merge_file(Platform::CircleCI, &file, &filter, &existing).unwrap());
        assert!(!content.contains("stale-only"), "{content}");
        assert!(!content.contains("rust-msrv"), "{content}");
        assert!(matches!(
            merge_file(Platform::CircleCI, &file, &filter, &content).unwrap(),
            MergeOutcome::Unchanged
        ));
    }

    /// A flow-style invocation list has no lines of its own to edit, so the
    /// missing invocations can't be appended to it textually — block entries
    /// written below `jobs: [a, b]` are not YAML at all. The list is
    /// rewritten whole instead, keeping the entries it already held.
    #[test]
    fn test_circleci_flow_workflow_list_gains_the_missing_invocations() {
        let facts = full_facts();
        let resolved = resolve(&facts, &CiboxConfig::default());
        let file = planned_file(Platform::CircleCI, &resolved, 0);
        let filter = no_selection(&resolved);

        let existing = r#"version: "2.1"
jobs:
  rust-test:
    docker: [{image: old}]
    steps: [checkout, run: echo stale]
  my-job:
    docker: [{image: mine}]
    steps: [checkout]
workflows:
  main:
    jobs: [rust-test, my-job]
"#;

        let (content, _) = merged(merge_file(Platform::CircleCI, &file, &filter, existing).unwrap());
        let doc: Value = serde_yaml::from_str(&content)
            .unwrap_or_else(|e| panic!("merged output is not YAML: {e}\n{content}"));
        let entries = doc["workflows"]["main"]["jobs"]
            .as_sequence()
            .unwrap_or_else(|| panic!("no main workflow list: {content}"));
        let invoked: Vec<String> = entries.iter().filter_map(entry_id).collect();
        // The user's own entry survives, and every generated job is invoked
        assert!(invoked.contains(&"my-job".to_string()), "{content}");
        for job in &file.jobs {
            assert!(invoked.contains(&job.id), "{} not invoked: {content}", job.id);
        }
        assert!(matches!(
            merge_file(Platform::CircleCI, &file, &filter, &content).unwrap(),
            MergeOutcome::Unchanged
        ));
    }
}

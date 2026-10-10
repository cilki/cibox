//! Commented-out ("disabled") jobs in an existing pipeline file.
//!
//! Users disable a cibox job by commenting its block out; `cibox update`
//! keeps regenerating the commented content so it is current when
//! uncommented. This module finds those blocks and converts between the
//! commented and plain forms. No YAML is interpreted by hand: a candidate
//! comment run is uncommented textually and handed to `serde_yaml` — it is a
//! disabled job only if the result parses to mappings whose keys are all
//! cibox-managed job ids.

use crate::config::Platform;
use crate::generator::JobFilter;
use serde_yaml::Value;
use std::ops::Range;
use yaml_edit::{AsYaml, YamlFile};

/// One contiguous run of comment lines defining disabled jobs.
pub(crate) struct DisabledBlock {
    /// The job ids the block defines, in order
    pub ids: Vec<String>,
    /// The uncommented content: job id → job body
    pub jobs: serde_yaml::Mapping,
    /// Byte range of the whole lines the block occupies in the source text
    pub span: Range<usize>,
    /// Column the job keys sit at once uncommented (re-emit indentation)
    pub indent: usize,
}

/// CircleCI only: a commented run of workflow invocation entries.
pub(crate) struct DisabledEntries {
    pub workflow: String,
    /// Entry ids (see [`super::merge::entry_id`]), deduplicated
    pub ids: Vec<String>,
    /// The uncommented entries
    pub entries: Vec<Value>,
    pub span: Range<usize>,
    pub indent: usize,
}

#[derive(Default)]
pub(crate) struct Scan {
    pub blocks: Vec<DisabledBlock>,
    pub entries: Vec<DisabledEntries>,
}

/// Where a comment run was found, which decides how it may parse
#[derive(Clone, PartialEq)]
enum Region {
    /// The job region: a mapping of job id → job body
    Jobs,
    /// A CircleCI workflow's `jobs:` sequence
    Workflow(String),
}

/// Find the commented-out managed jobs (and, on CircleCI, workflow entries)
/// in `text`. `file` must be the parse of `text`.
pub(crate) fn scan(platform: Platform, text: &str, file: &YamlFile, filter: &JobFilter) -> Scan {
    let mut scan = Scan::default();
    let Some(doc) = file.document() else {
        return scan;
    };
    let Some(root) = doc.as_mapping() else {
        return scan;
    };

    // The nodes whose direct comment children are candidates. On GitLab jobs
    // are top-level, so the region is the root mapping itself; elsewhere it
    // is the value of the top-level `jobs:` entry — the mapping node, plus
    // the value wrapper it sits in, which is what leading comments (or all
    // of them, when every job is commented out) attach to. The root mapping
    // is a candidate region everywhere: a block that trails a nested mapping
    // attaches to the root in the syntax tree, and the managed-job parse
    // below weeds out everything that isn't a commented job anyway.
    let mut regions = Vec::new();
    if let Some(node) = root.as_node() {
        regions.push((node.clone(), Region::Jobs));
    }
    if !matches!(platform, Platform::GitLab) {
        if let Some(node) = root.get("jobs").and_then(|v| v.as_node().cloned()) {
            if let Some(value) = node.parent() {
                regions.push((value, Region::Jobs));
            }
            regions.push((node, Region::Jobs));
        }
    }
    if platform == Platform::CircleCI {
        if let Some(workflows) = root.get_mapping("workflows") {
            for entry in workflows.entries() {
                let Some(name) = entry.key_node().map(|k| k.to_string()) else {
                    continue;
                };
                let Some(value) = entry.value_node() else {
                    continue;
                };
                let Some(seq) = value
                    .as_mapping()
                    .and_then(|w| w.get("jobs"))
                    .and_then(|j| j.as_node().cloned())
                else {
                    continue;
                };
                if let Some(value) = seq.parent() {
                    regions.push((value, Region::Workflow(name.clone())));
                }
                regions.push((seq, Region::Workflow(name)));
            }
        }
    }

    // Comments under a region node, grouped into runs of adjacent lines
    // (whitespace-only gaps allowed within a run)
    let mut runs: Vec<(Region, Range<usize>)> = Vec::new();
    for comment in doc.comments() {
        let Some(parent) = comment.syntax().parent() else {
            continue;
        };
        let Some((_, region)) = regions.iter().find(|(node, _)| *node == parent) else {
            continue;
        };
        let range = comment.text_range();
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        match runs.last_mut() {
            Some((last_region, last))
                if *last_region == *region && text[last.end..start].trim().is_empty() =>
            {
                last.end = end;
            }
            _ => runs.push((region.clone(), start..end)),
        }
    }

    for (region, run) in runs {
        let span = line_span(text, &run);
        match region {
            Region::Jobs => {
                if let Some(block) = parse_block(text, span, filter) {
                    scan.blocks.push(block);
                }
            }
            Region::Workflow(name) => {
                if let Some(entries) = parse_entries(text, span, filter, name) {
                    scan.entries.push(entries);
                }
            }
        }
    }
    scan
}

/// Expand a byte range to cover its whole lines, trailing newline included
pub(crate) fn line_span(text: &str, range: &Range<usize>) -> Range<usize> {
    let start = text[..range.start].rfind('\n').map_or(0, |i| i + 1);
    // A range that already ends just past a newline is line-aligned
    let end = if range.end > 0 && text.as_bytes()[range.end - 1] == b'\n' {
        range.end
    } else {
        text[range.end..]
            .find('\n')
            .map_or(text.len(), |i| range.end + i + 1)
    };
    start..end
}

/// Strip the comment marker from one line: drop the leading whitespace, the
/// `#`, and at most one following space. `None` if the line is blank.
/// Returns the column the `#` sat at and the remaining content.
fn uncomment_line(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return None;
    }
    let indent = line.len() - trimmed.len();
    let content = trimmed.strip_prefix('#')?;
    Some((indent, content.strip_prefix(' ').unwrap_or(content)))
}

/// Uncomment the lines of `text[span]` into a column-0 YAML fragment.
/// Returns the fragment and the effective indent of its first line in the
/// original document (hash column + content indentation).
fn uncomment_span(text: &str, span: &Range<usize>) -> Option<(String, usize)> {
    let mut lines = Vec::new();
    for line in text[span.clone()].lines() {
        match uncomment_line(line) {
            Some((indent, content)) => lines.push((indent, content)),
            None if line.trim().is_empty() => lines.push((0, "")),
            None => return None,
        }
    }
    let (first_hash, first_content) = lines.iter().find(|(_, c)| !c.is_empty())?;
    let dedent = first_content.len() - first_content.trim_start().len();
    let indent = first_hash + dedent;
    let fragment: String = lines
        .iter()
        .map(|(_, content)| {
            let content = if content.len() >= dedent && content[..dedent].trim().is_empty() {
                &content[dedent..]
            } else {
                content.trim_start()
            };
            format!("{content}\n")
        })
        .collect();
    Some((fragment, indent))
}

/// Try to read a comment run as a block of disabled jobs. The run qualifies
/// when its uncommented text parses to a mapping whose keys are all managed
/// job ids with mapping bodies. A run that fails (e.g. a prose note above
/// the commented job) is retried from its first line that uncomments to a
/// bare managed `key:`, so the prose stays an ordinary comment.
fn parse_block(text: &str, span: Range<usize>, filter: &JobFilter) -> Option<DisabledBlock> {
    if let Some(block) = parse_block_exact(text, span.clone(), filter) {
        return Some(block);
    }
    // Retry from the first managed `key:` line
    let mut offset = span.start;
    for line in text[span.clone()].split_inclusive('\n') {
        if let Some((_, content)) = uncomment_line(line) {
            let key = content.trim_end().strip_suffix(':').unwrap_or("");
            if !key.is_empty() && !key.contains(char::is_whitespace) && filter.managed(key) {
                if offset > span.start {
                    return parse_block_exact(text, offset..span.end, filter);
                }
                return None; // already tried from here
            }
        }
        offset += line.len();
    }
    None
}

fn parse_block_exact(text: &str, span: Range<usize>, filter: &JobFilter) -> Option<DisabledBlock> {
    let (fragment, indent) = uncomment_span(text, &span)?;
    let Ok(Value::Mapping(jobs)) = serde_yaml::from_str::<Value>(&fragment) else {
        return None;
    };
    if jobs.is_empty() {
        return None;
    }
    let mut ids = Vec::new();
    for (key, value) in &jobs {
        let id = key.as_str()?;
        // A job body is a mapping; anything else is prose or a heading
        if !filter.managed(id) || !value.is_mapping() {
            return None;
        }
        ids.push(id.to_string());
    }
    Some(DisabledBlock {
        ids,
        jobs,
        span,
        indent,
    })
}

fn parse_entries(
    text: &str,
    span: Range<usize>,
    filter: &JobFilter,
    workflow: String,
) -> Option<DisabledEntries> {
    let (fragment, indent) = uncomment_span(text, &span)?;
    let Ok(Value::Sequence(entries)) = serde_yaml::from_str::<Value>(&fragment) else {
        return None;
    };
    if entries.is_empty() {
        return None;
    }
    let mut ids: Vec<String> = Vec::new();
    for entry in &entries {
        let id = super::merge::entry_id(entry)?;
        if !filter.managed(&id) {
            return None;
        }
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Some(DisabledEntries {
        workflow,
        ids,
        entries,
        span,
        indent,
    })
}

/// Comment a column-0 fragment out at the given indentation.
pub(crate) fn comment_out(fragment: &str, indent: usize) -> String {
    let pad = " ".repeat(indent);
    fragment
        .lines()
        .map(|line| {
            if line.trim().is_empty() {
                format!("{pad}#\n")
            } else {
                format!("{pad}# {line}\n")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CiboxConfig;
    use crate::rules::resolve;
    use std::str::FromStr;

    fn filter_fixture() -> Vec<crate::rules::ResolvedRule> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let facts = crate::detection::gather_facts(dir.path());
        resolve(&facts, &CiboxConfig::default())
    }

    fn scan_text(platform: Platform, text: &str) -> Scan {
        let resolved = filter_fixture();
        let filter = JobFilter {
            resolved: &resolved,
            selection: None,
        };
        let file = YamlFile::from_str(text).unwrap();
        scan(platform, text, &file, &filter)
    }

    #[test]
    fn test_scan_finds_commented_job_in_each_style() {
        for style in [
            "  # rust-test:\n  #   runs-on: ubuntu-latest\n",
            "  #rust-test:\n  #  runs-on: ubuntu-latest\n",
            "# rust-test:\n#   runs-on: ubuntu-latest\n",
        ] {
            let text = format!("name: CI\non: [push]\njobs:\n{style}  other:\n    runs-on: x\n");
            let scan = scan_text(Platform::GitHub, &text);
            assert_eq!(scan.blocks.len(), 1, "{style:?}");
            assert_eq!(scan.blocks[0].ids, vec!["rust-test"], "{style:?}");
            let body = scan.blocks[0].jobs.get("rust-test").unwrap();
            assert_eq!(
                body.get("runs-on").and_then(Value::as_str),
                Some("ubuntu-latest"),
                "{style:?}"
            );
        }
    }

    #[test]
    fn test_scan_tolerates_interior_blank_and_prose_above() {
        let text = "name: CI\njobs:\n  # too flaky, revisit\n  # rust-test:\n  #   runs-on: x\n\n  #   steps:\n  #     - run: cargo test\n  other:\n    runs-on: x\n";
        let scan = scan_text(Platform::GitHub, text);
        assert_eq!(scan.blocks.len(), 1);
        let block = &scan.blocks[0];
        assert_eq!(block.ids, vec!["rust-test"]);
        // The prose line stays outside the block
        assert!(!text[block.span.clone()].contains("flaky"));
        let body = block.jobs.get("rust-test").unwrap();
        assert!(body.get("steps").is_some());
    }

    #[test]
    fn test_scan_ignores_prose_headers_and_custom_jobs() {
        let text = "# Required CI variables: TOKEN\nstages: [test]\n# rust-test: runs the tests\n# my-own-job:\n#   stage: test\nreal:\n  stage: test\n";
        let scan = scan_text(Platform::GitLab, text);
        assert!(scan.blocks.is_empty());
    }

    #[test]
    fn test_scan_ignores_comments_inside_job_bodies() {
        let text = "name: CI\njobs:\n  real:\n    steps:\n      # rust-test:\n      #   nested: note\n      - run: hi\n";
        let scan = scan_text(Platform::GitHub, text);
        assert!(scan.blocks.is_empty());
    }

    #[test]
    fn test_scan_finds_gitlab_top_level_job() {
        let text = "stages: [test]\n# rust-test:\n#   stage: test\n#   script: [cargo test]\nreal:\n  stage: test\n";
        let scan = scan_text(Platform::GitLab, text);
        assert_eq!(scan.blocks.len(), 1);
        assert_eq!(scan.blocks[0].indent, 0);
    }

    #[test]
    fn test_scan_finds_circleci_workflow_entries() {
        let text = "version: \"2.1\"\njobs:\n  real:\n    docker: [{image: x}]\n    steps: [checkout]\nworkflows:\n  main:\n    jobs:\n      - real\n      # - rust-test:\n      #     requires: [real]\n";
        let scan = scan_text(Platform::CircleCI, text);
        assert_eq!(scan.entries.len(), 1);
        assert_eq!(scan.entries[0].workflow, "main");
        assert_eq!(scan.entries[0].ids, vec!["rust-test"]);
    }

    #[test]
    fn test_scan_requires_a_job_body() {
        // A bare `# rust-test:` heading is prose, not a disabled job
        let text = "name: CI\njobs:\n  # rust-test:\n  real:\n    runs-on: x\n";
        let scan = scan_text(Platform::GitHub, text);
        assert!(scan.blocks.is_empty());
    }

    #[test]
    fn test_comment_out_round_trips_through_uncomment() {
        let fragment = "rust-test:\n  runs-on: ubuntu-latest\n  steps:\n    - run: cargo test\n";
        let commented = comment_out(fragment, 2);
        let span = 0..commented.len();
        let (back, indent) = uncomment_span(&commented, &span).unwrap();
        assert_eq!(back, fragment);
        assert_eq!(indent, 2);
    }
}


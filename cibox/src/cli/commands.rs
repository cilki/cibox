use super::display;
use crate::config::{infer_platform, parse_config, CiboxConfig, Platform};
use crate::detection::{gather_facts, ProjectFacts};
use crate::error::Result;
use crate::rules::{resolve, ResolvedRule};
use anyhow::Context;
use colored::Colorize;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Load cibox.ron if present. A missing file at the default path is fine —
/// detection supplies everything; an explicitly named missing file is an error.
fn load_config(config_path: &str, explicit_required: bool) -> Result<CiboxConfig> {
    match std::fs::read_to_string(config_path) {
        Ok(ron_str) => parse_config(&ron_str)
            .with_context(|| format!("Failed to parse {config_path}")),
        Err(_) if !explicit_required => {
            println!(
                "{} no {} — using detected defaults",
                "Note:".cyan().bold(),
                config_path
            );
            Ok(CiboxConfig::default())
        }
        Err(e) => Err(e).with_context(|| format!("Failed to read config file: {config_path}")),
    }
}

/// CLI flag > inference from the repository
fn effective_platform(platform_arg: Option<String>, facts: &ProjectFacts) -> Result<Platform> {
    if let Some(p) = platform_arg {
        return Platform::from_str(&p).map_err(|_| crate::error::unsupported_platform_error(&p));
    }
    Ok(infer_platform(facts))
}

fn print_rule_table(resolved: &[ResolvedRule]) {
    for rule in resolved {
        let marker = if rule.enabled {
            "✓".green().bold()
        } else {
            "○".dimmed()
        };
        let origin = match (rule.detected, rule.enabled) {
            (true, true) => "detected".green().dimmed(),
            (false, true) => "enabled in cibox.ron".yellow(),
            (true, false) => "disabled in cibox.ron".yellow(),
            (false, false) => "not detected".dimmed(),
        };
        let name = if rule.enabled {
            rule.rule.id().normal()
        } else {
            rule.rule.id().dimmed()
        };
        println!("  {} {:<16} {}", marker, name, origin);
    }
}

/// Handle the update command
pub fn handle_update(
    config_path: &str,
    platform_arg: Option<String>,
    force: bool,
    rules: &[String],
) -> Result<()> {
    let working_dir = PathBuf::from(".");
    let facts = gather_facts(&working_dir);
    let config = load_config(config_path, config_path != "cibox.ron")?;
    let platform = effective_platform(platform_arg, &facts)?;
    let resolved = resolve(&facts, &config);

    // `--rule` must be validated by hand: nothing else rejects an unknown id
    let valid: Vec<&str> = resolved.iter().map(|r| r.rule.id()).collect();
    if let Some(bad) = rules.iter().find(|r| !valid.contains(&r.as_str())) {
        anyhow::bail!("unknown rule '{bad}'; valid rules: {}", valid.join(", "));
    }
    let selection: Option<std::collections::BTreeSet<String>> =
        (!rules.is_empty()).then(|| rules.iter().cloned().collect());
    let filter = crate::generator::JobFilter {
        resolved: &resolved,
        selection: selection.as_ref(),
    };

    println!(
        "{} {} for {}",
        "Updating".cyan().bold(),
        "CI configuration".normal(),
        platform.name().yellow()
    );
    print_rule_table(&resolved);

    let planned = crate::generator::plan(&facts, &resolved, platform)
        .with_context(|| format!("Failed to generate CI configuration for {platform}"))?;

    // With nothing enabled there is still work to do as long as a managed file
    // exists: its cibox jobs are stale and get pruned below. Only when there is
    // neither anything to write nor anything to prune is the empty resolution a
    // mistake worth reporting.
    let prunable = !force
        && planned.iter().any(|file| {
            std::fs::read_to_string(working_dir.join(&file.path))
                .is_ok_and(|text| !text.trim().is_empty())
        });
    if planned
        .iter()
        .all(|file| crate::generator::managed_jobs(file, &filter).is_empty())
        && !prunable
    {
        anyhow::bail!("{}", crate::generator::NOTHING_ENABLED);
    }

    println!();
    for file in planned {
        let output_path = working_dir.join(&file.path);
        let path_label = output_path.display().to_string().yellow();
        let existing = std::fs::read_to_string(&output_path).ok();

        // A missing or empty file gets the full canonical content (as does
        // --force), narrowed to the selected rules under --rule; a file with
        // content is merged: cibox-managed jobs are conformed, re-added, or
        // pruned, and everything the user did to it is kept
        let content = match existing {
            Some(text) if !force && !text.trim().is_empty() => {
                match crate::generator::merge_file(platform, &file, &filter, &text)? {
                    crate::generator::MergeOutcome::Unchanged => {
                        println!("  {} {} unchanged", "○".dimmed(), path_label);
                        continue;
                    }
                    crate::generator::MergeOutcome::WouldEmpty => {
                        println!(
                            "  {} {} would be left without jobs — remove it yourself if unwanted",
                            "!".yellow().bold(),
                            path_label
                        );
                        continue;
                    }
                    crate::generator::MergeOutcome::Merged { content, counts } => {
                        let mut parts = vec![format!("{} updated", counts.conformed)];
                        if counts.added > 0 {
                            parts.push(format!("{} added", counts.added));
                        }
                        parts.push(format!("{} removed", counts.removed));
                        if counts.disabled > 0 {
                            parts.push(format!("{} disabled", counts.disabled));
                        }
                        parts.push(format!("{} kept", counts.preserved));
                        println!(
                            "  {} {} ({})",
                            "✓".green().bold(),
                            path_label,
                            parts.join(", ")
                        );
                        content
                    }
                }
            }
            _ => {
                let fresh = crate::generator::PlannedFile {
                    jobs: crate::generator::managed_jobs(&file, &filter),
                    ..file.clone()
                };
                if fresh.jobs.is_empty() {
                    continue;
                }
                println!("  {} {}", "✓".green().bold(), path_label);
                crate::generator::render_file(platform, &fresh)?
            }
        };

        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }
        std::fs::write(&output_path, content)
            .with_context(|| format!("Failed to write file: {}", output_path.display()))?;
    }

    println!("\n{}", "Done!".green().bold());
    Ok(())
}

/// Handle the validate command
pub fn handle_validate(config_path: &str) -> Result<()> {
    println!("{} {}", "Validating".cyan().bold(), config_path);

    let ron_str = std::fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read config file: {config_path}"))?;
    let config = parse_config(&ron_str).with_context(|| {
        "Failed to parse RON config. Check syntax and structure:\n\
         - Ensure all fields are properly formatted\n\
         - Check for missing commas\n\
         - Verify rule names match the documented set"
    })?;

    println!("\n{}", "Configuration is valid!".green().bold());

    let facts = gather_facts(Path::new("."));
    let platform = infer_platform(&facts);
    println!("  Platform: {} (inferred)", platform.name().yellow());
    println!("  Rules:");
    print_rule_table(&resolve(&facts, &config));

    Ok(())
}

/// Handle the detect command
pub fn handle_detect(dir: &str) -> Result<()> {
    let working_dir = PathBuf::from(dir);
    let facts = gather_facts(&working_dir);

    println!("{}", "Project facts:".cyan().bold());
    let yes_no = |b: bool| if b { "yes".green() } else { "no".dimmed() };
    // Names read out of the project's own files are printed through
    // `display::untrusted`: see that module for why
    if let Some(rust) = &facts.rust {
        println!(
            "  {} Rust{}{}",
            "✓".green(),
            rust.package_name
                .as_deref()
                .map(|n| format!(" ({})", display::untrusted(n)))
                .unwrap_or_default(),
            if rust.is_workspace { " [workspace]" } else { "" },
        );
        println!("    publishable: {}", yes_no(rust.publishable));
    }
    if let Some(python) = &facts.python {
        println!("  {} Python", "✓".green());
        println!("    publishable: {}", yes_no(python.publishable));
    }
    if let Some(go) = &facts.go {
        println!(
            "  {} Go{}",
            "✓".green(),
            go.module_path
                .as_deref()
                .map(|m| format!(" ({})", display::untrusted(m)))
                .unwrap_or_default()
        );
    }
    if let Some(node) = &facts.node {
        println!(
            "  {} Node ({}{})",
            "✓".green(),
            node.package_manager.as_str(),
            // Classic and Modern take different install flags and lay the
            // project out differently, so which one it is matters
            if node.yarn_berry { " modern" } else { "" },
        );
        println!("    typescript: {}", yes_no(node.has_tsconfig));
    }
    if facts.zig.is_some() {
        println!("  {} Zig", "✓".green());
    }
    if let Some(cmake) = &facts.cmake {
        println!("  {} CMake", "✓".green());
        println!("    tests: {}", yes_no(cmake.has_tests));
    }
    if let Some(docker) = &facts.docker {
        println!(
            "  {} Docker ({})",
            "✓".green(),
            display::untrusted(&docker.dockerfile)
        );
    }
    println!("    git repository: {}", yes_no(facts.is_git_repo));
    if let Some(slug) = &facts.repo_slug {
        println!("    remote: {}", display::untrusted(slug));
    }
    if !facts.existing_ci.is_empty() {
        let names: Vec<&str> = facts.existing_ci.iter().map(|p| p.name()).collect();
        println!("    existing CI: {}", names.join(", "));
    }

    println!();
    println!(
        "{} {}",
        "Inferred platform:".cyan().bold(),
        infer_platform(&facts).name().yellow()
    );

    println!();
    println!("{}", "Rules:".cyan().bold());
    let config_path = working_dir.join("cibox.ron");
    let config = match std::fs::read_to_string(&config_path) {
        Ok(ron_str) => parse_config(&ron_str).unwrap_or_else(|e| {
            eprintln!("Warning: ignoring unparseable cibox.ron: {e}");
            CiboxConfig::default()
        }),
        Err(_) => CiboxConfig::default(),
    };
    print_rule_table(&resolve(&facts, &config));

    println!();
    println!("{}", "Next steps:".cyan().bold());
    println!(
        "  • Run {} to write or refresh the pipeline",
        "cibox update".yellow()
    );
    println!(
        "  • Run {} to adjust rules interactively",
        "cibox editor".yellow()
    );
    println!(
        "  • Or override rules in {} (with LSP support via {})",
        "cibox.ron".yellow(),
        "cibox lsp".yellow()
    );

    Ok(())
}

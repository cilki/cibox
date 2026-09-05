use crate::config::{preset_choice_to_config, CiboxConfig};
use crate::editor::registry::build_registry;
use crate::error::Result;
use crate::generator::MultiPresetGenerator;
use anyhow::{bail, Context};
use colored::Colorize;
use std::path::PathBuf;
use std::sync::Arc;

/// Handle the generate command
pub fn handle_generate(config_path: &str, platform_arg: Option<String>, force: bool) -> Result<()> {
    use crate::detection::DetectorRegistry;
    use crate::editor::state::Platform;

    // 1. Load and parse RON
    println!("{} {}", "Loading".cyan().bold(), config_path);
    let ron_str = std::fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read config file: {}", config_path))?;

    // 2. Parse RON
    println!("{}", "Parsing RON configuration...".cyan().bold());
    let config: CiboxConfig = crate::config::ron_options()
        .from_str(&ron_str)
        .with_context(|| "Failed to parse RON config. Check syntax and structure.")?;

    if config.is_empty() {
        bail!("No presets defined in configuration file");
    }

    // 3. Detect project and determine platform
    let working_dir = std::path::PathBuf::from(".");
    let detector_registry = DetectorRegistry::new();
    let detection = detector_registry.detect(&working_dir)?;

    let platform = if let Some(p) = platform_arg {
        match p.to_lowercase().as_str() {
            "github" => Platform::GitHub,
            "gitea" => Platform::Gitea,
            "gitlab" => Platform::GitLab,
            "circleci" => Platform::CircleCI,
            "jenkins" => Platform::Jenkins,
            _ => Platform::GitHub,
        }
    } else {
        Platform::GitHub // Default platform
    };

    println!(
        "{} {} preset(s) for platform {}",
        "Found".green().bold(),
        config.len(),
        format!("{:?}", platform).yellow()
    );

    // 4. Convert to preset configs
    let registry = Arc::new(build_registry());
    let mut preset_configs = Vec::new();

    for preset_choice in &config.presets {
        let (preset_id, preset_config) = preset_choice_to_config(preset_choice);
        println!("  {} {}", "•".blue(), preset_id);
        preset_configs.push((preset_id, preset_config));
    }

    // 5. Generate outputs
    println!("\n{}", "Generating CI configurations...".cyan().bold());
    let language_version = detection
        .language_version
        .unwrap_or_else(|| "stable".to_string());

    let generator = MultiPresetGenerator::new(preset_configs, registry, platform, language_version);

    let outputs = generator
        .generate_all()
        .with_context(|| "Failed to generate CI configurations")?;

    // 6. Write files
    let base_path = PathBuf::from(".");

    for (filename, content) in outputs {
        // Use the full path (includes subdirectories like .github/workflows)
        let output_path = base_path.join(&filename);

        // Check if file exists and force flag
        if output_path.exists() && !force {
            bail!(
                "File exists: {}. Use --force to overwrite",
                output_path.display()
            );
        }

        // Create parent directories if needed
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        // Write file
        std::fs::write(&output_path, content)
            .with_context(|| format!("Failed to write file: {}", output_path.display()))?;

        println!(
            "  {} {}",
            "✓".green().bold(),
            output_path.display().to_string().yellow()
        );
    }

    println!("\n{}", "Done!".green().bold());
    Ok(())
}

/// Handle the validate command
pub fn handle_validate(config_path: &str) -> Result<()> {
    println!("{} {}", "Validating".cyan().bold(), config_path);

    // Load file
    let ron_str = std::fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read config file: {}", config_path))?;

    // Parse RON
    let config: CiboxConfig = crate::config::ron_options()
        .from_str(&ron_str)
        .with_context(|| {
            "Failed to parse RON config. Check syntax and structure:\n\
             - Ensure all fields are properly formatted\n\
             - Check for missing commas\n\
             - Verify enum variants match expected values"
        })?;

    // Basic validation
    if config.is_empty() {
        bail!("Validation failed: No presets defined in configuration");
    }

    println!("\n{}", "Configuration is valid!".green().bold());
    println!("  Presets: {}", config.len());

    for (idx, preset) in config.presets.iter().enumerate() {
        let preset_name = match preset {
            crate::config::PresetChoice::PythonApp(_) => "Python",
            crate::config::PresetChoice::Rust(_) => "Rust",
            crate::config::PresetChoice::GoApp(_) => "Go App",
            crate::config::PresetChoice::Docker(_) => "Docker",
        };
        println!("    {}. {}", idx + 1, preset_name);
    }

    Ok(())
}

/// Handle the detect command
pub fn handle_detect(dir: &str) -> Result<()> {
    use crate::detection::DetectorRegistry;
    use crate::editor::registry::build_registry;
    use std::path::PathBuf;

    let working_dir = PathBuf::from(dir);

    println!("{}", "Detecting project type...".cyan().bold());
    println!();

    // 1. Detect project type
    let detector_registry = DetectorRegistry::new();
    let detection = match detector_registry.detect(&working_dir) {
        Ok(d) => d,
        Err(_) => {
            println!("{}", "✗ No project type detected".red().bold());
            println!();
            println!("This directory doesn't appear to contain a recognized project type.");
            println!("Supported project types:");
            println!("  • Rust (Cargo.toml)");
            println!("  • Python (pyproject.toml, setup.py, requirements.txt)");
            println!("  • Go (go.mod)");
            println!("  • Docker (Dockerfile, docker-compose.yml)");
            return Ok(());
        }
    };

    // 2. Display project type
    println!(
        "{} {}",
        "✓ Project Type:".green().bold(),
        detection.project_type
    );

    if let Some(version) = &detection.language_version {
        println!("  {} {}", "Language Version:".dimmed(), version);
    }

    // 3. Display metadata if any
    if !detection.metadata.is_empty() {
        println!();
        println!("{}", "Metadata:".cyan().bold());
        for (key, value) in &detection.metadata {
            println!("  {} {}", format!("{}:", key).dimmed(), value);
        }
    }

    // 4. Check for existing CI files
    println!();
    println!(
        "{}",
        "Checking for existing CI configurations...".cyan().bold()
    );

    let ci_files = vec![
        (".github/workflows", "GitHub Actions"),
        (".gitea/workflows", "Gitea Actions"),
        (".gitlab-ci.yml", "GitLab CI"),
        (".circleci/config.yml", "CircleCI"),
        ("Jenkinsfile", "Jenkins"),
    ];

    let mut found_ci = false;
    for (path, platform) in ci_files {
        let full_path = working_dir.join(path);
        if full_path.exists() {
            println!("  {} {}", "✓".green(), platform);
            found_ci = true;
        }
    }

    if !found_ci {
        println!("  {} No existing CI configurations found", "ℹ".blue());
    }

    // 5. Show matching presets
    println!();
    println!("{}", "Matching presets for this project:".cyan().bold());

    let registry = build_registry();
    let mut matching_presets = Vec::new();
    let mut available_presets = Vec::new();

    for preset in registry.all() {
        if preset.matches_project(&detection.project_type, &working_dir) {
            matching_presets.push(preset);
        } else {
            available_presets.push(preset);
        }
    }

    if matching_presets.is_empty() {
        println!("  {} No presets match this project type", "ℹ".blue());
    } else {
        for preset in &matching_presets {
            println!("  {} {}", "✓".green().bold(), preset.preset_name());
            println!("    {}", preset.preset_description().dimmed());
        }
    }

    // 6. Show other available presets
    if !available_presets.is_empty() {
        println!();
        println!("{}", "Other available presets:".dimmed());
        for preset in &available_presets {
            println!("  {} {}", "○".dimmed(), preset.preset_name().dimmed());
            println!("    {}", preset.preset_description().dimmed());
        }
    }

    // 7. Suggest next steps
    println!();
    println!("{}", "Next steps:".cyan().bold());
    if matching_presets.is_empty() {
        println!(
            "  • Run {} to configure CI for this project",
            "cibox editor".yellow()
        );
    } else {
        println!(
            "  • Run {} to interactively configure CI",
            "cibox editor".yellow()
        );
        println!(
            "  • Or create a {} file and run {}",
            "cibox.ron".yellow(),
            "cibox generate".yellow()
        );
    }

    Ok(())
}
